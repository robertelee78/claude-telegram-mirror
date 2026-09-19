//! ADR-016: OpenCode host observer.
//!
//! Every wire shape here was captured verbatim from OpenCode 1.18.31 during the
//! ADR-016 spikes (see `docs/adr/ADR-016` §Reformulated hypothesis), not read from
//! documentation. The load-bearing facts:
//!
//! - Subscribe to the **legacy global `GET /event`** SSE stream. `permission.*` and
//!   `question.*` fire ONLY there; `/api/event` never carried them, and the v2 engine
//!   (`/api/session`) is unusable in this build. `/event` is the runtime superset.
//! - Auth is HTTP Basic `opencode:<password>`; the password comes from the env var
//!   named in config (`OPENCODE_SERVER_PASSWORD` by default) and is never persisted.
//! - The attached full TUI is a **peer subscriber**: API replies dismiss its prompts,
//!   and operator answers are broadcast as `permission.replied` / `question.replied`.
//! - Race resolution is exactly-once; the loser gets `404 {…NotFoundError}`. So every
//!   reply is fire-and-forget and Telegram state is finalized by the host's own
//!   `*.replied` event, never by our POST succeeding.
//! - Rejected tool calls render unlabeled in the TUI whether rejected locally or via
//!   API (symmetric, OpenCode's own behaviour); a `/tui/show-toast` compensates.
//!
//! Structure: [`Translator`] is a pure state machine (host event → `BridgeMessage`s,
//! daemon message → [`HostCall`]s) with no I/O, unit-tested against the captured
//! samples. [`run`] is the thin network shell: SSE in, HTTP out, `ObserverLink` up.

use crate::config::{Config, OpenCodeHostConfig};
use crate::error::{AppError, Result};
use crate::host::link::{stamped, ApprovalFifo, Backoff, ObserverLink};
use crate::host::normalize_tool_name;
use crate::types::{BridgeMessage, HostKind, MessageType};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const KIND: HostKind = HostKind::OpenCode;
const BASIC_AUTH_USER: &str = "opencode";

/// A pending permission the daemon has been asked to decide, keyed per session in
/// FIFO order (both surfaces block on one at a time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPermission {
    pub request_id: String,
    pub directory: String,
}

/// What the observer must do against the OpenCode HTTP API. Produced by the pure
/// translator; executed by the runner. Each variant maps to exactly one endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCall {
    /// `POST /session/{id}/prompt_async?directory=` `{parts:[{type:"text",text}]}`
    Prompt {
        session_id: String,
        directory: String,
        text: String,
    },
    /// `POST /session/{id}/abort?directory=`
    Abort {
        session_id: String,
        directory: String,
    },
    /// `PATCH /session/{id}?directory=` `{title}`
    Rename {
        session_id: String,
        directory: String,
        title: String,
    },
    /// `POST /permission/{id}/reply?directory=` `{reply}`
    PermissionReply {
        request_id: String,
        directory: String,
        reply: String,
    },
    /// `POST /question/{id}/reply?directory=` `{answers}`
    QuestionReply {
        request_id: String,
        directory: String,
        answers: Value,
    },
    /// `POST /tui/show-toast` — best effort, attribution compensation.
    Toast { message: String, variant: String },
}

#[derive(Debug, Default, Clone)]
struct SessionCtx {
    directory: String,
    announced: bool,
}

/// Pure translation state. No I/O.
#[derive(Debug, Default)]
pub struct Translator {
    sessions: HashMap<String, SessionCtx>,
    /// messageID -> role, learned from `message.updated`, so text parts can be
    /// attributed to user vs assistant.
    roles: HashMap<String, String>,
    /// sessionID -> ordered (partID, text) of the assistant's text parts since the
    /// last `session.idle`. `message.part.updated` carries the FULL current text, so
    /// re-inserting by part id is idempotent.
    assistant_text: HashMap<String, Vec<(String, String)>>,
    /// callIDs whose `ToolStart` was already emitted (parts update repeatedly).
    tool_started: HashSet<String>,
    /// callIDs whose terminal `ToolResult` was emitted.
    tool_finished: HashSet<String>,
    /// User text parts already forwarded (parts can be re-sent).
    user_parts_seen: HashSet<String>,
    /// Requests WE answered (from Telegram), so a following `*.replied` is not
    /// misreported as "answered at terminal".
    replied_by_us: HashSet<String>,
    pub pending: ApprovalFifo<PendingPermission>,
}

impl Translator {
    pub fn new() -> Self {
        Self::default()
    }

    fn msg(
        &self,
        t: MessageType,
        session_id: &str,
        content: impl Into<String>,
        mut meta: Map<String, Value>,
    ) -> BridgeMessage {
        meta.insert("hostSessionId".into(), Value::String(session_id.into()));
        stamped(KIND, t, session_id, content, meta)
    }

    /// Ensure the daemon knows this session. Lazy: the first event for an unknown
    /// session announces it, so pre-existing idle sessions on the server do not each
    /// spawn a Telegram topic at observer start.
    fn ensure_announced(&mut self, session_id: &str, info: Option<&Value>) -> Vec<BridgeMessage> {
        let entry = self.sessions.entry(session_id.to_string()).or_default();
        if let Some(info) = info {
            if let Some(d) = info.get("directory").and_then(Value::as_str) {
                entry.directory = d.to_string();
            }
        }
        if entry.announced {
            return vec![];
        }
        entry.announced = true;
        let mut meta = Map::new();
        if !entry.directory.is_empty() {
            meta.insert("projectDir".into(), Value::String(entry.directory.clone()));
        }
        if let Some(t) = info.and_then(|i| i.get("title")).and_then(Value::as_str) {
            meta.insert("title".into(), Value::String(t.into()));
        }
        // ADR-013 lineage: OpenCode child sessions carry their own id + explicit parentID.
        if let Some(p) = info.and_then(|i| i.get("parentID")).and_then(Value::as_str) {
            meta.insert("parentSessionId".into(), Value::String(p.into()));
        }
        if let Some(a) = info.and_then(|i| i.get("agent")).and_then(Value::as_str) {
            meta.insert("agentType".into(), Value::String(a.into()));
        }
        meta.insert("entrypoint".into(), Value::String("cli".into()));
        vec![self.msg(MessageType::SessionStart, session_id, "", meta)]
    }

    fn directory_of(&self, session_id: &str) -> String {
        self.sessions
            .get(session_id)
            .map(|s| s.directory.clone())
            .unwrap_or_default()
    }

    /// Translate one SSE event (`{"id","type","properties"}`) into daemon messages.
    pub fn on_event(&mut self, ev: &Value) -> Vec<BridgeMessage> {
        let Some(t) = ev.get("type").and_then(Value::as_str) else {
            return vec![];
        };
        let p = ev.get("properties").cloned().unwrap_or(Value::Null);
        let sid = p
            .get("sessionID")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut out = Vec::new();

        match t {
            "session.created" | "session.updated" => {
                if let Some(sid) = sid {
                    out.extend(self.ensure_announced(&sid, p.get("info")));
                }
            }
            "session.deleted" => {
                if let Some(sid) = sid {
                    if self.sessions.remove(&sid).is_some_and(|s| s.announced) {
                        out.push(self.msg(MessageType::SessionEnd, &sid, "deleted", Map::new()));
                    }
                    self.assistant_text.remove(&sid);
                    self.pending.clear(&sid);
                }
            }
            "session.error" => {
                if let Some(sid) = sid {
                    out.extend(self.ensure_announced(&sid, None));
                    let err = p
                        .get("error")
                        .map(|e| {
                            e.get("data")
                                .and_then(|d| d.get("message"))
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .unwrap_or_else(|| e.to_string())
                        })
                        .unwrap_or_else(|| "unknown error".into());
                    out.push(self.msg(MessageType::Error, &sid, err, Map::new()));
                }
            }
            "session.idle" => {
                if let Some(sid) = sid {
                    out.extend(self.ensure_announced(&sid, None));
                    // Flush the turn's assistant text as ONE AgentResponse (same timing as
                    // Claude's Stop hook), then signal turn completion.
                    if let Some(parts) = self.assistant_text.remove(&sid) {
                        let text = parts
                            .into_iter()
                            .map(|(_, t)| t)
                            .filter(|t| !t.trim().is_empty())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        if !text.trim().is_empty() {
                            out.push(self.msg(MessageType::AgentResponse, &sid, text, Map::new()));
                        }
                    }
                    out.push(self.msg(MessageType::TurnComplete, &sid, "", Map::new()));
                }
            }
            "message.updated" => {
                if let Some(info) = p.get("info") {
                    if let (Some(id), Some(role)) = (
                        info.get("id").and_then(Value::as_str),
                        info.get("role").and_then(Value::as_str),
                    ) {
                        self.roles.insert(id.into(), role.into());
                    }
                }
            }
            "message.part.updated" => {
                if let (Some(sid), Some(part)) = (sid, p.get("part")) {
                    out.extend(self.ensure_announced(&sid, None));
                    out.extend(self.on_part(&sid, part));
                }
            }
            "permission.asked" => {
                if let Some(sid) = sid {
                    out.extend(self.ensure_announced(&sid, None));
                    out.extend(self.on_permission_asked(&sid, &p));
                }
            }
            "permission.replied" => {
                if let (Some(sid), Some(rid)) = (sid, p.get("requestID").and_then(Value::as_str)) {
                    let reply = p.get("reply").and_then(Value::as_str).unwrap_or("");
                    // Whoever answered, the request is done: drop our FIFO entry for it.
                    self.drop_pending(&sid, rid);
                    if !self.replied_by_us.remove(rid) {
                        // Operator answered at the terminal — tell the daemon so the
                        // Telegram keyboard is retired (ADR-016 both-surfaces).
                        let mut meta = Map::new();
                        meta.insert("source".into(), Value::String("terminal".into()));
                        meta.insert("hostRequestId".into(), Value::String(rid.into()));
                        out.push(self.msg(MessageType::ApprovalResponse, &sid, reply, meta));
                    }
                }
            }
            "question.asked" => {
                if let Some(sid) = sid {
                    out.extend(self.ensure_announced(&sid, None));
                    out.extend(self.on_question_asked(&sid, &p));
                }
            }
            "question.replied" | "question.rejected" => {
                if let (Some(sid), Some(rid)) = (sid, p.get("requestID").and_then(Value::as_str)) {
                    self.replied_by_us.remove(rid);
                    // Either surface answered: the daemon's resolve_pending_question is
                    // idempotent and arbitrates against an in-flight Submit All (ADR-015).
                    let mut meta = Map::new();
                    meta.insert("tool".into(), Value::String("AskUserQuestion".into()));
                    meta.insert("questionId".into(), Value::String(rid.into()));
                    let content = p
                        .get("answers")
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| "rejected".into());
                    out.push(self.msg(MessageType::ToolResult, &sid, content, meta));
                }
            }
            _ => {}
        }
        out
    }

    fn on_part(&mut self, sid: &str, part: &Value) -> Vec<BridgeMessage> {
        let mut out = Vec::new();
        let ptype = part.get("type").and_then(Value::as_str).unwrap_or("");
        let pid = part.get("id").and_then(Value::as_str).unwrap_or("");
        let mid = part.get("messageID").and_then(Value::as_str).unwrap_or("");
        match ptype {
            "text" => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                match self.roles.get(mid).map(String::as_str) {
                    Some("user") => {
                        if !pid.is_empty() && self.user_parts_seen.insert(pid.into()) {
                            let mut meta = Map::new();
                            meta.insert("source".into(), Value::String("cli".into()));
                            out.push(self.msg(MessageType::UserInput, sid, text, meta));
                        }
                    }
                    _ => {
                        // assistant (or role not yet known — treat as assistant; user parts
                        // always arrive after their message.updated in practice)
                        let parts = self.assistant_text.entry(sid.into()).or_default();
                        match parts.iter_mut().find(|(id, _)| id == pid) {
                            Some(slot) => slot.1 = text.into(),
                            None => parts.push((pid.into(), text.into())),
                        }
                    }
                }
            }
            "tool" => {
                let call_id = part.get("callID").and_then(Value::as_str).unwrap_or(pid);
                let native = part
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown");
                let tool = normalize_tool_name(KIND, native);
                let state = part.get("state").cloned().unwrap_or(Value::Null);
                let status = state.get("status").and_then(Value::as_str).unwrap_or("");
                let input = state.get("input").cloned().unwrap_or(json!({}));
                match status {
                    "running" => {
                        if self.tool_started.insert(call_id.into()) {
                            let mut meta = Map::new();
                            meta.insert("tool".into(), Value::String(tool));
                            meta.insert("input".into(), input);
                            meta.insert("toolUseId".into(), Value::String(call_id.into()));
                            out.push(self.msg(MessageType::ToolStart, sid, "", meta));
                        }
                    }
                    // Guard-side insert(): runs only when the pattern matches, so a repeated
                    // completed/error update for the same callID is deduped exactly once.
                    "completed" | "error" if self.tool_finished.insert(call_id.into()) => {
                        // A tool can complete without ctm ever seeing `running`
                        // (reconnect mid-call); emit the start so Telegram has context.
                        if self.tool_started.insert(call_id.into()) {
                            let mut meta = Map::new();
                            meta.insert("tool".into(), Value::String(tool.clone()));
                            meta.insert("input".into(), input.clone());
                            meta.insert("toolUseId".into(), Value::String(call_id.into()));
                            out.push(self.msg(MessageType::ToolStart, sid, "", meta));
                        }
                        let content = if status == "error" {
                            format!(
                                "error: {}",
                                state.get("error").and_then(Value::as_str).unwrap_or("")
                            )
                        } else {
                            state
                                .get("output")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string()
                        };
                        let mut meta = Map::new();
                        meta.insert("tool".into(), Value::String(tool));
                        meta.insert("input".into(), input);
                        meta.insert("toolUseId".into(), Value::String(call_id.into()));
                        out.push(self.msg(MessageType::ToolResult, sid, content, meta));
                    }
                    _ => {} // pending: input not yet known; or an already-finished callID
                }
            }
            _ => {} // step-start / step-finish / patch / reasoning: not mirrored
        }
        out
    }

    fn on_permission_asked(&mut self, sid: &str, p: &Value) -> Vec<BridgeMessage> {
        let Some(rid) = p.get("id").and_then(Value::as_str) else {
            return vec![];
        };
        let native = p
            .get("permission")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let tool = normalize_tool_name(KIND, native);
        let input = p.get("metadata").cloned().unwrap_or(json!({}));
        let prompt = crate::hook::format_tool_approval_prompt(&tool, &input);
        let directory = self.directory_of(sid);
        self.pending.push(
            sid,
            PendingPermission {
                request_id: rid.into(),
                directory,
            },
        );
        let mut meta = Map::new();
        meta.insert("tool".into(), Value::String(tool));
        meta.insert("input".into(), input);
        meta.insert("hostRequestId".into(), Value::String(rid.into()));
        if let Some(a) = p.get("always") {
            meta.insert("alwaysPatterns".into(), a.clone());
        }
        vec![self.msg(MessageType::ApprovalRequest, sid, prompt, meta)]
    }

    fn on_question_asked(&mut self, sid: &str, p: &Value) -> Vec<BridgeMessage> {
        let Some(rid) = p.get("id").and_then(Value::as_str) else {
            return vec![];
        };
        // OpenCode `QuestionInfo{question, header, options[{label,description}], multiple,
        // custom}` -> Claude AskUserQuestion `{questions:[{question, header, options,
        // multiSelect}]}`, which the daemon's existing renderer consumes unchanged.
        let questions: Vec<Value> = p
            .get("questions")
            .and_then(Value::as_array)
            .map(|qs| {
                qs.iter()
                    .map(|q| {
                        json!({
                            "question": q.get("question").and_then(Value::as_str).unwrap_or(""),
                            "header": q.get("header").and_then(Value::as_str).unwrap_or(""),
                            "options": q.get("options").cloned().unwrap_or(json!([])),
                            "multiSelect": q.get("multiple").and_then(Value::as_bool).unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let call_id = p
            .get("tool")
            .and_then(|t| t.get("callID"))
            .and_then(Value::as_str)
            .unwrap_or(rid);
        let mut meta = Map::new();
        meta.insert("tool".into(), Value::String("AskUserQuestion".into()));
        meta.insert("input".into(), json!({ "questions": questions }));
        meta.insert("toolUseId".into(), Value::String(call_id.into()));
        meta.insert("questionId".into(), Value::String(rid.into()));
        vec![self.msg(MessageType::ToolStart, sid, "", meta)]
    }

    fn drop_pending(&mut self, sid: &str, rid: &str) {
        // The FIFO may hold this id at the front (normal) or not at all (we already
        // popped it when answering). Pop only if it matches; otherwise leave the queue.
        if let Some(front) = self.pending.pop_for(sid) {
            if front.request_id != rid {
                // Not ours to drop — put it back at the front by re-pushing in order.
                let mut rest = Vec::new();
                while let Some(x) = self.pending.pop_for(sid) {
                    rest.push(x);
                }
                self.pending.push(sid, front);
                for x in rest {
                    self.pending.push(sid, x);
                }
            }
        }
    }

    /// Translate a daemon→observer message into host API calls.
    pub fn on_daemon(&mut self, msg: &BridgeMessage) -> Vec<HostCall> {
        let sid = msg.session_id.clone();
        let directory = self.directory_of(&sid);
        let meta = msg.meta();
        match msg.msg_type {
            MessageType::HostInject => match meta.action().unwrap_or("text") {
                "interrupt" | "abort" => vec![HostCall::Abort {
                    session_id: sid,
                    directory,
                }],
                "slash" => {
                    let cmd = msg.content.trim();
                    if let Some(title) = cmd.strip_prefix("/rename ") {
                        vec![HostCall::Rename {
                            session_id: sid,
                            directory,
                            title: title.trim().to_string(),
                        }]
                    } else {
                        // Other slash commands are TUI-local on OpenCode; deliver as text
                        // so the operator's intent is at least visible to the agent.
                        vec![HostCall::Prompt {
                            session_id: sid,
                            directory,
                            text: cmd.to_string(),
                        }]
                    }
                }
                _ => vec![HostCall::Prompt {
                    session_id: sid,
                    directory,
                    text: msg.content.clone(),
                }],
            },
            MessageType::QuestionResponse => {
                let Some(rid) = meta.question_id() else {
                    return vec![];
                };
                self.replied_by_us.insert(rid.into());
                let answers = meta.answers().cloned().unwrap_or(json!([]));
                vec![
                    HostCall::QuestionReply {
                        request_id: rid.into(),
                        directory,
                        answers,
                    },
                    HostCall::Toast {
                        message: "Answered from Telegram".into(),
                        variant: "info".into(),
                    },
                ]
            }
            MessageType::ApprovalResponse => {
                let Some(pending) = self.pending.pop_for(&sid) else {
                    tracing::info!(session_id = %sid, "OpenCode: approval response with nothing pending (already resolved)");
                    return vec![];
                };
                let (reply, toast, variant) = match msg.content.as_str() {
                    "approve" => ("once", "Approved from Telegram", "success"),
                    "abort" => ("reject", "Aborted from Telegram", "error"),
                    _ => ("reject", "Rejected from Telegram", "warning"),
                };
                self.replied_by_us.insert(pending.request_id.clone());
                let mut calls = vec![HostCall::PermissionReply {
                    request_id: pending.request_id,
                    directory: pending.directory.clone(),
                    reply: reply.into(),
                }];
                if msg.content == "abort" {
                    calls.push(HostCall::Abort {
                        session_id: sid,
                        directory: pending.directory,
                    });
                }
                calls.push(HostCall::Toast {
                    message: toast.into(),
                    variant: variant.into(),
                });
                calls
            }
            _ => vec![],
        }
    }
}

// ============================================================================ runner

struct Http {
    client: reqwest::Client,
    base: String,
    password: Option<String>,
}

impl Http {
    fn new(oc: &OpenCodeHostConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(AppError::Reqwest)?;
        let password = oc.resolve_password();
        if password.is_none() {
            // Doctor makes this a hard failure; the observer only warns because the
            // operator may be running an explicitly unauthenticated dev server.
            tracing::warn!(
                env = %oc.password_env,
                "OpenCode: no server password in environment — connecting unauthenticated"
            );
        }
        Ok(Self {
            client,
            base: oc.base_url.trim_end_matches('/').to_string(),
            password,
        })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let rb = self
            .client
            .request(method, format!("{}{}", self.base, path));
        match &self.password {
            Some(pw) => rb.basic_auth(BASIC_AUTH_USER, Some(pw)),
            None => rb,
        }
    }

    async fn call(&self, c: &HostCall) -> Result<()> {
        let (method, path, body, dir): (reqwest::Method, String, Value, Option<&str>) = match c {
            HostCall::Prompt {
                session_id,
                directory,
                text,
            } => (
                reqwest::Method::POST,
                format!("/session/{session_id}/prompt_async"),
                json!({ "parts": [{ "type": "text", "text": text }] }),
                Some(directory),
            ),
            HostCall::Abort {
                session_id,
                directory,
            } => (
                reqwest::Method::POST,
                format!("/session/{session_id}/abort"),
                json!({}),
                Some(directory),
            ),
            HostCall::Rename {
                session_id,
                directory,
                title,
            } => (
                reqwest::Method::PATCH,
                format!("/session/{session_id}"),
                json!({ "title": title }),
                Some(directory),
            ),
            HostCall::PermissionReply {
                request_id,
                directory,
                reply,
            } => (
                reqwest::Method::POST,
                format!("/permission/{request_id}/reply"),
                json!({ "reply": reply }),
                Some(directory),
            ),
            HostCall::QuestionReply {
                request_id,
                directory,
                answers,
            } => (
                reqwest::Method::POST,
                format!("/question/{request_id}/reply"),
                json!({ "answers": answers }),
                Some(directory),
            ),
            HostCall::Toast { message, variant } => (
                reqwest::Method::POST,
                "/tui/show-toast".into(),
                json!({ "message": message, "variant": variant }),
                None,
            ),
        };
        let mut rb = self.req(method, &path).json(&body);
        if let Some(d) = dir.filter(|d| !d.is_empty()) {
            rb = rb.query(&[("directory", d)]);
        }
        let resp = rb.send().await.map_err(AppError::Reqwest)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().await.unwrap_or_default();
        if matches!(c, HostCall::Toast { .. }) {
            // No TUI attached (or headless server): purely cosmetic, never an error.
            tracing::debug!(status = %status, "OpenCode: toast not shown");
            return Ok(());
        }
        // 404 on a reply = the operator answered first (exactly-once, spike-verified).
        if status == reqwest::StatusCode::NOT_FOUND
            && matches!(
                c,
                HostCall::PermissionReply { .. } | HostCall::QuestionReply { .. }
            )
        {
            tracing::info!(body = %text, "OpenCode: reply lost the race — already answered at terminal");
            return Ok(());
        }
        Err(AppError::Telegram(format!(
            "OpenCode {path} -> {status}: {}",
            crate::formatting::truncate(&text, 300)
        )))
    }
}

/// Run the OpenCode observer forever, reconnecting both legs with backoff.
pub async fn run(config: Arc<Config>, oc: OpenCodeHostConfig) {
    let mut backoff = Backoff::new();
    loop {
        match run_once(&config, &oc).await {
            Ok(()) => backoff.reset(),
            Err(e) => tracing::warn!(error = %e, "OpenCode observer stopped; will reconnect"),
        }
        tokio::time::sleep(backoff.delay()).await;
    }
}

/// One connection lifetime: connect both legs, pump until either drops. Public so
/// the end-to-end integration test (`tests/host_e2e.rs`) can drive a single run against
/// a real host binary with a stand-in daemon socket.
pub async fn run_once(config: &Config, oc: &OpenCodeHostConfig) -> Result<()> {
    let http = Http::new(oc)?;
    let mut link = ObserverLink::connect(KIND, &config.socket_path).await?;
    let mut tr = Translator::new();

    // SSE: legacy global stream (the only one carrying permission.*/question.*).
    let resp = http
        .req(reqwest::Method::GET, "/event")
        .header("Accept", "text/event-stream")
        .send()
        .await
        .map_err(AppError::Reqwest)?;
    if !resp.status().is_success() {
        return Err(AppError::Telegram(format!(
            "OpenCode /event -> {} (check {} and the server password)",
            resp.status(),
            oc.base_url
        )));
    }
    tracing::info!(base = %oc.base_url, "OpenCode observer connected");
    let mut resp = resp;
    let mut buf = String::new();

    loop {
        tokio::select! {
            chunk = resp.chunk() => {
                let Some(bytes) = chunk.map_err(AppError::Reqwest)? else {
                    return Err(AppError::Telegram("OpenCode /event stream ended".into()));
                };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                // SSE frames are separated by a blank line; each may hold several lines.
                while let Some(idx) = buf.find("\n\n") {
                    let frame = buf[..idx].to_string();
                    buf.drain(..idx + 2);
                    for line in frame.lines() {
                        let Some(data) = line.strip_prefix("data:") else { continue };
                        let Ok(ev) = serde_json::from_str::<Value>(data.trim()) else { continue };
                        for m in tr.on_event(&ev) {
                            link.send(&m).await?;
                        }
                    }
                }
            }
            down = link.recv() => {
                let Some(msg) = down else {
                    return Err(AppError::Socket("daemon link closed".into()));
                };
                for c in tr.on_daemon(&msg) {
                    if let Err(e) = http.call(&c).await {
                        tracing::warn!(error = %e, call = ?c, "OpenCode: host call failed");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim samples captured from OpenCode 1.18.31 `/event` during the ADR-016 spike.
    const SESSION_CREATED: &str = r#"{"id":"evt_1","type":"session.created","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","info":{"id":"ses_f44f3efa0ffeQKndY4KH1wmNdy","slug":"calm-rocket","version":"1.18.31","projectID":"global","directory":"/tmp/proj","path":"","title":"ctm-spike-probe-2","agent":"build"}}}"#;
    const PERM_ASKED: &str = r#"{"id":"evt_2","type":"permission.asked","properties":{"id":"per_0bb0c6b1a001YyxUCq7cG8N2nY","sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","permission":"bash","patterns":["echo CTM_SPIKE_MARKER_ALPHA"],"metadata":{"command":"echo CTM_SPIKE_MARKER_ALPHA"},"always":["echo *"],"tool":{"messageID":"msg_1","callID":"toolu_1"}}}"#;
    const PERM_REPLIED: &str = r#"{"id":"evt_3","type":"permission.replied","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","requestID":"per_0bb0c6b1a001YyxUCq7cG8N2nY","reply":"once"}}"#;
    const Q_ASKED: &str = r#"{"id":"evt_4","type":"question.asked","properties":{"id":"que_0bb0d706d001I2Q2SnPxzM2Xvi","sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","questions":[{"question":"which color do you prefer?","header":"Color preference","options":[{"label":"Red","description":"The color red"},{"label":"Blue","description":"The color blue"}]}],"tool":{"messageID":"msg_2","callID":"toolu_2"}}}"#;
    const Q_REPLIED: &str = r#"{"id":"evt_5","type":"question.replied","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","requestID":"que_0bb0d706d001I2Q2SnPxzM2Xvi","answers":[["Blue"]]}}"#;
    const MSG_ASSISTANT: &str = r#"{"id":"evt_6","type":"message.updated","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","info":{"id":"msg_a","role":"assistant","sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy"}}}"#;
    const MSG_USER: &str = r#"{"id":"evt_7","type":"message.updated","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","info":{"id":"msg_u","role":"user","sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy"}}}"#;
    const TOOL_RUNNING: &str = r#"{"id":"evt_8","type":"message.part.updated","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","part":{"type":"tool","tool":"bash","callID":"toolu_1","state":{"status":"running","input":{"command":"echo CTM_SPIKE_MARKER_ALPHA"}},"id":"prt_t1","messageID":"msg_a"}}}"#;
    const TOOL_DONE: &str = r#"{"id":"evt_9","type":"message.part.updated","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","part":{"type":"tool","tool":"bash","callID":"toolu_1","state":{"status":"completed","input":{"command":"echo CTM_SPIKE_MARKER_ALPHA"},"output":"CTM_SPIKE_MARKER_ALPHA\n"},"id":"prt_t1","messageID":"msg_a"}}}"#;
    const IDLE: &str = r#"{"id":"evt_10","type":"session.idle","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy"}}"#;

    fn ev(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }
    fn text_part(mid: &str, pid: &str, text: &str) -> Value {
        json!({"type":"message.part.updated","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","part":{"type":"text","text":text,"messageID":mid,"id":pid}}})
    }

    #[test]
    fn session_created_announces_once_with_host_kind_and_directory() {
        let mut t = Translator::new();
        let out = t.on_event(&ev(SESSION_CREATED));
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(m.msg_type, MessageType::SessionStart);
        assert_eq!(m.session_id, "ses_f44f3efa0ffeQKndY4KH1wmNdy");
        assert_eq!(m.meta().host_kind(), HostKind::OpenCode);
        assert_eq!(
            m.meta().host_session_id(),
            Some("ses_f44f3efa0ffeQKndY4KH1wmNdy")
        );
        assert_eq!(m.meta().project_dir(), Some("/tmp/proj"));
        assert!(
            t.on_event(&ev(SESSION_CREATED)).is_empty(),
            "no duplicate announce"
        );
    }

    #[test]
    fn permission_asked_becomes_approval_request_with_claude_tool_shape() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        let out = t.on_event(&ev(PERM_ASKED));
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(m.msg_type, MessageType::ApprovalRequest);
        assert_eq!(m.meta().tool(), Some("Bash"), "normalized from `bash`");
        assert_eq!(
            m.meta().input().unwrap()["command"],
            "echo CTM_SPIKE_MARKER_ALPHA"
        );
        assert!(
            m.content.contains("echo CTM_SPIKE_MARKER_ALPHA"),
            "prompt renders command"
        );
        assert_eq!(t.pending.pending("ses_f44f3efa0ffeQKndY4KH1wmNdy"), 1);
    }

    #[test]
    fn telegram_approve_replies_once_and_then_replied_event_is_not_misattributed() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        t.on_event(&ev(PERM_ASKED));
        let resp = BridgeMessage {
            msg_type: MessageType::ApprovalResponse,
            session_id: "ses_f44f3efa0ffeQKndY4KH1wmNdy".into(),
            timestamp: String::new(),
            content: "approve".into(),
            metadata: None,
        };
        let calls = t.on_daemon(&resp);
        assert!(
            matches!(&calls[0], HostCall::PermissionReply { request_id, reply, directory }
            if request_id == "per_0bb0c6b1a001YyxUCq7cG8N2nY" && reply == "once" && directory == "/tmp/proj")
        );
        assert!(matches!(&calls[1], HostCall::Toast { .. }));
        // The host then broadcasts permission.replied — since WE answered, the daemon
        // must NOT be told "answered at terminal".
        let out = t.on_event(&ev(PERM_REPLIED));
        assert!(out.is_empty(), "our own reply echo produces nothing");
        assert_eq!(t.pending.pending("ses_f44f3efa0ffeQKndY4KH1wmNdy"), 0);
    }

    #[test]
    fn operator_terminal_answer_informs_daemon() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        t.on_event(&ev(PERM_ASKED));
        let out = t.on_event(&ev(PERM_REPLIED));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].msg_type, MessageType::ApprovalResponse);
        assert_eq!(out[0].content, "once");
        assert_eq!(out[0].meta().source(), Some("terminal"));
        assert_eq!(
            t.pending.pending("ses_f44f3efa0ffeQKndY4KH1wmNdy"),
            0,
            "FIFO drained"
        );
    }

    #[test]
    fn abort_from_telegram_rejects_and_aborts() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        t.on_event(&ev(PERM_ASKED));
        let resp = BridgeMessage {
            msg_type: MessageType::ApprovalResponse,
            session_id: "ses_f44f3efa0ffeQKndY4KH1wmNdy".into(),
            timestamp: String::new(),
            content: "abort".into(),
            metadata: None,
        };
        let calls = t.on_daemon(&resp);
        assert!(matches!(&calls[0], HostCall::PermissionReply { reply, .. } if reply == "reject"));
        assert!(matches!(&calls[1], HostCall::Abort { .. }));
    }

    #[test]
    fn question_asked_maps_to_askuserquestion_tool_start_and_replied_resolves() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        let out = t.on_event(&ev(Q_ASKED));
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(m.msg_type, MessageType::ToolStart);
        assert_eq!(m.meta().tool(), Some("AskUserQuestion"));
        assert_eq!(
            m.meta().question_id(),
            Some("que_0bb0d706d001I2Q2SnPxzM2Xvi")
        );
        let qs = &m.meta().input().unwrap()["questions"];
        assert_eq!(qs[0]["header"], "Color preference");
        assert_eq!(qs[0]["options"][1]["label"], "Blue");
        assert_eq!(qs[0]["multiSelect"], false);

        let out = t.on_event(&ev(Q_REPLIED));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].msg_type, MessageType::ToolResult);
        assert_eq!(out[0].meta().tool(), Some("AskUserQuestion"));
    }

    #[test]
    fn submit_all_from_telegram_posts_labels_to_question_reply() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        t.on_event(&ev(Q_ASKED));
        let mut meta = Map::new();
        meta.insert(
            "questionId".into(),
            Value::String("que_0bb0d706d001I2Q2SnPxzM2Xvi".into()),
        );
        meta.insert("answers".into(), json!([["Blue"]]));
        let qr = BridgeMessage {
            msg_type: MessageType::QuestionResponse,
            session_id: "ses_f44f3efa0ffeQKndY4KH1wmNdy".into(),
            timestamp: String::new(),
            content: String::new(),
            metadata: Some(meta),
        };
        let calls = t.on_daemon(&qr);
        assert!(
            matches!(&calls[0], HostCall::QuestionReply { request_id, answers, directory }
            if request_id == "que_0bb0d706d001I2Q2SnPxzM2Xvi" && *answers == json!([["Blue"]]) && directory == "/tmp/proj")
        );
    }

    #[test]
    fn tool_parts_emit_start_once_and_result_once() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        t.on_event(&ev(MSG_ASSISTANT));
        let a = t.on_event(&ev(TOOL_RUNNING));
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].msg_type, MessageType::ToolStart);
        assert_eq!(a[0].meta().tool(), Some("Bash"));
        assert_eq!(a[0].meta().tool_use_id(), Some("toolu_1"));
        assert!(
            t.on_event(&ev(TOOL_RUNNING)).is_empty(),
            "repeat running update is deduped"
        );
        let b = t.on_event(&ev(TOOL_DONE));
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].msg_type, MessageType::ToolResult);
        assert_eq!(b[0].content, "CTM_SPIKE_MARKER_ALPHA\n");
        assert!(
            t.on_event(&ev(TOOL_DONE)).is_empty(),
            "repeat completed is deduped"
        );
    }

    #[test]
    fn assistant_text_accumulates_and_flushes_on_idle_as_one_agent_response() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        t.on_event(&ev(MSG_ASSISTANT));
        assert!(t.on_event(&text_part("msg_a", "prt_1", "Hel")).is_empty());
        assert!(
            t.on_event(&text_part("msg_a", "prt_1", "Hello")).is_empty(),
            "full-text overwrite"
        );
        assert!(t.on_event(&text_part("msg_a", "prt_2", "world")).is_empty());
        let out = t.on_event(&ev(IDLE));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].msg_type, MessageType::AgentResponse);
        assert_eq!(out[0].content, "Hello\n\nworld");
        assert_eq!(out[1].msg_type, MessageType::TurnComplete);
        // Next idle with no new text: only TurnComplete.
        let out = t.on_event(&ev(IDLE));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].msg_type, MessageType::TurnComplete);
    }

    #[test]
    fn user_text_parts_are_forwarded_once_as_user_input() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        t.on_event(&ev(MSG_USER));
        let out = t.on_event(&text_part("msg_u", "prt_u", "run ls"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].msg_type, MessageType::UserInput);
        assert_eq!(out[0].content, "run ls");
        assert_eq!(out[0].meta().source(), Some("cli"));
        assert!(t
            .on_event(&text_part("msg_u", "prt_u", "run ls"))
            .is_empty());
    }

    #[test]
    fn host_inject_actions_map_to_endpoints() {
        let mut t = Translator::new();
        t.on_event(&ev(SESSION_CREATED));
        let mk = |action: &str, content: &str| {
            let mut meta = Map::new();
            meta.insert("action".into(), Value::String(action.into()));
            BridgeMessage {
                msg_type: MessageType::HostInject,
                session_id: "ses_f44f3efa0ffeQKndY4KH1wmNdy".into(),
                timestamp: String::new(),
                content: content.into(),
                metadata: Some(meta),
            }
        };
        assert!(
            matches!(&t.on_daemon(&mk("text", "hi"))[0], HostCall::Prompt { text, .. } if text == "hi")
        );
        assert!(matches!(
            &t.on_daemon(&mk("interrupt", ""))[0],
            HostCall::Abort { .. }
        ));
        assert!(
            matches!(&t.on_daemon(&mk("slash", "/rename New Title"))[0], HostCall::Rename { title, .. } if title == "New Title")
        );
        assert!(
            matches!(&t.on_daemon(&mk("slash", "/clear"))[0], HostCall::Prompt { text, .. } if text == "/clear")
        );
    }

    #[test]
    fn session_deleted_ends_only_announced_sessions() {
        let mut t = Translator::new();
        let del =
            json!({"type":"session.deleted","properties":{"sessionID":"ses_never_seen","info":{}}});
        assert!(
            t.on_event(&del).is_empty(),
            "unknown session: nothing to end"
        );
        t.on_event(&ev(SESSION_CREATED));
        let del = json!({"type":"session.deleted","properties":{"sessionID":"ses_f44f3efa0ffeQKndY4KH1wmNdy","info":{}}});
        let out = t.on_event(&del);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].msg_type, MessageType::SessionEnd);
    }
}
