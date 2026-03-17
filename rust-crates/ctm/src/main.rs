// Public API modules — some exports used in tests and future phases.
#[allow(dead_code)]
mod bot;
#[allow(dead_code)]
mod config;
mod daemon;
mod doctor;
#[allow(dead_code)]
mod error;
#[allow(dead_code)]
mod formatting;
mod hook;
#[allow(dead_code)]
mod injector;
mod installer;
mod service;
#[allow(dead_code)]
mod session;
mod setup;
#[allow(dead_code)]
mod socket;
#[allow(dead_code)]
mod summarize;
#[allow(dead_code)]
mod types;

use clap::{Parser, Subcommand};
use std::fs;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------- token scrubbing

/// A `Write` + `MakeWriter` that forwards all log output through `scrub_bot_token`
/// before writing to stderr. This ensures that no log message — regardless of
/// which code path emits it — can leak a raw Telegram bot token.
struct ScrubWriter;

impl std::io::Write for ScrubWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let scrubbed = bot::scrub_bot_token(&text);
        std::io::stderr().write_all(scrubbed.as_bytes())?;
        // Return the original length so callers believe the full buffer was consumed.
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ScrubWriter {
    type Writer = ScrubWriter;
    fn make_writer(&'a self) -> Self::Writer {
        ScrubWriter
    }
}

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
        /// Run in foreground (accepted for script compatibility; daemon always runs in foreground)
        #[arg(long)]
        foreground: bool,
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

    /// Install Claude Code hooks for Telegram mirroring
    InstallHooks {
        /// Install to current project's .claude/settings.json
        #[arg(short, long)]
        project: bool,
    },

    /// Remove Claude Code hooks
    UninstallHooks,

    /// Show hook installation status
    Hooks,

    /// Interactive setup wizard
    Setup,

    /// Diagnose configuration and connectivity issues
    Doctor {
        /// Attempt to automatically fix detected issues
        #[arg(long)]
        fix: bool,
    },

    /// Manage systemd/launchd service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Subcommand, Clone)]
pub enum ServiceAction {
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
    // Initialize tracing — all output goes through ScrubWriter which strips
    // any Telegram bot token (regex bot\d+:[A-Za-z0-9_-]+/) before writing
    // to stderr. This is a global defence-in-depth layer: even if a code path
    // interpolates a raw API URL into a log message the token never reaches
    // the terminal or log files.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_writer(ScrubWriter)
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Hook => hook::process_hook().await,

        Commands::Start {
            verbose,
            foreground: _,
        } => cmd_start(verbose).await,
        Commands::Stop { force } => cmd_stop(force).await,
        Commands::Restart { verbose } => cmd_restart(verbose).await,
        Commands::Status => cmd_status(),
        Commands::Config { show: _, test } => cmd_config(test).await,

        // Phase 4: Native Rust implementations — no TypeScript delegation
        Commands::InstallHooks { project } => installer::install_hooks(project),
        Commands::UninstallHooks => installer::uninstall_hooks(),
        Commands::Hooks => installer::print_hook_status(),
        Commands::Setup => setup::run_setup().await,
        Commands::Doctor { fix } => doctor::run_doctor(fix).await,
        Commands::Service { action } => service::handle_service_command(&action),
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
    // Check if running as OS service first — delegate if installed and running.
    if service::is_service_installed() {
        let status = service::get_service_status();
        if status.running {
            println!("Stopping via system service...");
            let result = service::stop_service();
            if result.success {
                println!("{}", result.message);
            } else {
                eprintln!("{}", result.message);
                std::process::exit(1);
            }
            return Ok(());
        }
    }

    // Fall back to PID file / SIGTERM for direct daemon mode.
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
    // Check if running as OS service first — delegate if installed (running or enabled).
    if service::is_service_installed() {
        let status = service::get_service_status();
        if status.running || status.enabled {
            println!("Restarting via system service...");
            let result = service::restart_service();
            if result.success {
                println!("{}", result.message);
            } else {
                eprintln!("{}", result.message);
                std::process::exit(1);
            }
            return Ok(());
        }
    }

    // Fall back to stopping existing direct-mode daemon then starting fresh.
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

    // Check daemon running state — service layer takes priority.
    println!("Daemon:");
    let mut daemon_running = false;

    if service::is_service_installed() {
        let svc_status = service::get_service_status();
        if svc_status.running {
            println!("  Status: Running (via system service)");
            daemon_running = true;
        } else if pid_file.exists() {
            if let Ok(pid_str) = fs::read_to_string(&pid_file) {
                if let Ok(pid) = pid_str.trim().parse::<i32>() {
                    if is_process_running(pid) {
                        println!("  Status: Running (PID {pid})");
                        daemon_running = true;
                    } else {
                        println!("  Status: Not running (stale PID file)");
                    }
                }
            }
        } else {
            println!("  Status: Not running");
        }
    } else if pid_file.exists() {
        if let Ok(pid_str) = fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                if is_process_running(pid) {
                    println!("  Status: Running (PID {pid})");
                    daemon_running = true;
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
    } else if daemon_running {
        println!("  Socket: Missing (expected: {})", socket_file.display());
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

    // Hook status
    let _ = installer::print_hook_status();

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
