#!/usr/bin/env node
/**
 * Claude Telegram Mirror CLI
 * Main command-line interface
 */

import { Command } from 'commander';
import { readFileSync, existsSync, unlinkSync } from 'fs';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import { homedir } from 'os';
import { BridgeDaemon } from './bridge/daemon.js';
import { installHooks, uninstallHooks, printHookStatus } from './hooks/installer.js';
import { loadConfig, validateConfig } from './utils/config.js';
import {
  installService,
  uninstallService,
  getServiceStatus,
  startService,
  stopService,
  restartService,
  isServiceInstalled
} from './service/manager.js';
import { runSetup } from './service/setup.js';
import { runDoctor } from './service/doctor.js';

// Get version from package.json
const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const packageJson = JSON.parse(readFileSync(join(__dirname, '..', 'package.json'), 'utf8'));

const program = new Command();

program
  .name('claude-telegram-mirror')
  .description('Mirror Claude Code sessions to Telegram')
  .version(packageJson.version);

/**
 * Start command - Run the bridge daemon
 */
program
  .command('start')
  .description('Start the bridge daemon')
  .option('-v, --verbose', 'Enable verbose logging')
  .option('--foreground', 'Run in foreground (default)', true)
  .action(async (options) => {
    console.log('🚀 Starting Claude Code Telegram Mirror...\n');

    // Load and validate config
    const config = loadConfig();
    const validation = validateConfig(config);

    if (!validation.valid) {
      console.error('❌ Configuration errors:');
      validation.errors.forEach((err: string) => console.error(`   • ${err}`));
      console.error('\nSee: claude-telegram-mirror config --help\n');
      process.exit(1);
    }

    if (validation.warnings.length > 0) {
      console.warn('⚠️  Warnings:');
      validation.warnings.forEach((warn: string) => console.warn(`   • ${warn}`));
      console.log('');
    }

    // Override verbose from CLI
    if (options.verbose) {
      config.verbose = true;
    }

    // Create and start daemon
    const daemon = new BridgeDaemon(config);

    // Handle shutdown signals
    const shutdown = async (signal: string) => {
      console.log(`\n📴 Received ${signal}, shutting down...`);
      await daemon.stop();
      process.exit(0);
    };

    process.on('SIGINT', () => shutdown('SIGINT'));
    process.on('SIGTERM', () => shutdown('SIGTERM'));

    try {
      await daemon.start();
      console.log('✅ Bridge daemon running\n');
      console.log('📱 Telegram chat will receive Claude Code updates');
      console.log('💬 Reply in Telegram to send input to CLI\n');
      console.log('Press Ctrl+C to stop\n');
    } catch (error) {
      console.error('❌ Failed to start daemon:', error);
      process.exit(1);
    }
  });

/**
 * Status command - Show daemon status
 */
program
  .command('status')
  .description('Show bridge daemon status')
  .action(() => {
    const config = loadConfig();
    const configDirPath = join(homedir(), '.config', 'claude-telegram-mirror');
    const pidFilePath = join(configDirPath, 'bridge.pid');  // Must match socket.ts DEFAULT_PID_PATH
    const socketPath = join(configDirPath, 'bridge.sock');

    console.log('\n📊 Claude Telegram Mirror Status\n');

    // Check daemon running state
    console.log('Daemon:');
    let daemonRunning = false;
    let daemonPid: number | null = null;

    // Check if running as OS service
    if (isServiceInstalled()) {
      const serviceStatus = getServiceStatus();
      if (serviceStatus.running) {
        console.log('  Status: ✅ Running (via system service)');
        daemonRunning = true;
      } else if (existsSync(pidFilePath)) {
        daemonPid = parseInt(readFileSync(pidFilePath, 'utf8').trim());
        if (isProcessRunning(daemonPid)) {
          console.log(`  Status: ✅ Running (PID ${daemonPid})`);
          daemonRunning = true;
        } else {
          console.log('  Status: ⚪ Not running (stale PID file)');
        }
      } else {
        console.log('  Status: ⚪ Not running');
      }
    } else if (existsSync(pidFilePath)) {
      daemonPid = parseInt(readFileSync(pidFilePath, 'utf8').trim());
      if (isProcessRunning(daemonPid)) {
        console.log(`  Status: ✅ Running (PID ${daemonPid})`);
        daemonRunning = true;
      } else {
        console.log('  Status: ⚪ Not running (stale PID file)');
      }
    } else {
      console.log('  Status: ⚪ Not running');
    }

    // Check socket
    if (existsSync(socketPath)) {
      console.log(`  Socket: ✅ ${socketPath}`);
    } else if (daemonRunning) {
      console.log(`  Socket: ⚠️  Missing (expected: ${socketPath})`);
    } else {
      console.log(`  Socket: ⚪ Not created`);
    }
    console.log('');

    // Check configuration
    console.log('Configuration:');
    console.log(`  Bot Token: ${config.botToken ? '✅ Set' : '❌ Not set'}`);
    console.log(`  Chat ID: ${config.chatId ? `✅ ${config.chatId}` : '❌ Not set'}`);
    console.log(`  Enabled: ${config.enabled ? '✅ Yes' : '⚪ No'}`);
    console.log(`  Verbose: ${config.verbose ? '✅ Yes' : '⚪ No'}`);
    console.log('');

    // Check hooks
    printHookStatus();
  });

/**
 * Config command - Show/set configuration
 */
program
  .command('config')
  .description('Show or modify configuration')
  .option('--show', 'Show current configuration')
  .option('--test', 'Test Telegram connection')
  .action(async (options) => {
    const config = loadConfig();

    if (options.test) {
      console.log('🔄 Testing Telegram connection...\n');

      try {
        const response = await fetch(
          `https://api.telegram.org/bot${config.botToken}/getMe`
        );
        const data = await response.json() as { ok: boolean; result?: { username: string } };

        if (data.ok) {
          console.log(`✅ Bot connected: @${data.result?.username}`);

          // Try sending a test message
          const msgResponse = await fetch(
            `https://api.telegram.org/bot${config.botToken}/sendMessage`,
            {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({
                chat_id: config.chatId,
                text: '🧪 Test message from Claude Telegram Mirror',
                parse_mode: 'Markdown'
              })
            }
          );
          const msgData = await msgResponse.json() as { ok: boolean };

          if (msgData.ok) {
            console.log('✅ Test message sent to chat');
          } else {
            console.log('❌ Failed to send test message');
          }
        } else {
          console.log('❌ Invalid bot token');
        }
      } catch (error) {
        console.error('❌ Connection failed:', error);
      }
      return;
    }

    // Default: show config
    console.log('\n⚙️  Configuration\n');
    console.log('Environment Variables:');
    console.log(`  TELEGRAM_MIRROR=${config.enabled}`);
    console.log(`  TELEGRAM_BOT_TOKEN=${config.botToken ? '[SET]' : '[NOT SET]'}`);
    console.log(`  TELEGRAM_CHAT_ID=${config.chatId || '[NOT SET]'}`);
    console.log(`  TELEGRAM_MIRROR_VERBOSE=${config.verbose}`);
    console.log(`  TELEGRAM_BRIDGE_SOCKET=${config.socketPath}`);
    console.log('');
    console.log('Add to ~/.bashrc or ~/.zshrc:');
    console.log('');
    console.log('  export TELEGRAM_MIRROR=true');
    console.log('  export TELEGRAM_BOT_TOKEN="your-bot-token"');
    console.log('  export TELEGRAM_CHAT_ID="your-chat-id"');
    console.log('');
  });

/**
 * Install hooks command
 */
program
  .command('install-hooks')
  .description('Install Claude Code hooks')
  .option('-p, --project', 'Install to current project\'s .claude/settings.json (run from project directory)')
  .action((options) => {
    if (options.project) {
      console.log(`📌 Installing hooks to project: ${process.cwd()}\n`);
    } else {
      console.log('📌 Configuring Claude Code hooks (global)...\n');
    }

    const result = installHooks({ project: options.project });

    if (result.success) {
      const added = result.changes.filter(c => c.status === 'added');
      const updated = result.changes.filter(c => c.status === 'updated');
      const unchanged = result.changes.filter(c => c.status === 'unchanged');

      if (added.length > 0) {
        console.log('✅ Added hooks:');
        added.forEach(c => {
          const details = c.details ? ` (${c.details})` : '';
          console.log(`   • ${c.hookType}${details}`);
        });
      }

      if (updated.length > 0) {
        console.log('\n🔄 Updated hooks:');
        updated.forEach(c => {
          const details = c.details ? ` (${c.details})` : '';
          console.log(`   • ${c.hookType}${details}`);
        });
      }

      if (unchanged.length > 0) {
        console.log('\n✓ Already correct:');
        unchanged.forEach(c => console.log(`   • ${c.hookType}`));
      }

      // Summary
      const changedCount = added.length + updated.length;
      if (changedCount > 0) {
        console.log(`\n✅ Configuration updated: ${result.settingsPath}`);
        console.log('💡 Restart Claude Code to activate changes.\n');
      } else {
        console.log(`\n✅ Configuration already correct: ${result.settingsPath}\n`);
      }
    } else {
      console.error('❌ Failed to install hooks:', result.error);
      process.exit(1);
    }
  });

/**
 * Uninstall hooks command
 */
program
  .command('uninstall-hooks')
  .description('Remove Claude Code hooks')
  .action(() => {
    console.log('🗑️  Removing Claude Code hooks...\n');

    const result = uninstallHooks();

    if (result.success) {
      if (result.removed.length > 0) {
        console.log('✅ Removed hooks:');
        result.removed.forEach(h => console.log(`   • ${h}`));
      } else {
        console.log('⚪ No hooks were installed');
      }
      console.log('');
    } else {
      console.error('❌ Failed to remove hooks:', result.error);
      process.exit(1);
    }
  });

/**
 * Hook status command
 */
program
  .command('hooks')
  .description('Show hook installation status')
  .action(() => {
    printHookStatus();
  });

/**
 * Setup command - Interactive configuration wizard
 */
program
  .command('setup')
  .description('Interactive setup wizard for configuring claude-telegram-mirror')
  .action(async () => {
    try {
      await runSetup();
    } catch (error) {
      console.error('Setup failed:', error);
      process.exit(1);
    }
  });

/**
 * Doctor command - Diagnostic tool
 */
program
  .command('doctor')
  .description('Diagnose configuration and connectivity issues')
  .action(async () => {
    try {
      await runDoctor();
    } catch (error) {
      console.error('Doctor failed:', error);
      process.exit(1);
    }
  });

// Helper: Config directory and paths
const configDir = join(homedir(), '.config', 'claude-telegram-mirror');
const pidFile = join(configDir, 'bridge.pid');  // Must match socket.ts DEFAULT_PID_PATH
const socketFile = join(configDir, 'bridge.sock');

/**
 * Check if a process is running by PID
 */
function isProcessRunning(pid: number): boolean {
  try {
    process.kill(pid, 0); // Signal 0 = just check if process exists
    return true;
  } catch {
    return false;
  }
}

/**
 * Wait for a process to exit with timeout
 */
async function waitForProcessExit(pid: number, timeoutMs: number): Promise<boolean> {
  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    if (!isProcessRunning(pid)) {
      return true;
    }
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  return false;
}

/**
 * Stop command - Stop the bridge daemon
 */
program
  .command('stop')
  .description('Stop the bridge daemon')
  .option('--force', 'Force kill if graceful shutdown fails')
  .action(async (options) => {
    // Check if running as OS service first
    if (isServiceInstalled()) {
      const status = getServiceStatus();
      if (status.running) {
        console.log('🔄 Stopping via system service...');
        const result = stopService();
        if (result.success) {
          console.log('✅ ' + result.message);
        } else {
          console.error('❌ ' + result.message);
          process.exit(1);
        }
        return;
      }
    }

    // Direct daemon mode - read PID file
    if (!existsSync(pidFile)) {
      console.log('⚪ Daemon is not running (no PID file)');
      return;
    }

    const pid = parseInt(readFileSync(pidFile, 'utf8').trim());

    // Check if process exists
    if (!isProcessRunning(pid)) {
      console.log('⚪ Daemon is not running (stale PID file), cleaning up...');
      unlinkSync(pidFile);
      if (existsSync(socketFile)) {
        unlinkSync(socketFile);
      }
      return;
    }

    // Send SIGTERM for graceful shutdown
    console.log(`🔄 Stopping daemon (PID ${pid})...`);
    process.kill(pid, 'SIGTERM');

    // Wait for process to exit (5 second timeout)
    const exited = await waitForProcessExit(pid, 5000);

    if (!exited) {
      if (options.force) {
        console.log('⚠️  Graceful shutdown timed out, force killing...');
        try {
          process.kill(pid, 'SIGKILL');
          await waitForProcessExit(pid, 1000);
        } catch {
          // Process may have exited between check and kill
        }
      } else {
        console.log('⚠️  Daemon did not stop within 5 seconds. Use --force to kill it.');
        process.exit(1);
      }
    }

    // Clean up stale files if process is gone
    if (!isProcessRunning(pid)) {
      if (existsSync(pidFile)) {
        unlinkSync(pidFile);
      }
      if (existsSync(socketFile)) {
        unlinkSync(socketFile);
      }
      console.log('✅ Daemon stopped');
    }
  });

/**
 * Restart command - Restart the bridge daemon
 */
program
  .command('restart')
  .description('Restart the bridge daemon')
  .option('-v, --verbose', 'Enable verbose logging')
  .action(async (options) => {
    // Check if running as OS service
    if (isServiceInstalled()) {
      const status = getServiceStatus();
      if (status.running || status.enabled) {
        console.log('🔄 Restarting via system service...');
        const result = restartService();
        if (result.success) {
          console.log('✅ ' + result.message);
        } else {
          console.error('❌ ' + result.message);
          process.exit(1);
        }
        return;
      }
    }

    // Stop existing daemon if running
    if (existsSync(pidFile)) {
      const pid = parseInt(readFileSync(pidFile, 'utf8').trim());
      if (isProcessRunning(pid)) {
        console.log(`🔄 Stopping existing daemon (PID ${pid})...`);
        process.kill(pid, 'SIGTERM');
        await waitForProcessExit(pid, 5000);
      }
      // Clean up
      if (existsSync(pidFile)) unlinkSync(pidFile);
      if (existsSync(socketFile)) unlinkSync(socketFile);
    }

    // Now start fresh
    console.log('🚀 Starting Claude Code Telegram Mirror...\n');

    const config = loadConfig();
    const validation = validateConfig(config);

    if (!validation.valid) {
      console.error('❌ Configuration errors:');
      validation.errors.forEach((err: string) => console.error(`   • ${err}`));
      process.exit(1);
    }

    if (options.verbose) {
      config.verbose = true;
    }

    const daemon = new BridgeDaemon(config);

    const shutdown = async (signal: string) => {
      console.log(`\n📴 Received ${signal}, shutting down...`);
      await daemon.stop();
      process.exit(0);
    };

    process.on('SIGINT', () => shutdown('SIGINT'));
    process.on('SIGTERM', () => shutdown('SIGTERM'));

    try {
      await daemon.start();
      console.log('✅ Bridge daemon running\n');
      console.log('Press Ctrl+C to stop\n');
    } catch (error) {
      console.error('❌ Failed to start daemon:', error);
      process.exit(1);
    }
  });

/**
 * Service management commands
 */
const serviceCmd = program
  .command('service')
  .description('Manage systemd/launchd service');

serviceCmd
  .command('install')
  .description('Install as a system service (systemd on Linux, launchd on macOS)')
  .action(() => {
    console.log('📦 Installing system service...\n');
    const result = installService();
    if (result.success) {
      console.log('✅ ' + result.message);
    } else {
      console.error('❌ ' + result.message);
      process.exit(1);
    }
  });

serviceCmd
  .command('uninstall')
  .description('Uninstall the system service')
  .action(() => {
    console.log('🗑️  Uninstalling system service...\n');
    const result = uninstallService();
    if (result.success) {
      console.log('✅ ' + result.message);
    } else {
      console.error('❌ ' + result.message);
      process.exit(1);
    }
  });

serviceCmd
  .command('start')
  .description('Start the service')
  .action(() => {
    const result = startService();
    if (result.success) {
      console.log('✅ ' + result.message);
    } else {
      console.error('❌ ' + result.message);
      process.exit(1);
    }
  });

serviceCmd
  .command('stop')
  .description('Stop the service')
  .action(() => {
    const result = stopService();
    if (result.success) {
      console.log('✅ ' + result.message);
    } else {
      console.error('❌ ' + result.message);
      process.exit(1);
    }
  });

serviceCmd
  .command('restart')
  .description('Restart the service')
  .action(() => {
    const result = restartService();
    if (result.success) {
      console.log('✅ ' + result.message);
    } else {
      console.error('❌ ' + result.message);
      process.exit(1);
    }
  });

serviceCmd
  .command('status')
  .description('Show service status')
  .action(() => {
    const status = getServiceStatus();
    console.log('\n🔧 Service Status\n');
    console.log(`  Running: ${status.running ? '✅ Yes' : '⚪ No'}`);
    console.log(`  Enabled: ${status.enabled ? '✅ Yes' : '⚪ No'}`);
    console.log(`  Info:    ${status.info}`);
    console.log('');
  });

// Parse arguments
program.parse();
