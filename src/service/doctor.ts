/**
 * Doctor Command - Diagnostic tool for troubleshooting
 * Checks all components and reports issues
 */

import { existsSync, readFileSync, statSync, chmodSync, unlinkSync } from 'fs';
import { join } from 'path';
import { homedir, hostname, platform, arch } from 'os';
import { execSync } from 'child_process';
import { ensureConfigDir } from '../utils/config.js';

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
  fixed?: boolean;
  fixMessage?: string;
}

const CONFIG_DIR = join(homedir(), '.config', 'claude-telegram-mirror');
const CONFIG_FILE = join(CONFIG_DIR, 'config.json');
const SOCKET_PATH = join(CONFIG_DIR, 'bridge.sock');
const PID_PATH = join(CONFIG_DIR, 'bridge.pid');
const CLAUDE_SETTINGS = join(homedir(), '.claude', 'settings.json');

/**
 * Check config directory existence and permissions
 */
function checkConfigDir(fix: boolean): CheckResult {
  if (!existsSync(CONFIG_DIR)) {
    if (fix) {
      try {
        ensureConfigDir(CONFIG_DIR);
        return {
          name: 'Config Directory',
          status: 'pass',
          message: 'Missing config directory',
          fixed: true,
          fixMessage: `created ${CONFIG_DIR}`
        };
      } catch (error) {
        return {
          name: 'Config Directory',
          status: 'fail',
          message: 'Missing config directory (auto-fix failed)',
          details: (error as Error).message
        };
      }
    }
    return {
      name: 'Config Directory',
      status: 'warn',
      message: 'Config directory does not exist',
      details: `Expected: ${CONFIG_DIR}`
    };
  }

  try {
    const stats = statSync(CONFIG_DIR);
    const mode = stats.mode & 0o777;
    if (mode !== 0o700) {
      const octalMode = `0o${mode.toString(8)}`;
      if (fix) {
        try {
          chmodSync(CONFIG_DIR, 0o700);
          return {
            name: 'Config Directory',
            status: 'pass',
            message: `Config directory has insecure permissions (${octalMode})`,
            fixed: true,
            fixMessage: 'set to 0o700'
          };
        } catch (error) {
          return {
            name: 'Config Directory',
            status: 'warn',
            message: `Config directory has insecure permissions (${octalMode}) — auto-fix failed`,
            details: (error as Error).message
          };
        }
      }
      return {
        name: 'Config Directory',
        status: 'warn',
        message: `Config directory has insecure permissions (${octalMode})`,
        details: `Expected 0o700, got ${octalMode}`
      };
    }
    return {
      name: 'Config Directory',
      status: 'pass',
      message: `Exists with correct permissions (0o700)`
    };
  } catch (error) {
    return {
      name: 'Config Directory',
      status: 'fail',
      message: 'Error checking config directory',
      details: (error as Error).message
    };
  }
}

/**
 * Check stale PID file
 */
function checkPidFile(fix: boolean): CheckResult {
  if (!existsSync(PID_PATH)) {
    return {
      name: 'PID File',
      status: 'pass',
      message: 'No stale PID file'
    };
  }

  try {
    const pidStr = readFileSync(PID_PATH, 'utf-8').trim();
    const pid = parseInt(pidStr, 10);
    if (!isNaN(pid)) {
      // Check if process is actually running
      try {
        process.kill(pid, 0);
        // Process exists — PID file is valid
        return {
          name: 'PID File',
          status: 'pass',
          message: `Daemon running (PID ${pid})`
        };
      } catch {
        // Process not running — stale PID file
      }
    }

    // Stale PID file
    if (fix) {
      try {
        unlinkSync(PID_PATH);
        return {
          name: 'PID File',
          status: 'pass',
          message: 'Stale PID file detected',
          fixed: true,
          fixMessage: 'removed stale PID file'
        };
      } catch (error) {
        return {
          name: 'PID File',
          status: 'warn',
          message: 'Stale PID file — auto-fix failed',
          details: (error as Error).message
        };
      }
    }
    return {
      name: 'PID File',
      status: 'warn',
      message: `Stale PID file (process not running)`,
      details: PID_PATH
    };
  } catch (error) {
    return {
      name: 'PID File',
      status: 'fail',
      message: 'Error reading PID file',
      details: (error as Error).message
    };
  }
}

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
function checkHooks(fix: boolean): CheckResult {
  if (!existsSync(CLAUDE_SETTINGS)) {
    const result: CheckResult = {
      name: 'Claude Code Hooks',
      status: 'warn',
      message: 'Claude settings not found',
      details: 'Run: ctm install-hooks'
    };
    if (fix) {
      console.log(`    ${gray('→ Suggestion: Run `ctm install-hooks` to fix')}`);
    }
    return result;
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
      const result: CheckResult = {
        name: 'Claude Code Hooks',
        status: 'warn',
        message: `${installed}/3 hooks installed`,
        details: 'Run: ctm install-hooks -f'
      };
      if (fix) {
        console.log(`    ${gray('→ Suggestion: Run `ctm install-hooks -f` to fix')}`);
      }
      return result;
    }

    const result: CheckResult = {
      name: 'Claude Code Hooks',
      status: 'warn',
      message: 'No hooks installed',
      details: 'Run: ctm install-hooks'
    };
    if (fix) {
      console.log(`    ${gray('→ Suggestion: Run `ctm install-hooks` to fix')}`);
    }
    return result;
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
function checkSocket(fix: boolean): CheckResult {
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
      // Socket exists — check if the daemon PID is alive to determine staleness
      if (existsSync(PID_PATH)) {
        try {
          const pidStr = readFileSync(PID_PATH, 'utf-8').trim();
          const pid = parseInt(pidStr, 10);
          if (!isNaN(pid)) {
            try {
              process.kill(pid, 0);
              // Daemon is alive — socket is valid
              return {
                name: 'Bridge Socket',
                status: 'pass',
                message: 'Socket exists',
                details: SOCKET_PATH
              };
            } catch {
              // Daemon is not running — stale socket
              if (fix) {
                try {
                  unlinkSync(SOCKET_PATH);
                  return {
                    name: 'Bridge Socket',
                    status: 'pass',
                    message: 'Stale socket file (daemon not running)',
                    fixed: true,
                    fixMessage: 'removed stale socket file'
                  };
                } catch (unlinkError) {
                  return {
                    name: 'Bridge Socket',
                    status: 'warn',
                    message: 'Stale socket file — auto-fix failed',
                    details: (unlinkError as Error).message
                  };
                }
              }
              return {
                name: 'Bridge Socket',
                status: 'warn',
                message: 'Stale socket file (daemon not running)',
                details: SOCKET_PATH
              };
            }
          }
        } catch {
          // Can't read PID file — treat socket as existing
        }
      }
      return {
        name: 'Bridge Socket',
        status: 'pass',
        message: 'Socket exists',
        details: SOCKET_PATH
      };
    }

    // Path exists but is not a socket — stale/corrupt file
    if (fix) {
      try {
        unlinkSync(SOCKET_PATH);
        return {
          name: 'Bridge Socket',
          status: 'pass',
          message: 'Path exists but is not a socket',
          fixed: true,
          fixMessage: 'removed invalid socket file'
        };
      } catch (unlinkError) {
        return {
          name: 'Bridge Socket',
          status: 'fail',
          message: 'Path exists but is not a socket — auto-fix failed',
          details: (unlinkError as Error).message
        };
      }
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

  const fixSuffix = result.fixed
    ? ` ${green(`→ FIXED (${result.fixMessage})`)}`
    : '';

  console.log(`  ${icons[result.status]} ${bold(result.name)}: ${statusColors[result.status](result.message)}${fixSuffix}`);

  if (result.details) {
    console.log(`    ${gray(result.details)}`);
  }
}

/**
 * Run all diagnostic checks
 */
export async function runDoctor(fix: boolean = false): Promise<void> {
  console.log('');
  console.log(cyan('╔════════════════════════════════════════════════════════════╗'));
  console.log(cyan('║') + bold('  Claude Telegram Mirror - Diagnostics                      ') + cyan('║'));
  console.log(cyan('╚════════════════════════════════════════════════════════════╝'));
  console.log('');

  if (fix) {
    console.log(yellow('  Auto-fix mode enabled. Safe issues will be remediated automatically.'));
    console.log('');
  }

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
  checks.push(checkConfigDir(fix));
  checks.push(checkConfig());
  checks.push(checkPidFile(fix));
  checks.push(checkHooks(fix));
  checks.push(checkSocket(fix));
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
  const fixed = checks.filter(c => c.fixed).length;
  const issuesFound = warnings + failed;
  const requireManual = issuesFound - fixed;

  console.log(gray('─'.repeat(60)));
  console.log(bold('Summary'));
  console.log(gray('─'.repeat(60)));

  if (fix) {
    console.log(`  ${issuesFound} issue${issuesFound !== 1 ? 's' : ''} found, ${green(fixed.toString())} auto-fixed, ${requireManual > 0 ? yellow(requireManual.toString()) : '0'} require manual action`);
  }

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
    if (hooksCheck?.status !== 'pass' && !hooksCheck?.fixed) {
      console.log(`  ${cyan('ctm install-hooks')}   Install Claude Code hooks`);
    }

    const serviceCheck = checks.find(c => c.name === 'Systemd Service');
    if (serviceCheck?.status !== 'pass') {
      console.log(`  ${cyan('ctm service install')} Install systemd service`);
      console.log(`  ${cyan('ctm service start')}   Start the service`);
    }

    const socketCheck = checks.find(c => c.name === 'Bridge Socket');
    if (socketCheck?.status !== 'pass' && !socketCheck?.fixed) {
      console.log(`  ${cyan('ctm start')}           Start the daemon`);
    }

    console.log('');
  }
}

export default runDoctor;
