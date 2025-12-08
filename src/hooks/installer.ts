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
const NODE_HANDLER_NAME = 'dist/hooks/handler.js';

/**
 * Hook configuration for Claude Code (old format)
 */
interface ClaudeHookConfig {
  type: 'command';
  command: string;
  timeout?: number;
}

/**
 * Hook entry with matcher wrapper (required format for Claude Code)
 */
interface ClaudeHookEntry {
  matcher: string;
  hooks: ClaudeHookConfig[];
}

// Union type for hooks (can be old or new format)
type ClaudeHookItem = ClaudeHookConfig | ClaudeHookEntry;

interface ClaudeSettings {
  hooks?: {
    PreToolUse?: ClaudeHookItem[];
    PostToolUse?: ClaudeHookItem[];
    Notification?: ClaudeHookItem[];
    Stop?: ClaudeHookItem[];
    UserPromptSubmit?: ClaudeHookItem[];
    PreCompact?: ClaudeHookItem[];
    [key: string]: ClaudeHookItem[] | undefined;
  };
  [key: string]: unknown;
}

/**
 * Get the path to the hook script
 * Priority order:
 * 1. Standard user install (~/.local/share/claude-telegram-mirror/scripts/)
 * 2. npm global install
 * 3. Local development (relative to this file)
 * 4. Legacy /opt/ path (for backward compatibility)
 */
function getHookScriptPath(): string {
  // 1. Check standard user install path FIRST (from install.sh)
  const userInstallPath = join(homedir(), '.local', 'share', 'claude-telegram-mirror', 'scripts', HOOK_SCRIPT_NAME);
  if (existsSync(userInstallPath)) {
    return userInstallPath;
  }

  // 2. Check if installed globally via npm
  try {
    const globalPath = execSync('npm root -g', { encoding: 'utf8' }).trim();
    const globalScript = join(globalPath, 'claude-telegram-mirror', 'scripts', HOOK_SCRIPT_NAME);
    if (existsSync(globalScript)) {
      return globalScript;
    }
  } catch {
    // Not installed globally
  }

  // 3. Check local development path (relative to compiled dist/hooks/installer.js)
  const localScript = join(dirname(dirname(dirname(import.meta.url.replace('file://', '')))), 'scripts', HOOK_SCRIPT_NAME);
  if (existsSync(localScript)) {
    return localScript;
  }

  // 4. Fallback: legacy /opt/ path for backward compatibility
  const legacyPath = '/opt/claude-mobile/packages/claude-telegram-mirror/scripts/telegram-hook.sh';
  if (existsSync(legacyPath)) {
    return legacyPath;
  }

  throw new Error('Hook script not found. Please reinstall claude-telegram-mirror.');
}

/**
 * Get the path to the Node.js hook handler for PreToolUse (used for Telegram approvals)
 * Returns a command string like: "node /path/to/dist/hooks/handler.js"
 */
function getNodeHandlerCommand(): string {
  // 1. Check if installed globally via npm
  try {
    const globalPath = execSync('npm root -g', { encoding: 'utf8' }).trim();
    const globalHandler = join(globalPath, 'claude-telegram-mirror', NODE_HANDLER_NAME);
    if (existsSync(globalHandler)) {
      return `node "${globalHandler}"`;
    }
  } catch {
    // Not installed globally
  }

  // 2. Check local development path (relative to compiled dist/hooks/installer.js)
  const localHandler = join(dirname(dirname(dirname(import.meta.url.replace('file://', '')))), NODE_HANDLER_NAME);
  if (existsSync(localHandler)) {
    return `node "${localHandler}"`;
  }

  // 3. Fallback: legacy /opt/ path for backward compatibility
  const legacyPath = '/opt/claude-mobile/packages/claude-telegram-mirror/' + NODE_HANDLER_NAME;
  if (existsSync(legacyPath)) {
    return `node "${legacyPath}"`;
  }

  throw new Error('Node.js hook handler not found. Please reinstall claude-telegram-mirror.');
}

/**
 * Load Claude settings
 */
function loadSettings(settingsPath: string = CLAUDE_SETTINGS_FILE): ClaudeSettings {
  if (!existsSync(settingsPath)) {
    return {};
  }

  try {
    const content = readFileSync(settingsPath, 'utf8');
    return JSON.parse(content);
  } catch (error) {
    logger.warn('Failed to parse Claude settings, starting fresh', { error, path: settingsPath });
    return {};
  }
}

/**
 * Save Claude settings
 */
function saveSettings(settings: ClaudeSettings, settingsPath: string = CLAUDE_SETTINGS_FILE, configDir: string = CLAUDE_CONFIG_DIR): void {
  // Ensure directory exists
  if (!existsSync(configDir)) {
    mkdirSync(configDir, { recursive: true });
  }

  writeFileSync(settingsPath, JSON.stringify(settings, null, 2));
}

/**
 * Create hook configuration with required matcher wrapper
 * Claude Code requires: { matcher: "", hooks: [{ type, command }] }
 */
function createHookEntry(scriptPath: string): ClaudeHookEntry {
  return {
    matcher: '',
    hooks: [
      {
        type: 'command',
        command: scriptPath
      }
    ]
  };
}

/**
 * Create hook configuration for PreToolUse with Telegram approval support
 * Uses Node.js handler which can do bidirectional socket communication
 * Timeout is set to 310 seconds (5 min for user + 10s buffer) since approvals may wait for user input
 */
function createApprovalHookEntry(nodeHandlerCommand: string): ClaudeHookEntry {
  return {
    matcher: '',
    hooks: [
      {
        type: 'command',
        command: nodeHandlerCommand,
        timeout: 310  // 5 minutes + 10s buffer for Telegram approval
      }
    ]
  };
}


/**
 * Install Telegram hooks
 */
export function installHooks(options: { force?: boolean; project?: boolean; projectPath?: string } = {}): {
  success: boolean;
  installed: string[];
  skipped: string[];
  settingsPath: string;
  error?: string;
} {
  const installed: string[] = [];
  const skipped: string[] = [];

  // Determine which settings file to use
  let settingsPath = CLAUDE_SETTINGS_FILE;
  let configDir = CLAUDE_CONFIG_DIR;

  if (options.project || options.projectPath) {
    // Use specified path or current directory's .claude/settings.json
    const basePath = options.projectPath || process.cwd();
    const projectSettings = join(basePath, '.claude', 'settings.json');
    const projectConfigDir = join(basePath, '.claude');

    if (!existsSync(projectConfigDir)) {
      return {
        success: false,
        installed,
        skipped,
        settingsPath: projectSettings,
        error: `No .claude directory found in ${basePath}. Run from a Claude project directory.`
      };
    }

    settingsPath = projectSettings;
    configDir = projectConfigDir;
    logger.info('Installing to project settings', { path: settingsPath });
  }

  try {
    const scriptPath = getHookScriptPath();
    logger.info('Found hook script', { path: scriptPath });

    // Ensure script is executable
    chmodSync(scriptPath, 0o755);

    // Get Node.js handler for PreToolUse (supports Telegram approvals)
    let nodeHandlerCommand: string | null = null;
    try {
      nodeHandlerCommand = getNodeHandlerCommand();
      logger.info('Found Node.js handler', { command: nodeHandlerCommand });
    } catch (error) {
      logger.warn('Node.js handler not found, PreToolUse will use bash script (no approval support)', { error });
    }

    const settings = loadSettings(settingsPath);

    // Initialize hooks object if needed
    if (!settings.hooks) {
      settings.hooks = {};
    }

    // Hook types to install (fewer for project - just the essentials)
    // PreToolUse only makes sense for global hooks (approval workflow)
    const hookTypes = options.project
      ? ['Notification', 'Stop', 'UserPromptSubmit', 'PreCompact']
      : ['PreToolUse', 'PostToolUse', 'Notification', 'Stop', 'UserPromptSubmit', 'PreCompact'];

    for (const hookType of hookTypes) {
      const existingHooks = settings.hooks[hookType];

      // Check for existing telegram hooks (both bash script and node handler)
      const isInstalled = existingHooks?.some(h => {
        if ('hooks' in h && Array.isArray(h.hooks)) {
          return h.hooks.some(hh =>
            hh.command?.includes('telegram-hook') ||
            hh.command?.includes('hooks/handler')
          );
        }
        if ('command' in h) {
          return h.command?.includes('telegram-hook') || h.command?.includes('hooks/handler');
        }
        return false;
      });

      if (!options.force && isInstalled) {
        skipped.push(hookType);
        continue;
      }

      // Handle array format
      if (Array.isArray(existingHooks)) {
        // Remove old telegram hooks if present (handles both formats)
        const filteredHooks = existingHooks.filter(h => {
          if ('hooks' in h && Array.isArray(h.hooks)) {
            return !h.hooks.some(hh =>
              hh.command?.includes('telegram-hook') ||
              hh.command?.includes('hooks/handler')
            );
          }
          if ('command' in h) {
            return !h.command?.includes('telegram-hook') && !h.command?.includes('hooks/handler');
          }
          return true;
        });

        // Add new hook at the beginning (runs first)
        // PreToolUse uses Node.js handler (supports Telegram approval flow)
        // Other hooks use bash script (faster, fire-and-forget)
        if (hookType === 'PreToolUse' && nodeHandlerCommand) {
          filteredHooks.unshift(createApprovalHookEntry(nodeHandlerCommand));
        } else {
          filteredHooks.unshift(createHookEntry(scriptPath));
        }

        settings.hooks[hookType] = filteredHooks;
        installed.push(hookType);
      } else {
        // No existing hooks, create new array with correct format
        if (hookType === 'PreToolUse' && nodeHandlerCommand) {
          settings.hooks[hookType] = [createApprovalHookEntry(nodeHandlerCommand)];
        } else {
          settings.hooks[hookType] = [createHookEntry(scriptPath)];
        }
        installed.push(hookType);
      }
    }

    saveSettings(settings, settingsPath, configDir);

    logger.info('Hooks installed', { installed, skipped, settingsPath });

    return { success: true, installed, skipped, settingsPath };

  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    logger.error('Failed to install hooks', { error: errorMessage });
    return { success: false, installed, skipped, settingsPath, error: errorMessage };
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
      const hooks = settings.hooks[hookType] as (ClaudeHookConfig | ClaudeHookEntry)[];
      if (!hooks) continue;

      const filteredHooks = hooks.filter(h => {
        // New format: { matcher, hooks: [...] }
        if ('hooks' in h && Array.isArray(h.hooks)) {
          return !h.hooks.some(hh =>
            hh.command?.includes('telegram-hook') ||
            hh.command?.includes('hooks/handler')
          );
        }
        // Old format: { type, command }
        if ('command' in h) {
          return !h.command?.includes('telegram-hook') && !h.command?.includes('hooks/handler');
        }
        return true;
      });

      if (filteredHooks.length < hooks.length) {
        removed.push(hookType);
      }

      if (filteredHooks.length === 0) {
        delete settings.hooks[hookType];
      } else {
        settings.hooks[hookType] = filteredHooks as ClaudeHookConfig[];
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
        const configs = hookConfigs as (ClaudeHookConfig | ClaudeHookEntry)[];
        if (configs?.some(h => {
          // New format: { matcher, hooks: [...] }
          if ('hooks' in h && Array.isArray(h.hooks)) {
            return h.hooks.some(hh =>
              hh.command?.includes('telegram-hook') ||
              hh.command?.includes('hooks/handler')
            );
          }
          // Old format: { type, command }
          if ('command' in h) {
            return h.command?.includes('telegram-hook') || h.command?.includes('hooks/handler');
          }
          return false;
        })) {
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
 * Check if hooks need updating (e.g., PreToolUse missing Node handler for approval)
 */
export function hooksNeedUpdate(): boolean {
  try {
    const settings = loadSettings();

    if (!settings.hooks?.PreToolUse) {
      return false; // No hooks installed at all
    }

    const preToolHooks = settings.hooks.PreToolUse as ClaudeHookItem[];

    // Check if any telegram hook exists for PreToolUse
    const hasTelegramHook = preToolHooks.some(h => {
      if ('hooks' in h && Array.isArray(h.hooks)) {
        return h.hooks.some(hh =>
          hh.command?.includes('telegram-hook') ||
          hh.command?.includes('hooks/handler')
        );
      }
      if ('command' in h) {
        return h.command?.includes('telegram-hook') || h.command?.includes('hooks/handler');
      }
      return false;
    });

    if (!hasTelegramHook) {
      return false; // Not our hooks
    }

    // Check if PreToolUse uses the Node handler (for approval support)
    const hasNodeHandler = preToolHooks.some(h => {
      if ('hooks' in h && Array.isArray(h.hooks)) {
        return h.hooks.some(hh => hh.command?.includes('hooks/handler'));
      }
      if ('command' in h) {
        return h.command?.includes('hooks/handler');
      }
      return false;
    });

    // If PreToolUse doesn't have Node handler, it needs updating
    return !hasNodeHandler;
  } catch {
    return false;
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
  hooksNeedUpdate,
  printHookStatus
};
