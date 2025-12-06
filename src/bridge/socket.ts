/**
 * Unix Domain Socket Server/Client
 * IPC layer for Claude Code ↔ Telegram bridge
 */

import { createServer, connect, Server, Socket } from 'net';
import { unlinkSync, existsSync, writeFileSync, readFileSync, mkdirSync, chmodSync } from 'fs';
import { EventEmitter } from 'events';
import { join } from 'path';
import { homedir } from 'os';
import logger from '../utils/logger.js';
import type { BridgeMessage } from './types.js';

// Use secure user-specific directory instead of world-writable /tmp
const SOCKET_DIR = join(homedir(), '.config', 'claude-telegram-mirror');
const DEFAULT_SOCKET_PATH = join(SOCKET_DIR, 'bridge.sock');
const DEFAULT_PID_PATH = join(SOCKET_DIR, 'bridge.pid');

/**
 * Check if a socket is stale (file exists but no daemon listening)
 * Returns: 'stale' | 'active' | 'none'
 */
async function checkSocketStatus(socketPath: string): Promise<'stale' | 'active' | 'none'> {
  if (!existsSync(socketPath)) {
    return 'none';
  }

  return new Promise((resolve) => {
    const testSocket = connect(socketPath);
    const timeout = setTimeout(() => {
      testSocket.destroy();
      resolve('stale'); // Timeout = no response = stale
    }, 1000);

    testSocket.on('connect', () => {
      clearTimeout(timeout);
      testSocket.destroy();
      resolve('active'); // Connection succeeded = daemon alive
    });

    testSocket.on('error', (err: NodeJS.ErrnoException) => {
      clearTimeout(timeout);
      testSocket.destroy();
      if (err.code === 'ECONNREFUSED') {
        resolve('stale'); // Connection refused = daemon dead
      } else {
        resolve('stale'); // Other error = assume stale
      }
    });
  });
}

/**
 * Check if a PID is still running
 */
function isPidRunning(pid: number): boolean {
  try {
    process.kill(pid, 0); // Signal 0 = check if process exists
    return true;
  } catch {
    return false;
  }
}

/**
 * Acquire PID lock, returns true if lock acquired
 */
function acquirePidLock(pidPath: string): boolean {
  if (existsSync(pidPath)) {
    try {
      const existingPid = parseInt(readFileSync(pidPath, 'utf-8').trim(), 10);
      if (!isNaN(existingPid) && isPidRunning(existingPid)) {
        logger.warn('Another daemon instance is running', { pid: existingPid });
        return false;
      }
      // Stale PID file - process not running
      logger.info('Removing stale PID file', { stalePid: existingPid });
    } catch {
      // Can't read PID file - remove it
    }
  }

  // Write our PID
  writeFileSync(pidPath, process.pid.toString());
  logger.debug('PID lock acquired', { pid: process.pid });
  return true;
}

/**
 * Release PID lock
 */
function releasePidLock(pidPath: string): void {
  if (existsSync(pidPath)) {
    try {
      const storedPid = parseInt(readFileSync(pidPath, 'utf-8').trim(), 10);
      if (storedPid === process.pid) {
        unlinkSync(pidPath);
        logger.debug('PID lock released');
      }
    } catch {
      // Ignore errors during cleanup
    }
  }
}

/**
 * Unix Socket Server
 * Accepts connections from hook scripts and routes messages
 */
export class SocketServer extends EventEmitter {
  private server: Server | null = null;
  private clients: Map<string, Socket> = new Map();
  private socketPath: string;
  private pidPath: string;
  private buffer: Map<string, string> = new Map();

  constructor(socketPath: string = DEFAULT_SOCKET_PATH, pidPath: string = DEFAULT_PID_PATH) {
    super();
    this.socketPath = socketPath;
    this.pidPath = pidPath;
  }

  /**
   * Start listening for connections
   * Includes stale socket detection and PID file locking
   */
  async listen(): Promise<void> {
    // Step 0: Ensure socket directory exists with secure permissions (0o700)
    const socketDir = join(this.socketPath, '..');
    if (!existsSync(socketDir)) {
      mkdirSync(socketDir, { recursive: true, mode: 0o700 });
      logger.debug('Created socket directory', { path: socketDir, mode: '0700' });
    } else {
      // Ensure existing directory has correct permissions
      try {
        chmodSync(socketDir, 0o700);
      } catch (error) {
        logger.warn('Could not set socket directory permissions', { error });
      }
    }

    // Step 1: Acquire PID lock (prevents multiple daemon instances)
    if (!acquirePidLock(this.pidPath)) {
      throw new Error('Another daemon instance is already running. Kill it first or check the PID file.');
    }

    // Register cleanup on process exit
    const cleanup = () => {
      releasePidLock(this.pidPath);
    };
    process.on('exit', cleanup);
    process.on('SIGINT', cleanup);
    process.on('SIGTERM', cleanup);

    // Step 2: Check for stale socket
    const socketStatus = await checkSocketStatus(this.socketPath);

    if (socketStatus === 'active') {
      releasePidLock(this.pidPath);
      throw new Error(`Socket ${this.socketPath} is already in use by another process`);
    }

    if (socketStatus === 'stale') {
      try {
        unlinkSync(this.socketPath);
        logger.info('Removed stale socket file', { path: this.socketPath });
      } catch (error) {
        logger.error('Failed to remove stale socket', { error });
      }
    }

    // Step 3: Start the server
    return new Promise((resolve, reject) => {
      this.server = createServer((socket) => {
        this.handleConnection(socket);
      });

      this.server.on('error', (error) => {
        logger.error('Socket server error', { error });
        releasePidLock(this.pidPath);
        this.emit('error', error);
        reject(error);
      });

      this.server.listen(this.socketPath, () => {
        // Set socket file permissions to owner-only (0o600)
        try {
          chmodSync(this.socketPath, 0o600);
          logger.debug('Set socket permissions', { path: this.socketPath, mode: '0600' });
        } catch (error) {
          logger.warn('Could not set socket file permissions', { error });
        }
        logger.info(`Socket server listening on ${this.socketPath}`, { pid: process.pid });
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
   * Close the server and release all locks
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

          // Release PID lock
          releasePidLock(this.pidPath);

          logger.info('Socket server closed');
          resolve();
        });
      } else {
        // Still release PID lock even if server wasn't created
        releasePidLock(this.pidPath);
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

export { SOCKET_DIR, DEFAULT_SOCKET_PATH, DEFAULT_PID_PATH, checkSocketStatus, isPidRunning };
