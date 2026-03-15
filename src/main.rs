mod bot;
mod bridge;
mod config;
mod error;
mod formatting;
mod hook;
mod injector;
mod session;
mod socket;
mod types;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "ctm",
    about = "Claude Telegram Mirror - Bidirectional Claude Code <-> Telegram bridge",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the bridge daemon
    Start {
        /// Run in foreground (don't daemonize)
        #[arg(long, default_value_t = true)]
        foreground: bool,
    },

    /// Process hook events from stdin (called by Claude Code hooks)
    Hook,

    /// Show daemon and configuration status
    Status,

    /// Run diagnostics and validate configuration
    Doctor {
        /// Attempt to fix issues automatically
        #[arg(long)]
        fix: bool,
    },

    /// Interactive setup wizard
    Setup,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start { foreground: _ } => cmd_start().await,
        Commands::Hook => cmd_hook().await,
        Commands::Status => cmd_status().await,
        Commands::Doctor { fix } => cmd_doctor(fix).await,
        Commands::Setup => cmd_setup().await,
    }
}

async fn cmd_start() -> anyhow::Result<()> {
    let cfg = config::load_config(true)?;

    tracing::info!(
        chat_id = cfg.chat_id,
        verbose = cfg.verbose,
        use_threads = cfg.use_threads,
        auto_delete_topics = cfg.auto_delete_topics,
        "Starting CTM bridge"
    );

    let bridge = bridge::Bridge::new(cfg)?;
    bridge.start().await?;
    Ok(())
}

async fn cmd_hook() -> anyhow::Result<()> {
    let cfg = config::load_config(false)?;
    hook::process_hook(&cfg.socket_path).await?;
    Ok(())
}

async fn cmd_status() -> anyhow::Result<()> {
    let cfg = config::load_config(false)?;

    println!("Claude Telegram Mirror - Status");
    println!("================================");

    // Check socket
    let socket_exists = cfg.socket_path.exists();
    println!(
        "Bridge daemon: {}",
        if socket_exists { "RUNNING" } else { "STOPPED" }
    );
    println!("Socket: {}", cfg.socket_path.display());

    // Check config
    let (errors, warnings) = config::validate_config(&cfg);
    if errors.is_empty() {
        println!("Configuration: OK");
    } else {
        println!("Configuration: ERRORS");
        for e in &errors {
            println!("  ERROR: {}", e);
        }
    }
    for w in &warnings {
        println!("  WARN: {}", w);
    }

    // Check sessions
    match session::SessionManager::new(&cfg.config_dir, 5) {
        Ok(mgr) => {
            let (active, pending) = mgr.get_stats();
            println!("Active sessions: {}", active);
            println!("Pending approvals: {}", pending);
        }
        Err(e) => println!("Database: ERROR - {}", e),
    }

    Ok(())
}

async fn cmd_doctor(fix: bool) -> anyhow::Result<()> {
    println!("Claude Telegram Mirror - Doctor");
    println!("================================");

    let mut issues = 0;
    let mut fixed = 0;

    // Check Rust binary
    println!("\n[1/6] Binary...");
    println!("  OK: ctm binary running");

    // Check config directory
    println!("\n[2/6] Config directory...");
    let config_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config")
        .join("claude-telegram-mirror");

    if config_dir.exists() {
        // Check permissions
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&config_dir)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0);

        if mode & 0o077 != 0 {
            println!("  WARN: Config dir has insecure permissions ({:o})", mode);
            issues += 1;
            if fix {
                std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700))?;
                println!("  FIXED: Set permissions to 0700");
                fixed += 1;
            }
        } else {
            println!("  OK: Config directory exists with secure permissions");
        }
    } else {
        println!("  WARN: Config directory does not exist");
        issues += 1;
        if fix {
            config::ensure_config_dir(&config_dir)?;
            println!("  FIXED: Created config directory with 0700 permissions");
            fixed += 1;
        }
    }

    // Check environment
    println!("\n[3/6] Environment variables...");
    let cfg = config::load_config(false)?;
    let (errors, warnings) = config::validate_config(&cfg);
    for e in &errors {
        println!("  ERROR: {}", e);
        issues += 1;
    }
    for w in &warnings {
        println!("  WARN: {}", w);
    }
    if errors.is_empty() && warnings.is_empty() {
        println!("  OK: All environment variables set");
    }

    // Check tmux
    println!("\n[4/6] tmux...");
    if injector::InputInjector::is_tmux_available() {
        println!("  OK: tmux is available");
        if let Some(info) = injector::InputInjector::detect_tmux_session() {
            println!("  OK: tmux session detected: {}", info.target);
        } else {
            println!("  WARN: No active tmux session detected");
        }
    } else {
        println!("  WARN: tmux is not installed");
        issues += 1;
    }

    // Check socket
    println!("\n[5/6] Socket...");
    if cfg.socket_path.exists() {
        println!("  OK: Bridge socket exists at {}", cfg.socket_path.display());
    } else {
        println!("  INFO: Bridge socket not found (daemon not running)");
    }

    // Check database
    println!("\n[6/6] Database...");
    match session::SessionManager::new(&cfg.config_dir, 5) {
        Ok(mgr) => {
            let (active, pending) = mgr.get_stats();
            println!(
                "  OK: Database accessible ({} sessions, {} approvals)",
                active, pending
            );
        }
        Err(e) => {
            println!("  ERROR: Database error: {}", e);
            issues += 1;
        }
    }

    // Summary
    println!("\n================================");
    if issues == 0 {
        println!("All checks passed!");
    } else {
        println!(
            "{} issues found, {} fixed",
            issues,
            fixed
        );
    }

    Ok(())
}

async fn cmd_setup() -> anyhow::Result<()> {
    println!("Claude Telegram Mirror - Setup");
    println!("================================\n");
    println!("This wizard will help you configure CTM.\n");

    println!("Step 1: Create a Telegram bot");
    println!("  1. Open Telegram and message @BotFather");
    println!("  2. Send /newbot and follow the prompts");
    println!("  3. Copy the bot token\n");

    println!("Step 2: Set environment variables");
    println!("  export TELEGRAM_BOT_TOKEN=\"your-bot-token\"");
    println!("  export TELEGRAM_CHAT_ID=\"your-chat-id\"\n");

    println!("Step 3: Get your chat ID");
    println!("  1. Create a forum-enabled group or use an existing one");
    println!("  2. Add your bot to the group");
    println!("  3. Send a message in the group");
    println!("  4. Visit: https://api.telegram.org/bot<TOKEN>/getUpdates");
    println!("  5. Find the chat.id in the response\n");

    println!("Step 4: Install hooks");
    println!("  Add to ~/.claude/settings.json:");
    println!("  {{");
    println!("    \"hooks\": {{");
    println!("      \"PreToolUse\": [{{ \"command\": \"ctm hook\" }}],");
    println!("      \"PostToolUse\": [{{ \"command\": \"ctm hook\" }}],");
    println!("      \"Notification\": [{{ \"command\": \"ctm hook\" }}],");
    println!("      \"Stop\": [{{ \"command\": \"ctm hook\" }}],");
    println!("      \"UserPromptSubmit\": [{{ \"command\": \"ctm hook\" }}]");
    println!("    }}");
    println!("  }}\n");

    println!("Step 5: Start the daemon");
    println!("  ctm start\n");

    println!("Step 6: Verify");
    println!("  ctm doctor\n");

    Ok(())
}
