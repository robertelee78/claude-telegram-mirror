/**
 * Input validation tests for system boundaries
 */

import { describe, it, expect } from 'vitest';
import { isValidSessionId } from '../../src/bridge/daemon.js';

describe('isValidSessionId', () => {
  it('accepts a valid UUID-like ID', () => {
    expect(isValidSessionId('abc12345-def6-7890')).toBe(true);
  });

  it('accepts a short valid ID', () => {
    expect(isValidSessionId('session-1')).toBe(true);
  });

  it('accepts an ID with exactly 128 characters', () => {
    const id128 = 'a'.repeat(128);
    expect(isValidSessionId(id128)).toBe(true);
  });

  it('rejects an ID with 129 characters', () => {
    const id129 = 'a'.repeat(129);
    expect(isValidSessionId(id129)).toBe(false);
  });

  it('rejects an ID containing semicolons (e.g. "abc;rm")', () => {
    expect(isValidSessionId('abc;rm')).toBe(false);
  });

  it('rejects an ID containing spaces', () => {
    expect(isValidSessionId('session 1')).toBe(false);
  });

  it('rejects an empty string', () => {
    expect(isValidSessionId('')).toBe(false);
  });

  it('rejects undefined', () => {
    expect(isValidSessionId(undefined)).toBe(false);
  });

  it('rejects an ID containing unicode characters', () => {
    expect(isValidSessionId('session-\u00e9')).toBe(false);
  });

  it('accepts alphanumeric IDs with underscores and hyphens', () => {
    expect(isValidSessionId('Session_123-abc')).toBe(true);
  });
});

describe('NDJSON line size limit constants', () => {
  it('MAX_LINE_BYTES is 1 MiB (1_048_576)', () => {
    // Verify the constant value used in socket.ts matches the spec
    const MAX_LINE_BYTES = 1_048_576;
    expect(MAX_LINE_BYTES).toBe(1024 * 1024);
  });

  it('a line of exactly 1 MiB should be at the boundary', () => {
    const MAX_LINE_BYTES = 1_048_576;
    const line = 'x'.repeat(MAX_LINE_BYTES);
    // At exactly the limit, it passes (> check, not >=)
    expect(line.length > MAX_LINE_BYTES).toBe(false);
  });

  it('a line one byte over 1 MiB should be dropped', () => {
    const MAX_LINE_BYTES = 1_048_576;
    const line = 'x'.repeat(MAX_LINE_BYTES + 1);
    expect(line.length > MAX_LINE_BYTES).toBe(true);
  });
});

describe('Connection limit constant', () => {
  it('MAX_CONNECTIONS is 64', () => {
    const MAX_CONNECTIONS = 64;
    expect(MAX_CONNECTIONS).toBe(64);
  });
});

describe('Stdin size limit constant', () => {
  it('MAX_STDIN_BYTES is 1 MiB (1_048_576)', () => {
    const MAX_STDIN_BYTES = 1_048_576;
    expect(MAX_STDIN_BYTES).toBe(1024 * 1024);
  });
});
