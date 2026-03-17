//! Service Manager — systemd (Linux) and launchd (macOS) service management.
//!
//! Ported from `src/service/manager.ts`.

pub mod env;
mod launchd;
mod systemd;

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config;

/// Service management actions (moved here from main.rs so the lib crate can resolve it).
#[derive(clap::Subcommand, Clone)]
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

const SERVICE_NAME: &str = "claude-telegram-mirror";

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn home_dir() -> PathBuf {
    config::home_dir()
}

fn systemd_user_dir() -> PathBuf {
    home_dir().join(".config").join("systemd").join("user")
}

fn systemd_service_file() -> PathBuf {
    systemd_user_dir().join(format!("{SERVICE_NAME}.service"))
}

fn launchd_dir() -> PathBuf {
    home_dir().join("Library").join("LaunchAgents")
}

fn launchd_plist() -> PathBuf {
    launchd_dir().join(format!("com.claude.{SERVICE_NAME}.plist"))
}

fn env_file_path() -> PathBuf {
    home_dir().join(".telegram-env")
}

fn systemd_env_file_path() -> PathBuf {
    home_dir().join(".config").join(SERVICE_NAME).join("env")
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

fn is_linux() -> bool {
    cfg!(target_os = "linux")
}

fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

fn has_systemd() -> bool {
    if !is_linux() {
        return false;
    }
    Command::new("systemctl")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Binary path helper
// ---------------------------------------------------------------------------

fn ctm_binary_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ctm"))
}

// ---------------------------------------------------------------------------
// XML escaping for plist
// ---------------------------------------------------------------------------

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub struct ServiceResult {
    pub success: bool,
    pub message: String,
}

pub struct ServiceStatus {
    pub running: bool,
    pub enabled: bool,
    pub info: String,
}

/// Re-export parse_env_file for external consumers.
pub use env::parse_env_file;

/// Check if the service is installed (service file / plist exists).
pub fn is_service_installed() -> bool {
    if has_systemd() {
        systemd_service_file().exists()
    } else if is_macos() {
        launchd_plist().exists()
    } else {
        false
    }
}

pub fn install_service() -> ServiceResult {
    if !env_file_path().exists() {
        let env = env_file_path();
        return ServiceResult {
            success: false,
            message: format!(
                "Environment file not found: {}\n\nCreate it with:\ncat > {} << 'EOF'\nTELEGRAM_BOT_TOKEN=your-token-here\nTELEGRAM_CHAT_ID=your-chat-id\nTELEGRAM_MIRROR=true\nEOF",
                env.display(),
                env.display(),
            ),
        };
    }

    if has_systemd() {
        systemd::install_systemd_service()
    } else if is_macos() {
        launchd::install_launchd_service()
    } else {
        ServiceResult {
            success: false,
            message:
                "Unsupported platform. Only Linux (systemd) and macOS (launchd) are supported."
                    .into(),
        }
    }
}

pub fn uninstall_service() -> ServiceResult {
    if has_systemd() {
        systemd::uninstall_systemd_service()
    } else if is_macos() {
        launchd::uninstall_launchd_service()
    } else {
        ServiceResult {
            success: false,
            message: "Unsupported platform.".into(),
        }
    }
}

pub fn start_service() -> ServiceResult {
    if has_systemd() {
        systemd::start_systemd_service()
    } else if is_macos() {
        launchd::start_launchd_service()
    } else {
        ServiceResult {
            success: false,
            message: "Unsupported platform.".into(),
        }
    }
}

pub fn stop_service() -> ServiceResult {
    if has_systemd() {
        systemd::stop_systemd_service()
    } else if is_macos() {
        launchd::stop_launchd_service()
    } else {
        ServiceResult {
            success: false,
            message: "Unsupported platform.".into(),
        }
    }
}

pub fn restart_service() -> ServiceResult {
    if has_systemd() {
        systemd::restart_systemd_service()
    } else if is_macos() {
        let r = stop_service();
        if !r.success {
            return r;
        }
        start_service()
    } else {
        ServiceResult {
            success: false,
            message: "Unsupported platform.".into(),
        }
    }
}

pub fn get_service_status() -> ServiceStatus {
    if has_systemd() {
        systemd::get_systemd_status()
    } else if is_macos() {
        launchd::get_launchd_status()
    } else {
        ServiceStatus {
            running: false,
            enabled: false,
            info: "Unsupported platform".into(),
        }
    }
}

/// Handle the `ctm service <action>` CLI command.
pub fn handle_service_command(action: &ServiceAction) -> anyhow::Result<()> {
    let result = match action {
        ServiceAction::Install => {
            println!("Installing service...\n");
            let r = install_service();
            println!("{}", r.message);
            r
        }
        ServiceAction::Uninstall => {
            println!("Uninstalling service...\n");
            let r = uninstall_service();
            println!("{}", r.message);
            r
        }
        ServiceAction::Start => {
            let r = start_service();
            println!("{}", r.message);
            r
        }
        ServiceAction::Stop => {
            let r = stop_service();
            println!("{}", r.message);
            r
        }
        ServiceAction::Restart => {
            let r = restart_service();
            println!("{}", r.message);
            r
        }
        ServiceAction::Status => {
            let s = get_service_status();
            println!("\nService Status\n");
            println!("  Running: {}", if s.running { "Yes" } else { "No" });
            println!("  Enabled: {}", if s.enabled { "Yes" } else { "No" });
            println!("  Info:    {}", s.info);
            println!();
            return Ok(());
        }
    };

    if !result.success {
        std::process::exit(1);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_xml() {
        assert_eq!(
            escape_xml("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
    }

    #[test]
    fn test_service_status_unsupported() {
        // On CI (likely Linux without systemd user session), this should still work
        let status = get_service_status();
        // Just check it doesn't panic
        assert!(!status.info.is_empty());
    }
}
