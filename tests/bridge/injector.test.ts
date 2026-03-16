import { describe, it, expect } from 'vitest';
import { isValidSlashCommand } from '../../src/bridge/injector.js';

describe('isValidSlashCommand — character whitelist', () => {
  // --- commands that must pass ---
  it('accepts /clear', () => {
    expect(isValidSlashCommand('/clear')).toBe(true);
  });

  it('accepts /compact', () => {
    expect(isValidSlashCommand('/compact')).toBe(true);
  });

  it('accepts /rename My Feature', () => {
    expect(isValidSlashCommand('/rename My Feature')).toBe(true);
  });

  it('accepts commands with hyphens and underscores', () => {
    expect(isValidSlashCommand('/my-command_name')).toBe(true);
  });

  it('accepts a plain alphanumeric command without slash', () => {
    expect(isValidSlashCommand('compact')).toBe(true);
  });

  // --- commands that must be rejected ---
  it('rejects /clear; rm -rf / (semicolon)', () => {
    expect(isValidSlashCommand('/clear; rm -rf /')).toBe(false);
  });

  it('rejects /clear$(whoami) (dollar/parentheses)', () => {
    expect(isValidSlashCommand('/clear$(whoami)')).toBe(false);
  });

  it('rejects commands with backticks', () => {
    expect(isValidSlashCommand('/clear`id`')).toBe(false);
  });

  it('rejects commands with pipe |', () => {
    expect(isValidSlashCommand('/clear | cat /etc/passwd')).toBe(false);
  });

  it('rejects commands with redirect >', () => {
    expect(isValidSlashCommand('/clear > /tmp/out')).toBe(false);
  });

  it('rejects commands with redirect <', () => {
    expect(isValidSlashCommand('/clear < /etc/passwd')).toBe(false);
  });

  it('rejects commands with && operator', () => {
    expect(isValidSlashCommand('/clear && id')).toBe(false);
  });

  it('rejects empty string', () => {
    expect(isValidSlashCommand('')).toBe(false);
  });

  it('rejects null-like falsy input (undefined cast to empty)', () => {
    // isValidSlashCommand expects a string; passing an empty string covers the guard
    expect(isValidSlashCommand('')).toBe(false);
  });

  it('rejects newline character', () => {
    expect(isValidSlashCommand('/clear\n')).toBe(false);
  });

  it('rejects command with single-quote', () => {
    expect(isValidSlashCommand("/clear 'arg'")).toBe(false);
  });

  it('rejects command with double-quote', () => {
    expect(isValidSlashCommand('/clear "arg"')).toBe(false);
  });
});
