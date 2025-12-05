/**
 * Bridge Module
 * Exports all bridge-related functionality
 */

export { BridgeDaemon } from './daemon.js';
export { SocketServer, SocketClient, DEFAULT_SOCKET_PATH } from './socket.js';
export { SessionManager } from './session.js';
export { InputInjector, createInjector } from './injector.js';
export type {
  BridgeMessage,
  Session,
  PendingApproval
} from './types.js';
