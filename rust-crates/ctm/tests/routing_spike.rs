//! MANUAL SPIKE (ROUTING-001): cross-session mis-routing of Telegram->CLI input.
//!
//! Field bug: "I type in session-2's topic on Telegram and the text lands in
//! pane 0 (session-1) instead of pane 1 (session-2)."
//!
//! ── Spike #1 (root cause, ALREADY PROVEN against the pre-fix code) ──────────
//! The pre-fix `InputInjector` stored a mutable `tmux_target`. `handle_telegram_text`
//! called `set_target()` in one lock acquisition, DROPPED the lock, awaited, then
//! re-acquired a FRESH lock to `inject()`. Because each Telegram update is
//! `tokio::spawn`-ed, a concurrent handler for another session could `set_target()`
//! in the gap, so `inject()` read the wrong pane. The pre-fix spike forced exactly
//! that interleaving against real tmux panes and observed:
//!     pane0 (session-1) got: "MSG_FOR_SESSION_2"
//!     pane1 (session-2) got: ""
//! i.e. session-2's reply landed in pane 0 — the reported symptom. That racy
//! `set_target` API has since been REMOVED, so the buggy pattern can no longer be
//! written (the fix eliminates the primitive, not just a call site).
//!
//! ── Spike #2 (fix verification, THIS FILE) ─────────────────────────────────
//! With the stateless injector (`inject(target, socket, text)` — target passed per
//! call, no shared mutable state), run the SAME adversarial interleaving that broke
//! the old code and assert each session's text reaches ITS OWN pane.
//!
//! Uses REAL tmux panes (each runs `cat >> <file>`, so injected keystrokes are
//! observable as file contents). Run with:
//!     cargo test -p ctm --test routing_spike -- --ignored --nocapture

use ctm::injector::InputInjector;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

const SOCK: &str = "/tmp/ctm-routing-spike.sock";
const SESS: &str = "ctmspike";
const PANE0_OUT: &str = "/tmp/ctm-spike-pane0.txt";
const PANE1_OUT: &str = "/tmp/ctm-spike-pane1.txt";

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-S", SOCK])
        .args(args)
        .output()
        .expect("tmux invocation failed")
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build a 2-pane tmux session. pane 0 ("ctmspike:0.0") = "session-1",
/// pane 1 ("ctmspike:0.1") = "session-2". Each pane appends stdin to its file.
fn setup_panes() {
    let _ = std::fs::remove_file(PANE0_OUT);
    let _ = std::fs::remove_file(PANE1_OUT);
    let _ = tmux(&["kill-session", "-t", SESS]);
    std::thread::sleep(Duration::from_millis(150));

    let out = tmux(&["new-session", "-d", "-s", SESS, "-x", "120", "-y", "40"]);
    assert!(
        out.status.success(),
        "new-session failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let c0 = format!("cat >> {PANE0_OUT}");
    tmux(&["send-keys", "-t", "ctmspike:0.0", "-l", &c0]);
    tmux(&["send-keys", "-t", "ctmspike:0.0", "Enter"]);

    let out = tmux(&["split-window", "-v", "-t", "ctmspike:0"]);
    assert!(
        out.status.success(),
        "split-window failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let c1 = format!("cat >> {PANE1_OUT}");
    tmux(&["send-keys", "-t", "ctmspike:0.1", "-l", &c1]);
    tmux(&["send-keys", "-t", "ctmspike:0.1", "Enter"]);

    std::thread::sleep(Duration::from_millis(300));
}

fn teardown() {
    let _ = tmux(&["kill-session", "-t", SESS]);
}

fn read_out(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Models the FIXED `handle_telegram_text` shape: the per-session target is a
/// LOCAL, resolved before injection; `inject()` takes it explicitly. The shared
/// `Arc<Mutex<InputInjector>>` only serializes tmux command execution and carries
/// no routing state, so a concurrent handler cannot clobber this one's target.
async fn fixed_resolve_then_inject(
    inj: Arc<Mutex<InputInjector>>,
    target: &str,
    text: &str,
    after_resolve: Arc<Notify>,
    before_inject: Arc<Notify>,
) {
    // "resolve" the per-session target into a local (no shared set_target).
    let my_target = target.to_string();

    after_resolve.notify_one();
    before_inject.notified().await; // let the other session's handler run in the gap

    let g = inj.lock().await;
    let _ = g.inject(&my_target, Some(SOCK), text);
}

/// Spike #2: under the exact interleaving that misrouted the pre-fix code,
/// each session's text now reaches its OWN pane.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires tmux; run explicitly with --ignored"]
async fn spike_fixed_stateless_injector_routes_per_session() {
    if !tmux_available() {
        eprintln!("tmux not available — skipping");
        return;
    }
    setup_panes();

    // ONE shared injector, exactly like the daemon.
    let inj = Arc::new(Mutex::new(InputInjector::new()));

    // session-2's handler: targets pane 1.
    let s2_after = Arc::new(Notify::new());
    let s2_before = Arc::new(Notify::new());
    let inj2 = Arc::clone(&inj);
    let a = Arc::clone(&s2_after);
    let b = Arc::clone(&s2_before);
    let t2 = tokio::spawn(async move {
        fixed_resolve_then_inject(inj2, "ctmspike:0.1", "MSG_FOR_SESSION_2", a, b).await;
    });

    // session-1's handler races in AFTER s2 resolved but BEFORE s2 injected —
    // the precise window that corrupted the shared target in the old design.
    let inj1 = Arc::clone(&inj);
    let a2 = Arc::clone(&s2_after);
    let b2 = Arc::clone(&s2_before);
    let t1 = tokio::spawn(async move {
        a2.notified().await;
        {
            let g = inj1.lock().await;
            let _ = g.inject("ctmspike:0.0", Some(SOCK), "MSG_FOR_SESSION_1");
        }
        b2.notify_one(); // release s2 to inject — it carries its OWN target now
    });

    t1.await.unwrap();
    t2.await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let pane0 = read_out(PANE0_OUT);
    let pane1 = read_out(PANE1_OUT);
    teardown();

    eprintln!("--- FIX SPIKE (ROUTING-001) ---");
    eprintln!("pane0 (session-1) got: {pane0:?}");
    eprintln!("pane1 (session-2) got: {pane1:?}");

    // Correct routing: each message reaches its own pane, none leaks to the other.
    assert!(
        pane1.contains("MSG_FOR_SESSION_2"),
        "session-2's text must reach pane 1. pane1={pane1:?}"
    );
    assert!(
        !pane1.contains("MSG_FOR_SESSION_1"),
        "session-1's text must NOT leak into pane 1. pane1={pane1:?}"
    );
    assert!(
        pane0.contains("MSG_FOR_SESSION_1"),
        "session-1's text must reach pane 0. pane0={pane0:?}"
    );
    assert!(
        !pane0.contains("MSG_FOR_SESSION_2"),
        "REGRESSION: session-2's text leaked into pane 0 (the original bug). pane0={pane0:?}"
    );
}

/// Stress variant: many concurrent injects to two distinct panes; assert no
/// cross-contamination in either direction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires tmux; run explicitly with --ignored"]
async fn spike_fixed_concurrent_stress_no_crosstalk() {
    if !tmux_available() {
        eprintln!("tmux not available — skipping");
        return;
    }
    setup_panes();
    let inj = Arc::new(Mutex::new(InputInjector::new()));

    let mut handles = Vec::new();
    for i in 0..20 {
        let inj = Arc::clone(&inj);
        handles.push(tokio::spawn(async move {
            // Even i -> pane 0 / session-1; odd i -> pane 1 / session-2.
            let (target, msg) = if i % 2 == 0 {
                ("ctmspike:0.0", format!("S1_{i}"))
            } else {
                ("ctmspike:0.1", format!("S2_{i}"))
            };
            // Yield to interleave aggressively.
            tokio::task::yield_now().await;
            let g = inj.lock().await;
            let _ = g.inject(target, Some(SOCK), &msg);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let pane0 = read_out(PANE0_OUT);
    let pane1 = read_out(PANE1_OUT);
    teardown();

    eprintln!("--- STRESS SPIKE (ROUTING-001) ---");
    eprintln!("pane0: {pane0:?}");
    eprintln!("pane1: {pane1:?}");

    // Every S1_* must be in pane0 and never in pane1, and vice-versa.
    for i in (0..20).step_by(2) {
        let m = format!("S1_{i}");
        assert!(pane0.contains(&m), "{m} missing from pane0");
        assert!(!pane1.contains(&m), "{m} leaked into pane1");
    }
    for i in (1..20).step_by(2) {
        let m = format!("S2_{i}");
        assert!(pane1.contains(&m), "{m} missing from pane1");
        assert!(!pane0.contains(&m), "{m} leaked into pane0");
    }
}
