#!/usr/bin/env node
'use strict';

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

// Try native Rust binary first
let binary = null;
let verifyBinaryIntegrity = null;
try {
  const resolveMod = require('./resolve-binary.cjs');
  const result = resolveMod.resolveBinary();
  if (result) binary = result.binary;
  verifyBinaryIntegrity = resolveMod.verifyBinaryIntegrity;
} catch {
  // resolve-binary not available or failed — fall through
}

// Local development fallback: check for a local Rust build
if (!binary) {
  const localBuild = path.resolve(__dirname, '..', 'rust-crates', 'target', 'release', 'ctm');
  if (fs.existsSync(localBuild)) {
    try {
      fs.accessSync(localBuild, fs.constants.X_OK);
      binary = localBuild;
    } catch {
      // Not executable
    }
  }
}

if (!binary) {
  console.error('Error: No native ctm binary found for this platform.');
  console.error(`Platform: ${process.platform}-${process.arch}`);
  console.error('');
  console.error('Supported platforms:');
  console.error('  - Linux x64');
  console.error('  - Linux arm64');
  console.error('  - macOS ARM64 (Apple Silicon)');
  console.error('  - macOS x64 (Intel)');
  console.error('');
  console.error('To build from source: cd rust-crates && cargo build --release');
  process.exit(1);
}

// Verify binary integrity before execution (ADR-006 L3.8)
if (verifyBinaryIntegrity) {
  if (!verifyBinaryIntegrity(binary)) {
    console.error('Error: Binary integrity verification failed. Aborting.');
    process.exit(1);
  }
}

// Spawn Rust binary with all CLI args forwarded
const child = spawn(binary, process.argv.slice(2), {
  stdio: 'inherit',
  env: process.env,
});

// Forward signals to child process
['SIGINT', 'SIGTERM', 'SIGHUP'].forEach(function (sig) {
  process.on(sig, function () {
    child.kill(sig);
  });
});

// Propagate exit code or signal from child
child.on('exit', function (code, signal) {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code || 0);
  }
});

child.on('error', function (err) {
  console.error('Failed to start:', err.message);
  process.exit(1);
});
