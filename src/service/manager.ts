/**
 * Service Manager
 * Handles systemd (Linux) and launchd (macOS) service installation
 */

import { existsSync, mkdirSync, writeFileSync, unlinkSync, readFileSync } from 'fs';
import { join, dirname } from 'path';
import { homedir, platform } from 'os';
import { execSync } from 'child_process';

const SERVICE_NAME = 'claude-telegram-mirror';

// Paths
const SYSTEMD_USER_DIR = join(homedir(), '.config', 'systemd', 'user');
const SYSTEMD_SERVICE_FILE = join(SYSTEMD_USER_DIR, `${SERVICE_NAME}.service`);
const LAUNCHD_DIR = join(homedir(), 'Library', 'LaunchAgents');
const LAUNCHD_PLIST = join(LAUNCHD_DIR, `com.claude.${SERVICE_NAME}.plist`);
const ENV_FILE = join(homedir(), '.telegram-env');
const SYSTEMD_ENV_FILE = join(homedir(), '.config', SERVICE_NAME, 'env');

/**
 * Parse environment file and return clean key=value pairs
 * Handles: 'export KEY=value', 'KEY="value"', inline comments
 */
function parseEnvFile(filePath: string): Map<string, string> {
  const vars = new Map<string, string>();
  if (!existsSync(filePath)) return vars;

  const content = readFileSync(filePath, 'utf-8');
  for (const line of content.split('\n')) {
    // Skip empty lines and comments
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;

    // Remove 'export ' prefix
    let cleanLine = trimmed.replace(/^export\s+/, '');

    // Remove inline comments (but not # inside quotes)
    const commentMatch = cleanLine.match(/^([^#"']*(?:"[^"]*"|'[^']*')?[^#]*)#/);
    if (commentMatch) {
      cleanLine = commentMatch[1].trim();
    }

    // Split key=value
    const eqIndex = cleanLine.indexOf('=');
    if (eqIndex === -1) continue;

    const key = cleanLine.substring(0, eqIndex).trim();
    let value = cleanLine.substring(eqIndex + 1).trim();

    // Remove surrounding quotes
    if ((value.startsWith('"') && value.endsWith('"')) ||
        (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1);
    }

    if (key) vars.set(key, value);
  }
  return vars;
}

/**
 * Create systemd-compatible env file (no 'export', no inline comments)
 */
function createSystemdEnvFile(): string {
  const envVars = parseEnvFile(ENV_FILE);
  const configDir = join(homedir(), '.config', SERVICE_NAME);

  // Ensure config directory exists
  if (!existsSync(configDir)) {
    mkdirSync(configDir, { recursive: true, mode: 0o700 });
  }

  // Write systemd-compatible env file
  const lines: string[] = ['# Auto-generated from ~/.telegram-env for systemd'];
  for (const [key, value] of envVars) {
    // Quote values that contain spaces or special characters
    if (value.includes(' ') || value.includes('$') || value.includes('`')) {
      lines.push(`${key}="${value}"`);
    } else {
      lines.push(`${key}=${value}`);
    }
  }

  writeFileSync(SYSTEMD_ENV_FILE, lines.join('\n') + '\n', { mode: 0o600 });
  return SYSTEMD_ENV_FILE;
}

/**
 * Get the path to the Node.js binary
 */
function getNodePath(): string {
  try {
    return execSync('which node', { encoding: 'utf-8' }).trim();
  } catch {
    return '/usr/bin/node';
  }
}

/**
 * Get the package directory (where dist/cli.js lives)
 */
function getPackageDir(): string {
  // This file is in src/service/manager.ts, so package root is two levels up
  return join(dirname(new URL(import.meta.url).pathname), '..', '..');
}

/**
 * Generate systemd service file content
 */
function generateSystemdService(): string {
  const nodeDir = getNodePath();
  const packageDir = getPackageDir();
  const configDir = join(homedir(), '.config', SERVICE_NAME);

  return `[Unit]
Description=Claude Code Telegram Mirror Bridge
Documentation=https://github.com/robertelee78/claude-telegram-mirror
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=${packageDir}
ExecStart=${nodeDir} ${join(packageDir, 'dist', 'cli.js')} start
EnvironmentFile=${SYSTEMD_ENV_FILE}

# Restart policy
Restart=on-failure
RestartSec=10s
StartLimitInterval=300s
StartLimitBurst=5

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=${SERVICE_NAME}

# Security hardening
NoNewPrivileges=true
PrivateTmp=false

# Allow writes to config directory
ReadWritePaths=${configDir}

[Install]
WantedBy=default.target
`;
}

/**
 * Escape a string for XML (plist) content
 */
function escapeXml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&apos;');
}

/**
 * Get the PATH that includes common Node.js install locations
 */
function getMacOSPath(): string {
  const home = homedir();
  const pathDirs = [
    '/usr/local/bin',
    '/usr/bin',
    '/bin',
    '/usr/sbin',
    '/sbin',
    join(home, '.nvm/versions/node') + '/*/bin',  // nvm
    '/opt/homebrew/bin',  // Homebrew on Apple Silicon
    '/usr/local/opt/node/bin',  // Homebrew on Intel
    join(home, '.local/bin'),
  ];

  // Try to get current PATH and merge
  const currentPath = process.env.PATH || '';
  const allPaths = new Set([...pathDirs, ...currentPath.split(':')]);
  return Array.from(allPaths).filter(Boolean).join(':');
}

/**
 * Generate launchd plist content for macOS
 */
function generateLaunchdPlist(): string {
  const nodePath = getNodePath();
  const packageDir = getPackageDir();
  const home = homedir();
  const configDir = join(home, '.config', SERVICE_NAME);
  const logFile = join(configDir, 'daemon.log');
  const errFile = join(configDir, 'daemon.err.log');

  // Use parseEnvFile for proper env parsing (handles export, quotes, comments)
  const envVars = parseEnvFile(ENV_FILE);

  // Build environment variables section
  const envLines: string[] = [];

  // Always include essential environment variables
  envLines.push(`        <key>HOME</key>`);
  envLines.push(`        <string>${escapeXml(home)}</string>`);
  envLines.push(`        <key>PATH</key>`);
  envLines.push(`        <string>${escapeXml(getMacOSPath())}</string>`);
  envLines.push(`        <key>NODE_ENV</key>`);
  envLines.push(`        <string>production</string>`);

  // Add user-defined environment variables from ~/.telegram-env
  for (const [key, value] of envVars) {
    // Skip if we already set it above
    if (['HOME', 'PATH', 'NODE_ENV'].includes(key)) continue;
    envLines.push(`        <key>${escapeXml(key)}</key>`);
    envLines.push(`        <string>${escapeXml(value)}</string>`);
  }

  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.claude.${SERVICE_NAME}</string>

    <key>ProgramArguments</key>
    <array>
        <string>${escapeXml(nodePath)}</string>
        <string>${escapeXml(join(packageDir, 'dist', 'cli.js'))}</string>
        <string>start</string>
    </array>

    <key>WorkingDirectory</key>
    <string>${escapeXml(packageDir)}</string>

    <key>EnvironmentVariables</key>
    <dict>
${envLines.join('\n')}
    </dict>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
        <key>Crashed</key>
        <true/>
    </dict>

    <key>ThrottleInterval</key>
    <integer>10</integer>

    <key>StandardOutPath</key>
    <string>${escapeXml(logFile)}</string>

    <key>StandardErrorPath</key>
    <string>${escapeXml(errFile)}</string>
</dict>
</plist>
`;
}

/**
 * Check if running on Linux with systemd
 */
function hasSystemd(): boolean {
  if (platform() !== 'linux') return false;
  try {
    execSync('systemctl --version', { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

/**
 * Check if running on macOS
 */
function isMacOS(): boolean {
  return platform() === 'darwin';
}

/**
 * Install the service
 */
export function installService(): { success: boolean; message: string } {
  // Check for environment file
  if (!existsSync(ENV_FILE)) {
    return {
      success: false,
      message: `Environment file not found: ${ENV_FILE}\n\nCreate it with:\ncat > ${ENV_FILE} << 'EOF'\nTELEGRAM_BOT_TOKEN=your-token-here\nTELEGRAM_CHAT_ID=your-chat-id\nTELEGRAM_MIRROR=true\nEOF`
    };
  }

  if (hasSystemd()) {
    return installSystemdService();
  } else if (isMacOS()) {
    return installLaunchdService();
  } else {
    return {
      success: false,
      message: 'Unsupported platform. Only Linux (systemd) and macOS (launchd) are supported.'
    };
  }
}

/**
 * Install systemd user service (Linux)
 */
function installSystemdService(): { success: boolean; message: string } {
  try {
    // Create systemd-compatible env file from ~/.telegram-env
    const envFile = createSystemdEnvFile();

    // Create systemd user directory
    if (!existsSync(SYSTEMD_USER_DIR)) {
      mkdirSync(SYSTEMD_USER_DIR, { recursive: true });
    }

    // Write service file
    const serviceContent = generateSystemdService();
    writeFileSync(SYSTEMD_SERVICE_FILE, serviceContent);

    console.log(`  Created env file: ${envFile}`);

    // Reload systemd
    execSync('systemctl --user daemon-reload', { stdio: 'inherit' });

    // Enable service
    execSync(`systemctl --user enable ${SERVICE_NAME}.service`, { stdio: 'inherit' });

    return {
      success: true,
      message: `Service installed: ${SYSTEMD_SERVICE_FILE}

Commands:
  Start:   systemctl --user start ${SERVICE_NAME}
  Stop:    systemctl --user stop ${SERVICE_NAME}
  Status:  systemctl --user status ${SERVICE_NAME}
  Logs:    journalctl --user -u ${SERVICE_NAME} -f

To run without being logged in:
  sudo loginctl enable-linger $USER`
    };
  } catch (error) {
    return {
      success: false,
      message: `Failed to install systemd service: ${error}`
    };
  }
}

/**
 * Install launchd service (macOS)
 */
function installLaunchdService(): { success: boolean; message: string } {
  try {
    // Create LaunchAgents directory
    if (!existsSync(LAUNCHD_DIR)) {
      mkdirSync(LAUNCHD_DIR, { recursive: true });
    }

    // Ensure config directory exists for logs
    const configDir = join(homedir(), '.config', SERVICE_NAME);
    if (!existsSync(configDir)) {
      mkdirSync(configDir, { recursive: true });
    }

    // Write plist file
    const plistContent = generateLaunchdPlist();
    writeFileSync(LAUNCHD_PLIST, plistContent);

    return {
      success: true,
      message: `Service installed: ${LAUNCHD_PLIST}

Commands:
  Load & Start:  launchctl load ${LAUNCHD_PLIST}
  Start:         launchctl start com.claude.${SERVICE_NAME}
  Stop:          launchctl stop com.claude.${SERVICE_NAME}
  Unload:        launchctl unload ${LAUNCHD_PLIST}
  Logs:          tail -f ~/.config/${SERVICE_NAME}/daemon.log`
    };
  } catch (error) {
    return {
      success: false,
      message: `Failed to install launchd service: ${error}`
    };
  }
}

/**
 * Uninstall the service
 */
export function uninstallService(): { success: boolean; message: string } {
  if (hasSystemd()) {
    return uninstallSystemdService();
  } else if (isMacOS()) {
    return uninstallLaunchdService();
  } else {
    return {
      success: false,
      message: 'Unsupported platform.'
    };
  }
}

/**
 * Uninstall systemd service
 */
function uninstallSystemdService(): { success: boolean; message: string } {
  try {
    // Stop and disable service
    try {
      execSync(`systemctl --user stop ${SERVICE_NAME}.service`, { stdio: 'ignore' });
      execSync(`systemctl --user disable ${SERVICE_NAME}.service`, { stdio: 'ignore' });
    } catch {
      // Service might not be running/enabled
    }

    // Remove service file
    if (existsSync(SYSTEMD_SERVICE_FILE)) {
      unlinkSync(SYSTEMD_SERVICE_FILE);
    }

    // Reload systemd
    execSync('systemctl --user daemon-reload', { stdio: 'ignore' });

    return {
      success: true,
      message: 'Service uninstalled successfully.'
    };
  } catch (error) {
    return {
      success: false,
      message: `Failed to uninstall service: ${error}`
    };
  }
}

/**
 * Uninstall launchd service
 */
function uninstallLaunchdService(): { success: boolean; message: string } {
  try {
    // Unload service
    try {
      execSync(`launchctl unload ${LAUNCHD_PLIST}`, { stdio: 'ignore' });
    } catch {
      // Service might not be loaded
    }

    // Remove plist file
    if (existsSync(LAUNCHD_PLIST)) {
      unlinkSync(LAUNCHD_PLIST);
    }

    return {
      success: true,
      message: 'Service uninstalled successfully.'
    };
  } catch (error) {
    return {
      success: false,
      message: `Failed to uninstall service: ${error}`
    };
  }
}

/**
 * Get service status
 */
export function getServiceStatus(): { running: boolean; enabled: boolean; info: string } {
  if (hasSystemd()) {
    return getSystemdStatus();
  } else if (isMacOS()) {
    return getLaunchdStatus();
  } else {
    return {
      running: false,
      enabled: false,
      info: 'Unsupported platform'
    };
  }
}

/**
 * Get systemd service status
 */
function getSystemdStatus(): { running: boolean; enabled: boolean; info: string } {
  let running = false;
  let enabled = false;
  let info = '';

  try {
    const isActive = execSync(`systemctl --user is-active ${SERVICE_NAME}.service 2>/dev/null || true`, { encoding: 'utf-8' }).trim();
    running = isActive === 'active';
  } catch {
    running = false;
  }

  try {
    const isEnabled = execSync(`systemctl --user is-enabled ${SERVICE_NAME}.service 2>/dev/null || true`, { encoding: 'utf-8' }).trim();
    enabled = isEnabled === 'enabled';
  } catch {
    enabled = false;
  }

  if (!existsSync(SYSTEMD_SERVICE_FILE)) {
    info = 'Service not installed';
  } else {
    info = `Service file: ${SYSTEMD_SERVICE_FILE}`;
  }

  return { running, enabled, info };
}

/**
 * Get launchd service status
 */
function getLaunchdStatus(): { running: boolean; enabled: boolean; info: string } {
  let running = false;
  const enabled = existsSync(LAUNCHD_PLIST);
  let info = '';

  try {
    const list = execSync('launchctl list 2>/dev/null || true', { encoding: 'utf-8' });
    running = list.includes(`com.claude.${SERVICE_NAME}`);
  } catch {
    running = false;
  }

  if (!enabled) {
    info = 'Service not installed';
  } else {
    info = `Plist file: ${LAUNCHD_PLIST}`;
  }

  return { running, enabled, info };
}

/**
 * Start the service
 */
export function startService(): { success: boolean; message: string } {
  if (hasSystemd()) {
    try {
      execSync(`systemctl --user start ${SERVICE_NAME}.service`, { stdio: 'inherit' });
      return { success: true, message: 'Service started.' };
    } catch (error) {
      return { success: false, message: `Failed to start: ${error}` };
    }
  } else if (isMacOS()) {
    try {
      // Load if not loaded, then start
      try {
        execSync(`launchctl load ${LAUNCHD_PLIST}`, { stdio: 'ignore' });
      } catch {
        // Already loaded
      }
      execSync(`launchctl start com.claude.${SERVICE_NAME}`, { stdio: 'inherit' });
      return { success: true, message: 'Service started.' };
    } catch (error) {
      return { success: false, message: `Failed to start: ${error}` };
    }
  }
  return { success: false, message: 'Unsupported platform.' };
}

/**
 * Stop the service
 */
export function stopService(): { success: boolean; message: string } {
  if (hasSystemd()) {
    try {
      execSync(`systemctl --user stop ${SERVICE_NAME}.service`, { stdio: 'inherit' });
      return { success: true, message: 'Service stopped.' };
    } catch (error) {
      return { success: false, message: `Failed to stop: ${error}` };
    }
  } else if (isMacOS()) {
    try {
      execSync(`launchctl stop com.claude.${SERVICE_NAME}`, { stdio: 'inherit' });
      return { success: true, message: 'Service stopped.' };
    } catch (error) {
      return { success: false, message: `Failed to stop: ${error}` };
    }
  }
  return { success: false, message: 'Unsupported platform.' };
}

/**
 * Restart the service
 */
export function restartService(): { success: boolean; message: string } {
  if (hasSystemd()) {
    try {
      execSync(`systemctl --user restart ${SERVICE_NAME}.service`, { stdio: 'inherit' });
      return { success: true, message: 'Service restarted.' };
    } catch (error) {
      return { success: false, message: `Failed to restart: ${error}` };
    }
  } else if (isMacOS()) {
    const stopResult = stopService();
    if (!stopResult.success) return stopResult;
    return startService();
  }
  return { success: false, message: 'Unsupported platform.' };
}

/**
 * Check if service is installed (service file exists)
 */
export function isServiceInstalled(): boolean {
  if (hasSystemd()) {
    return existsSync(SYSTEMD_SERVICE_FILE);
  } else if (isMacOS()) {
    return existsSync(LAUNCHD_PLIST);
  }
  return false;
}
