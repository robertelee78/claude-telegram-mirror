#!/usr/bin/env node
/**
 * Post-install script for claude-telegram-mirror
 * Shows helpful guidance after npm install
 */

const os = require('os');
const path = require('path');
const fs = require('fs');

// Try to load chalk, fall back to plain text if not available
let chalk;
try {
  chalk = require('chalk');
} catch (e) {
  // Chalk not available, use plain text functions
  const identity = (s) => s;
  chalk = {
    cyan: identity,
    green: identity,
    yellow: identity,
    red: identity,
    blue: identity,
    gray: identity,
    white: { bold: identity },
  };
}

// Don't show guidance during CI or if TELEGRAM_BOT_TOKEN is already set
if (process.env.CI || process.env.TELEGRAM_BOT_TOKEN) {
  process.exit(0);
}

console.log('');
console.log(chalk.cyan('╔════════════════════════════════════════════════════════════╗'));
console.log(chalk.cyan('║') + chalk.white.bold('  Claude Telegram Mirror - Installation Complete!           ') + chalk.cyan('║'));
console.log(chalk.cyan('╚════════════════════════════════════════════════════════════╝'));
console.log('');

console.log(chalk.yellow('📱 Quick Setup:'));
console.log('');
console.log('   ' + chalk.white.bold('1. Run the interactive setup:'));
console.log('      ' + chalk.green('ctm setup'));
console.log('');
console.log('   ' + chalk.white.bold('2. Or configure manually:'));
console.log('      ' + chalk.gray('export TELEGRAM_BOT_TOKEN="your-bot-token"'));
console.log('      ' + chalk.gray('export TELEGRAM_CHAT_ID="your-chat-id"'));
console.log('      ' + chalk.gray('export TELEGRAM_MIRROR=true'));
console.log('');

console.log(chalk.yellow('🔧 Commands:'));
console.log('');
console.log('   ' + chalk.green('ctm setup') + '           Interactive configuration wizard');
console.log('   ' + chalk.green('ctm doctor') + '          Diagnose configuration issues');
console.log('   ' + chalk.green('ctm start') + '           Start the mirror daemon');
console.log('   ' + chalk.green('ctm status') + '          Show current status');
console.log('   ' + chalk.green('ctm install-hooks') + '   Install Claude Code hooks');
console.log('   ' + chalk.green('ctm service install') + ' Install as system service');
console.log('');

console.log(chalk.yellow('📚 Documentation:'));
console.log('   ' + chalk.blue('https://github.com/robertelee78/claude-mobile'));
console.log('');

// Detect native Rust binary (default entry point)
try {
  const { resolveBinary, getPlatformPackageName } = require('./scripts/resolve-binary.cjs');
  const result = resolveBinary();
  if (result) {
    console.log(chalk.green('✓ Using native binary (fast mode):'));
    console.log('   ' + chalk.gray(result.binary));
    console.log('');
  } else {
    const pkg = getPlatformPackageName();
    console.log(chalk.yellow('⚠ Native binary not available for this platform.'));
    console.log('   ' + chalk.gray('Using TypeScript fallback.'));
    if (pkg) {
      console.log('   ' + chalk.gray('Install the native binary: npm install ' + pkg));
    }
    console.log('');
  }
} catch (e) {
  // resolve-binary.cjs not available or errored — skip silently
}

// Check for existing config
const configDir = path.join(os.homedir(), '.config', 'claude-telegram-mirror');
const configFile = path.join(configDir, 'config.json');

if (fs.existsSync(configFile)) {
  console.log(chalk.green('✓ Existing configuration found at:'));
  console.log('   ' + chalk.gray(configFile));
  console.log('');
}

console.log(chalk.gray('Run "ctm doctor" to verify your setup.'));
console.log('');
