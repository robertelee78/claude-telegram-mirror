//! ADR-016 §Default enablement: the OpenCode pipe plugin, provisioned by ctm itself.
//!
//! OpenCode loads every `plugins/*.js` under its global config dir into each process
//! it starts (TUI, `serve`, `web`). ctm writes ONE file there — `ctm.js`, generated
//! from the template embedded in this binary with the pipe socket path filled in — the
//! same way it writes hooks into Claude Code's `settings.json`. The daemon (re)writes it
//! at start and re-checks periodically, so `ctm update` rolls the plugin forward with
//! no operator step, and a later OpenCode install is picked up within a minute.

use crate::error::{AppError, Result};
use std::path::{Path, PathBuf};

const TEMPLATE: &str = include_str!("opencode_plugin.js");
pub const PLUGIN_FILE: &str = "ctm.js";

/// Outcome of an idempotent provisioning pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    Installed,
    Updated,
    Unchanged,
}

/// Where the plugin lives: `<opencode config dir>/plugins/ctm.js`.
pub fn plugin_path() -> PathBuf {
    super::detect::opencode_config_dir()
        .join("plugins")
        .join(PLUGIN_FILE)
}

/// The pipe socket the daemon listens on: a sibling of the bridge socket, so a custom
/// `TELEGRAM_BRIDGE_SOCKET` moves both together and the 0700 directory protects both.
pub fn pipe_socket_path(bridge_socket: &Path) -> PathBuf {
    bridge_socket.with_file_name("opencode.sock")
}

/// Exact file contents for this binary and this socket path.
pub fn render(pipe_socket: &Path) -> String {
    TEMPLATE
        .replace("__CTM_VERSION__", env!("CARGO_PKG_VERSION"))
        .replace(
            "__CTM_PIPE_SOCKET__",
            &js_string(&pipe_socket.to_string_lossy()),
        )
}

/// Escape for inclusion inside a double-quoted JS string literal.
fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Is the installed file byte-identical to what this binary would write?
pub fn is_current(pipe_socket: &Path) -> bool {
    std::fs::read_to_string(plugin_path()).is_ok_and(|s| s == render(pipe_socket))
}

/// Write the plugin if missing or different (atomic: temp file + rename).
pub fn ensure(pipe_socket: &Path) -> Result<PluginState> {
    ensure_at(&plugin_path(), pipe_socket)
}

pub fn ensure_at(path: &Path, pipe_socket: &Path) -> Result<PluginState> {
    let want = render(pipe_socket);
    let existing = std::fs::read_to_string(path).ok();
    if existing.as_deref() == Some(want.as_str()) {
        return Ok(PluginState::Unchanged);
    }
    let dir = path
        .parent()
        .ok_or_else(|| AppError::Config("plugin path has no parent".into()))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| AppError::Config(format!("cannot create {}: {e}", dir.display())))?;
    let tmp = dir.join(format!(".{PLUGIN_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &want)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            AppError::Config(format!("cannot write {}: {e}", path.display()))
        })?;
    Ok(if existing.is_some() {
        PluginState::Updated
    } else {
        PluginState::Installed
    })
}

/// How often the daemon re-checks the plugin (an OpenCode installed after ctm, or a
/// plugin file edited or deleted by hand, is repaired within this interval).
pub const KEEPER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Daemon task: provision the plugin now and keep it current.
pub async fn run_keeper(config: std::sync::Arc<crate::config::Config>) {
    let sock = pipe_socket_path(&config.socket_path);
    let mut announced_absent = false;
    loop {
        if super::detect::opencode_present() {
            announced_absent = false;
            match ensure(&sock) {
                Ok(PluginState::Unchanged) => {}
                Ok(state) => tracing::info!(
                    ?state,
                    path = %plugin_path().display(),
                    "OpenCode plugin provisioned — bare `opencode` sessions are mirrored"
                ),
                Err(e) => tracing::warn!(error = %e, "OpenCode plugin could not be written"),
            }
        } else if !announced_absent {
            tracing::info!("OpenCode not installed — will watch for it");
            announced_absent = true;
        }
        tokio::time::sleep(KEEPER_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_socket_and_version_and_nothing_else_is_left() {
        let s = render(Path::new(
            "/home/u/.config/claude-telegram-mirror/opencode.sock",
        ));
        assert!(
            s.contains("const SOCKET = \"/home/u/.config/claude-telegram-mirror/opencode.sock\"")
        );
        assert!(s.contains(&format!("Generated by ctm {}", env!("CARGO_PKG_VERSION"))));
        assert!(!s.contains("__CTM_"));
        assert!(s.contains("export default async function ctm(input)"));
    }

    #[test]
    fn socket_path_is_js_escaped() {
        let s = render(Path::new("/odd \"quoted\"\\path/opencode.sock"));
        assert!(s.contains(r#"const SOCKET = "/odd \"quoted\"\\path/opencode.sock""#));
    }

    #[test]
    fn ensure_is_idempotent_and_reports_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode/plugins/ctm.js");
        let sock = Path::new("/tmp/x.sock");
        assert_eq!(ensure_at(&path, sock).unwrap(), PluginState::Installed);
        assert_eq!(ensure_at(&path, sock).unwrap(), PluginState::Unchanged);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), render(sock));
        // A different socket (or a new ctm version) rewrites in place.
        assert_eq!(
            ensure_at(&path, Path::new("/tmp/y.sock")).unwrap(),
            PluginState::Updated
        );
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("/tmp/y.sock"));
        // No temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn pipe_socket_is_sibling_of_bridge_socket() {
        assert_eq!(
            pipe_socket_path(Path::new("/a/b/bridge.sock")),
            PathBuf::from("/a/b/opencode.sock")
        );
    }
}
