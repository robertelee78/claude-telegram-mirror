//! `ctm codex-hook` — the command Codex runs for each lifecycle event.
//!
//! It reads one JSON payload on stdin, converts it to `BridgeMessage`s, writes them to
//! ctm's socket and exits. It prints `{}` and always exits 0: a mirror must never
//! change what Codex does (ADR-014's PR-E lesson), so a daemon that is down, a socket
//! that is missing or a payload ctm does not understand are all silent no-ops.
//!
//! Payload shapes are the ones Codex 0.155.1 embeds (captured live, and cross-checked
//! against the twelve schemas in the binary):
//! - every event: `session_id`, `transcript_path`, `cwd`, `hook_event_name`
//! - `SessionStart`: + `model`, `permission_mode`, `source`
//! - `UserPromptSubmit`: + `turn_id`, `prompt`
//! - `PreToolUse`: + `turn_id`, `tool_name`, `tool_use_id`, `tool_input`
//! - `PostToolUse`: + those and `tool_response`
//! - `Stop`: + `turn_id`, `stop_hook_active`, `last_assistant_message`
//! - `SessionEnd`: + `reason` (and no `model`)
//!
//! `session_id` is the app-server `threadId` for a root thread (spike-verified against
//! the TUI footer), which is what keeps the hook (outbound) and the app-server
//! (inbound) on one Telegram topic.

use crate::host::link::stamped;
use crate::types::{BridgeMessage, HostKind, MessageType};
use serde_json::{Map, Value};
use std::io::Read;

const KIND: HostKind = HostKind::Codex;

/// Entry point for the CLI subcommand. Never returns an error to the caller.
pub async fn run() -> anyhow::Result<()> {
    let mut raw = String::new();
    // Bounded: a hook payload is small; a runaway stdin must not eat memory.
    let mut limited = std::io::stdin().take(crate::types::MAX_LINE_BYTES as u64);
    let _ = limited.read_to_string(&mut raw);

    if let Ok(payload) = serde_json::from_str::<Value>(&raw) {
        let messages = translate(&payload);
        if !messages.is_empty() {
            if let Ok(cfg) = crate::config::load_config(false) {
                let _ = send(&cfg.socket_path, &messages).await;
            }
        }
    }
    // Neutral output: no decision, no added context, no interference.
    println!("{{}}");
    Ok(())
}

/// Translate one Codex hook payload into daemon messages. Pure; unit-tested.
pub fn translate(p: &Value) -> Vec<BridgeMessage> {
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default();
    let session_id = s("session_id");
    if session_id.is_empty() {
        return vec![];
    }
    let event = s("hook_event_name");
    let mut base = Map::new();
    base.insert(
        "hostSessionId".into(),
        Value::String(session_id.to_string()),
    );
    // Marks the transport so the daemon does not mistake this short-lived client for
    // the app-server observer it delivers injections to (daemon/host_dispatch.rs).
    base.insert("hostTransport".into(), Value::String("hook".into()));
    let msg = |t: MessageType, content: &str, meta: Map<String, Value>| {
        stamped(KIND, t, session_id, content, meta)
    };

    match event {
        "SessionStart" => {
            let mut meta = base;
            if !s("cwd").is_empty() {
                meta.insert("projectDir".into(), Value::String(s("cwd").into()));
            }
            if !s("model").is_empty() {
                meta.insert("model".into(), Value::String(s("model").into()));
            }
            meta.insert("entrypoint".into(), Value::String("cli".into()));
            vec![msg(MessageType::SessionStart, "", meta)]
        }
        "UserPromptSubmit" => {
            let prompt = s("prompt");
            if prompt.is_empty() {
                return vec![];
            }
            vec![msg(MessageType::UserInput, prompt, base)]
        }
        "PreToolUse" => {
            let tool = crate::host::normalize_tool_name(KIND, s("tool_name"));
            let mut meta = base;
            meta.insert("tool".into(), Value::String(tool.clone()));
            if let Some(input) = p.get("tool_input") {
                meta.insert("input".into(), input.clone());
            }
            if !s("tool_use_id").is_empty() {
                meta.insert("toolUseId".into(), Value::String(s("tool_use_id").into()));
            }
            vec![msg(MessageType::ToolStart, &tool, meta)]
        }
        "PostToolUse" => {
            let tool = crate::host::normalize_tool_name(KIND, s("tool_name"));
            let mut meta = base;
            meta.insert("tool".into(), Value::String(tool.clone()));
            if let Some(input) = p.get("tool_input") {
                meta.insert("input".into(), input.clone());
            }
            if !s("tool_use_id").is_empty() {
                meta.insert("toolUseId".into(), Value::String(s("tool_use_id").into()));
            }
            let output = p
                .get("tool_response")
                .map(|r| match r {
                    Value::String(t) => t.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            vec![msg(MessageType::ToolResult, &output, meta)]
        }
        "Stop" => {
            // The only assistant text a hook ever sees: the turn's final message.
            let text = p
                .get("last_assistant_message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if text.trim().is_empty() {
                return vec![];
            }
            vec![msg(MessageType::AgentResponse, text, base)]
        }
        "SessionEnd" => {
            let reason = s("reason");
            let reason = if reason.is_empty() { "ended" } else { reason };
            vec![msg(MessageType::SessionEnd, reason, base)]
        }
        _ => vec![],
    }
}

async fn send(socket_path: &std::path::Path, messages: &[BridgeMessage]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::net::UnixStream::connect(socket_path).await?;
    for m in messages {
        let Ok(json) = serde_json::to_string(m) else {
            continue;
        };
        stream.write_all(format!("{json}\n").as_bytes()).await?;
    }
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured verbatim from Codex 0.155.1 in a bare TUI (scratchpad payloads.jsonl).
    const SESSION_START: &str = r#"{"session_id":"01a0bbe8-61f0-73a2-9617-ea9855a402cb","transcript_path":"/Users/u/.codex/sessions/2026/09/20/rollout-2026-09-20T01-02-43-01a0bbe8-61f0-73a2-9617-ea9855a402cb.jsonl","cwd":"/private/tmp/ctm-hook-test","hook_event_name":"SessionStart","model":"gpt-6-astra","permission_mode":"default","source":"startup"}"#;
    const SESSION_END: &str = r#"{"session_id":"01a0bbe7-110e-79b2-a466-5fdfaba8eabe","transcript_path":"/Users/u/.codex/sessions/2026/09/20/rollout.jsonl","cwd":"/private/tmp/ctm-hook-test","hook_event_name":"SessionEnd","reason":"other"}"#;
    const STOP: &str = r#"{"session_id":"01a0bbe8-61f0-73a2-9617-ea9855a402cb","turn_id":"01a0bbe9-54b4-77e1-bbf0-5fb6860813c6","transcript_path":"/x.jsonl","cwd":"/private/tmp/ctm-hook-test","hook_event_name":"Stop","model":"gpt-6-astra","permission_mode":"default","stop_hook_active":false,"last_assistant_message":"HOOKTEST2"}"#;
    const PRE_TOOL: &str = r#"{"session_id":"01a0bbe8-61f0-73a2-9617-ea9855a402cb","turn_id":"01a0bbe9","transcript_path":"/x.jsonl","cwd":"/p","hook_event_name":"PreToolUse","model":"gpt-6-astra","permission_mode":"default","tool_name":"shell","tool_use_id":"call_1","tool_input":{"command":"echo HOOKTEST2"}}"#;
    const POST_TOOL: &str = r#"{"session_id":"01a0bbe8-61f0-73a2-9617-ea9855a402cb","turn_id":"01a0bbe9","transcript_path":"/x.jsonl","cwd":"/p","hook_event_name":"PostToolUse","model":"gpt-6-astra","permission_mode":"default","tool_name":"shell","tool_use_id":"call_1","tool_input":{"command":"echo HOOKTEST2"},"tool_response":"HOOKTEST2\n"}"#;
    const PROMPT: &str = r#"{"session_id":"01a0bbe8-61f0-73a2-9617-ea9855a402cb","turn_id":"01a0bbe9","transcript_path":"/x.jsonl","cwd":"/p","hook_event_name":"UserPromptSubmit","model":"gpt-6-astra","permission_mode":"default","prompt":"run the shell command: echo HOOKTEST2"}"#;

    fn tr(s: &str) -> Vec<BridgeMessage> {
        translate(&serde_json::from_str(s).unwrap())
    }

    #[test]
    fn session_start_carries_host_kind_thread_id_and_project() {
        let m = tr(SESSION_START);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].msg_type, MessageType::SessionStart);
        assert_eq!(m[0].session_id, "01a0bbe8-61f0-73a2-9617-ea9855a402cb");
        let meta = m[0].meta();
        assert_eq!(meta.host_kind(), HostKind::Codex);
        // The app-server threadId: this is what unifies the topic with injection.
        assert_eq!(
            meta.host_session_id(),
            Some("01a0bbe8-61f0-73a2-9617-ea9855a402cb")
        );
        assert_eq!(meta.project_dir(), Some("/private/tmp/ctm-hook-test"));
    }

    #[test]
    fn hook_messages_are_marked_as_the_hook_transport() {
        // The daemon must not register this short-lived client as the observer it
        // delivers injections to.
        for s in [
            SESSION_START,
            STOP,
            PRE_TOOL,
            POST_TOOL,
            PROMPT,
            SESSION_END,
        ] {
            let m = tr(s);
            assert_eq!(
                m[0].metadata.as_ref().unwrap()["hostTransport"],
                "hook",
                "payload: {s}"
            );
        }
    }

    #[test]
    fn stop_becomes_the_agent_response_and_empty_text_is_dropped() {
        let m = tr(STOP);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].msg_type, MessageType::AgentResponse);
        assert_eq!(m[0].content, "HOOKTEST2");
        let empty = STOP.replace(
            r#""last_assistant_message":"HOOKTEST2""#,
            r#""last_assistant_message":null"#,
        );
        assert!(tr(&empty).is_empty());
    }

    #[test]
    fn tool_events_map_to_start_and_result_with_claude_shaped_names() {
        let start = tr(PRE_TOOL);
        assert_eq!(start[0].msg_type, MessageType::ToolStart);
        // `shell` is Codex's name for a command execution; the summarizer speaks Claude's.
        assert_eq!(start[0].content, "Bash");
        assert_eq!(start[0].meta().tool(), Some("Bash"));
        assert_eq!(
            start[0]
                .meta()
                .input()
                .and_then(|i| i.get("command"))
                .and_then(Value::as_str),
            Some("echo HOOKTEST2")
        );
        assert_eq!(start[0].meta().tool_use_id(), Some("call_1"));
        let done = tr(POST_TOOL);
        assert_eq!(done[0].msg_type, MessageType::ToolResult);
        assert_eq!(done[0].content, "HOOKTEST2\n");
    }

    #[test]
    fn user_prompt_is_forwarded_and_session_end_carries_its_reason() {
        let p = tr(PROMPT);
        assert_eq!(p[0].msg_type, MessageType::UserInput);
        assert_eq!(p[0].content, "run the shell command: echo HOOKTEST2");
        let e = tr(SESSION_END);
        assert_eq!(e[0].msg_type, MessageType::SessionEnd);
        assert_eq!(e[0].content, "other");
    }

    #[test]
    fn unknown_events_and_missing_session_are_ignored() {
        assert!(tr(r#"{"session_id":"s","hook_event_name":"Interrupt"}"#).is_empty());
        assert!(tr(r#"{"hook_event_name":"Stop","last_assistant_message":"x"}"#).is_empty());
        assert!(translate(&Value::Null).is_empty());
    }
}
