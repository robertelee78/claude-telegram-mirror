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

*Bot Commands:*
/status - Show current session status
/sessions - List active sessions
/attach <id> - Attach to a session
/detach - Detach from current session
/mute - Mute notifications
/unmute - Resume notifications
/abort - Abort current session
/help - Show this message

*Claude Code Commands:*
Use \`cc <command>\` to forward to Claude:
• \`cc clear\` - Clear conversation history
• \`cc compact\` - Compress context
• \`cc cost\` - Show token usage
• \`cc review\` - Code review mode
• \`cc model\` - Switch model
• \`cc <any>\` - Any command or skill

_Type \`cc\` for full command list_

*Inline Responses:*
Simply reply with text to send input to the session.

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

/**
 * Truncate text with ellipsis, respecting a max length
 */
function truncate(text: string, maxLen: number): string {
  if (text.length <= maxLen) return text;
  return text.slice(0, maxLen - 3) + '...';
}

/**
 * Get short filename from path
 */
function shortPath(path: string): string {
  const parts = path.split('/');
  if (parts.length <= 2) return path;
  return '.../' + parts.slice(-2).join('/');
}

/**
 * Format tool details for mobile-friendly Telegram display
 */
export function formatToolDetails(tool: string, input: unknown): string {
  const data = input as Record<string, unknown>;

  switch (tool) {
    case 'Edit': {
      const file = shortPath(String(data.file_path || ''));
      const oldStr = String(data.old_string || '');
      const newStr = String(data.new_string || '');

      let msg = `✏️ *Edit*\n📄 \`${file}\`\n\n`;

      // Show diff-style view
      if (oldStr) {
        msg += `➖ *Remove:*\n\`\`\`\n${truncate(oldStr, 800)}\n\`\`\`\n\n`;
      }
      if (newStr) {
        msg += `➕ *Add:*\n\`\`\`\n${truncate(newStr, 800)}\n\`\`\``;
      }
      return msg;
    }

    case 'Write': {
      const file = shortPath(String(data.file_path || ''));
      const content = String(data.content || '');
      const lines = content.split('\n').length;

      let msg = `📝 *Write*\n📄 \`${file}\`\n📏 ${lines} lines\n\n`;
      msg += `\`\`\`\n${truncate(content, 1500)}\n\`\`\``;
      return msg;
    }

    case 'Read': {
      const file = shortPath(String(data.file_path || ''));
      let msg = `👁 *Read*\n📄 \`${file}\``;
      if (data.offset) msg += `\n📍 Line ${data.offset}`;
      if (data.limit) msg += ` (+${data.limit} lines)`;
      return msg;
    }

    case 'Bash': {
      const cmd = String(data.command || '');
      let msg = `💻 *Bash*\n\n\`\`\`bash\n${truncate(cmd, 1500)}\n\`\`\``;
      if (data.timeout) msg += `\n⏱ Timeout: ${data.timeout}ms`;
      return msg;
    }

    case 'Grep': {
      const pattern = String(data.pattern || '');
      const path = data.path ? shortPath(String(data.path)) : 'cwd';
      let msg = `🔍 *Grep*\n🎯 Pattern: \`${truncate(pattern, 100)}\`\n📂 Path: \`${path}\``;
      if (data.glob) msg += `\n📋 Glob: \`${data.glob}\``;
      return msg;
    }

    case 'Glob': {
      const pattern = String(data.pattern || '');
      const path = data.path ? shortPath(String(data.path)) : 'cwd';
      return `📂 *Glob*\n🎯 Pattern: \`${pattern}\`\n📂 Path: \`${path}\``;
    }

    case 'Task': {
      const desc = String(data.description || '');
      const prompt = String(data.prompt || '');
      let msg = `🤖 *Task*\n📋 ${desc}`;
      if (prompt) {
        msg += `\n\n\`\`\`\n${truncate(prompt, 1000)}\n\`\`\``;
      }
      return msg;
    }

    case 'WebFetch': {
      const url = String(data.url || '');
      const prompt = String(data.prompt || '');
      return `🌐 *WebFetch*\n🔗 \`${truncate(url, 100)}\`\n📝 ${truncate(prompt, 200)}`;
    }

    case 'WebSearch': {
      const query = String(data.query || '');
      return `🔎 *WebSearch*\n📝 "${query}"`;
    }

    case 'TodoWrite': {
      const todos = data.todos as Array<{content: string; status: string}> | undefined;
      if (!todos || !Array.isArray(todos)) return `📋 *TodoWrite*`;

      let msg = `📋 *TodoWrite* (${todos.length} items)\n\n`;
      const statusEmoji: Record<string, string> = {
        'pending': '⬜',
        'in_progress': '🔄',
        'completed': '✅'
      };

      for (const todo of todos.slice(0, 10)) {
        const emoji = statusEmoji[todo.status] || '⬜';
        msg += `${emoji} ${truncate(todo.content, 60)}\n`;
      }
      if (todos.length > 10) msg += `... +${todos.length - 10} more`;
      return msg;
    }

    default: {
      // Generic JSON fallback but nicely formatted
      const jsonStr = JSON.stringify(input, null, 2);
      return `🔧 *${tool}*\n\n\`\`\`json\n${truncate(jsonStr, 2000)}\n\`\`\``;
    }
  }
}
