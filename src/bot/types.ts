/**
 * Telegram Bot Types
 */

export interface SendOptions {
  parseMode?: 'Markdown' | 'MarkdownV2' | 'HTML';
  disableNotification?: boolean;
  replyToMessageId?: number;
  threadId?: number;
}

export interface InlineButton {
  text: string;
  callbackData: string;
}

export interface BotSession {
  attachedSessionId: string | null;
  muted: boolean;
  lastActivity: Date;
}

export interface MessageQueueItem {
  chatId: number;
  text: string;
  options?: SendOptions;
  buttons?: InlineButton[];
  threadId?: number;
  retries: number;
  createdAt: Date;
}
