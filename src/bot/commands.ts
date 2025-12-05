/**
 * Bot Command Handlers
 * Implements all Telegram bot commands
 */

import { Bot, Context, SessionFlavor, InlineKeyboard } from 'grammy';
import {
  formatHelp,
  formatStatus
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
}

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
      await ctx.editMessageText(
        `${originalText}\n\n*Decision:* ${actionText}`,
        { parse_mode: 'Markdown' }
      );

      await ctx.answerCallbackQuery({ text: `${actionText}!` });

    } catch (error) {
      logger.error('Failed to process approval', { approvalId, action, error });
      await ctx.answerCallbackQuery({ text: 'Error processing response' });
    }
  });
}

export default registerCommands;
