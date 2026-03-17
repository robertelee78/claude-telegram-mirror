/**
 * Bridge Daemon
 * Central coordinator for Claude Code ↔ Telegram bridge
 */

import { EventEmitter } from 'events';
import { TelegramBot } from '../bot/telegram.js';
import { registerCommands, registerApprovalHandlers, registerToolDetailsHandler, registerAnswerHandlers } from '../bot/commands.js';
import { SocketServer } from './socket.js';
import { SessionManager } from './session.js';
import { InputInjector } from './injector.js';
import { loadConfig, TelegramMirrorConfig, readMirrorStatus, writeMirrorStatus } from '../utils/config.js';
import {
  formatAgentResponse,
  formatToolExecution,
  formatApprovalRequest,
  formatError,
  formatSessionStart,
  formatSessionEnd
} from '../bot/formatting.js';
import logger from '../utils/logger.js';
import { summarizeToolAction, summarizeToolResult } from '../utils/summarize.js';
import type { BridgeMessage, Session } from './types.js';
import { execSync } from 'child_process';
import { randomUUID } from 'crypto';
import path from 'path';
import { existsSync, mkdirSync, readdirSync, statSync, unlinkSync, openSync, fstatSync, readSync, closeSync } from 'fs';
import { getConfigDir } from '../utils/config.js';

const MAX_SESSION_ID_LENGTH = 128;
const SESSION_ID_PATTERN = /^[a-zA-Z0-9_-]+$/;

/**
 * Validate a session ID: must be non-empty, at most 128 chars, and contain
 * only alphanumeric characters, underscores, or hyphens.
 */
export function isValidSessionId(sessionId: string | undefined): boolean {
  if (!sessionId) return false;
  if (sessionId.length > MAX_SESSION_ID_LENGTH) return false;
  return SESSION_ID_PATTERN.test(sessionId);
}

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
  private mirroringEnabled: boolean = true;
  private cleanupInterval: NodeJS.Timeout | null = null;
  private sessionThreads: Map<string, number> = new Map(); // sessionId -> threadId
  private sessionTmuxTargets: Map<string, string> = new Map(); // sessionId -> tmux target
  private recentTelegramInputs: Set<string> = new Set(); // Track recent inputs from Telegram to avoid echo
  private toolInputCache: Map<string, { tool: string; input: unknown; timestamp: number }> = new Map(); // Cache tool inputs for details button
  private compactingSessions: Set<string> = new Set(); // Track sessions currently compacting
  private sessionCustomTitles: Map<string, string> = new Map(); // Track last known custom title per session (Epic 5)

  // BUG-002 fix: Promise-based topic creation lock to prevent race conditions
  // When ensureSessionExists() creates a session before session_start arrives,
  // other handlers wait for the topic to be created via these promises
  private topicCreationPromises: Map<string, Promise<number | undefined>> = new Map();
  private topicCreationResolvers: Map<string, (threadId: number | undefined) => void> = new Map();

  // Topic auto-deletion: track pending deletions so we can cancel if session resumes
  private pendingTopicDeletions: Map<string, NodeJS.Timeout> = new Map();

  // AskUserQuestion: track pending questions awaiting user response
  private pendingQuestions: Map<string, {
    sessionId: string;
    questions: Array<{
      question: string;
      header: string;
      options: Array<{ label: string; description: string }>;
      multiSelect: boolean;
    }>;
    messageIds: number[];
    answered: boolean[];
    selectedOptions: Map<number, Set<number>>;  // questionIndex -> selected option indices
    timestamp: number;
  }> = new Map();

  constructor(config?: TelegramMirrorConfig) {
    super();
    this.config = config || loadConfig();
    this.bot = new TelegramBot(this.config);
    this.socket = new SocketServer(this.config.socketPath);
    this.sessions = new SessionManager();
    this.injector = new InputInjector();
    this.mirroringEnabled = readMirrorStatus(getConfigDir());
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
      },
      toggleMirroring: async (force?: boolean) => {
        const newState = force !== undefined ? force : !this.mirroringEnabled;
        this.mirroringEnabled = newState;
        writeMirrorStatus(getConfigDir(), newState, process.pid);
        return newState;
      },
      getMirroringEnabled: () => {
        return this.mirroringEnabled;
      },
      injectSlashCommandToThread: async (threadId: number, command: string) => {
        // Look up session by threadId, then inject the slash command via tmux
        const session = this.sessions.getSessionByThreadId(threadId);
        if (!session) return false;

        let tmuxTarget = this.sessionTmuxTargets.get(session.id);
        let tmuxSocket: string | undefined;

        if (!tmuxTarget) {
          const tmuxInfo = this.sessions.getTmuxInfo(session.id);
          tmuxTarget = tmuxInfo.target || undefined;
          tmuxSocket = tmuxInfo.socket || undefined;
          if (tmuxTarget) {
            this.sessionTmuxTargets.set(session.id, tmuxTarget);
          }
        } else {
          tmuxSocket = session.tmuxSocket;
        }

        if (!tmuxTarget) return false;

        this.injector.setTmuxSession(tmuxTarget, tmuxSocket);
        return this.injector.sendSlashCommand(command);
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

    // Register answer handlers (AskUserQuestion inline buttons)
    registerAnswerHandlers(
      this.bot.getBot(),
      (sessionId, questionIndex, optionIndex) => {
        return this.handleAnswerCallback(sessionId, questionIndex, optionIndex);
      },
      (sessionId, questionIndex, optionIndex) => {
        return this.handleToggleCallback(sessionId, questionIndex, optionIndex);
      },
      (sessionId, questionIndex) => {
        return this.handleSubmitCallback(sessionId, questionIndex);
      },
      this.config.chatId
    );

    // Start bot
    await this.bot.start();

    // Start cleanup interval (every 5 minutes)
    this.cleanupInterval = setInterval(() => {
      this.sessions.expireOldApprovals();
      this.cleanupStaleSessions();  // BUG-003: Check for stale sessions
      this.cleanupOldDownloads();   // Epic 4: Clean up old downloaded files
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
      // Validate session ID before any processing
      if (!isValidSessionId(msg.sessionId)) {
        logger.warn('Invalid session ID, dropping message', {
          sessionId: msg.sessionId ? msg.sessionId.slice(0, 20) + '...' : 'undefined',
          length: msg.sessionId?.length,
          type: msg.type
        });
        return;
      }

      logger.debug('Received socket message', { type: msg.type, sessionId: msg.sessionId });

      // Update session activity
      const session = this.sessions.getSession(msg.sessionId);
      if (session) {
        this.sessions.updateActivity(msg.sessionId);
      }

      // BUG-001 fix: Auto-update tmux target if it changed
      // Every message from hooks includes current tmux info, so we can detect moves
      this.checkAndUpdateTmuxTarget(msg);

      // Always-process types: approval_request, approval_response, command
      // All other outbound message types are gated by mirroringEnabled
      const alwaysProcess = msg.type === 'approval_request' ||
        msg.type === 'approval_response' ||
        msg.type === 'command';

      if (!this.mirroringEnabled && !alwaysProcess) {
        logger.debug('Mirroring disabled, skipping message', { type: msg.type, sessionId: msg.sessionId });
        return;
      }

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

        case 'session_rename':
          await this.handleSessionRename(msg.sessionId, msg.content);
          break;

        case 'command':
          this.handleCommand(msg);
          break;

        case 'send_image':
          await this.handleSendImage(msg);
          break;

        default:
          logger.debug('Unknown message type', { type: msg.type });
      }

      // After processing, check for custom-title rename in transcript (Epic 5)
      // This catches renames even if the bash hook doesn't send session_rename
      const transcriptPath = msg.metadata?.transcript_path as string | undefined;
      if (transcriptPath) {
        const newTitle = this.checkForSessionRename(transcriptPath, msg.sessionId);
        if (newTitle) {
          await this.handleSessionRename(msg.sessionId, newTitle);
        }
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
      logger.info('onMessage received', { text: text.substring(0, 50), chatId, threadId });

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

      // Check for cc command prefix - transform "cc clear" → "/clear"
      if (text.toLowerCase().startsWith('cc ')) {
        const command = '/' + text.slice(3).trim();
        logger.info('CC command detected, transforming', { original: text, command });

        // Track to prevent echo
        const inputKey = `${session.id}:${command}`;
        this.recentTelegramInputs.add(inputKey);
        setTimeout(() => this.recentTelegramInputs.delete(inputKey), 10000);

        // Send slash command using same method as special keys (no -l flag)
        const injected = await this.injector.sendSlashCommand(command);
        logger.info('CC command injection result', { command, injected });
        return;
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

      // Check if there's a pending AskUserQuestion for this session
      // If so, treat the text as a free-text "Other" response
      if (this.handleFreeTextAnswer(session.id, text)) {
        logger.info('Free-text answer consumed for AskUserQuestion', { sessionId: session.id });
        return;
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

    // Handle photo messages (forward to CLI as file path)
    this.bot.onPhoto(async (photo, caption, _chatId, threadId) => {
      // BUG-005 fix: Ignore General topic
      if (!threadId) return;

      const session = this.sessions.getSessionByThreadId(threadId);
      if (!session) {
        logger.debug('Ignoring photo for unknown topic', { threadId });
        return;
      }

      // Check file size (20MB limit)
      if (photo.fileSize && photo.fileSize > 20 * 1024 * 1024) {
        await this.bot.sendMessage('File too large (max 20MB)', {}, threadId);
        return;
      }

      // Download the photo
      const downloadsDir = this.ensureDownloadsDir();
      const filename = this.sanitizeFilename(`photo_${photo.fileUniqueId}.jpg`);
      const destPath = path.join(downloadsDir, filename);

      const localPath = await this.bot.downloadFile(photo.fileId, destPath);
      if (!localPath) {
        await this.bot.sendMessage('Failed to download photo', {}, threadId);
        return;
      }

      // Build injection text
      let injectionText = `[Image from Telegram: ${localPath}]`;
      if (caption) {
        injectionText += ` Caption: ${caption}`;
      }

      // Inject into tmux
      let tmuxTarget = this.sessionTmuxTargets.get(session.id);
      let tmuxSocket: string | undefined;

      if (!tmuxTarget) {
        const tmuxInfo = this.sessions.getTmuxInfo(session.id);
        tmuxTarget = tmuxInfo.target || undefined;
        tmuxSocket = tmuxInfo.socket || undefined;
        if (tmuxTarget) {
          this.sessionTmuxTargets.set(session.id, tmuxTarget);
        }
      } else {
        tmuxSocket = session.tmuxSocket;
      }

      if (tmuxTarget) {
        this.injector.setTmuxSession(tmuxTarget, tmuxSocket);
        const injected = await this.injector.inject(injectionText);
        if (injected) {
          await this.bot.sendMessage('Photo sent to Claude', {}, threadId);
        } else {
          await this.bot.sendMessage('Failed to inject photo path into session', {}, threadId);
        }
      } else {
        await this.bot.sendMessage('No tmux session found for this topic', {}, threadId);
      }
    });

    // Handle document messages (forward to CLI as file path)
    this.bot.onDocument(async (doc, caption, _chatId, threadId) => {
      // BUG-005 fix: Ignore General topic
      if (!threadId) return;

      const session = this.sessions.getSessionByThreadId(threadId);
      if (!session) {
        logger.debug('Ignoring document for unknown topic', { threadId });
        return;
      }

      // Check file size (20MB limit)
      if (doc.fileSize && doc.fileSize > 20 * 1024 * 1024) {
        await this.bot.sendMessage('File too large (max 20MB)', {}, threadId);
        return;
      }

      // Determine filename with fallback
      const originalName = doc.fileName || `unnamed.${doc.mimeType?.split('/')[1] || 'bin'}`;
      const filename = this.sanitizeFilename(originalName);
      const destPath = path.join(this.ensureDownloadsDir(), filename);

      const localPath = await this.bot.downloadFile(doc.fileId, destPath);
      if (!localPath) {
        await this.bot.sendMessage('Failed to download document', {}, threadId);
        return;
      }

      // Build injection text
      let injectionText = `[Document from Telegram: ${localPath}]`;
      if (caption) {
        injectionText += ` Caption: ${caption}`;
      }

      // Inject into tmux
      let tmuxTarget = this.sessionTmuxTargets.get(session.id);
      let tmuxSocket: string | undefined;

      if (!tmuxTarget) {
        const tmuxInfo = this.sessions.getTmuxInfo(session.id);
        tmuxTarget = tmuxInfo.target || undefined;
        tmuxSocket = tmuxInfo.socket || undefined;
        if (tmuxTarget) {
          this.sessionTmuxTargets.set(session.id, tmuxTarget);
        }
      } else {
        tmuxSocket = session.tmuxSocket;
      }

      if (tmuxTarget) {
        this.injector.setTmuxSession(tmuxTarget, tmuxSocket);
        const injected = await this.injector.inject(injectionText);
        if (injected) {
          await this.bot.sendMessage('Document sent to Claude', {}, threadId);
        } else {
          await this.bot.sendMessage('Failed to inject document path into session', {}, threadId);
        }
      } else {
        await this.bot.sendMessage('No tmux session found for this topic', {}, threadId);
      }
    });
  }

  // ============ File Download Helpers (Epic 4) ============

  /**
   * Ensure the downloads directory exists with restrictive permissions.
   */
  private ensureDownloadsDir(): string {
    const dir = path.join(getConfigDir(), 'downloads');
    if (!existsSync(dir)) {
      mkdirSync(dir, { recursive: true, mode: 0o700 });
    }
    return dir;
  }

  /**
   * Sanitize a filename to prevent path traversal and ensure uniqueness.
   */
  private sanitizeFilename(name: string): string {
    // Replace path separators
    let safe = name.replace(/[/\\]/g, '_');
    // Reject directory traversal
    if (safe.includes('..')) safe = safe.replace(/\.\./g, '_');
    // Prefix dotfiles to prevent hidden files
    if (safe.startsWith('.')) safe = '_' + safe;
    // Limit length (UUID is 36 chars + underscore = 37 prefix)
    if (safe.length > 200) safe = safe.substring(0, 200);
    // Prepend UUID for uniqueness
    return `${randomUUID()}_${safe}`;
  }

  /**
   * Clean up downloaded files older than 24 hours.
   */
  private cleanupOldDownloads(): void {
    const dir = path.join(getConfigDir(), 'downloads');
    if (!existsSync(dir)) return;

    const now = Date.now();
    const maxAge = 24 * 60 * 60 * 1000; // 24 hours

    try {
      for (const file of readdirSync(dir)) {
        const filePath = path.join(dir, file);
        try {
          const stat = statSync(filePath);
          if (now - stat.mtimeMs > maxAge) {
            unlinkSync(filePath);
            logger.debug('Cleaned up old download', { filePath });
          }
        } catch (error) {
          logger.debug('Failed to stat/delete download file', { filePath, error });
        }
      }
    } catch (error) {
      logger.debug('Failed to read downloads directory', { dir, error });
    }
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
   * 1. Sessions WITH tmux info: 24h timeout + pane must be dead OR reassigned
   * 2. Sessions WITHOUT tmux info: 1h timeout (can't verify if still active)
   */
  private async cleanupStaleSessions(): Promise<void> {
    // Different timeouts based on whether we can verify session is alive
    const TMUX_SESSION_TIMEOUT_HOURS = 24;  // With tmux: 24h + pane check
    const NO_TMUX_SESSION_TIMEOUT_HOURS = 1; // Without tmux: 1h (can't verify, clean up fast)

    // Get candidates with the shorter timeout first (1h)
    const allCandidates = this.sessions.getStaleSessionCandidates(NO_TMUX_SESSION_TIMEOUT_HOURS);

    if (allCandidates.length === 0) {
      // Still check orphaned threads even if no stale candidates
    } else {
      logger.debug('Checking stale session candidates', { count: allCandidates.length });

      const now = new Date();
      const tmuxCutoff = new Date(now.getTime() - TMUX_SESSION_TIMEOUT_HOURS * 60 * 60 * 1000);

      for (const session of allCandidates) {
        const tmuxTarget = session.tmuxTarget;
        const tmuxSocket = session.tmuxSocket;

        // Sessions WITHOUT tmux info: clean up after 1h (already filtered by query)
        if (!tmuxTarget) {
          logger.info('Cleaning up stale session (no tmux info, >1h inactive)', {
            sessionId: session.id,
            lastActivity: session.lastActivity.toISOString()
          });
          await this.handleStaleSessionCleanup(session, 'inactivity timeout (no tmux info)');
          continue;
        }

        // Sessions WITH tmux info: only process if older than 24h
        if (session.lastActivity >= tmuxCutoff) {
          logger.debug('Session with tmux info not old enough for cleanup', {
            sessionId: session.id,
            lastActivity: session.lastActivity.toISOString(),
            cutoff: tmuxCutoff.toISOString()
          });
          continue;
        }

        // Sessions WITH tmux info (24h timeout): check if pane is still alive
        const paneExists = this.isTmuxPaneAlive(tmuxTarget, tmuxSocket);
        const paneReassigned = this.sessions.isTmuxTargetOwnedByOtherSession(tmuxTarget, session.id);

        if (!paneExists || paneReassigned) {
          const reason = !paneExists ? 'pane no longer exists' : 'pane reassigned to another session';
          logger.info('Cleaning up stale session (tmux)', {
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

    // Also clean up orphaned threads (ended sessions with topics that weren't deleted)
    await this.cleanupOrphanedThreads();
  }

  /**
   * Clean up ended sessions that still have thread_ids (topic deletion failed earlier)
   */
  private async cleanupOrphanedThreads(): Promise<void> {
    const orphaned = this.sessions.getOrphanedThreadSessions();

    if (orphaned.length === 0) return;

    logger.info('Cleaning up orphaned threads', { count: orphaned.length });

    // Process in batches to avoid rate limiting
    let cleaned = 0;
    for (const session of orphaned) {
      const threadId = session.threadId;
      if (!threadId) continue;

      try {
        // Try to delete the topic
        const deleted = await this.bot.deleteForumTopic(threadId);
        if (deleted) {
          this.sessions.clearThreadId(session.id);
          cleaned++;
          logger.info('Deleted orphaned topic', { sessionId: session.id, threadId });
        } else {
          // Topic might already be deleted, clear the reference anyway
          this.sessions.clearThreadId(session.id);
          cleaned++;
          logger.debug('Cleared orphaned thread reference (topic may not exist)', { sessionId: session.id, threadId });
        }

        // Rate limit: wait 200ms between deletions
        await new Promise(resolve => setTimeout(resolve, 200));
      } catch (error) {
        logger.warn('Failed to delete orphaned topic', { sessionId: session.id, threadId, error });
        // Still clear the thread_id to avoid retrying forever on invalid topics
        this.sessions.clearThreadId(session.id);
      }

      // Stop after 50 per cycle to avoid rate limiting
      if (cleaned >= 50) {
        logger.info('Orphan cleanup batch complete, will continue next cycle', { cleaned, remaining: orphaned.length - cleaned });
        break;
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

        // Delete or close the topic based on config
        if (this.config.autoDeleteTopics) {
          const deleted = await this.bot.deleteForumTopic(threadId);
          if (deleted) {
            this.sessions.clearThreadId(session.id);
            logger.info('Deleted stale session topic', { sessionId: session.id, threadId });
          } else {
            // Fallback to close if delete fails
            await this.bot.closeForumTopic(threadId);
          }
        } else {
          await this.bot.closeForumTopic(threadId);
        }
      } catch (error) {
        logger.warn('Failed to send stale session notification', { sessionId: session.id, error });
      }
    }

    // Clean up in-memory caches
    this.sessionThreads.delete(session.id);
    this.sessionTmuxTargets.delete(session.id);
    this.sessionCustomTitles.delete(session.id);

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

    // Remove Telegram's auto-pin of the first message (it's redundant with topic name)
    if (threadId) {
      await this.bot.unpinAllTopicMessages(threadId);
    }
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
   * Create a new topic for an existing session (e.g., after topic was auto-deleted and session resumed)
   */
  private async createTopicForSession(sessionId: string, projectDir?: string): Promise<number | null> {
    // Get session to retrieve hostname
    const session = this.sessions.getSession(sessionId);
    const hostname = session?.hostname;

    const topicName = this.formatTopicName(sessionId, hostname, projectDir);
    const threadId = await this.bot.createForumTopic(topicName, 0); // Blue color

    if (threadId) {
      // Update both SQLite and in-memory cache
      this.sessions.setSessionThread(sessionId, threadId);
      this.sessionThreads.set(sessionId, threadId);
      logger.info('Created new topic for resumed session', { sessionId, threadId, topicName });

      // Send notification that session resumed with new topic
      await this.bot.sendMessage(
        '🔄 *Session resumed*\n\n_Previous topic was auto-deleted. New topic created._',
        { parseMode: 'Markdown' },
        threadId
      );

      // Remove Telegram's auto-pin
      await this.bot.unpinAllTopicMessages(threadId);
    } else {
      logger.warn('Failed to create topic for resumed session', { sessionId });
    }

    return threadId;
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

        // Cancel any pending topic deletion since session is resuming
        this.cancelPendingTopicDeletion(msg.sessionId);
      }

      // Check if topic was deleted (thread_id is NULL) - need to create new topic
      const threadId = this.getSessionThreadId(msg.sessionId);
      if (!threadId && this.config.useThreads) {
        logger.info('Session exists but topic was deleted, creating new topic', { sessionId: msg.sessionId });
        await this.createTopicForSession(msg.sessionId, existing.projectDir);
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

  // ============ Session Rename (Epic 5) ============

  /**
   * Check the session transcript JSONL for a custom-title record.
   * Returns the customTitle if found and different from last known, null otherwise.
   * Reads only the last 8KB of the file for efficiency.
   */
  private checkForSessionRename(transcriptPath: string, sessionId: string): string | null {
    if (!transcriptPath || !existsSync(transcriptPath)) return null;

    try {
      const fd = openSync(transcriptPath, 'r');
      const stat = fstatSync(fd);
      const readSize = Math.min(8192, stat.size);
      const buffer = Buffer.alloc(readSize);
      readSync(fd, buffer, 0, readSize, Math.max(0, stat.size - readSize));
      closeSync(fd);

      const tail = buffer.toString('utf8');
      const lines = tail.split('\n');

      // Search backwards for the most recent custom-title
      for (let i = lines.length - 1; i >= 0; i--) {
        const line = lines[i].trim();
        if (!line) continue;
        try {
          const record = JSON.parse(line);
          if (record.type === 'custom-title' && record.customTitle) {
            const lastKnown = this.sessionCustomTitles.get(sessionId);
            if (record.customTitle !== lastKnown) {
              this.sessionCustomTitles.set(sessionId, record.customTitle);
              return record.customTitle;
            }
            return null; // Same as before, no change
          }
        } catch {
          // Skip unparseable lines (may be partial due to tail read)
        }
      }
      return null;
    } catch (error) {
      logger.debug('Failed to check for session rename', { transcriptPath, error });
      return null;
    }
  }

  /**
   * Handle a session rename: update the Telegram forum topic name.
   * Prepends the custom title to the original topic suffix.
   */
  private async handleSessionRename(sessionId: string, customTitle: string): Promise<void> {
    const threadId = this.getSessionThreadId(sessionId);
    if (!threadId) {
      logger.debug('No thread for session rename', { sessionId, customTitle });
      return;
    }

    // Get the original topic name components
    const session = this.sessions.getSession(sessionId);
    const hostname = session?.hostname || '';
    const projectDir = session?.projectDir || '';

    // Build new topic name: "Custom Title | hostname . project (abc123)"
    const suffix = this.formatTopicName(sessionId, hostname, projectDir);
    const newName = `${customTitle} | ${suffix}`.slice(0, 128); // Telegram 128-char limit

    logger.info('Renaming forum topic', { sessionId, customTitle, newName });

    const success = await this.bot.editForumTopic(threadId, newName);
    if (success) {
      await this.bot.sendMessage(
        `Topic renamed: *${customTitle}*`,
        { parseMode: 'Markdown' },
        threadId
      );
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

      // Handle topic cleanup based on config
      if (threadId) {
        if (this.config.autoDeleteTopics) {
          // Schedule topic deletion after delay (allows for session resume via claude -c)
          const delayMs = this.config.topicDeleteDelayMinutes * 60 * 1000;
          logger.info('Scheduling topic deletion', {
            sessionId: msg.sessionId,
            threadId,
            delayMinutes: this.config.topicDeleteDelayMinutes
          });

          const timeoutHandle = setTimeout(async () => {
            await this.executeTopicDeletion(msg.sessionId, threadId);
          }, delayMs);

          this.pendingTopicDeletions.set(msg.sessionId, timeoutHandle);
        } else {
          // Just close the topic (legacy behavior)
          await this.bot.closeForumTopic(threadId);
          this.sessionThreads.delete(msg.sessionId);
        }
      }

      // Clean up tmux target
      this.sessionTmuxTargets.delete(msg.sessionId);

      // Clean up custom title tracking (Epic 5)
      this.sessionCustomTitles.delete(msg.sessionId);

      // Clean up pending questions
      this.cleanupPendingQuestions(msg.sessionId);

      this.sessions.endSession(msg.sessionId);
    }
  }

  /**
   * Execute topic deletion after delay timer fires
   * Clears thread_id so session can be resumed with a new topic
   */
  private async executeTopicDeletion(sessionId: string, threadId: number): Promise<void> {
    // Remove from pending deletions
    this.pendingTopicDeletions.delete(sessionId);

    // Delete the topic from Telegram
    const deleted = await this.bot.deleteForumTopic(threadId);

    if (deleted) {
      logger.info('Auto-deleted forum topic', { sessionId, threadId });

      // Clear thread_id in memory
      this.sessionThreads.delete(sessionId);

      // Clear thread_id in SQLite (allows topic recreation on session resume)
      this.sessions.clearThreadId(sessionId);
    } else {
      // Deletion failed - fall back to closing
      logger.warn('Failed to delete topic, falling back to close', { sessionId, threadId });
      await this.bot.closeForumTopic(threadId);
      this.sessionThreads.delete(sessionId);
    }
  }

  /**
   * Cancel pending topic deletion (called when session resumes)
   */
  private cancelPendingTopicDeletion(sessionId: string): boolean {
    const timeoutHandle = this.pendingTopicDeletions.get(sessionId);
    if (timeoutHandle) {
      clearTimeout(timeoutHandle);
      this.pendingTopicDeletions.delete(sessionId);
      logger.info('Cancelled pending topic deletion (session resumed)', { sessionId });
      return true;
    }
    return false;
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
    const toolName = msg.metadata?.tool as string || 'Unknown';

    // Intercept AskUserQuestion tool - always handle regardless of verbose mode
    if (toolName === 'AskUserQuestion') {
      await this.handleAskUserQuestion(msg);
      return;
    }

    // Only show tool starts in verbose mode to avoid noise
    if (!this.config.verbose) return;

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

    // Generate human-readable summary
    const summary = toolInput
      ? summarizeToolAction(toolName, toolInput)
      : `Using ${toolName}`;

    // Send with "Details" button if there's any input to show
    if (toolInput && Object.keys(toolInput).length > 0) {
      await this.bot.sendWithButtons(
        `🔧 ${summary}\n    Tool: \`${toolName}\`${preview}`,
        [{ text: '📋 Details', callbackData: `tooldetails:${toolUseId}` }],
        { parseMode: 'Markdown' },
        threadId
      );
    } else {
      await this.bot.sendMessage(
        `🔧 ${summary}\n    Tool: \`${toolName}\`${preview}`,
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

    // Augment with human-readable result summary in verbose mode
    const resultSummary = toolOutput
      ? summarizeToolResult(toolName, toolOutput)
      : 'Completed (no output)';
    const formatted = formatToolExecution(toolName, toolInput, toolOutput, this.config.verbose);

    await this.bot.sendMessage(
      `✅ ${resultSummary}\n${formatted}`,
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

  // ============ AskUserQuestion Handling (Epic 3) ============

  /**
   * Handle AskUserQuestion tool - render questions as Telegram inline buttons
   */
  private async handleAskUserQuestion(msg: BridgeMessage): Promise<void> {
    const threadId = await this.waitForTopic(msg.sessionId);
    if (threadId === undefined && this.config.useThreads) {
      logger.error('Topic creation timeout - dropping AskUserQuestion', { sessionId: msg.sessionId });
      return;
    }

    const toolInput = msg.metadata?.input as Record<string, unknown> | undefined;
    if (!toolInput) {
      logger.warn('AskUserQuestion with no input', { sessionId: msg.sessionId });
      return;
    }

    // Parse questions from tool input
    const questions = toolInput.questions as Array<{
      question: string;
      header: string;
      options: Array<{ label: string; description: string }>;
      multiSelect: boolean;
    }> | undefined;

    if (!questions || questions.length === 0) {
      logger.warn('AskUserQuestion with no questions', { sessionId: msg.sessionId });
      return;
    }

    // Truncate sessionId for callback data (Telegram 64-byte limit)
    const shortSessionId = msg.sessionId.slice(0, 20);

    // Create pending question entry
    const pendingKey = shortSessionId;
    this.pendingQuestions.set(pendingKey, {
      sessionId: msg.sessionId,
      questions,
      messageIds: [],
      answered: new Array(questions.length).fill(false),
      selectedOptions: new Map(),
      timestamp: Date.now(),
    });

    // Set 10-minute expiry
    setTimeout(() => {
      const pending = this.pendingQuestions.get(pendingKey);
      if (pending && Date.now() - pending.timestamp >= 10 * 60 * 1000) {
        this.pendingQuestions.delete(pendingKey);
        logger.debug('Expired pending question', { sessionId: msg.sessionId });
      }
    }, 10 * 60 * 1000);

    // Render each question as a separate message
    for (let qIdx = 0; qIdx < questions.length; qIdx++) {
      const q = questions[qIdx];

      // Build message text
      let text = `❓ *${this.escapeMarkdown(q.header)}*\n\n${this.escapeMarkdown(q.question)}\n`;
      for (const opt of q.options) {
        text += `\n• *${this.escapeMarkdown(opt.label)}* — ${this.escapeMarkdown(opt.description)}`;
      }
      text += '\n\n_Or type your answer in this topic_';

      // Build inline keyboard buttons - one per row for mobile readability
      const buttons: Array<{ text: string; callbackData: string }> = [];

      if (q.multiSelect) {
        // Initialize selected options set for this question
        const pending = this.pendingQuestions.get(pendingKey)!;
        pending.selectedOptions.set(qIdx, new Set());

        for (let oIdx = 0; oIdx < q.options.length; oIdx++) {
          const opt = q.options[oIdx];
          const callbackData = `toggle:${shortSessionId}:${qIdx}:${oIdx}`;
          buttons.push({ text: opt.label, callbackData });
        }
        // Submit button
        buttons.push({ text: '✅ Submit', callbackData: `submit:${shortSessionId}:${qIdx}` });
      } else {
        // Single-select: answer buttons
        for (let oIdx = 0; oIdx < q.options.length; oIdx++) {
          const opt = q.options[oIdx];
          const callbackData = `answer:${shortSessionId}:${qIdx}:${oIdx}`;
          buttons.push({ text: opt.label, callbackData });
        }
      }

      await this.bot.sendWithButtons(text, buttons, { parseMode: 'Markdown' }, threadId);
    }

    logger.info('AskUserQuestion rendered', {
      sessionId: msg.sessionId,
      questionCount: questions.length,
    });
  }

  /**
   * Escape markdown special characters for Telegram Markdown mode
   */
  private escapeMarkdown(text: string): string {
    // In Telegram Markdown (v1) mode, backticks are special
    // Replace them to avoid breaking formatting
    return text.replace(/([`])/g, "'");
  }

  /**
   * Handle single-select answer callback
   */
  private handleAnswerCallback(sessionId: string, questionIndex: number, optionIndex: number): string | undefined {
    const pending = this.pendingQuestions.get(sessionId);
    if (!pending) {
      return 'Question expired';
    }
    if (pending.answered[questionIndex]) {
      return 'Already answered';
    }

    pending.answered[questionIndex] = true;

    const question = pending.questions[questionIndex];
    const option = question?.options[optionIndex];
    const answerText = option?.label || String(optionIndex + 1);

    // Inject into tmux
    const tmuxTarget = this.sessionTmuxTargets.get(pending.sessionId);
    if (tmuxTarget) {
      const session = this.sessions.getSession(pending.sessionId);
      const tmuxSocket = session?.tmuxSocket;
      this.injector.setTmuxSession(tmuxTarget, tmuxSocket);
      this.injector.inject(answerText);
    } else {
      logger.warn('No tmux target for answer injection', { sessionId: pending.sessionId });
    }

    // Clean up if all questions answered
    if (pending.answered.every(a => a)) {
      this.pendingQuestions.delete(sessionId);
    }

    logger.info('AskUserQuestion answered (single-select)', {
      sessionId: pending.sessionId,
      questionIndex,
      optionIndex,
      answer: answerText,
    });

    return undefined;
  }

  /**
   * Handle multi-select toggle callback
   */
  private handleToggleCallback(
    sessionId: string,
    questionIndex: number,
    optionIndex: number
  ): { error?: string; labels?: string[] } {
    const pending = this.pendingQuestions.get(sessionId);
    if (!pending) {
      return { error: 'Question expired' };
    }
    if (pending.answered[questionIndex]) {
      return { error: 'Already submitted' };
    }

    let selected = pending.selectedOptions.get(questionIndex);
    if (!selected) {
      selected = new Set();
      pending.selectedOptions.set(questionIndex, selected);
    }

    // Toggle the option
    if (selected.has(optionIndex)) {
      selected.delete(optionIndex);
    } else {
      selected.add(optionIndex);
    }

    // Build updated labels with checkmarks
    const question = pending.questions[questionIndex];
    const labels = question.options.map((opt, idx) => {
      return selected!.has(idx) ? `✓ ${opt.label}` : opt.label;
    });

    logger.debug('AskUserQuestion toggle', {
      sessionId: pending.sessionId,
      questionIndex,
      optionIndex,
      selected: Array.from(selected),
    });

    return { labels };
  }

  /**
   * Handle multi-select submit callback
   */
  private handleSubmitCallback(sessionId: string, questionIndex: number): string | undefined {
    const pending = this.pendingQuestions.get(sessionId);
    if (!pending) {
      return 'Question expired';
    }
    if (pending.answered[questionIndex]) {
      return 'Already submitted';
    }

    pending.answered[questionIndex] = true;

    const question = pending.questions[questionIndex];
    const selected = pending.selectedOptions.get(questionIndex) || new Set<number>();

    // Build answer text from selected option labels
    const selectedLabels = Array.from(selected)
      .sort((a, b) => a - b)
      .map(idx => question.options[idx]?.label)
      .filter(Boolean);

    const answerText = selectedLabels.length > 0
      ? selectedLabels.join(', ')
      : 'none';

    // Inject into tmux
    const tmuxTarget = this.sessionTmuxTargets.get(pending.sessionId);
    if (tmuxTarget) {
      const session = this.sessions.getSession(pending.sessionId);
      const tmuxSocket = session?.tmuxSocket;
      this.injector.setTmuxSession(tmuxTarget, tmuxSocket);
      this.injector.inject(answerText);
    } else {
      logger.warn('No tmux target for answer injection', { sessionId: pending.sessionId });
    }

    // Clean up if all questions answered
    if (pending.answered.every(a => a)) {
      this.pendingQuestions.delete(sessionId);
    }

    logger.info('AskUserQuestion submitted (multi-select)', {
      sessionId: pending.sessionId,
      questionIndex,
      selected: selectedLabels,
    });

    return undefined;
  }

  /**
   * Find pending question for a session by full sessionId
   * Used for free-text "Other" responses from the text message handler
   */
  private findPendingQuestionBySessionId(sessionId: string): string | undefined {
    for (const [key, pending] of this.pendingQuestions) {
      if (pending.sessionId === sessionId && !pending.answered.every(a => a)) {
        return key;
      }
    }
    return undefined;
  }

  /**
   * Handle free-text response to a pending AskUserQuestion
   * Returns true if the text was consumed as an answer
   */
  private handleFreeTextAnswer(sessionId: string, text: string): boolean {
    const pendingKey = this.findPendingQuestionBySessionId(sessionId);
    if (!pendingKey) return false;

    const pending = this.pendingQuestions.get(pendingKey);
    if (!pending) return false;

    // Find first unanswered question
    const qIdx = pending.answered.findIndex(a => !a);
    if (qIdx === -1) return false;

    pending.answered[qIdx] = true;

    // Inject the free-text answer into tmux
    const tmuxTarget = this.sessionTmuxTargets.get(pending.sessionId);
    if (tmuxTarget) {
      const session = this.sessions.getSession(pending.sessionId);
      const tmuxSocket = session?.tmuxSocket;
      this.injector.setTmuxSession(tmuxTarget, tmuxSocket);
      this.injector.inject(text);
    }

    // Clean up if all questions answered
    if (pending.answered.every(a => a)) {
      this.pendingQuestions.delete(pendingKey);
    }

    logger.info('AskUserQuestion answered (free-text)', {
      sessionId: pending.sessionId,
      questionIndex: qIdx,
      answer: text.substring(0, 50),
    });

    return true;
  }

  /**
   * Clean up pending questions for a session (called on session end)
   */
  private cleanupPendingQuestions(sessionId: string): void {
    for (const [key, pending] of this.pendingQuestions) {
      if (pending.sessionId === sessionId) {
        this.pendingQuestions.delete(key);
      }
    }
  }

  // ============ Toggle Command Handler (Epic 1) ============

  private async handleCommand(msg: BridgeMessage): Promise<void> {
    const cmd = msg.content.trim().toLowerCase();
    let newState: boolean;
    switch (cmd) {
      case 'toggle':
        newState = !this.mirroringEnabled;
        break;
      case 'enable':
      case 'on':
        newState = true;
        break;
      case 'disable':
      case 'off':
        newState = false;
        break;
      default:
        return;
    }
    this.mirroringEnabled = newState;
    writeMirrorStatus(getConfigDir(), newState, process.pid);
    const statusText = newState
      ? '\u{1F7E2} *Telegram mirroring: ON*'
      : '\u{1F534} *Telegram mirroring: OFF*';
    await this.bot.sendMessage(statusText, { parseMode: 'Markdown' });
  }

  // ============ Outbound File Transfer Handler (Epic 2) ============

  private static readonly IMAGE_EXTENSIONS = new Set(['jpg', 'jpeg', 'png', 'gif', 'webp', 'bmp']);
  private static readonly MAX_UPLOAD_BYTES = 50 * 1024 * 1024;

  private async handleSendImage(msg: BridgeMessage): Promise<void> {
    const filePath = msg.content;

    // Validate path
    if (!path.isAbsolute(filePath)) {
      logger.warn('SendImage: path must be absolute', { path: filePath });
      return;
    }
    if (filePath.includes('..')) {
      logger.warn('SendImage: path must not contain ..', { path: filePath });
      return;
    }
    if (!existsSync(filePath)) {
      logger.warn('SendImage: file not found', { path: filePath });
      return;
    }
    const stats = statSync(filePath);
    if (stats.size > BridgeDaemon.MAX_UPLOAD_BYTES) {
      logger.warn('SendImage: file exceeds 50MB', { path: filePath, size: stats.size });
      return;
    }

    const threadId = this.getSessionThreadId(msg.sessionId);
    if (!threadId) {
      return;
    }

    const ext = path.extname(filePath).toLowerCase().replace('.', '');
    const caption = msg.metadata?.caption as string | undefined;

    try {
      if (BridgeDaemon.IMAGE_EXTENSIONS.has(ext)) {
        await this.bot.sendPhoto(filePath, caption, threadId);
      } else {
        await this.bot.sendDocument(filePath, caption, threadId);
      }
      logger.info('Sent file to Telegram', { path: filePath, sessionId: msg.sessionId });
    } catch (err) {
      logger.warn('Failed to send file to Telegram', { path: filePath, error: err });
    }
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
