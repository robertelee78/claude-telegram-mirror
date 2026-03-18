use crate::config;
use crate::error::{AppError, Result};
use crate::formatting;
use crate::injector::{self, InputInjector};
use crate::types::{self, BridgeMessage, HookEvent, MessageType, MAX_LINE_BYTES, SAFE_COMMANDS};
use std::io::Read;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{timeout, Duration};

/// Process a hook event from stdin.
/// This is the entry point for `ctm hook`.
pub async fn process_hook() -> anyhow::Result<()> {
    let cfg = config::load_config(false)?;

    // Read stdin with size limit
    let mut input = String::new();
    let bytes_read = std::io::stdin()
        .take(MAX_LINE_BYTES as u64)
        .read_to_string(&mut input)?;

    if bytes_read >= MAX_LINE_BYTES {
        tracing::warn!(
            bytes = bytes_read,
            max = MAX_LINE_BYTES,
            "Hook stdin exceeded size limit"
        );
        return Ok(());
    }

    let input = input.trim();
    if input.is_empty() {
        return Ok(());
    }

    // Parse the hook event
    let event: HookEvent = match serde_json::from_str(input) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(error = %e, "Failed to parse hook event JSON");
            return Ok(());
        }
    };

    // Get session ID and validate.
    //
    // L6.4 (INTENTIONAL): Events with missing or invalid session IDs are dropped
    // rather than generating a synthetic fallback ID.  This is more correct than
    // the TypeScript implementation which fabricated IDs like `unknown-<timestamp>`,
    // because synthetic IDs cannot be correlated back to real sessions and produce
    // orphaned database rows.  Dropping is safe: the event simply goes unmirrored.
    let session_id = get_session_id(&event);
    if !types::is_valid_session_id(&session_id) {
        tracing::warn!(
            session_id = %session_id.chars().take(20).collect::<String>(),
            "Invalid session ID, skipping"
        );
        return Ok(());
    }

    // Get tmux info from environment
    let tmux_info = InputInjector::detect_tmux_session();
    let hostname = injector::get_hostname();

    // Build messages to send
    let messages = build_messages(&event, &session_id, &tmux_info, &hostname, &cfg).await;

    // Check for hook output (PreToolUse approval)
    let hook_output = get_hook_output(&event, &session_id, &cfg).await;

    // Send messages to bridge daemon via socket
    if !messages.is_empty() && cfg.socket_path.exists() {
        if let Err(e) = send_messages(&cfg.socket_path, &messages).await {
            tracing::debug!(error = %e, "Failed to send to bridge (daemon may not be running)");
        }
    }

    // Write hook output to stdout (for PreToolUse)
    if let Some(output) = hook_output {
        print!("{}", output);
    }

    // Pass through original input (required by hook contract)
    // Actually, Claude Code hooks just need stdout output for PreToolUse.
    // Other hooks: stdout is passed through to Claude as context.

    Ok(())
}

/// Extract session ID from any hook event
fn get_session_id(event: &HookEvent) -> String {
    match event {
        HookEvent::Stop(e) => e.base.session_id.clone(),
        HookEvent::SubagentStop(e) => e.base.session_id.clone(),
        HookEvent::PreToolUse(e) => e.base.session_id.clone(),
        HookEvent::PostToolUse(e) => e.base.session_id.clone(),
        HookEvent::Notification(e) => e.base.session_id.clone(),
        HookEvent::UserPromptSubmit(e) => e.base.session_id.clone(),
        HookEvent::PreCompact(e) => e.base.session_id.clone(),
        HookEvent::SessionEnd(e) => e.base.session_id.clone(),
    }
}

/// Extract transcript path from any hook event
fn get_transcript_path(event: &HookEvent) -> Option<&str> {
    let base = match event {
        HookEvent::Stop(e) => &e.base,
        HookEvent::SubagentStop(e) => &e.base,
        HookEvent::PreToolUse(e) => &e.base,
        HookEvent::PostToolUse(e) => &e.base,
        HookEvent::Notification(e) => &e.base,
        HookEvent::UserPromptSubmit(e) => &e.base,
        HookEvent::PreCompact(e) => &e.base,
        HookEvent::SessionEnd(e) => &e.base,
    };
    base.transcript_path.as_deref()
}

/// Extract cwd (project directory) from any hook event
fn get_cwd(event: &HookEvent) -> Option<&str> {
    let base = match event {
        HookEvent::Stop(e) => &e.base,
        HookEvent::SubagentStop(e) => &e.base,
        HookEvent::PreToolUse(e) => &e.base,
        HookEvent::PostToolUse(e) => &e.base,
        HookEvent::Notification(e) => &e.base,
        HookEvent::UserPromptSubmit(e) => &e.base,
        HookEvent::PreCompact(e) => &e.base,
        HookEvent::SessionEnd(e) => &e.base,
    };
    base.cwd.as_deref()
}

/// GAP-8: Extract agent_id from any hook event's base fields.
fn get_agent_id(event: &HookEvent) -> Option<&str> {
    let base = match event {
        HookEvent::Stop(e) => &e.base,
        HookEvent::SubagentStop(e) => &e.base,
        HookEvent::PreToolUse(e) => &e.base,
        HookEvent::PostToolUse(e) => &e.base,
        HookEvent::Notification(e) => &e.base,
        HookEvent::UserPromptSubmit(e) => &e.base,
        HookEvent::PreCompact(e) => &e.base,
        HookEvent::SessionEnd(e) => &e.base,
    };
    base.agent_id.as_deref()
}

/// GAP-8: Extract agent_type from any hook event's base fields.
fn get_agent_type(event: &HookEvent) -> Option<&str> {
    let base = match event {
        HookEvent::Stop(e) => &e.base,
        HookEvent::SubagentStop(e) => &e.base,
        HookEvent::PreToolUse(e) => &e.base,
        HookEvent::PostToolUse(e) => &e.base,
        HookEvent::Notification(e) => &e.base,
        HookEvent::UserPromptSubmit(e) => &e.base,
        HookEvent::PreCompact(e) => &e.base,
        HookEvent::SessionEnd(e) => &e.base,
    };
    base.agent_type.as_deref()
}

/// Build metadata object with tmux info, hostname, project dir, agent info, and headless flag
fn build_metadata(
    tmux_info: &Option<injector::TmuxInfo>,
    hostname: &str,
    transcript_path: Option<&str>,
    project_dir: Option<&str>,
    agent_id: Option<&str>,
    agent_type: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut meta = serde_json::Map::new();
    if let Some(info) = tmux_info {
        meta.insert(
            "tmuxTarget".into(),
            serde_json::Value::String(info.target.clone()),
        );
        if let Some(socket) = &info.socket {
            meta.insert(
                "tmuxSocket".into(),
                serde_json::Value::String(socket.clone()),
            );
        }
    }
    if !hostname.is_empty() {
        meta.insert(
            "hostname".into(),
            serde_json::Value::String(hostname.to_string()),
        );
    }
    if let Some(path) = transcript_path {
        meta.insert(
            "transcript_path".into(),
            serde_json::Value::String(path.to_string()),
        );
    }
    if let Some(dir) = project_dir {
        meta.insert(
            "projectDir".into(),
            serde_json::Value::String(dir.to_string()),
        );
    }
    // GAP-8: Include agent_id and agent_type from the hook event base if present
    if let Some(aid) = agent_id {
        meta.insert(
            "agentId".into(),
            serde_json::Value::String(aid.to_string()),
        );
    }
    if let Some(at) = agent_type {
        meta.insert(
            "agentType".into(),
            serde_json::Value::String(at.to_string()),
        );
    }
    // GAP-9: Include headless flag if CLAUDE_CODE_HEADLESS is set
    if std::env::var("CLAUDE_CODE_HEADLESS")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
    {
        meta.insert("headless".into(), serde_json::Value::Bool(true));
    }
    meta
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn make_message(
    msg_type: MessageType,
    session_id: &str,
    content: &str,
    metadata: serde_json::Map<String, serde_json::Value>,
) -> BridgeMessage {
    BridgeMessage {
        msg_type,
        session_id: session_id.to_string(),
        timestamp: now_iso(),
        content: content.to_string(),
        metadata: Some(metadata),
    }
}

/// Build bridge messages for the given hook event.
///
/// This function serves as the Rust equivalent of both `buildMessages()` and
/// `handleAgentResponse()` from the TypeScript implementation. In TS, agent
/// responses were handled by a dedicated method; here, `build_messages` generates
/// `agent_response` messages from any source — `transcript_summary`,
/// `last_assistant_message`, or JSONL transcript file I/O — as part of the
/// unified message construction pipeline.  No standalone `handle_agent_response`
/// method is needed because the Rust architecture builds all messages in a
/// single pass rather than dispatching to separate per-type handlers.
async fn build_messages(
    event: &HookEvent,
    session_id: &str,
    tmux_info: &Option<injector::TmuxInfo>,
    hostname: &str,
    cfg: &config::Config,
) -> Vec<BridgeMessage> {
    let transcript_path = get_transcript_path(event);
    let project_dir = get_cwd(event);
    let agent_id = get_agent_id(event);
    let agent_type = get_agent_type(event);
    let meta = build_metadata(tmux_info, hostname, transcript_path, project_dir, agent_id, agent_type);
    let mut messages = Vec::new();

    // C3.1: Always send session_start as the first message in every batch.
    // The daemon's ensure_session_exists / create_session handles dedup — if
    // the session already exists this is a no-op update.  This guarantees the
    // daemon knows about the session even on the very first hook invocation.
    //
    // NOTE (ADR-006 M4.4): session_start is sent on every hook invocation.
    // This is intentional. The daemon's create_session is idempotent (INSERT OR
    // REPLACE), so duplicates are harmless. This simplifies the hook (no state
    // tracking) at the cost of minor socket overhead.
    messages.push(make_message(
        MessageType::SessionStart,
        session_id,
        "Claude Code session started",
        meta.clone(),
    ));

    match event {
        HookEvent::PreToolUse(e) => {
            // Send tool_start (fire-and-forget preview)
            let mut tool_meta = meta.clone();
            tool_meta.insert(
                "tool".into(),
                serde_json::Value::String(e.tool_name.clone()),
            );
            tool_meta.insert("input".into(), e.tool_input.clone());
            if let Some(id) = &e.tool_use_id {
                tool_meta.insert("toolUseId".into(), serde_json::Value::String(id.clone()));
            }
            messages.push(make_message(
                MessageType::ToolStart,
                session_id,
                &e.tool_name,
                tool_meta,
            ));
        }
        HookEvent::PostToolUse(e) => {
            // H2: fall back to tool_error when tool_output is absent
            let output = e
                .tool_output
                .as_deref()
                .filter(|s| !s.is_empty())
                .or(e.tool_error.as_deref())
                .unwrap_or("No output");
            // M4.2: Send full output — let the daemon's formatting/chunking layer
            // handle display truncation. Truncating here at 2000 chars lost
            // important tool output (stack traces, large diffs, etc.).
            if cfg.verbose || (output.len() >= 10 && !output.trim().is_empty()) {
                let mut tool_meta = meta.clone();
                tool_meta.insert(
                    "tool".into(),
                    serde_json::Value::String(e.tool_name.clone()),
                );
                // H3: include tool_input in metadata
                tool_meta.insert("input".into(), e.tool_input.clone());
                // H2: include tool_error in metadata under "error" key
                if let Some(err) = &e.tool_error {
                    tool_meta.insert("error".into(), serde_json::Value::String(err.clone()));
                }
                messages.push(make_message(
                    MessageType::ToolResult,
                    session_id,
                    output,
                    tool_meta,
                ));
            }
        }
        HookEvent::Notification(e) => {
            // Skip idle_prompt notifications
            if e.notification_type.as_deref() == Some("idle_prompt") {
                return messages;
            }
            let msg_type = if e.level.as_deref() == Some("error") {
                MessageType::Error
            } else {
                MessageType::AgentResponse
            };
            messages.push(make_message(msg_type, session_id, &e.message, meta));
        }
        HookEvent::UserPromptSubmit(e) => {
            let mut prompt_meta = meta.clone();
            prompt_meta.insert("source".into(), serde_json::Value::String("cli".into()));
            messages.push(make_message(
                MessageType::UserInput,
                session_id,
                &e.prompt,
                prompt_meta,
            ));
        }
        HookEvent::Stop(e) => {
            // H4: check transcript_summary / last_assistant_message before expensive JSONL I/O
            let summary_text: Option<String> = e
                .transcript_summary
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    e.last_assistant_message
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                });

            if let Some(text) = summary_text {
                messages.push(make_message(
                    MessageType::AgentResponse,
                    session_id,
                    &text,
                    meta.clone(),
                ));
            } else if let Some(path) = transcript_path {
                // Fall back to JSONL file I/O when no inline summary is provided
                if let Some(text) = extract_transcript_text(path, session_id, &cfg.config_dir) {
                    if !text.is_empty() {
                        messages.push(make_message(
                            MessageType::AgentResponse,
                            session_id,
                            &text,
                            meta.clone(),
                        ));
                    }
                }
            }

            // Check for session rename (custom-title in JSONL)
            if let Some(path) = transcript_path {
                if let Some(title) = check_custom_title(path) {
                    messages.push(make_message(
                        MessageType::SessionRename,
                        session_id,
                        &title,
                        meta.clone(),
                    ));
                }
            }

            // Send turn_complete
            messages.push(make_message(
                MessageType::TurnComplete,
                session_id,
                "",
                meta.clone(),
            ));

            // NOTE: We intentionally do NOT send session_end here. The Stop hook
            // fires after every assistant turn, not just on process exit. Sending
            // session_end would mark the session as ended and trigger topic deletion
            // after every turn, causing a cycle of session death and recreation.
            // Real session cleanup is handled by the SessionEnd hook event,
            // which fires exactly once when the session actually terminates.
            // Stale session detection in cleanup.rs serves as a fallback.
        }
        HookEvent::SubagentStop(e) => {
            // ADR-013 Part C: Generate an agent_response message for sub-agent completion.
            // This flows through to Telegram as a message in the parent topic.
            let agent_id = e.subagent_id.as_deref().unwrap_or("unknown");
            let result_text = e.result.as_deref().unwrap_or("(no result summary)");

            // Extract agent_id and agent_type from the transcript_path if available
            let transcript_agent_id = transcript_path
                .and_then(|tp| crate::types::extract_agent_id(tp));
            let display_agent_id = transcript_agent_id
                .as_deref()
                .unwrap_or(agent_id);

            let agent_type_str = e.agent_type.as_deref().unwrap_or("");
            let content = if agent_type_str.is_empty() {
                format!("\u{2705} Agent completed: {display_agent_id}\n\n{result_text}")
            } else {
                format!("\u{2705} Agent completed: {display_agent_id} ({agent_type_str})\n\n{result_text}")
            };

            let mut agent_meta = meta.clone();
            agent_meta.insert(
                "agentId".into(),
                serde_json::Value::String(display_agent_id.to_string()),
            );
            // Include the subagent_id from the event if present
            if let Some(ref sub_id) = e.subagent_id {
                agent_meta.insert(
                    "subagentId".into(),
                    serde_json::Value::String(sub_id.clone()),
                );
            }
            // ADR-013 GAP-2: Include agent_type in metadata for spawn notification and message labeling.
            // The SubagentStopEvent may carry agent_type from upstream. If not, we cannot infer it.
            if let Some(ref agent_type) = e.agent_type {
                agent_meta.insert(
                    "agentType".into(),
                    serde_json::Value::String(agent_type.clone()),
                );
            }

            messages.push(make_message(
                MessageType::AgentResponse,
                session_id,
                &content,
                agent_meta,
            ));
        }
        HookEvent::PreCompact(_) => {
            messages.push(make_message(MessageType::PreCompact, session_id, "", meta));
        }
        HookEvent::SessionEnd(e) => {
            // SessionEnd fires exactly once when the session actually terminates
            // (process exit, /clear, logout, etc.) — unlike Stop which fires
            // after every turn. This is the correct place to send session_end.
            let reason = e.reason.as_deref().unwrap_or("unknown");
            messages.push(make_message(
                MessageType::SessionEnd,
                session_id,
                reason,
                meta.clone(),
            ));

            // Clean up transcript state file
            let state_file = cfg.config_dir.join(format!(".last_line_{}", session_id));
            if state_file.exists() {
                let _ = std::fs::remove_file(&state_file);
            }
        }
    }

    messages
}

/// Get hook output for PreToolUse (approval workflow)
async fn get_hook_output(
    event: &HookEvent,
    session_id: &str,
    cfg: &config::Config,
) -> Option<String> {
    let pre_tool = match event {
        HookEvent::PreToolUse(e) => e,
        _ => return None,
    };

    // Check bypass mode
    if pre_tool.base.permission_mode.as_deref() == Some("bypassPermissions") {
        return None;
    }

    // Check if tool requires approval
    if !tool_requires_approval(&pre_tool.tool_name, &pre_tool.tool_input) {
        return None;
    }

    // Send approval request and wait for response
    if !cfg.socket_path.exists() {
        return None;
    }

    let tmux_info = InputInjector::detect_tmux_session();
    let hostname = injector::get_hostname();
    let meta = build_metadata(
        &tmux_info,
        &hostname,
        get_transcript_path(event),
        get_cwd(event),
        get_agent_id(event),
        get_agent_type(event),
    );

    let mut approval_meta = meta;
    approval_meta.insert(
        "tool".into(),
        serde_json::Value::String(pre_tool.tool_name.clone()),
    );
    approval_meta.insert("input".into(), pre_tool.tool_input.clone());
    // M5.1: Include hookId in approval_request metadata so the daemon can
    // correlate the approval response back to the originating hook instance.
    if let Some(hook_id) = &pre_tool.base.hook_id {
        approval_meta.insert("hookId".into(), serde_json::Value::String(hook_id.clone()));
    }

    let prompt = format_tool_approval_prompt(&pre_tool.tool_name, &pre_tool.tool_input);
    let msg = make_message(
        MessageType::ApprovalRequest,
        session_id,
        &prompt,
        approval_meta,
    );

    // L20: Probe the socket first so we can distinguish "daemon not running"
    // (connection refused / no socket) from a real timeout (approval expired).
    // If the socket file exists but connect fails immediately we treat it the
    // same way — the daemon is not available so we return None and let Claude
    // continue normally rather than blocking on a phantom approval request.
    match send_and_wait(&cfg.socket_path, &msg, Duration::from_secs(300)).await {
        Ok(response) => {
            let action = response.content.as_str();
            let (decision, reason) = match action {
                "approve" => ("allow", "Approved via Telegram"),
                "reject" => (
                    "deny",
                    "Rejected via Telegram. The user denied this tool execution.",
                ),
                "abort" => (
                    "deny",
                    "Session aborted via Telegram. The user chose to stop the session.",
                ),
                _ => (
                    "ask",
                    "Telegram approval timed out. Falling back to CLI approval.",
                ),
            };

            Some(format!(
                "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"{}\",\"permissionDecisionReason\":\"{}\"}}}}",
                decision, reason
            ))
        }
        Err(AppError::Socket(ref msg_str)) if msg_str.contains("Failed to connect") => {
            // L20: Connection refused — daemon is not running.
            // Return None so Claude continues normally without blocking.
            tracing::debug!(
                "Approval socket connect failed (daemon not running), letting Claude continue"
            );
            None
        }
        Err(_) => {
            // L20: Timeout — approval window expired, escalate to CLI.
            Some(
                "{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"ask\",\"permissionDecisionReason\":\"Telegram approval timed out.\"}}"
                    .to_string(),
            )
        }
    }
}

/// H5: Format a rich approval prompt matching TypeScript's formatToolDescription()
///
/// L5.4 (INTENTIONAL): String previews are truncated at display-friendly limits.
/// Bash commands use 200 chars, edit old/new strings use 200 chars, Write content
/// uses 500 chars, and generic JSON input uses 500 chars. These limits apply only
/// to the Telegram display prompt. The full, untruncated tool input is always
/// available in the `approval_request` message's `metadata.input` field, so no
/// data is lost for programmatic consumers.
fn format_tool_approval_prompt(tool_name: &str, tool_input: &serde_json::Value) -> String {
    let mut desc = format!("\u{1F527} **Tool:** `{tool_name}`\n\n");
    match tool_name {
        "Write" => {
            let file_path = tool_input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            let content = tool_input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let preview = formatting::truncate(content, 500);
            desc.push_str(&format!(
                "\u{1F4DD} **File:** `{file_path}`\n**Content preview:**\n```\n{preview}\n```"
            ));
        }
        "Edit" | "MultiEdit" => {
            let file_path = tool_input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            desc.push_str(&format!("\u{270F}\u{FE0F} **File:** `{file_path}`"));
            if let Some(old) = tool_input.get("old_string").and_then(|v| v.as_str()) {
                let snip = formatting::truncate(old, 200);
                desc.push_str(&format!("\n**Old:** ```{snip}```"));
            }
            if let Some(new) = tool_input.get("new_string").and_then(|v| v.as_str()) {
                let snip = formatting::truncate(new, 200);
                desc.push_str(&format!("\n**New:** ```{snip}```"));
            }
        }
        "Bash" => {
            let command = tool_input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let cmd = formatting::truncate(command, 200);
            desc.push_str(&format!("\u{1F4BB} **Command:**\n```bash\n{cmd}\n```"));
        }
        _ => {
            let json = serde_json::to_string_pretty(tool_input).unwrap_or_default();
            let truncated = formatting::truncate(&json, 500);
            desc.push_str(&format!("**Input:**\n```json\n{truncated}\n```"));
        }
    }
    desc
}

/// Check if a tool requires Telegram approval
fn tool_requires_approval(tool_name: &str, tool_input: &serde_json::Value) -> bool {
    match tool_name {
        "Write" | "Edit" | "MultiEdit" => true,
        "Bash" => {
            let command = tool_input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let first_word = command.split_whitespace().next().unwrap_or("");
            !SAFE_COMMANDS.contains(&first_word)
        }
        _ => false,
    }
}

/// S-1: Validate a transcript_path before opening it.
///
/// Accepts only absolute paths that canonicalize to a location inside the
/// user's home directory.  This prevents path traversal attacks where a
/// malicious hook payload crafts a `transcript_path` pointing outside the
/// expected location (e.g. `/etc/passwd` or `/../sensitive`).
///
/// Returns `Some(canonicalized_path)` on success, `None` if validation fails.
pub(crate) fn validate_transcript_path(raw: &str) -> Option<std::path::PathBuf> {
    use std::path::Path;

    let p = Path::new(raw);

    // Must be absolute
    if !p.is_absolute() {
        tracing::warn!(path = raw, "transcript_path is not absolute, skipping");
        return None;
    }

    // Canonicalize — resolves symlinks and removes `..` components.
    // This also verifies the path exists on disk.
    let canonical = match std::fs::canonicalize(p) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = raw, error = %e, "transcript_path canonicalization failed, skipping");
            return None;
        }
    };

    // Must reside within the user's home directory
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    if !canonical.starts_with(&home) {
        tracing::warn!(
            path = raw,
            canonical = %canonical.display(),
            home = %home.display(),
            "transcript_path is outside home directory, skipping"
        );
        return None;
    }

    Some(canonical)
}

/// Extract new assistant text from the transcript JSONL file
fn extract_transcript_text(
    transcript_path: &str,
    session_id: &str,
    config_dir: &std::path::Path,
) -> Option<String> {
    use std::fs;
    use std::io::BufRead;

    // S-1: Validate path before opening (prevents path traversal)
    let path = validate_transcript_path(transcript_path)?;

    if !path.exists() {
        return None;
    }

    // State file tracks last processed line
    let state_file = config_dir.join(format!(".last_line_{}", session_id));
    let last_line: usize = fs::read_to_string(&state_file)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    let file = fs::File::open(&path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut current_line = 0;
    let mut text_parts = Vec::new();

    for line in reader.lines() {
        current_line += 1;
        if current_line <= last_line {
            continue;
        }

        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        // Parse JSONL record — look for assistant text content
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) {
            if record.get("type").and_then(|t| t.as_str()) == Some("assistant") {
                if let Some(message) = record.get("message") {
                    if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
                        for block in content {
                            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    text_parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Update state file
    if current_line > last_line {
        let _ = fs::write(&state_file, current_line.to_string());
    }

    if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join("\n\n"))
    }
}

/// Check for custom-title record in the transcript JSONL (session rename detection)
fn check_custom_title(transcript_path: &str) -> Option<String> {
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};

    // S-1: Validate path before opening (prevents path traversal)
    let validated = validate_transcript_path(transcript_path)?;
    let mut file = fs::File::open(&validated).ok()?;
    let file_size = file.metadata().ok()?.len();

    // Read last 8KB
    let read_size = std::cmp::min(8192, file_size) as usize;
    let offset = file_size.saturating_sub(read_size as u64);
    file.seek(SeekFrom::Start(offset)).ok()?;

    let mut buffer = vec![0u8; read_size];
    file.read_exact(&mut buffer).ok()?;

    let tail = String::from_utf8_lossy(&buffer);

    // Search backwards for the most recent custom-title
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
            if record.get("type").and_then(|t| t.as_str()) == Some("custom-title") {
                if let Some(title) = record.get("customTitle").and_then(|t| t.as_str()) {
                    return Some(title.to_string());
                }
            }
        }
    }

    None
}

/// Send messages to the bridge daemon via Unix socket (NDJSON)
async fn send_messages(socket_path: &std::path::Path, messages: &[BridgeMessage]) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| AppError::Socket(format!("Failed to connect: {}", e)))?;

    for msg in messages {
        let json = serde_json::to_string(msg)?;
        stream
            .write_all(format!("{}\n", json).as_bytes())
            .await
            .map_err(|e| AppError::Socket(format!("Failed to write: {}", e)))?;
    }

    stream
        .shutdown()
        .await
        .map_err(|e| AppError::Socket(format!("Failed to shutdown: {}", e)))?;

    Ok(())
}

/// Send a message and wait for a correlated response (for approval workflow)
async fn send_and_wait(
    socket_path: &std::path::Path,
    message: &BridgeMessage,
    wait_timeout: Duration,
) -> Result<BridgeMessage> {
    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| AppError::Socket(format!("Failed to connect: {}", e)))?;

    let (reader, mut writer) = stream.into_split();

    // Send the message
    let json = serde_json::to_string(message)?;
    writer
        .write_all(format!("{}\n", json).as_bytes())
        .await
        .map_err(|e| AppError::Socket(format!("Failed to write: {}", e)))?;

    // Wait for response matching our session ID
    let mut buf_reader = BufReader::new(reader);
    let session_id = &message.session_id;

    let result = timeout(wait_timeout, async {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes =
                crate::socket::read_bounded_line(&mut buf_reader, &mut line, MAX_LINE_BYTES)
                    .await
                    .map_err(|e| AppError::Socket(format!("Failed to read: {}", e)))?;

            if bytes == 0 {
                return Err(AppError::Socket("Connection closed".into()));
            }

            // FR31: Bound client read to MAX_LINE_BYTES
            if bytes > MAX_LINE_BYTES {
                return Err(AppError::Socket(format!(
                    "Response line too large ({bytes} bytes, max {MAX_LINE_BYTES})",
                )));
            }

            if let Ok(msg) = serde_json::from_str::<BridgeMessage>(line.trim()) {
                if msg.session_id == *session_id && msg.msg_type == MessageType::ApprovalResponse {
                    return Ok(msg);
                }
            }
        }
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(AppError::Socket("Approval timeout".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_requires_approval() {
        // Dangerous tools require approval
        assert!(tool_requires_approval("Write", &serde_json::Value::Null));
        assert!(tool_requires_approval("Edit", &serde_json::Value::Null));
        assert!(tool_requires_approval(
            "MultiEdit",
            &serde_json::Value::Null
        ));

        // Bash with safe commands auto-approves
        let safe_input = serde_json::json!({"command": "ls -la"});
        assert!(!tool_requires_approval("Bash", &safe_input));

        let safe_input2 = serde_json::json!({"command": "cat /etc/hosts"});
        assert!(!tool_requires_approval("Bash", &safe_input2));

        // Bash with dangerous commands requires approval
        let danger_input = serde_json::json!({"command": "rm -rf /"});
        assert!(tool_requires_approval("Bash", &danger_input));

        let danger_input2 = serde_json::json!({"command": "npm install"});
        assert!(tool_requires_approval("Bash", &danger_input2));

        // Other tools don't require approval
        assert!(!tool_requires_approval("Read", &serde_json::Value::Null));
        assert!(!tool_requires_approval("Grep", &serde_json::Value::Null));
        assert!(!tool_requires_approval("Glob", &serde_json::Value::Null));
    }

    #[test]
    fn test_safe_command_whitelist() {
        for cmd in SAFE_COMMANDS {
            let input = serde_json::json!({"command": format!("{} something", cmd)});
            assert!(
                !tool_requires_approval("Bash", &input),
                "Expected '{}' to be safe",
                cmd
            );
        }
    }
}
