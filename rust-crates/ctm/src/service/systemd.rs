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

    // Writing the unit file is not installing the service: it is only installed once
    // the user manager has reloaded and enabled it. Both steps used to run with their
    // results discarded and "Service installed" reported regardless — so on a box where
    // `systemctl --user` cannot talk to a user manager (a plain SSH login with no
    // lingering is the usual case) ctm claimed success and the next `ctm service start`
    // failed with systemd's bare "Unit ... not found".
    for (args, what) in [
        (vec!["--user", "daemon-reload"], "daemon-reload"),
        (
            vec!["--user", "enable", &format!("{SERVICE_NAME}.service")],
            "enable",
        ),
    ] {
        match Command::new("systemctl").args(&args).output() {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return ServiceResult {
                    success: false,
                    message: format!(
                        "Unit file written to {}, but `systemctl --user {what}` failed: {}{}",
                        service_path.display(),
                        if stderr.is_empty() {
                            "(no output)".into()
                        } else {
                            stderr.clone()
                        },
                        user_manager_hint(&stderr),
                    ),
                };
            }
            Err(e) => {
                return ServiceResult {
                    success: false,
                    message: format!(
                        "Unit file written to {}, but systemctl could not be run: {e}",
                        service_path.display()
                    ),
                };
            }
        }
    }

    ServiceResult {
        success: true,
        message: format!(
            "Service installed: {}\n\nCommands:\n  Start:   systemctl --user start {SERVICE_NAME}\n  Stop:    systemctl --user stop {SERVICE_NAME}\n  Status:  systemctl --user status {SERVICE_NAME}\n  Logs:    journalctl --user -u {SERVICE_NAME} -f\n\nTo run without being logged in:\n  sudo loginctl enable-linger $USER",
            service_path.display(),
        ),
    }
}

/// Advice for the failures that actually happen on a headless Linux box.
///
/// A user unit needs a running *user manager*, which a plain SSH session may not have:
/// without `loginctl enable-linger` the manager stops with the last session, and without
/// `XDG_RUNTIME_DIR`/`DBUS_SESSION_BUS_ADDRESS` `systemctl --user` cannot reach it at all.
pub(super) fn user_manager_hint(stderr: &str) -> String {
    let s = stderr.to_ascii_lowercase();
    if s.contains("failed to connect to bus") || s.contains("no medium found") {
        return format!(
            "\n\nThis user has no running systemd user manager. Enable it with:\n               sudo loginctl enable-linger {}\n\
             then log in again (or `export XDG_RUNTIME_DIR=/run/user/$(id -u)`) and re-run \
             `ctm service install`.",
            std::env::var("USER").unwrap_or_else(|_| "$USER".into())
        );
    }
    String::new()
}

/// Has the unit file been written? (Distinct from "the manager knows about it".)
pub(super) fn systemd_unit_present() -> bool {
    systemd_service_file().exists()
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
    // "Unit not found" means the service was never installed, which is a thing ctm can
    // simply do rather than make the operator decode systemd's error. (Reported from a
    // fresh Linux install: `ctm service start` → "Unit claude-telegram-mirror.service
    // not found." with no next step.)
    if !systemd_unit_present() {
        let installed = install_systemd_service();
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
    match Command::new("systemctl")
        .args(["--user", "start", &format!("{SERVICE_NAME}.service")])
        .output()
    {
        Ok(out) if out.status.success() => ServiceResult {
            success: true,
            message: "Service started.".into(),
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            ServiceResult {
                success: false,
                message: format!(
                    "Failed to start systemd service: {}{}",
                    if stderr.is_empty() {
                        "(no output)".into()
                    } else {
                        stderr.clone()
                    },
                    user_manager_hint(&stderr)
                ),
            }
        }
        Err(e) => ServiceResult {
            success: false,
            message: format!("Failed to run systemctl: {e}"),
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

    #[test]
    fn a_missing_user_manager_is_explained_not_just_reported() {
        // The failure a headless SSH login actually produces.
        let hint = user_manager_hint("Failed to connect to bus: No medium found");
        assert!(hint.contains("loginctl enable-linger"), "{hint}");
        assert!(hint.contains("XDG_RUNTIME_DIR"), "{hint}");
    }

    #[test]
    fn unrelated_errors_get_no_invented_advice() {
        assert_eq!(user_manager_hint("Unit foo.service not found."), "");
        assert_eq!(user_manager_hint(""), "");
    }

    #[test]
    fn the_unit_path_is_the_users_systemd_directory() {
        let p = systemd_service_file();
        assert!(p.ends_with(format!("{SERVICE_NAME}.service")), "{p:?}");
        assert!(
            p.to_string_lossy().contains(".config/systemd/user"),
            "a user unit, which is why every systemctl call passes --user: {p:?}"
        );
    }
}
