/**
 * Unix Domain Socket Server/Client
 * IPC layer for Claude Code ↔ Telegram bridge
 */

import { createServer, connect, Server, Socket } from 'net';
import { unlinkSync, existsSync } from 'fs';
import { EventEmitter } from 'events';
import logger from '../utils/logger.js';
import type { BridgeMessage } from './types.js';

const DEFAULT_SOCKET_PATH = '/tmp/claude-telegram-bridge.sock';

/**
 * Unix Socket Server
 * Accepts connections from hook scripts and routes messages
 */
export class SocketServer extends EventEmitter {
  private server: Server | null = null;
  private clients: Map<string, Socket> = new Map();
  private socketPath: string;
  private buffer: Map<string, string> = new Map();

  constructor(socketPath: string = DEFAULT_SOCKET_PATH) {
    super();
    this.socketPath = socketPath;
  }

  /**
   * Start listening for connections
   */
  listen(): Promise<void> {
    return new Promise((resolve, reject) => {
      // Clean up stale socket file
      if (existsSync(this.socketPath)) {
        try {
          unlinkSync(this.socketPath);
          logger.debug('Removed stale socket file');
        } catch (error) {
          logger.error('Failed to remove stale socket', { error });
        }
      }

      this.server = createServer((socket) => {
        this.handleConnection(socket);
      });

      this.server.on('error', (error) => {
        logger.error('Socket server error', { error });
        this.emit('error', error);
        reject(error);
      });

      this.server.listen(this.socketPath, () => {
        logger.info(`Socket server listening on ${this.socketPath}`);
        resolve();
      });
    });
  }

  /**
   * Handle new client connection
   */
  private handleConnection(socket: Socket): void {
    const clientId = `client-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    this.clients.set(clientId, socket);
    this.buffer.set(clientId, '');

    logger.debug('Client connected', { clientId });
    this.emit('connect', clientId);

    socket.on('data', (data) => {
      this.handleData(clientId, data);
    });

    socket.on('close', () => {
      this.clients.delete(clientId);
      this.buffer.delete(clientId);
      logger.debug('Client disconnected', { clientId });
      this.emit('disconnect', clientId);
    });

    socket.on('error', (error) => {
      logger.error('Client socket error', { clientId, error });
      this.clients.delete(clientId);
      this.buffer.delete(clientId);
    });
  }

  /**
   * Handle incoming data (NDJSON protocol)
   */
  private handleData(clientId: string, data: Buffer): void {
    let buffer = this.buffer.get(clientId) || '';
    buffer += data.toString();

    // Process complete lines (NDJSON)
    const lines = buffer.split('\n');
    this.buffer.set(clientId, lines.pop() || '');

    for (const line of lines) {
      if (!line.trim()) continue;

      try {
        const message = JSON.parse(line) as BridgeMessage;
        this.emit('message', message, clientId);
      } catch (error) {
        logger.error('Failed to parse message', { line, error });
      }
    }
  }

  /**
   * Send message to specific client
   */
  send(clientId: string, message: BridgeMessage): boolean {
    const socket = this.clients.get(clientId);
    if (!socket || socket.destroyed) {
      logger.warn('Client not found or disconnected', { clientId });
      return false;
    }

    try {
      socket.write(JSON.stringify(message) + '\n');
      return true;
    } catch (error) {
      logger.error('Failed to send message', { clientId, error });
      return false;
    }
  }

  /**
   * Broadcast message to all clients
   */
  broadcast(message: BridgeMessage): void {
    const data = JSON.stringify(message) + '\n';
    for (const [clientId, socket] of this.clients) {
      if (!socket.destroyed) {
        try {
          socket.write(data);
        } catch (error) {
          logger.error('Failed to broadcast to client', { clientId, error });
        }
      }
    }
  }

  /**
   * Get connected client count
   */
  getClientCount(): number {
    return this.clients.size;
  }

  /**
   * Close the server
   */
  close(): Promise<void> {
    return new Promise((resolve) => {
      // Close all client connections
      for (const socket of this.clients.values()) {
        socket.destroy();
      }
      this.clients.clear();

      // Close server
      if (this.server) {
        this.server.close(() => {
          // Clean up socket file
          if (existsSync(this.socketPath)) {
            try {
              unlinkSync(this.socketPath);
            } catch (error) {
              logger.error('Failed to clean up socket file', { error });
            }
          }
          logger.info('Socket server closed');
          resolve();
        });
      } else {
        resolve();
      }
    });
  }
}

/**
 * Unix Socket Client
 * Used by hook scripts to send messages to bridge
 */
export class SocketClient extends EventEmitter {
  private socket: Socket | null = null;
  private socketPath: string;
  private buffer = '';
  private reconnectTimer: NodeJS.Timeout | null = null;
  private connected = false;

  constructor(socketPath: string = DEFAULT_SOCKET_PATH) {
    super();
    this.socketPath = socketPath;
  }

  /**
   * Connect to the server
   */
  connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.socket = connect(this.socketPath);

      this.socket.on('connect', () => {
        this.connected = true;
        logger.debug('Connected to bridge');
        this.emit('connect');
        resolve();
      });

      this.socket.on('data', (data) => {
        this.handleData(data);
      });

      this.socket.on('close', () => {
        this.connected = false;
        logger.debug('Disconnected from bridge');
        this.emit('disconnect');
      });

      this.socket.on('error', (error: NodeJS.ErrnoException) => {
        this.connected = false;

        if (error.code === 'ENOENT') {
          logger.warn('Bridge not running (socket not found)');
        } else if (error.code === 'ECONNREFUSED') {
          logger.warn('Bridge refused connection');
        } else {
          logger.error('Socket error', { error });
        }

        this.emit('error', error);
        reject(error);
      });
    });
  }

  /**
   * Handle incoming data
   */
  private handleData(data: Buffer): void {
    this.buffer += data.toString();

    const lines = this.buffer.split('\n');
    this.buffer = lines.pop() || '';

    for (const line of lines) {
      if (!line.trim()) continue;

      try {
        const message = JSON.parse(line) as BridgeMessage;
        this.emit('message', message);
      } catch (error) {
        logger.error('Failed to parse server message', { error });
      }
    }
  }

  /**
   * Send message to server
   */
  send(message: BridgeMessage): boolean {
    if (!this.socket || this.socket.destroyed || !this.connected) {
      logger.warn('Not connected to bridge');
      return false;
    }

    try {
      this.socket.write(JSON.stringify(message) + '\n');
      return true;
    } catch (error) {
      logger.error('Failed to send message', { error });
      return false;
    }
  }

  /**
   * Send message and wait for response
   */
  sendAndWait(message: BridgeMessage, timeout: number = 30000): Promise<BridgeMessage> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        reject(new Error('Response timeout'));
      }, timeout);

      const handler = (response: BridgeMessage) => {
        if (response.sessionId === message.sessionId) {
          clearTimeout(timer);
          this.off('message', handler);
          resolve(response);
        }
      };

      this.on('message', handler);

      if (!this.send(message)) {
        clearTimeout(timer);
        this.off('message', handler);
        reject(new Error('Failed to send message'));
      }
    });
  }

  /**
   * Check if connected
   */
  isConnected(): boolean {
    return this.connected;
  }

  /**
   * Disconnect from server
   */
  disconnect(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    if (this.socket) {
      this.socket.destroy();
      this.socket = null;
    }

    this.connected = false;
  }
}

export { DEFAULT_SOCKET_PATH };
