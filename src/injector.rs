use crate::error::Result;
use crate::types::ALLOWED_TMUX_KEYS;
use std::process::Command;

/// Input injector for sending user input from Telegram to Claude Code CLI via tmux
///
/// Security: ALL tmux commands use Command::arg() - NO shell interpolation.
/// This prevents command injection via user-controlled inputs like session names,
/// socket paths, or message text (Security fixes #1, #2, #7).
pub struct InputInjector {
    tmux_target: Option<String>,
    tmux_socket: Option<String>,
}

impl InputInjector {
    pub fn new() -> Self {
        Self {
            tmux_target: None,
            tmux_socket: None,
        }
    }

    /// Set the tmux target and optional socket path
    /// HIGH-01: Validate socket path to prevent arbitrary socket targeting
    pub fn set_target(&mut self, target: &str, socket: Option<&str>) {
        self.tmux_target = Some(target.to_string());
        self.tmux_socket = socket.and_then(|s| {
            let path = std::path::Path::new(s);
            // Reject paths with traversal components
            if s.contains("..") {
                tracing::warn!(path = %s, "Rejecting tmux socket path with '..' traversal");
                return None;
            }
            // Must be an absolute path
            if !path.is_absolute() {
                tracing::warn!(path = %s, "Rejecting non-absolute tmux socket path");
                return None;
            }
            // Reasonable length limit
            if s.len() > 256 {
                tracing::warn!(len = s.len(), "Rejecting oversized tmux socket path");
                return None;
            }
            Some(s.to_string())
        });
    }

    /// Get current tmux target
    #[allow(dead_code)]
    pub fn target(&self) -> Option<&str> {
        self.tmux_target.as_deref()
    }

    /// Check if tmux is available
    pub fn is_tmux_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Validate that the tmux target pane exists
    pub fn validate_target(&self) -> std::result::Result<(), String> {
        let target = self
            .tmux_target
            .as_deref()
            .ok_or_else(|| "No tmux session configured".to_string())?;

        // Security fix #7: tmux target passed as .arg() never interpolated
        let mut cmd = Command::new("tmux");
        if let Some(socket) = &self.tmux_socket {
            cmd.arg("-S").arg(socket);
        }
        cmd.arg("list-panes").arg("-t").arg(target);

        match cmd.output() {
            Ok(output) if output.status.success() => Ok(()),
            _ => Err(format!(
                "Pane \"{}\" not found. Claude may have moved to a different pane.",
                target
            )),
        }
    }

    /// Inject text input into the tmux pane
    /// Security fix #1: Command::new("tmux").arg() - no shell interpolation
    pub fn inject(&self, text: &str) -> Result<bool> {
        let target = match &self.tmux_target {
            Some(t) => t,
            None => return Ok(false),
        };

        // Validate target exists
        if let Err(reason) = self.validate_target() {
            tracing::warn!(%reason, "Target validation failed");
            return Ok(false);
        }

        // Send text with -l (literal mode) - tmux handles escaping
        let mut send_cmd = Command::new("tmux");
        if let Some(socket) = &self.tmux_socket {
            send_cmd.arg("-S").arg(socket);
        }
        send_cmd
            .arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("-l") // literal mode - no special key interpretation
            .arg(text);

        let output = send_cmd.output()?;
        if !output.status.success() {
            tracing::error!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "tmux send-keys failed"
            );
            return Ok(false);
        }

        // Send Enter key separately
        let mut enter_cmd = Command::new("tmux");
        if let Some(socket) = &self.tmux_socket {
            enter_cmd.arg("-S").arg(socket);
        }
        enter_cmd
            .arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("Enter");

        let output = enter_cmd.output()?;
        if !output.status.success() {
            tracing::error!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "tmux send-keys Enter failed"
            );
            return Ok(false);
        }

        tracing::debug!(%target, text_len = text.len(), "Injected text via tmux");
        Ok(true)
    }

    /// Send a special key (from whitelist only)
    /// Security fix #7: Key is validated against whitelist, passed as .arg()
    pub fn send_key(&self, key: &str) -> Result<bool> {
        let target = match &self.tmux_target {
            Some(t) => t,
            None => return Ok(false),
        };

        // Map human-readable names to tmux key names
        let tmux_key = match key {
            "Enter" => "Enter",
            "Escape" => "Escape",
            "Tab" => "Tab",
            "Ctrl-C" => "C-c",
            "Ctrl-U" => "C-u",
            "Ctrl-D" => "C-d",
            "Ctrl-L" => "C-l",
            _ => key,
        };

        // Validate against whitelist
        if !ALLOWED_TMUX_KEYS.contains(&tmux_key) {
            tracing::warn!(key = %tmux_key, "Key not in whitelist, rejecting");
            return Ok(false);
        }

        let mut cmd = Command::new("tmux");
        if let Some(socket) = &self.tmux_socket {
            cmd.arg("-S").arg(socket);
        }
        cmd.arg("send-keys").arg("-t").arg(target).arg(tmux_key);

        let output = cmd.output()?;
        if !output.status.success() {
            tracing::error!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "tmux send-keys failed for key"
            );
            return Ok(false);
        }

        tracing::debug!(%target, %key, "Sent key via tmux");
        Ok(true)
    }

    /// Send a slash command (like /clear)
    /// Security fix #1: Command text passed as .arg(), not interpolated into shell
    pub fn send_slash_command(&self, command: &str) -> Result<bool> {
        let target = match &self.tmux_target {
            Some(t) => t,
            None => return Ok(false),
        };

        // Validate command matches safe pattern: /word (alphanumeric + hyphens only)
        let cmd_body = command.strip_prefix('/').unwrap_or(command);
        if !cmd_body
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ' ')
        {
            tracing::warn!(
                command,
                "Slash command rejected: contains unsafe characters"
            );
            return Ok(false);
        }

        // Send command text with -l (literal mode) to prevent tmux key interpretation
        let mut cmd = Command::new("tmux");
        if let Some(socket) = &self.tmux_socket {
            cmd.arg("-S").arg(socket);
        }
        cmd.arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("-l")
            .arg(command);

        let output = cmd.output()?;
        if !output.status.success() {
            return Ok(false);
        }

        // Send Enter
        let mut enter_cmd = Command::new("tmux");
        if let Some(socket) = &self.tmux_socket {
            enter_cmd.arg("-S").arg(socket);
        }
        enter_cmd
            .arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("Enter");

        let output = enter_cmd.output()?;
        Ok(output.status.success())
    }

    /// Check if a tmux pane is alive
    pub fn is_pane_alive(target: &str, socket: Option<&str>) -> bool {
        let mut cmd = Command::new("tmux");
        if let Some(s) = socket {
            cmd.arg("-S").arg(s);
        }
        cmd.arg("list-panes").arg("-t").arg(target);
        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// Detect current tmux session from environment
    pub fn detect_tmux_session() -> Option<TmuxInfo> {
        let tmux_env = std::env::var("TMUX").ok()?;
        let socket_path = tmux_env.split(',').next().map(|s| s.to_string());

        let session = Command::new("tmux")
            .arg("display-message")
            .arg("-p")
            .arg("#S")
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })?;

        let window = Command::new("tmux")
            .arg("display-message")
            .arg("-p")
            .arg("#I")
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "0".to_string());

        let pane = Command::new("tmux")
            .arg("display-message")
            .arg("-p")
            .arg("#P")
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "0".to_string());

        let target = format!("{}:{}.{}", session, window, pane);

        Some(TmuxInfo {
            session,
            pane,
            target,
            socket: socket_path,
        })
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TmuxInfo {
    pub session: String,
    pub pane: String,
    pub target: String,
    pub socket: Option<String>,
}

/// Get hostname safely
pub fn get_hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_whitelist() {
        assert!(ALLOWED_TMUX_KEYS.contains(&"Enter"));
        assert!(ALLOWED_TMUX_KEYS.contains(&"Escape"));
        assert!(ALLOWED_TMUX_KEYS.contains(&"C-c"));
        assert!(!ALLOWED_TMUX_KEYS.contains(&"rm -rf /"));
    }

    #[test]
    fn test_injector_no_target() {
        let injector = InputInjector::new();
        assert!(injector.target().is_none());
        assert!(!injector.inject("test").unwrap());
    }
}
