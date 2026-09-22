//! ADR-023: no lock may be held across a Telegram call.
//!
//! The 2026-09-22 stall was one instance of a family: a shared thing held while
//! waiting on a slow, rate-limited, third-party service. The audit that found them
//! was a script run once; this is that script, kept, so the next one fails in CI
//! instead of on the user's phone.
//!
//! The rule it enforces: a guard bound by `let g = x.lock()/read()/write().await;`
//! must not still be in scope when a Telegram call is awaited. Compute under the
//! lock, drop it, then talk to the network.

use std::path::Path;

/// Calls that go to Telegram, directly or through the queue.
const TELEGRAM_CALLS: &[&str] = &[
    "send_message",
    "send_message_low",
    "send_with_buttons",
    "send_with_buttons_critical",
    "send_and_get_id",
    "api_call",
    "edit_message",
    "create_forum_topic",
];

fn guard_binding(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    let rest = t.strip_prefix("let ")?;
    let rest = rest.strip_prefix("mut ").unwrap_or(rest);
    let (name, tail) = rest.split_once(" = ")?;
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    for kind in [".lock().await;", ".read().await;", ".write().await;"] {
        if let Some(obj) = tail.strip_suffix(kind) {
            return Some((name.to_string(), obj.to_string()));
        }
    }
    None
}

fn scan(path: &Path, findings: &mut Vec<String>) {
    let text = std::fs::read_to_string(path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let Some((name, obj)) = guard_binding(line) else {
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        for (j, later) in lines.iter().enumerate().skip(i + 1) {
            if later.trim().is_empty() {
                continue;
            }
            // The guard's scope ends at an explicit drop or the first dedent.
            if later.contains(&format!("drop({name})")) {
                break;
            }
            if later.len() - later.trim_start().len() < indent {
                break;
            }
            let code = later.trim();
            if code.starts_with("//") {
                continue;
            }
            if let Some(call) = TELEGRAM_CALLS.iter().find(|c| code.contains(**c)) {
                findings.push(format!(
                    "{}:{} — `{name} = {obj}` is still held at line {} (`{}`), which calls Telegram",
                    path.file_name().unwrap().to_string_lossy(),
                    i + 1,
                    j + 1,
                    call
                ));
                break;
            }
        }
    }
}

#[test]
fn no_lock_is_held_across_a_telegram_call() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut findings = Vec::new();
    let mut stack = vec![src];
    let mut files = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                files += 1;
                scan(&p, &mut findings);
            }
        }
    }
    assert!(files > 20, "the scan found the source tree ({files} files)");
    assert!(
        findings.is_empty(),
        "a lock is held across a Telegram call — compute under the lock, drop it, then send:\n  {}",
        findings.join("\n  ")
    );
}

/// The scanner has to actually catch the shape it claims to catch.
#[test]
fn the_scanner_catches_the_shape_it_is_meant_to_catch() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.rs");
    std::fs::write(
        &bad,
        r#"
async fn handler(ctx: &Ctx) {
    if ready {
        let inj = ctx.injector.lock().await;
        let ok = inj.inject("x");
        ctx.bot.send_message("done", None, None).await;
    }
}
"#,
    )
    .unwrap();
    let mut findings = Vec::new();
    scan(&bad, &mut findings);
    assert_eq!(findings.len(), 1, "should flag the held lock: {findings:?}");
    assert!(findings[0].contains("send_message"));

    let good = dir.path().join("good.rs");
    std::fs::write(
        &good,
        r#"
async fn handler(ctx: &Ctx) {
    if ready {
        let ok = {
            let inj = ctx.injector.lock().await;
            inj.inject("x")
        };
        ctx.bot.send_message("done", None, None).await;
    }
}
"#,
    )
    .unwrap();
    let mut findings = Vec::new();
    scan(&good, &mut findings);
    assert!(
        findings.is_empty(),
        "the fixed shape must pass: {findings:?}"
    );
}
