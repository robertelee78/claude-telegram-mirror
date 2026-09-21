//! ADR-021: keep the app-server daemon on the account the user signed in with.
//!
//! A running codex reloads `auth.json` only for the account it already holds; a
//! different account on disk is deliberately ignored ("Skipping auth reload due to
//! account id mismatch"). ctm keeps the daemon alive indefinitely, so after
//! `codex login` the daemon — where every `--remote` session actually runs — would
//! stay on the old account forever. The remedy is a restart, done here only when
//! no Codex session is live, and reported when it cannot be.
//!
//! `auth.json` is read for identity only (mode, email, account id); tokens are never
//! logged or kept. The id token's payload is decoded, not verified — it is a label.

use super::codex_rpc::Rpc;
use crate::config::CodexHostConfig;
use crate::error::{AppError, Result};
use base64::Engine;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// How often the daemon's keeper reconciles (ADR-021 §3).
pub const TICK: Duration = Duration::from_secs(60);
const STOP_BUDGET: Duration = Duration::from_secs(10);

/// Who `~/.codex/auth.json` says is signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskAuth {
    /// `chatgpt` | `apikey` | … (`auth_mode`), or `apikey` when only a key is present.
    pub mode: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
}

/// Who the running app-server says it is (`account/read`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonAccount {
    /// `chatgpt` | `apiKey` | …
    pub kind: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    /// Daemon and disk agree.
    InSync(String),
    /// Restarted the daemon; it now reports the new account.
    Restarted { from: String, to: String },
    /// Mismatch, but `live` Codex sessions exist — not restarting under them.
    Deferred {
        from: String,
        to: String,
        live: usize,
    },
    /// Could not tell or could not act (daemon down, no auth.json, restart failed).
    Unavailable(String),
}

impl Reconciled {
    /// One line for a terminal, a log, or `doctor`.
    pub fn line(&self) -> String {
        match self {
            Reconciled::InSync(who) => format!("Codex app-server signed in as {who}"),
            Reconciled::Restarted { from, to } => {
                format!("Codex app-server restarted: now signed in as {to} (was {from})")
            }
            Reconciled::Deferred { from, to, live } => format!(
                "Codex app-server is signed in as {from}, but you are now {to}; {live} live session(s) — it switches when they end"
            ),
            Reconciled::Unavailable(why) => format!("Codex account: {why}"),
        }
    }
}

fn decode_jwt_email(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("email")?
        .as_str()
        .map(|s| s.to_ascii_lowercase())
}

/// Identity from `auth.json` text. Pure.
pub fn parse_auth_json(text: &str) -> Result<DiskAuth> {
    let v: Value = serde_json::from_str(text)
        .map_err(|e| AppError::Config(format!("auth.json is not valid JSON: {e}")))?;
    let tokens = v.get("tokens");
    let mode = v
        .get("auth_mode")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            v.get("OPENAI_API_KEY")
                .and_then(Value::as_str)
                .map(|_| "apikey".into())
        })
        .ok_or_else(|| AppError::Config("auth.json has no auth_mode".into()))?;
    let email = tokens
        .and_then(|t| t.get("id_token"))
        .and_then(Value::as_str)
        .and_then(decode_jwt_email);
    let account_id = tokens
        .and_then(|t| t.get("account_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(DiskAuth {
        mode,
        email,
        account_id,
    })
}

/// Identity from the daemon's `account/read` result. `None` = signed out.
pub fn parse_account_read(v: &Value) -> Option<DaemonAccount> {
    let a = v.get("account")?;
    let kind = a.get("type")?.as_str()?.to_string();
    let email = a
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    Some(DaemonAccount { kind, email })
}

fn label_disk(d: &DiskAuth) -> String {
    match (&d.email, d.mode.as_str()) {
        (Some(e), _) => e.clone(),
        (None, "apikey") => "an API key".into(),
        (None, m) => format!("{m} (no email)"),
    }
}

fn label_daemon(a: Option<&DaemonAccount>) -> String {
    match a {
        None => "nobody (signed out)".into(),
        Some(DaemonAccount { email: Some(e), .. }) => e.clone(),
        Some(DaemonAccount { kind, .. }) if kind.eq_ignore_ascii_case("apikey") => {
            "an API key".into()
        }
        Some(DaemonAccount { kind, .. }) => format!("{kind} (no email)"),
    }
}

/// Does the daemon need a restart to match disk? Pure.
pub fn differs(disk: &DiskAuth, daemon: Option<&DaemonAccount>) -> bool {
    let Some(daemon) = daemon else {
        return true; // signed out in memory, credentials on disk
    };
    let disk_is_key = disk.mode == "apikey";
    let daemon_is_key = daemon.kind.eq_ignore_ascii_case("apikey");
    if disk_is_key || daemon_is_key {
        return disk_is_key != daemon_is_key;
    }
    match (&disk.email, &daemon.email) {
        (Some(a), Some(b)) => a != b,
        // Without an email on either side there is nothing to compare; assume in sync
        // rather than restart on every tick.
        _ => false,
    }
}

pub use super::codex_daemon::codex_home;

async fn daemon_account(sock: &Path) -> Result<Option<DaemonAccount>> {
    let mut rpc = Rpc::connect(sock).await?;
    let v = rpc.call("account/read", serde_json::json!({})).await?;
    Ok(parse_account_read(&v))
}

/// Compare, and restart the daemon when it is safe to (ADR-021 §1–2).
pub async fn reconcile(cx: &CodexHostConfig, live_sessions: usize) -> Reconciled {
    let auth_path = codex_home(cx).join("auth.json");
    let disk = match std::fs::read_to_string(&auth_path) {
        Ok(t) => match parse_auth_json(&t) {
            Ok(d) => d,
            Err(e) => return Reconciled::Unavailable(e.to_string()),
        },
        Err(_) => return Reconciled::Unavailable("not signed in (no auth.json)".into()),
    };
    let daemon = match daemon_account(&cx.socket_path).await {
        Ok(a) => a,
        Err(e) => return Reconciled::Unavailable(format!("app-server not reachable: {e}")),
    };
    let (from, to) = (label_daemon(daemon.as_ref()), label_disk(&disk));
    if !differs(&disk, daemon.as_ref()) {
        return Reconciled::InSync(from);
    }
    if live_sessions > 0 {
        return Reconciled::Deferred {
            from,
            to,
            live: live_sessions,
        };
    }
    let Some(bin) = super::detect::codex_binary(cx.binary.as_deref()) else {
        return Reconciled::Unavailable("codex binary not found".into());
    };
    if let Err(e) = super::codex_daemon::stop(&bin, &codex_home(cx)).await {
        return Reconciled::Unavailable(format!("could not stop the app-server: {e}"));
    }
    if !super::codex_daemon::wait_gone(&cx.socket_path, STOP_BUDGET).await {
        return Reconciled::Unavailable("app-server did not stop".into());
    }
    if let Err(e) = super::codex_daemon::ensure_running(cx).await {
        return Reconciled::Unavailable(format!("app-server did not come back: {e}"));
    }
    match daemon_account(&cx.socket_path).await {
        Ok(after) if !differs(&disk, after.as_ref()) => Reconciled::Restarted { from, to },
        Ok(after) => Reconciled::Unavailable(format!(
            "restarted, but the app-server reports {} while auth.json says {to}",
            label_daemon(after.as_ref())
        )),
        Err(e) => Reconciled::Unavailable(format!("restarted, but not reachable: {e}")),
    }
}

/// Live Codex sessions in ctm's store — the "do not restart under them" signal.
pub fn live_codex_sessions(config: &crate::config::Config) -> usize {
    crate::session::SessionManager::new(&config.config_dir, config.session_timeout)
        .and_then(|m| m.get_active_sessions())
        .map(|v| {
            v.iter()
                .filter(|s| s.host_kind() == crate::types::HostKind::Codex)
                .count()
        })
        .unwrap_or(0)
}

/// The daemon's keeper: reconcile every `TICK` (ADR-021 §3).
pub async fn run_keeper(config: std::sync::Arc<crate::config::Config>, cx: CodexHostConfig) {
    loop {
        tokio::time::sleep(TICK).await;
        let live = live_codex_sessions(&config);
        match reconcile(&cx, live).await {
            Reconciled::InSync(_) => {}
            r @ Reconciled::Restarted { .. } => tracing::info!("{}", r.line()),
            r @ Reconciled::Deferred { .. } => tracing::warn!("{}", r.line()),
            Reconciled::Unavailable(why) => {
                tracing::debug!(%why, "Codex account reconcile skipped")
            }
        }
    }
}

/// `ctm codex-preflight`: run by the `codex` shell function right before a
/// `--remote` launch. Prints one line to stderr when something happened or could not
/// happen; silent when in sync; never fails the launch.
pub async fn run_preflight() -> anyhow::Result<()> {
    let Ok(cfg) = crate::config::load_config(false) else {
        return Ok(());
    };
    if !cfg.hosts.codex.enabled {
        return Ok(());
    }
    let live = live_codex_sessions(&cfg);
    match reconcile(&cfg.hosts.codex, live).await {
        Reconciled::InSync(_) | Reconciled::Unavailable(_) => {}
        r => eprintln!("ctm: {}", r.line()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(email: &str) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::json!({ "email": email }).to_string());
        format!("hdr.{payload}.sig")
    }

    fn chatgpt_auth(email: &str, account: &str) -> String {
        serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": { "id_token": jwt(email), "access_token": "x", "refresh_token": "y", "account_id": account },
            "last_refresh": "2026-09-21T20:32:56Z"
        })
        .to_string()
    }

    #[test]
    fn disk_identity_is_mode_email_and_account_only() {
        let d = parse_auth_json(&chatgpt_auth("Me@Example.com", "acct-1")).unwrap();
        assert_eq!(d.mode, "chatgpt");
        assert_eq!(d.email.as_deref(), Some("me@example.com"), "case-folded");
        assert_eq!(d.account_id.as_deref(), Some("acct-1"));
        let k = parse_auth_json(r#"{"OPENAI_API_KEY":"sk-…"}"#).unwrap();
        assert_eq!(k.mode, "apikey");
        assert!(k.email.is_none());
        assert!(parse_auth_json("{}").is_err());
        assert!(parse_auth_json("nope").is_err());
    }

    #[test]
    fn daemon_identity_from_account_read() {
        let v = serde_json::json!({"account":{"email":"Robert@x.us","planType":"pro","type":"chatgpt"},"requiresOpenaiAuth":true});
        let a = parse_account_read(&v).unwrap();
        assert_eq!(a.kind, "chatgpt");
        assert_eq!(a.email.as_deref(), Some("robert@x.us"));
        assert!(parse_account_read(&serde_json::json!({"account": null})).is_none());
        assert!(parse_account_read(&serde_json::json!({})).is_none());
    }

    #[test]
    fn a_different_account_on_disk_means_restart() {
        let a = parse_auth_json(&chatgpt_auth("a@x", "1")).unwrap();
        let b = parse_auth_json(&chatgpt_auth("b@x", "2")).unwrap();
        let daemon_a = DaemonAccount {
            kind: "chatgpt".into(),
            email: Some("a@x".into()),
        };
        assert!(!differs(&a, Some(&daemon_a)), "same account: in sync");
        assert!(differs(&b, Some(&daemon_a)), "other account: restart");
        assert!(
            differs(&a, None),
            "daemon signed out, disk signed in: restart"
        );
        let key = parse_auth_json(r#"{"OPENAI_API_KEY":"k"}"#).unwrap();
        assert!(
            differs(&key, Some(&daemon_a)),
            "chatgpt in memory, api key on disk"
        );
        let daemon_key = DaemonAccount {
            kind: "apiKey".into(),
            email: None,
        };
        assert!(!differs(&key, Some(&daemon_key)), "api key both sides");
        assert!(
            differs(&a, Some(&daemon_key)),
            "api key in memory, chatgpt on disk"
        );
    }

    #[test]
    fn the_lines_name_both_sides() {
        let r = Reconciled::Deferred {
            from: "a@x".into(),
            to: "b@x".into(),
            live: 2,
        };
        assert!(
            r.line().contains("a@x") && r.line().contains("b@x") && r.line().contains("2 live")
        );
        let r = Reconciled::Restarted {
            from: "a@x".into(),
            to: "b@x".into(),
        };
        assert!(r
            .line()
            .starts_with("Codex app-server restarted: now signed in as b@x"));
    }

    #[test]
    fn codex_home_is_the_sockets_grandparent() {
        let cx = CodexHostConfig {
            enabled: true,
            socket_path: std::path::PathBuf::from(
                "/tmp/h/app-server-control/app-server-control.sock",
            ),
            binary: None,
        };
        assert_eq!(codex_home(&cx), std::path::PathBuf::from("/tmp/h"));
    }
}
