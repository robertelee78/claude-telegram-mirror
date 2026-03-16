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
 * Resolve the native CTM binary path using multiple search strategies.
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
    console.log(result.binary);
  } else {
    const pkg = getPlatformPackageName();
    console.error(`No native binary found for this platform (${os.platform()}-${os.arch()}).`);
    console.error(`Expected package: ${pkg || 'unsupported platform'}`);
    console.error('Falling back to TypeScript implementation.');
    process.exit(1);
  }
}

module.exports = { resolveBinary, getPlatformPackageName };
