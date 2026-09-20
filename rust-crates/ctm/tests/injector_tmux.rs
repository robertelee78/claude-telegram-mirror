//! ADR-004: the tmux injection path, against a real tmux pane.
//!
//! Reported by the operator: a reply sent from Telegram arrived in Claude Code's
//! composer but was never submitted — they had to press Enter at the console. The cause
//! was that `inject` sent the text and fired Enter in the same breath, racing the TUI's
//! input handling; for a long message the Enter was swallowed.
//!
//! A shell in a pane is a faithful stand-in for what the injector must reason about:
//! typed text sits on the bottom line, Enter consumes it, and afterwards the text is
//! still on screen but has moved up into the scrollback. That is exactly the distinction
//! the fix depends on — "still in the composer" versus "visible in the transcript" — so
//! it can be tested without spending a model turn.

use ctm::injector::InputInjector;
use std::process::{Command, Stdio};

fn have(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A throwaway tmux server + session, killed on drop.
struct Pane {
    socket: String,
    session: String,
}

impl Pane {
    fn new(tag: &str) -> Option<Self> {
        if !have("tmux") {
            eprintln!("skip: tmux not installed");
            return None;
        }
        // ctm addresses tmux by socket PATH (`-S`), which is what a Claude Code hook
        // records, so the test server is created the same way.
        let socket = format!("/tmp/ctm-tmux-{tag}-{}.sock", std::process::id());
        let session = "t".to_string();
        let _ = std::fs::remove_file(&socket);
        let ok = Command::new("tmux")
            .args(["-S", &socket, "new-session", "-d", "-s", &session])
            .args(["-x", "100", "-y", "30", "sh -i"])
            .env("PS1", "ctm> ")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("skip: could not start tmux");
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(800));
        Some(Self { socket, session })
    }

    fn target(&self) -> String {
        format!("{}:0.0", self.session)
    }

    fn capture(&self) -> String {
        Command::new("tmux")
            .args([
                "-S",
                &self.socket,
                "capture-pane",
                "-t",
                &self.target(),
                "-p",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    }

    fn wait_for(&self, needle: &str, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if self.capture().contains(needle) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
        let _ = Command::new("tmux")
            .args(["-S", &self.socket, "kill-server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[test]
fn injected_text_is_actually_submitted_not_left_sitting_in_the_composer() {
    let Some(pane) = Pane::new("submit") else {
        return;
    };
    let marker = format!("CTM_SUBMITTED_{}", std::process::id());
    let injector = InputInjector::new();

    let ok = injector
        .inject(
            &pane.target(),
            Some(&pane.socket),
            &format!("echo {marker}"),
        )
        .expect("inject ran");
    assert!(ok, "inject reported success");

    // The pane executed it, which can only happen if Enter actually landed. The
    // regression this guards: the text arrives, the Enter does not, and ctm reports
    // success anyway.
    assert!(
        pane.wait_for(&marker, 5),
        "the injected line was submitted; pane was:\n{}",
        pane.capture()
    );
}

#[test]
fn a_long_message_still_submits() {
    // The operator's failing case was long: the Enter raced a large paste.
    let Some(pane) = Pane::new("long") else {
        return;
    };
    let marker = format!("CTM_LONG_{}", std::process::id());
    let filler = "word ".repeat(300);
    let injector = InputInjector::new();

    let ok = injector
        .inject(
            &pane.target(),
            Some(&pane.socket),
            &format!("echo {marker} # {filler}"),
        )
        .expect("inject ran");
    assert!(ok, "inject reported success for a long message");
    assert!(
        pane.wait_for(&marker, 8),
        "a long injected line was submitted; pane was:\n{}",
        pane.capture()
    );
}

#[test]
fn injection_into_a_dead_target_fails_rather_than_claiming_success() {
    let injector = InputInjector::new();
    let ok = injector
        .inject(
            "no-such-session:0.0",
            Some("/tmp/ctm-nonexistent-socket.sock"),
            "echo x",
        )
        .unwrap_or(false);
    assert!(!ok, "a missing pane must not be reported as delivered");
}

/// The same check against a REAL Claude Code pane, when one is supplied.
///
/// Run it by starting Claude Code in a tmux server and pointing the test at it:
///
/// ```sh
/// tmux -S /tmp/c.sock new-session -d -s c "cd /some/dir && exec claude"
/// CTM_TEST_TMUX_SOCKET=/tmp/c.sock CTM_TEST_TMUX_TARGET=c:0.0 \
///   cargo test --test injector_tmux against_a_real_claude -- --ignored
/// ```
///
/// It spends one small model turn, which is why it is `#[ignore]`. The shell-pane tests
/// above cover the mechanism; this one confirms the composer geometry of the actual TUI
/// (its composer sits above a status block, not on the last line).
#[test]
#[ignore = "needs a running Claude Code pane and spends a model turn"]
fn against_a_real_claude_code_pane() {
    let (Ok(socket), Ok(target)) = (
        std::env::var("CTM_TEST_TMUX_SOCKET"),
        std::env::var("CTM_TEST_TMUX_TARGET"),
    ) else {
        eprintln!("skip: set CTM_TEST_TMUX_SOCKET and CTM_TEST_TMUX_TARGET");
        return;
    };
    let marker = format!("CTMREAL{}", std::process::id());
    let injector = InputInjector::new();
    let ok = injector
        .inject(
            &target,
            Some(&socket),
            &format!("reply with exactly {marker} and nothing else"),
        )
        .expect("inject ran");
    assert!(ok, "inject reported the message submitted");

    // Submitted means the composer no longer holds it.
    let pane = Command::new("tmux")
        .args(["-S", &socket, "capture-pane", "-t", &target, "-p"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let bottom: String = pane
        .lines()
        .rev()
        .take(14)
        .collect::<Vec<_>>()
        .join("")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        !bottom.contains(&marker),
        "the composer emptied; bottom was:\n{bottom}"
    );
}
