/**
 * Message Formatting Utilities
 * Formats Claude Code output for Telegram display
 */

import { chunkMessage, needsChunking } from '../utils/chunker.js';

// ANSI escape code pattern
const ANSI_REGEX = /\x1b\[[0-9;]*[a-zA-Z]/g;

// MarkdownV2 special characters that need escaping
const MARKDOWN_V2_SPECIAL = /([_*\[\]()~`>#+\-=|{}.!\\])/g;

/**
 * Strip ANSI escape codes from text
 */
export function stripAnsi(text: string): string {
  return text.replace(ANSI_REGEX, '');
}

/**
 * Escape special characters for MarkdownV2
 * Note: Don't escape inside code blocks
 */
export function escapeMarkdownV2(text: string): string {
  // Split by code blocks to avoid escaping inside them
  const parts = text.split(/(```[\s\S]*?```|`[^`]+`)/g);

  return parts.map((part, index) => {
    // Odd indices are code blocks, don't escape them
    if (index % 2 === 1) {
      return part;
    }
    // Escape special characters in regular text
    return part.replace(MARKDOWN_V2_SPECIAL, '\\$1');
  }).join('');
}

/**
 * Format agent response for Telegram
 */
export function formatAgentResponse(content: string): string {
  const cleaned = stripAnsi(content);
  return `🤖 *Claude:*\n\n${cleaned}`;
}

/**
 * Format tool execution for Telegram
 */
export function formatToolExecution(
  tool: string,
  input: unknown,
  output: unknown,
  verbose: boolean = true
): string {
  let message = `🔧 *Tool: ${tool}*\n`;

  if (verbose && input) {
    const inputStr = typeof input === 'string'
      ? input
      : JSON.stringify(input, null, 2);

    // Truncate long inputs
    const truncatedInput = inputStr.length > 500
      ? inputStr.slice(0, 500) + '...'
      : inputStr;

    message += `\n📥 Input:\n\`\`\`\n${truncatedInput}\n\`\`\`\n`;
  }

  if (output) {
    const outputStr = typeof output === 'string'
      ? output
      : JSON.stringify(output, null, 2);

    // Truncate long outputs
    const truncatedOutput = outputStr.length > 1000
      ? outputStr.slice(0, 1000) + '\n... (truncated)'
      : outputStr;

    message += `\n📤 Output:\n\`\`\`\n${stripAnsi(truncatedOutput)}\n\`\`\``;
  }

  return message;
}

/**
 * Format approval request for Telegram
 */
export function formatApprovalRequest(prompt: string): string {
  return `⚠️ *Approval Required*\n\n${stripAnsi(prompt)}\n\nPlease respond:`;
}

/**
 * Format error message for Telegram
 */
export function formatError(error: Error | string): string {
  const message = error instanceof Error ? error.message : error;
  return `❌ *Error:*\n\n\`\`\`\n${stripAnsi(message)}\n\`\`\``;
}

/**
 * Format session start notification
 */
export function formatSessionStart(sessionId: string, projectDir?: string, hostname?: string): string {
  let message = `🚀 *Session Started*\n\nSession ID: \`${sessionId}\``;
  if (hostname) {
    message += `\nHost: \`${hostname}\``;
  }
  if (projectDir) {
    message += `\nProject: \`${projectDir}\``;
  }
  return message;
}

/**
 * Format session end notification
 */
export function formatSessionEnd(sessionId: string, duration?: number): string {
  let message = `👋 *Session Ended*\n\nSession ID: \`${sessionId}\``;
  if (duration) {
    const minutes = Math.floor(duration / 60000);
    const seconds = Math.floor((duration % 60000) / 1000);
    message += `\nDuration: ${minutes}m ${seconds}s`;
  }
  return message;
}

/**
 * Format status message
 */
export function formatStatus(
  isActive: boolean,
  sessionId?: string,
  muted?: boolean
): string {
  if (!isActive) {
    return '📊 *Status*\n\nNo active session attached.';
  }

  let message = `📊 *Status*\n\n`;
  message += `Session: \`${sessionId}\`\n`;
  message += `Notifications: ${muted ? '🔇 Muted' : '🔔 Active'}`;

  return message;
}

/**
 * Format help message
 */
export function formatHelp(): string {
  return `📚 *Claude Code Mirror - Commands*

/status - Show current session status
/sessions - List active sessions
/attach <id> - Attach to a session
/detach - Detach from current session
/mute - Mute notifications
/unmute - Resume notifications
/abort - Abort current session
/help - Show this message

*Inline Responses:*
Simply reply with text to send input to the attached session.

*Approval Buttons:*
When Claude requests permission, tap:
✅ Approve - Allow the action
❌ Reject - Deny the action
🛑 Abort - End the session`;
}

/**
 * Format a message and chunk if necessary
 */
export function formatAndChunk(
  content: string,
  maxLength: number = 4000
): string[] {
  const cleaned = stripAnsi(content);

  if (needsChunking(cleaned, maxLength)) {
    return chunkMessage(cleaned, { maxLength, preserveCodeBlocks: true });
  }

  return [cleaned];
}

/**
 * Detect code language from content (best effort)
 */
export function detectLanguage(content: string): string {
  const patterns: Array<[RegExp, string]> = [
    [/^#!\/usr\/bin\/env node|^import .* from ['"]|^const .* = require\(/, 'javascript'],
    [/^#!\/usr\/bin\/env python|^import |^from .* import |^def /, 'python'],
    [/^package |^import ".*"|^func /, 'go'],
    [/^use |^fn |^let mut |^impl /, 'rust'],
    [/^#include |^int main\(|^void /, 'cpp'],
    [/^\$ |^#.*bash|^#!/, 'bash'],
    [/^\{[\s\S]*\}$|^\[[\s\S]*\]$/, 'json'],
    [/^<\?xml|^<!DOCTYPE|^<html/, 'xml'],
  ];

  for (const [pattern, lang] of patterns) {
    if (pattern.test(content.trim())) {
      return lang;
    }
  }

  return '';
}

/**
 * Wrap content in code block with language detection
 */
export function wrapInCodeBlock(content: string, language?: string): string {
  const lang = language || detectLanguage(content);
  return `\`\`\`${lang}\n${content}\n\`\`\``;
}
