/**
 * Interactive Setup Wizard
 * Guides users through configuring claude-telegram-mirror
 */

import { existsSync, mkdirSync, writeFileSync, readFileSync } from 'fs';
import { join } from 'path';
import { homedir } from 'os';
import { createInterface } from 'readline';

const CONFIG_DIR = join(homedir(), '.config', 'claude-telegram-mirror');
const CONFIG_FILE = join(CONFIG_DIR, 'config.json');

// ANSI color codes (works in Node.js terminal)
const colors = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  cyan: '\x1b[36m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  red: '\x1b[31m',
  blue: '\x1b[34m',
  gray: '\x1b[90m',
};

function cyan(text: string): string { return `${colors.cyan}${text}${colors.reset}`; }
function green(text: string): string { return `${colors.green}${text}${colors.reset}`; }
function yellow(text: string): string { return `${colors.yellow}${text}${colors.reset}`; }
function red(text: string): string { return `${colors.red}${text}${colors.reset}`; }
function gray(text: string): string { return `${colors.gray}${text}${colors.reset}`; }
function bold(text: string): string { return `${colors.bold}${text}${colors.reset}`; }

/**
 * Simple readline prompt
 */
async function prompt(question: string, defaultValue?: string): Promise<string> {
  const rl = createInterface({
    input: process.stdin,
    output: process.stdout,
  });

  return new Promise((resolve) => {
    const defaultHint = defaultValue ? gray(` (${defaultValue})`) : '';
    rl.question(`${question}${defaultHint}: `, (answer) => {
      rl.close();
      resolve(answer.trim() || defaultValue || '');
    });
  });
}

/**
 * Yes/No prompt
 */
async function confirm(question: string, defaultValue: boolean = true): Promise<boolean> {
  const hint = defaultValue ? '[Y/n]' : '[y/N]';
  const answer = await prompt(`${question} ${gray(hint)}`);

  if (!answer) return defaultValue;
  return answer.toLowerCase().startsWith('y');
}

/**
 * Test Telegram bot token
 */
async function testBotToken(token: string): Promise<{ valid: boolean; username?: string; error?: string }> {
  try {
    const response = await fetch(`https://api.telegram.org/bot${token}/getMe`);
    const data = await response.json() as { ok: boolean; result?: { username: string }; description?: string };

    if (data.ok) {
      return { valid: true, username: data.result?.username };
    }
    return { valid: false, error: data.description || 'Invalid token' };
  } catch (error) {
    return { valid: false, error: 'Network error - check your connection' };
  }
}

/**
 * Test chat ID by sending a message
 */
async function testChatId(token: string, chatId: string): Promise<{ valid: boolean; error?: string }> {
  try {
    const response = await fetch(`https://api.telegram.org/bot${token}/sendMessage`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        chat_id: chatId,
        text: '🧪 Test message from Claude Telegram Mirror setup wizard.\n\nIf you see this, your configuration is correct!',
        parse_mode: 'Markdown'
      })
    });

    const data = await response.json() as { ok: boolean; description?: string };

    if (data.ok) {
      return { valid: true };
    }
    return { valid: false, error: data.description || 'Failed to send message' };
  } catch (error) {
    return { valid: false, error: 'Network error' };
  }
}

/**
 * Run interactive setup wizard
 */
export async function runSetup(): Promise<void> {
  console.log('');
  console.log(cyan('╔════════════════════════════════════════════════════════════╗'));
  console.log(cyan('║') + bold('  Claude Telegram Mirror - Setup Wizard                     ') + cyan('║'));
  console.log(cyan('╚════════════════════════════════════════════════════════════╝'));
  console.log('');

  // Load existing config if present
  let existingConfig: Record<string, unknown> = {};
  if (existsSync(CONFIG_FILE)) {
    try {
      existingConfig = JSON.parse(readFileSync(CONFIG_FILE, 'utf-8'));
      console.log(green('✓') + ' Found existing configuration');
      console.log('');
    } catch {
      // Ignore
    }
  }

  // Step 1: Bot Token
  console.log(yellow('Step 1: Telegram Bot Token'));
  console.log(gray('─'.repeat(50)));
  console.log('');
  console.log('Create a bot via @BotFather on Telegram:');
  console.log('  1. Open Telegram and search for @BotFather');
  console.log('  2. Send /newbot and follow the prompts');
  console.log('  3. Copy the API token provided');
  console.log('');

  let botToken = '';
  let botUsername = '';

  while (!botToken) {
    const token = await prompt('Enter your bot token', existingConfig.botToken as string);

    if (!token) {
      console.log(red('Bot token is required'));
      continue;
    }

    process.stdout.write('Verifying token... ');
    const result = await testBotToken(token);

    if (result.valid) {
      console.log(green('✓ Valid'));
      console.log(`Bot: @${result.username}`);
      botToken = token;
      botUsername = result.username || '';
    } else {
      console.log(red('✗ Invalid'));
      console.log(red(`Error: ${result.error}`));
    }
  }

  console.log('');

  // Step 2: Chat ID
  console.log(yellow('Step 2: Telegram Chat ID'));
  console.log(gray('─'.repeat(50)));
  console.log('');
  console.log('Get your chat ID:');
  console.log(`  1. Message your bot @${botUsername} on Telegram`);
  console.log('  2. Visit: ' + cyan(`https://api.telegram.org/bot${botToken}/getUpdates`));
  console.log('  3. Look for "chat":{"id":XXXXXXXX} in the response');
  console.log('');
  console.log(gray('Tip: For a group/supergroup, the ID starts with -100'));
  console.log('');

  let chatId = '';

  while (!chatId) {
    const id = await prompt('Enter your chat ID', existingConfig.chatId?.toString());

    if (!id) {
      console.log(red('Chat ID is required'));
      continue;
    }

    process.stdout.write('Testing chat access... ');
    const result = await testChatId(botToken, id);

    if (result.valid) {
      console.log(green('✓ Message sent'));
      console.log('Check your Telegram for the test message!');
      chatId = id;
    } else {
      console.log(red('✗ Failed'));
      console.log(red(`Error: ${result.error}`));
      console.log(gray('Make sure you have started a chat with your bot first.'));
    }
  }

  console.log('');

  // Step 3: Options
  console.log(yellow('Step 3: Configuration Options'));
  console.log(gray('─'.repeat(50)));
  console.log('');

  const useThreads = await confirm(
    'Enable forum threads for per-session topics?',
    (existingConfig.useThreads as boolean) ?? true
  );

  const installHooks = await confirm('Install Claude Code hooks?', true);
  const installService = await confirm('Install as systemd service (auto-start)?', true);

  console.log('');

  // Save configuration
  console.log(yellow('Saving configuration...'));
  console.log(gray('─'.repeat(50)));

  // Create config directory
  if (!existsSync(CONFIG_DIR)) {
    mkdirSync(CONFIG_DIR, { recursive: true });
    console.log(green('✓') + ' Created config directory');
  }

  // Save config file
  const config = {
    botToken,
    chatId: parseInt(chatId, 10),
    enabled: true,
    useThreads,
    verbose: true,
    approvals: true,
  };

  writeFileSync(CONFIG_FILE, JSON.stringify(config, null, 2));
  console.log(green('✓') + ' Saved config to ' + gray(CONFIG_FILE));

  // Also suggest environment variables
  console.log('');
  console.log(yellow('Environment variables (add to ~/.bashrc or ~/.zshrc):'));
  console.log('');
  console.log(gray('  export TELEGRAM_BOT_TOKEN="') + botToken + gray('"'));
  console.log(gray('  export TELEGRAM_CHAT_ID="') + chatId + gray('"'));
  console.log(gray('  export TELEGRAM_MIRROR=true'));
  console.log('');

  // Install hooks if requested
  if (installHooks) {
    console.log(yellow('Installing Claude Code hooks...'));
    try {
      // Import dynamically to avoid circular dependencies
      const { installHooks: doInstallHooks } = await import('../hooks/installer.js');
      const result = doInstallHooks({ force: false });
      if (result.success) {
        console.log(green('✓') + ' Hooks installed');
      } else {
        console.log(yellow('⚠') + ' Hook installation: ' + result.error);
      }
    } catch (error) {
      console.log(yellow('⚠') + ' Could not install hooks: ' + (error as Error).message);
    }
  }

  // Install service if requested
  if (installService) {
    console.log(yellow('Installing systemd service...'));
    try {
      const { installService: doInstallService } = await import('./manager.js');
      const result = doInstallService();
      if (result.success) {
        console.log(green('✓') + ' Service installed');
        console.log(gray('  Start with: ctm service start'));
      } else {
        console.log(yellow('⚠') + ' Service installation: ' + result.message);
      }
    } catch (error) {
      console.log(yellow('⚠') + ' Could not install service: ' + (error as Error).message);
    }
  }

  // Final summary
  console.log('');
  console.log(cyan('╔════════════════════════════════════════════════════════════╗'));
  console.log(cyan('║') + green('  Setup Complete!                                          ') + cyan('║'));
  console.log(cyan('╚════════════════════════════════════════════════════════════╝'));
  console.log('');
  console.log('Next steps:');
  console.log('');
  console.log('  1. ' + cyan('ctm start') + '           Start the daemon');
  console.log('  2. ' + cyan('ctm service start') + '   Start via systemd');
  console.log('  3. ' + cyan('ctm doctor') + '          Verify everything works');
  console.log('');
  console.log(gray('Run ctm --help for all available commands.'));
  console.log('');
}

export default runSetup;
