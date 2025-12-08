/**
 * Interactive Setup Wizard
 * Guides users through configuring claude-telegram-mirror
 * Matches the comprehensive guidance of install.sh
 */

import { existsSync, mkdirSync, writeFileSync, readFileSync } from 'fs';
import { join } from 'path';
import { homedir, platform } from 'os';
import { createInterface } from 'readline';

const CONFIG_DIR = join(homedir(), '.config', 'claude-telegram-mirror');
const CONFIG_FILE = join(CONFIG_DIR, 'config.json');
const ENV_FILE = join(homedir(), '.telegram-env');

// ANSI color codes
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
 * Parse ~/.telegram-env file for existing configuration
 */
function parseEnvFile(): { botToken?: string; chatId?: string } {
  const result: { botToken?: string; chatId?: string } = {};

  if (!existsSync(ENV_FILE)) {
    return result;
  }

  try {
    const content = readFileSync(ENV_FILE, 'utf-8');
    const lines = content.split('\n');

    for (const line of lines) {
      const match = line.match(/^(?:export\s+)?(\w+)=["']?([^"'\n]*)["']?/);
      if (!match) continue;

      const [, key, value] = match;
      if (key === 'TELEGRAM_BOT_TOKEN' && value) {
        result.botToken = value;
      } else if (key === 'TELEGRAM_CHAT_ID' && value) {
        result.chatId = value;
      }
    }
  } catch {
    // Ignore parse errors
  }

  return result;
}

/**
 * Simple readline prompt
 */
async function prompt(question: string, defaultValue?: string): Promise<string> {
  const rl = createInterface({
    input: process.stdin,
    output: process.stdout,
  });

  return new Promise((resolve) => {
    const defaultHint = defaultValue ? gray(` [${defaultValue}]`) : '';
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
 * Press Enter to continue
 */
async function pressEnter(message: string = 'Press Enter to continue...'): Promise<void> {
  await prompt(gray(message));
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
    const errorMessage = error instanceof Error ? error.message : String(error);
    return { valid: false, error: `Network error - ${errorMessage}` };
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
        text: '🤖 Claude Telegram Mirror - Setup test successful!\n\nIf you see this, your bot configuration is correct.',
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

interface TelegramChat {
  id: number;
  title?: string;
  type: string;
}

/**
 * Auto-detect groups from getUpdates
 */
async function detectGroups(token: string): Promise<TelegramChat[]> {
  try {
    const response = await fetch(`https://api.telegram.org/bot${token}/getUpdates?limit=100`);
    const data = await response.json() as { ok: boolean; result?: Array<{ message?: { chat: TelegramChat } }> };

    if (!data.ok || !data.result) {
      return [];
    }

    const groups = new Map<number, TelegramChat>();
    for (const update of data.result) {
      const chat = update.message?.chat;
      if (chat && (chat.type === 'supergroup' || chat.type === 'group')) {
        groups.set(chat.id, chat);
      }
    }

    return Array.from(groups.values());
  } catch {
    return [];
  }
}

/**
 * Print a boxed warning message
 */
function printBox(lines: string[], color: (s: string) => string = yellow): void {
  const maxLen = Math.max(...lines.map(l => l.length));
  const border = '─'.repeat(maxLen + 2);

  console.log(color(`┌${border}┐`));
  for (const line of lines) {
    const padding = ' '.repeat(maxLen - line.length);
    console.log(color('│') + ` ${line}${padding} ` + color('│'));
  }
  console.log(color(`└${border}┘`));
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

  // Load existing config from multiple sources
  let existingConfig: Record<string, unknown> = {};

  // Check ~/.telegram-env first
  const envConfig = parseEnvFile();
  if (envConfig.botToken || envConfig.chatId) {
    console.log(green('✓') + ' Found existing ~/.telegram-env');
    if (envConfig.botToken) {
      existingConfig.botToken = envConfig.botToken;
    }
    if (envConfig.chatId) {
      existingConfig.chatId = parseInt(envConfig.chatId, 10);
    }
  }

  // Check config.json (takes precedence)
  if (existsSync(CONFIG_FILE)) {
    try {
      const fileConfig = JSON.parse(readFileSync(CONFIG_FILE, 'utf-8'));
      existingConfig = { ...existingConfig, ...fileConfig };
      console.log(green('✓') + ' Found existing config.json');
    } catch {
      // Ignore parse errors
    }
  }

  // Also check environment variables (highest precedence)
  if (process.env.TELEGRAM_BOT_TOKEN) {
    existingConfig.botToken = process.env.TELEGRAM_BOT_TOKEN;
    console.log(green('✓') + ' Found TELEGRAM_BOT_TOKEN in environment');
  }
  if (process.env.TELEGRAM_CHAT_ID) {
    existingConfig.chatId = parseInt(process.env.TELEGRAM_CHAT_ID, 10);
    console.log(green('✓') + ' Found TELEGRAM_CHAT_ID in environment');
  }

  if (Object.keys(existingConfig).length > 0) {
    console.log('');
  }

  // ============================================================
  // STEP 1: Bot Token
  // ============================================================
  console.log(yellow('STEP 1: CREATE TELEGRAM BOT'));
  console.log(gray('─'.repeat(60)));
  console.log('');
  console.log('You need to create a Telegram bot via @BotFather.');
  console.log('');
  console.log('  1. Open Telegram and search for ' + cyan('@BotFather'));
  console.log('  2. Send ' + cyan('/newbot'));
  console.log("  3. Choose a name (e.g., 'Claude Mirror')");
  console.log("  4. Choose a username (must end in 'bot', e.g., 'claude_mirror_bot')");
  console.log('  5. Copy the API token provided');
  console.log('');

  let botToken = '';
  let botUsername = '';

  while (!botToken) {
    const token = await prompt('Enter your bot token', existingConfig.botToken as string);

    if (!token) {
      console.log(red('✗ Token cannot be empty'));
      continue;
    }

    // Basic format validation
    if (!token.includes(':')) {
      console.log(red('✗ Token format looks incorrect. Expected format: 123456789:ABCdefGHI...'));
      continue;
    }

    process.stdout.write(gray('Verifying token with Telegram... '));
    const result = await testBotToken(token);

    if (result.valid) {
      console.log(green('✓ Valid'));
      console.log(green('✓') + ` Bot verified: @${result.username}`);
      botToken = token;
      botUsername = result.username || '';
    } else {
      console.log(red('✗ Invalid'));
      console.log(red(`  Error: ${result.error}`));

      const retry = await confirm('Try again?', true);
      if (!retry) {
        console.log(red('Setup cancelled.'));
        process.exit(1);
      }
    }
  }

  console.log('');

  // ============================================================
  // STEP 2: Disable Privacy Mode
  // ============================================================
  console.log(yellow('STEP 2: DISABLE PRIVACY MODE'));
  console.log(gray('─'.repeat(60)));
  console.log('');
  console.log('Your bot needs to see all group messages (not just commands).');
  console.log('');
  console.log('  1. Go back to ' + cyan('@BotFather') + ' in Telegram');
  console.log('  2. Send ' + cyan('/mybots'));
  console.log(`  3. Select @${botUsername}`);
  console.log("  4. Click '" + cyan('Bot Settings') + "'");
  console.log("  5. Click '" + cyan('Group Privacy') + "'");
  console.log("  6. Click '" + cyan('Turn off') + "'");
  console.log('');
  console.log(gray(`You should see: "Privacy mode is disabled for @${botUsername}"`));
  console.log('');

  let privacyDone = false;
  while (!privacyDone) {
    privacyDone = await confirm('Have you disabled privacy mode?', false);

    if (!privacyDone) {
      console.log('');
      console.log(yellow('⚠ Privacy mode MUST be disabled for the bot to work in groups.'));
      console.log(gray('  Please complete this step before continuing.'));
      console.log('');

      const retry = await confirm('Try again?', true);
      if (!retry) {
        console.log(red('Setup cancelled.'));
        process.exit(1);
      }
    }
  }

  console.log(green('✓') + ' Privacy mode configured');
  console.log('');

  // ============================================================
  // STEP 3: Setup Supergroup with Topics
  // ============================================================
  console.log(yellow('STEP 3: SETUP SUPERGROUP WITH TOPICS'));
  console.log(gray('─'.repeat(60)));
  console.log('');
  console.log(bold('Option A: Use an existing supergroup'));
  console.log(`  1. Add @${botUsername} to your existing supergroup`);
  console.log("  2. Make the bot an admin with '" + cyan('Manage Topics') + "' permission");
  console.log('  3. Send any message in the group (so we can detect it)');
  console.log('');
  console.log(bold('Option B: Create a new group'));
  console.log('  1. In Telegram, create a new group');
  console.log(`  2. Add @${botUsername} to the group`);
  console.log("  3. Go to group settings → Enable '" + cyan('Topics') + "'");
  console.log('     ' + gray('(This converts it to a supergroup)'));
  console.log("  4. Make the bot an admin with '" + cyan('Manage Topics') + "' permission");
  console.log('  5. Send any message in the group');
  console.log('');

  await pressEnter('Press Enter when you have completed these steps...');
  console.log('');

  let chatId = '';

  // Try to auto-detect groups
  process.stdout.write(gray('Looking for your group... '));
  const groups = await detectGroups(botToken);

  if (groups.length === 1) {
    // Single group found
    console.log(green('✓ Found'));
    console.log('');
    console.log(green('✓') + ` Found group: ${bold(groups[0].title || 'Unnamed')} (${groups[0].id})`);

    const useThis = await confirm('Is this the correct group?', true);
    if (useThis) {
      chatId = groups[0].id.toString();
    }
  } else if (groups.length > 1) {
    // Multiple groups found
    console.log(green('✓ Found'));
    console.log('');
    console.log('Found multiple groups:');
    console.log('');

    groups.forEach((g, i) => {
      console.log(`  ${i + 1}) ${g.title || 'Unnamed'} (${g.id})`);
    });
    console.log(`  ${groups.length + 1}) Enter manually`);
    console.log('');

    while (!chatId) {
      const selection = await prompt(`Select group number (1-${groups.length + 1})`);
      const num = parseInt(selection, 10);

      if (num >= 1 && num <= groups.length) {
        chatId = groups[num - 1].id.toString();
        console.log(green('✓') + ` Selected: ${groups[num - 1].title || 'Unnamed'}`);
      } else if (num === groups.length + 1) {
        break; // Will fall through to manual entry
      } else {
        console.log(yellow('⚠ Invalid selection'));
      }
    }
  } else {
    // No groups found
    console.log(yellow('not found'));
    console.log('');
    console.log(yellow('⚠ No supergroups found. This can happen if:'));
    console.log(gray("  - The bot hasn't seen any messages yet"));
    console.log(gray("  - The group wasn't converted to a supergroup (enable Topics!)"));
    console.log('');
  }

  // Manual entry if needed
  if (!chatId) {
    console.log('');
    console.log('Enter the chat ID manually.');
    console.log('You can find it by:');
    console.log('  1. Send a message in the group');
    console.log(`  2. Visit: ${cyan(`https://api.telegram.org/bot${botToken}/getUpdates`)}`);
    console.log("  3. Look for " + cyan('"chat":{"id": -100XXXXXXXXXX}'));
    console.log('');

    while (!chatId) {
      const id = await prompt('Enter chat ID (starts with -100)', existingConfig.chatId?.toString());

      if (!id) {
        console.log(red('✗ Chat ID is required'));
        continue;
      }

      if (!id.startsWith('-100') && !id.startsWith('-')) {
        console.log(yellow('⚠ Chat ID should start with -100 (supergroup format)'));
        const proceed = await confirm('Use this value anyway?', false);
        if (!proceed) continue;
      }

      chatId = id;
    }
  }

  console.log('');

  // ============================================================
  // STEP 4: Verify Bot Permissions
  // ============================================================
  console.log(yellow('STEP 4: VERIFY BOT PERMISSIONS'));
  console.log(gray('─'.repeat(60)));
  console.log('');

  let permissionsOk = false;
  while (!permissionsOk) {
    process.stdout.write(gray('Testing if bot can post to the group... '));
    const result = await testChatId(botToken, chatId);

    if (result.valid) {
      console.log(green('✓ Success'));
      console.log('');
      console.log(green('✓') + ' Bot can post to the group!');
      console.log(gray('  Check your Telegram group - you should see a test message.'));
      permissionsOk = true;
    } else {
      console.log(red('✗ Failed'));
      console.log('');
      console.log(red(`✗ Bot cannot post: ${result.error}`));
      console.log('');
      console.log(bold('Common fixes:'));
      console.log(gray('  - Make sure the bot is an admin in the group'));
      console.log(gray("  - Ensure 'Post Messages' permission is enabled"));
      console.log(gray("  - Check that 'Manage Topics' permission is enabled"));
      console.log('');

      await pressEnter('Fix the issue and press Enter to retry, or Ctrl+C to exit...');
    }
  }

  await pressEnter();
  console.log('');

  // ============================================================
  // STEP 5: Configuration Options
  // ============================================================
  console.log(yellow('STEP 5: CONFIGURATION OPTIONS'));
  console.log(gray('─'.repeat(60)));
  console.log('');

  const useThreads = await confirm(
    'Enable forum threads (each session gets its own topic)?',
    (existingConfig.useThreads as boolean) ?? true
  );

  const installHooksChoice = await confirm('Install Claude Code hooks?', true);
  const installServiceChoice = await confirm(
    platform() === 'darwin'
      ? 'Install as launchd service (auto-start on login)?'
      : 'Install as systemd service (auto-start)?',
    true
  );

  console.log('');

  // ============================================================
  // STEP 6: Save Configuration
  // ============================================================
  console.log(yellow('STEP 6: SAVING CONFIGURATION'));
  console.log(gray('─'.repeat(60)));
  console.log('');

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

  // Also create/update ~/.telegram-env
  const envContent = `# Claude Telegram Mirror Configuration
export TELEGRAM_BOT_TOKEN="${botToken}"
export TELEGRAM_CHAT_ID="${chatId}"
export TELEGRAM_MIRROR=true
`;
  writeFileSync(ENV_FILE, envContent);
  console.log(green('✓') + ' Saved environment to ' + gray(ENV_FILE));

  // Suggest sourcing
  console.log('');
  console.log(gray('Add to your shell profile (~/.bashrc or ~/.zshrc):'));
  console.log(cyan('  [[ -f ~/.telegram-env ]] && source ~/.telegram-env'));
  console.log('');

  // ============================================================
  // STEP 7: Install Hooks
  // ============================================================
  if (installHooksChoice) {
    console.log(yellow('STEP 7: INSTALL CLAUDE CODE HOOKS'));
    console.log(gray('─'.repeat(60)));
    console.log('');

    try {
      const { installHooks: doInstallHooks } = await import('../hooks/installer.js');
      const result = doInstallHooks({ force: false });
      if (result.success) {
        console.log(green('✓') + ' Global hooks installed to ~/.claude/settings.json');
      } else {
        console.log(yellow('⚠') + ' Hook installation: ' + result.error);
      }
    } catch (error) {
      console.log(yellow('⚠') + ' Could not install hooks: ' + (error as Error).message);
    }

    console.log('');

    // Project-level hooks warning
    printBox([
      '⚠️  IMPORTANT: PROJECT-LEVEL HOOKS',
      '',
      'If you use Claude Code in projects that have their own',
      '.claude/settings.json file, the GLOBAL hooks we just',
      'installed will be IGNORED in those projects.',
      '',
      'To enable Telegram mirroring in a specific project:',
      '',
      '  cd /path/to/your/project',
      '  ctm install-hooks --project',
      '',
      "This adds hooks to that project's .claude/settings.json",
    ]);
    console.log('');

    const hasProjectHooks = await confirm('Do you have a project with .claude/settings.json that needs hooks?', false);

    if (hasProjectHooks) {
      let addMore = true;
      while (addMore) {
        const projectPath = await prompt("Enter project path (or 'done' to finish)");

        if (projectPath.toLowerCase() === 'done' || !projectPath) {
          break;
        }

        const fullPath = projectPath.startsWith('/') ? projectPath : join(process.cwd(), projectPath);
        const claudeDir = join(fullPath, '.claude');

        if (!existsSync(claudeDir)) {
          console.log(yellow('⚠') + ` No .claude/ directory in ${projectPath}`);
          console.log(gray("  This project doesn't have custom Claude settings."));
          console.log(gray('  Global hooks will work here - no action needed!'));
        } else {
          try {
            const { installHooks: doInstallHooks } = await import('../hooks/installer.js');
            const result = doInstallHooks({ force: false, projectPath: fullPath });
            if (result.success) {
              console.log(green('✓') + ` Hooks installed to ${projectPath}/.claude/settings.json`);
            } else {
              console.log(yellow('⚠') + ' ' + result.error);
            }
          } catch (error) {
            console.log(yellow('⚠') + ' ' + (error as Error).message);
          }
        }

        addMore = await confirm('Add another project?', false);
      }
    }

    console.log('');
  }

  // ============================================================
  // STEP 8: Install Service
  // ============================================================
  if (installServiceChoice) {
    const stepNum = installHooksChoice ? 8 : 7;
    console.log(yellow(`STEP ${stepNum}: INSTALL SYSTEM SERVICE`));
    console.log(gray('─'.repeat(60)));
    console.log('');

    try {
      const { installService: doInstallService, startService } = await import('./manager.js');

      process.stdout.write(gray('Installing service... '));
      const installResult = doInstallService();
      if (installResult.success) {
        console.log(green('✓'));

        process.stdout.write(gray('Starting service... '));
        const startResult = startService();
        if (startResult.success) {
          console.log(green('✓'));
          console.log('');
          console.log(green('✓') + ' Service is running!');
        } else {
          console.log(yellow('⚠'));
          console.log('');
          console.log(yellow('⚠') + ' Service may not have started correctly');
        }
      } else {
        console.log(yellow('⚠'));
        console.log(yellow('⚠') + ' ' + installResult.message);
      }
    } catch (error) {
      console.log(yellow('⚠') + ' Could not install service: ' + (error as Error).message);
    }

    console.log('');

    // Platform-specific troubleshooting
    if (platform() === 'darwin') {
      console.log(gray('Troubleshooting (macOS):'));
      console.log(gray('  Check status: launchctl list | grep claude'));
      console.log(gray('  View logs: cat ~/Library/Logs/claude-telegram-mirror.*.log'));
    } else {
      console.log(gray('Troubleshooting (Linux):'));
      console.log(gray('  Check status: systemctl --user status claude-telegram-mirror'));
      console.log(gray('  View logs: journalctl --user -u claude-telegram-mirror -f'));
      console.log('');
      console.log(gray('You may need to enable user lingering:'));
      console.log(gray('  loginctl enable-linger $USER'));
    }

    console.log('');
  }

  // ============================================================
  // COMPLETION
  // ============================================================
  console.log(cyan('╔════════════════════════════════════════════════════════════╗'));
  console.log(cyan('║') + green('  ✅ INSTALLATION COMPLETE                                   ') + cyan('║'));
  console.log(cyan('╚════════════════════════════════════════════════════════════╝'));
  console.log('');

  console.log(bold('Summary:'));
  console.log(`  Bot:     @${botUsername}`);
  console.log(`  Chat:    ${chatId}`);
  console.log(`  Config:  ${CONFIG_FILE}`);
  console.log(`  Env:     ${ENV_FILE}`);
  console.log('');

  console.log(bold('Commands:'));
  console.log('  ' + cyan('ctm start') + '            Start daemon (foreground)');
  console.log('  ' + cyan('ctm status') + '           Show status');
  console.log('  ' + cyan('ctm service status') + '   Service status');
  console.log('  ' + cyan('ctm doctor') + '           Diagnose issues');
  console.log('');

  // Project hooks reminder
  printBox([
    '📌 REMEMBER: Project-specific hooks',
    '',
    'For projects with .claude/settings.json:',
    '  cd /path/to/project && ctm install-hooks -p',
  ], cyan);
  console.log('');

  console.log(bold('Next steps:'));
  console.log("  1. Run '" + cyan('source ~/.telegram-env') + "' or restart terminal");
  console.log('  2. Start a Claude Code session in tmux:');
  console.log('     ' + cyan('tmux new -s claude'));
  console.log('     ' + cyan('claude'));
  console.log('');
  console.log(green('Your Claude sessions will now be mirrored to Telegram!'));
  console.log('');
}

export default runSetup;
