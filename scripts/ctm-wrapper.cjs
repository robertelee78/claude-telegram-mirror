#!/usr/bin/env node
'use strict';

const { spawn } = require('child_process');
const path = require('path');

const fs = require('fs');

// Try native Rust binary first
let binary = null;
try {
  const { resolveBinary } = require('./resolve-binary.cjs');
  const result = resolveBinary();
  if (result) binary = result.binary;
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

let child;

if (binary) {
  // Spawn Rust binary with all CLI args forwarded
  child = spawn(binary, process.argv.slice(2), {
    stdio: 'inherit',
    env: process.env,
  });
} else {
  // Fall back to TypeScript CLI via node
  const tsCli = path.resolve(__dirname, '..', 'dist', 'cli.js');
  child = spawn(process.execPath, [tsCli, ...process.argv.slice(2)], {
    stdio: 'inherit',
    env: process.env,
  });
}

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
