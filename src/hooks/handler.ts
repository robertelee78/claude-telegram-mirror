#!/usr/bin/env node
/**
 * Claude Code Hook Handler
 * Captures hook events and sends to bridge daemon
 */

import { hostname as getHostname, homedir } from 'os';
import { execSync } from 'child_process';
import { existsSync, writeFileSync, unlinkSync, mkdirSync } from 'fs';
import { join } from 'path';
import { SocketClient, DEFAULT_SOCKET_PATH } from '../bridge/socket.js';
import type { BridgeMessage } from '../bridge/types.js';
import type {
  AnyHookEvent,
  HookHandlerConfig,
  PreToolUseHookEvent,
  PostToolUseHookEvent,
  NotificationHookEvent,
  StopHookEvent,
  UserPromptSubmitHookEvent
} from './types.js';

/**
 * Hook Handler Class
 * Processes Claude Code hook events and forwards to bridge
 */
export class HookHandler {
  private client: SocketClient;
  private config: HookHandlerConfig;
  private connected = false;
  private sessionId: string;

  constructor(config: Partial<HookHandlerConfig> = {}) {
    this.config = {
      socketPath: config.socketPath || DEFAULT_SOCKET_PATH,
      sessionId: config.sessionId,
      verbose: config.verbose || process.env.TELEGRAM_MIRROR_VERBOSE === 'true'
    };
    this.client = new SocketClient(this.config.socketPath);
    this.sessionId = this.config.sessionId || this.generateSessionId();
  }

  /**
   * Generate a unique session ID
   */
  private generateSessionId(): string {
    const timestamp = Date.now().toString(36);
    const random = Math.random().toString(36).slice(2, 8);
    return `hook-${timestamp}-${random}`;
  }

  /**
   * Connect to bridge daemon
   */
  async connect(): Promise<boolean> {
    try {
      await this.client.connect();
      this.connected = true;
      return true;
    } catch (error) {
      // Bridge not running - gracefully continue
      if (this.config.verbose) {
        console.error('[telegram-hook] Bridge not available:', (error as Error).message);
      }
      return false;
    }
  }

  /**
   * Disconnect from bridge
   */
  disconnect(): void {
    this.client.disconnect();
    this.connected = false;
  }

  /**
   * Send message to bridge
   */
  private send(message: BridgeMessage): boolean {
    if (!this.connected) return false;
    return this.client.send(message);
  }

  /**
   * Handle session start (first hook event)
   */
  async handleSessionStart(projectDir?: string): Promise<void> {
    if (!await this.connect()) return;

    const hostname = getHostname();
    const tmuxInfo = this.detectTmuxSession();

    this.send({
      type: 'session_start',
      sessionId: this.sessionId,
      timestamp: new Date().toISOString(),
      content: 'Claude Code session started',
      metadata: {
        projectDir: projectDir || process.cwd(),
        hostname,
        tmuxSession: tmuxInfo.session,
        tmuxPane: tmuxInfo.pane,
        tmuxTarget: tmuxInfo.target
      }
    });
  }

  /**
   * Detect current tmux session and pane
   */
  private detectTmuxSession(): { session: string | null; pane: string | null; target: string | null; socket: string | null } {
    // Check if we're inside tmux
    if (!process.env.TMUX) {
      return { session: null, pane: null, target: null, socket: null };
    }

    try {
      // Extract socket path from $TMUX env var (format: /path/to/socket,pid,index)
      const socket = process.env.TMUX.split(',')[0] || null;

      const session = execSync('tmux display-message -p "#S"', { encoding: 'utf8' }).trim();
      const pane = execSync('tmux display-message -p "#P"', { encoding: 'utf8' }).trim();
      const windowIndex = execSync('tmux display-message -p "#I"', { encoding: 'utf8' }).trim();

      // Full target for send-keys: session:window.pane
      const target = `${session}:${windowIndex}.${pane}`;

      return { session, pane, target, socket };
    } catch {
      return { session: null, pane: null, target: null, socket: null };
    }
  }

  /**
   * Handle session end
   */
  handleSessionEnd(): void {
    if (!this.connected) return;

    this.send({
      type: 'session_end',
      sessionId: this.sessionId,
      timestamp: new Date().toISOString(),
      content: 'Claude Code session ended'
    });

    this.disconnect();
  }

  /**
   * Handle Stop hook
   * BUG-001 fix: Include current tmux info for auto-refresh
   */
  handleStop(event: StopHookEvent): void {
    if (!this.connected) return;

    const timestamp = event.timestamp || new Date().toISOString();
    const tmuxInfo = this.detectTmuxSession();

    // Send final response if available
    if (event.transcript_summary) {
      this.send({
        type: 'agent_response',
        sessionId: this.sessionId,
        timestamp,
        content: event.transcript_summary,
        metadata: {
          tmuxTarget: tmuxInfo.target,
          tmuxSocket: tmuxInfo.socket
        }
      });
    }

    // Send session end
    this.send({
      type: 'session_end',
      sessionId: this.sessionId,
      timestamp,
      content: 'Session stopped',
      metadata: {
        tmuxTarget: tmuxInfo.target,
        tmuxSocket: tmuxInfo.socket
      }
    });
  }

  /**
   * Handle PreToolUse hook
   * Returns approval decision for Claude Code's permission system
   *
   * Output format (Claude Code's hookSpecificOutput):
   * - permissionDecision: "allow" | "deny" | "ask"
   * - permissionDecisionReason: string shown to user (allow/ask) or Claude (deny)
   */
  async handlePreToolUse(event: PreToolUseHookEvent): Promise<{
    permissionDecision: 'allow' | 'deny' | 'ask';
    permissionDecisionReason: string;
  } | null> {
    if (!this.connected) return null;

    // Respect Claude Code's permission mode - don't prompt if bypassing permissions
    if (event.permission_mode === 'bypassPermissions') {
      return null;
    }

    // Check if this tool requires approval via Telegram
    const requiresApproval = this.toolRequiresApproval(event.tool_name, event.tool_input);

    if (!requiresApproval) {
      // Let Claude Code handle permission normally
      return null;
    }

    // Format approval request for Telegram
    const toolDescription = this.formatToolDescription(event.tool_name, event.tool_input);

    // Send approval request and wait for response
    const timestamp = event.timestamp || new Date().toISOString();
    try {
      const response = await this.client.sendAndWait({
        type: 'approval_request',
        sessionId: this.sessionId,
        timestamp,
        content: toolDescription,
        metadata: {
          tool: event.tool_name,
          input: event.tool_input,
          hookId: event.hook_id,
          toolUseId: event.tool_use_id
        }
      }, 300000); // 5 minute timeout for user to respond

      if (response.type === 'approval_response') {
        const action = response.content as string;

        if (action === 'approve') {
          return {
            permissionDecision: 'allow',
            permissionDecisionReason: 'Approved via Telegram'
          };
        } else if (action === 'reject') {
          return {
            permissionDecision: 'deny',
            permissionDecisionReason: 'Rejected via Telegram - user declined this operation'
          };
        } else if (action === 'abort') {
          return {
            permissionDecision: 'deny',
            permissionDecisionReason: 'Session aborted via Telegram - user requested to stop all operations'
          };
        }
      }
    } catch (error) {
      // Timeout or error - fall back to CLI prompt
      if (this.config.verbose) {
        console.error('[telegram-hook] Telegram approval timeout, falling back to CLI');
      }
      return {
        permissionDecision: 'ask',
        permissionDecisionReason: 'Telegram approval timed out - please approve in CLI'
      };
    }

    return null;
  }

  /**
   * Handle PostToolUse hook
   * BUG-001 fix: Include current tmux info for auto-refresh
   */
  handlePostToolUse(event: PostToolUseHookEvent): void {
    if (!this.connected) return;

    // Only send if verbose mode or significant output
    if (this.config.verbose || this.isSignificantOutput(event.tool_output)) {
      const tmuxInfo = this.detectTmuxSession();
      this.send({
        type: 'tool_result',
        sessionId: this.sessionId,
        timestamp: event.timestamp || new Date().toISOString(),
        content: event.tool_output || event.tool_error || 'No output',
        metadata: {
          tool: event.tool_name,
          input: event.tool_input,
          error: event.tool_error,
          tmuxTarget: tmuxInfo.target,
          tmuxSocket: tmuxInfo.socket
        }
      });
    }
  }

  /**
   * Handle Notification hook
   * BUG-001 fix: Include current tmux info for auto-refresh
   */
  handleNotification(event: NotificationHookEvent): void {
    if (!this.connected) return;

    // Map notification levels
    const type = event.level === 'error' ? 'error' : 'agent_response';
    const tmuxInfo = this.detectTmuxSession();

    this.send({
      type,
      sessionId: this.sessionId,
      timestamp: event.timestamp || new Date().toISOString(),
      content: event.message,
      metadata: {
        level: event.level,
        tmuxTarget: tmuxInfo.target,
        tmuxSocket: tmuxInfo.socket
      }
    });
  }

  /**
   * Handle UserPromptSubmit hook
   * BUG-001 fix: Include current tmux info for auto-refresh
   */
  handleUserPromptSubmit(event: UserPromptSubmitHookEvent): void {
    if (!this.connected) return;

    const tmuxInfo = this.detectTmuxSession();

    // Mirror user prompts to Telegram
    this.send({
      type: 'user_input',
      sessionId: this.sessionId,
      timestamp: event.timestamp || new Date().toISOString(),
      content: event.prompt,
      metadata: {
        source: 'cli',
        tmuxTarget: tmuxInfo.target,
        tmuxSocket: tmuxInfo.socket
      }
    });
  }

  /**
   * Handle agent text response (assistant message)
   * BUG-001 fix: Include current tmux info for auto-refresh
   */
  handleAgentResponse(text: string): void {
    if (!this.connected) return;

    const tmuxInfo = this.detectTmuxSession();

    this.send({
      type: 'agent_response',
      sessionId: this.sessionId,
      timestamp: new Date().toISOString(),
      content: text,
      metadata: {
        tmuxTarget: tmuxInfo.target,
        tmuxSocket: tmuxInfo.socket
      }
    });
  }

  /**
   * Check if tool requires approval
   */
  private toolRequiresApproval(toolName: string, input: Record<string, unknown>): boolean {
    // Tools that modify files or execute commands need approval
    const dangerousTools = ['Write', 'Edit', 'Bash', 'MultiEdit'];

    if (!dangerousTools.includes(toolName)) {
      return false;
    }

    // Additional checks for Bash
    if (toolName === 'Bash') {
      const command = (input.command as string) || '';
      // Skip approval for safe commands
      const safeCommands = ['ls', 'pwd', 'cat', 'head', 'tail', 'echo', 'grep', 'find', 'which'];
      const firstWord = command.split(/\s+/)[0];
      if (safeCommands.includes(firstWord)) {
        return false;
      }
    }

    return true;
  }

  /**
   * Format tool description for approval request
   */
  private formatToolDescription(toolName: string, input: Record<string, unknown>): string {
    let description = `🔧 **Tool:** \`${toolName}\`\n\n`;

    switch (toolName) {
      case 'Write':
        description += `📝 **File:** \`${input.file_path}\`\n`;
        description += `**Content preview:**\n\`\`\`\n${String(input.content).slice(0, 500)}${String(input.content).length > 500 ? '...' : ''}\n\`\`\``;
        break;

      case 'Edit':
        description += `✏️ **File:** \`${input.file_path}\`\n`;
        description += `**Old:** \`\`\`${String(input.old_string).slice(0, 200)}\`\`\`\n`;
        description += `**New:** \`\`\`${String(input.new_string).slice(0, 200)}\`\`\``;
        break;

      case 'Bash':
        description += `💻 **Command:**\n\`\`\`bash\n${input.command}\n\`\`\``;
        break;

      default:
        description += `**Input:**\n\`\`\`json\n${JSON.stringify(input, null, 2).slice(0, 500)}\n\`\`\``;
    }

    return description;
  }

  /**
   * Check if output is significant enough to send
   */
  private isSignificantOutput(output?: string): boolean {
    if (!output) return false;
    if (output.length < 10) return false;
    // Skip empty or minimal outputs
    if (/^\s*$/.test(output)) return false;
    return true;
  }

  /**
   * Process raw hook event from stdin
   * Note: Claude Code sends 'hook_event_name' not 'type' in the JSON payload
   */
  async processEvent(event: AnyHookEvent): Promise<string | null> {
    switch (event.hook_event_name) {
      case 'Stop':
        this.handleStop(event);
        return null;

      case 'SubagentStop':
        // Just log for now
        return null;

      case 'PreToolUse':
        const result = await this.handlePreToolUse(event);
        if (result) {
          // Return Claude Code's hookSpecificOutput format
          return JSON.stringify({
            hookSpecificOutput: {
              hookEventName: 'PreToolUse',
              permissionDecision: result.permissionDecision,
              permissionDecisionReason: result.permissionDecisionReason
            }
          });
        }
        return null;

      case 'PostToolUse':
        this.handlePostToolUse(event);
        return null;

      case 'Notification':
        this.handleNotification(event);
        return null;

      case 'UserPromptSubmit':
        this.handleUserPromptSubmit(event);
        return null;

      default:
        if (this.config.verbose) {
          console.error('[telegram-hook] Unknown event type:', (event as AnyHookEvent).hook_event_name);
        }
        return null;
    }
  }
}

// Session tracking file to detect first event - uses Claude's native session_id
function getSessionTrackingPath(sessionId: string): string {
  const configDir = join(homedir(), '.config', 'claude-telegram-mirror');
  if (!existsSync(configDir)) {
    mkdirSync(configDir, { recursive: true });
  }
  // Use Claude's session_id - this is stable for the entire Claude session
  return join(configDir, `.session_active_${sessionId}`);
}

function isFirstEventOfSession(sessionId: string): boolean {
  const trackingPath = getSessionTrackingPath(sessionId);
  if (existsSync(trackingPath)) {
    return false;
  }
  // Mark session as started
  writeFileSync(trackingPath, sessionId);
  return true;
}

function clearSessionTracking(sessionId: string): void {
  const trackingPath = getSessionTrackingPath(sessionId);
  try {
    if (existsSync(trackingPath)) {
      unlinkSync(trackingPath);
    }
  } catch {
    // Ignore cleanup errors
  }
}

/**
 * Main CLI entry point for hook processing
 */
export async function main(): Promise<void> {
  // Read event from stdin first
  let input = '';

  process.stdin.setEncoding('utf8');

  for await (const chunk of process.stdin) {
    input += chunk;
  }

  if (!input.trim()) {
    process.exit(0);
  }

  try {
    const event = JSON.parse(input) as AnyHookEvent;

    // Create handler with Claude's native session_id from the event
    // This ensures all events from the same Claude session use the same ID
    const handler = new HookHandler({
      sessionId: event.session_id
    });

    // Check if this is the first event of the session (using Claude's session_id)
    const isFirstEvent = isFirstEventOfSession(event.session_id);

    // Connect to bridge
    const connected = await handler.connect();

    if (!connected) {
      // Bridge not running, just exit silently
      process.exit(0);
    }

    // On first event, trigger session start
    if (isFirstEvent) {
      await handler.handleSessionStart(process.cwd());
    }

    // Clean up session tracking on Stop event
    if (event.hook_event_name === 'Stop') {
      clearSessionTracking(event.session_id);
    }

    // Process event
    const result = await handler.processEvent(event);

    // Output result if any
    if (result) {
      process.stdout.write(result);
    }

    handler.disconnect();
  } catch (error) {
    console.error('[telegram-hook] Error processing event:', error);
    process.exit(1);
  }
}

// Run if executed directly
if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch(console.error);
}

export default HookHandler;
