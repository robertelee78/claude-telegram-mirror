/**
 * Formatting Tests
 */

import { describe, it, expect } from 'vitest';
import {
  formatAgentResponse,
  formatToolExecution,
  formatApprovalRequest,
  formatError,
  formatSessionStart,
  formatSessionEnd,
  formatHelp,
  formatStatus
} from '../../src/bot/formatting.js';

describe('formatAgentResponse', () => {
  it('should add robot emoji prefix', () => {
    const result = formatAgentResponse('Hello world');
    expect(result).toContain('🤖');
  });

  it('should include the response content', () => {
    const content = 'This is my response';
    const result = formatAgentResponse(content);
    expect(result).toContain(content);
  });

  it('should strip ANSI codes', () => {
    const withAnsi = '\x1b[31mRed text\x1b[0m';
    const result = formatAgentResponse(withAnsi);
    expect(result).not.toContain('\x1b');
    expect(result).toContain('Red text');
  });

  it('should handle empty content', () => {
    const result = formatAgentResponse('');
    expect(result).toContain('🤖');
  });
});

describe('formatToolExecution', () => {
  it('should include tool name', () => {
    const result = formatToolExecution('Read', { file_path: '/test' }, 'content', false);
    expect(result).toContain('Read');
  });

  it('should abbreviate very long output in non-verbose mode', () => {
    const longOutput = 'x'.repeat(2000);
    const result = formatToolExecution('Bash', { command: 'ls' }, longOutput, false);
    // Should truncate but still have some content
    expect(result.length).toBeLessThan(2500);
  });

  it('should show output in verbose mode', () => {
    const output = 'Full output here';
    const result = formatToolExecution('Bash', { command: 'ls' }, output, true);
    expect(result).toContain(output);
  });

  it('should include tool emoji', () => {
    const result = formatToolExecution('Write', { file_path: '/test' }, 'success', true);
    expect(result).toContain('🔧');
  });
});

describe('formatApprovalRequest', () => {
  it('should include warning emoji', () => {
    const result = formatApprovalRequest('Permission needed');
    expect(result).toContain('⚠️');
  });

  it('should include the prompt', () => {
    const prompt = 'Write file /test.ts';
    const result = formatApprovalRequest(prompt);
    expect(result).toContain(prompt);
  });
});

describe('formatError', () => {
  it('should include error emoji', () => {
    const result = formatError('Something went wrong');
    expect(result).toContain('❌');
  });

  it('should include error message', () => {
    const message = 'Connection failed';
    const result = formatError(message);
    expect(result).toContain(message);
  });
});

describe('formatSessionStart', () => {
  it('should include session ID', () => {
    const result = formatSessionStart('session-123', '/project');
    expect(result).toContain('session-123');
  });

  it('should include project directory when provided', () => {
    const result = formatSessionStart('session-123', '/my/project');
    expect(result).toContain('/my/project');
  });

  it('should handle missing project directory', () => {
    const result = formatSessionStart('session-123');
    expect(result).toContain('session-123');
  });

  it('should include hostname when provided', () => {
    const result = formatSessionStart('session-123', '/project', 'my-laptop');
    expect(result).toContain('my-laptop');
    expect(result).toContain('Host:');
  });
});

describe('formatSessionEnd', () => {
  it('should include session ID', () => {
    const result = formatSessionEnd('session-123', 60000);
    expect(result).toContain('session-123');
  });

  it('should format duration', () => {
    const result = formatSessionEnd('session-123', 120000); // 2 minutes
    expect(result).toMatch(/2.*m/i);
  });
});

describe('formatHelp', () => {
  it('should include bot command list', () => {
    const result = formatHelp();
    expect(result).toContain('/status');
    expect(result).toContain('/help');
    expect(result).toContain('/sessions');
  });

  it('should include Claude Code commands with cc prefix', () => {
    const result = formatHelp();
    expect(result).toContain('cc clear');
    expect(result).toContain('cc compact');
    expect(result).toContain('cc cost');
  });

  it('should include title', () => {
    const result = formatHelp();
    expect(result).toContain('Commands');
  });
});

describe('formatStatus', () => {
  it('should show active session when attached', () => {
    const result = formatStatus(true, 'session-123', false);
    expect(result).toContain('session-123');
  });

  it('should show no active session when not attached', () => {
    const result = formatStatus(false, undefined, false);
    expect(result).toContain('No active session');
  });

  it('should show muted status', () => {
    const result = formatStatus(true, 'session-123', true);
    expect(result).toContain('Muted');
  });

  it('should include Status header', () => {
    const result = formatStatus(true, 'session-123', false);
    expect(result).toContain('Status');
  });
});
