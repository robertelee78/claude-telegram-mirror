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
    username: Option<String>,
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
                break;
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
        println!("{}", yellow("  IMPORTANT: PROJECT-LEVEL HOOKS"));
        println!();
        println!("  If you use Claude Code in projects that have their own");
        println!("  .claude/settings.json file, the GLOBAL hooks we just");
        println!("  installed will be IGNORED in those projects.");
        println!();
        println!("  To enable Telegram mirroring in a specific project:");
        println!();
        println!("    cd /path/to/your/project");
        println!("    ctm install-hooks --project");
        println!();

        let has_project = Confirm::new()
            .with_prompt("Do you have a project with .claude/settings.json that needs hooks?")
            .default(false)
            .interact()?;

        if has_project {
            loop {
                let project_path: String = Input::new()
                    .with_prompt("Enter project path (or 'done' to finish)")
                    .default("done".into())
                    .interact_text()?;

                if project_path.trim().to_lowercase() == "done" || project_path.trim().is_empty() {
                    break;
                }

                let full_path = if project_path.starts_with('/') {
                    PathBuf::from(&project_path)
                } else {
                    std::env::current_dir()?.join(&project_path)
                };

                let claude_dir = full_path.join(".claude");
                if !claude_dir.exists() {
                    println!("{} No .claude/ directory in {project_path}", yellow("WARN"));
                    println!(
                        "  {}",
                        gray("This project doesn't have custom Claude settings.")
                    );
                    println!(
                        "  {}",
                        gray("Global hooks will work here - no action needed!")
                    );
                } else {
                    // L9: pass project path directly instead of using set_current_dir
                    match crate::installer::install_hooks_for_project(&full_path) {
                        Ok(()) => {
                            println!(
                                "{} Hooks installed to {project_path}/.claude/settings.json",
                                green("OK")
                            );
                        }
                        Err(e) => {
                            println!("{} {e}", yellow("WARN"));
                        }
                    }
                }

                if !Confirm::new()
                    .with_prompt("Add another project?")
                    .default(false)
                    .interact()?
                {
                    break;
                }
            }
        }
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

    // L3.7: Project-hooks reminder box (L6.7: uses reusable print_box)
    println!();
    print_box(
        "  REMEMBER: If your project has .claude/settings  \n\
         \x20 that override global hooks, run:                \n\
         \x20   ctm install-hooks -p                          \n\
         \x20 from your project directory.                    ",
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
