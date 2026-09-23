//! ADR-022: the runner half of `ctm codex-launch` — carry out a [`Plan`] against the
//! app-server and attach codex to the result.
//!
//! Every step either succeeds or stops the launch with one line saying why. Nothing
//! attaches to a thread whose settings are not the ones typed: a failed or unverified
//! apply is exit 1, never "attaching as-is" (the reviewers' first ship-blocker — with
//! the flags already stripped, as-is can be *wider* than what was typed).
//!
//! The resume sequence, each step run against a real daemon (spike 2026-09-22):
//! 1. `thread/read` — does the id exist, and is a turn in progress (`status.type`
//!    `active`, `idle`, `notLoaded`)?
//! 2. guard — no turn in progress, and not live in another terminal (ctm's store).
//! 3. `thread/resume {excludeTurns}` — loads an unloaded thread (an unloaded one
//!    cannot take settings: "thread not found"), and returns the effective
//!    `cwd`/`sandbox`/`approvalPolicy` — the same values the TUI's `/status` shows.
//! 4. `thread/settings/update` — cwd, permission profile, approval policy, in one
//!    call (a named profile is first checked against `permissionProfile/list {cwd}`).
//! 5. `thread/resume` again — verify the effective values are the requested ones.
//! 6. attach: `codex --remote unix://<sock> resume <id> …` with the translated flags
//!    gone and the selector replaced by the exact id. Report the exit by that id.
//!
//! A fork is steps 1 and 3–6 on a *new* thread: `thread/fork {threadId, cwd, sandbox,
//! approvalPolicy}` (a remote `codex fork` refuses the flags exactly as resume does),
//! then codex attaches to the fork with `resume`. The parent is not touched.
//!
//! The pure parts (what a listed thread is, selection among them, the
//! effective-settings check) live in `codex_threads.rs` with their unit tests; the
//! sequence is proven by the `--ignored` e2e in `tests/host_e2e.rs` against an
//! isolated daemon and a real TUI.

use super::codex_launch::{self, Kind, Plan, Refusal, ThreadRef};
use super::codex_launch_exec::{attach, exec_verbatim, fail, value_flags};
use super::codex_rpc::Rpc;
use super::codex_threads::{
    describe, describe_effective, find_by_name, parse_effective, parse_thread, pick_last,
    satisfied, short, Effective, ThreadSummary,
};
use serde_json::{json, Value};
use std::io::IsTerminal;
use std::time::Duration;

/// Loading a large rollout through `thread/resume` is slower than a management call.
const RPC_BUDGET: Duration = Duration::from_secs(60);
/// `thread/list` page size and the most pages walked when resolving a name/`--last`.
const PAGE: u32 = 50;
const MAX_PAGES: usize = 20;
/// What the picker shows.
const PICK: usize = 10;

/// `ctm codex-launch <args…>`. Exits with codex's status (or the launcher's own).
pub async fn run(args: Vec<String>) -> anyhow::Result<()> {
    let code = launch(args).await;
    std::process::exit(code)
}

async fn launch(args: Vec<String>) -> i32 {
    let pwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return fail(2, &format!("cannot read the current directory: {e}")),
    };
    let cfg = crate::config::load_config(false);
    let bin_override = cfg.as_ref().ok().and_then(|c| c.hosts.codex.binary.clone());
    let Some(bin) = super::detect::codex_binary(bin_override.as_deref()) else {
        return fail(127, "codex binary not found");
    };
    let plan = match codex_launch::plan(&args, &pwd, &value_flags(&bin).await) {
        Ok(p) => p,
        Err(Refusal { message, exit_code }) => return fail(exit_code, &message),
    };
    if matches!(plan.kind, Kind::Verbatim(_)) {
        return exec_verbatim(&bin, &args);
    }
    // Why not the app-server: the operator opted out, ctm is not configured, the host
    // is disabled, or the daemon is not there. Local is exactly what was typed; a
    // thread the daemon holds is then codex's own "open in another app" to explain.
    if std::env::var("CTM_CODEX_REMOTE").is_ok_and(|v| v == "0") {
        return exec_verbatim(&bin, &args);
    }
    let cfg = match cfg {
        Ok(c) if c.hosts.codex.enabled => c,
        Ok(_) => return exec_verbatim(&bin, &args),
        Err(e) => {
            eprintln!("ctm: config not loaded ({e}); running codex locally");
            return exec_verbatim(&bin, &args);
        }
    };
    // ADR-021: the daemon must be on the signed-in account before anything attaches.
    super::codex_account::preflight(&cfg).await;
    let cx = cfg.hosts.codex.clone();
    let rpc = match Rpc::connect_experimental(&cx.socket_path).await {
        Ok(r) => r.with_timeout(RPC_BUDGET),
        Err(e) => {
            eprintln!("ctm: {}; running codex locally, not mirrored", plain(e));
            return exec_verbatim(&bin, &args);
        }
    };
    match &plan.kind {
        Kind::New => {
            drop(rpc);
            attach(&bin, &cx, &plan, None).await
        }
        Kind::Resume(thread) => match prepare_resume(rpc, &plan, thread).await {
            Ok(id) => attach(&bin, &cx, &plan, Some(&id)).await,
            Err(Stop { code, message }) => fail(code, &message),
        },
        Kind::Fork(thread) => match prepare_fork(rpc, &plan, thread).await {
            Ok(id) => attach(&bin, &cx, &plan, Some(&id)).await,
            Err(Stop { code, message }) => fail(code, &message),
        },
        Kind::Verbatim(_) => unreachable!("handled above"),
    }
}

/// Why the launch stopped before attaching, and the exit status for it.
struct Stop {
    code: i32,
    message: String,
}

fn stop(message: impl Into<String>) -> Stop {
    Stop {
        code: 1,
        message: message.into(),
    }
}

impl From<crate::error::AppError> for Stop {
    fn from(e: crate::error::AppError) -> Self {
        stop(plain(e))
    }
}

/// The daemon's words without ctm's error-category prefix: a terminal line, not a log.
fn plain(e: crate::error::AppError) -> String {
    match e {
        crate::error::AppError::Socket(m) => m,
        other => other.to_string(),
    }
}

/// Which thread the command names, as an exact id.
async fn resolve(rpc: &mut Rpc, plan: &Plan, thread: &ThreadRef) -> Result<String, Stop> {
    Ok(match thread {
        ThreadRef::Id(id) => id.clone(),
        ThreadRef::Name(name) => {
            let threads = list_threads(rpc, false, None).await?;
            find_by_name(&threads, name).map_err(stop)?.id.clone()
        }
        ThreadRef::Last {
            all,
            include_non_interactive,
        } => {
            let threads = list_threads(rpc, *include_non_interactive, None).await?;
            pick_last(&threads, &plan.cwd, *all)
                .ok_or_else(|| {
                    stop(if *all {
                        "no recorded session".to_string()
                    } else {
                        format!(
                            "no recorded session in {} (use --all for any directory)",
                            plan.cwd.display()
                        )
                    })
                })?
                .id
                .clone()
        }
        ThreadRef::Picker => pick_interactively(rpc).await?,
    })
}

/// Step 1: the thread exists, and this is its state.
async fn read_thread(rpc: &mut Rpc, id: &str) -> Result<ThreadSummary, Stop> {
    let read = rpc
        .call(
            "thread/read",
            json!({"threadId": id, "includeTurns": false}),
        )
        .await
        .map_err(|e| stop(format!("session {id}: {}", plain(e))))?;
    read.get("thread")
        .and_then(parse_thread)
        .ok_or_else(|| stop(format!("session {id}: unexpected thread/read reply")))
}

/// Step 4's precondition: a named profile must be one the daemon resolves for the
/// directory; the update would otherwise fail with a less useful message.
async fn check_named_profile(rpc: &mut Rpc, plan: &Plan) -> Result<(), Stop> {
    let Some(p) = plan
        .settings
        .permissions
        .as_deref()
        .filter(|p| !p.starts_with(':'))
    else {
        return Ok(());
    };
    let profiles = rpc
        .call(
            "permissionProfile/list",
            json!({"cwd": plan.cwd.display().to_string()}),
        )
        .await?;
    let allowed: Vec<String> = profiles
        .get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|e| e.get("allowed").and_then(Value::as_bool) != Some(false))
                .filter_map(|e| e.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if allowed.iter().any(|a| a == p) {
        return Ok(());
    }
    Err(stop(format!(
        "permission profile {p:?} is not defined for {} (define [permissions.{p}] in its .codex/config.toml; available: {})",
        plan.cwd.display(),
        allowed.join(", ")
    )))
}

/// Steps 1–5 for a resume. Returns the exact thread id to attach to.
async fn prepare_resume(mut rpc: Rpc, plan: &Plan, thread: &ThreadRef) -> Result<String, Stop> {
    let id = resolve(&mut rpc, plan, thread).await?;
    let summary = read_thread(&mut rpc, &id).await?;

    // 2. Rejoining is not an error (ADR-022 amendment, 2026-09-23).
    //
    // This used to refuse when the thread was mid-turn, or when ctm's session store
    // showed it live in another terminal. Both were wrong, and the second one broke
    // resume outright: a `--remote` TUI outlives its terminal, so ctm only learns of
    // an exit if the launcher reports it — every session started before 0.2.53, and
    // any whose report was missed, leaves a row that says "active" forever. A gate
    // that fails closed on data that is known to go stale refuses forever.
    //
    // It was also a restriction the platform does not have. Reconnecting to a live
    // thread is Codex's own model: quitting a remote TUI prints "Disconnected from
    // this task. Any running work continues. Reconnect: codex --remote … resume
    // <id>". So ctm reconnects, and says what it found rather than refusing.
    if summary.status == "active" {
        let what = match summary.active_flags.first().map(String::as_str) {
            Some("waitingOnApproval") => "is waiting on an approval",
            Some("waitingOnUserInput") => "is waiting on an answer",
            _ => "has a turn in progress",
        };
        eprintln!(
            "ctm: session {} {what} — rejoining it (anything already running keeps running)",
            short(&id)
        );
    }

    // 3. Load (or rejoin) and read what is in effect now.
    let before = resume_effective(&mut rpc, &id).await?;

    // 4. Apply, one call.
    check_named_profile(&mut rpc, plan).await?;
    let mut params = json!({"threadId": id, "cwd": plan.cwd.display().to_string()});
    if let Some(p) = &plan.settings.permissions {
        params["permissions"] = Value::String(p.clone());
    }
    if let Some(a) = &plan.settings.approval {
        params["approvalPolicy"] = Value::String(a.clone());
    }
    rpc.call("thread/settings/update", params)
        .await
        .map_err(|e| stop(format!("could not apply your flags to session {} ({}); not attaching with different permissions than you typed. Currently in effect: {}", short(&id), plain(e), describe_effective(&before))))?;

    // 5. Verify from the daemon's own report, not from the update's `{}`.
    let after = resume_effective(&mut rpc, &id).await?;
    satisfied(&plan.settings, &plan.cwd, &after).map_err(|why| {
        stop(format!(
            "session {}: the daemon reports different settings than asked ({why}); not attaching",
            short(&id)
        ))
    })?;
    let changed = before != after;
    eprintln!(
        "ctm: session {}: {}{}",
        short(&id),
        describe_effective(&after),
        if changed {
            format!("  (was: {})", describe_effective(&before))
        } else {
            String::new()
        }
    );
    Ok(id)
}

/// A fork: a new thread with the parent's history and the typed settings. The parent
/// is read (it must exist) but never modified.
async fn prepare_fork(mut rpc: Rpc, plan: &Plan, thread: &ThreadRef) -> Result<String, Stop> {
    let parent = resolve(&mut rpc, plan, thread).await?;
    read_thread(&mut rpc, &parent).await?;
    check_named_profile(&mut rpc, plan).await?;
    let mut params =
        json!({"threadId": parent, "cwd": plan.cwd.display().to_string(), "excludeTurns": true});
    // `thread/fork` takes a sandbox mode, not a profile id; a named profile is
    // applied to the fork afterwards, as a resume would.
    let named = match plan.settings.permissions.as_deref() {
        Some(":read-only") => {
            params["sandbox"] = json!("read-only");
            None
        }
        Some(":workspace") => {
            params["sandbox"] = json!("workspace-write");
            None
        }
        Some(":danger-full-access") => {
            params["sandbox"] = json!("danger-full-access");
            None
        }
        other => other,
    };
    if let Some(a) = &plan.settings.approval {
        params["approvalPolicy"] = Value::String(a.clone());
    }
    let forked = rpc.call("thread/fork", params).await.map_err(|e| {
        stop(format!(
            "could not fork session {} ({})",
            short(&parent),
            plain(e)
        ))
    })?;
    let id = forked
        .get("thread")
        .and_then(|t| t.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| stop("unexpected thread/fork reply"))?
        .to_string();
    let mut effective =
        parse_effective(&forked).ok_or_else(|| stop("unexpected thread/fork reply"))?;
    if let Some(name) = named {
        rpc.call(
            "thread/settings/update",
            json!({"threadId": id, "cwd": plan.cwd.display().to_string(), "permissions": name}),
        )
        .await
        .map_err(|e| {
            stop(format!(
                "forked session {} as {}, but could not apply profile {name:?} ({}); not attaching",
                short(&parent),
                short(&id),
                plain(e)
            ))
        })?;
        effective = resume_effective(&mut rpc, &id).await?;
    }
    satisfied(&plan.settings, &plan.cwd, &effective).map_err(|why| {
        stop(format!(
            "forked session {} as {}, but the daemon reports different settings than asked ({why}); not attaching",
            short(&parent),
            short(&id)
        ))
    })?;
    eprintln!(
        "ctm: forked session {} as {}: {}",
        short(&parent),
        short(&id),
        describe_effective(&effective)
    );
    Ok(id)
}

async fn resume_effective(rpc: &mut Rpc, id: &str) -> Result<Effective, Stop> {
    let v = rpc
        .call(
            "thread/resume",
            json!({"threadId": id, "excludeTurns": true}),
        )
        .await
        .map_err(|e| {
            stop(format!(
                "session {}: could not load it ({})",
                short(id),
                plain(e)
            ))
        })?;
    parse_effective(&v).ok_or_else(|| {
        stop(format!(
            "session {}: unexpected thread/resume reply",
            short(id)
        ))
    })
}

/// Newest first. Interactive sources unless `non_interactive` (codex's `--last` rule).
async fn list_threads(
    rpc: &mut Rpc,
    non_interactive: bool,
    limit: Option<u32>,
) -> Result<Vec<ThreadSummary>, Stop> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut params = json!({"limit": limit.unwrap_or(PAGE), "sortKey": "updated_at"});
        if non_interactive {
            params["sourceKinds"] = json!(["cli", "vscode", "appServer", "exec"]);
        }
        if let Some(c) = &cursor {
            params["cursor"] = Value::String(c.clone());
        }
        let v = rpc.call("thread/list", params).await?;
        out.extend(
            v.get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(parse_thread),
        );
        if limit.is_some_and(|l| out.len() >= l as usize) {
            break;
        }
        match v.get("nextCursor").and_then(Value::as_str) {
            Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

/// `resume` with no session named: ctm's own picker, so the flags typed still apply
/// to whatever is picked (codex's picker runs after any settings could be applied).
async fn pick_interactively(rpc: &mut Rpc) -> Result<String, Stop> {
    let threads = list_threads(rpc, false, Some(PICK as u32)).await?;
    if threads.is_empty() {
        return Err(stop("no recorded session to resume"));
    }
    let listing: Vec<String> = threads
        .iter()
        .take(PICK)
        .enumerate()
        .map(|(i, t)| format!("  {:>2}  {}", i + 1, describe(t)))
        .collect();
    if !std::io::stdin().is_terminal() {
        return Err(stop(format!(
            "which session? Not a terminal, so resume by id:\n{}",
            listing.join("\n")
        )));
    }
    eprintln!("ctm: which session? (newest first)\n{}", listing.join("\n"));
    eprint!("resume [1-{}], or q: ", listing.len());
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Err(stop("nothing resumed"));
    }
    let choice = line.trim();
    match choice.parse::<usize>() {
        Ok(n) if (1..=listing.len()).contains(&n) => Ok(threads[n - 1].id.clone()),
        _ => Err(stop("nothing resumed")),
    }
}
