//! Integration tests for the tool summarizer.
//!
//! Extracted from `src/summarize.rs` inline `#[cfg(test)]` module (Story 13.6).

use ctm::summarize::{find_meaningful_command, summarize_tool_action, summarize_tool_result};
use serde_json::json;

// short_path tests live in formatting_tests.rs — no duplication.

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
    assert_eq!(result, format!("Searching for '{}...'", &"a".repeat(27)));
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
    assert_eq!(result, format!("Searching: {}...", &"a".repeat(37)));
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
