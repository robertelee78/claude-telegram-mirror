/**
 * Bot Command Handlers
 * Implements all Telegram bot commands
 */

import { Bot, Context, SessionFlavor, InlineKeyboard } from 'grammy';
import {
  formatHelp,
  formatStatus,
  formatToolDetails
} from './formatting.js';
import logger from '../utils/logger.js';

// Session data type (must match telegram.ts)
interface SessionData {
  attachedSessionId: string | null;
  muted: boolean;
  lastActivity: number;
}

type BotContext = Context & SessionFlavor<SessionData>;

// Bridge interface for command callbacks
export interface BridgeCallbacks {
  getActiveSessions(): Promise<Array<{ id: string; startedAt: Date; projectDir?: string }>>;
  abortSession(sessionId: string): Promise<boolean>;
  sendToSession(sessionId: string, text: string): Promise<boolean>;
  /** Inject a command into Claude Code via tmux (looks up session by threadId) */
  injectCommand(threadId: number, command: string): Promise<{ success: boolean; error?: string }>;
}

// Claude Code commands that can be forwarded via "cc <command>" pattern
// Bot commands (still use /): /start, /help, /status, /sessions, /attach, /detach, /mute, /unmute, /abort, /ping
// Claude Code commands use "cc" prefix: cc clear, cc compact, cc config, etc.
const CLAUDE_CODE_COMMANDS = [
  'clear',      // Clear conversation history
  'compact',    // Compress context
  'config',     // Open config panel
  'cost',       // Show token usage
  'context',    // View context usage
  'memory',     // Edit CLAUDE.md
  'model',      // Switch model
  'permissions',// Manage permissions
  'review',     // Code review
  'init',       // Initialize project
  'login',      // Login to Anthropic
  'logout',     // Logout
  'doctor',     // Run diagnostics
  'vim',        // Toggle vim mode
  'resume',     // Resume conversation
  'pr',         // Create PR (skill)
  'commit',     // Create commit (skill)
] as const;

/**
 * Register all bot commands
 */
export function registerCommands(
  bot: Bot<BotContext>,
  bridge?: BridgeCallbacks
): void {
  // /start - Welcome message
  bot.command('start', async (ctx) => {
    await ctx.reply(
      '👋 *Claude Code Mirror Bot*\n\n' +
      'I mirror your Claude Code sessions to Telegram, allowing you to:\n' +
      '• Monitor agent progress from your phone\n' +
      '• Send responses and commands remotely\n' +
      '• Approve/reject actions via buttons\n\n' +
      'Use /help to see all available commands.',
      { parse_mode: 'Markdown' }
    );
    logger.info('User started bot');
  });

  // /help - Show commands
  bot.command('help', async (ctx) => {
    await ctx.reply(formatHelp(), { parse_mode: 'Markdown' });
  });

  // /status - Current session status
  bot.command('status', async (ctx) => {
    const session = ctx.session;
    const hasSession = !!session?.attachedSessionId;

    await ctx.reply(
      formatStatus(hasSession, session?.attachedSessionId || undefined, session?.muted),
      { parse_mode: 'Markdown' }
    );
  });

  // /sessions - List active sessions
  bot.command('sessions', async (ctx) => {
    if (!bridge) {
      await ctx.reply('⚠️ Bridge not connected. No session info available.');
      return;
    }

    try {
      const sessions = await bridge.getActiveSessions();

      if (sessions.length === 0) {
        await ctx.reply('📭 No active sessions.');
        return;
      }

      let message = '📋 *Active Sessions:*\n\n';
      sessions.forEach((s, idx) => {
        const age = Math.floor((Date.now() - s.startedAt.getTime()) / 60000);
        message += `${idx + 1}. \`${s.id}\`\n`;
        message += `   Started: ${age}m ago\n`;
        if (s.projectDir) {
          message += `   Project: \`${s.projectDir}\`\n`;
        }
        message += '\n';
      });

      message += '_Use /attach <id> to attach to a session_';

      await ctx.reply(message, { parse_mode: 'Markdown' });
    } catch (error) {
      logger.error('Failed to get sessions', { error });
      await ctx.reply('❌ Failed to fetch sessions.');
    }
  });

  // /attach <id> - Attach to session
  bot.command('attach', async (ctx) => {
    const sessionId = ctx.match?.trim();

    if (!sessionId) {
      await ctx.reply(
        '⚠️ Please provide a session ID.\n\n' +
        'Usage: `/attach <session-id>`\n\n' +
        'Use /sessions to see available sessions.',
        { parse_mode: 'Markdown' }
      );
      return;
    }

    ctx.session.attachedSessionId = sessionId;
    ctx.session.muted = false;

    await ctx.reply(
      `✅ Attached to session \`${sessionId}\`\n\n` +
      'You will now receive updates from this session.\n' +
      'Reply with text to send input.',
      { parse_mode: 'Markdown' }
    );

    logger.info('Attached to session', { sessionId });
  });

  // /detach - Detach from session
  bot.command('detach', async (ctx) => {
    const currentSession = ctx.session.attachedSessionId;

    if (!currentSession) {
      await ctx.reply('ℹ️ You are not attached to any session.');
      return;
    }

    ctx.session.attachedSessionId = null;

    await ctx.reply(
      `🔌 Detached from session \`${currentSession}\`\n\n` +
      'You will no longer receive updates.',
      { parse_mode: 'Markdown' }
    );

    logger.info('Detached from session', { sessionId: currentSession });
  });

  // /mute - Mute notifications
  bot.command('mute', async (ctx) => {
    if (ctx.session.muted) {
      await ctx.reply('🔇 Notifications already muted.');
      return;
    }

    ctx.session.muted = true;
    await ctx.reply(
      '🔇 Notifications muted.\n\n' +
      'Use /unmute to resume.',
      { parse_mode: 'Markdown' }
    );

    logger.info('Notifications muted');
  });

  // /unmute - Resume notifications
  bot.command('unmute', async (ctx) => {
    if (!ctx.session.muted) {
      await ctx.reply('🔔 Notifications already active.');
      return;
    }

    ctx.session.muted = false;
    await ctx.reply('🔔 Notifications resumed.');

    logger.info('Notifications unmuted');
  });

  // /abort - Abort current session
  bot.command('abort', async (ctx) => {
    const sessionId = ctx.session.attachedSessionId;

    if (!sessionId) {
      await ctx.reply('⚠️ No session attached. Use /attach first.');
      return;
    }

    // Show confirmation
    const keyboard = new InlineKeyboard()
      .text('🛑 Yes, abort', `confirm_abort:${sessionId}`)
      .text('❌ Cancel', 'cancel_abort');

    await ctx.reply(
      `⚠️ *Abort Session?*\n\n` +
      `This will terminate session \`${sessionId}\`.\n\n` +
      'Are you sure?',
      {
        parse_mode: 'Markdown',
        reply_markup: keyboard
      }
    );
  });

  // Handle abort confirmation
  bot.callbackQuery(/^confirm_abort:(.+)$/, async (ctx) => {
    const sessionId = ctx.match![1];

    if (bridge) {
      try {
        const success = await bridge.abortSession(sessionId);
        if (success) {
          ctx.session.attachedSessionId = null;
          await ctx.editMessageText(
            `🛑 Session \`${sessionId}\` aborted.`,
            { parse_mode: 'Markdown' }
          );
        } else {
          await ctx.editMessageText('❌ Failed to abort session.');
        }
      } catch (error) {
        logger.error('Failed to abort session', { sessionId, error });
        await ctx.editMessageText('❌ Error aborting session.');
      }
    } else {
      ctx.session.attachedSessionId = null;
      await ctx.editMessageText(
        `🛑 Detached from session \`${sessionId}\`.\n\n` +
        '_(Bridge not connected - session may still be running)_',
        { parse_mode: 'Markdown' }
      );
    }

    await ctx.answerCallbackQuery();
  });

  // Handle abort cancellation
  bot.callbackQuery('cancel_abort', async (ctx) => {
    await ctx.editMessageText('✅ Abort cancelled.');
    await ctx.answerCallbackQuery();
  });

  // /ping - Simple health check
  bot.command('ping', async (ctx) => {
    const start = Date.now();
    const msg = await ctx.reply('🏓 Pong!');
    const latency = Date.now() - start;

    await ctx.api.editMessageText(
      ctx.chat.id,
      msg.message_id,
      `🏓 Pong! _${latency}ms_`,
      { parse_mode: 'Markdown' }
    );
  });

  // ============ Claude Code Command Passthrough ============
  // Use "cc <command>" pattern instead of "/command" because slash commands
  // cannot be passed correctly inside Telegram topics/threads.
  // Examples: "cc clear", "cc compact", "cc my-skill"

  // Match "cc <command>" as plain text (case insensitive)
  // This uses hears() instead of command() so it works as regular text in topics
  bot.hears(/^cc\s+(.+)$/i, async (ctx) => {
    const threadId = ctx.message?.message_thread_id;
    const args = ctx.match![1].trim();

    if (!args) {
      await ctx.reply(
        '📚 *Claude Code Command Passthrough*\n\n' +
        'Usage: `cc <command>` - forwards to Claude Code\n\n' +
        '*Examples:*\n' +
        '• `cc clear` → clears conversation\n' +
        '• `cc compact` → compresses context\n' +
        '• `cc my-skill` → runs custom skill\n' +
        '• `cc review src/` → code review\n\n' +
        '*Available commands:*\n' +
        CLAUDE_CODE_COMMANDS.map(c => `cc ${c}`).join(', '),
        { parse_mode: 'Markdown' }
      );
      return;
    }

    if (!threadId) {
      await ctx.reply(
        '⚠️ Use `cc` in a session topic, not the General channel.',
        { parse_mode: 'Markdown' }
      );
      return;
    }

    if (!bridge?.injectCommand) {
      await ctx.reply('⚠️ Bridge not connected. Cannot send command to Claude.');
      return;
    }

    // Convert "cc clear" to "/clear" for Claude Code
    // Claude Code CLI still requires the slash prefix
    const fullCommand = args.startsWith('/') ? args : `/${args}`;

    const result = await bridge.injectCommand(threadId, fullCommand);

    if (result.success) {
      await ctx.reply(`➡️ \`${fullCommand}\``, { parse_mode: 'Markdown' });
      logger.info('Forwarded Claude Code command via cc', { command: fullCommand, threadId });
    } else {
      await ctx.reply(
        `⚠️ *Could not send command*\n\n${result.error || 'No session found for this topic.'}`,
        { parse_mode: 'Markdown' }
      );
      logger.warn('Failed to forward command via cc', { command: fullCommand, threadId, error: result.error });
    }
  });

  // Also support bare "cc" to show help
  bot.hears(/^cc$/i, async (ctx) => {
    await ctx.reply(
      '📚 *Claude Code Command Passthrough*\n\n' +
      'Usage: `cc <command>` - forwards to Claude Code\n\n' +
      '*Examples:*\n' +
      '• `cc clear` → clears conversation\n' +
      '• `cc compact` → compresses context\n' +
      '• `cc my-skill` → runs custom skill\n\n' +
      '*Available commands:*\n' +
      CLAUDE_CODE_COMMANDS.map(c => `cc ${c}`).join(', '),
      { parse_mode: 'Markdown' }
    );
  });
}

/**
 * Create approval keyboard
 */
export function createApprovalKeyboard(approvalId: string): InlineKeyboard {
  return new InlineKeyboard()
    .text('✅ Approve', `approve:${approvalId}`)
    .text('❌ Reject', `reject:${approvalId}`)
    .row()
    .text('🛑 Abort Session', `abort:${approvalId}`);
}

/**
 * Register approval handlers
 */
export function registerApprovalHandlers(
  bot: Bot<BotContext>,
  onApproval: (approvalId: string, action: 'approve' | 'reject' | 'abort') => Promise<void>
): void {
  bot.callbackQuery(/^(approve|reject|abort):(.+)$/, async (ctx) => {
    const action = ctx.match![1] as 'approve' | 'reject' | 'abort';
    const approvalId = ctx.match![2];

    try {
      await onApproval(approvalId, action);

      // Update message to show decision
      const actionText = {
        approve: '✅ Approved',
        reject: '❌ Rejected',
        abort: '🛑 Session Aborted'
      }[action];

      const originalText = ctx.callbackQuery.message?.text || '';

      // Try to update with markdown, fall back to plain text if parsing fails
      try {
        await ctx.editMessageText(
          `${originalText}\n\nDecision: ${actionText}`,
          { parse_mode: undefined }  // Plain text to avoid markdown conflicts
        );
      } catch (editError) {
        // If edit fails, try without the original text
        logger.warn('Failed to edit with original text, using simple format', { editError });
        await ctx.editMessageText(`Decision: ${actionText}`);
      }

      await ctx.answerCallbackQuery({ text: `${actionText}!` });

    } catch (error) {
      logger.error('Failed to process approval', { approvalId, action, error });
      await ctx.answerCallbackQuery({ text: 'Error processing response' });
    }
  });
}

/**
 * Register tool details handler
 */
export function registerToolDetailsHandler(
  bot: Bot<BotContext>,
  getToolDetails: (toolUseId: string) => { tool: string; input: unknown } | undefined
): void {
  bot.callbackQuery(/^tooldetails:(.+)$/, async (ctx) => {
    const toolUseId = ctx.match![1];

    try {
      const details = getToolDetails(toolUseId);

      if (!details) {
        await ctx.answerCallbackQuery({ text: 'Details expired (5 min cache)', show_alert: true });
        return;
      }

      // Format the tool details nicely for mobile
      const formattedDetails = formatToolDetails(details.tool, details.input);

      // Reply with full details (don't edit original - keep it clean)
      await ctx.reply(
        formattedDetails,
        {
          parse_mode: 'Markdown',
          reply_parameters: { message_id: ctx.callbackQuery.message?.message_id || 0 }
        }
      );

      await ctx.answerCallbackQuery();

    } catch (error) {
      logger.error('Failed to show tool details', { toolUseId, error });
      await ctx.answerCallbackQuery({ text: 'Error loading details' });
    }
  });
}

export default registerCommands;
