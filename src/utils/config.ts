/**
 * Configuration Management
 * Loads configuration from environment variables and optional config file
 */

import { existsSync, readFileSync } from 'fs';
import { join } from 'path';
import { homedir } from 'os';
import logger from './logger.js';

export interface TelegramMirrorConfig {
  // Required
  botToken: string;
  chatId: number;

  // Optional with defaults
  enabled: boolean;
  verbose: boolean;
  approvals: boolean;
  socketPath: string;

  // Threading (Forum Topics)
  useThreads: boolean;        // Enable per-session threads
  forumEnabled: boolean;      // Is the chat a forum/supergroup with topics

  // Advanced
  chunkSize: number;
  rateLimit: number;
  sessionTimeout: number;
  staleSessionTimeoutHours: number;  // BUG-003: Hours before cleaning up stale sessions

  // Topic auto-cleanup
  autoDeleteTopics: boolean;         // Delete topics after session ends (vs just close)
  topicDeleteDelayMinutes: number;   // Minutes to wait before deleting topic

  // Internal
  configPath: string;
}

interface ConfigFile {
  botToken?: string;
  chatId?: number;
  enabled?: boolean;
  verbose?: boolean;
  approvals?: boolean;
  socketPath?: string;
  useThreads?: boolean;
  chunkSize?: number;
  rateLimit?: number;
  sessionTimeout?: number;
  staleSessionTimeoutHours?: number;
  autoDeleteTopics?: boolean;
  topicDeleteDelayMinutes?: number;
}

const CONFIG_DIR = join(homedir(), '.config', 'claude-telegram-mirror');
const CONFIG_FILE = join(CONFIG_DIR, 'config.json');
const DEFAULT_SOCKET_PATH = join(CONFIG_DIR, 'bridge.sock');

class ConfigurationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ConfigurationError';
  }
}

/**
 * Load configuration from config file if it exists
 */
function loadConfigFile(): ConfigFile {
  if (!existsSync(CONFIG_FILE)) {
    return {};
  }

  try {
    const content = readFileSync(CONFIG_FILE, 'utf-8');
    return JSON.parse(content) as ConfigFile;
  } catch (error) {
    logger.warn(`Failed to parse config file: ${CONFIG_FILE}`, { error });
    return {};
  }
}

/**
 * Parse boolean from environment variable
 */
function parseEnvBool(value: string | undefined, defaultValue: boolean): boolean {
  if (value === undefined || value === '') {
    return defaultValue;
  }
  return value.toLowerCase() === 'true' || value === '1';
}

/**
 * Parse number from environment variable
 */
function parseEnvNumber(value: string | undefined, defaultValue: number): number {
  if (value === undefined || value === '') {
    return defaultValue;
  }
  const num = parseInt(value, 10);
  return isNaN(num) ? defaultValue : num;
}

/**
 * Validate and load configuration
 */
export function loadConfig(requireAuth: boolean = true): TelegramMirrorConfig {
  const fileConfig = loadConfigFile();

  // Get values with priority: env vars > config file > defaults
  const botToken = process.env.TELEGRAM_BOT_TOKEN || fileConfig.botToken || '';
  const chatIdStr = process.env.TELEGRAM_CHAT_ID || fileConfig.chatId?.toString() || '';
  const chatId = parseInt(chatIdStr, 10);

  // Validate required fields only if auth is required
  if (requireAuth) {
    if (!botToken) {
      throw new ConfigurationError(
        'TELEGRAM_BOT_TOKEN is required.\n' +
        'Get one from @BotFather on Telegram and set:\n' +
        '  export TELEGRAM_BOT_TOKEN="your-token-here"'
      );
    }

    if (!chatIdStr || isNaN(chatId)) {
      throw new ConfigurationError(
        'TELEGRAM_CHAT_ID is required.\n' +
        'Get your chat ID by messaging your bot and visiting:\n' +
        `  https://api.telegram.org/bot${botToken}/getUpdates\n` +
        'Then set:\n' +
        '  export TELEGRAM_CHAT_ID="your-chat-id"'
      );
    }
  }

  const config: TelegramMirrorConfig = {
    botToken,
    chatId: isNaN(chatId) ? 0 : chatId,

    enabled: parseEnvBool(
      process.env.TELEGRAM_MIRROR,
      fileConfig.enabled ?? false
    ),

    verbose: parseEnvBool(
      process.env.TELEGRAM_MIRROR_VERBOSE,
      fileConfig.verbose ?? true
    ),

    approvals: parseEnvBool(
      process.env.TELEGRAM_MIRROR_APPROVALS,
      fileConfig.approvals ?? true
    ),

    socketPath:
      process.env.TELEGRAM_BRIDGE_SOCKET ||
      fileConfig.socketPath ||
      DEFAULT_SOCKET_PATH,

    useThreads: parseEnvBool(
      process.env.TELEGRAM_USE_THREADS,
      fileConfig.useThreads ?? true
    ),

    forumEnabled: false, // Detected at runtime

    chunkSize: parseEnvNumber(
      process.env.TELEGRAM_CHUNK_SIZE,
      fileConfig.chunkSize ?? 4000
    ),

    rateLimit: parseEnvNumber(
      process.env.TELEGRAM_RATE_LIMIT,
      fileConfig.rateLimit ?? 1
    ),

    sessionTimeout: parseEnvNumber(
      process.env.TELEGRAM_SESSION_TIMEOUT,
      fileConfig.sessionTimeout ?? 30
    ),

    staleSessionTimeoutHours: parseEnvNumber(
      process.env.TELEGRAM_STALE_SESSION_TIMEOUT_HOURS,
      fileConfig.staleSessionTimeoutHours ?? 72  // BUG-003: Default 72 hours
    ),

    autoDeleteTopics: parseEnvBool(
      process.env.TELEGRAM_AUTO_DELETE_TOPICS,
      fileConfig.autoDeleteTopics ?? true  // Default: auto-delete topics after session ends
    ),

    topicDeleteDelayMinutes: parseEnvNumber(
      process.env.TELEGRAM_TOPIC_DELETE_DELAY_MINUTES,
      fileConfig.topicDeleteDelayMinutes ?? 5  // Default: 5 minutes after session ends
    ),

    configPath: CONFIG_FILE
  };

  return config;
}

/**
 * Check if mirroring is enabled
 */
export function isMirrorEnabled(): boolean {
  return parseEnvBool(process.env.TELEGRAM_MIRROR, false) &&
    !!process.env.TELEGRAM_BOT_TOKEN &&
    !!process.env.TELEGRAM_CHAT_ID;
}

/**
 * Get config directory path
 */
export function getConfigDir(): string {
  return CONFIG_DIR;
}

/**
 * Singleton config instance (lazy loaded)
 */
let _config: TelegramMirrorConfig | null = null;

export const config = new Proxy({} as TelegramMirrorConfig, {
  get(_target, prop: keyof TelegramMirrorConfig) {
    if (!_config) {
      _config = loadConfig(false);
    }
    return _config[prop];
  }
});

/**
 * Reset config (useful for testing)
 */
export function resetConfig(): void {
  _config = null;
}

/**
 * Validate configuration
 */
export function validateConfig(config: TelegramMirrorConfig): {
  valid: boolean;
  errors: string[];
  warnings: string[];
} {
  const errors: string[] = [];
  const warnings: string[] = [];

  // Check required fields
  if (!config.botToken) {
    errors.push('TELEGRAM_BOT_TOKEN is not set');
  }

  if (!config.chatId) {
    errors.push('TELEGRAM_CHAT_ID is not set');
  }

  // Check warnings
  if (!config.enabled) {
    warnings.push('TELEGRAM_MIRROR is not enabled');
  }

  if (config.chunkSize < 1000 || config.chunkSize > 4096) {
    warnings.push(`Chunk size ${config.chunkSize} may cause issues (recommended: 1000-4096)`);
  }

  return {
    valid: errors.length === 0,
    errors,
    warnings
  };
}

export { ConfigurationError };
