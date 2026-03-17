#!/usr/bin/env node
/**
 * Post-install script for claude-telegram-mirror
 * Shows helpful guidance after npm install
 */

const os = require('os');
const path = require('path');
const fs = require('fs');

// Don't show guidance during CI or if TELEGRAM_BOT_TOKEN is already set
if (process.env.CI || process.env.TELEGRAM_BOT_TOKEN) {
  process.exit(0);
}

console.log('');
console.log('+------------------------------------------------------------+');
console.log('|  Claude Telegram Mirror - Installation Complete!           |');
console.log('+------------------------------------------------------------+');
console.log('');

console.log('Quick Setup:');
console.log('');
console.log('   1. Run the interactive setup:');
console.log('      ctm setup');
console.log('');
console.log('   2. Or configure manually:');
console.log('      export TELEGRAM_BOT_TOKEN="your-bot-token"');
console.log('      export TELEGRAM_CHAT_ID="your-chat-id"');
console.log('      export TELEGRAM_MIRROR=true');
console.log('');

console.log('Commands:');
console.log('');
console.log('   ctm setup           Interactive configuration wizard');
console.log('   ctm doctor          Diagnose configuration issues');
console.log('   ctm start           Start the mirror daemon');
console.log('   ctm status          Show current status');
console.log('   ctm install-hooks   Install Claude Code hooks');
console.log('   ctm service install Install as system service');
console.log('');

console.log('Documentation:');
console.log('   https://github.com/robertelee78/claude-mobile');
console.log('');

// Detect native Rust binary
try {
  const { resolveBinary, getPlatformPackageName } = require('./scripts/resolve-binary.cjs');
  const result = resolveBinary();
  if (result) {
    console.log('Native binary found:');
    console.log('   ' + result.binary);
    console.log('');
  } else {
    const pkg = getPlatformPackageName();
    console.log('WARNING: Native binary not available for this platform.');
    console.log('   Platform: ' + process.platform + '-' + process.arch);
    if (pkg) {
      console.log('   Install the native binary: npm install ' + pkg);
    }
    console.log('   Or build from source: cd rust-crates && cargo build --release');
    console.log('');
  }
} catch (e) {
  // resolve-binary.cjs not available or errored — skip silently
}

// Check for existing config
const configDir = path.join(os.homedir(), '.config', 'claude-telegram-mirror');
const configFile = path.join(configDir, 'config.json');

if (fs.existsSync(configFile)) {
  console.log('Existing configuration found at:');
  console.log('   ' + configFile);
  console.log('');
}

console.log('Run "ctm doctor" to verify your setup.');
console.log('');
