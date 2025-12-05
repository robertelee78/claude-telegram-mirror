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
  private detectTmuxSession(): { session: string | null; pane: string | null; target: string | null } {
    // Check if we're inside tmux
    if (!process.env.TMUX) {
      return { session: null, pane: null, target: null };
    }

    try {
      const session = execSync('tmux display-message -p "#S"', { encoding: 'utf8' }).trim();
      const pane = execSync('tmux display-message -p "#P"', { encoding: 'utf8' }).trim();
      const windowIndex = execSync('tmux display-message -p "#I"', { encoding: 'utf8' }).trim();

      // Full target for send-keys: session:window.pane
      const target = `${session}:${windowIndex}.${pane}`;

      return { session, pane, target };
    } catch {
      return { session: null, pane: null, target: null };
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
   */
  handleStop(event: StopHookEvent): void {
    if (!this.connected) return;

    // Send final response if available
    if (event.transcript_summary) {
      this.send({
        type: 'agent_response',
        sessionId: this.sessionId,
        timestamp: event.timestamp,
        content: event.transcript_summary
      });
    }

    // Send session end
    this.send({
      type: 'session_end',
      sessionId: this.sessionId,
      timestamp: event.timestamp,
      content: 'Session stopped'
    });
  }

  /**
   * Handle PreToolUse hook
   * Returns approval decision if interactive mode enabled
   */
  async handlePreToolUse(event: PreToolUseHookEvent): Promise<{ decision: 'approve' | 'reject' } | null> {
    if (!this.connected) return null;

    // Check if this tool requires approval
    const requiresApproval = this.toolRequiresApproval(event.tool_name, event.tool_input);

    if (requiresApproval) {
      // Format approval request
      const toolDescription = this.formatToolDescription(event.tool_name, event.tool_input);

      this.send({
        type: 'approval_request',
        sessionId: this.sessionId,
        timestamp: event.timestamp,
        content: toolDescription,
        metadata: {
          tool: event.tool_name,
          input: event.tool_input,
          hookId: event.hook_id
        }
      });

      // Wait for response from bridge
      try {
        const response = await this.client.sendAndWait({
          type: 'approval_request',
          sessionId: this.sessionId,
          timestamp: event.timestamp,
          content: toolDescription,
          metadata: { hookId: event.hook_id }
        }, 300000); // 5 minute timeout

        if (response.type === 'approval_response') {
          return { decision: response.content === 'approve' ? 'approve' : 'reject' };
        }
      } catch (error) {
        // Timeout or error - default to approve
        if (this.config.verbose) {
          console.error('[telegram-hook] Approval timeout, defaulting to approve');
        }
      }
    }

    return null;
  }

  /**
   * Handle PostToolUse hook
   */
  handlePostToolUse(event: PostToolUseHookEvent): void {
    if (!this.connected) return;

    // Only send if verbose mode or significant output
    if (this.config.verbose || this.isSignificantOutput(event.tool_output)) {
      this.send({
        type: 'tool_result',
        sessionId: this.sessionId,
        timestamp: event.timestamp,
        content: event.tool_output || event.tool_error || 'No output',
        metadata: {
          tool: event.tool_name,
          input: event.tool_input,
          error: event.tool_error
        }
      });
    }
  }

  /**
   * Handle Notification hook
   */
  handleNotification(event: NotificationHookEvent): void {
    if (!this.connected) return;

    // Map notification levels
    const type = event.level === 'error' ? 'error' : 'agent_response';

    this.send({
      type,
      sessionId: this.sessionId,
      timestamp: event.timestamp,
      content: event.message,
      metadata: { level: event.level }
    });
  }

  /**
   * Handle UserPromptSubmit hook
   */
  handleUserPromptSubmit(event: UserPromptSubmitHookEvent): void {
    if (!this.connected) return;

    // Mirror user prompts to Telegram
    this.send({
      type: 'user_input',
      sessionId: this.sessionId,
      timestamp: event.timestamp,
      content: event.prompt,
      metadata: { source: 'cli' }
    });
  }

  /**
   * Handle agent text response (assistant message)
   */
  handleAgentResponse(text: string): void {
    if (!this.connected) return;

    this.send({
      type: 'agent_response',
      sessionId: this.sessionId,
      timestamp: new Date().toISOString(),
      content: text
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
   */
  async processEvent(event: AnyHookEvent): Promise<string | null> {
    switch (event.type) {
      case 'Stop':
        this.handleStop(event);
        return null;

      case 'SubagentStop':
        // Just log for now
        return null;

      case 'PreToolUse':
        const result = await this.handlePreToolUse(event);
        if (result) {
          // Return decision as JSON for hook script
          return JSON.stringify({ decision: result.decision });
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
          console.error('[telegram-hook] Unknown event type:', (event as AnyHookEvent).type);
        }
        return null;
    }
  }
}

// Session tracking file to detect first event
function getSessionTrackingPath(): string {
  const configDir = join(homedir(), '.config', 'claude-telegram-mirror');
  if (!existsSync(configDir)) {
    mkdirSync(configDir, { recursive: true });
  }
  // Use a unique file per terminal/PTY to handle multiple sessions
  const ttyId = process.env.TTY || process.env.SSH_TTY || process.ppid?.toString() || 'default';
  const safeId = ttyId.replace(/[^a-zA-Z0-9]/g, '_');
  return join(configDir, `.session_active_${safeId}`);
}

function isFirstEventOfSession(): boolean {
  const trackingPath = getSessionTrackingPath();
  if (existsSync(trackingPath)) {
    return false;
  }
  // Mark session as started
  writeFileSync(trackingPath, Date.now().toString());
  return true;
}

function clearSessionTracking(): void {
  const trackingPath = getSessionTrackingPath();
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
  const handler = new HookHandler();

  // Read event from stdin
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

    // Check if this is the first event of the session
    const isFirstEvent = isFirstEventOfSession();

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
    if (event.type === 'Stop') {
      clearSessionTracking();
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
