//! Tool summarizer — maps tool names + inputs to human-readable one-liners.
//!
//! Ported from `summarize.ts` with all 30+ Bash command patterns.

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::formatting::{short_path, truncate};

// ------------------------------------------------------------------ helpers

/// Extract hostname from a URL, falling back to first 40 chars.
fn short_url(url: &str) -> String {
    // Minimal URL parse — look for "://" and take the host part.
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        if let Some(host) = rest.split('/').next() {
            return host.to_string();
        }
    }
    truncate(url, 40)
}

// -------------------------------------------------------- trivial / wrapper

static TRIVIAL_COMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "cd", "export", "echo", "sleep", "set", "source", "true", "false", ":",
    ]
    .into_iter()
    .collect()
});

static WRAPPER_COMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    ["sudo", "nohup", "timeout", "env", "nice"]
        .into_iter()
        .collect()
});

/// For chained commands (`&&`, `;`, `||`), skip trivial prefixes.
/// Pipes are NOT split — they are part of the same command.
pub fn find_meaningful_command(command: &str) -> String {
    // Split on &&, ;, || but NOT single |
    let segments: Vec<&str> = command
        .split("&&")
        .flat_map(|s| s.split(';'))
        .flat_map(|s| split_on_double_pipe(s))
        .collect();

    for segment in &segments {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        if !TRIVIAL_COMMANDS.contains(first_word) {
            return trimmed.to_string();
        }
    }

    // All trivial — return last segment.
    segments
        .last()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| command.trim().to_string())
}

/// Split on `||` only, not single `|`.
fn split_on_double_pipe(s: &str) -> Vec<&str> {
    let mut results = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'|' && bytes[i + 1] == b'|' {
            results.push(&s[start..i]);
            start = i + 2;
            i += 2;
        } else {
            i += 1;
        }
    }
    results.push(&s[start..]);
    results
}

/// Strip wrapper commands (sudo, nohup, etc.) and return the inner command.
fn strip_wrappers(command: &str) -> String {
    let cmd = command.trim();
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let mut i = 0;
    while i < parts.len() && WRAPPER_COMMANDS.contains(parts[i]) {
        let prev = parts[i];
        i += 1;
        // `timeout` has a duration arg: `timeout 30 cmd`
        if prev == "timeout" && i < parts.len() {
            // Check if next looks like a duration (digits with optional suffix)
            let next = parts[i];
            if next.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
        }
    }
    if i >= parts.len() {
        return command.trim().to_string();
    }
    parts[i..].join(" ")
}

// -------------------------------------------------------- bash summarizer

fn summarize_bash_command(raw_command: &str) -> String {
    let meaningful = find_meaningful_command(raw_command);
    let command = strip_wrappers(&meaningful);
    let parts: Vec<&str> = command.split_whitespace().collect();
    let first = parts.first().copied().unwrap_or("");

    match first {
        "cargo" => {
            let sub = parts.get(1).copied().unwrap_or("");
            match sub {
                "build" => {
                    if parts.contains(&"--release") {
                        "Building project (release)".to_string()
                    } else {
                        "Building project".to_string()
                    }
                }
                "test" => "Running tests".to_string(),
                "clippy" => "Running linter".to_string(),
                "fmt" => "Formatting code".to_string(),
                "run" => "Running project".to_string(),
                "add" => "Adding dependency".to_string(),
                "install" => "Installing tool".to_string(),
                "clean" => "Cleaning build".to_string(),
                "doc" => "Generating docs".to_string(),
                "publish" => "Publishing crate".to_string(),
                "bench" => "Running benchmarks".to_string(),
                "check" => "Type checking".to_string(),
                _ => format!("Running `cargo {sub}`"),
            }
        }

        "git" => {
            let sub = parts.get(1).copied().unwrap_or("");
            match sub {
                "clone" => "Cloning repository".to_string(),
                "commit" => "Committing changes".to_string(),
                "push" => "Pushing to remote".to_string(),
                "pull" => "Pulling from remote".to_string(),
                "checkout" | "switch" => "Switching branch".to_string(),
                "merge" => "Merging branches".to_string(),
                "rebase" => "Rebasing".to_string(),
                "stash" => "Stashing changes".to_string(),
                "diff" => "Viewing diff".to_string(),
                "log" => "Viewing history".to_string(),
                "status" => "Checking status".to_string(),
                "branch" => "Managing branches".to_string(),
                "tag" => "Managing tags".to_string(),
                "fetch" => "Fetching remote".to_string(),
                "reset" => "Resetting changes".to_string(),
                _ => format!("Running `git {sub}`"),
            }
        }

        "npm" => {
            let sub = parts.get(1).copied().unwrap_or("");
            match sub {
                "install" | "ci" | "i" => "Installing dependencies".to_string(),
                "run" => {
                    let script = parts.get(2).copied().unwrap_or("unknown");
                    match script {
                        "build" => "Building project".to_string(),
                        "test" => "Running tests".to_string(),
                        "lint" => "Running linter".to_string(),
                        _ => format!("Running npm script: {script}"),
                    }
                }
                "test" | "t" => "Running tests".to_string(),
                "publish" => "Publishing package".to_string(),
                _ => format!("Running `npm {sub}`"),
            }
        }

        "npx" => {
            let pkg = parts.get(1).copied().unwrap_or("unknown");
            format!("Running npx: {pkg}")
        }

        "yarn" => {
            let sub = parts.get(1).copied();
            match sub {
                None | Some("install") => "Installing dependencies".to_string(),
                Some("build") => "Building project".to_string(),
                Some("test") => "Running tests".to_string(),
                Some("lint") => "Running linter".to_string(),
                Some("add") => "Installing dependencies".to_string(),
                Some("publish") => "Publishing package".to_string(),
                Some(s) => format!("Running `yarn {s}`"),
            }
        }

        "pnpm" => {
            let sub = parts.get(1).copied().unwrap_or("");
            match sub {
                "install" | "i" => "Installing dependencies".to_string(),
                "run" => {
                    let script = parts.get(2).copied().unwrap_or("unknown");
                    match script {
                        "build" => "Building project".to_string(),
                        "test" => "Running tests".to_string(),
                        "lint" => "Running linter".to_string(),
                        _ => format!("Running pnpm script: {script}"),
                    }
                }
                "test" => "Running tests".to_string(),
                "add" => "Installing dependencies".to_string(),
                "publish" => "Publishing package".to_string(),
                _ => format!("Running `pnpm {sub}`"),
            }
        }

        "bun" => {
            let sub = parts.get(1).copied().unwrap_or("");
            match sub {
                "install" | "i" => "Installing dependencies".to_string(),
                "run" => {
                    let script = parts.get(2).copied().unwrap_or("unknown");
                    match script {
                        "build" => "Building project".to_string(),
                        "test" => "Running tests".to_string(),
                        "lint" => "Running linter".to_string(),
                        _ => format!("Running bun script: {script}"),
                    }
                }
                "test" => "Running tests".to_string(),
                "add" => "Installing dependencies".to_string(),
                "publish" => "Publishing package".to_string(),
                _ => format!("Running `bun {sub}`"),
            }
        }

        "pip" | "pip3" => {
            if parts.get(1).copied() == Some("install") {
                "Installing Python dependencies".to_string()
            } else {
                format!(
                    "Running `{} {}`",
                    first,
                    parts.get(1).copied().unwrap_or("")
                )
            }
        }

        "pytest" => "Running Python tests".to_string(),

        "python" | "python3" => {
            if parts.get(1).copied() == Some("-m") && parts.get(2).copied() == Some("pytest") {
                "Running Python tests".to_string()
            } else {
                format!("Running `{first}`")
            }
        }

        "docker" => {
            let sub = parts.get(1).copied().unwrap_or("");
            // Handle "docker compose" (two-word modern syntax)
            if sub == "compose" {
                let compose_sub = parts.get(2).copied().unwrap_or("");
                return if compose_sub == "up" {
                    "Starting containers".to_string()
                } else {
                    format!("Running `docker compose {compose_sub}`")
                };
            }
            match sub {
                "build" => "Building Docker image".to_string(),
                "run" => "Running container".to_string(),
                _ => format!("Running `docker {sub}`"),
            }
        }

        "docker-compose" => {
            let sub = parts.get(1).copied().unwrap_or("");
            if sub == "up" {
                "Starting containers".to_string()
            } else {
                format!("Running `docker-compose {sub}`")
            }
        }

        "make" => {
            let target = parts.get(1).copied();
            match target {
                Some(t) => format!("Running make: {t}"),
                None => "Building with make".to_string(),
            }
        }

        "tsc" => "Type checking".to_string(),
        "vitest" => "Running tests".to_string(),
        "eslint" => "Running linter".to_string(),

        "curl" => "Fetching URL".to_string(),
        "wget" => "Downloading file".to_string(),
        "ssh" => "Connecting via SSH".to_string(),
        "scp" => "Copying files via SSH".to_string(),

        "tar" => "Archiving/extracting".to_string(),
        "chmod" => "Changing permissions".to_string(),
        "chown" => "Changing ownership".to_string(),
        "mkdir" => "Creating directory".to_string(),
        "rm" => "Removing files".to_string(),
        "cp" => "Copying files".to_string(),
        "mv" => "Moving files".to_string(),

        "grep" | "rg" => "Searching files".to_string(),
        "find" => "Finding files".to_string(),

        "kubectl" => "Managing Kubernetes".to_string(),
        "terraform" => "Managing infrastructure".to_string(),

        "go" => {
            let sub = parts.get(1).copied().unwrap_or("");
            match sub {
                "build" => "Building Go project".to_string(),
                "test" => "Running Go tests".to_string(),
                "run" => "Running Go project".to_string(),
                _ => format!("Running `go {sub}`"),
            }
        }

        "rustc" => "Compiling Rust".to_string(),

        _ => format!("Running `{first}`"),
    }
}

// -------------------------------------------------------- public API

/// Map tool name + input to a human-readable one-liner.
pub fn summarize_tool_action(tool: &str, input: &serde_json::Value) -> String {
    match tool {
        "Bash" => {
            let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if command.is_empty() {
                "Running command".to_string()
            } else {
                summarize_bash_command(command)
            }
        }
        "Read" => {
            let fp = input.get("file_path").and_then(|v| v.as_str());
            match fp {
                Some(p) => format!("Reading {}", short_path(p)),
                None => "Reading file".to_string(),
            }
        }
        "Write" => {
            let fp = input.get("file_path").and_then(|v| v.as_str());
            match fp {
                Some(p) => format!("Writing {}", short_path(p)),
                None => "Writing file".to_string(),
            }
        }
        "Edit" | "MultiEdit" => {
            let fp = input.get("file_path").and_then(|v| v.as_str());
            match fp {
                Some(p) => format!("Editing {}", short_path(p)),
                None => "Editing file".to_string(),
            }
        }
        "Grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str());
            match pattern {
                Some(p) => format!("Searching for '{}'", truncate(p, 30)),
                None => "Searching files".to_string(),
            }
        }
        "Glob" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str());
            match pattern {
                Some(p) => format!("Finding files: {p}"),
                None => "Finding files".to_string(),
            }
        }
        "Task" => "Running task".to_string(),
        "WebSearch" => {
            let query = input.get("query").and_then(|v| v.as_str());
            match query {
                Some(q) => format!("Searching: {}", truncate(q, 40)),
                None => "Searching the web".to_string(),
            }
        }
        "WebFetch" => {
            let url = input.get("url").and_then(|v| v.as_str());
            match url {
                Some(u) => format!("Fetching {}", short_url(u)),
                None => "Fetching URL".to_string(),
            }
        }
        "TodoWrite" => "Updating tasks".to_string(),
        "TodoRead" => "Reading tasks".to_string(),
        "AskUserQuestion" => "Asking user a question".to_string(),
        "NotebookEdit" => "Editing notebook".to_string(),
        _ => format!("Using {tool}"),
    }
}

/// Detect error patterns in tool output and return a summary.
pub fn summarize_tool_result(_tool: &str, output: &str) -> String {
    if output.is_empty() {
        return "Completed (no output)".to_string();
    }

    let lines: Vec<&str> = output.lines().collect();

    // Rust compiler error
    for line in &lines {
        if line.contains("error[E") {
            return format!("Failed: {}", truncate(line.trim(), 60));
        }
    }

    // Generic Error:
    for line in &lines {
        if regex_matches_error(line) {
            return format!("Failed: {}", truncate(line.trim(), 60));
        }
    }

    // FAILED pattern
    if output.contains("FAILED") {
        return "Tests failed".to_string();
    }

    // Panic
    for line in &lines {
        if line.contains("panic!") || line.contains("panicked at") {
            return format!("Panicked: {}", truncate(line.trim(), 60));
        }
    }

    // npm error
    if output.contains("npm ERR!") {
        return "npm error".to_string();
    }

    format!("Completed ({} lines of output)", lines.len())
}

fn regex_matches_error(line: &str) -> bool {
    // Case-insensitive match for word boundary + "Error:"
    let lower = line.to_lowercase();
    // Find "error:" with a word boundary before it
    if let Some(idx) = lower.find("error:") {
        if idx == 0 {
            return true;
        }
        let prev = lower.as_bytes()[idx - 1];
        // Word boundary: not alphanumeric
        return !prev.is_ascii_alphanumeric();
    }
    false
}
