use regex::Regex;
use std::sync::LazyLock;

static ANSI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").expect("valid regex"));

/// Strip ANSI escape codes from text
pub fn strip_ansi(text: &str) -> String {
    ANSI_RE.replace_all(text, "").to_string()
}

/// Escape special characters for Telegram Markdown (not V2)
/// Keeps code blocks intact
pub fn escape_markdown(text: &str) -> String {
    // For basic Markdown mode, we only need minimal escaping
    // Code blocks are already handled by the formatting functions
    text.to_string()
}

/// Format agent response for Telegram
pub fn format_agent_response(content: &str) -> String {
    let cleaned = strip_ansi(content);
    format!("\u{1f916} *Claude:*\n\n{}", cleaned)
}

/// Format tool execution for Telegram
pub fn format_tool_execution(tool: &str, input: Option<&str>, output: &str, verbose: bool) -> String {
    let mut msg = format!("\u{1f527} *Tool: {}*\n", tool);

    if verbose {
        if let Some(inp) = input {
            let truncated = truncate(inp, 500);
            msg.push_str(&format!("\n\u{1f4e5} Input:\n```\n{}\n```\n", truncated));
        }
    }

    if !output.is_empty() {
        let truncated = truncate(&strip_ansi(output), 1000);
        msg.push_str(&format!("\n\u{1f4e4} Output:\n```\n{}\n```", truncated));
    }

    msg
}

/// Format approval request for Telegram
pub fn format_approval_request(prompt: &str) -> String {
    format!(
        "\u{26a0}\u{fe0f} *Approval Required*\n\n{}\n\nPlease respond:",
        strip_ansi(prompt)
    )
}

/// Format session start notification
pub fn format_session_start(session_id: &str, project_dir: Option<&str>, hostname: Option<&str>) -> String {
    let mut msg = format!(
        "\u{1f680} *Session Started*\n\nSession ID: `{}`",
        session_id
    );
    if let Some(host) = hostname {
        msg.push_str(&format!("\nHost: `{}`", host));
    }
    if let Some(dir) = project_dir {
        msg.push_str(&format!("\nProject: `{}`", dir));
    }
    msg
}

/// Format session end notification
pub fn format_session_end(session_id: &str, duration_ms: Option<u64>) -> String {
    let mut msg = format!(
        "\u{1f44b} *Session Ended*\n\nSession ID: `{}`",
        session_id
    );
    if let Some(dur) = duration_ms {
        let minutes = dur / 60000;
        let seconds = (dur % 60000) / 1000;
        msg.push_str(&format!("\nDuration: {}m {}s", minutes, seconds));
    }
    msg
}

/// Format status message
pub fn format_status(is_active: bool, session_id: Option<&str>, muted: bool) -> String {
    if !is_active {
        return "\u{1f4ca} *Status*\n\nNo active session attached.".to_string();
    }
    let mut msg = "\u{1f4ca} *Status*\n\n".to_string();
    if let Some(sid) = session_id {
        msg.push_str(&format!("Session: `{}`\n", sid));
    }
    msg.push_str(if muted {
        "Notifications: \u{1f507} Muted"
    } else {
        "Notifications: \u{1f514} Active"
    });
    msg
}

/// Format error message for Telegram
pub fn format_error(error: &str) -> String {
    format!(
        "\u{274c} *Error:*\n\n```\n{}\n```",
        strip_ansi(error)
    )
}

/// Format help message
pub fn format_help() -> String {
    "\u{1f4da} *Claude Code Mirror - Commands*\n\n\
     /status - Show current session status\n\
     /sessions - List active sessions\n\
     /attach <id> - Attach to a session\n\
     /detach - Detach from current session\n\
     /mute - Mute notifications\n\
     /unmute - Resume notifications\n\
     /abort - Abort current session\n\
     /help - Show this message\n\n\
     *Inline Responses:*\n\
     Simply reply with text to send input to the attached session.\n\n\
     *Approval Buttons:*\n\
     When Claude requests permission, tap:\n\
     \u{2705} Approve - Allow the action\n\
     \u{274c} Reject - Deny the action\n\
     \u{1f6d1} Abort - End the session"
        .to_string()
}

/// Truncate text with ellipsis
fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        format!("{}...", &text[..max_len.saturating_sub(3)])
    }
}

/// Get short filename from path
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    // Absolute paths start with / so split gives ["", "a", "b"] for "/a/b"
    if parts.len() <= 3 {
        path.to_string()
    } else {
        format!(".../{}", parts[parts.len() - 2..].join("/"))
    }
}

/// Format tool details for mobile-friendly Telegram display
pub fn format_tool_details(tool: &str, input: &serde_json::Value) -> String {
    let data = input.as_object();

    match tool {
        "Edit" => {
            let file = data
                .and_then(|d| d.get("file_path"))
                .and_then(|v| v.as_str())
                .map(short_path)
                .unwrap_or_default();
            let old_str = data
                .and_then(|d| d.get("old_string"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_str = data
                .and_then(|d| d.get("new_string"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let mut msg = format!("\u{270f}\u{fe0f} *Edit*\n\u{1f4c4} `{}`\n\n", file);
            if !old_str.is_empty() {
                msg.push_str(&format!(
                    "\u{2796} *Remove:*\n```\n{}\n```\n\n",
                    truncate(old_str, 800)
                ));
            }
            if !new_str.is_empty() {
                msg.push_str(&format!(
                    "\u{2795} *Add:*\n```\n{}\n```",
                    truncate(new_str, 800)
                ));
            }
            msg
        }
        "Write" => {
            let file = data
                .and_then(|d| d.get("file_path"))
                .and_then(|v| v.as_str())
                .map(short_path)
                .unwrap_or_default();
            let content = data
                .and_then(|d| d.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let lines = content.lines().count();
            format!(
                "\u{1f4dd} *Write*\n\u{1f4c4} `{}`\n\u{1f4cf} {} lines\n\n```\n{}\n```",
                file,
                lines,
                truncate(content, 1500)
            )
        }
        "Read" => {
            let file = data
                .and_then(|d| d.get("file_path"))
                .and_then(|v| v.as_str())
                .map(short_path)
                .unwrap_or_default();
            let mut msg = format!("\u{1f441} *Read*\n\u{1f4c4} `{}`", file);
            if let Some(offset) = data.and_then(|d| d.get("offset")).and_then(|v| v.as_u64()) {
                msg.push_str(&format!("\n\u{1f4cd} Line {}", offset));
            }
            if let Some(limit) = data.and_then(|d| d.get("limit")).and_then(|v| v.as_u64()) {
                msg.push_str(&format!(" (+{} lines)", limit));
            }
            msg
        }
        "Bash" => {
            let cmd = data
                .and_then(|d| d.get("command"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mut msg = format!(
                "\u{1f4bb} *Bash*\n\n```bash\n{}\n```",
                truncate(cmd, 1500)
            );
            if let Some(timeout) = data.and_then(|d| d.get("timeout")).and_then(|v| v.as_u64()) {
                msg.push_str(&format!("\n\u{23f1} Timeout: {}ms", timeout));
            }
            msg
        }
        "Grep" => {
            let pattern = data
                .and_then(|d| d.get("pattern"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let path = data
                .and_then(|d| d.get("path"))
                .and_then(|v| v.as_str())
                .map(short_path)
                .unwrap_or_else(|| "cwd".to_string());
            let mut msg = format!(
                "\u{1f50d} *Grep*\n\u{1f3af} Pattern: `{}`\n\u{1f4c2} Path: `{}`",
                truncate(pattern, 100),
                path
            );
            if let Some(glob) = data.and_then(|d| d.get("glob")).and_then(|v| v.as_str()) {
                msg.push_str(&format!("\n\u{1f4cb} Glob: `{}`", glob));
            }
            msg
        }
        "Task" => {
            let desc = data
                .and_then(|d| d.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let prompt = data
                .and_then(|d| d.get("prompt"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mut msg = format!("\u{1f916} *Task*\n\u{1f4cb} {}", desc);
            if !prompt.is_empty() {
                msg.push_str(&format!("\n\n```\n{}\n```", truncate(prompt, 1000)));
            }
            msg
        }
        _ => {
            let json_str = serde_json::to_string_pretty(input).unwrap_or_default();
            format!(
                "\u{1f527} *{}*\n\n```json\n{}\n```",
                tool,
                truncate(&json_str, 2000)
            )
        }
    }
}

/// Split a message into chunks that fit Telegram's 4096 char limit
pub fn chunk_message(text: &str, max_length: usize) -> Vec<String> {
    if text.len() <= max_length {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        if current.len() + line.len() + 1 > max_length {
            if !current.is_empty() {
                chunks.push(current);
                current = String::new();
            }
            // If single line exceeds max, split it
            if line.len() > max_length {
                let mut remaining = line;
                while remaining.len() > max_length {
                    let (chunk, rest) = remaining.split_at(max_length);
                    chunks.push(chunk.to_string());
                    remaining = rest;
                }
                if !remaining.is_empty() {
                    current = remaining.to_string();
                }
            } else {
                current = line.to_string();
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

/// Truncate file path to show basename and parent
pub fn truncate_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 3 {
        path.to_string()
    } else {
        format!(".../{}", parts[parts.len() - 2..].join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("no ansi"), "no ansi");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a long string", 10), "this is...");
    }

    #[test]
    fn test_short_path() {
        assert_eq!(short_path("/a/b"), "/a/b");
        assert_eq!(short_path("/a/b/c/d/e.rs"), ".../d/e.rs");
    }

    #[test]
    fn test_chunk_message() {
        let text = "line1\nline2\nline3";
        let chunks = chunk_message(text, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], text);

        let chunks = chunk_message(text, 10);
        assert!(chunks.len() > 1);
    }
}
