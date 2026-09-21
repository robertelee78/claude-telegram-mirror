//! Service Manager — systemd (Linux) and launchd (macOS) service management.
//!
//! ADR-019: every mutating operation is act → observe → report. The `*_state`
//! modules are the only readers of the managers; the ops modules never derive a
//! result from a command's exit status.

pub mod env;
mod launchd;
pub mod launchd_state;
mod spec;
mod systemd;
pub mod systemd_state;

pub use spec::ServiceSpec;

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

fn launchd_dir() -> PathBuf {
    home_dir().join("Library").join("LaunchAgents")
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

fn unsupported() -> ServiceResult {
    ServiceResult {
        success: false,
        message: "Unsupported platform. Only Linux (systemd) and macOS (launchd) are supported."
            .into(),
    }
}

// ---- generic operations on any spec (what the e2e test drives)

pub fn install_with(spec: &ServiceSpec) -> ServiceResult {
    if has_systemd() {
        systemd::install_with(spec)
    } else if is_macos() {
        launchd::install_with(spec)
    } else {
        unsupported()
    }
}

pub fn uninstall_with(spec: &ServiceSpec) -> ServiceResult {
    if has_systemd() {
        systemd::uninstall_with(spec)
    } else if is_macos() {
        launchd::uninstall_with(spec)
    } else {
        unsupported()
    }
}

pub fn start_with(spec: &ServiceSpec) -> ServiceResult {
    if has_systemd() {
        systemd::start_with(spec)
    } else if is_macos() {
        launchd::start_with(spec)
    } else {
        unsupported()
    }
}

pub fn stop_with(spec: &ServiceSpec) -> ServiceResult {
    if has_systemd() {
        systemd::stop_with(spec)
    } else if is_macos() {
        launchd::stop_with(spec)
    } else {
        unsupported()
    }
}

pub fn restart_with(spec: &ServiceSpec) -> ServiceResult {
    if has_systemd() {
        systemd::restart_with(spec)
    } else if is_macos() {
        launchd::restart_with(spec)
    } else {
        unsupported()
    }
}

pub fn status_with(spec: &ServiceSpec) -> ServiceStatus {
    if has_systemd() {
        systemd::status_with(spec)
    } else if is_macos() {
        launchd::status_with(spec)
    } else {
        ServiceStatus {
            running: false,
            enabled: false,
            info: "Unsupported platform".into(),
        }
    }
}

/// The PID the manager reports for the spec's process, if it is running.
pub fn pid_with(spec: &ServiceSpec) -> Option<u32> {
    if has_systemd() {
        match systemd_state::observe(&spec.systemd_unit()) {
            systemd_state::Observation::Unit(u) if u.running() => u.main_pid,
            _ => None,
        }
    } else if is_macos() {
        launchd_state::observe(&spec.launchd_target())
            .pid
            .and_then(|p| u32::try_from(p).ok())
    } else {
        None
    }
}

/// The program the manager has loaded for the spec (not the file on disk).
pub fn program_with(spec: &ServiceSpec) -> Option<PathBuf> {
    if has_systemd() {
        match systemd_state::observe(&spec.systemd_unit()) {
            systemd_state::Observation::Unit(u) => u.exec_path,
            _ => None,
        }
    } else if is_macos() {
        launchd_state::observe(&spec.launchd_target()).program
    } else {
        None
    }
}

// ---- the ctm daemon

/// ADR-017: the binary the installed service unit runs, if a unit exists.
/// launchd: first `<string>` under `ProgramArguments`; systemd: `ExecStart=<bin> start`.
pub fn service_binary_path() -> Option<PathBuf> {
    let spec = ServiceSpec::ctm();
    if has_systemd() {
        let text = std::fs::read_to_string(spec.systemd_unit_path()).ok()?;
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with("ExecStart="))?;
        let rest = line.trim_start().trim_start_matches("ExecStart=").trim();
        return rest.split_whitespace().next().map(PathBuf::from);
    }
    if is_macos() {
        let text = std::fs::read_to_string(spec.launchd_plist_path()).ok()?;
        let after = text.split("<key>ProgramArguments</key>").nth(1)?;
        let start = after.find("<string>")? + "<string>".len();
        let end = after[start..].find("</string>")? + start;
        return Some(PathBuf::from(after[start..end].trim()));
    }
    None
}

/// Is the ctm unit definition on disk? (Distinct from "the manager knows it".)
pub fn is_service_installed() -> bool {
    let spec = ServiceSpec::ctm();
    if has_systemd() {
        spec.systemd_unit_path().exists()
    } else if is_macos() {
        spec.launchd_plist_path().exists()
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
        // The unit's EnvironmentFile is generated from ~/.telegram-env on every install.
        match env::create_systemd_env_file() {
            Ok(f) => println!("  Created env file: {}", f.display()),
            Err(e) => {
                return ServiceResult {
                    success: false,
                    message: format!("Failed to create env file: {e}"),
                }
            }
        }
    }
    install_with(&ServiceSpec::ctm())
}

pub fn uninstall_service() -> ServiceResult {
    uninstall_with(&ServiceSpec::ctm())
}

pub fn start_service() -> ServiceResult {
    // `start_with` installs first when the unit is missing; on Linux that needs the
    // env file the ctm-specific installer generates.
    if has_systemd() && !is_service_installed() {
        let installed = install_service();
        if !installed.success {
            return ServiceResult {
                success: false,
                message: format!(
                    "Service is not installed, and installing it failed.\n{}",
                    installed.message
                ),
            };
        }
        println!("Service was not installed; installed it first.");
    }
    start_with(&ServiceSpec::ctm())
}

pub fn stop_service() -> ServiceResult {
    stop_with(&ServiceSpec::ctm())
}

pub fn restart_service() -> ServiceResult {
    restart_with(&ServiceSpec::ctm())
}

pub fn get_service_status() -> ServiceStatus {
    status_with(&ServiceSpec::ctm())
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
            let spec = ServiceSpec::ctm();
            let s = status_with(&spec);
            println!("\nService Status\n");
            match pid_with(&spec) {
                Some(pid) => println!("  Running: Yes (pid {pid})"),
                None => println!("  Running: No"),
            }
            println!("  Enabled: {}", if s.enabled { "Yes" } else { "No" });
            if let Some(p) = program_with(&spec) {
                // What the manager will exec, which after a move may differ from
                // the unit on disk until the next restart (ADR-019).
                println!("  Program: {}", p.display());
            }
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
    fn status_never_panics_wherever_it_runs() {
        let status = get_service_status();
        assert!(!status.info.is_empty());
    }

    /// ADR-019 contract: no command result in this module tree is discarded. A
    /// discarded result is how "Service installed" got printed on a box with no
    /// user manager and how a restart left the old binary running.
    #[test]
    fn no_service_command_result_is_discarded() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/service");
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            for (n, line) in text.lines().enumerate() {
                let l = line.trim_start();
                assert!(
                    !(l.starts_with("let _ = Command::new")
                        || l.starts_with("let _ = std::process::Command::new")
                        || l.starts_with("let _ = launchctl(")
                        || l.starts_with("let _ = systemctl(")),
                    "{}:{}: discards a command result: {line}",
                    path.display(),
                    n + 1
                );
            }
        }
    }
}
