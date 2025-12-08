#!/usr/bin/env node
/**
 * Claude Telegram Mirror CLI
 * Main command-line interface
 */

import { Command } from 'commander';
import { readFileSync } from 'fs';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import { BridgeDaemon } from './bridge/daemon.js';
import { installHooks, uninstallHooks, printHookStatus } from './hooks/installer.js';
import { loadConfig, validateConfig } from './utils/config.js';
import {
  installService,
  uninstallService,
  getServiceStatus,
  startService,
  stopService,
  restartService
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

    console.log('\n📊 Claude Telegram Mirror Status\n');

    // Check configuration
    console.log('Configuration:');
    console.log(`  Bot Token: ${config.botToken ? '✅ Set' : '❌ Not set'}`);
    console.log(`  Chat ID: ${config.chatId ? `✅ ${config.chatId}` : '❌ Not set'}`);
    console.log(`  Enabled: ${config.enabled ? '✅ Yes' : '⚪ No'}`);
    console.log(`  Verbose: ${config.verbose ? '✅ Yes' : '⚪ No'}`);
    console.log(`  Socket: ${config.socketPath}`);
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
  .option('-f, --force', 'Force reinstall hooks')
  .option('-p, --project', 'Install to current project\'s .claude/settings.json (run from project directory)')
  .action((options) => {
    if (options.project) {
      console.log(`📌 Installing hooks to project: ${process.cwd()}\n`);
    } else {
      console.log('📌 Installing Claude Code hooks (global)...\n');
    }

    const result = installHooks({ force: options.force, project: options.project });

    if (result.success) {
      if (result.installed.length > 0) {
        console.log('✅ Installed hooks:');
        result.installed.forEach(h => console.log(`   • ${h}`));
      }
      if (result.skipped.length > 0) {
        console.log('\n⚪ Already installed:');
        result.skipped.forEach(h => console.log(`   • ${h}`));
      }
      console.log(`\n✅ Hooks installed to: ${result.settingsPath}\n`);

      if (options.project) {
        console.log('💡 Restart Claude Code in this project to activate hooks.\n');
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
