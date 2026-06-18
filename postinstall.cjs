#!/usr/bin/env node
/**
 * Post-install script for claude-telegram-mirror
 * Shows helpful guidance after npm install
 */

const os = require('os');
const path = require('path');
const fs = require('fs');
const { execFileSync } = require('child_process');

/**
 * macOS hardening: a binary delivered via npm carries only the ad-hoc signature
 * the build toolchain applied — it is NOT Developer-ID signed or notarized. On
 * modern macOS the kernel can SIGKILL such a binary for a code-signing /
 * launch-constraint violation (EXC_CRASH / "Code Signature Invalid"),
 * especially on the first launchd-spawned launch after install. This pass is
 * defense-in-depth: strip any quarantine flag, ensure the binary has a coherent
 * (at least ad-hoc) signature, and smoke-test that it can actually exec. It
 * never throws — install must still succeed — but it warns loudly so the user
 * is not left with a binary the OS silently refuses to run.
 */
function hardenMacBinary(binary) {
  if (process.platform !== 'darwin') return;

  const run = (cmd, args) => {
    try {
      return { ok: true, out: execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }) };
    } catch (e) {
      return { ok: false, out: (e.stdout || '') + (e.stderr || ''), err: e };
    }
  };

  // 1. Remove the quarantine xattr if a download path applied one. Best-effort.
  run('xattr', ['-d', 'com.apple.quarantine', binary]);

  // 2. If the signature is missing/invalid, re-apply an ad-hoc signature so the
  //    Mach-O is at least internally consistent and execable on Apple Silicon.
  const verify = run('codesign', ['--verify', '--strict', binary]);
  if (!verify.ok) {
    const resign = run('codesign', ['--force', '--sign', '-', binary]);
    if (!resign.ok) {
      console.log('WARNING: Could not re-sign the native binary; macOS may refuse to run it.');
      console.log('   Try manually: codesign --force --sign - "' + binary + '"');
    }
  }

  // 3. Smoke-test exec. If the OS kills it here, the user would otherwise only
  //    discover it when the daemon silently fails to start.
  const smoke = run(binary, ['--version']);
  if (!smoke.ok) {
    console.log('');
    console.log('WARNING: The native ctm binary failed to execute on this machine.');
    console.log('   This is typically a macOS code-signing / Gatekeeper rejection of a');
    console.log('   non-notarized binary. To inspect:');
    console.log('     codesign -dvvv "' + binary + '"');
    console.log('     ls ~/Library/Logs/DiagnosticReports/ctm-*.ips');
    console.log('   Or build from source: cd rust-crates && cargo build --release');
    console.log('');
  }
}

// Protect the native binary on every real install — independent of whether we
// also print the setup guidance below. Skipped under CI (the binary is built
// and verified there, not consumed).
if (!process.env.CI) {
  try {
    const { resolveBinary } = require('./scripts/resolve-binary.cjs');
    const r = resolveBinary();
    if (r) hardenMacBinary(r.binary);
  } catch {
    // resolve-binary unavailable — skip silently
  }
}

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
console.log('   https://github.com/robertelee78/claude-telegram-mirror');
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
