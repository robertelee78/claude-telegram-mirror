//! ADR-016: the daemon's ONLY host-aware seam.
//!
//! Everything upstream of `BridgeMessage` is host-neutral. This module is where the
//! daemon decides, for a given session, whether a Telegram-originated action goes to
//! Claude Code over tmux (ADR-004, unchanged) or to a **host observer** over the
//! observer's own socket connection (`HostInject` / `QuestionResponse`).
//!
//! Design constraints, all verified by the ADR-016 spikes:
//! - The observer is a client of ctm's Unix socket, exactly like `ctm hook`. The
//!   daemon writes to it the same way it already writes an `ApprovalResponse` to the
//!   originating hook client (`callback_handlers.rs`, S-2 routing).
//! - Delivery is **fire-and-forget**. The host's own resolution signal
//!   (`serverRequest/resolved`, `permission.replied`, `question.replied`) comes back
//!   through the observer as an ordinary `tool_result`, and THAT is what finalizes
//!   Telegram state — never our own write succeeding. A write that lands after the
//!   operator already answered at the terminal is harmless on both hosts.
//! - `HostKind` is recorded at `SessionStart` from observer-supplied metadata and
//!   never inferred later (ADR-016 Consequences).

use super::*;
use crate::types::HostKind;
use tokio::io::AsyncWriteExt;

/// Resolve a session's host kind: in-memory cache, then the DB row, defaulting to
/// Claude Code for sessions that predate ADR-016 (NULL column) or arrive without the
/// `hostKind` metadata field (older hook binaries).
pub(super) async fn session_host_kind(ctx: &HandlerContext, session_id: &str) -> HostKind {
    if let Some(k) = ctx.session_hosts.read().await.get(session_id) {
        return *k;
    }
    let sid = session_id.to_string();
    let kind = ctx
        .db_op(move |sess| sess.get_session(&sid).ok().flatten().map(|s| s.host_kind()))
        .await
        .unwrap_or_default();
    ctx.session_hosts
        .write()
        .await
        .insert(session_id.to_string(), kind);
    kind
}

/// Called from `handle_session_start`: cache the host kind and, for native-API hosts,
/// remember which socket client is the observer so the daemon can write back to it.
///
/// `_client_id` is injected into metadata by `socket.rs` on every inbound line, so an
/// observer that reconnects under a new id is re-learned on its next `session_start`
/// (which observers re-send on reconnect, mirroring how hooks resend it per invocation).
/// Which socket client — if any — should be remembered as this session's observer, the
/// connection a Telegram reply is delivered to.
///
/// Pure so the rule can be tested directly. Only a long-lived, native-API observer
/// qualifies: a `ctm codex-hook` process has already exited by the time anything needs
/// delivering, and Claude Code is injected through tmux, not a socket.
/// Which live session a "the TUI for this directory quit" report refers to.
///
/// A Codex TUI never learns its own thread id, so `ctm codex-exited` reports only a
/// directory; the newest live session of that host in that directory is the one that
/// just ended. Paths are compared canonically because a shell reports `$PWD` while the
/// host recorded a resolved path (`/tmp` vs `/private/tmp`).
///
/// Pure, so the choice is testable without a daemon.
pub(super) fn resolve_exited_session<'a>(
    sessions: impl IntoIterator<Item = (&'a str, crate::types::HostKind, Option<&'a str>, &'a str)>,
    kind: crate::types::HostKind,
    dir: &str,
) -> Option<String> {
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
    let want = canon(dir);
    let mut best: Option<(&str, &str)> = None;
    for (id, host, project_dir, last_activity) in sessions {
        if host != kind {
            continue;
        }
        let Some(pd) = project_dir else { continue };
        if pd != dir && canon(pd) != want {
            continue;
        }
        if best.is_none_or(|(_, seen)| last_activity > seen) {
            best = Some((id, last_activity));
        }
    }
    best.map(|(id, _)| id.to_string())
}

pub(super) fn observer_binding<'a>(meta: &crate::types::MessageMetadata<'a>) -> Option<&'a str> {
    if !meta.host_kind().uses_native_api() {
        return None;
    }
    if meta.host_transport() == Some("hook") {
        return None;
    }
    meta.client_id()
}

pub(super) async fn record_session_host(ctx: &HandlerContext, msg: &BridgeMessage) {
    let meta = msg.meta();
    let kind = meta.host_kind();
    ctx.session_hosts
        .write()
        .await
        .insert(msg.session_id.clone(), kind);
    // ADR-016 §Codex outbound: hook-sourced messages come from a short-lived `ctm
    // codex-hook` process that exits immediately. Registering it as the session's
    // observer would point injection at a dead socket, so the app-server observer
    // (which stays connected) keeps that role.
    if kind.uses_native_api() {
        match observer_binding(&meta) {
            Some(cid) => {
                ctx.session_host_clients
                    .write()
                    .await
                    .insert(msg.session_id.clone(), cid.to_string());
                tracing::info!(
                    session_id = %msg.session_id,
                    host = %kind,
                    client_id = cid,
                    "ADR-016: host observer registered for session"
                );
            }
            // A hook-sourced announcement is the normal case here and binds nothing —
            // the observer's own announcement does that, whichever arrives first.
            None if meta.host_transport() == Some("hook") => {}
            None => {
                // Only reachable if a message bypassed socket.rs's `_client_id` injection —
                // e.g. a unit test constructing a BridgeMessage directly. Real observers
                // always arrive with one. Warn loudly: without it, nothing can be delivered.
                tracing::warn!(
                    session_id = %msg.session_id,
                    host = %kind,
                    "ADR-016: native host session_start arrived without _client_id — daemon->observer delivery impossible"
                );
            }
        }
    }
}

/// Forget a session's host bindings (session end / cleanup).
pub(super) async fn forget_session_host(ctx: &HandlerContext, session_id: &str) {
    ctx.session_hosts.write().await.remove(session_id);
    ctx.session_host_clients.write().await.remove(session_id);
}

/// Write a `BridgeMessage` to the observer client that owns `session_id`.
///
/// Returns `false` if no observer is registered or its connection is gone. Unlike the
/// approval-response path there is deliberately NO broadcast fallback: a `HostInject`
/// broadcast to every socket client would be delivered to unrelated hooks/observers,
/// and — unlike an approval response — it is not idempotent to receive.
pub(super) async fn send_to_host_observer(
    ctx: &HandlerContext,
    session_id: &str,
    msg: &BridgeMessage,
) -> bool {
    let client_id = ctx
        .session_host_clients
        .read()
        .await
        .get(session_id)
        .cloned();
    let Some(client_id) = client_id else {
        tracing::warn!(
            session_id,
            msg_type = %msg.msg_type,
            "ADR-016: no host observer registered for session — cannot deliver"
        );
        return false;
    };
    let json = match serde_json::to_string(msg) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(error = %e, "ADR-016: failed to serialise observer message");
            return false;
        }
    };
    let line = format!("{json}\n");
    let guard = ctx.socket_clients.lock().await;
    let Some(writer) = guard.get(&client_id) else {
        drop(guard);
        tracing::warn!(
            session_id,
            client_id,
            msg_type = %msg.msg_type,
            "ADR-016: host observer client disconnected — cannot deliver"
        );
        return false;
    };
    let mut w = writer.lock().await;
    match w.write_all(line.as_bytes()).await {
        Ok(()) => {
            tracing::info!(
                session_id,
                client_id,
                msg_type = %msg.msg_type,
                "ADR-016: delivered to host observer"
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                session_id,
                client_id,
                error = %e,
                "ADR-016: write to host observer failed"
            );
            false
        }
    }
}

fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Fetch the host's own session id for a ctm session, if the row carries one in its
/// metadata JSON. Observers set it at `session_start`; it is stored verbatim.
async fn host_session_id_for(ctx: &HandlerContext, session_id: &str) -> Option<String> {
    let sid = session_id.to_string();
    ctx.db_op(move |sess| {
        sess.get_session(&sid)
            .ok()
            .flatten()
            .and_then(|s| s.metadata)
            .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
            .and_then(|v| v.get("hostSessionId")?.as_str().map(str::to_string))
    })
    .await
}

/// Deliver user text or a control action into a native-API host session.
///
/// `action` is one of `"text"`, `"interrupt"`, `"abort"`, `"slash"`. Returns whether the
/// message reached the observer's socket — NOT whether the host accepted it; that comes
/// back asynchronously as the host's own events.
pub(super) async fn host_inject(
    ctx: &HandlerContext,
    session_id: &str,
    action: &str,
    text: &str,
) -> bool {
    let mut metadata = serde_json::Map::new();
    metadata.insert("action".into(), serde_json::Value::String(action.into()));
    metadata.insert(
        "source".into(),
        serde_json::Value::String("telegram".into()),
    );
    if let Some(hsid) = host_session_id_for(ctx, session_id).await {
        metadata.insert("hostSessionId".into(), serde_json::Value::String(hsid));
    }
    let msg = BridgeMessage {
        msg_type: MessageType::HostInject,
        session_id: session_id.to_string(),
        timestamp: now_ts(),
        content: text.to_string(),
        metadata: Some(metadata),
    };
    send_to_host_observer(ctx, session_id, &msg).await
}

/// Deliver a Submit-All answer set for a structured question to the host observer.
///
/// `answers` is a JSON array of arrays of strings — one inner array per question, in
/// question order, each holding the selected option LABELS (or the free-text answer).
/// This is the shape both hosts accept natively: OpenCode `POST /question/{id}/reply
/// {answers: string[][]}`, and Codex `{answers: {qid: {answers: [..]}}}` after the
/// observer re-keys by question id. Labels, not indices, so the observer never needs
/// ctm's option ordering.
pub(super) async fn host_answer_question(
    ctx: &HandlerContext,
    session_id: &str,
    host_question_id: &str,
    host_session_id: Option<&str>,
    answers: serde_json::Value,
) -> bool {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "questionId".into(),
        serde_json::Value::String(host_question_id.into()),
    );
    metadata.insert("answers".into(), answers);
    metadata.insert(
        "source".into(),
        serde_json::Value::String("telegram".into()),
    );
    let hsid = match host_session_id {
        Some(h) => Some(h.to_string()),
        None => host_session_id_for(ctx, session_id).await,
    };
    if let Some(h) = hsid {
        metadata.insert("hostSessionId".into(), serde_json::Value::String(h));
    }
    let msg = BridgeMessage {
        msg_type: MessageType::QuestionResponse,
        session_id: session_id.to_string(),
        timestamp: now_ts(),
        content: String::new(),
        metadata: Some(metadata),
    };
    send_to_host_observer(ctx, session_id, &msg).await
}

/// Native-host replacement for the tmux text path in `handle_telegram_text`.
///
/// Preserves the exact user-facing behaviour of the tmux path — `cc <cmd>` slash
/// commands, interrupt/kill words, pending free-text question answers, echo suppression,
/// and the `MAX_INJECT_CHARS` cap — but delivers via `HostInject` instead of keystrokes.
/// Wording avoids "tmux" and "Claude", because neither applies here.
pub(super) async fn handle_native_host_text(
    ctx: &HandlerContext,
    session: &crate::session::Session,
    text: &str,
    thread_id: i64,
) {
    let host = session.host_kind();
    let label = host.label();

    // cc command prefix: "cc clear" -> "/clear"
    if text.to_lowercase().starts_with("cc ") {
        let command = format!("/{}", text[3..].trim());
        add_echo_key(ctx, &session.id, &command).await;
        let _ = host_inject(ctx, &session.id, "slash", &command).await;
        return;
    }

    // BUG-004: Interrupt commands
    if is_interrupt_command(text) {
        let ok = host_inject(ctx, &session.id, "interrupt", "").await;
        let msg_text = if ok {
            format!("\u{23F8}\u{FE0F} *Interrupt sent*\n\n_{label} should pause the current operation._")
        } else {
            format!("\u{26A0}\u{FE0F} *Could not send interrupt*\n\nNo {label} observer connected.")
        };
        ctx.bot
            .send_message(
                &msg_text,
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                Some(thread_id),
            )
            .await;
        return;
    }

    // BUG-004: Kill commands
    if is_kill_command(text) {
        let ok = host_inject(ctx, &session.id, "abort", "").await;
        let msg_text = if ok {
            format!("\u{1F6D1} *Abort sent*\n\n_{label} should stop the current turn._")
        } else {
            format!("\u{26A0}\u{FE0F} *Could not send abort*\n\nNo {label} observer connected.")
        };
        ctx.bot
            .send_message(
                &msg_text,
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                Some(thread_id),
            )
            .await;
        return;
    }

    // Pending AskUserQuestion free-text answer (tentative until Submit All — host-neutral)
    if telegram_handlers::handle_free_text_answer(ctx, &session.id, text).await {
        return;
    }

    // BUG-011: echo prevention
    add_echo_key(ctx, &session.id, text.trim()).await;

    // FR32: cap length (same limit as the tmux path so behaviour is host-independent)
    let inject_text: std::borrow::Cow<'_, str> = if text.chars().count() > MAX_INJECT_CHARS {
        tracing::warn!(
            chars = text.chars().count(),
            max = MAX_INJECT_CHARS,
            host = %host,
            "Telegram text truncated before host injection"
        );
        ctx.bot
            .send_message(
                &format!("Message truncated to {MAX_INJECT_CHARS} characters"),
                None,
                Some(thread_id),
            )
            .await;
        std::borrow::Cow::Owned(truncate(text, MAX_INJECT_CHARS))
    } else {
        std::borrow::Cow::Borrowed(text)
    };

    if !host_inject(ctx, &session.id, "text", &inject_text).await {
        // ADR-013 D1/D2 parity: warn on every failed delivery, no suppression.
        tracing::warn!(
            session_id = %session.id,
            host = %host,
            "ADR-016: host injection failed — observer not connected"
        );
        ctx.bot
            .send_message(
                &format!(
                    "\u{26A0}\u{FE0F} Reply failed \u{2014} the {label} observer is not connected. Check `ctm doctor`."
                ),
                None,
                Some(thread_id),
            )
            .await;
    }
}

/// ADR-016 §transport arbitration: should this message be mirrored, or is it the
/// duplicate of one that came the other way?
///
/// A Codex session in app-server mode is reported by BOTH paths: ctm's hooks run inside
/// the app-server process, and ctm's observer streams the same thread over the protocol.
/// The protocol path is strictly richer (it carries approvals and per-item events), so
/// once the observer is subscribed it claims the session (`hostTransport: "protocol"`)
/// and hook-sourced *content* is dropped. Before that claim — a bare `codex`, or the
/// first moments of a session — the hooks are the only source and must pass.
///
/// `SessionEnd` is never dropped: it is the signal that closes the topic, it is
/// idempotent in the daemon, and losing it is worse than handling it twice.
pub(super) async fn is_duplicate_transport(ctx: &HandlerContext, msg: &BridgeMessage) -> bool {
    let meta = msg.meta();
    match meta.host_transport() {
        Some("protocol") => {
            ctx.session_transports
                .write()
                .await
                .insert(msg.session_id.clone(), ());
            false
        }
        Some("hook") => {
            if matches!(msg.msg_type, MessageType::SessionEnd) {
                return false;
            }
            let claimed = ctx
                .session_transports
                .read()
                .await
                .contains_key(&msg.session_id);
            if claimed {
                tracing::debug!(
                    session_id = %msg.session_id,
                    msg_type = %msg.msg_type,
                    "ADR-016: hook message dropped — this session is mirrored over the protocol"
                );
            }
            claimed
        }
        _ => false,
    }
}

/// Forget a session's transport claim (session end / cleanup).
pub(super) async fn forget_transport(ctx: &HandlerContext, session_id: &str) {
    ctx.session_transports.write().await.remove(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta_of(v: serde_json::Value) -> BridgeMessage {
        BridgeMessage {
            msg_type: MessageType::SessionStart,
            session_id: "s".into(),
            timestamp: String::new(),
            content: String::new(),
            metadata: v.as_object().cloned(),
        }
    }

    #[test]
    fn only_a_long_lived_native_observer_is_bound_for_delivery() {
        // The app-server observer: this is the connection replies go to.
        let m = meta_of(json!({"hostKind": "codex", "_client_id": "client-1"}));
        assert_eq!(observer_binding(&m.meta()), Some("client-1"));

        // A `ctm codex-hook` process has exited by the time anything needs delivering;
        // binding it would point injection at a dead socket.
        let m = meta_of(json!({
            "hostKind": "codex", "_client_id": "client-2", "hostTransport": "hook"
        }));
        assert_eq!(observer_binding(&m.meta()), None);

        // Claude Code is injected over tmux, never a socket.
        let m = meta_of(json!({"hostKind": "claude_code", "_client_id": "client-3"}));
        assert_eq!(observer_binding(&m.meta()), None);

        // Native host, but the transport did not identify itself: nothing to bind.
        let m = meta_of(json!({"hostKind": "opencode"}));
        assert_eq!(observer_binding(&m.meta()), None);
    }

    #[test]
    fn the_newest_live_session_in_that_directory_is_the_one_that_exited() {
        use crate::types::HostKind::{ClaudeCode, Codex, OpenCode};
        let rows = vec![
            ("older", Codex, Some("/work/proj"), "2026-09-20T08:00:00Z"),
            ("newest", Codex, Some("/work/proj"), "2026-09-20T09:30:00Z"),
            (
                "other-dir",
                Codex,
                Some("/work/else"),
                "2026-09-20T10:00:00Z",
            ),
            (
                "other-host",
                OpenCode,
                Some("/work/proj"),
                "2026-09-20T11:00:00Z",
            ),
            (
                "claude",
                ClaudeCode,
                Some("/work/proj"),
                "2026-09-20T12:00:00Z",
            ),
            ("no-dir", Codex, None, "2026-09-20T13:00:00Z"),
        ];
        assert_eq!(
            resolve_exited_session(rows.clone(), Codex, "/work/proj"),
            Some("newest".into()),
            "several TUIs can share a directory; the newest is the one that just quit"
        );
        assert_eq!(
            resolve_exited_session(rows.clone(), Codex, "/nowhere"),
            None
        );
        assert_eq!(
            resolve_exited_session(rows, OpenCode, "/work/proj"),
            Some("other-host".into()),
            "hosts do not resolve each other's exits"
        );
    }

    #[test]
    fn a_reported_directory_matches_its_resolved_form() {
        use crate::types::HostKind::Codex;
        // The shell reports $PWD while the host recorded a resolved path (/tmp on macOS
        // is /private/tmp). Build the symlink rather than assuming the platform has one.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let resolved = std::fs::canonicalize(&link).unwrap();
        let rows = vec![(
            "s1",
            Codex,
            Some(resolved.to_str().unwrap()),
            "2026-09-20T09:00:00Z",
        )];
        assert_eq!(
            resolve_exited_session(rows, Codex, link.to_str().unwrap()),
            Some("s1".into())
        );
    }

    #[test]
    fn an_opencode_observer_is_bound_too() {
        let m = meta_of(json!({"hostKind": "opencode", "_client_id": "client-9"}));
        assert_eq!(observer_binding(&m.meta()), Some("client-9"));
    }
}
