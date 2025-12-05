/**
 * Hook Installer
 * Installs Claude Code hooks for Telegram mirror integration
 */

import { existsSync, readFileSync, writeFileSync, mkdirSync, chmodSync } from 'fs';
import { join, dirname } from 'path';
import { homedir } from 'os';
import { execSync } from 'child_process';
import logger from '../utils/logger.js';

/**
 * Claude Code settings paths
 */
const CLAUDE_CONFIG_DIR = join(homedir(), '.claude');
const CLAUDE_SETTINGS_FILE = join(CLAUDE_CONFIG_DIR, 'settings.json');
const HOOK_SCRIPT_NAME = 'telegram-hook.sh';

/**
 * Hook configuration for Claude Code
 */
interface ClaudeHookConfig {
  type: 'command';
  command: string;
  timeout?: number;
}

interface ClaudeSettings {
  hooks?: {
    PreToolUse?: ClaudeHookConfig[];
    PostToolUse?: ClaudeHookConfig[];
    Notification?: ClaudeHookConfig[];
    Stop?: ClaudeHookConfig[];
    UserPromptSubmit?: ClaudeHookConfig[];
    [key: string]: ClaudeHookConfig[] | undefined;
  };
  [key: string]: unknown;
}

/**
 * Get the path to the hook script
 */
function getHookScriptPath(): string {
  // Check if installed globally via npm
  try {
    const globalPath = execSync('npm root -g', { encoding: 'utf8' }).trim();
    const globalScript = join(globalPath, 'claude-telegram-mirror', 'scripts', HOOK_SCRIPT_NAME);
    if (existsSync(globalScript)) {
      return globalScript;
    }
  } catch {
    // Not installed globally
  }

  // Check local development path
  const localScript = join(dirname(dirname(dirname(import.meta.url.replace('file://', '')))), 'scripts', HOOK_SCRIPT_NAME);
  if (existsSync(localScript)) {
    return localScript;
  }

  // Fallback: look in common locations
  const commonPaths = [
    '/opt/claude-mobile/packages/claude-telegram-mirror/scripts/telegram-hook.sh',
    join(homedir(), '.local', 'share', 'claude-telegram-mirror', HOOK_SCRIPT_NAME),
    join(homedir(), 'bin', HOOK_SCRIPT_NAME)
  ];

  for (const path of commonPaths) {
    if (existsSync(path)) {
      return path;
    }
  }

  throw new Error('Hook script not found. Please reinstall claude-telegram-mirror.');
}

/**
 * Load Claude settings
 */
function loadSettings(): ClaudeSettings {
  if (!existsSync(CLAUDE_SETTINGS_FILE)) {
    return {};
  }

  try {
    const content = readFileSync(CLAUDE_SETTINGS_FILE, 'utf8');
    return JSON.parse(content);
  } catch (error) {
    logger.warn('Failed to parse Claude settings, starting fresh', { error });
    return {};
  }
}

/**
 * Save Claude settings
 */
function saveSettings(settings: ClaudeSettings): void {
  // Ensure directory exists
  if (!existsSync(CLAUDE_CONFIG_DIR)) {
    mkdirSync(CLAUDE_CONFIG_DIR, { recursive: true });
  }

  writeFileSync(CLAUDE_SETTINGS_FILE, JSON.stringify(settings, null, 2));
}

/**
 * Create hook configuration
 */
function createHookConfig(scriptPath: string): ClaudeHookConfig {
  return {
    type: 'command',
    command: scriptPath,
    timeout: 300000 // 5 minutes for approvals
  };
}

/**
 * Check if hook is already installed
 */
function isHookInstalled(hooks: ClaudeHookConfig[] | undefined, scriptPath: string): boolean {
  if (!hooks) return false;
  return hooks.some(h => h.command === scriptPath || h.command.includes('telegram-hook'));
}

/**
 * Install Telegram hooks
 */
export function installHooks(options: { force?: boolean } = {}): {
  success: boolean;
  installed: string[];
  skipped: string[];
  error?: string;
} {
  const installed: string[] = [];
  const skipped: string[] = [];

  try {
    const scriptPath = getHookScriptPath();
    logger.info('Found hook script', { path: scriptPath });

    // Ensure script is executable
    chmodSync(scriptPath, 0o755);

    const settings = loadSettings();

    // Initialize hooks object if needed
    if (!settings.hooks) {
      settings.hooks = {};
    }

    // Hook types to install
    const hookTypes = [
      'PreToolUse',
      'PostToolUse',
      'Notification',
      'Stop',
      'UserPromptSubmit'
    ];

    for (const hookType of hookTypes) {
      const existingHooks = settings.hooks[hookType];

      if (!options.force && isHookInstalled(existingHooks, scriptPath)) {
        skipped.push(hookType);
        continue;
      }

      // Remove old telegram hooks if present
      const filteredHooks = existingHooks?.filter(
        h => !h.command.includes('telegram-hook')
      ) || [];

      // Add new hook
      filteredHooks.push(createHookConfig(scriptPath));

      settings.hooks[hookType] = filteredHooks;
      installed.push(hookType);
    }

    saveSettings(settings);

    logger.info('Hooks installed', { installed, skipped });

    return { success: true, installed, skipped };

  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    logger.error('Failed to install hooks', { error: errorMessage });
    return { success: false, installed, skipped, error: errorMessage };
  }
}

/**
 * Uninstall Telegram hooks
 */
export function uninstallHooks(): {
  success: boolean;
  removed: string[];
  error?: string;
} {
  const removed: string[] = [];

  try {
    const settings = loadSettings();

    if (!settings.hooks) {
      return { success: true, removed };
    }

    for (const hookType of Object.keys(settings.hooks)) {
      const hooks = settings.hooks[hookType];
      if (!hooks) continue;

      const filteredHooks = hooks.filter(
        h => !h.command.includes('telegram-hook')
      );

      if (filteredHooks.length < hooks.length) {
        removed.push(hookType);
      }

      if (filteredHooks.length === 0) {
        delete settings.hooks[hookType];
      } else {
        settings.hooks[hookType] = filteredHooks;
      }
    }

    // Remove empty hooks object
    if (Object.keys(settings.hooks).length === 0) {
      delete settings.hooks;
    }

    saveSettings(settings);

    logger.info('Hooks uninstalled', { removed });

    return { success: true, removed };

  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    logger.error('Failed to uninstall hooks', { error: errorMessage });
    return { success: false, removed, error: errorMessage };
  }
}

/**
 * Check hook installation status
 */
export function checkHookStatus(): {
  installed: boolean;
  hooks: string[];
  scriptPath?: string;
  error?: string;
} {
  try {
    const settings = loadSettings();
    const hooks: string[] = [];

    if (settings.hooks) {
      for (const [hookType, hookConfigs] of Object.entries(settings.hooks)) {
        if (hookConfigs?.some(h => h.command.includes('telegram-hook'))) {
          hooks.push(hookType);
        }
      }
    }

    let scriptPath: string | undefined;
    try {
      scriptPath = getHookScriptPath();
    } catch {
      // Script not found
    }

    return {
      installed: hooks.length > 0,
      hooks,
      scriptPath
    };

  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    return {
      installed: false,
      hooks: [],
      error: errorMessage
    };
  }
}

/**
 * Print hook status
 */
export function printHookStatus(): void {
  const status = checkHookStatus();

  console.log('\n📌 Claude Code Telegram Hook Status\n');

  if (status.error) {
    console.log(`❌ Error: ${status.error}\n`);
    return;
  }

  if (status.installed) {
    console.log('✅ Hooks installed\n');
    console.log('Active hooks:');
    status.hooks.forEach(hook => {
      console.log(`  • ${hook}`);
    });
    console.log(`\nScript: ${status.scriptPath || 'Not found'}`);
  } else {
    console.log('❌ Hooks not installed\n');
    console.log('Run: claude-telegram-mirror install-hooks');
  }

  console.log('');
}

export default {
  installHooks,
  uninstallHooks,
  checkHookStatus,
  printHookStatus
};
