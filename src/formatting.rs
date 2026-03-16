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
#[allow(dead_code)]
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

/// Format tool execution for Telegram (used by Details button)
#[allow(dead_code)]
pub fn format_tool_execution(
    tool: &str,
    input: Option<&str>,
    output: &str,
    verbose: bool,
) -> String {
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
pub fn format_session_start(
    session_id: &str,
    project_dir: Option<&str>,
    hostname: Option<&str>,
) -> String {
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
    let mut msg = format!("\u{1f44b} *Session Ended*\n\nSession ID: `{}`", session_id);
    if let Some(dur) = duration_ms {
        let minutes = dur / 60000;
        let seconds = (dur % 60000) / 1000;
        msg.push_str(&format!("\nDuration: {}m {}s", minutes, seconds));
    }
    msg
}

/// Format status message
#[allow(dead_code)]
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
    format!("\u{274c} *Error:*\n\n```\n{}\n```", strip_ansi(error))
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

/// Truncate text with ellipsis (UTF-8 safe)
fn truncate(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        text.to_string()
    } else {
        let boundary = text
            .char_indices()
            .nth(max_len.saturating_sub(3))
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        format!("{}...", &text[..boundary])
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
            let mut msg = format!("\u{1f4bb} *Bash*\n\n```bash\n{}\n```", truncate(cmd, 1500));
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
#[allow(dead_code)]
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
            // If single line exceeds max, split it (UTF-8 safe)
            if line.len() > max_length {
                let mut remaining = line;
                while remaining.len() > max_length {
                    // Find a char boundary at or before max_length
                    let boundary = remaining
                        .char_indices()
                        .take_while(|(i, _)| *i <= max_length)
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(remaining.len());
                    let (chunk, rest) = remaining.split_at(boundary);
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
#[allow(dead_code)]
pub fn truncate_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 3 {
        path.to_string()
    } else {
        format!(".../{}", parts[parts.len() - 2..].join("/"))
    }
}

/// Get just the filename from a path
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Summarize a tool action in natural, human-readable language.
/// Returns a one-liner suitable for Telegram notification.
pub fn summarize_tool_action(tool: &str, input: Option<&serde_json::Value>) -> String {
    let obj = input.and_then(|v| v.as_object());

    match tool {
        "Bash" => summarize_bash(obj),
        "Read" => {
            let file = obj
                .and_then(|o| o.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("a file");
            format!("Reading {}", basename(file))
        }
        "Write" => {
            let file = obj
                .and_then(|o| o.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("a file");
            format!("Creating {}", basename(file))
        }
        "Edit" => {
            let file = obj
                .and_then(|o| o.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("a file");
            format!("Editing {}", basename(file))
        }
        "MultiEdit" => {
            let file = obj
                .and_then(|o| o.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("a file");
            format!("Editing {} (multiple changes)", basename(file))
        }
        "Grep" => {
            let pattern = obj
                .and_then(|o| o.get("pattern"))
                .and_then(|v| v.as_str())
                .unwrap_or("a pattern");
            let short: String = pattern.chars().take(40).collect();
            format!("Searching for '{}'", short)
        }
        "Glob" => {
            let pattern = obj
                .and_then(|o| o.get("pattern"))
                .and_then(|v| v.as_str())
                .unwrap_or("files");
            format!("Finding files matching {}", pattern)
        }
        "Task" => {
            let desc = obj
                .and_then(|o| o.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("a subtask");
            format!("Delegating: {}", desc)
        }
        "WebSearch" => {
            let query = obj
                .and_then(|o| o.get("query"))
                .and_then(|v| v.as_str())
                .unwrap_or("the web");
            let short: String = query.chars().take(50).collect();
            format!("Searching web for '{}'", short)
        }
        "WebFetch" => {
            let url = obj
                .and_then(|o| o.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("a page");
            // Show just the domain
            let domain = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
                .and_then(|s| s.split('/').next())
                .unwrap_or(url);
            format!("Fetching content from {}", domain)
        }
        "NotebookEdit" => "Editing notebook".to_string(),
        "TodoWrite" | "TodoRead" => "Managing task list".to_string(),
        "AskUser" | "AskUserQuestion" => "Asking for input".to_string(),
        _ => format!("Using {}", tool),
    }
}

/// Summarize a Bash command into human-readable form
fn summarize_bash(obj: Option<&serde_json::Map<String, serde_json::Value>>) -> String {
    let cmd = match obj.and_then(|o| o.get("command")).and_then(|v| v.as_str()) {
        Some(c) => c.trim(),
        None => return "Running a command".to_string(),
    };

    // Extract the first word/program from the command
    let first_word = cmd.split_whitespace().next().unwrap_or("");

    match first_word {
        "cargo" => summarize_cargo(cmd),
        "git" => summarize_git(cmd),
        "npm" | "npx" | "yarn" | "pnpm" | "bun" => summarize_node(cmd),
        "pip" | "pip3" | "python" | "python3" | "pytest" => summarize_python(cmd),
        "rustc" | "rustup" => format!("Running {}", first_word),
        "docker" | "docker-compose" => summarize_docker(cmd),
        "make" => {
            let target = cmd.split_whitespace().nth(1).unwrap_or("default");
            format!("Running make {}", target)
        }
        "curl" => "Making HTTP request".to_string(),
        "wget" => "Downloading file".to_string(),
        "chmod" => "Changing file permissions".to_string(),
        "chown" => "Changing file ownership".to_string(),
        "mkdir" => "Creating directory".to_string(),
        "rm" => "Removing files".to_string(),
        "cp" => "Copying files".to_string(),
        "mv" => "Moving files".to_string(),
        "ln" => "Creating symlink".to_string(),
        "tar" => {
            if cmd.contains('x') {
                "Extracting archive".to_string()
            } else {
                "Creating archive".to_string()
            }
        }
        "ssh" => "Connecting via SSH".to_string(),
        "tmux" => "Managing tmux session".to_string(),
        "supervisorctl" => "Managing services".to_string(),
        "kill" | "killall" | "pkill" => "Stopping process".to_string(),
        "ps" => "Listing processes".to_string(),
        "ls" => "Listing directory".to_string(),
        _ => {
            // For chained commands, just describe the first meaningful one
            let short: String = cmd.chars().take(60).collect();
            let ellipsis = if cmd.chars().count() > 60 { "..." } else { "" };
            format!("Running `{}{}`", short, ellipsis)
        }
    }
}

fn summarize_cargo(cmd: &str) -> String {
    let subcmd = cmd.split_whitespace().nth(1).unwrap_or("");
    match subcmd {
        "build" => {
            if cmd.contains("--release") {
                "Building project (release)".to_string()
            } else {
                "Building project".to_string()
            }
        }
        "test" => {
            // Extract test name if specified
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            let test_name = parts.iter().skip(2).find(|p| !p.starts_with('-'));
            match test_name {
                Some(name) => format!("Running test: {}", name),
                None => "Running tests".to_string(),
            }
        }
        "clippy" => "Running linter (clippy)".to_string(),
        "fmt" => "Formatting code".to_string(),
        "check" => "Checking code".to_string(),
        "run" => "Running program".to_string(),
        "add" => {
            let dep = cmd.split_whitespace().nth(2).unwrap_or("dependency");
            format!("Adding dependency: {}", dep)
        }
        "install" => "Installing Rust package".to_string(),
        "clean" => "Cleaning build artifacts".to_string(),
        "doc" => "Generating documentation".to_string(),
        "publish" => "Publishing crate".to_string(),
        "bench" => "Running benchmarks".to_string(),
        _ => format!("Running cargo {}", subcmd),
    }
}

fn summarize_git(cmd: &str) -> String {
    let subcmd = cmd.split_whitespace().nth(1).unwrap_or("");
    match subcmd {
        "status" => "Checking git status".to_string(),
        "diff" => "Viewing changes".to_string(),
        "log" => "Viewing commit history".to_string(),
        "add" => "Staging changes".to_string(),
        "commit" => "Committing changes".to_string(),
        "push" => "Pushing to remote".to_string(),
        "pull" => "Pulling from remote".to_string(),
        "fetch" => "Fetching updates".to_string(),
        "checkout" | "switch" => {
            let branch = cmd.split_whitespace().nth(2).unwrap_or("branch");
            format!("Switching to {}", branch)
        }
        "branch" => "Managing branches".to_string(),
        "merge" => "Merging branches".to_string(),
        "rebase" => "Rebasing commits".to_string(),
        "stash" => "Stashing changes".to_string(),
        "clone" => "Cloning repository".to_string(),
        "init" => "Initializing repository".to_string(),
        "tag" => "Managing tags".to_string(),
        "remote" => "Managing remotes".to_string(),
        _ => format!("Running git {}", subcmd),
    }
}

fn summarize_node(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let manager = parts.first().unwrap_or(&"npm");
    let subcmd = parts.get(1).unwrap_or(&"");
    match *subcmd {
        "install" | "i" | "add" => "Installing dependencies".to_string(),
        "test" | "t" => "Running tests".to_string(),
        "run" => {
            let script = parts.get(2).unwrap_or(&"script");
            format!("Running {} {}", manager, script)
        }
        "build" => "Building project".to_string(),
        "start" => "Starting application".to_string(),
        "lint" => "Running linter".to_string(),
        "publish" => "Publishing package".to_string(),
        _ => {
            if *manager == "npx" {
                let pkg = parts.get(1).unwrap_or(&"package");
                format!("Running {}", pkg)
            } else {
                format!("Running {} {}", manager, subcmd)
            }
        }
    }
}

fn summarize_python(cmd: &str) -> String {
    let first = cmd.split_whitespace().next().unwrap_or("");
    match first {
        "pytest" => "Running Python tests".to_string(),
        "pip" | "pip3" => {
            if cmd.contains("install") {
                "Installing Python packages".to_string()
            } else {
                "Managing Python packages".to_string()
            }
        }
        _ => "Running Python script".to_string(),
    }
}

fn summarize_docker(cmd: &str) -> String {
    let subcmd = cmd.split_whitespace().nth(1).unwrap_or("");
    match subcmd {
        "build" => "Building Docker image".to_string(),
        "run" => "Running Docker container".to_string(),
        "compose" | "-compose" => {
            let action = cmd.split_whitespace().nth(2).unwrap_or("up");
            format!("Docker compose {}", action)
        }
        "push" => "Pushing Docker image".to_string(),
        "pull" => "Pulling Docker image".to_string(),
        "exec" => "Running command in container".to_string(),
        "stop" => "Stopping container".to_string(),
        "ps" => "Listing containers".to_string(),
        _ => format!("Running docker {}", subcmd),
    }
}

/// Summarize a tool result in human-readable form.
/// Returns a brief one-liner for the result notification.
pub fn summarize_tool_result(tool: &str, output: &str) -> String {
    let cleaned = strip_ansi(output);
    let line_count = cleaned.lines().count();

    // Check for obvious error patterns
    let is_error = cleaned.contains("error[E")
        || cleaned.contains("Error:")
        || cleaned.contains("FAILED")
        || cleaned.contains("panic!")
        || cleaned.starts_with("error");

    if is_error {
        // Extract first error line
        let first_err = cleaned
            .lines()
            .find(|l| l.contains("error") || l.contains("Error") || l.contains("FAILED"))
            .unwrap_or("See details");
        let short: String = first_err.chars().take(80).collect();
        return format!("Failed: {}", short);
    }

    match tool {
        "Bash" => {
            if cleaned.is_empty() {
                "Completed (no output)".to_string()
            } else if line_count == 1 {
                let short: String = cleaned.trim().chars().take(80).collect();
                format!("Result: {}", short)
            } else {
                format!("Completed ({} lines of output)", line_count)
            }
        }
        "Read" => format!("Read {} lines", line_count),
        "Write" => "File written".to_string(),
        "Edit" | "MultiEdit" => "Changes applied".to_string(),
        "Grep" => {
            if cleaned.is_empty() {
                "No matches found".to_string()
            } else {
                let matches = cleaned.lines().count();
                format!(
                    "Found {} match{}",
                    matches,
                    if matches == 1 { "" } else { "es" }
                )
            }
        }
        "Glob" => {
            let files = cleaned.lines().count();
            format!("Found {} file{}", files, if files == 1 { "" } else { "s" })
        }
        "Task" => "Subtask completed".to_string(),
        "WebSearch" => "Search results received".to_string(),
        "WebFetch" => "Content fetched".to_string(),
        _ => {
            if cleaned.is_empty() {
                "Completed".to_string()
            } else {
                format!("Completed ({} lines)", line_count)
            }
        }
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

    #[test]
    fn test_summarize_bash_cargo() {
        let input = serde_json::json!({"command": "cargo test"});
        assert_eq!(summarize_tool_action("Bash", Some(&input)), "Running tests");

        let input = serde_json::json!({"command": "cargo build --release"});
        assert_eq!(
            summarize_tool_action("Bash", Some(&input)),
            "Building project (release)"
        );

        let input = serde_json::json!({"command": "cargo clippy"});
        assert_eq!(
            summarize_tool_action("Bash", Some(&input)),
            "Running linter (clippy)"
        );

        let input = serde_json::json!({"command": "cargo fmt"});
        assert_eq!(
            summarize_tool_action("Bash", Some(&input)),
            "Formatting code"
        );
    }

    #[test]
    fn test_summarize_bash_git() {
        let input = serde_json::json!({"command": "git status"});
        assert_eq!(
            summarize_tool_action("Bash", Some(&input)),
            "Checking git status"
        );

        let input = serde_json::json!({"command": "git push"});
        assert_eq!(
            summarize_tool_action("Bash", Some(&input)),
            "Pushing to remote"
        );

        let input = serde_json::json!({"command": "git diff"});
        assert_eq!(
            summarize_tool_action("Bash", Some(&input)),
            "Viewing changes"
        );
    }

    #[test]
    fn test_summarize_file_ops() {
        let input = serde_json::json!({"file_path": "/home/user/project/src/main.rs"});
        assert_eq!(
            summarize_tool_action("Read", Some(&input)),
            "Reading main.rs"
        );
        assert_eq!(
            summarize_tool_action("Edit", Some(&input)),
            "Editing main.rs"
        );
        assert_eq!(
            summarize_tool_action("Write", Some(&input)),
            "Creating main.rs"
        );
    }

    #[test]
    fn test_summarize_search() {
        let input = serde_json::json!({"pattern": "fn main"});
        assert_eq!(
            summarize_tool_action("Grep", Some(&input)),
            "Searching for 'fn main'"
        );

        let input = serde_json::json!({"pattern": "**/*.rs"});
        assert_eq!(
            summarize_tool_action("Glob", Some(&input)),
            "Finding files matching **/*.rs"
        );
    }

    #[test]
    fn test_summarize_task() {
        let input = serde_json::json!({"description": "Explore auth module"});
        assert_eq!(
            summarize_tool_action("Task", Some(&input)),
            "Delegating: Explore auth module"
        );
    }

    #[test]
    fn test_summarize_unknown_tool() {
        let input = serde_json::json!({"foo": "bar"});
        assert_eq!(
            summarize_tool_action("CustomTool", Some(&input)),
            "Using CustomTool"
        );
    }

    #[test]
    fn test_summarize_tool_result_success() {
        assert_eq!(summarize_tool_result("Bash", ""), "Completed (no output)");
        assert_eq!(summarize_tool_result("Bash", "ok"), "Result: ok");
        assert_eq!(
            summarize_tool_result("Bash", "line1\nline2\nline3"),
            "Completed (3 lines of output)"
        );
        assert_eq!(summarize_tool_result("Write", "ok"), "File written");
        assert_eq!(summarize_tool_result("Edit", "ok"), "Changes applied");
        assert_eq!(summarize_tool_result("Grep", ""), "No matches found");
        assert_eq!(summarize_tool_result("Grep", "a\nb"), "Found 2 matches");
    }

    #[test]
    fn test_summarize_tool_result_error() {
        assert!(
            summarize_tool_result("Bash", "error[E0433]: use of undeclared type")
                .starts_with("Failed:")
        );
        assert!(summarize_tool_result("Bash", "Error: file not found").starts_with("Failed:"));
    }
}
