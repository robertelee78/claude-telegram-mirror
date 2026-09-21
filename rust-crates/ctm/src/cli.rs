//! The `ctm` command-line surface (clap). Lives in its own module so both the binary
//! and the library see one definition — `shell.rs` needs the `clap::Command` to
//! generate completions, and the library builds without `main.rs`.

use clap::{CommandFactory, Parser, Subcommand};

pub use crate::service::ServiceAction;

#[derive(Parser)]
#[command(
    name = "ctm",
    about = "Claude Telegram Mirror — Bidirectional Claude Code <-> Telegram bridge",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
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
        /// Install even if a ctm hook already exists in another scope
        /// (by default a project install is skipped to avoid double-firing).
        #[arg(long)]
        force: bool,
    },

    /// Remove Claude Code hooks
    UninstallHooks {
        /// Remove from the current project's settings.json + settings.local.json
        /// instead of the global ~/.claude/settings.json.
        #[arg(short, long)]
        project: bool,
    },

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

    /// Print a shell completion script (bash, zsh, fish)
    Completions {
        #[arg(value_enum)]
        shell: crate::shell::Shell,
    },

    /// Install (or remove) PATH + tab-completion for your shell (ADR-017)
    ShellSetup {
        /// Remove ctm's managed block and completion files
        #[arg(long)]
        remove: bool,
    },

    /// Forward one Codex hook event to the daemon (ADR-016; run by Codex, not by you)
    #[command(hide = true)]
    CodexHook,

    /// Report that a Codex TUI exited (ADR-016; run by ctm's shell integration)
    /// ADR-021: reconcile the app-server's account with auth.json before a launch.
    #[command(hide = true)]
    CodexPreflight,
    #[command(hide = true)]
    CodexExited {
        /// Working directory the session was started in
        #[arg(long)]
        cwd: String,
    },

    /// Update ctm to the latest GitHub release (ADR-017)
    Update {
        /// Only report whether an update is available; change nothing
        #[arg(long)]
        check: bool,
        /// Restore the previously installed binary
        #[arg(long)]
        rollback: bool,
    },

    /// Manage systemd/launchd service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Toggle Telegram mirroring on/off
    Toggle {
        /// Force mirroring ON
        #[arg(long)]
        on: bool,
        /// Force mirroring OFF
        #[arg(long)]
        off: bool,
    },

    /// Prune stale Telegram forum topics (clear an accumulated backlog).
    ///
    /// Three modes:
    ///   --ledger              delete every topic in the persistent ledger whose Claude
    ///                         session is no longer alive (the surefire path for topics
    ///                         this build created).
    ///   --ids FILE            delete exactly the topic ids listed in FILE (one id per
    ///                         line). Pair with scripts/list_topics.py, which enumerates
    ///                         every existing topic via MTProto — the precise way to clear
    ///                         legacy orphans the Bot API cannot list.
    ///   --from N --to M       sweep a numeric topic-id range with deleteForumTopic — a
    ///                         blunt fallback for legacy orphans when you have no id list.
    ///                         Non-topic ids in the range are skipped harmlessly.
    ///
    /// All modes always skip the General topic (id 1) and any currently-active session's
    /// topic.
    PruneTopics {
        /// Ledger mode: prune all recorded topics whose session is dead.
        #[arg(long)]
        ledger: bool,
        /// Ids mode: file of topic ids to delete (one per line).
        #[arg(long, value_name = "FILE")]
        ids: Option<std::path::PathBuf>,
        /// Range mode: first topic id (inclusive).
        #[arg(long)]
        from: Option<i64>,
        /// Range mode: last topic id (inclusive).
        #[arg(long)]
        to: Option<i64>,
        /// Show what would be deleted without deleting anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

/// The full clap command tree, for completion generation.
pub fn cli_command() -> clap::Command {
    Cli::command()
}
