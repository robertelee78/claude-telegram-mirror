use crate::error::Result;
use crate::types::{is_valid_slash_command, ALLOWED_TMUX_KEYS};
use std::process::Command;

/// Input injector for sending user input from Telegram to Claude Code CLI via tmux.
///
/// Security: ALL tmux commands use Command::arg() — NO shell interpolation.
/// This prevents command injection via user-controlled inputs like session names,
/// socket paths, or message text.
pub struct InputInjector {
    tmux_target: Option<String>,
    tmux_socket: Option<String>,
}

impl Default for InputInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInjector {
    pub fn new() -> Self {
        Self {
            tmux_target: None,
            tmux_socket: None,
        }
    }

    /// Set the tmux target and optional socket path.
    /// Validates socket path to prevent directory traversal.
    pub fn set_target(&mut self, target: &str, socket: Option<&str>) {
        self.tmux_target = Some(target.to_string());
        self.tmux_socket = socket.and_then(|s| {
            if s.contains("..") {
                tracing::warn!(path = %s, "Rejecting tmux socket path with '..' traversal");
                return None;
            }
            if !s.starts_with('/') {
                tracing::warn!(path = %s, "Rejecting non-absolute tmux socket path");
                return None;
            }
            if s.len() > 256 {
                tracing::warn!(len = s.len(), "Rejecting oversized tmux socket path");
                return None;
            }
            Some(s.to_string())
        });
    }

    /// Get the socket args for tmux commands
    fn socket_args(&self) -> Vec<&str> {
        match &self.tmux_socket {
            Some(s) => vec!["-S", s.as_str()],
            None => vec![],
        }
    }

    /// Validate that the tmux target pane exists (BUG-001 fix)
    pub fn validate_target(&self) -> std::result::Result<(), String> {
        let target = self
            .tmux_target
            .as_deref()
            .ok_or_else(|| "No tmux session configured".to_string())?;

        let mut cmd = Command::new("tmux");
        for arg in self.socket_args() {
            cmd.arg(arg);
        }
        cmd.arg("list-panes").arg("-t").arg(target);

        match cmd.output() {
            Ok(output) if output.status.success() => Ok(()),
            _ => Err(format!(
                "Pane \"{}\" not found. Claude may have moved to a different pane. \
                 Send any command in Claude to refresh the connection.",
                target
            )),
        }
    }

    /// Inject text input into the tmux pane.
    /// Uses Command::arg() — no shell interpolation possible.
    pub fn inject(&self, text: &str) -> Result<bool> {
        let target = match &self.tmux_target {
            Some(t) => t,
            None => return Ok(false),
        };

        if let Err(reason) = self.validate_target() {
            tracing::warn!(%reason, "Target validation failed");
            return Ok(false);
        }

        // Send text with -l (literal mode)
        let mut send_cmd = Command::new("tmux");
        for arg in self.socket_args() {
            send_cmd.arg(arg);
        }
        send_cmd
            .arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("-l")
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
        for arg in self.socket_args() {
            enter_cmd.arg(arg);
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

    /// Send a special key (from whitelist only).
    /// BUG-004 fix: includes socket flag for correct tmux server targeting.
    pub fn send_key(&self, key: &str) -> Result<bool> {
        let target = match &self.tmux_target {
            Some(t) => t,
            None => return Ok(false),
        };

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

        if !ALLOWED_TMUX_KEYS.contains(&tmux_key) {
            tracing::warn!(key = %tmux_key, "Key not in whitelist, rejecting");
            return Ok(false);
        }

        let mut cmd = Command::new("tmux");
        for arg in self.socket_args() {
            cmd.arg(arg);
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

    /// Send a slash command (like /clear).
    /// Validates against character whitelist, sends with -l flag.
    pub fn send_slash_command(&self, command: &str) -> Result<bool> {
        let target = match &self.tmux_target {
            Some(t) => t,
            None => return Ok(false),
        };

        if !is_valid_slash_command(command) {
            tracing::warn!(%command, "Slash command rejected: contains unsafe characters");
            return Ok(false);
        }

        // Send command text with -l (literal mode)
        let mut cmd = Command::new("tmux");
        for arg in self.socket_args() {
            cmd.arg(arg);
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
        for arg in self.socket_args() {
            enter_cmd.arg(arg);
        }
        enter_cmd
            .arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("Enter");

        let output = enter_cmd.output()?;
        Ok(output.status.success())
    }

    /// Check if tmux is available
    #[allow(dead_code)] // Library API
    pub fn is_tmux_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
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

    /// Find a tmux session running Claude Code (fallback when $TMUX not set)
    pub fn find_claude_code_session() -> Option<String> {
        // Search pane commands
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{session_name}:#{pane_current_command}",
            ])
            .output()
            .ok()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if let Some((session, command)) = line.split_once(':') {
                    // "node" match removed — was legacy from when ctm was a Node.js process
                    if command.contains("claude") {
                        return Some(session.to_string());
                    }
                }
            }
        }

        // Fallback: session names
        let output = Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .ok()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let lower = line.to_lowercase();
                if lower.contains("claude") || lower.contains("code") {
                    return Some(line.to_string());
                }
            }
        }

        None
    }
}

/// L3.4: Get the injection method name.
impl InputInjector {
    /// Returns the injection method: "tmux" if a target is configured, "none" otherwise.
    #[allow(dead_code)] // Library API
    pub fn get_method(&self) -> &str {
        if self.tmux_target.is_some() {
            "tmux"
        } else {
            "none"
        }
    }

    /// Returns the configured tmux session target, if any.
    #[allow(dead_code)] // Library API
    pub fn get_tmux_session(&self) -> Option<&str> {
        self.tmux_target.as_deref()
    }

    /// Returns the configured tmux socket path, if any.
    #[allow(dead_code)] // Library API
    pub fn get_tmux_socket(&self) -> Option<&str> {
        self.tmux_socket.as_deref()
    }
}

/// L3.5: Factory function that creates a fully-configured InputInjector.
///
/// Detects the current tmux session automatically if tmux is available.
#[allow(dead_code)] // Library API
pub fn create_injector() -> InputInjector {
    let mut inj = InputInjector::new();
    if InputInjector::is_tmux_available() {
        if let Some(info) = InputInjector::detect_tmux_session() {
            inj.set_target(&info.target, info.socket.as_deref());
        }
    }
    inj
}

/// L3.6: Escape text for tmux send-keys.
///
/// DEPRECATED: With `Command::arg()` and `-l` flag, no escaping is needed.
/// This function is retained for API compatibility with external callers.
#[deprecated(note = "Not needed with Command::arg() + -l flag")]
#[allow(dead_code)] // Library API (deprecated)
pub fn escape_tmux_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
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

#[derive(Debug, Clone)]
pub struct TmuxInfo {
    pub session: String,
    #[allow(dead_code)] // Library API
    pub pane: String,
    pub target: String,
    pub socket: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::is_valid_slash_command;

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
        assert!(!injector.inject("test").unwrap());
    }

    #[test]
    fn test_socket_path_validation() {
        let mut injector = InputInjector::new();

        // Valid absolute path
        injector.set_target("session:0.0", Some("/tmp/tmux-1000/default"));
        assert!(injector.tmux_socket.is_some());

        // Path traversal rejected
        injector.set_target("session:0.0", Some("/tmp/../etc/evil"));
        assert!(injector.tmux_socket.is_none());

        // Relative path rejected
        injector.set_target("session:0.0", Some("relative/path"));
        assert!(injector.tmux_socket.is_none());

        // Oversized path rejected
        let long = format!("/{}", "a".repeat(256));
        injector.set_target("session:0.0", Some(&long));
        assert!(injector.tmux_socket.is_none());
    }

    #[test]
    fn test_slash_command_validation() {
        assert!(is_valid_slash_command("/clear"));
        assert!(is_valid_slash_command("/rename My Feature"));
        assert!(!is_valid_slash_command("/clear;rm -rf /"));
        assert!(!is_valid_slash_command(""));
    }
}
