/**
 * Telegram Bot Implementation
 * Core bot using grammY with rate limiting and message queue
 */

import { Bot, Context, session, SessionFlavor, GrammyError, HttpError } from 'grammy';
import { InlineKeyboard } from 'grammy';
import { loadConfig, TelegramMirrorConfig } from '../utils/config.js';
import { chunkMessage } from '../utils/chunker.js';
import logger from '../utils/logger.js';
import type { SendOptions, InlineButton, MessageQueueItem } from './types.js';

// Session data type
interface SessionData {
  attachedSessionId: string | null;
  muted: boolean;
  lastActivity: number;
}

type BotContext = Context & SessionFlavor<SessionData>;

/**
 * Message queue for rate limiting
 */
class MessageQueue {
  private queue: MessageQueueItem[] = [];
  private processing = false;
  private rateLimit: number;

  constructor(rateLimit: number = 1) {
    this.rateLimit = rateLimit;
  }

  add(item: Omit<MessageQueueItem, 'retries' | 'createdAt'>): void {
    this.queue.push({
      ...item,
      retries: 0,
      createdAt: new Date()
    });
    this.process();
  }

  private async process(): Promise<void> {
    if (this.processing || this.queue.length === 0) return;

    this.processing = true;

    while (this.queue.length > 0) {
      const item = this.queue.shift()!;

      try {
        await this.sendItem(item);
      } catch (error) {
        if (item.retries < 3) {
          item.retries++;
          // Exponential backoff
          await this.delay(1000 * Math.pow(2, item.retries));
          this.queue.unshift(item);
        } else {
          logger.error('Failed to send message after retries', { error });
        }
      }

      // Rate limiting delay
      await this.delay(1000 / this.rateLimit);
    }

    this.processing = false;
  }

  private async sendItem(_item: MessageQueueItem): Promise<void> {
    // Implemented by TelegramBot class
    throw new Error('sendItem must be implemented');
  }

  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }

  setHandler(handler: (item: MessageQueueItem) => Promise<void>): void {
    this.sendItem = handler;
  }
}

/**
 * Telegram Bot Class
 */
export class TelegramBot {
  private bot: Bot<BotContext>;
  private config: TelegramMirrorConfig;
  private messageQueue: MessageQueue;
  private running = false;
  private messageHandlers: Array<(text: string, chatId: number, threadId?: number) => void> = [];
  private callbackHandlers: Array<(data: string, chatId: number) => void> = [];

  constructor(config?: TelegramMirrorConfig) {
    this.config = config || loadConfig();
    this.bot = new Bot<BotContext>(this.config.botToken);
    this.messageQueue = new MessageQueue(this.config.rateLimit);

    this.setupMiddleware();
    this.setupErrorHandling();
    this.setupMessageQueueHandler();
  }

  /**
   * Setup middleware
   */
  private setupMiddleware(): void {
    // Session middleware
    this.bot.use(session({
      initial: (): SessionData => ({
        attachedSessionId: null,
        muted: false,
        lastActivity: Date.now()
      })
    }));

    // Security: Only respond to configured chat
    this.bot.use(async (ctx, next) => {
      const chatId = ctx.chat?.id;

      if (chatId && chatId !== this.config.chatId) {
        logger.warn('Unauthorized access attempt', { chatId });
        await ctx.reply('⛔ Unauthorized. This bot is private.');
        return;
      }

      // Update activity timestamp
      if (ctx.session) {
        ctx.session.lastActivity = Date.now();
      }

      await next();
    });
  }

  /**
   * Setup error handling
   */
  private setupErrorHandling(): void {
    this.bot.catch((err) => {
      const ctx = err.ctx;
      const error = err.error;

      logger.error('Bot error', {
        updateType: ctx.update.update_id,
        error: error instanceof Error ? error.message : String(error)
      });

      if (error instanceof GrammyError) {
        logger.error('Telegram API error', {
          code: error.error_code,
          description: error.description
        });
      } else if (error instanceof HttpError) {
        logger.error('Network error', { error: error.message });
      }
    });
  }

  /**
   * Setup message queue handler
   */
  private setupMessageQueueHandler(): void {
    this.messageQueue.setHandler(async (item) => {
      const threadId = item.threadId || item.options?.threadId;
      const parseMode = item.options?.parseMode || 'Markdown';

      try {
        if (item.buttons && item.buttons.length > 0) {
          const keyboard = new InlineKeyboard();
          item.buttons.forEach((btn, idx) => {
            keyboard.text(btn.text, btn.callbackData);
            if ((idx + 1) % 2 === 0) keyboard.row();
          });

          await this.bot.api.sendMessage(item.chatId, item.text, {
            parse_mode: parseMode,
            disable_notification: item.options?.disableNotification,
            message_thread_id: threadId,
            reply_markup: keyboard
          });
        } else {
          await this.bot.api.sendMessage(item.chatId, item.text, {
            parse_mode: parseMode,
            disable_notification: item.options?.disableNotification,
            message_thread_id: threadId
          });
        }
      } catch (error) {
        if (error instanceof GrammyError && error.error_code === 400) {
          // Handle closed topic - reopen and retry
          if (error.description.includes('TOPIC_CLOSED') && threadId) {
            logger.info('Topic was closed, attempting to reopen...', { threadId });
            const reopened = await this.reopenForumTopic(threadId);
            if (reopened) {
              // Send reopened notification
              await this.bot.api.sendMessage(item.chatId, '📂 Topic reopened', {
                message_thread_id: threadId,
                disable_notification: true
              });
              // Retry the original message
              if (item.buttons && item.buttons.length > 0) {
                const keyboard = new InlineKeyboard();
                item.buttons.forEach((btn, idx) => {
                  keyboard.text(btn.text, btn.callbackData);
                  if ((idx + 1) % 2 === 0) keyboard.row();
                });
                await this.bot.api.sendMessage(item.chatId, item.text, {
                  parse_mode: parseMode,
                  disable_notification: item.options?.disableNotification,
                  message_thread_id: threadId,
                  reply_markup: keyboard
                });
              } else {
                await this.bot.api.sendMessage(item.chatId, item.text, {
                  parse_mode: parseMode,
                  disable_notification: item.options?.disableNotification,
                  message_thread_id: threadId
                });
              }
              return; // Success after reopen
            }
            // If reopen failed, fall through to throw
            logger.error('Failed to reopen topic, message will be lost', { threadId });
            throw error;
          }

          // Handle markdown parsing failure - retry as plain text
          if (error.description.includes("can't parse entities")) {
            logger.warn('Markdown parsing failed, retrying as plain text', {
              error: error.description
            });

            // Strip markdown formatting for plain text fallback
            const plainText = item.text
              .replace(/\*\*/g, '')  // Remove bold **
              .replace(/\*/g, '')    // Remove italic *
              .replace(/`/g, "'")    // Replace backticks with quotes
              .replace(/_/g, '');    // Remove underscores

            if (item.buttons && item.buttons.length > 0) {
              const keyboard = new InlineKeyboard();
              item.buttons.forEach((btn, idx) => {
                keyboard.text(btn.text, btn.callbackData);
                if ((idx + 1) % 2 === 0) keyboard.row();
              });
              await this.bot.api.sendMessage(item.chatId, plainText, {
                disable_notification: item.options?.disableNotification,
                message_thread_id: threadId,
                reply_markup: keyboard
              });
            } else {
              await this.bot.api.sendMessage(item.chatId, plainText, {
                disable_notification: item.options?.disableNotification,
                message_thread_id: threadId
              });
            }
            return; // Success after plain text fallback
          }
        }
        throw error; // Re-throw other errors for retry logic
      }
    });
  }

  /**
   * Start the bot
   */
  async start(): Promise<void> {
    if (this.running) {
      logger.warn('Bot already running');
      return;
    }

    logger.info('Starting Telegram bot...');
    this.running = true;

    // Start long polling
    this.bot.start({
      onStart: (botInfo) => {
        logger.info(`Bot started: @${botInfo.username}`);
      }
    });
  }

  /**
   * Stop the bot
   */
  async stop(): Promise<void> {
    if (!this.running) return;

    logger.info('Stopping Telegram bot...');
    this.running = false;
    await this.bot.stop();
  }

  /**
   * Send a message (queued with rate limiting)
   */
  async sendMessage(text: string, options?: SendOptions, threadId?: number): Promise<void> {
    // Chunk long messages
    const chunks = chunkMessage(text, { maxLength: this.config.chunkSize });

    for (const chunk of chunks) {
      this.messageQueue.add({
        chatId: this.config.chatId,
        text: chunk,
        options,
        threadId: threadId || options?.threadId
      });
    }
  }

  /**
   * Send message with inline buttons
   */
  async sendWithButtons(text: string, buttons: InlineButton[], options?: SendOptions, threadId?: number): Promise<void> {
    this.messageQueue.add({
      chatId: this.config.chatId,
      text,
      options,
      buttons,
      threadId: threadId || options?.threadId
    });
  }

  // Valid forum topic icon colors (Telegram API requirement)
  private static readonly TOPIC_COLORS = [
    0x6FB9F0, // Blue
    0xFFD67E, // Yellow
    0xCB86DB, // Purple
    0x8EEE98, // Green
    0xFF93B2, // Pink
    0xFB6F5F  // Red
  ] as const;

  /**
   * Create a forum topic for a session
   * Returns the thread ID if successful, null if not supported
   */
  async createForumTopic(name: string, colorIndex: number = 0): Promise<number | null> {
    if (!this.config.useThreads) {
      return null;
    }

    try {
      // Use a valid color from the allowed list
      const iconColor = TelegramBot.TOPIC_COLORS[colorIndex % TelegramBot.TOPIC_COLORS.length];

      const result = await this.bot.api.createForumTopic(
        this.config.chatId,
        name,
        { icon_color: iconColor as 7322096 | 16766590 | 13338331 | 9367192 | 16749490 | 16478047 }
      );
      logger.info('Created forum topic', { name, threadId: result.message_thread_id });
      return result.message_thread_id;
    } catch (error) {
      // Forum topics not supported (not a supergroup with topics enabled)
      logger.debug('Forum topics not supported', { error });
      return null;
    }
  }

  /**
   * Close a forum topic
   */
  async closeForumTopic(threadId: number): Promise<boolean> {
    try {
      await this.bot.api.closeForumTopic(this.config.chatId, threadId);
      logger.info('Closed forum topic', { threadId });
      return true;
    } catch (error) {
      logger.debug('Failed to close forum topic', { threadId, error });
      return false;
    }
  }

  /**
   * Reopen a closed forum topic
   */
  async reopenForumTopic(threadId: number): Promise<boolean> {
    try {
      await this.bot.api.reopenForumTopic(this.config.chatId, threadId);
      logger.info('Reopened forum topic', { threadId });
      return true;
    } catch (error) {
      logger.warn('Failed to reopen forum topic', { threadId, error });
      return false;
    }
  }

  /**
   * Register text message handler
   */
  onMessage(handler: (text: string, chatId: number, threadId?: number) => void): void {
    this.messageHandlers.push(handler);

    // Only register once
    if (this.messageHandlers.length === 1) {
      this.bot.on('message:text', (ctx) => {
        const text = ctx.message.text;
        const chatId = ctx.chat.id;
        const threadId = ctx.message.message_thread_id;

        // Skip commands
        if (text.startsWith('/')) return;

        this.messageHandlers.forEach(h => h(text, chatId, threadId));
      });
    }
  }

  /**
   * Register callback query handler
   */
  onCallback(handler: (data: string, chatId: number) => void): void {
    this.callbackHandlers.push(handler);

    // Only register once
    if (this.callbackHandlers.length === 1) {
      this.bot.on('callback_query:data', async (ctx) => {
        const data = ctx.callbackQuery.data;
        const chatId = ctx.chat?.id;

        if (chatId) {
          this.callbackHandlers.forEach(h => h(data, chatId));
        }

        await ctx.answerCallbackQuery();
      });
    }
  }

  /**
   * Get the underlying grammY bot instance
   */
  getBot(): Bot<BotContext> {
    return this.bot;
  }

  /**
   * Check if bot is running
   */
  isRunning(): boolean {
    return this.running;
  }

  /**
   * Get session data for a chat
   */
  getSession(_chatId: number): SessionData | null {
    // Note: This would need proper session storage for production
    return null;
  }

  /**
   * Update message (for editing after button press)
   */
  async editMessage(
    messageId: number,
    text: string,
    options?: SendOptions
  ): Promise<void> {
    try {
      await this.bot.api.editMessageText(
        this.config.chatId,
        messageId,
        text,
        {
          parse_mode: options?.parseMode || 'Markdown'
        }
      );
    } catch (error) {
      logger.error('Failed to edit message', { messageId, error });
    }
  }

  /**
   * Remove inline keyboard from message
   */
  async removeKeyboard(messageId: number): Promise<void> {
    try {
      await this.bot.api.editMessageReplyMarkup(
        this.config.chatId,
        messageId,
        { reply_markup: undefined }
      );
    } catch (error) {
      logger.error('Failed to remove keyboard', { messageId, error });
    }
  }
}

export default TelegramBot;
