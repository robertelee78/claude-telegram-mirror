//! ADR-022: the process half of `ctm codex-launch` — asking codex for its help
//! text, replacing ourselves with codex, and running codex in the app-server while
//! staying alive to report its exit.

use super::codex_launch::{self, Plan};
use crate::config::CodexHostConfig;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

const HELP_BUDGET: Duration = Duration::from_secs(5);

pub(super) fn fail(code: i32, message: &str) -> i32 {
    eprintln!("ctm: {message}");
    code
}

/// Replace this process with codex, exactly as typed. Returns only on failure.
pub(super) fn exec_verbatim(bin: &Path, args: &[String]) -> i32 {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(bin).args(args).exec();
    fail(126, &format!("cannot run {}: {err}", bin.display()))
}

/// Value-taking flags from codex's own help (both the TUI and `resume` forms), so a
/// flag added between releases is never mistaken for a session name or a prompt.
pub(super) async fn value_flags(bin: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut complete = true;
    for sub in [&["resume", "--help"][..], &["--help"][..]] {
        match help_text(bin, sub).await {
            Some(text) => out.extend(codex_launch::value_flags_from_help(&text)),
            None => complete = false,
        }
    }
    if !complete {
        out.extend(codex_launch::fallback_value_flags());
    }
    out
}

async fn help_text(bin: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(
        HELP_BUDGET,
        tokio::process::Command::new(bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Step 6. Runs codex in the app-server and reports its exit; returns the exit code.
pub(super) async fn attach(
    bin: &Path,
    cx: &CodexHostConfig,
    plan: &Plan,
    thread_id: Option<&str>,
) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    use tokio::signal::unix::{signal, SignalKind};
    let mut full = vec![
        "--remote".to_string(),
        format!("unix://{}", cx.socket_path.display()),
    ];
    match thread_id {
        Some(id) => full.extend(codex_launch::args_with_thread(plan, id)),
        None => {
            if !plan.explicit_cd {
                full.push("-C".into());
                full.push(plan.cwd.display().to_string());
            }
            full.extend(plan.codex_args.iter().cloned());
        }
    }
    // The launcher must outlive codex to report the exit. Ctrl-C and a closed
    // terminal are delivered to the whole foreground group: codex handles its own
    // copy (in raw mode ^C is a keystroke to it), ours is absorbed here. Handlers are
    // reset by exec, so codex's disposition is untouched.
    let _int = signal(SignalKind::interrupt());
    let _hup = signal(SignalKind::hangup());
    let status = tokio::process::Command::new(bin).args(&full).status().await;
    let code = match status {
        Ok(s) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
        Err(e) => fail(126, &format!("cannot run {}: {e}", bin.display())),
    };
    // Quitting a --remote TUI emits nothing of its own; the launcher is what tells
    // ctm the session is over — by thread id when it knows it (a resume), else by
    // directory (a new session's id is learned by the daemon from the hooks).
    let cwd = plan.cwd.display().to_string();
    let _ = match thread_id {
        Some(id) => super::codex_hook_cmd::run_exited_for(id, Some(&cwd)).await,
        None => super::codex_hook_cmd::run_exited(&cwd).await,
    };
    code
}
