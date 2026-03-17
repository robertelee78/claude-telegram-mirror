//! CLI smoke tests — verify the `ctm` binary responds correctly to
//! common invocations without requiring any runtime configuration.

use std::process::Command;

/// Locate the compiled `ctm` binary via Cargo's built-in env var.
fn ctm_binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ctm"))
}

#[test]
fn help_exits_zero_and_contains_name() {
    let output = ctm_binary()
        .arg("--help")
        .output()
        .expect("failed to execute ctm --help");

    assert!(
        output.status.success(),
        "ctm --help exited with non-zero status: {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Claude Telegram Mirror") || stdout.contains("ctm"),
        "stdout should mention 'Claude Telegram Mirror' or 'ctm', got: {}",
        stdout
    );
}

#[test]
fn version_exits_zero_and_contains_version() {
    let output = ctm_binary()
        .arg("--version")
        .output()
        .expect("failed to execute ctm --version");

    assert!(
        output.status.success(),
        "ctm --version exited with non-zero status: {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Verify output contains a semver version (e.g. "0.2.4"), not a specific value.
    let has_version = stdout
        .split_whitespace()
        .any(|w| w.split('.').count() == 3 && w.chars().all(|c| c.is_ascii_digit() || c == '.'));
    assert!(
        has_version,
        "stdout should contain a semver version, got: {}",
        stdout
    );
}

#[test]
fn invalid_command_exits_nonzero() {
    let output = ctm_binary()
        .arg("invalid-command")
        .output()
        .expect("failed to execute ctm invalid-command");

    assert!(
        !output.status.success(),
        "ctm invalid-command should exit with non-zero status"
    );
}

#[test]
fn daemon_help_exits_zero() {
    // "start" is the daemon start command; verify its --help works
    let output = ctm_binary()
        .args(["start", "--help"])
        .output()
        .expect("failed to execute ctm start --help");

    assert!(
        output.status.success(),
        "ctm start --help should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn setup_help_exits_zero() {
    let output = ctm_binary()
        .args(["setup", "--help"])
        .output()
        .expect("failed to execute ctm setup --help");

    assert!(
        output.status.success(),
        "ctm setup --help should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn doctor_help_exits_zero() {
    let output = ctm_binary()
        .args(["doctor", "--help"])
        .output()
        .expect("failed to execute ctm doctor --help");

    assert!(
        output.status.success(),
        "ctm doctor --help should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn toggle_help_exits_zero() {
    let output = ctm_binary()
        .args(["toggle", "--help"])
        .output()
        .expect("failed to execute ctm toggle --help");

    assert!(
        output.status.success(),
        "ctm toggle --help should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
