// Public API modules — some exports used in tests and future phases
#[allow(unused)]
mod config;
mod error;
#[allow(unused)]
mod formatting;
mod hook;
#[allow(unused)]
mod injector;
#[allow(unused)]
mod session;
#[allow(unused)]
mod socket;
#[allow(unused)]
mod summarize;
#[allow(unused)]
mod types;

use clap::{Parser, Subcommand};
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
    }
}
