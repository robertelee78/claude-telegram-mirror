//! Doctor — diagnostic checks with `--fix` auto-remediation.
//!
//! Ported from `src/service/doctor.ts`.

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;

use crate::colors::{bold, cyan, gray, green, red, yellow};
use crate::config;

// ---------------------------------------------------------------------------
// Check result
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CheckResult {
    name: String,
    status: CheckStatus,
    message: String,
    details: Option<String>,
    fixed: bool,
    fix_message: Option<String>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckResult {
    fn pass(name: &str, message: &str) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Pass,
            message: message.into(),
            details: None,
            fixed: false,
            fix_message: None,
        }
    }

    fn warn(name: &str, message: &str) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warn,
            message: message.into(),
            details: None,
            fixed: false,
            fix_message: None,
        }
    }

    fn fail(name: &str, message: &str) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Fail,
            message: message.into(),
            details: None,
            fixed: false,
            fix_message: None,
        }
    }

    fn with_details(mut self, details: &str) -> Self {
        self.details = Some(details.into());
        self
    }

    fn into_fixed(mut self, fix_message: &str) -> Self {
        self.status = CheckStatus::Pass;
        self.fixed = true;
        self.fix_message = Some(fix_message.into());
        self
    }
}

fn print_result(result: &CheckResult) {
    let icon = match result.status {
        CheckStatus::Pass => green("OK"),
        CheckStatus::Warn => yellow("WARN"),
        CheckStatus::Fail => red("FAIL"),
    };

    let msg_colored = match result.status {
        CheckStatus::Pass => green(&result.message),
        CheckStatus::Warn => yellow(&result.message),
        CheckStatus::Fail => red(&result.message),
    };

    let fix_suffix = if result.fixed {
        format!(
            " {} FIXED ({})",
            green("->"),
            green(result.fix_message.as_deref().unwrap_or(""))
        )
    } else {
        String::new()
    };

    println!(
        "  {icon}: {name}: {msg}{fix}",
        name = bold(&result.name),
        msg = msg_colored,
        fix = fix_suffix
    );

    if let Some(details) = &result.details {
        println!("    {}", gray(details));
    }
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

fn socket_path() -> PathBuf {
    config_dir().join("bridge.sock")
}

fn pid_path() -> PathBuf {
    config_dir().join("bridge.pid")
}

fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

fn check_binary_version() -> CheckResult {
    let version = env!("CARGO_PKG_VERSION");
    CheckResult::pass("Binary", &format!("ctm v{version}"))
}

fn check_config_dir(fix: bool) -> CheckResult {
    let dir = config_dir();

    if !dir.exists() {
        if fix {
            match config::ensure_config_dir(&dir) {
                Ok(()) => {
                    return CheckResult::pass("Config Directory", "Missing config directory")
                        .into_fixed(&format!("created {}", dir.display()));
                }
                Err(e) => {
                    return CheckResult::fail(
                        "Config Directory",
                        "Missing config directory (auto-fix failed)",
                    )
                    .with_details(&e.to_string());
                }
            }
        }
        return CheckResult::warn("Config Directory", "Config directory does not exist")
            .with_details(&format!("Expected: {}", dir.display()));
    }

    match fs::metadata(&dir) {
        Ok(meta) => {
            let mode = meta.permissions().mode() & 0o777;
            if mode != 0o700 {
                let octal = format!("0o{:o}", mode);
                if fix {
                    match fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)) {
                        Ok(()) => {
                            return CheckResult::pass(
                                "Config Directory",
                                &format!("Permissions were {octal}"),
                            )
                            .into_fixed("set to 0o700");
                        }
                        Err(e) => {
                            return CheckResult::warn(
                                "Config Directory",
                                &format!("Insecure permissions ({octal}) -- auto-fix failed"),
                            )
                            .with_details(&e.to_string());
                        }
                    }
                }
                return CheckResult::warn(
                    "Config Directory",
                    &format!("Insecure permissions ({octal})"),
                )
                .with_details(&format!("Expected 0o700, got {octal}"));
            }
            CheckResult::pass(
                "Config Directory",
                "Exists with correct permissions (0o700)",
            )
        }
        Err(e) => CheckResult::fail("Config Directory", "Error checking config directory")
            .with_details(&e.to_string()),
    }
}

fn check_env_vars() -> CheckResult {
    let has_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some();
    let has_chat = std::env::var("TELEGRAM_CHAT_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some();

    // Also check config file
    let cfg_ok = config_file().exists() && {
        fs::read_to_string(config_file())
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .map(|v| {
                v.get("botToken")
                    .and_then(|t| t.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
                    && v.get("chatId")
                        .and_then(|c| c.as_i64())
                        .map(|i| i != 0)
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    };

    if has_token && has_chat {
        CheckResult::pass("Configuration", "Using environment variables")
    } else if cfg_ok {
        CheckResult::pass("Configuration", "Valid config file")
            .with_details(&config_file().display().to_string())
    } else {
        CheckResult::fail("Configuration", "No config file or env vars found")
            .with_details("Run: ctm setup")
    }
}

fn check_hooks(fix: bool) -> CheckResult {
    let path = claude_settings_path();
    if !path.exists() {
        let result = CheckResult::warn("Claude Code Hooks", "Claude settings not found")
            .with_details("Run: ctm install-hooks");
        if fix {
            println!(
                "    {}",
                gray("-> Suggestion: Run `ctm install-hooks` to fix")
            );
        }
        return result;
    }

    match fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
    {
        Some(settings) => {
            let hooks = settings.get("hooks");
            let check_types = [
                "PreToolUse",
                "PostToolUse",
                "Notification",
                "Stop",
                "UserPromptSubmit",
                "PreCompact",
            ];
            let installed = check_types
                .iter()
                .filter(|&&ht| {
                    hooks
                        .and_then(|h| h.get(ht))
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter().any(|item| {
                                // New format
                                item.get("hooks")
                                    .and_then(|h| h.as_array())
                                    .map(|hooks| {
                                        hooks.iter().any(|h| {
                                            h.get("command")
                                                .and_then(|c| c.as_str())
                                                .map(|c| {
                                                    c.contains("telegram-hook")
                                                        || c.contains("ctm")
                                                })
                                                .unwrap_or(false)
                                        })
                                    })
                                    .unwrap_or(false)
                                    // Old format
                                    || item
                                        .get("command")
                                        .and_then(|c| c.as_str())
                                        .map(|c| {
                                            c.contains("telegram-hook") || c.contains("ctm")
                                        })
                                        .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
                })
                .count();

            if installed == check_types.len() {
                CheckResult::pass("Claude Code Hooks", "All hooks installed")
            } else if installed > 0 {
                // Detect legacy 3-hook installs (PreToolUse + PostToolUse + Notification only)
                let legacy_hooks = ["PreToolUse", "PostToolUse", "Notification"];
                let is_legacy = installed == 3
                    && legacy_hooks.iter().all(|&ht| {
                        hooks
                            .and_then(|h| h.get(ht))
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter().any(|item| {
                                    item.get("hooks")
                                        .and_then(|h| h.as_array())
                                        .map(|hooks| {
                                            hooks.iter().any(|h| {
                                                h.get("command")
                                                    .and_then(|c| c.as_str())
                                                    .map(|c| {
                                                        c.contains("telegram-hook")
                                                            || c.contains("ctm")
                                                    })
                                                    .unwrap_or(false)
                                            })
                                        })
                                        .unwrap_or(false)
                                        || item
                                            .get("command")
                                            .and_then(|c| c.as_str())
                                            .map(|c| {
                                                c.contains("telegram-hook") || c.contains("ctm")
                                            })
                                            .unwrap_or(false)
                                })
                            })
                            .unwrap_or(false)
                    });

                let msg = if is_legacy {
                    format!(
                        "{installed}/{} hooks installed (legacy \u{2014} run `ctm install-hooks` to update)",
                        check_types.len()
                    )
                } else {
                    format!("{installed}/{} hooks installed", check_types.len())
                };
                let r = CheckResult::warn("Claude Code Hooks", &msg)
                    .with_details("Run: ctm install-hooks");
                if fix {
                    println!(
                        "    {}",
                        gray("-> Suggestion: Run `ctm install-hooks` to fix")
                    );
                }
                r
            } else {
                let r = CheckResult::warn("Claude Code Hooks", "No hooks installed")
                    .with_details("Run: ctm install-hooks");
                if fix {
                    println!(
                        "    {}",
                        gray("-> Suggestion: Run `ctm install-hooks` to fix")
                    );
                }
                r
            }
        }
        None => CheckResult::fail("Claude Code Hooks", "Error reading Claude settings"),
    }
}

fn check_socket(fix: bool) -> CheckResult {
    let sock = socket_path();
    if !sock.exists() {
        return CheckResult::warn("Bridge Socket", "Socket not found (daemon not running?)")
            .with_details("Run: ctm start");
    }

    let meta = match fs::symlink_metadata(&sock) {
        Ok(m) => m,
        Err(e) => {
            return CheckResult::fail("Bridge Socket", "Error checking socket")
                .with_details(&e.to_string());
        }
    };

    let is_socket = meta.file_type().is_socket();

    if is_socket {
        // Check if daemon is alive via PID file
        let pid = pid_path();
        if pid.exists() {
            if let Ok(pid_str) = fs::read_to_string(&pid) {
                if let Ok(pid_num) = pid_str.trim().parse::<i32>() {
                    let alive =
                        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid_num), None).is_ok();

                    if alive {
                        return CheckResult::pass("Bridge Socket", "Socket exists")
                            .with_details(&sock.display().to_string());
                    }

                    // Stale socket
                    if fix {
                        if fs::remove_file(&sock).is_ok() {
                            return CheckResult::pass(
                                "Bridge Socket",
                                "Stale socket file (daemon not running)",
                            )
                            .into_fixed("removed stale socket file");
                        }
                        return CheckResult::warn(
                            "Bridge Socket",
                            "Stale socket file -- auto-fix failed",
                        );
                    }
                    return CheckResult::warn(
                        "Bridge Socket",
                        "Stale socket file (daemon not running)",
                    )
                    .with_details(&sock.display().to_string());
                }
            }
        }
        CheckResult::pass("Bridge Socket", "Socket exists")
            .with_details(&sock.display().to_string())
    } else {
        // Not a socket — corrupt/stale
        if fix {
            if fs::remove_file(&sock).is_ok() {
                return CheckResult::pass("Bridge Socket", "Path exists but is not a socket")
                    .into_fixed("removed invalid socket file");
            }
            return CheckResult::fail(
                "Bridge Socket",
                "Path exists but is not a socket -- auto-fix failed",
            );
        }
        CheckResult::fail("Bridge Socket", "Path exists but is not a socket")
    }
}

fn check_tmux() -> CheckResult {
    let has_tmux = Command::new("which")
        .arg("tmux")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !has_tmux {
        return CheckResult::warn("Tmux", "Not installed")
            .with_details("Install tmux for Telegram -> CLI input injection");
    }

    // Check for TMUX env
    if std::env::var("TMUX").is_ok() {
        return CheckResult::pass("Tmux", "Available and active")
            .with_details("Input injection will work");
    }

    // Check sessions
    match Command::new("tmux")
        .args(["list-sessions"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            let sessions = String::from_utf8_lossy(&output.stdout);
            let count = sessions.lines().count();
            CheckResult::pass(
                "Tmux",
                &format!(
                    "Available ({count} session{})",
                    if count == 1 { "" } else { "s" }
                ),
            )
            .with_details("Input injection available")
        }
        _ => CheckResult::warn("Tmux", "Available but no sessions")
            .with_details("Start Claude Code in tmux for input injection"),
    }
}

fn check_service() -> CheckResult {
    let service = crate::service::get_service_status();

    if service.info.contains("not installed") {
        return CheckResult::warn("Service", "Service not installed")
            .with_details("Run: ctm service install");
    }

    if service.running {
        CheckResult::pass("Service", "Running")
    } else {
        CheckResult::warn("Service", "Installed but not running")
            .with_details("Run: ctm service start")
    }
}

async fn check_telegram() -> CheckResult {
    // Get token from env or config
    let token = std::env::var("TELEGRAM_BOT_TOKEN").ok().or_else(|| {
        config_file()
            .exists()
            .then(|| {
                fs::read_to_string(config_file())
                    .ok()
                    .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
                    .and_then(|v| v.get("botToken").and_then(|t| t.as_str()).map(String::from))
            })
            .flatten()
    });

    let token = match token {
        Some(t) if !t.is_empty() => t,
        _ => {
            return CheckResult::fail("Telegram API", "No bot token configured");
        }
    };

    match reqwest::Client::new()
        .get(format!("https://api.telegram.org/bot{token}/getMe"))
        .send()
        .await
    {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(data) => {
                if data["ok"].as_bool() == Some(true) {
                    let username = data["result"]["username"].as_str().unwrap_or("unknown");
                    CheckResult::pass("Telegram API", &format!("Connected as @{username}"))
                } else {
                    CheckResult::fail("Telegram API", "Invalid bot token")
                }
            }
            Err(e) => CheckResult::fail("Telegram API", "Error parsing response")
                .with_details(&e.to_string()),
        },
        Err(e) => CheckResult::fail("Telegram API", "Network error").with_details(&e.to_string()),
    }
}

fn check_pid_file(fix: bool) -> CheckResult {
    let pid = pid_path();
    if !pid.exists() {
        return CheckResult::pass("PID File", "No stale PID file");
    }

    match fs::read_to_string(&pid) {
        Ok(pid_str) => {
            if let Ok(pid_num) = pid_str.trim().parse::<i32>() {
                let alive =
                    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid_num), None).is_ok();

                if alive {
                    return CheckResult::pass(
                        "PID File",
                        &format!("Daemon running (PID {pid_num})"),
                    );
                }
            }

            // Stale
            if fix {
                if fs::remove_file(&pid).is_ok() {
                    return CheckResult::pass("PID File", "Stale PID file detected")
                        .into_fixed("removed stale PID file");
                }
                return CheckResult::warn("PID File", "Stale PID file -- auto-fix failed");
            }
            CheckResult::warn("PID File", "Stale PID file (process not running)")
                .with_details(&pid.display().to_string())
        }
        Err(e) => {
            CheckResult::fail("PID File", "Error reading PID file").with_details(&e.to_string())
        }
    }
}

fn check_database() -> CheckResult {
    let db_path = config_dir().join("sessions.db");
    if !db_path.exists() {
        return CheckResult::warn("Database", "Database file not found")
            .with_details("Will be created on first daemon start");
    }

    match rusqlite::Connection::open(&db_path) {
        Ok(conn) => {
            // Try to query session count
            let count: Result<i64, _> =
                conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0));
            match count {
                Ok(n) => CheckResult::pass(
                    "Database",
                    &format!("OK ({n} session{})", if n == 1 { "" } else { "s" }),
                ),
                Err(_) => {
                    // Table might not exist yet
                    CheckResult::pass("Database", "Database accessible (no sessions table yet)")
                }
            }
        }
        Err(e) => {
            CheckResult::fail("Database", "Cannot open database").with_details(&e.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

pub async fn run_doctor(fix: bool) -> anyhow::Result<()> {
    println!();
    println!(
        "{}",
        cyan("================================================================")
    );
    println!("{}", bold("  Claude Telegram Mirror - Diagnostics"));
    println!(
        "{}",
        cyan("================================================================")
    );
    println!();

    if fix {
        println!(
            "  {}",
            yellow("Auto-fix mode enabled. Safe issues will be remediated automatically.")
        );
        println!();
    }

    // System info
    println!("{}", gray(&"-".repeat(60)));
    println!("{}", bold("System Information"));
    println!("{}", gray(&"-".repeat(60)));

    let hostname = std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let os_name = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "unknown"
    };
    let arch_name = std::env::consts::ARCH;
    println!("  Hostname: {}", cyan(&hostname));
    println!("  Platform: {}", cyan(&format!("{os_name} {arch_name}")));
    println!(
        "  Binary:   {}",
        cyan(&format!("ctm v{}", env!("CARGO_PKG_VERSION")))
    );
    println!();

    // Run checks
    println!("{}", gray(&"-".repeat(60)));
    println!("{}", bold("Checks"));
    println!("{}", gray(&"-".repeat(60)));
    println!();

    let mut checks = Vec::new();

    // [1/9] Binary
    let c = check_binary_version();
    print!("[1/10] ");
    print_result(&c);
    checks.push(c);

    // [2/9] Config directory
    let c = check_config_dir(fix);
    print!("[2/10] ");
    print_result(&c);
    checks.push(c);

    // [3/9] Configuration (env vars / config file)
    let c = check_env_vars();
    print!("[3/10] ");
    print_result(&c);
    checks.push(c);

    // [4/9] Hooks
    let c = check_hooks(fix);
    print!("[4/10] ");
    print_result(&c);
    checks.push(c);

    // [5/9] PID file
    let c = check_pid_file(fix);
    print!("[5/10] ");
    print_result(&c);
    checks.push(c);

    // [6/9] Socket
    let c = check_socket(fix);
    print!("[6/10] ");
    print_result(&c);
    checks.push(c);

    // [7/10] Tmux
    let c = check_tmux();
    print!("[7/10] ");
    print_result(&c);
    checks.push(c);

    // [8/10] Service
    let c = check_service();
    print!("[8/10] ");
    print_result(&c);
    checks.push(c);

    // [9/10] Telegram API
    let c = check_telegram().await;
    print!("[9/10] ");
    print_result(&c);
    checks.push(c);

    // [10/10] Database
    let c = check_database();
    print!("[10/10] ");
    print_result(&c);
    checks.push(c);

    println!();

    // Summary
    let passed = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Pass)
        .count();
    let warnings = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
    let failed = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .count();
    let fixed_count = checks.iter().filter(|c| c.fixed).count();
    let issues_found = warnings + failed;
    let require_manual = issues_found.saturating_sub(fixed_count);

    println!("{}", gray(&"-".repeat(60)));
    println!("{}", bold("Summary"));
    println!("{}", gray(&"-".repeat(60)));

    if fix {
        println!(
            "  {issues_found} issue{} found, {} auto-fixed, {} require manual action",
            if issues_found != 1 { "s" } else { "" },
            green(&fixed_count.to_string()),
            if require_manual > 0 {
                yellow(&require_manual.to_string())
            } else {
                "0".to_string()
            },
        );
    }

    if failed == 0 && warnings == 0 {
        println!("  {}", green("All checks passed! Everything looks good."));
    } else {
        println!(
            "  {} passed, {} warnings, {} failed",
            green(&passed.to_string()),
            yellow(&warnings.to_string()),
            red(&failed.to_string()),
        );

        if failed > 0 {
            println!();
            println!("  {}", red("Some checks failed. Review the errors above."));
        }
    }

    println!();

    // Suggested actions
    if failed > 0 || warnings > 0 {
        println!("{}", gray(&"-".repeat(60)));
        println!("{}", bold("Suggested Actions"));
        println!("{}", gray(&"-".repeat(60)));

        let config_check = checks.iter().find(|c| c.name == "Configuration");
        if config_check.map(|c| c.status) == Some(CheckStatus::Fail) {
            println!("  {}           Run interactive setup", cyan("ctm setup"));
        }

        let hooks_check = checks.iter().find(|c| c.name == "Claude Code Hooks");
        if hooks_check
            .map(|c| c.status != CheckStatus::Pass && !c.fixed)
            .unwrap_or(false)
        {
            println!(
                "  {}   Install Claude Code hooks",
                cyan("ctm install-hooks")
            );
        }

        let service_check = checks.iter().find(|c| c.name == "Service");
        if service_check
            .map(|c| c.status != CheckStatus::Pass)
            .unwrap_or(false)
        {
            println!("  {} Install system service", cyan("ctm service install"));
            println!("  {}   Start the service", cyan("ctm service start"));
        }

        let socket_check = checks.iter().find(|c| c.name == "Bridge Socket");
        if socket_check
            .map(|c| c.status != CheckStatus::Pass && !c.fixed)
            .unwrap_or(false)
        {
            println!("  {}           Start the daemon", cyan("ctm start"));
        }

        println!();
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
    fn test_check_binary_version() {
        let result = check_binary_version();
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("ctm v"));
    }

    #[test]
    fn test_check_config_dir_creates_when_fix() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sub").join("nested");
        // Doesn't exist yet
        assert!(!target.exists());
    }

    #[test]
    fn test_check_tmux_does_not_panic() {
        // Just ensure the check doesn't panic
        let _result = check_tmux();
    }

    #[test]
    fn test_check_pid_file_no_file() {
        // When there's no PID file, should pass
        let result = check_pid_file(false);
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[test]
    fn test_check_database_missing() {
        let result = check_database();
        // In test env, database may or may not exist
        assert!(result.status == CheckStatus::Pass || result.status == CheckStatus::Warn);
    }

    #[test]
    fn test_check_result_builder() {
        let r = CheckResult::fail("Test", "message")
            .with_details("details")
            .into_fixed("fixed it");
        assert_eq!(r.status, CheckStatus::Pass); // into_fixed promotes to Pass
        assert!(r.fixed);
        assert_eq!(r.fix_message.as_deref(), Some("fixed it"));
        assert_eq!(r.details.as_deref(), Some("details"));
    }
}
