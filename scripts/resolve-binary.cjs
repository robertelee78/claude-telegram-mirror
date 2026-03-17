#!/usr/bin/env node

/**
 * Resolve the native CTM binary path.
 *
 * Searches for the platform-specific @agidreams/ctm-{os}-{arch} package
 * in node_modules, following the same pattern as swictation.
 *
 * Returns the absolute path to the ctm binary, or null if not available
 * (unsupported platform, binary not installed, etc.).
 */

'use strict';

const { execSync } = require('child_process');
const { createHash } = require('crypto');
const path = require('path');
const fs = require('fs');
const os = require('os');

/**
 * Get the platform package name for the current system.
 * @returns {string|null} Package name like "@agidreams/ctm-linux-x64" or null if unsupported.
 */
function getPlatformPackageName() {
  const platform = os.platform();
  const arch = os.arch();

  const packageMap = {
    'linux-x64': '@agidreams/ctm-linux-x64',
    'linux-arm64': '@agidreams/ctm-linux-arm64',
    'darwin-arm64': '@agidreams/ctm-darwin-arm64',
    'darwin-x64': '@agidreams/ctm-darwin-x64',
  };

  const key = `${platform}-${arch}`;
  return packageMap[key] || null;
}

/**
 * Check if a binary path exists and is executable.
 * @param {string} binPath
 * @returns {boolean}
 */
function isExecutable(binPath) {
  try {
    fs.accessSync(binPath, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/**
 * Search for the platform package in a node_modules directory.
 * @param {string} nodeModulesDir
 * @param {string} packageName
 * @returns {string|null} Path to the ctm binary, or null.
 */
function findInNodeModules(nodeModulesDir, packageName) {
  // @agidreams/ctm-linux-x64 -> node_modules/@agidreams/ctm-linux-x64/bin/ctm
  const binPath = path.join(nodeModulesDir, packageName, 'bin', 'ctm');
  if (isExecutable(binPath)) {
    return binPath;
  }
  return null;
}

/**
 * Verify the integrity of a resolved binary against checksums.json.
 *
 * Looks for checksums.json in the same directory as the binary. If found,
 * computes SHA-256 of the binary and compares against the expected hash.
 *
 * @param {string} binaryPath - Absolute path to the resolved binary.
 * @returns {boolean} true if verification passed or was skipped (no checksums.json).
 */
function verifyBinaryIntegrity(binaryPath) {
  const binDir = path.dirname(binaryPath);
  const checksumsPath = path.join(binDir, 'checksums.json');

  if (!fs.existsSync(checksumsPath)) {
    console.warn('Warning: No checksums.json found — skipping integrity check');
    return true;
  }

  let checksums;
  try {
    checksums = JSON.parse(fs.readFileSync(checksumsPath, 'utf8'));
  } catch (err) {
    console.error(`Error: Failed to parse checksums.json: ${err.message}`);
    return false;
  }

  const entry = checksums.ctm;
  if (!entry || !entry.sha256) {
    console.error('Error: checksums.json missing "ctm.sha256" field');
    return false;
  }

  const binaryData = fs.readFileSync(binaryPath);

  // Verify file size if present
  if (typeof entry.size === 'number' && binaryData.length !== entry.size) {
    console.error(
      `Binary integrity check failed: size mismatch (expected ${entry.size}, got ${binaryData.length})`
    );
    return false;
  }

  const actualHash = createHash('sha256').update(binaryData).digest('hex');
  if (actualHash !== entry.sha256) {
    console.error('Binary integrity check failed: SHA-256 mismatch');
    console.error(`  expected: ${entry.sha256}`);
    console.error(`  actual:   ${actualHash}`);
    return false;
  }

  return true;
}

/**
 * Resolve the native CTM binary path using multiple search strategies.
 *
 * Does NOT verify binary integrity. Callers should call
 * verifyBinaryIntegrity() separately after resolution. See ADR-006 L3.8.
 *
 * @returns {{ binary: string, packageDir: string } | null}
 */
function resolveBinary() {
  const packageName = getPlatformPackageName();
  if (!packageName) {
    return null;
  }

  // Strategy 1: npm global root
  try {
    const npmRoot = execSync('npm root -g', { encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] }).trim();
    const binary = findInNodeModules(npmRoot, packageName);
    if (binary) {
      return { binary, packageDir: path.join(npmRoot, packageName) };
    }
  } catch {
    // npm not available or not installed globally
  }

  // Strategy 2: Walk upward from this file to find node_modules
  let dir = __dirname;
  for (let i = 0; i < 10; i++) {
    const nodeModules = path.join(dir, 'node_modules');
    if (fs.existsSync(nodeModules)) {
      const binary = findInNodeModules(nodeModules, packageName);
      if (binary) {
        return { binary, packageDir: path.join(nodeModules, packageName) };
      }
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }

  // Strategy 3: Check NVM_DIR global installs
  const nvmDir = process.env.NVM_DIR;
  if (nvmDir) {
    try {
      const versionDirs = fs.readdirSync(path.join(nvmDir, 'versions', 'node'));
      for (const version of versionDirs) {
        const nodeModules = path.join(nvmDir, 'versions', 'node', version, 'lib', 'node_modules');
        const binary = findInNodeModules(nodeModules, packageName);
        if (binary) {
          return { binary, packageDir: path.join(nodeModules, packageName) };
        }
      }
    } catch {
      // NVM dir structure not as expected
    }
  }

  // Strategy 4: Common global install paths
  const globalPaths = [
    '/usr/lib/node_modules',
    '/usr/local/lib/node_modules',
    path.join(os.homedir(), '.local/lib/node_modules'),
  ];

  for (const globalPath of globalPaths) {
    const binary = findInNodeModules(globalPath, packageName);
    if (binary) {
      return { binary, packageDir: path.join(globalPath, packageName) };
    }
  }

  return null;
}

// If run directly, print the binary path
if (require.main === module) {
  const result = resolveBinary();
  if (result) {
    if (!verifyBinaryIntegrity(result.binary)) {
      process.exit(1);
    }
    console.log(result.binary);
  } else {
    const pkg = getPlatformPackageName();
    console.error(`No native binary found for this platform (${os.platform()}-${os.arch()}).`);
    console.error(`Expected package: ${pkg || 'unsupported platform'}`);
    console.error('Build from source: cd rust-crates && cargo build --release');
    process.exit(1);
  }
}

module.exports = { resolveBinary, getPlatformPackageName, verifyBinaryIntegrity };
