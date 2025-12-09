/**
 * Bridge Daemon
 * Central coordinator for Claude Code ↔ Telegram bridge
 */

import { EventEmitter } from 'events';
import { TelegramBot } from '../bot/telegram.js';
import { registerCommands, registerApprovalHandlers, registerToolDetailsHandler } from '../bot/commands.js';
import { SocketServer } from './socket.js';
import { SessionManager } from './session.js';
import { InputInjector } from './injector.js';
import { loadConfig, TelegramMirrorConfig } from '../utils/config.js';
import {
  formatAgentResponse,
  formatToolExecution,
  formatApprovalRequest,
  formatError,
  formatSessionStart,
  formatSessionEnd
} from '../bot/formatting.js';
import logger from '../utils/logger.js';
import type { BridgeMessage, Session } from './types.js';
import { execSync } from 'child_process';

/**
 * Bridge Daemon Class
 * Orchestrates all components
 */
export class BridgeDaemon extends EventEmitter {
  private config: TelegramMirrorConfig;
  private bot: TelegramBot;
  private socket: SocketServer;
  private sessions: SessionManager;
  private injector: InputInjector;
  private running = false;
  private cleanupInterval: NodeJS.Timeout | null = null;
  private sessionThreads: Map<string, number> = new Map(); // sessionId -> threadId
  private sessionTmuxTargets: Map<string, string> = new Map(); // sessionId -> tmux target
  private recentTelegramInputs: Set<string> = new Set(); // Track recent inputs from Telegram to avoid echo
  private toolInputCache: Map<string, { tool: string; input: unknown; timestamp: number }> = new Map(); // Cache tool inputs for details button
  private compactingSessions: Set<string> = new Set(); // Track sessions currently compacting

  // BUG-002 fix: Promise-based topic creation lock to prevent race conditions
  // When ensureSessionExists() creates a session before session_start arrives,
  // other handlers wait for the topic to be created via these promises
  private topicCreationPromises: Map<string, Promise<number | undefined>> = new Map();
  private topicCreationResolvers: Map<string, (threadId: number | undefined) => void> = new Map();

  constructor(config?: TelegramMirrorConfig) {
    super();
    this.config = config || loadConfig();
    this.bot = new TelegramBot(this.config);
    this.socket = new SocketServer(this.config.socketPath);
    this.sessions = new SessionManager();
    this.injector = new InputInjector();
  }

  /**
   * Start the daemon
   */
  async start(): Promise<void> {
    if (this.running) {
      logger.warn('Daemon already running');
      return;
    }

    logger.info('Starting bridge daemon...');

    // Start socket server
    await this.socket.listen();

    // Initialize input injector for Telegram → CLI
    const injectorReady = await this.injector.init();
    if (injectorReady) {
      logger.info('Input injector ready', {
        method: this.injector.getMethod(),
        tmuxSession: this.injector.getTmuxSession()
      });
    } else {
      logger.warn('Input injection not available - Telegram → CLI disabled');
    }

    // Setup message routing
    this.setupSocketHandlers();
    this.setupBotHandlers();

    // Register bot commands
    registerCommands(this.bot.getBot(), {
      getActiveSessions: async () => {
        return this.sessions.getActiveSessions().map(s => ({
          id: s.id,
          startedAt: s.startedAt,
          projectDir: s.metadata?.projectDir as string | undefined
        }));
      },
      abortSession: async (sessionId: string) => {
        this.sessions.endSession(sessionId, 'aborted');
        this.socket.broadcast({
          type: 'command',
          sessionId,
          timestamp: new Date().toISOString(),
          content: 'abort'
        });
        return true;
      },
      sendToSession: async (sessionId: string, text: string) => {
        return this.socket.broadcast({
          type: 'user_input',
          sessionId,
          timestamp: new Date().toISOString(),
          content: text
        }), true;
      }
    });

    // Register tool details handler (tap "Details" button on tool use messages)
    registerToolDetailsHandler(this.bot.getBot(), (toolUseId: string) => {
      const cached = this.toolInputCache.get(toolUseId);
      if (!cached) return undefined;
      return { tool: cached.tool, input: cached.input };
    });

    // Register approval handlers
    registerApprovalHandlers(this.bot.getBot(), async (approvalId, action) => {
      const approval = this.sessions.getApproval(approvalId);
      if (!approval) {
        logger.warn('Approval not found', { approvalId });
        return;
      }

      if (action === 'abort') {
        this.sessions.endSession(approval.sessionId, 'aborted');
        this.sessions.resolveApproval(approvalId, 'rejected');
        this.socket.broadcast({
          type: 'command',
          sessionId: approval.sessionId,
          timestamp: new Date().toISOString(),
          content: 'abort'
        });
      } else {
        this.sessions.resolveApproval(approvalId, action === 'approve' ? 'approved' : 'rejected');
        this.socket.broadcast({
          type: 'approval_response',
          sessionId: approval.sessionId,
          timestamp: new Date().toISOString(),
          content: action,
          metadata: { approvalId }
        });
      }
    });

    // Start bot
    await this.bot.start();

    // Start cleanup interval (every 5 minutes)
    this.cleanupInterval = setInterval(() => {
      this.sessions.expireOldApprovals();
      this.cleanupStaleSessions();  // BUG-003: Check for stale sessions
    }, 5 * 60 * 1000);

    this.running = true;
    logger.info('Bridge daemon started');

    // Send startup notification
    await this.bot.sendMessage(
      '🟢 *Bridge Daemon Started*\n\n' +
      'Claude Code sessions will now be mirrored here.',
      { parseMode: 'Markdown' }
    );
  }

  /**
   * Setup socket message handlers (CLI → Telegram)
   */
  private setupSocketHandlers(): void {
    this.socket.on('message', async (msg: BridgeMessage) => {
      logger.debug('Received socket message', { type: msg.type, sessionId: msg.sessionId });

      // Update session activity
      const session = this.sessions.getSession(msg.sessionId);
      if (session) {
        this.sessions.updateActivity(msg.sessionId);
      }

      // BUG-001 fix: Auto-update tmux target if it changed
      // Every message from hooks includes current tmux info, so we can detect moves
      this.checkAndUpdateTmuxTarget(msg);

      switch (msg.type) {
        case 'session_start':
          await this.handleSessionStart(msg);
          break;

        case 'session_end':
          await this.handleSessionEnd(msg);
          break;

        case 'agent_response':
          await this.ensureSessionExists(msg);
          await this.handleAgentResponse(msg);
          break;

        case 'tool_start':
          await this.ensureSessionExists(msg);
          await this.handleToolStart(msg);
          break;

        case 'tool_result':
          await this.ensureSessionExists(msg);
          await this.handleToolResult(msg);
          break;

        case 'user_input':
          await this.ensureSessionExists(msg);
          await this.handleUserInput(msg);
          break;

        case 'approval_request':
          await this.ensureSessionExists(msg);
          await this.handleApprovalRequest(msg);
          break;

        case 'error':
          await this.ensureSessionExists(msg);
          await this.handleError(msg);
          break;

        case 'turn_complete':
          // Claude fires Stop after every turn, not when session ends
          // Just log it and update activity - don't close the topic
          logger.debug('Turn complete', { sessionId: msg.sessionId });
          // Check if we were compacting - if so, send completion notification
          if (this.compactingSessions.has(msg.sessionId)) {
            await this.handleCompactComplete(msg.sessionId);
          }
          break;

        case 'pre_compact':
          await this.ensureSessionExists(msg);
          await this.handlePreCompact(msg);
          break;

        default:
          logger.debug('Unknown message type', { type: msg.type });
      }
    });

    this.socket.on('connect', (clientId: string) => {
      logger.debug('Hook client connected', { clientId });
    });

    this.socket.on('disconnect', (clientId: string) => {
      logger.debug('Hook client disconnected', { clientId });
    });
  }

  /**
   * Check if text is an interrupt command (sends Escape to pause Claude)
   * BUG-004 fix: Escape interrupts operation, Ctrl-C exits entirely
   */
  private isInterruptCommand(text: string): boolean {
    const normalized = text.trim().toLowerCase();
    return normalized === 'stop' ||
           normalized === '/stop' ||
           normalized === 'cancel' ||
           normalized === '/cancel' ||
           normalized === 'abort' ||
           normalized === '/abort' ||
           normalized === 'esc' ||
           normalized === '/esc' ||
           normalized === 'escape' ||
           normalized === '/escape';
  }

  /**
   * Check if text is a kill command (sends Ctrl-C to exit Claude entirely)
   */
  private isKillCommand(text: string): boolean {
    const normalized = text.trim().toLowerCase();
    return normalized === 'kill' ||
           normalized === '/kill' ||
           normalized === 'exit' ||
           normalized === '/exit' ||
           normalized === 'quit' ||
           normalized === '/quit' ||
           normalized === 'ctrl+c' ||
           normalized === 'ctrl-c' ||
           normalized === '^c';
  }

  /**
   * Setup bot message handlers (Telegram → CLI)
   */
  private setupBotHandlers(): void {
    // Handle text messages (forward to CLI)
    this.bot.onMessage(async (text, chatId, threadId) => {
      // BUG-005 fix: Ignore General topic entirely
      // Messages without threadId can't be routed to a specific session because:
      // 1. We don't know which Claude session/tmux target they belong to
      // 2. If user accidentally types in General, we'd send it to the wrong place
      // The daemon can still WRITE to General (startup messages), just not READ from it
      if (!threadId) {
        logger.debug('Ignoring message in General topic (no threadId)', { chatId, textPreview: text.substring(0, 50) });
        return;
      }

      // Message is in a specific topic - ONLY process if we own that topic
      // This is critical for multi-bot architecture: each bot ignores topics it didn't create
      const session = this.sessions.getSessionByThreadId(threadId);
      if (!session) {
        // This topic is not in our sessions.db - belongs to another bot/system
        // Silently ignore - another daemon will handle it
        logger.debug('Ignoring message for unknown topic (multi-bot)', { threadId, chatId });
        return;
      }

      // Get the tmux info for this session
      // First check memory cache, then fallback to database (handles daemon restart)
      let tmuxTarget = this.sessionTmuxTargets.get(session.id);
      let tmuxSocket: string | undefined;

      if (!tmuxTarget) {
        // Cache miss - restore from database (e.g., after daemon restart)
        const tmuxInfo = this.sessions.getTmuxInfo(session.id);
        tmuxTarget = tmuxInfo.target || undefined;
        tmuxSocket = tmuxInfo.socket || undefined;
        if (tmuxTarget) {
          // Repopulate cache for future requests
          this.sessionTmuxTargets.set(session.id, tmuxTarget);
          logger.info('Restored tmux info from database', { sessionId: session.id, tmuxTarget, tmuxSocket });
        }
      } else {
        // Have target in cache, get socket from session data
        tmuxSocket = session.tmuxSocket;
      }

      if (tmuxTarget) {
        this.injector.setTmuxSession(tmuxTarget, tmuxSocket);
      }

      // Check for interrupt command - send Escape to pause Claude (BUG-004 fix)
      if (this.isInterruptCommand(text)) {
        const sessionThreadId = this.getSessionThreadId(session.id);

        const interrupted = await this.injector.sendKey('Escape');
        if (interrupted) {
          logger.info('Sent interrupt signal (Escape) to CLI', { sessionId: session.id });
          await this.bot.sendMessage(
            '⏸️ *Interrupt sent* (Escape)\n\n_Claude should pause the current operation._',
            { parseMode: 'Markdown' },
            sessionThreadId
          );
        } else {
          logger.warn('Failed to send interrupt signal', { sessionId: session.id });
          await this.bot.sendMessage(
            '⚠️ *Could not send interrupt*\n\nNo tmux session found.',
            { parseMode: 'Markdown' },
            sessionThreadId
          );
        }
        return; // Don't inject command as text
      }

      // Check for kill command - send Ctrl-C to exit Claude entirely (BUG-004 fix)
      if (this.isKillCommand(text)) {
        const sessionThreadId = this.getSessionThreadId(session.id);

        const killed = await this.injector.sendKey('Ctrl-C');
        if (killed) {
          logger.info('Sent kill signal (Ctrl-C) to CLI', { sessionId: session.id });
          await this.bot.sendMessage(
            '🛑 *Kill sent* (Ctrl-C)\n\n_Claude should exit entirely._',
            { parseMode: 'Markdown' },
            sessionThreadId
          );
        } else {
          logger.warn('Failed to send kill signal', { sessionId: session.id });
          await this.bot.sendMessage(
            '⚠️ *Could not send kill*\n\nNo tmux session found.',
            { parseMode: 'Markdown' },
            sessionThreadId
          );
        }
        return; // Don't inject command as text
      }

      // Track this input so we don't echo it back when the hook fires
      const inputKey = `${session.id}:${text.trim()}`;
      this.recentTelegramInputs.add(inputKey);
      // Auto-remove after 10 seconds
      setTimeout(() => this.recentTelegramInputs.delete(inputKey), 10000);

      // Inject input into Claude Code via tmux
      const injected = await this.injector.inject(text);

      if (injected) {
        logger.info('Injected input to CLI', { sessionId: session.id, method: this.injector.getMethod() });
        // Silently inject - no confirmation needed, user already sees their message
      } else {
        logger.warn('Failed to inject input', { sessionId: session.id });

        // BUG-001 fix: Clear, actionable error message
        // The user needs to know HOW to recover
        const sessionThreadId = this.getSessionThreadId(session.id);
        const validation = this.injector.validateTarget();

        let errorMessage: string;
        if (!validation.valid) {
          // Target pane doesn't exist - give recovery instructions
          errorMessage =
            `⚠️ *Could not send input to CLI*\n\n` +
            `${validation.reason || 'Target pane not found.'}\n\n` +
            `_Send any command in Claude to refresh the connection._`;
        } else {
          // Some other injection error
          errorMessage =
            `⚠️ *Could not send input to CLI*\n\n` +
            `No tmux session found. Make sure Claude Code is running in tmux.`;
        }

        await this.bot.sendMessage(errorMessage, { parseMode: 'Markdown' }, sessionThreadId);
      }

      // Also broadcast to socket for logging/other listeners
      this.socket.broadcast({
        type: 'user_input',
        sessionId: session.id,
        timestamp: new Date().toISOString(),
        content: text
      });
    });
  }

  // ============ Stale Session Cleanup (BUG-003 fix) ============

  /**
   * Check if a tmux pane exists
   */
  private isTmuxPaneAlive(tmuxTarget: string, tmuxSocket?: string): boolean {
    try {
      const socketFlag = tmuxSocket ? `-S "${tmuxSocket}"` : '';
      execSync(`tmux ${socketFlag} list-panes -t "${tmuxTarget}" 2>/dev/null`, {
        stdio: 'pipe',
        encoding: 'utf8'
      });
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Clean up stale sessions (BUG-003)
   * Conditions for cleanup:
   * 1. lastActivity > staleSessionTimeoutHours (default 72h)
   * 2. AND (tmux pane doesn't exist OR tmux pane belongs to different session)
   */
  private async cleanupStaleSessions(): Promise<void> {
    const candidates = this.sessions.getStaleSessionCandidates(this.config.staleSessionTimeoutHours);

    if (candidates.length === 0) return;

    logger.debug('Checking stale session candidates', { count: candidates.length });

    for (const session of candidates) {
      const tmuxTarget = session.tmuxTarget;
      const tmuxSocket = session.tmuxSocket;

      // Skip sessions without tmux info (can't verify)
      if (!tmuxTarget) {
        logger.debug('Skipping stale candidate without tmux target', { sessionId: session.id });
        continue;
      }

      // Check condition 2a: Does the tmux pane still exist?
      const paneExists = this.isTmuxPaneAlive(tmuxTarget, tmuxSocket);

      // Check condition 2b: Is the pane owned by a different active session?
      const paneReassigned = this.sessions.isTmuxTargetOwnedByOtherSession(tmuxTarget, session.id);

      // Only clean up if pane is gone OR reassigned
      if (!paneExists || paneReassigned) {
        const reason = !paneExists ? 'pane no longer exists' : 'pane reassigned to another session';
        logger.info('Cleaning up stale session', {
          sessionId: session.id,
          tmuxTarget,
          reason,
          lastActivity: session.lastActivity.toISOString()
        });

        await this.handleStaleSessionCleanup(session, reason);
      } else {
        logger.debug('Stale candidate still valid (pane exists)', {
          sessionId: session.id,
          tmuxTarget
        });
      }
    }
  }

  /**
   * Handle cleanup of a stale session
   */
  private async handleStaleSessionCleanup(session: Session, reason: string): Promise<void> {
    const threadId = this.getSessionThreadId(session.id);

    // Send final message to the forum topic
    if (threadId) {
      try {
        await this.bot.sendMessage(
          `🔌 *Session ended* (terminal closed)\n\n_${reason}_`,
          { parseMode: 'Markdown' },
          threadId
        );

        // Close the forum topic
        await this.bot.closeForumTopic(threadId);
      } catch (error) {
        logger.warn('Failed to send stale session notification', { sessionId: session.id, error });
      }
    }

    // Clean up in-memory caches
    this.sessionThreads.delete(session.id);
    this.sessionTmuxTargets.delete(session.id);

    // Mark session as ended in database
    this.sessions.endSession(session.id, 'ended');

    logger.info('Stale session cleaned up', { sessionId: session.id, reason });
  }

  // ============ tmux Target Auto-Refresh (BUG-001 fix) ============

  /**
   * Check if tmux target has changed and update if necessary
   * This enables auto-healing when user reorganizes tmux panes
   */
  private checkAndUpdateTmuxTarget(msg: BridgeMessage): void {
    const newTmuxTarget = msg.metadata?.tmuxTarget as string | undefined;
    const newTmuxSocket = msg.metadata?.tmuxSocket as string | undefined;

    // No tmux info in message, nothing to update
    if (!newTmuxTarget) return;

    // Get current stored target
    const currentTarget = this.sessionTmuxTargets.get(msg.sessionId);

    // Target hasn't changed
    if (currentTarget === newTmuxTarget) return;

    // Target has changed! Update cache and database
    logger.info('Tmux target changed, auto-updating', {
      sessionId: msg.sessionId,
      oldTarget: currentTarget || 'none',
      newTarget: newTmuxTarget,
      socket: newTmuxSocket
    });

    // Update in-memory cache
    this.sessionTmuxTargets.set(msg.sessionId, newTmuxTarget);

    // Update database for persistence across daemon restarts
    this.sessions.setTmuxInfo(msg.sessionId, newTmuxTarget, newTmuxSocket || undefined);
  }

  // ============ Message Handlers ============

  private async handleSessionStart(msg: BridgeMessage): Promise<void> {
    const hostname = msg.metadata?.hostname as string | undefined;
    const projectDir = msg.metadata?.projectDir as string | undefined;
    const tmuxTarget = msg.metadata?.tmuxTarget as string | undefined;
    const tmuxSocket = msg.metadata?.tmuxSocket as string | undefined;

    // Use Claude's native session_id from the message
    // This ensures all events from the same Claude session map to the same Telegram thread
    const sessionId = this.sessions.createSession(
      this.config.chatId,
      projectDir,
      undefined,
      hostname,
      msg.sessionId,  // Use Claude's session_id!
      tmuxTarget,     // Persist tmux target to database
      tmuxSocket      // Persist tmux socket path to database
    );

    // Also cache in memory for fast lookups
    if (tmuxTarget) {
      this.sessionTmuxTargets.set(sessionId, tmuxTarget);
      logger.info('Session tmux info stored', { sessionId, tmuxTarget, tmuxSocket });
    }

    // Check if session already has a thread (e.g., daemon restarted but session continues)
    let threadId: number | null = this.sessions.getSessionThread(sessionId);

    if (threadId) {
      // Reuse existing thread - don't create a new topic
      this.sessionThreads.set(sessionId, threadId);
      logger.info('Reusing existing session thread', { sessionId, threadId });
    } else if (this.config.useThreads) {
      // Create a new forum topic only if none exists
      const topicName = this.formatTopicName(sessionId, hostname, projectDir);
      threadId = await this.bot.createForumTopic(topicName, 0); // Blue color (index 0)

      if (threadId) {
        this.sessions.setSessionThread(sessionId, threadId);
        this.sessionThreads.set(sessionId, threadId);
        logger.info('Session thread created', { sessionId, threadId });
      }
    }

    // BUG-002 FIX: Resolve any pending topic creation promise
    // This unblocks message handlers that were waiting via waitForTopic()
    const resolver = this.topicCreationResolvers.get(sessionId);
    if (resolver) {
      resolver(threadId || undefined);
      this.topicCreationPromises.delete(sessionId);
      this.topicCreationResolvers.delete(sessionId);
      logger.debug('Topic creation promise resolved', { sessionId, threadId });
    }

    // Broadcast session registered (with Claude's session ID)
    this.socket.broadcast({
      type: 'session_start',
      sessionId,
      timestamp: new Date().toISOString(),
      content: 'Session registered',
      metadata: { threadId }
    });

    // Build session info message
    let sessionInfo = formatSessionStart(sessionId, projectDir, hostname);
    if (tmuxTarget) {
      sessionInfo += `\n📺 tmux: \`${tmuxTarget}\``;
    }

    // Notify user (in the thread if available)
    await this.bot.sendMessage(
      sessionInfo,
      { parseMode: 'Markdown' },
      threadId || undefined
    );
  }

  /**
   * Format topic name for a session
   */
  private formatTopicName(sessionId: string, hostname?: string, projectDir?: string): string {
    const parts: string[] = [];

    // Add hostname if available
    if (hostname) {
      parts.push(hostname);
    }

    // Add project directory basename
    if (projectDir) {
      const basename = projectDir.split('/').pop() || projectDir;
      parts.push(basename);
    }

    // Add short session ID
    const shortId = sessionId.replace('session-', '').substring(0, 8);
    parts.push(shortId);

    return parts.join(' • ') || `Session ${shortId}`;
  }

  /**
   * Ensure a session exists (create on-the-fly if needed)
   *
   * BUG-010 FIX: Now calls handleSessionStart() directly to create both session
   * AND topic immediately. Previously only created the session and waited for
   * a session_start message to create the topic, but that message is never sent
   * (removed in BUG-006 when hooks became stateless).
   *
   * BUG-009 FIX: Also reactivates sessions that were incorrectly marked as ended.
   *
   * Race condition safety (BUG-002 pattern preserved):
   * - First caller creates Promise synchronously, then calls handleSessionStart()
   * - Concurrent callers see Promise exists, wait on it
   * - handleSessionStart() creates topic and resolves Promise
   * - JavaScript's single-threaded event loop guarantees no interleaving between
   *   the synchronous check and set operations
   */
  private async ensureSessionExists(msg: BridgeMessage): Promise<void> {
    const existing = this.sessions.getSession(msg.sessionId);
    if (existing) {
      // BUG-009 fix: If session was ended/aborted but we're receiving hook events,
      // Claude is still running - reactivate the session
      if (existing.status !== 'active') {
        logger.info('Reactivating ended session (hook event received)', {
          sessionId: msg.sessionId,
          previousStatus: existing.status
        });
        this.sessions.reactivateSession(msg.sessionId);
      }
      return;
    }

    // BUG-010 FIX: Check if another call is already creating this session's topic
    // This prevents duplicate topics when concurrent events arrive for a new session
    const existingPromise = this.topicCreationPromises.get(msg.sessionId);
    if (existingPromise) {
      logger.debug('Topic creation already in progress, waiting...', { sessionId: msg.sessionId });
      await existingPromise;
      return;
    }

    // BUG-010 FIX: Create Promise BEFORE any async work (synchronous check-and-set)
    // This is safe because JavaScript's event loop guarantees no interleaving
    // between synchronous operations. The Promise must be in the map before we
    // await handleSessionStart(), so concurrent callers will see it and wait.
    if (this.config.useThreads) {
      let resolver: (threadId: number | undefined) => void;
      const promise = new Promise<number | undefined>((resolve) => {
        resolver = resolve;
      });
      this.topicCreationPromises.set(msg.sessionId, promise);
      this.topicCreationResolvers.set(msg.sessionId, resolver!);
      logger.debug('Topic creation promise created', { sessionId: msg.sessionId });
    }

    // BUG-010 FIX: Call handleSessionStart() directly to create session AND topic
    // Previously we only created the session here and waited for session_start
    // to create the topic, but session_start is never sent (removed in BUG-006)
    logger.info('Creating session on-the-fly', { sessionId: msg.sessionId });
    await this.handleSessionStart(msg);
  }

  /**
   * Get thread ID for a session
   */
  private getSessionThreadId(sessionId: string): number | undefined {
    // Check in-memory cache first
    let threadId = this.sessionThreads.get(sessionId);
    if (threadId) return threadId;

    // Fallback to database
    const dbThreadId = this.sessions.getSessionThread(sessionId);
    if (dbThreadId) {
      this.sessionThreads.set(sessionId, dbThreadId);
      return dbThreadId;
    }

    return undefined;
  }

  /**
   * Wait for topic creation to complete (BUG-002 fix, updated for BUG-010)
   *
   * When ensureSessionExists() creates a new session, it creates a Promise and
   * then calls handleSessionStart() to create the topic. Other message handlers
   * call this function to wait for that topic creation to complete.
   *
   * @param sessionId - The session to wait for
   * @param timeoutMs - Maximum time to wait (default 5 seconds)
   * @returns The threadId if topic exists/created, undefined if timeout or no topic
   */
  private async waitForTopic(sessionId: string, timeoutMs: number = 5000): Promise<number | undefined> {
    // Fast path: topic already exists
    const existing = this.getSessionThreadId(sessionId);
    if (existing) return existing;

    // Check if there's a pending topic creation promise
    const promise = this.topicCreationPromises.get(sessionId);
    if (!promise) {
      // No pending creation - topic doesn't exist and isn't being created
      // This can happen if session_start already ran but didn't create a topic (useThreads=false)
      return undefined;
    }

    logger.debug('Waiting for topic creation...', { sessionId, timeoutMs });

    // Wait for topic creation with timeout
    try {
      const result = await Promise.race([
        promise,
        new Promise<undefined>((resolve) =>
          setTimeout(() => {
            logger.error('Topic creation timeout - message will be dropped', { sessionId, timeoutMs });
            resolve(undefined);
          }, timeoutMs)
        )
      ]);

      // If we got a result, check cache again (promise may have resolved and updated cache)
      if (result !== undefined) {
        return result;
      }

      // Timeout occurred - check cache one more time in case it was created
      const finalCheck = this.getSessionThreadId(sessionId);
      if (finalCheck) {
        logger.debug('Topic found after timeout race', { sessionId, threadId: finalCheck });
        return finalCheck;
      }

      return undefined;
    } catch (error) {
      logger.error('Error waiting for topic creation', { sessionId, error });
      return undefined;
    }
  }

  private async handleSessionEnd(msg: BridgeMessage): Promise<void> {
    const session = this.sessions.getSession(msg.sessionId);
    if (session) {
      const duration = Date.now() - session.startedAt.getTime();
      const threadId = this.getSessionThreadId(msg.sessionId);

      // Send end message in the session's thread
      await this.bot.sendMessage(
        formatSessionEnd(msg.sessionId, duration),
        { parseMode: 'Markdown' },
        threadId
      );

      // Close the forum topic if it exists
      if (threadId) {
        await this.bot.closeForumTopic(threadId);
        this.sessionThreads.delete(msg.sessionId);
      }

      // Clean up tmux target
      this.sessionTmuxTargets.delete(msg.sessionId);

      this.sessions.endSession(msg.sessionId);
    }
  }

  private async handleAgentResponse(msg: BridgeMessage): Promise<void> {
    // BUG-002 FIX: Wait for topic creation if it's pending
    const threadId = await this.waitForTopic(msg.sessionId);
    if (threadId === undefined && this.config.useThreads) {
      logger.error('Topic creation timeout - dropping agent_response', { sessionId: msg.sessionId });
      return;
    }
    await this.bot.sendMessage(
      formatAgentResponse(msg.content),
      { parseMode: 'Markdown' },
      threadId
    );
  }

  private async handleToolStart(msg: BridgeMessage): Promise<void> {
    // Only show tool starts in verbose mode to avoid noise
    if (!this.config.verbose) return;

    const toolName = msg.metadata?.tool as string || 'Unknown';
    const toolInput = msg.metadata?.input as Record<string, unknown> | undefined;
    // BUG-002 FIX: Wait for topic creation if it's pending
    const threadId = await this.waitForTopic(msg.sessionId);
    if (threadId === undefined && this.config.useThreads) {
      logger.error('Topic creation timeout - dropping tool_start', { sessionId: msg.sessionId, toolName });
      return;
    }

    // Format brief preview based on tool type
    let preview = '';
    if (toolInput) {
      if (toolName === 'Read' && toolInput.file_path) {
        preview = ` \`${this.truncatePath(toolInput.file_path as string)}\``;
      } else if (toolName === 'Write' && toolInput.file_path) {
        preview = ` \`${this.truncatePath(toolInput.file_path as string)}\``;
      } else if (toolName === 'Edit' && toolInput.file_path) {
        preview = ` \`${this.truncatePath(toolInput.file_path as string)}\``;
      } else if (toolName === 'Bash' && toolInput.command) {
        const cmd = (toolInput.command as string).slice(0, 50);
        preview = `\n\`${cmd}${(toolInput.command as string).length > 50 ? '...' : ''}\``;
      } else if (toolName === 'Grep' && toolInput.pattern) {
        preview = ` \`${toolInput.pattern}\``;
      } else if (toolName === 'Glob' && toolInput.pattern) {
        preview = ` \`${toolInput.pattern}\``;
      } else if (toolName === 'Task' && toolInput.description) {
        preview = ` ${toolInput.description}`;
      } else if (toolName === 'WebFetch' && toolInput.url) {
        preview = ` \`${(toolInput.url as string).slice(0, 40)}...\``;
      } else if (toolName === 'WebSearch' && toolInput.query) {
        preview = ` "${toolInput.query}"`;
      }
    }

    // Generate unique ID for this tool use
    const toolUseId = `tool_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;

    // Store tool input for detail retrieval (with 5 min expiry)
    this.toolInputCache.set(toolUseId, {
      tool: toolName,
      input: toolInput,
      timestamp: Date.now()
    });
    setTimeout(() => this.toolInputCache.delete(toolUseId), 5 * 60 * 1000);

    // Send with "Details" button if there's any input to show
    if (toolInput && Object.keys(toolInput).length > 0) {
      await this.bot.sendWithButtons(
        `🔧 *Running:* \`${toolName}\`${preview}`,
        [{ text: '📋 Details', callbackData: `tooldetails:${toolUseId}` }],
        { parseMode: 'Markdown' },
        threadId
      );
    } else {
      await this.bot.sendMessage(
        `🔧 *Running:* \`${toolName}\`${preview}`,
        { parseMode: 'Markdown' },
        threadId
      );
    }
  }

  /**
   * Truncate file path to show basename and parent
   */
  private truncatePath(path: string): string {
    const parts = path.split('/');
    if (parts.length <= 3) return path;
    return `.../${parts.slice(-2).join('/')}`;
  }

  private async handleToolResult(msg: BridgeMessage): Promise<void> {
    if (!this.config.verbose) return;

    const toolName = msg.metadata?.tool as string || 'Unknown';
    const toolInput = msg.metadata?.input;
    const toolOutput = msg.content;
    // BUG-002 FIX: Wait for topic creation if it's pending
    const threadId = await this.waitForTopic(msg.sessionId);
    if (threadId === undefined && this.config.useThreads) {
      logger.error('Topic creation timeout - dropping tool_result', { sessionId: msg.sessionId, toolName });
      return;
    }

    await this.bot.sendMessage(
      formatToolExecution(toolName, toolInput, toolOutput, this.config.verbose),
      { parseMode: 'Markdown' },
      threadId
    );
  }

  private async handleUserInput(msg: BridgeMessage): Promise<void> {
    const source = msg.metadata?.source as string || 'cli';

    // Skip if this was explicitly marked as from Telegram
    if (source === 'telegram') {
      logger.debug('Skipping echo for telegram input (source=telegram)');
      return;
    }

    // Check if this input was recently sent from Telegram (deduplication)
    const inputKey = `${msg.sessionId}:${msg.content?.trim()}`;
    if (this.recentTelegramInputs.has(inputKey)) {
      logger.debug('Skipping echo for telegram input (dedup match)', { inputKey });
      this.recentTelegramInputs.delete(inputKey); // Clean up
      return;
    }

    // BUG-002 FIX: Wait for topic creation if it's pending
    const threadId = await this.waitForTopic(msg.sessionId);
    if (threadId === undefined && this.config.useThreads) {
      logger.error('Topic creation timeout - dropping user_input', { sessionId: msg.sessionId });
      return;
    }

    logger.debug('handleUserInput', { sessionId: msg.sessionId, threadId, source, content: msg.content?.substring(0, 50) });

    // Show user input in Telegram (from CLI only)
    await this.bot.sendMessage(
      `👤 *User (cli):*\n${msg.content}`,
      { parseMode: 'Markdown' },
      threadId
    );
  }

  private async handleApprovalRequest(msg: BridgeMessage): Promise<void> {
    const approvalId = this.sessions.createApproval(msg.sessionId, msg.content);
    // BUG-002 FIX: Wait for topic creation if it's pending
    const threadId = await this.waitForTopic(msg.sessionId);
    if (threadId === undefined && this.config.useThreads) {
      logger.error('Topic creation timeout - dropping approval_request', { sessionId: msg.sessionId });
      return;
    }

    await this.bot.sendWithButtons(
      formatApprovalRequest(msg.content),
      [
        { text: '✅ Approve', callbackData: `approve:${approvalId}` },
        { text: '❌ Reject', callbackData: `reject:${approvalId}` },
        { text: '🛑 Abort', callbackData: `abort:${approvalId}` }
      ],
      { parseMode: 'Markdown' },
      threadId
    );
  }

  private async handleError(msg: BridgeMessage): Promise<void> {
    // BUG-002 FIX: Wait for topic creation if it's pending
    const threadId = await this.waitForTopic(msg.sessionId);
    if (threadId === undefined && this.config.useThreads) {
      logger.error('Topic creation timeout - dropping error message', { sessionId: msg.sessionId });
      return;
    }
    await this.bot.sendMessage(
      formatError(msg.content),
      { parseMode: 'Markdown' },
      threadId
    );
  }

  private async handlePreCompact(msg: BridgeMessage): Promise<void> {
    const trigger = msg.metadata?.trigger as string || 'auto';
    // BUG-002 FIX: Wait for topic creation if it's pending
    const threadId = await this.waitForTopic(msg.sessionId);
    if (threadId === undefined && this.config.useThreads) {
      logger.error('Topic creation timeout - dropping pre_compact message', { sessionId: msg.sessionId });
      return;
    }

    // Mark session as compacting
    this.compactingSessions.add(msg.sessionId);

    // Send notification based on trigger type
    const message = trigger === 'manual'
      ? '🔄 *Compacting session context...*\n\n_User requested /compact_'
      : '⏳ *Context limit reached*\n\n_Summarizing conversation, please wait..._';

    await this.bot.sendMessage(
      message,
      { parseMode: 'Markdown' },
      threadId
    );

    logger.info('PreCompact notification sent', { sessionId: msg.sessionId, trigger });
  }

  private async handleCompactComplete(sessionId: string): Promise<void> {
    // Clear compacting state
    this.compactingSessions.delete(sessionId);

    const threadId = this.getSessionThreadId(sessionId);

    await this.bot.sendMessage(
      '✅ *Compaction complete*\n\n_Resuming session..._',
      { parseMode: 'Markdown' },
      threadId
    );

    logger.info('Compact complete notification sent', { sessionId });
  }

  /**
   * Stop the daemon
   */
  async stop(): Promise<void> {
    if (!this.running) return;

    logger.info('Stopping bridge daemon...');

    // Clear cleanup interval
    if (this.cleanupInterval) {
      clearInterval(this.cleanupInterval);
      this.cleanupInterval = null;
    }

    // Send shutdown notification
    try {
      await this.bot.sendMessage(
        '🔴 *Bridge Daemon Stopped*\n\n' +
        'Session mirroring is now disabled.',
        { parseMode: 'Markdown' }
      );
    } catch (error) {
      logger.warn('Failed to send shutdown notification', { error });
    }

    // Stop components
    await this.bot.stop();
    await this.socket.close();
    this.sessions.close();

    this.running = false;
    logger.info('Bridge daemon stopped');
  }

  /**
   * Check if daemon is running
   */
  isRunning(): boolean {
    return this.running;
  }

  /**
   * Get daemon status
   */
  getStatus(): {
    running: boolean;
    clients: number;
    sessions: { activeSessions: number; pendingApprovals: number };
  } {
    return {
      running: this.running,
      clients: this.socket.getClientCount(),
      sessions: this.sessions.getStats()
    };
  }
}

export default BridgeDaemon;
