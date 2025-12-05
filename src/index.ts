/**
 * Claude Telegram Mirror
 * Bidirectional Telegram integration for Claude Code CLI
 */

// Bot exports
export { TelegramBot } from './bot/telegram.js';
export { registerCommands, registerApprovalHandlers } from './bot/commands.js';
export { formatAgentResponse, formatToolExecution } from './bot/formatting.js';

// Utils exports
export { chunkMessage, needsChunking, estimateChunks } from './utils/chunker.js';

// Bridge exports
export { BridgeDaemon } from './bridge/daemon.js';
export { SessionManager } from './bridge/session.js';
export { SocketServer, SocketClient, DEFAULT_SOCKET_PATH } from './bridge/socket.js';
export { InputInjector, createInjector } from './bridge/injector.js';

// Hook exports
export { HookHandler, installHooks, uninstallHooks, checkHookStatus } from './hooks/index.js';

// Config exports
export { loadConfig, validateConfig } from './utils/config.js';

// Type exports
export type { TelegramMirrorConfig } from './utils/config.js';
export type { BridgeMessage, Session, PendingApproval } from './bridge/types.js';
export type { SendOptions, InlineButton } from './bot/types.js';
export type { AnyHookEvent, HookEventType } from './hooks/types.js';
