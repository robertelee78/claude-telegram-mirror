use crate::config;
use crate::error::{AppError, Result};
use crate::injector::{self, InputInjector};
use crate::types::{self, BridgeMessage, HookEvent, MAX_LINE_BYTES, SAFE_COMMANDS};
use std::io::Read;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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

    // Get session ID and validate
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
    };
    base.transcript_path.as_deref()
}

/// Build metadata object with tmux info and hostname
fn build_metadata(
    tmux_info: &Option<injector::TmuxInfo>,
    hostname: &str,
    transcript_path: Option<&str>,
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
    meta
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn make_message(
    msg_type: &str,
    session_id: &str,
    content: &str,
    metadata: serde_json::Map<String, serde_json::Value>,
) -> BridgeMessage {
    BridgeMessage {
        msg_type: msg_type.to_string(),
        session_id: session_id.to_string(),
        timestamp: now_iso(),
        content: content.to_string(),
        metadata: Some(metadata),
    }
}

/// Build bridge messages for the given hook event
async fn build_messages(
    event: &HookEvent,
    session_id: &str,
    tmux_info: &Option<injector::TmuxInfo>,
    hostname: &str,
    cfg: &config::Config,
) -> Vec<BridgeMessage> {
    let transcript_path = get_transcript_path(event);
    let meta = build_metadata(tmux_info, hostname, transcript_path);
    let mut messages = Vec::new();

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
                "tool_start",
                session_id,
                &e.tool_name,
                tool_meta,
            ));
        }
        HookEvent::PostToolUse(e) => {
            let output = e.tool_output.as_deref().unwrap_or("");
            if cfg.verbose || output.len() > 10 {
                let truncated = if output.len() > 2000 {
                    format!("{}...", &output[..2000])
                } else {
                    output.to_string()
                };
                let mut tool_meta = meta.clone();
                tool_meta.insert(
                    "tool".into(),
                    serde_json::Value::String(e.tool_name.clone()),
                );
                messages.push(make_message(
                    "tool_result",
                    session_id,
                    &truncated,
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
                "error"
            } else {
                "agent_response"
            };
            messages.push(make_message(msg_type, session_id, &e.message, meta));
        }
        HookEvent::UserPromptSubmit(e) => {
            let mut prompt_meta = meta.clone();
            prompt_meta.insert("source".into(), serde_json::Value::String("cli".into()));
            messages.push(make_message(
                "user_input",
                session_id,
                &e.prompt,
                prompt_meta,
            ));
        }
        HookEvent::Stop(_e) => {
            // Extract new assistant text from transcript
            if let Some(path) = transcript_path {
                if let Some(text) = extract_transcript_text(path, session_id, &cfg.config_dir) {
                    if !text.is_empty() {
                        messages.push(make_message(
                            "agent_response",
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
                        "session_rename",
                        session_id,
                        &title,
                        meta.clone(),
                    ));
                }
            }

            // Send turn_complete
            messages.push(make_message("turn_complete", session_id, "", meta));
        }
        HookEvent::SubagentStop(_) => {
            // Recognized but no message sent
        }
        HookEvent::PreCompact(_) => {
            messages.push(make_message("pre_compact", session_id, "", meta));
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
    let meta = build_metadata(&tmux_info, &hostname, get_transcript_path(event));

    let mut approval_meta = meta;
    approval_meta.insert(
        "tool".into(),
        serde_json::Value::String(pre_tool.tool_name.clone()),
    );
    approval_meta.insert("input".into(), pre_tool.tool_input.clone());

    let prompt = format!("Allow {} tool?", pre_tool.tool_name);
    let msg = make_message("approval_request", session_id, &prompt, approval_meta);

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
        Err(_) => {
            // Timeout or connection error — fall back to CLI
            Some(
                "{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"ask\",\"permissionDecisionReason\":\"Telegram approval timed out. Falling back to CLI approval.\"}}"
                    .to_string(),
            )
        }
    }
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

/// Extract new assistant text from the transcript JSONL file
fn extract_transcript_text(
    transcript_path: &str,
    session_id: &str,
    config_dir: &std::path::Path,
) -> Option<String> {
    use std::fs;
    use std::io::BufRead;
    use std::path::Path;

    let path = Path::new(transcript_path);
    if !path.exists() {
        return None;
    }

    // State file tracks last processed line
    let state_file = config_dir.join(format!(".last_line_{}", session_id));
    let last_line: usize = fs::read_to_string(&state_file)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    let file = fs::File::open(path).ok()?;
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

    let mut file = fs::File::open(transcript_path).ok()?;
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
            let bytes = buf_reader
                .read_line(&mut line)
                .await
                .map_err(|e| AppError::Socket(format!("Failed to read: {}", e)))?;

            if bytes == 0 {
                return Err(AppError::Socket("Connection closed".into()));
            }

            if let Ok(msg) = serde_json::from_str::<BridgeMessage>(line.trim()) {
                if msg.session_id == *session_id && msg.msg_type == "approval_response" {
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
