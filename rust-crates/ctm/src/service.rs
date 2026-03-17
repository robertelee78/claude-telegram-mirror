//! Service Manager — systemd (Linux) and launchd (macOS) service management.
//!
//! Ported from `src/service/manager.ts`.

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
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
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
// Env file parsing
// ---------------------------------------------------------------------------

/// Parse an environment file into key-value pairs.
///
/// Handles: `export KEY=value`, `KEY="value"`, `KEY='value'`, inline `# comments`,
/// blank lines, and comment-only lines.
pub fn parse_env_file(path: &Path) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vars,
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Remove `export ` prefix
        let clean = if let Some(rest) = trimmed.strip_prefix("export ") {
            rest.trim()
        } else if let Some(rest) = trimmed.strip_prefix("export\t") {
            rest.trim()
        } else {
            trimmed
        };

        // Find `=`
        let eq_index = match clean.find('=') {
            Some(i) => i,
            None => continue,
        };

        let key = clean[..eq_index].trim();
        if key.is_empty() {
            continue;
        }

        let mut value = clean[eq_index + 1..].trim().to_string();

        // Remove inline comments (but not those inside quotes)
        value = strip_inline_comment(&value);

        // Strip surrounding quotes
        if ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
            && value.len() >= 2
        {
            value = value[1..value.len() - 1].to_string();
        }

        vars.insert(key.to_string(), value);
    }

    vars
}

/// Remove an inline `# comment` that is NOT inside quotes.
fn strip_inline_comment(value: &str) -> String {
    let mut in_single = false;
    let mut in_double = false;
    for (i, ch) in value.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => {
                return value[..i].trim_end().to_string();
            }
            _ => {}
        }
    }
    value.to_string()
}

// ---------------------------------------------------------------------------
// Env file generation for systemd
// ---------------------------------------------------------------------------

fn create_systemd_env_file() -> anyhow::Result<PathBuf> {
    let env_vars = parse_env_file(&env_file_path());
    let config_dir = home_dir().join(".config").join(SERVICE_NAME);
    config::ensure_config_dir(&config_dir)?;

    let mut lines = vec!["# Auto-generated from ~/.telegram-env for systemd".to_string()];
    for (key, value) in &env_vars {
        if value.contains(' ') || value.contains('$') || value.contains('`') {
            lines.push(format!("{key}=\"{value}\""));
        } else {
            lines.push(format!("{key}={value}"));
        }
    }

    let dest = systemd_env_file_path();
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&dest, lines.join("\n") + "\n")?;
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;

    Ok(dest)
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
// Service file generators
// ---------------------------------------------------------------------------

fn generate_systemd_service() -> String {
    let binary = ctm_binary_path();
    let config_dir = home_dir().join(".config").join(SERVICE_NAME);
    let env_file = systemd_env_file_path();

    // Note (M2.7): WorkingDirectory uses %h (the user's home directory), which is the
    // appropriate working directory for a Rust binary installed to the system.  The
    // TypeScript implementation used the package directory because it required
    // node_modules relative resolution — that constraint does not apply here.
    // %h ensures the daemon always starts in a predictable, writable directory.
    format!(
        r#"[Unit]
Description=Claude Code Telegram Mirror Bridge
Documentation=https://github.com/robertelee78/claude-telegram-mirror
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=%h
ExecStart={binary} start
EnvironmentFile={env_file}

# Restart policy
Restart=on-failure
RestartSec=10s
StartLimitInterval=300s
StartLimitBurst=5

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier={SERVICE_NAME}

# Security hardening
NoNewPrivileges=true
PrivateTmp=false

# Allow writes to config directory
ReadWritePaths={config_dir}

[Install]
WantedBy=default.target
"#,
        binary = binary.display(),
        env_file = env_file.display(),
        config_dir = config_dir.display(),
    )
}

fn get_macos_path() -> String {
    let home = home_dir();
    let mut paths: Vec<String> = vec![
        "/usr/local/bin".into(),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
        format!("{}/.nvm/versions/node/*/bin", home.display()),
        "/opt/homebrew/bin".into(),
        "/usr/local/opt/node/bin".into(),
        format!("{}/.local/bin", home.display()),
    ];

    // Merge with current PATH
    if let Ok(current) = std::env::var("PATH") {
        for dir in current.split(':') {
            if !dir.is_empty() && !paths.contains(&dir.to_string()) {
                paths.push(dir.to_string());
            }
        }
    }

    paths.join(":")
}

fn generate_launchd_plist() -> String {
    let binary = ctm_binary_path();
    let home = home_dir();
    let config_dir = home.join(".config").join(SERVICE_NAME);
    let log_file = config_dir.join("daemon.log");
    let err_file = config_dir.join("daemon.err.log");

    let env_vars = parse_env_file(&env_file_path());

    let mut env_lines = Vec::new();
    // Essential env vars
    env_lines.push(format!(
        "        <key>HOME</key>\n        <string>{}</string>",
        escape_xml(&home.display().to_string())
    ));
    env_lines.push(format!(
        "        <key>PATH</key>\n        <string>{}</string>",
        escape_xml(&get_macos_path())
    ));
    env_lines.push("        <key>NODE_ENV</key>\n        <string>production</string>".to_string());

    // User-defined env vars from ~/.telegram-env
    for (key, value) in &env_vars {
        if key == "HOME" || key == "PATH" || key == "NODE_ENV" {
            continue;
        }
        env_lines.push(format!(
            "        <key>{}</key>\n        <string>{}</string>",
            escape_xml(key),
            escape_xml(value),
        ));
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.claude.{service_name}</string>

    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>start</string>
    </array>

    <key>WorkingDirectory</key>
    <string>{home_dir}</string>

    <key>EnvironmentVariables</key>
    <dict>
{env_block}
    </dict>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
        <key>Crashed</key>
        <true/>
    </dict>

    <key>ThrottleInterval</key>
    <integer>10</integer>

    <key>StandardOutPath</key>
    <string>{log_file}</string>

    <key>StandardErrorPath</key>
    <string>{err_file}</string>
</dict>
</plist>
"#,
        service_name = SERVICE_NAME,
        binary = escape_xml(&binary.display().to_string()),
        home_dir = escape_xml(&home.display().to_string()),
        env_block = env_lines.join("\n"),
        log_file = escape_xml(&log_file.display().to_string()),
        err_file = escape_xml(&err_file.display().to_string()),
    )
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub struct ServiceResult {
    pub success: bool,
    pub message: String,
}

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
        install_systemd_service()
    } else if is_macos() {
        install_launchd_service()
    } else {
        ServiceResult {
            success: false,
            message:
                "Unsupported platform. Only Linux (systemd) and macOS (launchd) are supported."
                    .into(),
        }
    }
}

fn install_systemd_service() -> ServiceResult {
    let env_result = create_systemd_env_file();
    let env_file = match env_result {
        Ok(f) => f,
        Err(e) => {
            return ServiceResult {
                success: false,
                message: format!("Failed to create env file: {e}"),
            };
        }
    };

    let sdir = systemd_user_dir();
    if !sdir.exists() {
        if let Err(e) = fs::create_dir_all(&sdir) {
            return ServiceResult {
                success: false,
                message: format!("Failed to create systemd dir: {e}"),
            };
        }
    }

    let service_path = systemd_service_file();
    let content = generate_systemd_service();
    if let Err(e) = fs::write(&service_path, content) {
        return ServiceResult {
            success: false,
            message: format!("Failed to write service file: {e}"),
        };
    }

    println!("  Created env file: {}", env_file.display());

    // Reload systemd
    // M5.4: Uses `.status()` which inherits stdout/stderr so the user sees
    // systemctl output. The uninstall path intentionally suppresses output
    // with Stdio::null() because it runs during cleanup where noise is unhelpful.
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    // Enable
    let _ = Command::new("systemctl")
        .args(["--user", "enable", &format!("{SERVICE_NAME}.service")])
        .status();

    ServiceResult {
        success: true,
        message: format!(
            "Service installed: {}\n\nCommands:\n  Start:   systemctl --user start {SERVICE_NAME}\n  Stop:    systemctl --user stop {SERVICE_NAME}\n  Status:  systemctl --user status {SERVICE_NAME}\n  Logs:    journalctl --user -u {SERVICE_NAME} -f\n\nTo run without being logged in:\n  sudo loginctl enable-linger $USER",
            service_path.display(),
        ),
    }
}

fn install_launchd_service() -> ServiceResult {
    let plist_dir = launchd_dir();
    if !plist_dir.exists() {
        if let Err(e) = fs::create_dir_all(&plist_dir) {
            return ServiceResult {
                success: false,
                message: format!("Failed to create LaunchAgents dir: {e}"),
            };
        }
    }

    // Ensure config dir for logs
    let config_dir = home_dir().join(".config").join(SERVICE_NAME);
    if let Err(e) = config::ensure_config_dir(&config_dir) {
        return ServiceResult {
            success: false,
            message: format!("Failed to ensure config dir: {e}"),
        };
    }

    let plist_path = launchd_plist();
    let content = generate_launchd_plist();
    if let Err(e) = fs::write(&plist_path, content) {
        return ServiceResult {
            success: false,
            message: format!("Failed to write plist file: {e}"),
        };
    }

    ServiceResult {
        success: true,
        message: format!(
            "Service installed: {plist}\n\nCommands:\n  Load & Start:  launchctl load {plist}\n  Start:         launchctl start com.claude.{SERVICE_NAME}\n  Stop:          launchctl stop com.claude.{SERVICE_NAME}\n  Unload:        launchctl unload {plist}\n  Logs:          tail -f ~/.config/{SERVICE_NAME}/daemon.log",
            plist = plist_path.display(),
        ),
    }
}

pub fn uninstall_service() -> ServiceResult {
    if has_systemd() {
        uninstall_systemd_service()
    } else if is_macos() {
        uninstall_launchd_service()
    } else {
        ServiceResult {
            success: false,
            message: "Unsupported platform.".into(),
        }
    }
}

fn uninstall_systemd_service() -> ServiceResult {
    // Stop and disable
    let _ = Command::new("systemctl")
        .args(["--user", "stop", &format!("{SERVICE_NAME}.service")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = Command::new("systemctl")
        .args(["--user", "disable", &format!("{SERVICE_NAME}.service")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let path = systemd_service_file();
    if path.exists() {
        let _ = fs::remove_file(&path);
    }

    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    ServiceResult {
        success: true,
        message: "Service uninstalled successfully.".into(),
    }
}

fn uninstall_launchd_service() -> ServiceResult {
    let plist = launchd_plist();

    // Unload
    let _ = Command::new("launchctl")
        .args(["unload", &plist.display().to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if plist.exists() {
        let _ = fs::remove_file(&plist);
    }

    ServiceResult {
        success: true,
        message: "Service uninstalled successfully.".into(),
    }
}

pub fn start_service() -> ServiceResult {
    if has_systemd() {
        match Command::new("systemctl")
            .args(["--user", "start", &format!("{SERVICE_NAME}.service")])
            .status()
        {
            Ok(s) if s.success() => ServiceResult {
                success: true,
                message: "Service started.".into(),
            },
            _ => ServiceResult {
                success: false,
                message: "Failed to start systemd service.".into(),
            },
        }
    } else if is_macos() {
        let plist = launchd_plist();
        // Load if not loaded
        let _ = Command::new("launchctl")
            .args(["load", &plist.display().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match Command::new("launchctl")
            .args(["start", &format!("com.claude.{SERVICE_NAME}")])
            .status()
        {
            Ok(s) if s.success() => ServiceResult {
                success: true,
                message: "Service started.".into(),
            },
            _ => ServiceResult {
                success: false,
                message: "Failed to start launchd service.".into(),
            },
        }
    } else {
        ServiceResult {
            success: false,
            message: "Unsupported platform.".into(),
        }
    }
}

pub fn stop_service() -> ServiceResult {
    if has_systemd() {
        match Command::new("systemctl")
            .args(["--user", "stop", &format!("{SERVICE_NAME}.service")])
            .status()
        {
            Ok(s) if s.success() => ServiceResult {
                success: true,
                message: "Service stopped.".into(),
            },
            _ => ServiceResult {
                success: false,
                message: "Failed to stop systemd service.".into(),
            },
        }
    } else if is_macos() {
        match Command::new("launchctl")
            .args(["stop", &format!("com.claude.{SERVICE_NAME}")])
            .status()
        {
            Ok(s) if s.success() => ServiceResult {
                success: true,
                message: "Service stopped.".into(),
            },
            _ => ServiceResult {
                success: false,
                message: "Failed to stop launchd service.".into(),
            },
        }
    } else {
        ServiceResult {
            success: false,
            message: "Unsupported platform.".into(),
        }
    }
}

pub fn restart_service() -> ServiceResult {
    if has_systemd() {
        match Command::new("systemctl")
            .args(["--user", "restart", &format!("{SERVICE_NAME}.service")])
            .status()
        {
            Ok(s) if s.success() => ServiceResult {
                success: true,
                message: "Service restarted.".into(),
            },
            _ => ServiceResult {
                success: false,
                message: "Failed to restart systemd service.".into(),
            },
        }
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

pub struct ServiceStatus {
    pub running: bool,
    pub enabled: bool,
    pub info: String,
}

pub fn get_service_status() -> ServiceStatus {
    if has_systemd() {
        get_systemd_status()
    } else if is_macos() {
        get_launchd_status()
    } else {
        ServiceStatus {
            running: false,
            enabled: false,
            info: "Unsupported platform".into(),
        }
    }
}

fn get_systemd_status() -> ServiceStatus {
    let running = Command::new("sh")
        .args([
            "-c",
            &format!("systemctl --user is-active {SERVICE_NAME}.service 2>/dev/null || true"),
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false);

    let enabled = Command::new("sh")
        .args([
            "-c",
            &format!("systemctl --user is-enabled {SERVICE_NAME}.service 2>/dev/null || true"),
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "enabled")
        .unwrap_or(false);

    let info = if !systemd_service_file().exists() {
        "Service not installed".into()
    } else {
        format!("Service file: {}", systemd_service_file().display())
    };

    ServiceStatus {
        running,
        enabled,
        info,
    }
}

fn get_launchd_status() -> ServiceStatus {
    let plist = launchd_plist();
    let enabled = plist.exists();

    let running = Command::new("sh")
        .args(["-c", "launchctl list 2>/dev/null || true"])
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            out.contains(&format!("com.claude.{SERVICE_NAME}"))
        })
        .unwrap_or(false);

    let info = if !enabled {
        "Service not installed".into()
    } else {
        format!("Plist file: {}", plist.display())
    };

    ServiceStatus {
        running,
        enabled,
        info,
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
    use std::io::Write;

    #[test]
    fn test_parse_env_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "# Comment line").unwrap();
        writeln!(f, "export TOKEN=abc123").unwrap();
        writeln!(f, "CHAT_ID=\"-100999\"").unwrap();
        writeln!(f, "SINGLE='hello world'").unwrap();
        writeln!(f, "INLINE=value # comment").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "NOVAL=").unwrap();
        drop(f);

        let vars = parse_env_file(&path);
        assert_eq!(vars.get("TOKEN").unwrap(), "abc123");
        assert_eq!(vars.get("CHAT_ID").unwrap(), "-100999");
        assert_eq!(vars.get("SINGLE").unwrap(), "hello world");
        assert_eq!(vars.get("INLINE").unwrap(), "value");
        assert_eq!(vars.get("NOVAL").unwrap(), "");
    }

    #[test]
    fn test_parse_env_file_missing() {
        let vars = parse_env_file(Path::new("/nonexistent/path/.env"));
        assert!(vars.is_empty());
    }

    #[test]
    fn test_parse_env_file_hash_in_quotes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env");
        fs::write(&path, "KEY=\"value#with#hashes\"\n").unwrap();
        let vars = parse_env_file(&path);
        assert_eq!(vars.get("KEY").unwrap(), "value#with#hashes");
    }

    #[test]
    fn test_strip_inline_comment() {
        assert_eq!(strip_inline_comment("value # comment"), "value");
        assert_eq!(strip_inline_comment("\"val#ue\" # comment"), "\"val#ue\"");
        assert_eq!(strip_inline_comment("no_comment"), "no_comment");
    }

    #[test]
    fn test_escape_xml() {
        assert_eq!(
            escape_xml("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
    }

    #[test]
    fn test_generate_systemd_service_contains_key_fields() {
        let content = generate_systemd_service();
        assert!(content.contains("[Unit]"));
        assert!(content.contains("[Service]"));
        assert!(content.contains("Type=simple"));
        assert!(content.contains("Restart=on-failure"));
        assert!(content.contains("RestartSec=10s"));
        assert!(content.contains("StartLimitBurst=5"));
        assert!(content.contains("[Install]"));
        assert!(content.contains("WantedBy=default.target"));
    }

    #[test]
    fn test_generate_launchd_plist_contains_key_fields() {
        let content = generate_launchd_plist();
        assert!(content.contains("<key>Label</key>"));
        assert!(content.contains("<key>KeepAlive</key>"));
        assert!(content.contains("<key>Crashed</key>"));
        assert!(content.contains("<key>ThrottleInterval</key>"));
        assert!(content.contains("<integer>10</integer>"));
        assert!(content.contains("<key>StandardOutPath</key>"));
        assert!(content.contains("<key>StandardErrorPath</key>"));
    }

    #[test]
    fn test_service_status_unsupported() {
        // On CI (likely Linux without systemd user session), this should still work
        let status = get_service_status();
        // Just check it doesn't panic
        assert!(!status.info.is_empty());
    }
}
