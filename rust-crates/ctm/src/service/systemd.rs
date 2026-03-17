//! systemd unit file generation and lifecycle.

use super::*;

pub(super) fn generate_systemd_service() -> String {
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

pub(super) fn install_systemd_service() -> ServiceResult {
    let env_result = env::create_systemd_env_file();
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

pub(super) fn uninstall_systemd_service() -> ServiceResult {
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

pub(super) fn start_systemd_service() -> ServiceResult {
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
}

pub(super) fn stop_systemd_service() -> ServiceResult {
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
}

pub(super) fn restart_systemd_service() -> ServiceResult {
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
}

pub(super) fn get_systemd_status() -> ServiceStatus {
    let running = Command::new("systemctl")
        .args(["--user", "is-active", &format!("{SERVICE_NAME}.service")])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false);

    let enabled = Command::new("systemctl")
        .args(["--user", "is-enabled", &format!("{SERVICE_NAME}.service")])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
