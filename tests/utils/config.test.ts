/**
 * Config Tests
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { loadConfig, validateConfig, isMirrorEnabled, resetConfig } from '../../src/utils/config.js';

describe('loadConfig', () => {
  const originalEnv = process.env;

  beforeEach(() => {
    // Reset config singleton
    resetConfig();
    // Save original env
    process.env = { ...originalEnv };
  });

  afterEach(() => {
    // Restore original env
    process.env = originalEnv;
    resetConfig();
  });

  it('should load required config from environment', () => {
    process.env.TELEGRAM_BOT_TOKEN = 'test-token-123';
    process.env.TELEGRAM_CHAT_ID = '12345';

    const config = loadConfig();

    expect(config.botToken).toBe('test-token-123');
    expect(config.chatId).toBe(12345);
  });

  it('should throw error if bot token missing and requireAuth is true', () => {
    delete process.env.TELEGRAM_BOT_TOKEN;
    process.env.TELEGRAM_CHAT_ID = '12345';

    expect(() => loadConfig(true)).toThrow('TELEGRAM_BOT_TOKEN');
  });

  it('should throw error if chat ID missing and requireAuth is true', () => {
    process.env.TELEGRAM_BOT_TOKEN = 'test-token';
    delete process.env.TELEGRAM_CHAT_ID;

    expect(() => loadConfig(true)).toThrow('TELEGRAM_CHAT_ID');
  });

  it('should not throw if requireAuth is false', () => {
    delete process.env.TELEGRAM_BOT_TOKEN;
    delete process.env.TELEGRAM_CHAT_ID;

    expect(() => loadConfig(false)).not.toThrow();
  });

  it('should use default values for optional config', () => {
    process.env.TELEGRAM_BOT_TOKEN = 'test-token';
    process.env.TELEGRAM_CHAT_ID = '12345';
    // Explicitly unset optional vars to test defaults
    delete process.env.TELEGRAM_MIRROR;
    delete process.env.TELEGRAM_MIRROR_VERBOSE;
    delete process.env.TELEGRAM_MIRROR_APPROVALS;

    const config = loadConfig();

    expect(config.enabled).toBe(false);
    expect(config.verbose).toBe(true);
    expect(config.approvals).toBe(true);
    // Socket path defaults to ~/.config/claude-telegram-mirror/bridge.sock
    expect(config.socketPath).toContain('bridge.sock');
    expect(config.chunkSize).toBe(4000);
    expect(config.rateLimit).toBe(1);
    expect(config.sessionTimeout).toBe(30);
  });

  it('should parse boolean env vars correctly', () => {
    process.env.TELEGRAM_BOT_TOKEN = 'test-token';
    process.env.TELEGRAM_CHAT_ID = '12345';
    process.env.TELEGRAM_MIRROR = 'true';
    process.env.TELEGRAM_MIRROR_VERBOSE = 'false';

    const config = loadConfig();

    expect(config.enabled).toBe(true);
    expect(config.verbose).toBe(false);
  });

  it('should parse numeric env vars correctly', () => {
    process.env.TELEGRAM_BOT_TOKEN = 'test-token';
    process.env.TELEGRAM_CHAT_ID = '12345';
    process.env.TELEGRAM_CHUNK_SIZE = '3000';
    process.env.TELEGRAM_RATE_LIMIT = '2';

    const config = loadConfig();

    expect(config.chunkSize).toBe(3000);
    expect(config.rateLimit).toBe(2);
  });

  it('should default useThreads to true', () => {
    process.env.TELEGRAM_BOT_TOKEN = 'test-token';
    process.env.TELEGRAM_CHAT_ID = '12345';

    const config = loadConfig();

    expect(config.useThreads).toBe(true);
  });

  it('should parse TELEGRAM_USE_THREADS env var', () => {
    process.env.TELEGRAM_BOT_TOKEN = 'test-token';
    process.env.TELEGRAM_CHAT_ID = '12345';
    process.env.TELEGRAM_USE_THREADS = 'false';

    const config = loadConfig();

    expect(config.useThreads).toBe(false);
  });

  it('should handle invalid numeric values with defaults', () => {
    process.env.TELEGRAM_BOT_TOKEN = 'test-token';
    process.env.TELEGRAM_CHAT_ID = '12345';
    process.env.TELEGRAM_CHUNK_SIZE = 'invalid';

    const config = loadConfig();

    expect(config.chunkSize).toBe(4000);
  });
});

describe('validateConfig', () => {
  it('should return valid for complete config', () => {
    const config = {
      botToken: 'test-token',
      chatId: 12345,
      enabled: true,
      verbose: true,
      approvals: true,
      socketPath: '/tmp/test.sock',
      useThreads: true,
      forumEnabled: false,
      chunkSize: 4000,
      rateLimit: 1,
      sessionTimeout: 30,
      configPath: '/tmp/config.json'
    };

    const result = validateConfig(config);

    expect(result.valid).toBe(true);
    expect(result.errors).toHaveLength(0);
  });

  it('should return error for missing bot token', () => {
    const config = {
      botToken: '',
      chatId: 12345,
      enabled: true,
      verbose: true,
      approvals: true,
      socketPath: '/tmp/test.sock',
      useThreads: true,
      forumEnabled: false,
      chunkSize: 4000,
      rateLimit: 1,
      sessionTimeout: 30,
      configPath: '/tmp/config.json'
    };

    const result = validateConfig(config);

    expect(result.valid).toBe(false);
    expect(result.errors).toContain('TELEGRAM_BOT_TOKEN is not set');
  });

  it('should return error for missing chat ID', () => {
    const config = {
      botToken: 'test-token',
      chatId: 0,
      enabled: true,
      verbose: true,
      approvals: true,
      socketPath: '/tmp/test.sock',
      useThreads: true,
      forumEnabled: false,
      chunkSize: 4000,
      rateLimit: 1,
      sessionTimeout: 30,
      configPath: '/tmp/config.json'
    };

    const result = validateConfig(config);

    expect(result.valid).toBe(false);
    expect(result.errors).toContain('TELEGRAM_CHAT_ID is not set');
  });

  it('should return warning for disabled mirroring', () => {
    const config = {
      botToken: 'test-token',
      chatId: 12345,
      enabled: false,
      verbose: true,
      approvals: true,
      socketPath: '/tmp/test.sock',
      useThreads: true,
      forumEnabled: false,
      chunkSize: 4000,
      rateLimit: 1,
      sessionTimeout: 30,
      configPath: '/tmp/config.json'
    };

    const result = validateConfig(config);

    expect(result.warnings).toContain('TELEGRAM_MIRROR is not enabled');
  });

  it('should return warning for unusual chunk size', () => {
    const config = {
      botToken: 'test-token',
      chatId: 12345,
      enabled: true,
      verbose: true,
      approvals: true,
      socketPath: '/tmp/test.sock',
      useThreads: true,
      forumEnabled: false,
      chunkSize: 500, // Too small
      rateLimit: 1,
      sessionTimeout: 30,
      configPath: '/tmp/config.json'
    };

    const result = validateConfig(config);

    expect(result.warnings.some(w => w.includes('Chunk size'))).toBe(true);
  });
});

describe('isMirrorEnabled', () => {
  const originalEnv = process.env;

  beforeEach(() => {
    process.env = { ...originalEnv };
  });

  afterEach(() => {
    process.env = originalEnv;
  });

  it('should return false when TELEGRAM_MIRROR is not set', () => {
    delete process.env.TELEGRAM_MIRROR;
    expect(isMirrorEnabled()).toBe(false);
  });

  it('should return false when TELEGRAM_MIRROR is false', () => {
    process.env.TELEGRAM_MIRROR = 'false';
    expect(isMirrorEnabled()).toBe(false);
  });

  it('should return false when token is missing', () => {
    process.env.TELEGRAM_MIRROR = 'true';
    delete process.env.TELEGRAM_BOT_TOKEN;
    process.env.TELEGRAM_CHAT_ID = '12345';
    expect(isMirrorEnabled()).toBe(false);
  });

  it('should return false when chat ID is missing', () => {
    process.env.TELEGRAM_MIRROR = 'true';
    process.env.TELEGRAM_BOT_TOKEN = 'test-token';
    delete process.env.TELEGRAM_CHAT_ID;
    expect(isMirrorEnabled()).toBe(false);
  });

  it('should return true when all conditions met', () => {
    process.env.TELEGRAM_MIRROR = 'true';
    process.env.TELEGRAM_BOT_TOKEN = 'test-token';
    process.env.TELEGRAM_CHAT_ID = '12345';
    expect(isMirrorEnabled()).toBe(true);
  });
});
