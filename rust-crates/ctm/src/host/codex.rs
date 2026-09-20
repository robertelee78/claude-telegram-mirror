//! ADR-016: Codex host observer.
//!
//! Every wire shape here was captured verbatim from Codex CLI 0.155.1 (managed
//! app-server 0.153.2) during the ADR-016 spikes, not read from documentation. The
//! load-bearing facts:
//!
//! - Transport is JSON-RPC 2.0 over **WebSocket over the Unix control socket** created
//!   by `codex app-server daemon start` (`~/.codex/app-server-control/…sock`, 0600).
//!   A bare `codex` started while that daemon is running **auto-joins it** (spike:
//!   its thread appeared in `thread/started` on ctm's connection with no flags); one
//!   started with no daemon runs in-process and is invisible. So ctm keeps the daemon
//!   alive itself (`codex_daemon.rs`) — that is the whole of "enabling" Codex.
//! - `initialize` first. `turn/start` and `turn/steer` are **ungated**; only
//!   `thread/queue/*` needs `experimentalApi`, so ctm never declares it.
//! - **A client is blind and mute until it calls `thread/resume {threadId}`** — without
//!   it, only `thread/status/changed` flags arrive: no approval or question requests,
//!   no `serverRequest/resolved`. `thread/resume` fails with `no rollout found` on a
//!   fresh thread until its first turn persists, so it is retried on `turn/started`.
//!   Subscribing mid-turn **replays pending server requests** — free crash recovery.
//! - Server requests (`item/commandExecution/requestApproval`, `item/tool/requestUserInput`)
//!   are JSON-RPC requests *from* the server; answering is returning a result on the
//!   same id. Delivery is fire-and-forget: the loser of a double answer gets **silence**
//!   (no error, no ack). Telegram state is finalized by `serverRequest/resolved`, never
//!   by our own send succeeding.
//! - `item/tool/requestUserInput` only fires in plan collaboration mode
//!   (`HostCaps::structured_questions = NativePlanModeOnly`).
//! - The TUI renders `Ran <cmd>` even for commands that never executed after a remote
//!   cancel; ctm trusts `item.status` (`declined`), never the verb.
//! - Ephemeral `threadSource:"system"` sub-threads spawn per turn for title generation
//!   and appear in `thread/started`; they are filtered or ctm mirrors ghost sessions.
//! - The attribution gap (no `✔ You approved…` for remote answers) is a TUI limitation
//!   with no safe RPC compensation: `thread/inject_items` alters model-visible history
//!   with zero operator-visible effect — PR-E-shaped — and is deliberately NOT used.
//!
//! Structure mirrors `opencode.rs`: [`Translator`] is a pure state machine unit-tested
//! against captured samples; [`run`] is the thin transport shell.

use crate::config::{CodexHostConfig, Config};
use crate::error::{AppError, Result};
use crate::host::link::{stamped, ApprovalFifo, Backoff, ObserverLink};
use crate::host::normalize_tool_name;
use crate::types::{BridgeMessage, HostKind, MessageType};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const KIND: HostKind = HostKind::Codex;
const CLIENT_NAME: &str = "ctm";
/// Marker prefix on `clientUserMessageId` so our own injected user messages are not
/// re-mirrored as terminal input when they echo back as `item/started userMessage`.
const CLIENT_MSG_PREFIX: &str = "ctm-";
/// How often a deferred `thread/resume` is retried.
const RESUME_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// How long to keep retrying before concluding the thread is not app-server-owned.
const RESUME_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(90);

/// Something the observer must send to the app-server: a request we originate, or a
/// response to a server request.
#[derive(Debug, Clone, PartialEq)]
pub enum Rpc {
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    Response {
        id: Value,
        result: Value,
    },
}

impl Rpc {
    fn to_json(&self) -> Value {
        match self {
            Rpc::Request { id, method, params } => {
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
            }
            Rpc::Response { id, result } => json!({"jsonrpc":"2.0","id":id,"result":result}),
        }
    }
}

/// Output of the pure translator.
#[derive(Debug, Clone)]
pub enum Out {
    Bridge(BridgeMessage),
    Rpc(Rpc),
}

/// Which of OUR requests an outstanding id belongs to, so its result/error is routed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ours {
    Initialize,
    LoadedList,
    Resume(String),
    TurnStart(String),
    Other,
}

/// What a pending *server* request is, keyed by its JSON-RPC id.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerReq {
    Approval {
        thread_id: String,
    },
    /// Question ids in order, needed to re-key ctm's positional answers.
    Question {
        thread_id: String,
        question_ids: Vec<String>,
    },
}

#[derive(Debug, Default, Clone)]
struct ThreadCtx {
    subscribed: bool,
    running: bool,
    current_turn: Option<String>,
    announced: bool,
    /// agentMessage item id -> final text, in first-seen order, for the current turn.
    agent_text: Vec<(String, String)>,
}

/// Pure translation state. No I/O.
#[derive(Debug, Default)]
pub struct Translator {
    next_id: u64,
    ours: HashMap<u64, Ours>,
    threads: HashMap<String, ThreadCtx>,
    server_reqs: HashMap<String, ServerReq>,
    /// Approval FIFO per thread: JSON-RPC ids of unanswered approval requests.
    pub pending: ApprovalFifo<Value>,
    replied_by_us: HashSet<String>,
    tool_started: HashSet<String>,
    user_items_seen: HashSet<String>,
    initialized: bool,
}

fn id_key(id: &Value) -> String {
    id.to_string()
}

impl Translator {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            ..Default::default()
        }
    }

    fn msg(
        &self,
        t: MessageType,
        thread_id: &str,
        content: impl Into<String>,
        mut meta: Map<String, Value>,
    ) -> BridgeMessage {
        meta.insert("hostSessionId".into(), Value::String(thread_id.into()));
        stamped(KIND, t, thread_id, content, meta)
    }

    fn request(&mut self, kind: Ours, method: &str, params: Value) -> Out {
        let id = self.next_id;
        self.next_id += 1;
        self.ours.insert(id, kind);
        Out::Rpc(Rpc::Request {
            id,
            method: method.into(),
            params,
        })
    }

    /// First messages after the socket connects.
    pub fn on_connect(&mut self) -> Vec<Out> {
        self.initialized = false;
        vec![self.request(
            Ours::Initialize,
            "initialize",
            json!({"clientInfo": {"name": CLIENT_NAME, "title": "Claude Telegram Mirror", "version": env!("CARGO_PKG_VERSION")}}),
        )]
    }

    fn subscribe(&mut self, thread_id: &str) -> Out {
        // `excludeTurns: true` — full-history hydration is deprecated (deprecationNotice
        // observed live) and ctm only needs the thread record + live stream.
        self.request(
            Ours::Resume(thread_id.into()),
            "thread/resume",
            json!({"threadId": thread_id, "excludeTurns": true}),
        )
    }

    /// Ghost-thread filter (spike-verified): per-turn title-generation sub-threads are
    /// `ephemeral: true` with `threadSource: "system"`.
    fn is_ghost(thread: &Value) -> bool {
        thread
            .get("ephemeral")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || thread.get("threadSource").and_then(Value::as_str) == Some("system")
    }

    /// ADR-016 (2026-09-20): threads whose `thread/resume` was deferred and that are
    /// still unsubscribed. `turn/started` cannot be the retry trigger — it is only
    /// delivered to *subscribed* clients, so a deferred subscription could never
    /// recover from it. The runner retries these on a timer instead.
    pub fn resume_retries(&mut self) -> Vec<Out> {
        let pending: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, c)| c.announced && !c.subscribed)
            .map(|(id, _)| id.clone())
            .collect();
        pending.iter().map(|id| self.subscribe(id)).collect()
    }

    /// End an announced thread exactly once and forget its state.
    fn end_thread(&mut self, thread_id: &str, reason: &str) -> Vec<Out> {
        let Some(ctx) = self.threads.remove(thread_id) else {
            return vec![];
        };
        self.pending.clear(thread_id);
        if !ctx.announced {
            return vec![];
        }
        vec![Out::Bridge(self.msg(
            MessageType::SessionEnd,
            thread_id,
            reason,
            Map::new(),
        ))]
    }

    fn announce(&mut self, thread: &Value) -> Vec<Out> {
        let Some(tid) = thread.get("id").and_then(Value::as_str) else {
            return vec![];
        };
        let tid = tid.to_string();
        let ctx = self.threads.entry(tid.clone()).or_default();
        if ctx.announced {
            return vec![];
        }
        ctx.announced = true;
        ctx.running = thread
            .get("status")
            .and_then(|s| s.get("type"))
            .and_then(Value::as_str)
            == Some("active");
        let mut meta = Map::new();
        if let Some(cwd) = thread.get("cwd").and_then(Value::as_str) {
            meta.insert("projectDir".into(), Value::String(cwd.into()));
        }
        if let Some(name) = thread.get("name").and_then(Value::as_str) {
            meta.insert("title".into(), Value::String(name.into()));
        }
        // ADR-013 lineage, as direct protocol fields (no path parsing).
        if let Some(p) = thread.get("parentThreadId").and_then(Value::as_str) {
            meta.insert("parentSessionId".into(), Value::String(p.into()));
        }
        if let Some(a) = thread.get("agentNickname").and_then(Value::as_str) {
            meta.insert("agentId".into(), Value::String(a.into()));
        }
        if let Some(r) = thread.get("agentRole").and_then(Value::as_str) {
            meta.insert("agentType".into(), Value::String(r.into()));
        }
        meta.insert("entrypoint".into(), Value::String("cli".into()));
        let can_input = thread
            .get("canAcceptDirectInput")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        meta.insert("canAcceptDirectInput".into(), Value::Bool(can_input));
        vec![Out::Bridge(self.msg(
            MessageType::SessionStart,
            &tid,
            "",
            meta,
        ))]
    }

    /// Translate one inbound JSON-RPC message.
    pub fn on_rpc(&mut self, m: &Value) -> Vec<Out> {
        let method = m.get("method").and_then(Value::as_str);
        let has_id = m.get("id").is_some();
        match (method, has_id) {
            (Some(meth), true) => self.on_server_request(meth, m),
            (Some(meth), false) => {
                self.on_notification(meth, m.get("params").unwrap_or(&Value::Null))
            }
            (None, true) => self.on_our_result(m),
            (None, false) => vec![],
        }
    }

    fn on_our_result(&mut self, m: &Value) -> Vec<Out> {
        let Some(id) = m.get("id").and_then(Value::as_u64) else {
            return vec![];
        };
        let Some(kind) = self.ours.remove(&id) else {
            return vec![];
        };
        let err = m
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str);
        match kind {
            Ours::Initialize => {
                if let Some(e) = err {
                    tracing::error!(error = e, "Codex: initialize failed");
                    return vec![];
                }
                self.initialized = true;
                vec![self.request(Ours::LoadedList, "thread/loaded/list", json!({}))]
            }
            Ours::LoadedList => {
                let ids: Vec<String> = m
                    .get("result")
                    .and_then(|r| r.get("data"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                ids.iter().map(|t| self.subscribe(t)).collect()
            }
            Ours::Resume(tid) => {
                if let Some(e) = err {
                    // Fresh thread without a rollout yet: retried on its first turn/started.
                    tracing::info!(thread_id = %tid, error = e, "Codex: thread/resume deferred");
                    return vec![];
                }
                let mut out = Vec::new();
                if let Some(thread) = m.get("result").and_then(|r| r.get("thread")) {
                    if Self::is_ghost(thread) {
                        return vec![];
                    }
                    out.extend(self.announce(thread));
                }
                self.threads.entry(tid).or_default().subscribed = true;
                out
            }
            Ours::TurnStart(tid) => {
                if let Some(e) = err {
                    tracing::warn!(thread_id = %tid, error = e, "Codex: turn/start failed");
                    return vec![Out::Bridge(self.msg(
                        MessageType::Error,
                        &tid,
                        format!("Could not deliver message to Codex: {e}"),
                        Map::new(),
                    ))];
                }
                if let Some(turn_id) = m
                    .get("result")
                    .and_then(|r| r.get("turn"))
                    .and_then(|t| t.get("id"))
                    .and_then(Value::as_str)
                {
                    let ctx = self.threads.entry(tid).or_default();
                    ctx.running = true;
                    ctx.current_turn = Some(turn_id.into());
                }
                vec![]
            }
            Ours::Other => {
                if let Some(e) = err {
                    tracing::warn!(error = e, "Codex: request failed");
                }
                vec![]
            }
        }
    }

    fn on_notification(&mut self, method: &str, p: &Value) -> Vec<Out> {
        let tid = p
            .get("threadId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut out = Vec::new();
        match method {
            "thread/started" => {
                if let Some(thread) = p.get("thread") {
                    if Self::is_ghost(thread) {
                        return vec![];
                    }
                    out.extend(self.announce(thread));
                    if let Some(id) = thread.get("id").and_then(Value::as_str) {
                        let id = id.to_string();
                        if !self.threads.get(&id).is_some_and(|c| c.subscribed) {
                            out.push(self.subscribe(&id));
                        }
                    }
                }
            }
            "thread/name/updated" => {
                if let (Some(tid), Some(name)) = (tid, p.get("threadName").and_then(Value::as_str))
                {
                    out.push(Out::Bridge(self.msg(
                        MessageType::SessionRename,
                        &tid,
                        name,
                        Map::new(),
                    )));
                }
            }
            "turn/started" => {
                if let Some(tid) = tid {
                    let ctx = self.threads.entry(tid.clone()).or_default();
                    ctx.running = true;
                    ctx.current_turn = p
                        .get("turn")
                        .and_then(|t| t.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    ctx.agent_text.clear();
                    let need_sub = !ctx.subscribed;
                    if need_sub {
                        // The rollout now exists — retry the subscription (spike-verified).
                        out.push(self.subscribe(&tid));
                    }
                }
            }
            "turn/completed" => {
                if let Some(tid) = tid {
                    let status = p
                        .get("turn")
                        .and_then(|t| t.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("completed")
                        .to_string();
                    // Prefer the turn's own final items (authoritative) over deltas.
                    if let Some(items) = p
                        .get("turn")
                        .and_then(|t| t.get("items"))
                        .and_then(Value::as_array)
                    {
                        let ctx = self.threads.entry(tid.clone()).or_default();
                        for it in items {
                            if it.get("type").and_then(Value::as_str) == Some("agentMessage") {
                                let id = it
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                let text = it
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                match ctx.agent_text.iter_mut().find(|(i, _)| *i == id) {
                                    Some(slot) => slot.1 = text,
                                    None => ctx.agent_text.push((id, text)),
                                }
                            }
                        }
                    }
                    let ctx = self.threads.entry(tid.clone()).or_default();
                    ctx.running = false;
                    ctx.current_turn = None;
                    let text = std::mem::take(&mut ctx.agent_text)
                        .into_iter()
                        .map(|(_, t)| t)
                        .filter(|t| !t.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if !text.trim().is_empty() {
                        out.push(Out::Bridge(self.msg(
                            MessageType::AgentResponse,
                            &tid,
                            text,
                            Map::new(),
                        )));
                    }
                    out.push(Out::Bridge(self.msg(
                        MessageType::TurnComplete,
                        &tid,
                        status,
                        Map::new(),
                    )));
                }
            }
            "thread/status/changed" => {
                if let Some(tid) = tid {
                    let status = p
                        .get("status")
                        .and_then(|s| s.get("type"))
                        .and_then(Value::as_str);
                    // `notLoaded` is the app-server dropping the thread (the owning
                    // process exited). Spike-verified: it is broadcast to every client,
                    // subscribed or not, immediately before `thread/closed`.
                    if status == Some("notLoaded") {
                        out.extend(self.end_thread(&tid, "closed"));
                    } else {
                        self.threads.entry(tid).or_default().running = status == Some("active");
                    }
                }
            }
            // The owning process exited. Without this the Telegram topic stayed open
            // forever (reported after 0.2.32: "the topic didn't self delete when I
            // exited the codex session"). Delivered to unsubscribed clients too.
            "thread/closed" | "thread/deleted" | "thread/archived" => {
                if let Some(tid) = tid {
                    let reason = if method == "thread/closed" {
                        "closed"
                    } else if method == "thread/deleted" {
                        "deleted"
                    } else {
                        "archived"
                    };
                    out.extend(self.end_thread(&tid, reason));
                }
            }
            "item/started" | "item/completed" => {
                if let (Some(tid), Some(item)) = (tid, p.get("item")) {
                    out.extend(self.on_item(&tid, item, method == "item/completed"));
                }
            }
            "serverRequest/resolved" => {
                if let (Some(tid), Some(rid)) = (tid, p.get("requestId")) {
                    out.extend(self.on_resolved(&tid, rid));
                }
            }
            "error" => {
                if let Some(tid) = tid {
                    let text = p
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error");
                    out.push(Out::Bridge(self.msg(
                        MessageType::Error,
                        &tid,
                        text,
                        Map::new(),
                    )));
                }
            }
            _ => {}
        }
        out
    }

    fn on_item(&mut self, tid: &str, item: &Value, completed: bool) -> Vec<Out> {
        let itype = item.get("type").and_then(Value::as_str).unwrap_or("");
        let iid = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut out = Vec::new();
        match itype {
            "userMessage" => {
                if completed {
                    return vec![];
                }
                // Our own injections echo back with our clientId — never re-mirror them.
                if item
                    .get("clientId")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.starts_with(CLIENT_MSG_PREFIX))
                {
                    return vec![];
                }
                if !self.user_items_seen.insert(iid.clone()) {
                    return vec![];
                }
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|c| {
                        c.iter()
                            .filter_map(|x| x.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let mut meta = Map::new();
                meta.insert("source".into(), Value::String("cli".into()));
                out.push(Out::Bridge(self.msg(
                    MessageType::UserInput,
                    tid,
                    text,
                    meta,
                )));
            }
            "agentMessage" => {
                if completed {
                    let text = item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let ctx = self.threads.entry(tid.into()).or_default();
                    match ctx.agent_text.iter_mut().find(|(i, _)| *i == iid) {
                        Some(slot) => slot.1 = text,
                        None => ctx.agent_text.push((iid, text)),
                    }
                }
            }
            "reasoning" | "contextCompaction" | "enteredReviewMode" | "exitedReviewMode" => {}
            _ => {
                // Tool-like items: commandExecution, fileChange, mcpToolCall, webSearch, …
                let tool = normalize_tool_name(KIND, itype);
                let input = match itype {
                    "commandExecution" => {
                        let cmd = item
                            .get("commandActions")
                            .and_then(Value::as_array)
                            .and_then(|a| a.first())
                            .and_then(|a| a.get("command"))
                            .and_then(Value::as_str)
                            .or_else(|| item.get("command").and_then(Value::as_str))
                            .unwrap_or("");
                        json!({"command": cmd})
                    }
                    "fileChange" => {
                        json!({"file_path": item.get("path").cloned().unwrap_or(Value::Null),
                                           "changes": item.get("changes").cloned().unwrap_or(Value::Null)})
                    }
                    _ => item.clone(),
                };
                if !completed {
                    if self.tool_started.insert(iid.clone()) {
                        let mut meta = Map::new();
                        meta.insert("tool".into(), Value::String(tool));
                        meta.insert("input".into(), input);
                        meta.insert("toolUseId".into(), Value::String(iid));
                        out.push(Out::Bridge(self.msg(MessageType::ToolStart, tid, "", meta)));
                    }
                } else {
                    if self.tool_started.insert(iid.clone()) {
                        // completed without a seen start (reconnect mid-item): emit start too
                        let mut meta = Map::new();
                        meta.insert("tool".into(), Value::String(tool.clone()));
                        meta.insert("input".into(), input.clone());
                        meta.insert("toolUseId".into(), Value::String(iid.clone()));
                        out.push(Out::Bridge(self.msg(MessageType::ToolStart, tid, "", meta)));
                    }
                    // Trust item.status, never the TUI's verb ("Ran" renders even for
                    // commands that were declined/interrupted — spike-verified).
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("completed");
                    let content = match status {
                        "declined" => "declined — command was not run".to_string(),
                        "interrupted" | "cancelled" | "canceled" => {
                            "interrupted — command did not complete".to_string()
                        }
                        _ => {
                            let output = item
                                .get("aggregatedOutput")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            match item.get("exitCode").and_then(Value::as_i64) {
                                Some(code) if code != 0 => format!("{output}\n[exit {code}]"),
                                _ => output,
                            }
                        }
                    };
                    let mut meta = Map::new();
                    meta.insert("tool".into(), Value::String(tool));
                    meta.insert("input".into(), input);
                    meta.insert("toolUseId".into(), Value::String(iid));
                    out.push(Out::Bridge(self.msg(
                        MessageType::ToolResult,
                        tid,
                        content,
                        meta,
                    )));
                }
            }
        }
        out
    }

    fn on_server_request(&mut self, method: &str, m: &Value) -> Vec<Out> {
        let id = m.get("id").cloned().unwrap_or(Value::Null);
        let p = m.get("params").cloned().unwrap_or(Value::Null);
        let Some(tid) = p
            .get("threadId")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return vec![];
        };
        match method {
            "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "execCommandApproval"
            | "applyPatchApproval" => {
                let (tool, input) = if method.contains("fileChange")
                    || method == "applyPatchApproval"
                {
                    (
                        "Edit".to_string(),
                        json!({"file_path": p.get("path").cloned().unwrap_or(Value::Null),
                                                 "reason": p.get("reason").cloned().unwrap_or(Value::Null)}),
                    )
                } else {
                    let cmd = p
                        .get("commandActions")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|a| a.get("command"))
                        .and_then(Value::as_str)
                        .or_else(|| p.get("command").and_then(Value::as_str))
                        .unwrap_or("");
                    (
                        "Bash".to_string(),
                        json!({"command": cmd, "reason": p.get("reason").cloned().unwrap_or(Value::Null)}),
                    )
                };
                let mut prompt = crate::hook::format_tool_approval_prompt(&tool, &input);
                if let Some(r) = p.get("reason").and_then(Value::as_str) {
                    prompt.push_str(&format!("\n\n_{r}_"));
                }
                self.server_reqs.insert(
                    id_key(&id),
                    ServerReq::Approval {
                        thread_id: tid.clone(),
                    },
                );
                self.pending.push(&tid, id.clone());
                let mut meta = Map::new();
                meta.insert("tool".into(), Value::String(tool));
                meta.insert("input".into(), input);
                meta.insert("hostRequestId".into(), id.clone());
                vec![Out::Bridge(self.msg(
                    MessageType::ApprovalRequest,
                    &tid,
                    prompt,
                    meta,
                ))]
            }
            "item/tool/requestUserInput" => {
                let qs = p
                    .get("questions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let question_ids: Vec<String> = qs
                    .iter()
                    .filter_map(|q| q.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect();
                // Codex `{id, header, question, options[{label,description}], isOther, isSecret}`
                // -> Claude AskUserQuestion `{question, header, options, multiSelect}`.
                let questions: Vec<Value> = qs
                    .iter()
                    .map(|q| {
                        json!({
                            "question": q.get("question").and_then(Value::as_str).unwrap_or(""),
                            "header": q.get("header").and_then(Value::as_str).unwrap_or(""),
                            "options": q.get("options").cloned().unwrap_or(json!([])),
                            "multiSelect": false,
                        })
                    })
                    .collect();
                self.server_reqs.insert(
                    id_key(&id),
                    ServerReq::Question {
                        thread_id: tid.clone(),
                        question_ids,
                    },
                );
                let item_id = p
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut meta = Map::new();
                meta.insert("tool".into(), Value::String("AskUserQuestion".into()));
                meta.insert("input".into(), json!({"questions": questions}));
                meta.insert("toolUseId".into(), Value::String(item_id));
                meta.insert("questionId".into(), Value::String(id_key(&id)));
                vec![Out::Bridge(self.msg(
                    MessageType::ToolStart,
                    &tid,
                    "",
                    meta,
                ))]
            }
            _ => {
                tracing::debug!(method, "Codex: unhandled server request (left pending)");
                vec![]
            }
        }
    }

    fn on_resolved(&mut self, tid: &str, rid: &Value) -> Vec<Out> {
        let key = id_key(rid);
        let Some(req) = self.server_reqs.remove(&key) else {
            return vec![];
        };
        let ours = self.replied_by_us.remove(&key);
        match req {
            ServerReq::Approval { .. } => {
                // Drop from FIFO wherever it sits (normally the front).
                let mut rest = Vec::new();
                while let Some(x) = self.pending.pop_for(tid) {
                    if id_key(&x) != key {
                        rest.push(x);
                    }
                }
                for x in rest {
                    self.pending.push(tid, x);
                }
                if ours {
                    return vec![];
                }
                let mut meta = Map::new();
                meta.insert("source".into(), Value::String("terminal".into()));
                meta.insert("hostRequestId".into(), rid.clone());
                vec![Out::Bridge(self.msg(
                    MessageType::ApprovalResponse,
                    tid,
                    "",
                    meta,
                ))]
            }
            ServerReq::Question { .. } => {
                // Either surface answered; the daemon's resolve is idempotent (ADR-015).
                let mut meta = Map::new();
                meta.insert("tool".into(), Value::String("AskUserQuestion".into()));
                meta.insert("questionId".into(), Value::String(key));
                vec![Out::Bridge(self.msg(
                    MessageType::ToolResult,
                    tid,
                    "",
                    meta,
                ))]
            }
        }
    }

    /// Translate a daemon→observer message into RPCs.
    pub fn on_daemon(&mut self, msg: &BridgeMessage) -> Vec<Out> {
        let tid = msg.session_id.clone();
        let meta = msg.meta();
        match msg.msg_type {
            MessageType::HostInject => match meta.action().unwrap_or("text") {
                "interrupt" | "abort" => {
                    let turn = self.threads.get(&tid).and_then(|c| c.current_turn.clone());
                    match turn {
                        // `turn/interrupt` requires turnId (spike: `missing field turnId`).
                        Some(turn_id) => vec![self.request(
                            Ours::Other,
                            "turn/interrupt",
                            json!({"threadId": tid, "turnId": turn_id}),
                        )],
                        None => vec![], // nothing running: nothing to interrupt
                    }
                }
                "slash" => {
                    let cmd = msg.content.trim();
                    if let Some(name) = cmd.strip_prefix("/rename ") {
                        vec![self.request(
                            Ours::Other,
                            "thread/name/set",
                            json!({"threadId": tid, "name": name.trim()}),
                        )]
                    } else {
                        self.inject_text(&tid, cmd)
                    }
                }
                _ => self.inject_text(&tid, &msg.content),
            },
            MessageType::QuestionResponse => {
                let Some(qid) = meta.question_id() else {
                    return vec![];
                };
                let Some(ServerReq::Question { question_ids, .. }) =
                    self.server_reqs.get(qid).cloned()
                else {
                    tracing::info!(question_id = qid, "Codex: question already resolved");
                    return vec![];
                };
                // ctm answers are positional arrays of labels; Codex wants {qid: {answers}}.
                let positional = meta
                    .answers()
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut answers = Map::new();
                for (i, q) in question_ids.iter().enumerate() {
                    let a = positional.get(i).cloned().unwrap_or(json!([]));
                    answers.insert(q.clone(), json!({"answers": a}));
                }
                self.replied_by_us.insert(qid.to_string());
                let id: Value = serde_json::from_str(qid).unwrap_or(Value::String(qid.into()));
                vec![Out::Rpc(Rpc::Response {
                    id,
                    result: json!({"answers": answers}),
                })]
            }
            MessageType::ApprovalResponse => {
                let Some(rid) = self.pending.pop_for(&tid) else {
                    tracing::info!(thread_id = %tid, "Codex: approval response with nothing pending");
                    return vec![];
                };
                let key = id_key(&rid);
                self.replied_by_us.insert(key);
                let decision = if msg.content == "approve" {
                    "accept"
                } else {
                    "cancel"
                };
                let mut out = vec![Out::Rpc(Rpc::Response {
                    id: rid,
                    result: json!({"decision": decision}),
                })];
                if msg.content == "abort" {
                    if let Some(turn_id) =
                        self.threads.get(&tid).and_then(|c| c.current_turn.clone())
                    {
                        out.push(self.request(
                            Ours::Other,
                            "turn/interrupt",
                            json!({"threadId": tid, "turnId": turn_id}),
                        ));
                    }
                }
                out
            }
            _ => vec![],
        }
    }

    fn inject_text(&mut self, tid: &str, text: &str) -> Vec<Out> {
        let running = self.threads.get(tid).is_some_and(|c| c.running);
        let client_id = format!("{CLIENT_MSG_PREFIX}{}", uuid::Uuid::new_v4());
        let input = json!([{"type": "text", "text": text}]);
        if running {
            // Steer the in-flight turn (ungated, spike-verified).
            vec![self.request(
                Ours::Other,
                "turn/steer",
                json!({"threadId": tid, "clientUserMessageId": client_id, "input": input}),
            )]
        } else {
            vec![self.request(
                Ours::TurnStart(tid.into()),
                "turn/start",
                json!({"threadId": tid, "clientUserMessageId": client_id, "input": input}),
            )]
        }
    }
}

// ============================================================================ runner

/// Run the Codex observer forever, reconnecting with backoff.
pub async fn run(config: Arc<Config>, cx: CodexHostConfig) {
    let mut backoff = Backoff::new();
    let mut announced_absent = false;
    loop {
        // ADR-016 §Default enablement: Codex may not be installed (yet). Probe quietly
        // and keep the app-server daemon alive once it is.
        match super::codex_daemon::ensure_running(&cx).await {
            Ok(super::codex_daemon::Ensured::NotInstalled) => {
                if !announced_absent {
                    tracing::info!("Codex not installed — will watch for it");
                    announced_absent = true;
                }
                tokio::time::sleep(super::codex_daemon::ABSENT_POLL).await;
                continue;
            }
            Ok(state) => {
                announced_absent = false;
                if state != super::codex_daemon::Ensured::AlreadyRunning {
                    tracing::info!(?state, "Codex app-server daemon ready");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Codex app-server daemon could not be started; will retry");
                tokio::time::sleep(backoff.delay()).await;
                continue;
            }
        }
        match run_once(&config, &cx).await {
            Ok(()) => backoff.reset(),
            Err(e) => tracing::warn!(error = %e, "Codex observer stopped; will reconnect"),
        }
        tokio::time::sleep(backoff.delay()).await;
    }
}

/// One connection lifetime: connect both legs, pump until either drops. Public so
/// the end-to-end integration test (`tests/host_e2e.rs`) can drive a single run against
/// a real host binary with a stand-in daemon socket.
pub async fn run_once(config: &Config, cx: &CodexHostConfig) -> Result<()> {
    let mut link = ObserverLink::connect(KIND, &config.socket_path).await?;

    // WebSocket over the Unix control socket. `tokio_tungstenite::client_async` performs
    // the HTTP/1.1 upgrade over any AsyncRead+AsyncWrite; the Host header is required
    // by the handshake but ignored by the daemon.
    let stream = tokio::net::UnixStream::connect(&cx.socket_path)
        .await
        .map_err(|e| {
            AppError::Socket(format!(
            "Codex app-server socket {} not reachable ({e}) — run `codex app-server daemon start`",
            cx.socket_path.display()
        ))
        })?;
    let (ws, _resp) = tokio_tungstenite::client_async("ws://codex/", stream)
        .await
        .map_err(|e| AppError::Socket(format!("Codex WebSocket handshake failed: {e}")))?;
    let (mut tx, mut rx) = ws.split();
    tracing::info!(socket = %cx.socket_path.display(), "Codex observer connected");

    let mut tr = Translator::new();
    for o in tr.on_connect() {
        dispatch(o, &mut tx, &link).await?;
    }

    // A fresh thread has no rollout yet, so its first `thread/resume` is refused. For a
    // session the app-server owns (`codex --remote …`, which ctm's shell integration
    // makes the default) the rollout appears once the turn starts and a retry succeeds
    // — spike-verified. For a bare `codex`, which owns its own thread, it never does,
    // so retries are bounded and the outcome is reported once instead of churning.
    let mut resume_tick = tokio::time::interval(RESUME_RETRY_INTERVAL);
    resume_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let started = tokio::time::Instant::now();
    let mut gave_up = false;

    loop {
        tokio::select! {
            _ = resume_tick.tick() => {
                if !gave_up {
                    let retries = tr.resume_retries();
                    if !retries.is_empty() {
                        if started.elapsed() > RESUME_RETRY_WINDOW {
                            gave_up = true;
                            tracing::info!(
                                threads = retries.len(),
                                "Codex: these threads cannot be subscribed to — they are bare `codex` sessions, which own their own thread. They mirror out through ctm's Codex hooks; approvals need `codex --remote` (ctm's shell integration does this)."
                            );
                        } else {
                            for o in retries {
                                dispatch(o, &mut tx, &link).await?;
                            }
                        }
                    }
                }
            }
            frame = rx.next() => {
                let Some(frame) = frame else {
                    return Err(AppError::Socket("Codex WebSocket closed".into()));
                };
                let frame = frame.map_err(|e| AppError::Socket(format!("Codex WebSocket error: {e}")))?;
                use tokio_tungstenite::tungstenite::Message as W;
                match frame {
                    W::Text(t) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&t) {
                            for o in tr.on_rpc(&v) {
                                dispatch(o, &mut tx, &link).await?;
                            }
                        }
                    }
                    W::Ping(p) => { let _ = tx.send(W::Pong(p)).await; }
                    W::Close(_) => return Err(AppError::Socket("Codex WebSocket closed by server".into())),
                    _ => {}
                }
            }
            down = link.recv() => {
                let Some(msg) = down else {
                    return Err(AppError::Socket("daemon link closed".into()));
                };
                for o in tr.on_daemon(&msg) {
                    dispatch(o, &mut tx, &link).await?;
                }
            }
        }
    }
}

async fn dispatch<S>(o: Out, tx: &mut S, link: &ObserverLink) -> Result<()>
where
    S: SinkExt<tokio_tungstenite::tungstenite::Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match o {
        Out::Bridge(m) => link.send(&m).await,
        Out::Rpc(r) => tx
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r.to_json().to_string(),
            ))
            .await
            .map_err(|e| AppError::Socket(format!("Codex WebSocket send failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim samples captured from Codex 0.155.1 / app-server 0.153.2 (ADR-016 spike).
    const T: &str = "01a0bb11-b097-7941-8e2f-ae17c2d6ae68";
    const THREAD_STARTED_GHOST: &str = r#"{"method":"thread/started","params":{"thread":{"id":"01a0bb0d-5c00-7b33-ad6d-189ef1855afb","ephemeral":true,"threadSource":"system","source":"vscode","cwd":"/tmp","status":{"type":"idle"}}}}"#;
    // Verbatim from the 0.155.1 spike: broadcast to EVERY client, subscribed or not,
    // when the owning process exits (`listen.jsonl`, 22:22:58).
    const THREAD_NOT_LOADED: &str = r#"{"method":"thread/status/changed","params":{"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","status":{"type":"notLoaded"}}}"#;
    const THREAD_CLOSED: &str = r#"{"method":"thread/closed","params":{"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68"}}"#;
    const THREAD_STATUS_ACTIVE: &str = r#"{"method":"thread/status/changed","params":{"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","status":{"type":"active"}}}"#;
    const RESUME_OK: &str = r#"{"id":1000,"result":{"thread":{"id":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","parentThreadId":null,"ephemeral":false,"status":{"type":"idle"},"cwd":"/private/var/tmp/ctm-codex-spike","canAcceptDirectInput":true,"threadSource":"cli","name":null}}}"#;
    const TURN_STARTED: &str = r#"{"method":"turn/started","params":{"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","turn":{"id":"01a0bb12-01bf-77e1-88e8-53892ba8f83a","status":"inProgress"}}}"#;
    const APPROVAL_REQ: &str = r#"{"method":"item/commandExecution/requestApproval","id":2,"params":{"kind":"command","threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","turnId":"01a0bb12-01bf-77e1-88e8-53892ba8f83a","itemId":"exec-f721","reason":"Allow this exact touch command outside the read-only sandbox?","command":"/bin/zsh -lc 'touch /tmp/probe3.txt'","cwd":"/tmp","commandActions":[{"type":"unknown","command":"touch /tmp/probe3.txt"}],"availableDecisions":["accept",{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["touch","/tmp/probe3.txt"]}},"cancel"]}}"#;
    const RESOLVED_2: &str = r#"{"method":"serverRequest/resolved","params":{"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","requestId":2}}"#;
    const QUESTION_REQ: &str = r#"{"method":"item/tool/requestUserInput","id":0,"params":{"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","turnId":"01a0bb1b","itemId":"call_KSC","questions":[{"id":"color","header":"Color","question":"Which color?","isOther":true,"isSecret":false,"options":[{"label":"red","description":"Name it red."},{"label":"blue","description":"Name it blue."}]}],"isBlocking":true,"autoResolutionMs":null}}"#;
    const RESOLVED_0: &str = r#"{"method":"serverRequest/resolved","params":{"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","requestId":0}}"#;
    const CMD_STARTED: &str = r#"{"method":"item/started","params":{"item":{"type":"commandExecution","id":"exec-f721","command":"/bin/zsh -lc 'touch /tmp/probe3.txt'","cwd":"/tmp","status":"inProgress","commandActions":[{"type":"unknown","command":"touch /tmp/probe3.txt"}],"aggregatedOutput":null,"exitCode":null},"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","turnId":"01a0bb12"}}"#;
    const CMD_DECLINED: &str = r#"{"method":"item/completed","params":{"item":{"type":"commandExecution","id":"exec-f721","command":"/bin/zsh -lc 'touch /tmp/probe3.txt'","status":"declined","commandActions":[{"type":"unknown","command":"touch /tmp/probe3.txt"}],"aggregatedOutput":null,"exitCode":null},"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","turnId":"01a0bb12"}}"#;
    const AGENT_MSG_DONE: &str = r#"{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"msg_1","text":"I'll run the exact command.\n","phase":"commentary"},"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","turnId":"01a0bb12"}}"#;
    const TURN_COMPLETED: &str = r#"{"method":"turn/completed","params":{"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","turn":{"id":"01a0bb12","items":[{"type":"agentMessage","id":"msg_2","text":"Done.","phase":"final_answer"}],"status":"completed"}}}"#;
    const USER_MSG_OURS: &str = r#"{"method":"item/started","params":{"item":{"type":"userMessage","id":"u1","clientId":"ctm-abc","content":[{"type":"text","text":"from telegram"}]},"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","turnId":"x"}}"#;
    const USER_MSG_THEIRS: &str = r#"{"method":"item/started","params":{"item":{"type":"userMessage","id":"u2","clientId":"tui","content":[{"type":"text","text":"typed at terminal"}]},"threadId":"01a0bb11-b097-7941-8e2f-ae17c2d6ae68","turnId":"x"}}"#;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }
    fn bridges(out: &[Out]) -> Vec<&BridgeMessage> {
        out.iter()
            .filter_map(|o| {
                if let Out::Bridge(m) = o {
                    Some(m)
                } else {
                    None
                }
            })
            .collect()
    }
    fn rpcs(out: &[Out]) -> Vec<&Rpc> {
        out.iter()
            .filter_map(|o| if let Out::Rpc(r) = o { Some(r) } else { None })
            .collect()
    }
    /// Bring a translator to "initialized + thread T subscribed" using captured shapes.
    fn subscribed() -> Translator {
        let mut t = Translator::new();
        let init = t.on_connect();
        let Rpc::Request { id, .. } = rpcs(&init)[0].clone() else {
            panic!()
        };
        let out = t.on_rpc(&json!({"id": id, "result": {"userAgent":"codex-tui/0.153.2"}}));
        let Rpc::Request {
            id: list_id,
            method,
            ..
        } = rpcs(&out)[0].clone()
        else {
            panic!()
        };
        assert_eq!(method, "thread/loaded/list");
        let out = t.on_rpc(&json!({"id": list_id, "result": {"data": [T], "nextCursor": null}}));
        let Rpc::Request {
            id: resume_id,
            method,
            params,
        } = rpcs(&out)[0].clone()
        else {
            panic!()
        };
        assert_eq!(method, "thread/resume");
        assert_eq!(params["excludeTurns"], true);
        let mut resume = v(RESUME_OK);
        resume["id"] = json!(resume_id);
        let out = t.on_rpc(&resume);
        let b = bridges(&out);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].msg_type, MessageType::SessionStart);
        assert_eq!(b[0].meta().host_kind(), HostKind::Codex);
        assert_eq!(
            b[0].meta().project_dir(),
            Some("/private/var/tmp/ctm-codex-spike")
        );
        t
    }

    #[test]
    fn connect_initializes_without_experimental_api_then_lists_and_subscribes() {
        let mut t = Translator::new();
        let out = t.on_connect();
        let Rpc::Request { method, params, .. } = rpcs(&out)[0] else {
            panic!()
        };
        assert_eq!(method, "initialize");
        assert!(
            params.get("capabilities").is_none(),
            "turn/start+steer are ungated; never opt into experimentalApi"
        );
        subscribed();
    }

    #[test]
    fn ghost_threads_are_filtered() {
        let mut t = subscribed();
        let out = t.on_rpc(&v(THREAD_STARTED_GHOST));
        assert!(
            out.is_empty(),
            "ephemeral system thread must not become a session"
        );
    }

    #[test]
    fn thread_closed_ends_an_announced_session_once() {
        let mut t = subscribed();
        // `thread/closed` and the `notLoaded` status both mean the owning process is
        // gone; both are broadcast to unsubscribed clients (spike-verified 0.155.1).
        let ends: Vec<_> = t
            .on_rpc(&v(THREAD_CLOSED))
            .into_iter()
            .filter_map(|o| match o {
                Out::Bridge(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].msg_type, MessageType::SessionEnd);
        assert_eq!(ends[0].content, "closed");
        assert_eq!(ends[0].session_id, "01a0bb11-b097-7941-8e2f-ae17c2d6ae68");
        // Idempotent: a following notLoaded/closed for the same thread emits nothing.
        assert!(t.on_rpc(&v(THREAD_CLOSED)).is_empty());
        assert!(t.on_rpc(&v(THREAD_NOT_LOADED)).is_empty());
    }

    #[test]
    fn not_loaded_status_ends_the_session_but_active_only_tracks_running() {
        let mut t = subscribed();
        assert!(t.on_rpc(&v(THREAD_STATUS_ACTIVE)).is_empty());
        let ends: Vec<_> = t
            .on_rpc(&v(THREAD_NOT_LOADED))
            .into_iter()
            .filter_map(|o| match o {
                Out::Bridge(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].msg_type, MessageType::SessionEnd);
    }

    #[test]
    fn resume_deferred_on_fresh_thread_and_retried_on_turn_started() {
        let mut t = Translator::new();
        t.on_connect();
        let out = t.on_rpc(&json!({"id":1,"result":{}}));
        let Rpc::Request { id: list_id, .. } = rpcs(&out)[0].clone() else {
            panic!()
        };
        let out = t.on_rpc(&json!({"id": list_id, "result": {"data": [T]}}));
        let Rpc::Request { id: resume_id, .. } = rpcs(&out)[0].clone() else {
            panic!()
        };
        // Fresh thread: no rollout yet (spike-verified error text).
        let out = t.on_rpc(&json!({"id": resume_id, "error": {"code": -32600, "message": "no rollout found for thread id x"}}));
        assert!(out.is_empty());
        // First turn persists the rollout — subscription retried.
        let out = t.on_rpc(&v(TURN_STARTED));
        let r = rpcs(&out);
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0], Rpc::Request { method, .. } if method == "thread/resume"));
    }

    #[test]
    fn approval_request_maps_to_approval_request_and_telegram_accept_answers_same_id() {
        let mut t = subscribed();
        let out = t.on_rpc(&v(APPROVAL_REQ));
        let b = bridges(&out);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].msg_type, MessageType::ApprovalRequest);
        assert_eq!(b[0].meta().tool(), Some("Bash"));
        assert_eq!(
            b[0].meta().input().unwrap()["command"],
            "touch /tmp/probe3.txt"
        );
        assert!(b[0].content.contains("touch /tmp/probe3.txt"));
        assert_eq!(t.pending.pending(T), 1);

        let resp = BridgeMessage {
            msg_type: MessageType::ApprovalResponse,
            session_id: T.into(),
            timestamp: String::new(),
            content: "approve".into(),
            metadata: None,
        };
        let out = t.on_daemon(&resp);
        let r = rpcs(&out);
        assert_eq!(r.len(), 1);
        assert_eq!(
            *r[0],
            Rpc::Response {
                id: json!(2),
                result: json!({"decision": "accept"})
            }
        );
        // The server then broadcasts resolved — since we answered, the daemon gets nothing.
        assert!(t.on_rpc(&v(RESOLVED_2)).is_empty());
    }

    #[test]
    fn operator_answer_at_terminal_informs_daemon_via_resolved() {
        let mut t = subscribed();
        t.on_rpc(&v(APPROVAL_REQ));
        let out = t.on_rpc(&v(RESOLVED_2));
        let b = bridges(&out);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].msg_type, MessageType::ApprovalResponse);
        assert_eq!(b[0].meta().source(), Some("terminal"));
        assert_eq!(t.pending.pending(T), 0);
    }

    #[test]
    fn abort_cancels_and_interrupts_current_turn() {
        let mut t = subscribed();
        t.on_rpc(&v(TURN_STARTED));
        t.on_rpc(&v(APPROVAL_REQ));
        let resp = BridgeMessage {
            msg_type: MessageType::ApprovalResponse,
            session_id: T.into(),
            timestamp: String::new(),
            content: "abort".into(),
            metadata: None,
        };
        let out = t.on_daemon(&resp);
        let r = rpcs(&out);
        assert_eq!(
            *r[0],
            Rpc::Response {
                id: json!(2),
                result: json!({"decision": "cancel"})
            }
        );
        assert!(
            matches!(r[1], Rpc::Request { method, params, .. } if method == "turn/interrupt" && params["turnId"] == "01a0bb12-01bf-77e1-88e8-53892ba8f83a")
        );
    }

    #[test]
    fn question_request_maps_to_askuserquestion_and_answers_are_rekeyed_by_question_id() {
        let mut t = subscribed();
        let out = t.on_rpc(&v(QUESTION_REQ));
        let b = bridges(&out);
        assert_eq!(b[0].msg_type, MessageType::ToolStart);
        assert_eq!(b[0].meta().tool(), Some("AskUserQuestion"));
        assert_eq!(b[0].meta().question_id(), Some("0"));
        let qs = &b[0].meta().input().unwrap()["questions"];
        assert_eq!(qs[0]["header"], "Color");
        assert_eq!(qs[0]["options"][1]["label"], "blue");

        let mut meta = Map::new();
        meta.insert("questionId".into(), Value::String("0".into()));
        meta.insert("answers".into(), json!([["blue"]]));
        let qr = BridgeMessage {
            msg_type: MessageType::QuestionResponse,
            session_id: T.into(),
            timestamp: String::new(),
            content: String::new(),
            metadata: Some(meta),
        };
        let out = t.on_daemon(&qr);
        let r = rpcs(&out);
        assert_eq!(
            *r[0],
            Rpc::Response {
                id: json!(0),
                result: json!({"answers": {"color": {"answers": ["blue"]}}})
            }
        );
        // resolved -> ToolResult so the daemon's pending question is retired
        let out = t.on_rpc(&v(RESOLVED_0));
        let b = bridges(&out);
        assert_eq!(b[0].msg_type, MessageType::ToolResult);
        assert_eq!(b[0].meta().tool(), Some("AskUserQuestion"));
    }

    #[test]
    fn declined_command_is_reported_as_not_run_not_ran() {
        let mut t = subscribed();
        let out = t.on_rpc(&v(CMD_STARTED));
        let b = bridges(&out);
        assert_eq!(b[0].msg_type, MessageType::ToolStart);
        assert_eq!(b[0].meta().tool(), Some("Bash"));
        let out = t.on_rpc(&v(CMD_DECLINED));
        let b = bridges(&out);
        assert_eq!(b[0].msg_type, MessageType::ToolResult);
        assert!(
            b[0].content.contains("not run"),
            "trust item.status, not the TUI verb"
        );
    }

    #[test]
    fn agent_text_flushes_at_turn_completed_preferring_final_items() {
        let mut t = subscribed();
        t.on_rpc(&v(TURN_STARTED));
        assert!(bridges(&t.on_rpc(&v(AGENT_MSG_DONE))).is_empty());
        let out = t.on_rpc(&v(TURN_COMPLETED));
        let b = bridges(&out);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].msg_type, MessageType::AgentResponse);
        assert_eq!(b[0].content, "I'll run the exact command.\n\n\nDone.");
        assert_eq!(b[1].msg_type, MessageType::TurnComplete);
        assert_eq!(b[1].content, "completed");
    }

    #[test]
    fn our_injected_user_messages_are_not_re_mirrored_but_terminal_ones_are() {
        let mut t = subscribed();
        assert!(t.on_rpc(&v(USER_MSG_OURS)).is_empty());
        let out = t.on_rpc(&v(USER_MSG_THEIRS));
        let b = bridges(&out);
        assert_eq!(b[0].msg_type, MessageType::UserInput);
        assert_eq!(b[0].content, "typed at terminal");
    }

    #[test]
    fn inject_uses_turn_start_when_idle_and_turn_steer_when_running() {
        let mut t = subscribed();
        let mk = |action: &str, content: &str| {
            let mut meta = Map::new();
            meta.insert("action".into(), Value::String(action.into()));
            BridgeMessage {
                msg_type: MessageType::HostInject,
                session_id: T.into(),
                timestamp: String::new(),
                content: content.into(),
                metadata: Some(meta),
            }
        };
        let out = t.on_daemon(&mk("text", "hello"));
        assert!(
            matches!(rpcs(&out)[0], Rpc::Request { method, params, .. } if method == "turn/start" && params["input"][0]["text"] == "hello" && params["clientUserMessageId"].as_str().unwrap().starts_with("ctm-"))
        );
        t.on_rpc(&v(TURN_STARTED));
        let out = t.on_daemon(&mk("text", "more"));
        assert!(matches!(rpcs(&out)[0], Rpc::Request { method, .. } if method == "turn/steer"));
        let out = t.on_daemon(&mk("interrupt", ""));
        assert!(
            matches!(rpcs(&out)[0], Rpc::Request { method, params, .. } if method == "turn/interrupt" && params["turnId"].is_string())
        );
        let out = t.on_daemon(&mk("slash", "/rename Probe"));
        assert!(
            matches!(rpcs(&out)[0], Rpc::Request { method, params, .. } if method == "thread/name/set" && params["name"] == "Probe")
        );
    }
}
