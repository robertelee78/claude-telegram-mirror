/**
 * Doctor Command - Diagnostic tool for troubleshooting
 * Checks all components and reports issues
 */

import { existsSync, readFileSync, statSync } from 'fs';
import { join } from 'path';
import { homedir, hostname, platform, arch } from 'os';
import { execSync } from 'child_process';

// ANSI color codes
const colors = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  cyan: '\x1b[36m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  red: '\x1b[31m',
  blue: '\x1b[34m',
  gray: '\x1b[90m',
};

function cyan(text: string): string { return `${colors.cyan}${text}${colors.reset}`; }
function green(text: string): string { return `${colors.green}${text}${colors.reset}`; }
function yellow(text: string): string { return `${colors.yellow}${text}${colors.reset}`; }
function red(text: string): string { return `${colors.red}${text}${colors.reset}`; }
function gray(text: string): string { return `${colors.gray}${text}${colors.reset}`; }
function bold(text: string): string { return `${colors.bold}${text}${colors.reset}`; }

interface CheckResult {
  name: string;
  status: 'pass' | 'warn' | 'fail';
  message: string;
  details?: string;
}

const CONFIG_DIR = join(homedir(), '.config', 'claude-telegram-mirror');
const CONFIG_FILE = join(CONFIG_DIR, 'config.json');
const SOCKET_PATH = join(CONFIG_DIR, 'bridge.sock');
const CLAUDE_SETTINGS = join(homedir(), '.claude', 'settings.json');

/**
 * Check if systemd service is installed and running
 */
function checkSystemdService(): CheckResult {
  const serviceFile = join(homedir(), '.config', 'systemd', 'user', 'claude-telegram-mirror.service');

  if (!existsSync(serviceFile)) {
    return {
      name: 'Systemd Service',
      status: 'warn',
      message: 'Service not installed',
      details: 'Run: ctm service install'
    };
  }

  try {
    const status = execSync('systemctl --user is-active claude-telegram-mirror 2>/dev/null', {
      encoding: 'utf8'
    }).trim();

    if (status === 'active') {
      return {
        name: 'Systemd Service',
        status: 'pass',
        message: 'Running'
      };
    }

    return {
      name: 'Systemd Service',
      status: 'warn',
      message: `Status: ${status}`,
      details: 'Run: ctm service start'
    };
  } catch {
    return {
      name: 'Systemd Service',
      status: 'warn',
      message: 'Installed but not running',
      details: 'Run: ctm service start'
    };
  }
}

/**
 * Check configuration file
 */
function checkConfig(): CheckResult {
  if (!existsSync(CONFIG_FILE)) {
    // Check env vars
    if (process.env.TELEGRAM_BOT_TOKEN && process.env.TELEGRAM_CHAT_ID) {
      return {
        name: 'Configuration',
        status: 'pass',
        message: 'Using environment variables'
      };
    }

    return {
      name: 'Configuration',
      status: 'fail',
      message: 'No config file or env vars found',
      details: 'Run: ctm setup'
    };
  }

  try {
    const config = JSON.parse(readFileSync(CONFIG_FILE, 'utf-8'));

    if (!config.botToken) {
      return {
        name: 'Configuration',
        status: 'fail',
        message: 'Missing botToken in config',
        details: 'Run: ctm setup'
      };
    }

    if (!config.chatId) {
      return {
        name: 'Configuration',
        status: 'fail',
        message: 'Missing chatId in config',
        details: 'Run: ctm setup'
      };
    }

    return {
      name: 'Configuration',
      status: 'pass',
      message: 'Valid config file',
      details: CONFIG_FILE
    };
  } catch (error) {
    return {
      name: 'Configuration',
      status: 'fail',
      message: 'Invalid config file',
      details: (error as Error).message
    };
  }
}

/**
 * Check Telegram connection
 */
async function checkTelegram(): Promise<CheckResult> {
  let token = process.env.TELEGRAM_BOT_TOKEN;

  if (!token && existsSync(CONFIG_FILE)) {
    try {
      const config = JSON.parse(readFileSync(CONFIG_FILE, 'utf-8'));
      token = config.botToken;
    } catch {
      // Ignore
    }
  }

  if (!token) {
    return {
      name: 'Telegram API',
      status: 'fail',
      message: 'No bot token configured'
    };
  }

  try {
    const response = await fetch(`https://api.telegram.org/bot${token}/getMe`);
    const data = await response.json() as { ok: boolean; result?: { username: string } };

    if (data.ok) {
      return {
        name: 'Telegram API',
        status: 'pass',
        message: `Connected as @${data.result?.username}`
      };
    }

    return {
      name: 'Telegram API',
      status: 'fail',
      message: 'Invalid bot token'
    };
  } catch (error) {
    return {
      name: 'Telegram API',
      status: 'fail',
      message: 'Network error',
      details: (error as Error).message
    };
  }
}

/**
 * Check Claude Code hooks
 */
function checkHooks(): CheckResult {
  if (!existsSync(CLAUDE_SETTINGS)) {
    return {
      name: 'Claude Code Hooks',
      status: 'warn',
      message: 'Claude settings not found',
      details: 'Run: ctm install-hooks'
    };
  }

  try {
    const settings = JSON.parse(readFileSync(CLAUDE_SETTINGS, 'utf-8'));
    const hooks = settings.hooks || {};

    const hasPreTool = hooks.PreToolUse?.some((h: { command: string }) =>
      h.command?.includes('telegram-hook')
    );
    const hasPostTool = hooks.PostToolUse?.some((h: { command: string }) =>
      h.command?.includes('telegram-hook')
    );
    const hasNotify = hooks.Notification?.some((h: { command: string }) =>
      h.command?.includes('telegram-hook')
    );

    const installed = [hasPreTool, hasPostTool, hasNotify].filter(Boolean).length;

    if (installed === 3) {
      return {
        name: 'Claude Code Hooks',
        status: 'pass',
        message: 'All hooks installed'
      };
    }

    if (installed > 0) {
      return {
        name: 'Claude Code Hooks',
        status: 'warn',
        message: `${installed}/3 hooks installed`,
        details: 'Run: ctm install-hooks -f'
      };
    }

    return {
      name: 'Claude Code Hooks',
      status: 'warn',
      message: 'No hooks installed',
      details: 'Run: ctm install-hooks'
    };
  } catch (error) {
    return {
      name: 'Claude Code Hooks',
      status: 'fail',
      message: 'Error reading Claude settings',
      details: (error as Error).message
    };
  }
}

/**
 * Check socket file
 */
function checkSocket(): CheckResult {
  if (!existsSync(SOCKET_PATH)) {
    return {
      name: 'Bridge Socket',
      status: 'warn',
      message: 'Socket not found (daemon not running?)',
      details: 'Run: ctm start'
    };
  }

  try {
    const stats = statSync(SOCKET_PATH);
    if (stats.isSocket()) {
      return {
        name: 'Bridge Socket',
        status: 'pass',
        message: 'Socket exists',
        details: SOCKET_PATH
      };
    }

    return {
      name: 'Bridge Socket',
      status: 'fail',
      message: 'Path exists but is not a socket'
    };
  } catch (error) {
    return {
      name: 'Bridge Socket',
      status: 'fail',
      message: 'Error checking socket',
      details: (error as Error).message
    };
  }
}

/**
 * Check tmux availability (for input injection)
 */
function checkTmux(): CheckResult {
  try {
    execSync('which tmux', { stdio: 'pipe' });

    // Check if TMUX env is set (running inside tmux)
    if (process.env.TMUX) {
      return {
        name: 'Tmux',
        status: 'pass',
        message: 'Available and active',
        details: 'Input injection will work'
      };
    }

    // Check for any tmux sessions
    try {
      const sessions = execSync('tmux list-sessions 2>/dev/null', { encoding: 'utf8' });
      const count = sessions.trim().split('\n').length;
      return {
        name: 'Tmux',
        status: 'pass',
        message: `Available (${count} session${count === 1 ? '' : 's'})`,
        details: 'Input injection available'
      };
    } catch {
      return {
        name: 'Tmux',
        status: 'warn',
        message: 'Available but no sessions',
        details: 'Start Claude Code in tmux for input injection'
      };
    }
  } catch {
    return {
      name: 'Tmux',
      status: 'warn',
      message: 'Not installed',
      details: 'Install tmux for Telegram → CLI input injection'
    };
  }
}

/**
 * Check Node.js version
 */
function checkNodeVersion(): CheckResult {
  const version = process.version;
  const major = parseInt(version.slice(1).split('.')[0], 10);

  if (major >= 18) {
    return {
      name: 'Node.js',
      status: 'pass',
      message: `Version ${version}`
    };
  }

  return {
    name: 'Node.js',
    status: 'fail',
    message: `Version ${version} (requires >=18)`,
    details: 'Update Node.js to version 18 or later'
  };
}

/**
 * Print check result
 */
function printResult(result: CheckResult): void {
  const icons = {
    pass: green('✓'),
    warn: yellow('⚠'),
    fail: red('✗'),
  };

  const statusColors = {
    pass: green,
    warn: yellow,
    fail: red,
  };

  console.log(`  ${icons[result.status]} ${bold(result.name)}: ${statusColors[result.status](result.message)}`);

  if (result.details) {
    console.log(`    ${gray(result.details)}`);
  }
}

/**
 * Run all diagnostic checks
 */
export async function runDoctor(): Promise<void> {
  console.log('');
  console.log(cyan('╔════════════════════════════════════════════════════════════╗'));
  console.log(cyan('║') + bold('  Claude Telegram Mirror - Diagnostics                      ') + cyan('║'));
  console.log(cyan('╚════════════════════════════════════════════════════════════╝'));
  console.log('');

  // System info
  console.log(gray('─'.repeat(60)));
  console.log(bold('System Information'));
  console.log(gray('─'.repeat(60)));
  console.log(`  Hostname: ${cyan(hostname())}`);
  console.log(`  Platform: ${cyan(`${platform()} ${arch()}`)}`);
  console.log(`  Node.js:  ${cyan(process.version)}`);
  console.log('');

  // Run checks
  console.log(gray('─'.repeat(60)));
  console.log(bold('Checks'));
  console.log(gray('─'.repeat(60)));
  console.log('');

  const checks: CheckResult[] = [];

  // Synchronous checks
  checks.push(checkNodeVersion());
  checks.push(checkConfig());
  checks.push(checkHooks());
  checks.push(checkSocket());
  checks.push(checkTmux());

  if (platform() === 'linux') {
    checks.push(checkSystemdService());
  }

  // Async checks
  checks.push(await checkTelegram());

  // Print all results
  for (const result of checks) {
    printResult(result);
  }

  console.log('');

  // Summary
  const passed = checks.filter(c => c.status === 'pass').length;
  const warnings = checks.filter(c => c.status === 'warn').length;
  const failed = checks.filter(c => c.status === 'fail').length;

  console.log(gray('─'.repeat(60)));
  console.log(bold('Summary'));
  console.log(gray('─'.repeat(60)));

  if (failed === 0 && warnings === 0) {
    console.log(green('  ✓ All checks passed! Everything looks good.'));
  } else {
    console.log(`  ${green(passed.toString())} passed, ${yellow(warnings.toString())} warnings, ${red(failed.toString())} failed`);

    if (failed > 0) {
      console.log('');
      console.log(red('  Some checks failed. Review the errors above.'));
    }
  }

  console.log('');

  // Quick actions
  if (failed > 0 || warnings > 0) {
    console.log(gray('─'.repeat(60)));
    console.log(bold('Suggested Actions'));
    console.log(gray('─'.repeat(60)));

    const configCheck = checks.find(c => c.name === 'Configuration');
    if (configCheck?.status === 'fail') {
      console.log(`  ${cyan('ctm setup')}           Run interactive setup`);
    }

    const hooksCheck = checks.find(c => c.name === 'Claude Code Hooks');
    if (hooksCheck?.status !== 'pass') {
      console.log(`  ${cyan('ctm install-hooks')}   Install Claude Code hooks`);
    }

    const serviceCheck = checks.find(c => c.name === 'Systemd Service');
    if (serviceCheck?.status !== 'pass') {
      console.log(`  ${cyan('ctm service install')} Install systemd service`);
      console.log(`  ${cyan('ctm service start')}   Start the service`);
    }

    const socketCheck = checks.find(c => c.name === 'Bridge Socket');
    if (socketCheck?.status !== 'pass') {
      console.log(`  ${cyan('ctm start')}           Start the daemon`);
    }

    console.log('');
  }
}

export default runDoctor;
