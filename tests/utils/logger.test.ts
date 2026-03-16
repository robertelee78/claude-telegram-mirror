import { describe, it, expect } from 'vitest';
import { scrubBotToken } from '../../src/utils/logger.js';

describe('scrubBotToken', () => {
  it('scrubs bot token from Telegram API URL', () => {
    const input = 'Error: 400 https://api.telegram.org/bot12345:ABCdefGHIjklMNOpqrsTUV/sendMessage';
    expect(scrubBotToken(input)).toBe('Error: 400 https://api.telegram.org/bot<REDACTED>sendMessage');
  });

  it('scrubs multiple tokens in same string', () => {
    const input = 'bot111:abc/ and bot222:xyz/';
    expect(scrubBotToken(input)).toBe('bot<REDACTED> and bot<REDACTED>');
  });

  it('does not modify strings without tokens', () => {
    const input = 'Normal error message without tokens';
    expect(scrubBotToken(input)).toBe(input);
  });

  it('handles empty string', () => {
    expect(scrubBotToken('')).toBe('');
  });

  it('scrubs token with various valid characters', () => {
    const input = 'bot999999999:ABCdef_GHI-jkl123/getUpdates';
    expect(scrubBotToken(input)).toBe('bot<REDACTED>getUpdates');
  });
});
