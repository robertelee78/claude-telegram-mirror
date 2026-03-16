// Public API modules — some exports used in tests and future phases.
// Many items are prepared for Phase 4+ and intentionally unused now.
#[allow(dead_code)]
mod bot;
#[allow(dead_code)]
mod config;
mod daemon;
#[allow(dead_code)]
mod error;
#[allow(dead_code)]
mod formatting;
mod hook;
#[allow(dead_code)]
mod injector;
#[allow(dead_code)]
mod session;
#[allow(dead_code)]
mod socket;
#[allow(dead_code)]
mod summarize;
#[allow(dead_code)]
mod types;

use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "ctm",
    about = "Claude Telegram Mirror — Bidirectional Claude Code <-> Telegram bridge",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Process hook events from stdin (called by Claude Code hooks)
    Hook,

    /// Start the bridge daemon
    Start {
        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,
    },

    /// Stop the bridge daemon
    Stop {
        /// Force kill if graceful shutdown fails
        #[arg(long)]
        force: bool,
    },

    /// Restart the bridge daemon
    Restart {
        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,
    },

    /// Show bridge daemon status
    Status,

    /// Show or modify configuration
    Config {
        /// Show current configuration
        #[arg(long)]
        show: bool,

        /// Test Telegram connection
        #[arg(long)]
        test: bool,
    },

    /// Install Claude Code hooks (delegates to TypeScript)
    InstallHooks {
        /// Install to current project's .claude/settings.json
        #[arg(short, long)]
        project: bool,
    },

    /// Remove Claude Code hooks (delegates to TypeScript)
    UninstallHooks,

    /// Show hook installation status (delegates to TypeScript)
    Hooks,

    /// Interactive setup wizard (delegates to TypeScript)
    Setup,

    /// Diagnose configuration and connectivity issues (delegates to TypeScript)
    Doctor {
        /// Attempt to automatically fix detected issues
        #[arg(long)]
        fix: bool,
    },

    /// Manage systemd/launchd service (delegates to TypeScript)
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Install as a system service
    Install,
    /// Uninstall the system service
    Uninstall,
    /// Start the service
    Start,
    /// Stop the service
    Stop,
    /// Restart the service
    Restart,
    /// Show service status
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing — all output to stderr
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Hook => hook::process_hook().await,

        Commands::Start { verbose } => cmd_start(verbose).await,
        Commands::Stop { force } => cmd_stop(force).await,
        Commands::Restart { verbose } => cmd_restart(verbose).await,
        Commands::Status => cmd_status(),
        Commands::Config { show: _, test } => cmd_config(test).await,

        // Phase 4 commands delegate to TypeScript
        Commands::InstallHooks { project } => delegate_to_ts(&if project {
            vec!["install-hooks", "-p"]
        } else {
            vec!["install-hooks"]
        }),
        Commands::UninstallHooks => delegate_to_ts(&["uninstall-hooks"]),
        Commands::Hooks => delegate_to_ts(&["hooks"]),
        Commands::Setup => delegate_to_ts(&["setup"]),
        Commands::Doctor { fix } => delegate_to_ts(&if fix {
            vec!["doctor", "--fix"]
        } else {
            vec!["doctor"]
        }),
        Commands::Service { action } => {
            let args = match action {
                ServiceAction::Install => vec!["service", "install"],
                ServiceAction::Uninstall => vec!["service", "uninstall"],
                ServiceAction::Start => vec!["service", "start"],
                ServiceAction::Stop => vec!["service", "stop"],
                ServiceAction::Restart => vec!["service", "restart"],
                ServiceAction::Status => vec!["service", "status"],
            };
            delegate_to_ts(&args)
        }
    }
}

// ====================================================================== commands

/// Start the bridge daemon in foreground.
async fn cmd_start(verbose: bool) -> anyhow::Result<()> {
    println!("Starting Claude Code Telegram Mirror...\n");

    let mut cfg = config::load_config(true)?;
    if verbose {
        cfg.verbose = true;
    }

    let mut daemon = daemon::Daemon::new(cfg)?;

    // Handle shutdown signals
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(shutdown_tx)));

    {
        let tx = std::sync::Arc::clone(&shutdown_tx);
        ctrlc::set_handler(move || {
            if let Ok(mut guard) = tx.lock() {
                if let Some(sender) = guard.take() {
                    let _ = sender.send(());
                }
            }
        })
        .ok();
    }

    daemon.start().await?;

    println!("Bridge daemon running");
    println!("Telegram chat will receive Claude Code updates");
    println!("Reply in Telegram to send input to CLI\n");
    println!("Press Ctrl+C to stop\n");

    // Wait for shutdown signal
    let _ = shutdown_rx.await;

    println!("\nShutting down...");
    daemon.stop().await;

    Ok(())
}

/// Stop the bridge daemon.
async fn cmd_stop(force: bool) -> anyhow::Result<()> {
    let config_dir = config::get_config_dir();
    let pid_file = config_dir.join("bridge.pid");

    if !pid_file.exists() {
        println!("Daemon is not running (no PID file)");
        return Ok(());
    }

    let pid_str = fs::read_to_string(&pid_file)?;
    let pid: i32 = pid_str
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid PID file content"))?;

    if !is_process_running(pid) {
        println!("Daemon is not running (stale PID file), cleaning up...");
        cleanup_stale_files(&config_dir);
        return Ok(());
    }

    println!("Stopping daemon (PID {pid})...");

    // Send SIGTERM
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    );

    // Wait for exit (5s timeout)
    let exited = wait_for_exit(pid, 5000).await;

    if !exited {
        if force {
            println!("Graceful shutdown timed out, force killing...");
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
            wait_for_exit(pid, 1000).await;
        } else {
            println!("Daemon did not stop within 5 seconds. Use --force to kill it.");
            std::process::exit(1);
        }
    }

    if !is_process_running(pid) {
        cleanup_stale_files(&config_dir);
        println!("Daemon stopped");
    }

    Ok(())
}

/// Restart the bridge daemon.
async fn cmd_restart(verbose: bool) -> anyhow::Result<()> {
    let config_dir = config::get_config_dir();
    let pid_file = config_dir.join("bridge.pid");

    if pid_file.exists() {
        if let Ok(pid_str) = fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                if is_process_running(pid) {
                    println!("Stopping existing daemon (PID {pid})...");
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(pid),
                        nix::sys::signal::Signal::SIGTERM,
                    );
                    wait_for_exit(pid, 5000).await;
                }
            }
        }
        cleanup_stale_files(&config_dir);
    }

    cmd_start(verbose).await
}

/// Show daemon status.
fn cmd_status() -> anyhow::Result<()> {
    let cfg = config::load_config(false)?;
    let config_dir = config::get_config_dir();
    let pid_file = config_dir.join("bridge.pid");
    let socket_file = &cfg.socket_path;

    println!("\nClaude Telegram Mirror Status\n");

    // Check daemon running state
    println!("Daemon:");
    if pid_file.exists() {
        if let Ok(pid_str) = fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                if is_process_running(pid) {
                    println!("  Status: Running (PID {pid})");
                } else {
                    println!("  Status: Not running (stale PID file)");
                }
            }
        }
    } else {
        println!("  Status: Not running");
    }

    if socket_file.exists() {
        println!("  Socket: {}", socket_file.display());
    } else {
        println!("  Socket: Not created");
    }
    println!();

    // Configuration
    println!("Configuration:");
    println!(
        "  Bot Token: {}",
        if cfg.bot_token.is_empty() {
            "Not set"
        } else {
            "Set"
        }
    );
    println!(
        "  Chat ID: {}",
        if cfg.chat_id == 0 {
            "Not set".to_string()
        } else {
            cfg.chat_id.to_string()
        }
    );
    println!("  Enabled: {}", cfg.enabled);
    println!("  Verbose: {}", cfg.verbose);
    println!();

    Ok(())
}

/// Show or test configuration.
async fn cmd_config(test: bool) -> anyhow::Result<()> {
    let cfg = config::load_config(false)?;

    if test {
        println!("Testing Telegram connection...\n");

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "https://api.telegram.org/bot{}/getMe",
                cfg.bot_token
            ))
            .send()
            .await?;

        let data: serde_json::Value = resp.json().await?;
        if data["ok"].as_bool() == Some(true) {
            let username = data["result"]["username"].as_str().unwrap_or("unknown");
            println!("Bot connected: @{username}");

            // Try sending a test message
            let msg_resp = client
                .post(format!(
                    "https://api.telegram.org/bot{}/sendMessage",
                    cfg.bot_token
                ))
                .json(&serde_json::json!({
                    "chat_id": cfg.chat_id,
                    "text": "Test message from Claude Telegram Mirror (ctm)",
                    "parse_mode": "Markdown"
                }))
                .send()
                .await?;

            let msg_data: serde_json::Value = msg_resp.json().await?;
            if msg_data["ok"].as_bool() == Some(true) {
                println!("Test message sent to chat");
            } else {
                println!("Failed to send test message");
            }
        } else {
            println!("Invalid bot token");
        }
        return Ok(());
    }

    // Default: show config
    println!("\nConfiguration\n");
    println!("Environment Variables:");
    println!("  TELEGRAM_MIRROR={}", cfg.enabled);
    println!(
        "  TELEGRAM_BOT_TOKEN={}",
        if cfg.bot_token.is_empty() {
            "[NOT SET]"
        } else {
            "[SET]"
        }
    );
    println!(
        "  TELEGRAM_CHAT_ID={}",
        if cfg.chat_id == 0 {
            "[NOT SET]".to_string()
        } else {
            cfg.chat_id.to_string()
        }
    );
    println!("  TELEGRAM_MIRROR_VERBOSE={}", cfg.verbose);
    println!("  TELEGRAM_BRIDGE_SOCKET={}", cfg.socket_path.display());
    println!();
    println!("Add to ~/.bashrc or ~/.zshrc:");
    println!();
    println!("  export TELEGRAM_MIRROR=true");
    println!("  export TELEGRAM_BOT_TOKEN=\"your-bot-token\"");
    println!("  export TELEGRAM_CHAT_ID=\"your-chat-id\"");
    println!();

    Ok(())
}

// ====================================================================== delegation

/// Delegate a command to the TypeScript CLI (working delegation for Phase 4 commands).
fn delegate_to_ts(args: &[&str]) -> anyhow::Result<()> {
    // Try to find the TypeScript CLI
    let ts_cli = find_ts_cli();

    match ts_cli {
        Some(path) => {
            let status = std::process::Command::new("node")
                .arg(&path)
                .args(args)
                .status()?;

            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
            Ok(())
        }
        None => {
            eprintln!(
                "TypeScript CLI not found. This command requires the Node.js implementation."
            );
            eprintln!("Try: npx claude-telegram-mirror {}", args.join(" "));
            std::process::exit(1);
        }
    }
}

/// Find the TypeScript CLI distribution.
fn find_ts_cli() -> Option<PathBuf> {
    // Check standard locations
    let candidates = [
        PathBuf::from("/opt/claude-telegram-mirror/dist/cli.js"),
        PathBuf::from("dist/cli.js"),
    ];

    for path in &candidates {
        if path.exists() {
            return Some(path.clone());
        }
    }

    // Check relative to this binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("../dist/cli.js");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

// ====================================================================== helpers

fn is_process_running(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

async fn wait_for_exit(pid: i32, timeout_ms: u64) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed().as_millis() < timeout_ms as u128 {
        if !is_process_running(pid) {
            return true;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    false
}

fn cleanup_stale_files(config_dir: &std::path::Path) {
    let pid = config_dir.join("bridge.pid");
    let sock = config_dir.join("bridge.sock");
    if pid.exists() {
        let _ = fs::remove_file(&pid);
    }
    if sock.exists() {
        let _ = fs::remove_file(&sock);
    }
}
