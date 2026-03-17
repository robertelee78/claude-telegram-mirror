//! launchd plist generation and lifecycle.

use super::*;

fn get_macos_path() -> String {
    let home = home_dir();
    let mut paths: Vec<String> = vec![
        "/usr/local/bin".into(),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
        "/opt/homebrew/bin".into(),
        format!("{}/.local/bin", home.display()),
    ];

    // Merge with current PATH, excluding NVM paths (legacy Node.js artifact)
    if let Ok(current) = std::env::var("PATH") {
        for dir in current.split(':') {
            if !dir.is_empty() && !dir.contains(".nvm") && !paths.contains(&dir.to_string()) {
                paths.push(dir.to_string());
            }
        }
    }

    paths.join(":")
}

pub(super) fn generate_launchd_plist() -> String {
    let binary = ctm_binary_path();
    let home = home_dir();
    let config_dir = home.join(".config").join(SERVICE_NAME);
    let log_file = config_dir.join("daemon.log");
    let err_file = config_dir.join("daemon.err.log");

    let env_vars = env::parse_env_file(&env_file_path());

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
    // User-defined env vars from ~/.telegram-env
    for (key, value) in &env_vars {
        if key == "HOME" || key == "PATH" {
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

pub(super) fn install_launchd_service() -> ServiceResult {
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

pub(super) fn uninstall_launchd_service() -> ServiceResult {
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

pub(super) fn start_launchd_service() -> ServiceResult {
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
}

pub(super) fn stop_launchd_service() -> ServiceResult {
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
}

pub(super) fn get_launchd_status() -> ServiceStatus {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_generate_launchd_plist_no_node_artifacts() {
        let content = generate_launchd_plist();
        // NODE_ENV was a TypeScript artifact — should not be in the Rust plist
        assert!(
            !content.contains("NODE_ENV"),
            "plist should not contain NODE_ENV"
        );
        // NVM paths are not needed for the Rust binary
        assert!(
            !content.contains(".nvm"),
            "plist should not contain NVM paths"
        );
    }
}
