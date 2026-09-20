//! ADR-016 §Codex outbound: mirror a bare `codex` via Codex's own hook system.
//!
//! The app-server cannot observe a thread a bare `codex` owns (ADR-016 amendment
//! 2026-09-20: `thread/resume` → `no rollout found`, permanently). Codex's hooks can:
//! they run **inside** that process. So ctm installs a user-level hook file and the
//! daemon keeps it trusted, exactly as it installs Claude Code's hooks and OpenCode's
//! plugin. Inbound (Telegram → Codex) still goes through the app-server's `turn/start`.
//!
//! Everything here was established by executed spikes against Codex 0.155.1:
//!
//! - **`~/.codex/hooks.json`** (honouring `CODEX_HOME`) is a *user-level* hook source;
//!   no plugin or marketplace is needed and it is independent of project trust.
//! - Hooks **do** run in a bare TUI (the operator's own screenshot showed Codex
//!   clamping a SessionEnd hook timeout), and the payload's **`session_id` is the
//!   app-server `threadId`** — verified equal to the id in the TUI footer. That is what
//!   lets one Telegram topic carry both directions.
//! - `async: true` makes a hook non-blocking, which is what ADR-014's PR-E lesson
//!   requires: ctm observes, it never pre-empts Codex's own prompt.
//! - A new or changed hook file is **untrusted** until reviewed ("Hooks need review").
//!   ctm does not reimplement Codex's hash: it asks the app-server (`hooks/list`,
//!   which reports `key`, `currentHash`, `trustStatus`) and persists that value through
//!   Codex's own config RPC (`config/batchWrite`, `mergeStrategy: "replace"`), so the
//!   file stays Codex-owned and format-preserving. Verified idempotent.
//! - **Approvals do NOT come through hooks.** `PermissionRequest` has no request id, so
//!   a decision cannot be tied to the request it answers; answering by keystroke would
//!   be blind injection into an unknown screen (ADR-014's own failure class — Codex's
//!   review of the design made the same objection). Approvals instead come over the
//!   app-server, which has request ids and atomic resolution — see `shell.rs`, which
//!   makes a plain `codex` join it.

use crate::config::CodexHostConfig;
use crate::error::{AppError, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Events ctm observes, in the order they are written. All are non-blocking.
///
/// `SessionEnd` is deliberately included even though Codex always runs it
/// synchronously (and clamps it to 3s): it is what closes the Telegram topic when the
/// operator quits, and the forwarder is a socket write that returns in microseconds.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SessionEnd",
];

/// Marker on every entry ctm owns, so a merge can tell ctm's hooks from the user's.
const OWNER: &str = "ctm";

/// `~/.codex/hooks.json`, honouring `CODEX_HOME` the way Codex does.
pub fn hooks_path() -> PathBuf {
    codex_home().join("hooks.json")
}

pub fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => crate::config::home_dir().join(".codex"),
    }
}

/// One hook entry group for `event`, invoking `ctm codex-hook`.
fn group_for(event: &str, exe: &Path) -> Value {
    json!({
        "matcher": "",
        "id": format!("{OWNER}:{}", event.to_lowercase()),
        "hooks": [{
            "type": "command",
            "command": format!("{} codex-hook", shell_quote(&exe.to_string_lossy())),
            // Non-blocking everywhere Codex allows it (SessionEnd is always sync).
            "async": event != "SessionEnd",
            // Generous enough for a local socket write, short enough never to stall a
            // turn if the daemon is wedged. SessionEnd is clamped to 3s by Codex.
            "timeout": 5,
        }],
    })
}

/// Quote a path for a `/bin/sh`-style command string.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Is this group one of ctm's (by `id`, or by a command that invokes `ctm codex-hook`)?
fn is_ours(group: &Value) -> bool {
    if group
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.starts_with(&format!("{OWNER}:")))
    {
        return true;
    }
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.contains("codex-hook"))
            })
        })
}

/// The hooks.json ctm wants, merging ctm's entries into `existing` and preserving every
/// entry ctm does not own (the operator may have their own hooks in this file).
///
/// ctm's entries are placed FIRST within each event so an observation hook runs before
/// a user hook that might block the turn.
pub fn merged(existing: Option<&Value>, exe: &Path) -> Value {
    let mut hooks: Map<String, Value> = existing
        .and_then(|v| v.get("hooks"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    for event in EVENTS {
        let kept: Vec<Value> = hooks
            .get(*event)
            .and_then(Value::as_array)
            .map(|groups| groups.iter().filter(|g| !is_ours(g)).cloned().collect())
            .unwrap_or_default();
        let mut groups = vec![group_for(event, exe)];
        groups.extend(kept);
        hooks.insert((*event).to_string(), Value::Array(groups));
    }
    // An event ctm no longer uses must not keep a stale ctm entry.
    let stale: Vec<String> = hooks
        .iter()
        .filter(|(k, _)| !EVENTS.contains(&k.as_str()))
        .filter_map(|(k, v)| {
            let arr = v.as_array()?;
            arr.iter().any(is_ours).then(|| k.clone())
        })
        .collect();
    for event in stale {
        let kept: Vec<Value> = hooks[&event]
            .as_array()
            .map(|groups| groups.iter().filter(|g| !is_ours(g)).cloned().collect())
            .unwrap_or_default();
        if kept.is_empty() {
            hooks.remove(&event);
        } else {
            hooks.insert(event, Value::Array(kept));
        }
    }

    let mut root = existing
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    root.insert("hooks".into(), Value::Object(hooks));
    root.entry("description").or_insert(Value::String(
        "Codex lifecycle hooks. Entries marked ctm:* are managed by ctm (claude-telegram-mirror); edits to them are overwritten.".into(),
    ));
    Value::Object(root)
}

/// Serialised form written to disk (stable, pretty, newline-terminated).
pub fn render(existing: Option<&Value>, exe: &Path) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&merged(existing, exe)).unwrap_or_default()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HooksState {
    Installed,
    Updated,
    Unchanged,
}

/// Write `hooks.json` if ctm's entries are missing or stale. Never discards other hooks.
pub fn ensure(exe: &Path) -> Result<HooksState> {
    ensure_at(&hooks_path(), exe)
}

pub fn ensure_at(path: &Path, exe: &Path) -> Result<HooksState> {
    let raw = std::fs::read_to_string(path).ok();
    let existing: Option<Value> = raw.as_deref().and_then(|s| serde_json::from_str(s).ok());
    if raw.is_some() && existing.is_none() {
        return Err(AppError::Config(format!(
            "{} exists but is not valid JSON — refusing to overwrite it",
            path.display()
        )));
    }
    let want = render(existing.as_ref(), exe);
    if raw.as_deref() == Some(want.as_str()) {
        return Ok(HooksState::Unchanged);
    }
    let dir = path
        .parent()
        .ok_or_else(|| AppError::Config("hooks path has no parent".into()))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| AppError::Config(format!("cannot create {}: {e}", dir.display())))?;
    let tmp = dir.join(format!(".hooks.json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &want)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            AppError::Config(format!("cannot write {}: {e}", path.display()))
        })?;
    Ok(if raw.is_some() {
        HooksState::Updated
    } else {
        HooksState::Installed
    })
}

/// Remove ctm's entries, leaving any other hooks intact. Returns true if it changed.
pub fn remove_at(path: &Path) -> Result<bool> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    let Ok(mut doc) = serde_json::from_str::<Value>(&raw) else {
        return Ok(false);
    };
    let Some(hooks) = doc.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    let mut changed = false;
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let kept: Vec<Value> = hooks[&event]
            .as_array()
            .map(|g| g.iter().filter(|g| !is_ours(g)).cloned().collect())
            .unwrap_or_default();
        let before = hooks[&event].as_array().map(Vec::len).unwrap_or(0);
        if kept.len() != before {
            changed = true;
        }
        if kept.is_empty() {
            hooks.remove(&event);
        } else {
            hooks.insert(event, Value::Array(kept));
        }
    }
    if changed {
        let out = format!(
            "{}\n",
            serde_json::to_string_pretty(&doc).unwrap_or_default()
        );
        std::fs::write(path, out)
            .map_err(|e| AppError::Config(format!("cannot write {}: {e}", path.display())))?;
    }
    Ok(changed)
}

// ----------------------------------------------------------------------- trust

/// One hook as `hooks/list` reports it.
#[derive(Debug, Clone)]
pub struct ListedHook {
    pub key: String,
    pub current_hash: String,
    pub trusted: bool,
}

/// Do these two paths name the same file? Compares canonical forms first, because the
/// app-server reports a resolved path (macOS turns `/tmp/...` into `/private/tmp/...`,
/// and a symlinked HOME resolves likewise) while ctm holds the unresolved one.
fn same_file(reported: &str, ours: &Path) -> bool {
    if reported == ours.to_string_lossy() {
        return true;
    }
    match (std::fs::canonicalize(reported), std::fs::canonicalize(ours)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Parse a `hooks/list` result into ctm's own hooks (by source path + command).
pub fn ours_from_list(result: &Value, hooks_file: &Path) -> Vec<ListedHook> {
    let mut out = Vec::new();
    for cwd_entry in result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for h in cwd_entry
            .get("hooks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let source_path = h
                .get("sourcePath")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let command = h.get("command").and_then(Value::as_str).unwrap_or_default();
            if !command.contains("codex-hook") || !same_file(source_path, hooks_file) {
                continue;
            }
            let (Some(key), Some(hash)) = (
                h.get("key").and_then(Value::as_str),
                h.get("currentHash").and_then(Value::as_str),
            ) else {
                continue;
            };
            if out.iter().any(|o: &ListedHook| o.key == key) {
                continue; // same hook reported under several cwds
            }
            out.push(ListedHook {
                key: key.to_string(),
                current_hash: hash.to_string(),
                trusted: h.get("trustStatus").and_then(Value::as_str) == Some("trusted"),
            });
        }
    }
    out
}

/// The `config/batchWrite` edit that trusts one hook.
///
/// `keyPath` is a dotted TOML path whose middle segment is a quoted key (it contains
/// `/`, `.` and `:`), which is why it is serialised with `to_string` on a JSON string.
pub fn trust_edit(h: &ListedHook) -> Value {
    json!({
        "keyPath": format!("hooks.state.{}.trusted_hash", Value::String(h.key.clone())),
        "value": h.current_hash,
        "mergeStrategy": "replace",
    })
}

/// Is the installed hook file already exactly what this binary would write?
pub fn ensure_is_current(exe: &Path) -> bool {
    let path = hooks_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return false;
    };
    let existing: Option<Value> = serde_json::from_str(&raw).ok();
    existing.is_some() && raw == render(existing.as_ref(), exe)
}

/// How many of ctm's hooks the app-server reports as not yet trusted.
pub async fn untrusted_count(cx: &CodexHostConfig) -> Result<usize> {
    let mut rpc = super::codex_rpc::Rpc::connect(&cx.socket_path).await?;
    let listed = rpc.call("hooks/list", json!({})).await?;
    Ok(ours_from_list(&listed, &hooks_path())
        .iter()
        .filter(|h| !h.trusted)
        .count())
}

/// Install the hook file and trust ctm's entries through Codex's own RPCs.
///
/// Returns `(file state, number of hooks newly trusted)`. Trust needs a running
/// app-server; when it is not reachable the file is still written and the next daemon
/// pass (or `ctm doctor --fix`) completes the trust step.
pub async fn provision(cx: &CodexHostConfig, exe: &Path) -> Result<(HooksState, usize)> {
    let state = ensure(exe)?;
    let trusted = trust_via_app_server(cx).await?;
    Ok((state, trusted))
}

/// Ask the app-server for ctm's hooks and persist Codex's own `currentHash` as
/// `trusted_hash` for any that are not trusted yet. Returns how many were written.
pub async fn trust_via_app_server(cx: &CodexHostConfig) -> Result<usize> {
    let path = hooks_path();
    let mut rpc = super::codex_rpc::Rpc::connect(&cx.socket_path).await?;
    let listed = rpc.call("hooks/list", json!({})).await?;
    let ours = ours_from_list(&listed, &path);
    let untrusted: Vec<&ListedHook> = ours.iter().filter(|h| !h.trusted).collect();
    if untrusted.is_empty() {
        return Ok(0);
    }
    let edits: Vec<Value> = untrusted.iter().map(|h| trust_edit(h)).collect();
    rpc.call("config/batchWrite", json!({ "edits": edits }))
        .await?;
    Ok(untrusted.len())
}

/// How often the daemon re-checks the hook file and its trust state.
pub const KEEPER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Daemon task: install ctm's Codex hooks and keep them installed and trusted.
///
/// Trust needs the app-server, which `codex::run` starts and keeps alive, so a failure
/// here is never fatal — the next pass (or `ctm doctor --fix`) completes it.
pub async fn run_keeper(cx: CodexHostConfig) {
    let Ok(exe) = std::env::current_exe() else {
        tracing::warn!("Codex hooks: cannot resolve ctm's own path; not installing hooks");
        return;
    };
    let mut announced_absent = false;
    loop {
        if super::detect::codex_present(cx.binary.as_deref()) {
            announced_absent = false;
            match ensure(&exe) {
                Ok(HooksState::Unchanged) => {}
                Ok(state) => tracing::info!(
                    ?state,
                    path = %hooks_path().display(),
                    "Codex hooks installed — bare `codex` sessions mirror out"
                ),
                Err(e) => tracing::warn!(error = %e, "Codex hooks could not be written"),
            }
            match trust_via_app_server(&cx).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(count = n, "Codex hooks trusted via config/batchWrite"),
                Err(e) => tracing::debug!(error = %e, "Codex hooks: trust deferred"),
            }
        } else if !announced_absent {
            tracing::info!("Codex not installed — hooks not written");
            announced_absent = true;
        }
        tokio::time::sleep(KEEPER_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/home/u/.local/bin/ctm")
    }

    #[test]
    fn every_observed_event_is_written_and_only_session_end_is_synchronous() {
        let doc = merged(None, &exe());
        let hooks = doc["hooks"].as_object().unwrap();
        for e in EVENTS {
            let g = &hooks[*e][0];
            assert_eq!(g["hooks"][0]["type"], "command");
            assert_eq!(
                g["hooks"][0]["command"],
                "/home/u/.local/bin/ctm codex-hook"
            );
            assert_eq!(
                g["hooks"][0]["async"],
                Value::Bool(*e != "SessionEnd"),
                "{e} async flag"
            );
        }
        // ADR-014/PR-E: ctm must never register a blocking decision hook.
        assert!(!hooks.contains_key("PermissionRequest"));
    }

    #[test]
    fn merge_preserves_foreign_hooks_and_replaces_only_ctm_entries() {
        let existing = json!({
            "description": "mine",
            "hooks": {
                "PreToolUse": [
                    {"id": "user:audit", "hooks": [{"type": "command", "command": "/usr/local/bin/audit"}]},
                    {"id": "ctm:pretooluse", "hooks": [{"type": "command", "command": "/old/ctm codex-hook"}]}
                ],
                "Notification": [
                    {"id": "user:notify", "hooks": [{"type": "command", "command": "/usr/local/bin/notify"}]}
                ]
            }
        });
        let doc = merged(Some(&existing), &exe());
        let pre = doc["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2, "one ctm entry + the user's");
        assert_eq!(pre[0]["id"], "ctm:pretooluse", "ctm runs first");
        assert_eq!(
            pre[0]["hooks"][0]["command"],
            "/home/u/.local/bin/ctm codex-hook"
        );
        assert_eq!(pre[1]["id"], "user:audit");
        // An event ctm does not use is left exactly as it was.
        assert_eq!(
            doc["hooks"]["Notification"],
            existing["hooks"]["Notification"]
        );
        assert_eq!(doc["description"], "mine");
    }

    #[test]
    fn stale_ctm_entry_on_an_unused_event_is_dropped_but_foreign_ones_stay() {
        let existing = json!({"hooks": {"Interrupt": [
            {"id": "ctm:interrupt", "hooks": [{"type": "command", "command": "/old/ctm codex-hook"}]},
            {"id": "user:x", "hooks": [{"type": "command", "command": "/bin/true"}]}
        ]}});
        let doc = merged(Some(&existing), &exe());
        let it = doc["hooks"]["Interrupt"].as_array().unwrap();
        assert_eq!(it.len(), 1);
        assert_eq!(it[0]["id"], "user:x");
    }

    #[test]
    fn ensure_is_idempotent_and_never_destroys_a_foreign_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        assert_eq!(ensure_at(&path, &exe()).unwrap(), HooksState::Installed);
        assert_eq!(ensure_at(&path, &exe()).unwrap(), HooksState::Unchanged);
        assert_eq!(
            ensure_at(&path, Path::new("/new/ctm")).unwrap(),
            HooksState::Updated
        );
        // Not-JSON is refused rather than clobbered.
        std::fs::write(&path, "# not json\n").unwrap();
        assert!(ensure_at(&path, &exe()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# not json\n");
    }

    #[test]
    fn remove_strips_only_ctm_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({"hooks": {
                "PreToolUse": [
                    {"id": "ctm:pretooluse", "hooks": [{"type": "command", "command": "/x/ctm codex-hook"}]},
                    {"id": "user:audit", "hooks": [{"type": "command", "command": "/usr/local/bin/audit"}]}
                ],
                "Stop": [{"id": "ctm:stop", "hooks": [{"type": "command", "command": "/x/ctm codex-hook"}]}]
            }}))
            .unwrap(),
        )
        .unwrap();
        assert!(remove_at(&path).unwrap());
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(doc["hooks"]["PreToolUse"][0]["id"], "user:audit");
        assert!(doc["hooks"].get("Stop").is_none());
        assert!(!remove_at(&path).unwrap(), "second removal is a no-op");
    }

    #[test]
    fn paths_with_spaces_are_quoted_for_the_shell() {
        let doc = merged(None, Path::new("/Users/a b/.local/bin/ctm"));
        assert_eq!(
            doc["hooks"]["Stop"][0]["hooks"][0]["command"],
            "'/Users/a b/.local/bin/ctm' codex-hook"
        );
    }

    // Verbatim `hooks/list` result from the 0.155.1 spike (two ctm hooks plus a
    // plugin's, reported under two cwds).
    const LIST: &str = r#"{"data":[
      {"cwd":"/tmp/p1","hooks":[
        {"key":"/home/u/.codex/hooks.json:session_start:0:0","eventName":"sessionStart","handlerType":"command","command":"/home/u/.local/bin/ctm codex-hook","async":true,"matcher":"","timeoutSec":5,"sourcePath":"/home/u/.codex/hooks.json","source":"user","pluginId":null,"enabled":true,"isManaged":false,"currentHash":"sha256:aaa","trustStatus":"untrusted"},
        {"key":"/home/u/.codex/hooks.json:stop:0:0","eventName":"stop","handlerType":"command","command":"/home/u/.local/bin/ctm codex-hook","async":true,"matcher":null,"timeoutSec":5,"sourcePath":"/home/u/.codex/hooks.json","source":"user","pluginId":null,"enabled":true,"isManaged":false,"currentHash":"sha256:bbb","trustStatus":"trusted"},
        {"key":"other@mkt:hooks/codex-hooks.json:stop:0:0","eventName":"stop","handlerType":"command","command":"node /plugin/hook.js","async":false,"matcher":null,"timeoutSec":600,"sourcePath":"/home/u/.codex/plugins/cache/other/hooks/codex-hooks.json","source":"plugin","pluginId":"other@mkt","enabled":true,"isManaged":false,"currentHash":"sha256:ccc","trustStatus":"trusted"}]},
      {"cwd":"/tmp/p2","hooks":[
        {"key":"/home/u/.codex/hooks.json:session_start:0:0","eventName":"sessionStart","handlerType":"command","command":"/home/u/.local/bin/ctm codex-hook","async":true,"matcher":"","timeoutSec":5,"sourcePath":"/home/u/.codex/hooks.json","source":"user","pluginId":null,"enabled":true,"isManaged":false,"currentHash":"sha256:aaa","trustStatus":"untrusted"}]}]}"#;

    #[test]
    fn list_yields_only_ctms_hooks_deduped_across_cwds() {
        let v: Value = serde_json::from_str(LIST).unwrap();
        let ours = ours_from_list(&v, Path::new("/home/u/.codex/hooks.json"));
        assert_eq!(ours.len(), 2, "deduped, plugin hook excluded");
        assert_eq!(ours[0].key, "/home/u/.codex/hooks.json:session_start:0:0");
        assert!(!ours[0].trusted);
        assert!(ours[1].trusted);
        // A different hooks.json path (another CODEX_HOME) matches nothing.
        assert!(ours_from_list(&v, Path::new("/elsewhere/hooks.json")).is_empty());
    }

    #[test]
    fn a_resolved_source_path_still_matches_our_unresolved_one() {
        // The app-server reports a resolved path while ctm holds an unresolved one
        // (macOS turns /tmp into /private/tmp; a symlinked HOME does the same). Build
        // the symlink rather than assuming the platform provides one — asserting that
        // `/tmp` is a symlink passes on macOS and fails on Linux, which is how this
        // test broke CI.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let unresolved = link.join("hooks.json");
        std::fs::write(&unresolved, "{}").unwrap();
        let resolved = std::fs::canonicalize(&unresolved).unwrap();
        assert_ne!(resolved, unresolved, "the symlinked path differs by construction");

        let listed = json!({"data":[{"hooks":[{
            "key": "k:stop:0:0",
            "command": "/x/ctm codex-hook",
            "sourcePath": resolved.to_string_lossy(),
            "currentHash": "sha256:zzz",
            "trustStatus": "untrusted"
        }]}]});
        assert_eq!(
            ours_from_list(&listed, &unresolved).len(),
            1,
            "the same file reached by two paths is one hook"
        );
    }

    #[test]
    fn trust_edit_quotes_the_key_and_replaces() {
        let v: Value = serde_json::from_str(LIST).unwrap();
        let ours = ours_from_list(&v, Path::new("/home/u/.codex/hooks.json"));
        let e = trust_edit(&ours[0]);
        assert_eq!(
            e["keyPath"],
            "hooks.state.\"/home/u/.codex/hooks.json:session_start:0:0\".trusted_hash"
        );
        assert_eq!(e["value"], "sha256:aaa");
        assert_eq!(e["mergeStrategy"], "replace");
    }
}
