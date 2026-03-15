use crate::error::Result;
use crate::types::{BridgeMessage, HookEvent, HookOutput, HookSpecificOutput, MessageType};
use chrono::Utc;
use std::io::{self, Read, Write};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

/// Process a hook event from stdin, send to bridge, return output for Claude Code
///
/// This is the `ctm hook` subcommand handler.
/// It reads JSON from stdin, forwards to the bridge daemon via unix socket,
/// and optionally returns hookSpecificOutput to Claude Code on stdout.
pub async fn process_hook(socket_path: &std::path::Path) -> Result<()> {
    // Read all of stdin
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    let input = input.trim();
    if input.is_empty() {
        return Ok(());
    }

    // Parse the hook event (Security fix #10: no unwrap/panic)
    let event: HookEvent = match serde_json::from_str(input) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[ctm-hook] Failed to parse hook event: {}", e);
            // Pass through original input for Claude Code
            print!("{}", input);
            return Ok(());
        }
    };

    // Check if bridge is running (socket exists)
    if !socket_path.exists() {
        // Bridge not running, pass through silently
        print!("{}", input);
        return Ok(());
    }

    // Connect to bridge and forward the event
    let result = forward_to_bridge(socket_path, &event).await;

    // For PreToolUse, we might need to return a permission decision
    // For now, we just forward and pass through
    // The approval flow is handled by the daemon (it sends inline keyboards to Telegram)

    // Pass through original input for Claude Code
    print!("{}", input);
    io::stdout().flush()?;

    if let Err(e) = result {
        eprintln!("[ctm-hook] Failed to forward to bridge: {}", e);
    }

    Ok(())
}

/// Forward a hook event to the bridge daemon via unix socket
async fn forward_to_bridge(socket_path: &std::path::Path, event: &HookEvent) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path).await?;

    let timestamp = Utc::now().to_rfc3339();
    let session_id = event.session_id().to_string();

    // Convert hook event to bridge message(s)
    let messages = event_to_bridge_messages(event, &session_id, &timestamp);

    for msg in &messages {
        let json = serde_json::to_string(msg)?;
        let line = format!("{}\n", json);
        stream.write_all(line.as_bytes()).await?;
    }

    stream.shutdown().await?;
    Ok(())
}

/// Convert a HookEvent to one or more BridgeMessages
fn event_to_bridge_messages(
    event: &HookEvent,
    session_id: &str,
    timestamp: &str,
) -> Vec<BridgeMessage> {
    match event {
        HookEvent::PreToolUse {
            tool_name,
            tool_input,
            ..
        } => {
            let input_json = serde_json::to_value(tool_input).unwrap_or_default();
            let mut metadata = serde_json::Map::new();
            metadata.insert("tool".to_string(), serde_json::Value::String(tool_name.clone()));
            metadata.insert("input".to_string(), input_json);

            // Add tmux info if available
            add_tmux_metadata(&mut metadata);

            vec![BridgeMessage {
                msg_type: MessageType::ToolStart,
                session_id: session_id.to_string(),
                timestamp: timestamp.to_string(),
                content: format!("Tool: {}", tool_name),
                metadata: Some(metadata),
            }]
        }

        HookEvent::PostToolUse {
            tool_name,
            tool_output,
            ..
        } => {
            let output = tool_output.as_deref().unwrap_or("");
            if output.len() < 10 {
                return vec![];
            }

            let mut metadata = serde_json::Map::new();
            metadata.insert("tool".to_string(), serde_json::Value::String(tool_name.clone()));
            add_tmux_metadata(&mut metadata);

            vec![BridgeMessage {
                msg_type: MessageType::ToolResult,
                session_id: session_id.to_string(),
                timestamp: timestamp.to_string(),
                content: output.chars().take(2000).collect(),
                metadata: Some(metadata),
            }]
        }

        HookEvent::Notification {
            message,
            level,
            notification_type,
            ..
        } => {
            // Skip idle_prompt notifications
            if notification_type.as_deref() == Some("idle_prompt") {
                return vec![];
            }

            let msg_type = if level.as_deref() == Some("error") {
                MessageType::Error
            } else {
                MessageType::AgentResponse
            };

            let mut metadata = serde_json::Map::new();
            if let Some(l) = level {
                metadata.insert("level".to_string(), serde_json::Value::String(l.clone()));
            }
            add_tmux_metadata(&mut metadata);

            vec![BridgeMessage {
                msg_type,
                session_id: session_id.to_string(),
                timestamp: timestamp.to_string(),
                content: message.clone(),
                metadata: Some(metadata),
            }]
        }

        HookEvent::UserPromptSubmit { prompt, .. } => {
            let mut metadata = serde_json::Map::new();
            metadata.insert(
                "source".to_string(),
                serde_json::Value::String("cli".to_string()),
            );
            add_tmux_metadata(&mut metadata);

            vec![BridgeMessage {
                msg_type: MessageType::UserInput,
                session_id: session_id.to_string(),
                timestamp: timestamp.to_string(),
                content: prompt.clone(),
                metadata: Some(metadata),
            }]
        }

        HookEvent::Stop {
            transcript_path: _,
            transcript_summary,
            ..
        } => {
            let mut messages = Vec::new();

            // Send transcript summary as agent response if available
            if let Some(summary) = transcript_summary {
                if !summary.is_empty() {
                    let mut metadata = serde_json::Map::new();
                    add_tmux_metadata(&mut metadata);

                    messages.push(BridgeMessage {
                        msg_type: MessageType::AgentResponse,
                        session_id: session_id.to_string(),
                        timestamp: timestamp.to_string(),
                        content: summary.clone(),
                        metadata: Some(metadata),
                    });
                }
            }

            // Send turn_complete (not session_end - Claude fires Stop after every turn)
            let mut metadata = serde_json::Map::new();
            add_tmux_metadata(&mut metadata);

            messages.push(BridgeMessage {
                msg_type: MessageType::TurnComplete,
                session_id: session_id.to_string(),
                timestamp: timestamp.to_string(),
                content: "Turn complete".to_string(),
                metadata: Some(metadata),
            });

            messages
        }

        HookEvent::PreCompact { trigger, .. } => {
            let mut metadata = serde_json::Map::new();
            if let Some(t) = trigger {
                metadata.insert("trigger".to_string(), serde_json::Value::String(t.clone()));
            }
            add_tmux_metadata(&mut metadata);

            vec![BridgeMessage {
                msg_type: MessageType::PreCompact,
                session_id: session_id.to_string(),
                timestamp: timestamp.to_string(),
                content: "Context compaction starting".to_string(),
                metadata: Some(metadata),
            }]
        }

        HookEvent::SubagentStop { .. } => {
            // Just log, no forwarding
            vec![]
        }
    }
}

/// Add current tmux info to metadata (for auto-refresh)
fn add_tmux_metadata(metadata: &mut serde_json::Map<String, serde_json::Value>) {
    if let Some(info) = crate::injector::InputInjector::detect_tmux_session() {
        metadata.insert(
            "tmuxTarget".to_string(),
            serde_json::Value::String(info.target),
        );
        metadata.insert(
            "tmuxSession".to_string(),
            serde_json::Value::String(info.session),
        );
        if let Some(socket) = info.socket {
            metadata.insert("tmuxSocket".to_string(), serde_json::Value::String(socket));
        }
    }

    // Add hostname
    let hostname = crate::injector::get_hostname();
    if !hostname.is_empty() {
        metadata.insert(
            "hostname".to_string(),
            serde_json::Value::String(hostname),
        );
    }
}
