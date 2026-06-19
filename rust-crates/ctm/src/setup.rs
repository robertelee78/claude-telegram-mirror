//! Interactive Setup Wizard — 8-step guided configuration.
//!
//! Ported from `src/service/setup.ts`.
//! Uses the `dialoguer` crate for terminal prompts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use dialoguer::{Confirm, Input, Select};

use crate::colors::{bold, cyan, gray, green, red, yellow};
use crate::config;

/// ADR-014 D1: The trust-model notice shown (and recorded as accepted) during a
/// new setup. The chat-level model is intentional — anyone with channel access can
/// drive the shell and approve tool calls, so the channel must be treated like a
/// shared shell. Mirrored in the README. Per project terminology, any future
/// trusted-user list is a whitelist (and its inverse a blacklist).
pub const TRUST_NOTICE: &str =
    "Anyone you add to this Telegram channel can drive your shell and approve \
tool calls. Treat the channel like a shared shell: only add people you would \
already trust with git-commit access. Semi-trusted or public channels are not \
supported.";

fn separator() {
    println!("{}", gray(&"-".repeat(60)));
}

/// L6.7: Print a text block inside a Unicode box-drawing frame.
///
/// Each line of `text` (split on `\n`) is padded to `width` characters and
/// wrapped in vertical box-drawing characters.  The box is 52 columns wide by
/// default (inner width 50).
pub fn print_box(text: &str) {
    let inner_width = 50;
    let top = format!("\u{250C}{}\u{2510}", "\u{2500}".repeat(inner_width));
    let bottom = format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(inner_width));

    println!("{top}");
    for line in text.lines() {
        let display_len = line.chars().count();
        let padding = inner_width.saturating_sub(display_len);
        println!("\u{2502}{line}{}\u{2502}", " ".repeat(padding));
    }
    println!("{bottom}");
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn home_dir() -> PathBuf {
    config::home_dir()
}

fn config_dir() -> PathBuf {
    config::get_config_dir()
}

fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

fn env_file() -> PathBuf {
    home_dir().join(".telegram-env")
}

// ---------------------------------------------------------------------------
// Telegram API helpers
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(serde::Deserialize)]
struct BotUser {
    /// The bot's own user id, required to query getChatMember for itself.
    id: i64,
    username: Option<String>,
}

/// Subset of Telegram's ChatMember object needed to verify the bot can manage
/// forum topics. `can_manage_topics` is only present (and meaningful) when the
/// member is an administrator; it is absent for plain members, so we model it
/// as `Option<bool>` and treat absence as "not granted".
#[derive(serde::Deserialize)]
struct ChatMemberInfo {
    status: String,
    can_manage_topics: Option<bool>,
}

#[derive(serde::Deserialize, Clone)]
struct TelegramChat {
    id: i64,
    title: Option<String>,
    #[serde(rename = "type")]
    chat_type: String,
}

#[derive(serde::Deserialize)]
struct TelegramMessage {
    chat: Option<TelegramChat>,
}

#[derive(serde::Deserialize)]
struct TelegramUpdate {
    message: Option<TelegramMessage>,
}

async fn test_bot_token(client: &reqwest::Client, token: &str) -> Result<String, String> {
    let resp = client
        .get(format!("https://api.telegram.org/bot{token}/getMe"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let data: TelegramResponse<BotUser> =
        resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    if data.ok {
        let username = data
            .result
            .and_then(|r| r.username)
            .unwrap_or_else(|| "unknown".into());
        Ok(username)
    } else {
        Err(data.description.unwrap_or_else(|| "Invalid token".into()))
    }
}

/// L6.8 (INTENTIONAL): `detect_groups` is not exported as a public API.  It is
/// tightly coupled to the setup wizard flow -- it requires an authenticated
/// `reqwest::Client` and a bot token, and returns Telegram-specific types that
/// are private to this module.  Extracting it would require exposing internal
/// types (`TelegramChat`, `TelegramResponse`, `TelegramUpdate`) with no
/// external consumer.  It remains `pub(crate)` for testability if needed.
async fn detect_groups(client: &reqwest::Client, token: &str) -> Vec<TelegramChat> {
    let resp = match client
        .get(format!(
            "https://api.telegram.org/bot{token}/getUpdates?limit=100"
        ))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let data: TelegramResponse<Vec<TelegramUpdate>> = match resp.json().await {
        Ok(d) => d,
        Err(_) => return vec![],
    };

    let updates = match data.result {
        Some(u) => u,
        None => return vec![],
    };

    let mut groups = std::collections::HashMap::new();
    for update in &updates {
        if let Some(msg) = &update.message {
            if let Some(chat) = &msg.chat {
                if chat.chat_type == "supergroup" || chat.chat_type == "group" {
                    groups.insert(chat.id, chat.clone());
                }
            }
        }
    }

    groups.into_values().collect()
}

async fn test_chat_send(
    client: &reqwest::Client,
    token: &str,
    chat_id: &str,
) -> Result<(), String> {
    let resp = client
        .post(format!(
            "https://api.telegram.org/bot{token}/sendMessage"
        ))
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "text": "\u{1F916} Claude Telegram Mirror - Setup test successful!\n\nIf you see this, your bot configuration is correct.",
            "parse_mode": "Markdown",
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let data: TelegramResponse<serde_json::Value> =
        resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    if data.ok {
        Ok(())
    } else {
        Err(data
            .description
            .unwrap_or_else(|| "Failed to send message".into()))
    }
}

/// Fetch the bot's own numeric user id via getMe. Needed because getChatMember
/// is keyed by user_id, and a bot can only look itself up by its own id.
async fn get_bot_id(client: &reqwest::Client, token: &str) -> Result<i64, String> {
    let resp = client
        .get(format!("https://api.telegram.org/bot{token}/getMe"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let data: TelegramResponse<BotUser> =
        resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    if data.ok {
        data.result
            .map(|r| r.id)
            .ok_or_else(|| "getMe returned no result".to_string())
    } else {
        Err(data.description.unwrap_or_else(|| "getMe failed".into()))
    }
}

/// Query the bot's membership in the configured chat so the wizard can verify
/// the 'Manage Topics' admin permission. `test_chat_send` proves the bot can
/// post, but it cannot detect whether per-session forum topics will work — a
/// plain member (or an admin without 'Manage Topics') silently falls back to
/// the General topic at runtime, with the failure logged only at debug level.
async fn check_bot_permissions(
    client: &reqwest::Client,
    token: &str,
    chat_id: &str,
    bot_id: i64,
) -> Result<ChatMemberInfo, String> {
    let resp = client
        .post(format!(
            "https://api.telegram.org/bot{token}/getChatMember"
        ))
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "user_id": bot_id,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let data: TelegramResponse<ChatMemberInfo> =
        resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    if data.ok {
        data.result
            .ok_or_else(|| "getChatMember returned no result".to_string())
    } else {
        Err(data
            .description
            .unwrap_or_else(|| "getChatMember failed".into()))
    }
}

// ---------------------------------------------------------------------------
// Existing config parser
// ---------------------------------------------------------------------------

fn load_existing_config() -> (Option<String>, Option<String>) {
    let mut token: Option<String> = None;
    let mut chat_id: Option<String> = None;

    // Check env file
    let env = env_file();
    if env.exists() {
        let vars = crate::service::parse_env_file(&env);
        if let Some(t) = vars.get("TELEGRAM_BOT_TOKEN") {
            if !t.is_empty() {
                token = Some(t.clone());
            }
        }
        if let Some(c) = vars.get("TELEGRAM_CHAT_ID") {
            if !c.is_empty() {
                chat_id = Some(c.clone());
            }
        }
    }

    // Check config file (takes precedence)
    let cfg = config_file();
    if cfg.exists() {
        if let Ok(content) = fs::read_to_string(&cfg) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(t) = v.get("botToken").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        token = Some(t.to_string());
                    }
                }
                if let Some(c) = v.get("chatId").and_then(|c| c.as_i64()) {
                    if c != 0 {
                        chat_id = Some(c.to_string());
                    }
                }
            }
        }
    }

    // Check env vars (highest precedence)
    if let Ok(t) = std::env::var("TELEGRAM_BOT_TOKEN") {
        if !t.is_empty() {
            token = Some(t);
        }
    }
    if let Ok(c) = std::env::var("TELEGRAM_CHAT_ID") {
        if !c.is_empty() {
            chat_id = Some(c);
        }
    }

    (token, chat_id)
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

pub async fn run_setup() -> anyhow::Result<()> {
    println!();
    println!(
        "{}",
        cyan("================================================================")
    );
    println!("{}", bold("  Claude Telegram Mirror - Setup Wizard"));
    println!(
        "{}",
        cyan("================================================================")
    );
    println!();

    let (existing_token, existing_chat_id) = load_existing_config();
    if existing_token.is_some() {
        println!("{} Found existing bot token configuration", green("OK"));
    }
    if existing_chat_id.is_some() {
        println!("{} Found existing chat ID configuration", green("OK"));
    }
    if existing_token.is_some() || existing_chat_id.is_some() {
        println!();
    }

    // ADR-014 D1: a fresh setup is one with no existing token AND no existing chat
    // ID. The trust acknowledgment is required for new setups only — existing users
    // are not re-prompted on upgrade/reconfigure.
    let is_new_setup = existing_token.is_none() && existing_chat_id.is_none();

    let client = reqwest::Client::new();

    // ================================================================
    // STEP 1: Bot Token
    // ================================================================
    println!("{}", yellow("STEP 1: CREATE TELEGRAM BOT"));
    separator();
    println!();
    println!("You need to create a Telegram bot via @BotFather.");
    println!();
    println!("  1. Open Telegram and search for {}", cyan("@BotFather"));
    println!("  2. Send {}", cyan("/newbot"));
    println!("  3. Choose a name (e.g., 'Claude Mirror')");
    println!("  4. Choose a username (must end in 'bot', e.g., 'claude_mirror_bot')");
    println!("  5. Copy the API token provided");
    println!();

    let bot_token;
    let bot_username;

    loop {
        let default = existing_token.clone().unwrap_or_default();
        let input: String = Input::new()
            .with_prompt("Enter your bot token")
            .default(default)
            .interact_text()?;

        let input = input.trim().to_string();
        if input.is_empty() {
            println!("{}", red("Token cannot be empty"));
            continue;
        }

        if !input.contains(':') {
            println!(
                "{}",
                red("Token format looks incorrect. Expected format: 123456789:ABCdefGHI...")
            );
            continue;
        }

        print!("{}", gray("Verifying token with Telegram... "));
        match test_bot_token(&client, &input).await {
            Ok(username) => {
                println!("{}", green("Valid"));
                println!("{} Bot verified: @{username}", green("OK"));
                bot_token = input;
                bot_username = username;
                break;
            }
            Err(e) => {
                println!("{}", red("Invalid"));
                println!("  {}", red(&format!("Error: {e}")));

                if !Confirm::new()
                    .with_prompt("Try again?")
                    .default(true)
                    .interact()?
                {
                    println!("{}", red("Setup cancelled."));
                    std::process::exit(1);
                }
            }
        }
    }
    println!();

    // ================================================================
    // STEP 2: Disable Privacy Mode
    // ================================================================
    println!("{}", yellow("STEP 2: DISABLE PRIVACY MODE"));
    separator();
    println!();
    println!("Your bot needs to see all group messages (not just commands).");
    println!();
    println!("  1. Go back to {} in Telegram", cyan("@BotFather"));
    println!("  2. Send {}", cyan("/mybots"));
    println!("  3. Select @{bot_username}");
    println!("  4. Click '{}'", cyan("Bot Settings"));
    println!("  5. Click '{}'", cyan("Group Privacy"));
    println!("  6. Click '{}'", cyan("Turn off"));
    println!();
    println!(
        "{}",
        gray(&format!(
            "You should see: \"Privacy mode is disabled for @{bot_username}\""
        ))
    );
    println!();

    loop {
        let done = Confirm::new()
            .with_prompt("Have you disabled privacy mode?")
            .default(false)
            .interact()?;

        if done {
            break;
        }

        println!();
        println!(
            "{}",
            yellow("Privacy mode MUST be disabled for the bot to work in groups.")
        );
        println!("  {}", gray("Please complete this step before continuing."));
        println!();

        if !Confirm::new()
            .with_prompt("Try again?")
            .default(true)
            .interact()?
        {
            println!("{}", red("Setup cancelled."));
            std::process::exit(1);
        }
    }

    println!("{} Privacy mode configured", green("OK"));
    println!();

    // ================================================================
    // STEP 3: Setup Supergroup with Topics
    // ================================================================
    println!("{}", yellow("STEP 3: SETUP SUPERGROUP WITH TOPICS"));
    separator();
    println!();
    println!("{}", bold("Option A: Use an existing supergroup"));
    println!("  1. Add @{bot_username} to your existing supergroup");
    println!(
        "  2. Make the bot an admin with '{}' permission",
        cyan("Manage Topics")
    );
    println!("  3. Send any message in the group (so we can detect it)");
    println!();
    println!("{}", bold("Option B: Create a new group"));
    println!("  1. In Telegram, create a new group");
    println!("  2. Add @{bot_username} to the group");
    println!("  3. Go to group settings -> Enable '{}'", cyan("Topics"));
    println!("     {}", gray("(This converts it to a supergroup)"));
    println!(
        "  4. Make the bot an admin with '{}' permission",
        cyan("Manage Topics")
    );
    println!("  5. Send any message in the group");
    println!();

    let _: String = Input::new()
        .with_prompt(gray("Press Enter when you have completed these steps"))
        .default(String::new())
        .allow_empty(true)
        .interact_text()?;
    println!();

    let mut chat_id = String::new();

    // Try to auto-detect groups
    print!("{}", gray("Looking for your group... "));
    let groups = detect_groups(&client, &bot_token).await;

    if groups.len() == 1 {
        println!("{}", green("Found"));
        println!();
        let title = groups[0].title.as_deref().unwrap_or("Unnamed");
        println!(
            "{} Found group: {} ({})",
            green("OK"),
            bold(title),
            groups[0].id
        );

        if Confirm::new()
            .with_prompt("Is this the correct group?")
            .default(true)
            .interact()?
        {
            chat_id = groups[0].id.to_string();
        }
    } else if groups.len() > 1 {
        println!("{}", green("Found"));
        println!();
        println!("Found multiple groups:");
        println!();

        let mut items: Vec<String> = groups
            .iter()
            .map(|g| format!("{} ({})", g.title.as_deref().unwrap_or("Unnamed"), g.id))
            .collect();
        items.push("Enter manually".into());

        let selection = Select::new()
            .with_prompt("Select group")
            .items(&items)
            .default(0)
            .interact()?;

        if selection < groups.len() {
            chat_id = groups[selection].id.to_string();
            println!(
                "{} Selected: {}",
                green("OK"),
                groups[selection].title.as_deref().unwrap_or("Unnamed")
            );
        }
        // else: fall through to manual entry
    } else {
        println!("{}", yellow("not found"));
        println!();
        println!("{}", yellow("No supergroups found. This can happen if:"));
        println!("  {}", gray("- The bot hasn't seen any messages yet"));
        println!(
            "  {}",
            gray("- The group wasn't converted to a supergroup (enable Topics!)")
        );
        println!();
    }

    // Manual entry if needed
    if chat_id.is_empty() {
        println!();
        println!("Enter the chat ID manually.");
        println!("You can find it by:");
        println!("  1. Send a message in the group");
        println!(
            "  2. Visit: {}",
            cyan(&format!(
                "https://api.telegram.org/bot{bot_token}/getUpdates"
            ))
        );
        println!(
            "  3. Look for {}",
            cyan("\"chat\":{\"id\": -100XXXXXXXXXX}")
        );
        println!();

        loop {
            let default = existing_chat_id.clone().unwrap_or_default();
            let input: String = Input::new()
                .with_prompt("Enter chat ID (starts with -100)")
                .default(default)
                .interact_text()?;

            let input = input.trim().to_string();
            if input.is_empty() {
                println!("{}", red("Chat ID is required"));
                continue;
            }

            if !input.starts_with("-100") && !input.starts_with('-') {
                println!(
                    "{}",
                    yellow("Chat ID should start with -100 (supergroup format)")
                );
                if !Confirm::new()
                    .with_prompt("Use this value anyway?")
                    .default(false)
                    .interact()?
                {
                    continue;
                }
            }

            chat_id = input;
            break;
        }
    }
    println!();

    // ================================================================
    // STEP 4: Verify Bot Permissions
    // ================================================================
    println!("{}", yellow("STEP 4: VERIFY BOT PERMISSIONS"));
    separator();
    println!();

    loop {
        print!("{}", gray("Testing if bot can post to the group... "));
        match test_chat_send(&client, &bot_token, &chat_id).await {
            Ok(()) => {
                println!("{}", green("Success"));
                println!();
                println!("{} Bot can post to the group!", green("OK"));
                println!(
                    "  {}",
                    gray("Check your Telegram group - you should see a test message.")
                );
                println!();

                // The post test above proves the bot can send, but not that it
                // can create per-session forum topics. Actively probe getChatMember
                // for the 'Manage Topics' admin permission so a missing grant is
                // caught here during setup rather than silently degrading at
                // runtime (sessions collapsing into the General topic).
                print!("{}", gray("Checking 'Manage Topics' permission... "));
                let probe = match get_bot_id(&client, &bot_token).await {
                    Ok(bot_id) => check_bot_permissions(&client, &bot_token, &chat_id, bot_id).await,
                    Err(e) => Err(e),
                };

                match probe {
                    Ok(member) => {
                        let is_admin =
                            member.status == "administrator" || member.status == "creator";
                        let can_manage = member.can_manage_topics.unwrap_or(false);

                        if is_admin && can_manage {
                            println!("{}", green("Enabled"));
                            println!(
                                "  {} Bot can create a forum topic per session.",
                                green("OK")
                            );
                            break;
                        }

                        println!("{}", yellow("Missing"));
                        println!();
                        if is_admin {
                            println!(
                                "{}",
                                yellow("The bot is an admin but lacks 'Manage Topics'.")
                            );
                        } else {
                            println!(
                                "{}",
                                yellow("The bot is not an admin in this group.")
                            );
                        }
                        println!(
                            "{}",
                            gray("Per-session forum topics will NOT be created — every")
                        );
                        println!(
                            "{}",
                            gray("session would share the General topic until this is fixed.")
                        );
                        println!();
                        println!("{}", bold("To fix:"));
                        println!(
                            "  {}",
                            gray("Group Settings -> Administrators -> select the bot")
                        );
                        println!("  {}", gray("-> enable 'Manage Topics', then retry."));
                        println!();

                        let choice = Select::new()
                            .with_prompt("How would you like to proceed?")
                            .items(&[
                                "Retry (I've enabled the permission)",
                                "Continue anyway",
                            ])
                            .default(0)
                            .interact()?;

                        if choice == 1 {
                            break;
                        }
                        // Retry: re-run the whole permission step.
                        continue;
                    }
                    Err(e) => {
                        // The permission probe is advisory — the post test already
                        // passed — so a transient failure here must not block setup.
                        println!("{}", yellow("skipped"));
                        println!(
                            "  {}",
                            gray(&format!("Could not verify topic permission: {e}"))
                        );
                        break;
                    }
                }
            }
            Err(e) => {
                println!("{}", red("Failed"));
                println!();
                println!("{}", red(&format!("Bot cannot post: {e}")));
                println!();
                println!("{}", bold("Common fixes:"));
                println!("  {}", gray("- Make sure the bot is an admin in the group"));
                println!(
                    "  {}",
                    gray("- Ensure 'Post Messages' permission is enabled")
                );
                println!(
                    "  {}",
                    gray("- Check that 'Manage Topics' permission is enabled")
                );
                println!();

                let _: String = Input::new()
                    .with_prompt(gray(
                        "Fix the issue and press Enter to retry, or Ctrl+C to exit",
                    ))
                    .default(String::new())
                    .allow_empty(true)
                    .interact_text()?;
            }
        }
    }

    let _: String = Input::new()
        .with_prompt(gray("Press Enter to continue"))
        .default(String::new())
        .allow_empty(true)
        .interact_text()?;
    println!();

    // ================================================================
    // STEP 5: Configuration Options
    // ================================================================
    println!("{}", yellow("STEP 5: CONFIGURATION OPTIONS"));
    separator();
    println!();

    let existing_use_threads = config::load_config(false)
        .map(|c| c.use_threads)
        .unwrap_or(true);

    let use_threads = Confirm::new()
        .with_prompt("Enable forum threads (each session gets its own topic)?")
        .default(existing_use_threads)
        .interact()?;

    let install_hooks_choice = Confirm::new()
        .with_prompt("Install Claude Code hooks?")
        .default(true)
        .interact()?;

    let service_prompt = if cfg!(target_os = "macos") {
        "Install as launchd service (auto-start on login)?"
    } else {
        "Install as systemd service (auto-start)?"
    };
    let install_service_choice = Confirm::new()
        .with_prompt(service_prompt)
        .default(true)
        .interact()?;

    println!();

    // ================================================================
    // STEP 6: Save Configuration
    // ================================================================
    println!("{}", yellow("STEP 6: SAVING CONFIGURATION"));
    separator();
    println!();

    // ADR-014 D1: Require an explicit, recorded trust acknowledgment BEFORE any
    // config is written — and only for new setups (do not re-prompt configured
    // users on upgrade). The chat-level trust model is intentional (see ADR-014
    // "Neutral consequences"): anyone with channel access already has, in effect,
    // git-commit access. We make that an active operator decision, not a buried note.
    let trust_acknowledged = if is_new_setup {
        println!();
        println!("{}", yellow("TRUST MODEL — PLEASE READ"));
        separator();
        println!();
        print_box(TRUST_NOTICE);
        println!();
        let accepted = Confirm::new()
            .with_prompt("Do you understand and accept this?")
            .default(false)
            .interact()?;
        if !accepted {
            println!();
            println!(
                "{} Setup aborted — trust model not accepted. No configuration was written.",
                yellow("!")
            );
            return Ok(());
        }
        true
    } else {
        // Existing setup: the operator already accepted (or predates the prompt).
        // Do not re-prompt; preserve acceptance.
        true
    };

    let cdir = config_dir();
    config::ensure_config_dir(&cdir)?;
    println!("{} Created/verified config directory", green("OK"));

    // Save config.json
    let chat_id_num: i64 = chat_id.parse().unwrap_or(0);
    let config_value = serde_json::json!({
        "botToken": bot_token,
        "chatId": chat_id_num,
        "enabled": true,
        "useThreads": use_threads,
        "verbose": true,
        "approvals": true,
        // ADR-014 D1: record the recorded trust acknowledgment.
        "trustAcknowledged": trust_acknowledged,
    });

    let config_path = config_file();
    fs::write(&config_path, serde_json::to_string_pretty(&config_value)?)?;
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;
    println!(
        "{} Saved config to {}",
        green("OK"),
        gray(&config_path.display().to_string())
    );

    // Save ~/.telegram-env
    let env_content = format!(
        "# Claude Telegram Mirror Configuration\nexport TELEGRAM_BOT_TOKEN=\"{bot_token}\"\nexport TELEGRAM_CHAT_ID=\"{chat_id}\"\nexport TELEGRAM_MIRROR=true\n"
    );
    let env_path = env_file();
    fs::write(&env_path, env_content)?;
    fs::set_permissions(&env_path, fs::Permissions::from_mode(0o600))?;
    println!(
        "{} Saved environment to {}",
        green("OK"),
        gray(&env_path.display().to_string())
    );

    println!();
    println!(
        "{}",
        gray("Add to your shell profile (~/.bashrc or ~/.zshrc):")
    );
    println!(
        "  {}",
        cyan("[[ -f ~/.telegram-env ]] && source ~/.telegram-env")
    );
    println!();

    // ================================================================
    // STEP 7: Install Hooks
    // ================================================================
    if install_hooks_choice {
        println!("{}", yellow("STEP 7: INSTALL CLAUDE CODE HOOKS"));
        separator();
        println!();

        match crate::installer::install_hooks(false) {
            Ok(()) => {
                println!(
                    "{} Global hooks installed to ~/.claude/settings.json",
                    green("OK")
                );
            }
            Err(e) => {
                println!("{} Could not install hooks: {e}", yellow("WARN"));
            }
        }

        println!();
        println!("{}", gray("  Global hooks apply to EVERY project automatically —"));
        println!(
            "{}",
            gray("  Claude Code merges hooks across scopes, so a project with its")
        );
        println!(
            "{}",
            gray("  own .claude/settings.json still runs these global hooks.")
        );
        println!(
            "{}",
            gray("  You do NOT need per-project hooks; installing them too would")
        );
        println!("{}", gray("  make each hook fire twice."));
        println!();
        println!(
            "{}",
            gray("  Only if you want project-scoped hooks INSTEAD of global (e.g.")
        );
        println!("{}", gray("  committed team settings): run `ctm install-hooks -p` in"));
        println!(
            "{}",
            gray("  that project and remove the global ones (`ctm uninstall-hooks`).")
        );
        println!(
            "{}",
            gray("  `ctm doctor --fix` consolidates any accidental duplicates.")
        );
        println!();
    }

    // ================================================================
    // STEP 8: Install Service
    // ================================================================
    if install_service_choice {
        let step_num = if install_hooks_choice { 8 } else { 7 };
        println!(
            "{}",
            yellow(&format!("STEP {step_num}: INSTALL SYSTEM SERVICE"))
        );
        separator();
        println!();

        print!("{}", gray("Installing service... "));
        let install_result = crate::service::install_service();
        if install_result.success {
            println!("{}", green("OK"));

            print!("{}", gray("Starting service... "));
            let start_result = crate::service::start_service();
            if start_result.success {
                println!("{}", green("OK"));
                println!();
                println!("{} Service is running!", green("OK"));
            } else {
                println!("{}", yellow("WARN"));
                println!();
                println!("{} Service may not have started correctly", yellow("WARN"));
            }
        } else {
            println!("{}", yellow("WARN"));
            println!("{} {}", yellow("WARN"), install_result.message);
        }

        println!();

        if cfg!(target_os = "macos") {
            println!("{}", gray("Troubleshooting (macOS):"));
            println!("  {}", gray("Check status: launchctl list | grep claude"));
            println!(
                "  {}",
                gray("View logs: cat ~/Library/Logs/claude-telegram-mirror.*.log")
            );
        } else {
            println!("{}", gray("Troubleshooting (Linux):"));
            println!(
                "  {}",
                gray("Check status: systemctl --user status claude-telegram-mirror")
            );
            println!(
                "  {}",
                gray("View logs: journalctl --user -u claude-telegram-mirror -f")
            );
            println!();
            println!("  {}", gray("You may need to enable user lingering:"));
            println!("  {}", gray("  loginctl enable-linger $USER"));
        }

        println!();
    }

    // ================================================================
    // COMPLETION
    // ================================================================
    println!(
        "{}",
        cyan("================================================================")
    );
    println!("  {}", green("INSTALLATION COMPLETE"));
    println!(
        "{}",
        cyan("================================================================")
    );
    println!();

    println!("{}", bold("Summary:"));
    println!("  Bot:     @{bot_username}");
    println!("  Chat:    {chat_id}");
    println!("  Config:  {}", config_file().display());
    println!("  Env:     {}", env_file().display());
    println!();

    println!("{}", bold("Commands:"));
    println!(
        "  {}            Start daemon (foreground)",
        cyan("ctm start")
    );
    println!("  {}           Show status", cyan("ctm status"));
    println!("  {}   Service status", cyan("ctm service status"));
    println!("  {}           Diagnose issues", cyan("ctm doctor"));
    println!();

    println!("{}", bold("Next steps:"));
    println!(
        "  1. Run '{}' or restart terminal",
        cyan("source ~/.telegram-env")
    );
    println!("  2. Start a Claude Code session in tmux:");
    println!("     {}", cyan("tmux new -s claude"));
    println!("     {}", cyan("claude"));
    println!();
    println!(
        "{}",
        green("Your Claude sessions will now be mirrored to Telegram!")
    );
    println!();

    // L3.7: Project-hooks reminder box (L6.7: uses reusable print_box).
    // Global hooks already apply to every project (Claude merges hook scopes), so
    // the box clarifies that project hooks are a REPLACEMENT, not an addition.
    println!();
    print_box(
        "  Global hooks already cover every project.          \n\
         \x20 Project hooks are only for project-scoped INSTEAD \n\
         \x20 of global (then `ctm uninstall-hooks`):           \n\
         \x20   ctm install-hooks -p                            \n\
         \x20 `ctm doctor --fix` cleans accidental duplicates.  ",
    );
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-014 D1: the recorded trust notice must keep its load-bearing wording —
    /// the git-commit-access framing and the "not supported" stance for
    /// semi-trusted/public channels. Guards against accidental softening.
    #[test]
    fn trust_notice_states_the_model() {
        assert!(
            TRUST_NOTICE.contains("git-commit access"),
            "trust notice must frame access as git-commit-equivalent"
        );
        assert!(
            TRUST_NOTICE.to_lowercase().contains("not supported"),
            "trust notice must state semi-trusted/public channels are not supported"
        );
        assert!(TRUST_NOTICE.contains("drive your shell"));
    }

    #[test]
    fn test_load_existing_config_empty() {
        // In test env, should not panic even with no config
        let (token, chat_id) = load_existing_config();
        // Just check it doesn't panic — results depend on env
        let _ = (token, chat_id);
    }

    #[test]
    fn test_color_functions() {
        assert!(cyan("test").contains("test"));
        assert!(green("test").contains("test"));
        assert!(yellow("test").contains("test"));
        assert!(red("test").contains("test"));
        assert!(gray("test").contains("test"));
        assert!(bold("test").contains("test"));
    }
}
