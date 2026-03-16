//! Tool summarizer — maps tool names + inputs to human-readable one-liners.
//!
//! Ported from `summarize.ts` with all 30+ Bash command patterns.

use std::collections::HashSet;
use std::sync::LazyLock;

// ------------------------------------------------------------------ helpers

/// Last 2 path components with `.../` prefix.
pub fn short_path(file_path: &str) -> String {
    let parts: Vec<&str> = file_path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 2 {
        return file_path.to_string();
    }
    format!(".../{}", parts[parts.len() - 2..].join("/"))
}

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

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    format!("{}...", &s[..max_len])
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

// ===================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- shortPath ----

    #[test]
    fn short_path_long() {
        assert_eq!(
            short_path("/opt/project/src/utils/config.ts"),
            ".../utils/config.ts"
        );
    }

    #[test]
    fn short_path_short() {
        assert_eq!(short_path("/src/file.ts"), "/src/file.ts");
    }

    #[test]
    fn short_path_root_level() {
        assert_eq!(short_path("/file.ts"), "/file.ts");
    }

    #[test]
    fn short_path_three_components() {
        assert_eq!(short_path("/opt/project/file.ts"), ".../project/file.ts");
    }

    // ---- findMeaningfulCommand ----

    #[test]
    fn skip_cd_prefix() {
        assert_eq!(
            find_meaningful_command("cd /tmp && cargo test"),
            "cargo test"
        );
    }

    #[test]
    fn skip_export_prefix() {
        assert_eq!(
            find_meaningful_command("export FOO=bar && npm run build"),
            "npm run build"
        );
    }

    #[test]
    fn skip_multiple_trivial() {
        assert_eq!(
            find_meaningful_command("cd /tmp && export PATH=/usr && cargo build"),
            "cargo build"
        );
    }

    #[test]
    fn no_trivial_prefix() {
        assert_eq!(
            find_meaningful_command("cargo test --release"),
            "cargo test --release"
        );
    }

    #[test]
    fn split_on_semicolons() {
        assert_eq!(
            find_meaningful_command("echo hi; cargo build"),
            "cargo build"
        );
    }

    #[test]
    fn split_on_double_pipe_operator() {
        assert_eq!(find_meaningful_command("true || cargo test"), "cargo test");
    }

    #[test]
    fn no_split_on_single_pipe() {
        assert_eq!(
            find_meaningful_command("cargo test | grep FAIL"),
            "cargo test | grep FAIL"
        );
    }

    #[test]
    fn all_trivial_returns_last() {
        assert_eq!(find_meaningful_command("cd /tmp && echo done"), "echo done");
    }

    // ---- cargo commands ----

    #[test]
    fn cargo_build() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo build"})),
            "Building project"
        );
    }

    #[test]
    fn cargo_build_release() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo build --release"})),
            "Building project (release)"
        );
    }

    #[test]
    fn cargo_test() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo test"})),
            "Running tests"
        );
    }

    #[test]
    fn cargo_clippy() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo clippy"})),
            "Running linter"
        );
    }

    #[test]
    fn cargo_fmt() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo fmt"})),
            "Formatting code"
        );
    }

    #[test]
    fn cargo_run() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo run"})),
            "Running project"
        );
    }

    #[test]
    fn cargo_doc() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo doc"})),
            "Generating docs"
        );
    }

    #[test]
    fn cargo_bench() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo bench"})),
            "Running benchmarks"
        );
    }

    #[test]
    fn cargo_check() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo check"})),
            "Type checking"
        );
    }

    #[test]
    fn cargo_publish() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "cargo publish"})),
            "Publishing crate"
        );
    }

    // ---- git commands ----

    #[test]
    fn git_commit() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git commit -m \"fix\""})),
            "Committing changes"
        );
    }

    #[test]
    fn git_push() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git push origin main"})),
            "Pushing to remote"
        );
    }

    #[test]
    fn git_pull() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git pull"})),
            "Pulling from remote"
        );
    }

    #[test]
    fn git_checkout() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git checkout feature-branch"})),
            "Switching branch"
        );
    }

    #[test]
    fn git_switch() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git switch main"})),
            "Switching branch"
        );
    }

    #[test]
    fn git_status() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git status"})),
            "Checking status"
        );
    }

    #[test]
    fn git_diff() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git diff HEAD~1"})),
            "Viewing diff"
        );
    }

    #[test]
    fn git_log() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git log --oneline"})),
            "Viewing history"
        );
    }

    #[test]
    fn git_clone() {
        assert_eq!(
            summarize_tool_action(
                "Bash",
                &json!({"command": "git clone https://github.com/foo/bar"})
            ),
            "Cloning repository"
        );
    }

    #[test]
    fn git_stash() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git stash"})),
            "Stashing changes"
        );
    }

    #[test]
    fn git_fetch() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "git fetch origin"})),
            "Fetching remote"
        );
    }

    // ---- npm/yarn/pnpm/bun ----

    #[test]
    fn npm_install() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "npm install"})),
            "Installing dependencies"
        );
    }

    #[test]
    fn npm_ci() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "npm ci"})),
            "Installing dependencies"
        );
    }

    #[test]
    fn npm_run_build() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "npm run build"})),
            "Building project"
        );
    }

    #[test]
    fn npm_test() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "npm test"})),
            "Running tests"
        );
    }

    #[test]
    fn npm_run_test() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "npm run test"})),
            "Running tests"
        );
    }

    #[test]
    fn npm_run_lint() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "npm run lint"})),
            "Running linter"
        );
    }

    #[test]
    fn npm_publish() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "npm publish"})),
            "Publishing package"
        );
    }

    #[test]
    fn npx_with_package() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "npx vitest --run"})),
            "Running npx: vitest"
        );
    }

    #[test]
    fn yarn_install() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "yarn install"})),
            "Installing dependencies"
        );
    }

    #[test]
    fn yarn_bare() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "yarn"})),
            "Installing dependencies"
        );
    }

    #[test]
    fn pnpm_install() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "pnpm install"})),
            "Installing dependencies"
        );
    }

    #[test]
    fn bun_test() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "bun test"})),
            "Running tests"
        );
    }

    // ---- other bash commands ----

    #[test]
    fn pip_install() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "pip install requests"})),
            "Installing Python dependencies"
        );
    }

    #[test]
    fn pytest_direct() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "pytest tests/"})),
            "Running Python tests"
        );
    }

    #[test]
    fn python_m_pytest() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "python -m pytest"})),
            "Running Python tests"
        );
    }

    #[test]
    fn docker_build() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "docker build -t app ."})),
            "Building Docker image"
        );
    }

    #[test]
    fn docker_run() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "docker run -d app"})),
            "Running container"
        );
    }

    #[test]
    fn make_with_target() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "make build"})),
            "Running make: build"
        );
    }

    #[test]
    fn make_without_target() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "make"})),
            "Building with make"
        );
    }

    #[test]
    fn tsc_check() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "tsc --noEmit"})),
            "Type checking"
        );
    }

    #[test]
    fn vitest_run() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "vitest --run"})),
            "Running tests"
        );
    }

    #[test]
    fn eslint_check() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "eslint src/"})),
            "Running linter"
        );
    }

    #[test]
    fn curl_fetch() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "curl https://example.com"})),
            "Fetching URL"
        );
    }

    #[test]
    fn kubectl_manage() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "kubectl get pods"})),
            "Managing Kubernetes"
        );
    }

    #[test]
    fn terraform_manage() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "terraform apply"})),
            "Managing infrastructure"
        );
    }

    #[test]
    fn go_build() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "go build ./..."})),
            "Building Go project"
        );
    }

    #[test]
    fn go_test() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "go test ./..."})),
            "Running Go tests"
        );
    }

    #[test]
    fn mkdir_cmd() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "mkdir -p /tmp/test"})),
            "Creating directory"
        );
    }

    #[test]
    fn rm_cmd() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "rm -rf dist/"})),
            "Removing files"
        );
    }

    #[test]
    fn fallback_unknown_command() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "some-custom-tool --flag"})),
            "Running `some-custom-tool`"
        );
    }

    // ---- wrapper stripping ----

    #[test]
    fn strip_sudo() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "sudo npm install"})),
            "Installing dependencies"
        );
    }

    #[test]
    fn strip_nohup() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "nohup cargo build"})),
            "Building project"
        );
    }

    #[test]
    fn strip_env() {
        assert_eq!(
            summarize_tool_action("Bash", &json!({"command": "env cargo test"})),
            "Running tests"
        );
    }

    #[test]
    fn strip_chained_trivial_and_wrapper() {
        assert_eq!(
            summarize_tool_action(
                "Bash",
                &json!({"command": "cd /project && sudo npm install"})
            ),
            "Installing dependencies"
        );
    }

    // ---- file tools ----

    #[test]
    fn read_with_path() {
        assert_eq!(
            summarize_tool_action(
                "Read",
                &json!({"file_path": "/opt/project/src/utils/config.ts"})
            ),
            "Reading .../utils/config.ts"
        );
    }

    #[test]
    fn write_with_path() {
        assert_eq!(
            summarize_tool_action("Write", &json!({"file_path": "/opt/project/src/index.ts"})),
            "Writing .../src/index.ts"
        );
    }

    #[test]
    fn edit_with_path() {
        assert_eq!(
            summarize_tool_action("Edit", &json!({"file_path": "/home/user/project/main.rs"})),
            "Editing .../project/main.rs"
        );
    }

    #[test]
    fn multiedit_with_path() {
        assert_eq!(
            summarize_tool_action("MultiEdit", &json!({"file_path": "/opt/app/src/lib.ts"})),
            "Editing .../src/lib.ts"
        );
    }

    #[test]
    fn grep_with_pattern() {
        assert_eq!(
            summarize_tool_action("Grep", &json!({"pattern": "handleToolStart"})),
            "Searching for 'handleToolStart'"
        );
    }

    #[test]
    fn grep_truncates_long_pattern() {
        let long_pattern = "a".repeat(50);
        let result = summarize_tool_action("Grep", &json!({"pattern": long_pattern}));
        assert_eq!(result, format!("Searching for '{}...'", &"a".repeat(30)));
    }

    #[test]
    fn glob_with_pattern() {
        assert_eq!(
            summarize_tool_action("Glob", &json!({"pattern": "**/*.ts"})),
            "Finding files: **/*.ts"
        );
    }

    #[test]
    fn task_action() {
        assert_eq!(summarize_tool_action("Task", &json!({})), "Running task");
    }

    #[test]
    fn websearch_with_query() {
        assert_eq!(
            summarize_tool_action("WebSearch", &json!({"query": "vitest configuration guide"})),
            "Searching: vitest configuration guide"
        );
    }

    #[test]
    fn websearch_truncates_long_query() {
        let long_query = "a".repeat(60);
        let result = summarize_tool_action("WebSearch", &json!({"query": long_query}));
        assert_eq!(result, format!("Searching: {}...", &"a".repeat(40)));
    }

    #[test]
    fn webfetch_with_url() {
        assert_eq!(
            summarize_tool_action(
                "WebFetch",
                &json!({"url": "https://docs.example.com/api/v2/guide"})
            ),
            "Fetching docs.example.com"
        );
    }

    #[test]
    fn todowrite_action() {
        assert_eq!(
            summarize_tool_action("TodoWrite", &json!({})),
            "Updating tasks"
        );
    }

    #[test]
    fn todoread_action() {
        assert_eq!(
            summarize_tool_action("TodoRead", &json!({})),
            "Reading tasks"
        );
    }

    #[test]
    fn ask_user_question_action() {
        assert_eq!(
            summarize_tool_action("AskUserQuestion", &json!({})),
            "Asking user a question"
        );
    }

    #[test]
    fn notebook_edit_action() {
        assert_eq!(
            summarize_tool_action("NotebookEdit", &json!({})),
            "Editing notebook"
        );
    }

    #[test]
    fn fallback_unknown_tool() {
        assert_eq!(
            summarize_tool_action("SomeNewTool", &json!({})),
            "Using SomeNewTool"
        );
    }

    // ---- summarizeToolResult ----

    #[test]
    fn detect_rust_compiler_error() {
        let output = "error[E0308]: mismatched types\n --> src/main.rs:5:10";
        let result = summarize_tool_result("Bash", output);
        assert!(result.starts_with("Failed:"));
        assert!(result.contains("error[E0308]"));
    }

    #[test]
    fn detect_generic_error() {
        let output = "Some output\nError: file not found\nMore output";
        assert!(summarize_tool_result("Bash", output).starts_with("Failed:"));
    }

    #[test]
    fn detect_failed_pattern() {
        let output = "test result: FAILED. 2 passed; 1 failed";
        assert_eq!(summarize_tool_result("Bash", output), "Tests failed");
    }

    #[test]
    fn detect_panic() {
        let output = "thread 'main' panicked at 'index out of bounds'";
        assert!(summarize_tool_result("Bash", output).starts_with("Panicked:"));
    }

    #[test]
    fn detect_npm_error() {
        let output = "npm ERR! code ERESOLVE\nnpm ERR! Could not resolve dependency";
        assert_eq!(summarize_tool_result("Bash", output), "npm error");
    }

    #[test]
    fn normal_output_line_count() {
        let output = "line1\nline2\nline3";
        assert_eq!(
            summarize_tool_result("Bash", output),
            "Completed (3 lines of output)"
        );
    }

    #[test]
    fn empty_output() {
        assert_eq!(summarize_tool_result("Bash", ""), "Completed (no output)");
    }

    #[test]
    fn truncate_long_error_line() {
        let long_error = format!("Error: {}", "x".repeat(200));
        let result = summarize_tool_result("Bash", &long_error);
        // "Failed: " (8) + truncated to 60 + "..." (3) = 71
        assert!(result.len() <= 71);
    }
}
