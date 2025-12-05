/**
 * Chunker Tests
 */

import { describe, it, expect } from 'vitest';
import { chunkMessage, needsChunking, estimateChunks } from '../../src/utils/chunker.js';

describe('chunkMessage', () => {
  it('should not split short messages', () => {
    const text = 'Hello, world!';
    const chunks = chunkMessage(text);
    expect(chunks).toEqual([text]);
  });

  it('should not add part headers for single chunk', () => {
    const text = 'Short message';
    const chunks = chunkMessage(text);
    expect(chunks[0]).not.toContain('Part');
  });

  it('should split long messages at ~4000 chars', () => {
    const text = 'x'.repeat(8000);
    const chunks = chunkMessage(text);
    expect(chunks.length).toBe(2);
  });

  it('should add part headers for multi-part messages', () => {
    const text = 'x'.repeat(8500);
    const chunks = chunkMessage(text, { addPartHeaders: true });
    expect(chunks[0]).toContain('Part 1/');
    expect(chunks[1]).toContain('Part 2/');
  });

  it('should preserve code blocks when possible', () => {
    // Create a message with a code block that fits in one chunk
    const code = '```javascript\nconsole.log("hello");\n```';
    const text = 'Some text\n' + code + '\nMore text';
    const chunks = chunkMessage(text);
    expect(chunks.length).toBe(1);
    expect(chunks[0]).toContain('```javascript');
    expect(chunks[0]).toContain('```');
  });

  it('should handle empty string', () => {
    const chunks = chunkMessage('');
    expect(chunks).toEqual(['']);
  });

  it('should handle exactly maxLength', () => {
    const text = 'x'.repeat(4000);
    const chunks = chunkMessage(text);
    expect(chunks.length).toBe(1);
  });

  it('should split at natural break points', () => {
    const longParagraph = 'This is a sentence. '.repeat(250); // ~5000 chars
    const chunks = chunkMessage(longParagraph);
    expect(chunks.length).toBe(2);
    // Should split at sentence boundary
    expect(chunks[0].trim()).toMatch(/\.$/);
  });

  it('should handle custom maxLength', () => {
    const text = 'x'.repeat(200);
    const chunks = chunkMessage(text, { maxLength: 100 });
    expect(chunks.length).toBe(2);
  });

  it('should disable part headers when requested', () => {
    const text = 'x'.repeat(8000);
    const chunks = chunkMessage(text, { addPartHeaders: false });
    expect(chunks[0]).not.toContain('Part');
  });
});

describe('needsChunking', () => {
  it('should return false for short messages', () => {
    expect(needsChunking('Hello')).toBe(false);
  });

  it('should return true for long messages', () => {
    expect(needsChunking('x'.repeat(5000))).toBe(true);
  });

  it('should use custom maxLength', () => {
    expect(needsChunking('Hello World', 5)).toBe(true);
    expect(needsChunking('Hello World', 100)).toBe(false);
  });
});

describe('estimateChunks', () => {
  it('should estimate correctly for short messages', () => {
    expect(estimateChunks('Hello')).toBe(1);
  });

  it('should estimate correctly for long messages', () => {
    expect(estimateChunks('x'.repeat(8000))).toBe(2);
    expect(estimateChunks('x'.repeat(12000))).toBe(3);
  });

  it('should use custom maxLength', () => {
    expect(estimateChunks('x'.repeat(100), 50)).toBe(2);
  });
});
