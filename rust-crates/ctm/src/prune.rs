//! STALE-TOPICS: `ctm prune-topics` — clear an accumulated backlog of stale Telegram
//! forum topics.
//!
//! Two modes (see the CLI help on `Commands::PruneTopics`):
//!
//! `--ledger` deletes every topic recorded in the persistent ledger whose Claude session
//! is no longer alive — the surefire path for topics created by a build that has the
//! ledger.
//!
//! `--from`/`--to` sweeps a numeric topic-id range, calling `deleteForumTopic` on each id.
//! This is the ONLY way to reach legacy orphan topics that predate the ledger and have no
//! record anywhere (the Telegram Bot API cannot enumerate forum topics). Non-topic ids
//! return a harmless HTTP-400 and are skipped.
//!
//! Both modes refuse to touch the General topic (id 1) and the topic of any session that
//! is currently active, so a live conversation can never be pruned out from under you.

use crate::bot::TelegramBot;
use crate::config;
use crate::error::Result;
use crate::injector::{InputInjector, PaneClaudeState};
use crate::session::SessionManager;
use std::collections::HashSet;

/// Parsed `prune-topics` arguments.
pub struct PruneArgs {
    pub ledger: bool,
    pub ids: Option<std::path::PathBuf>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub dry_run: bool,
    pub yes: bool,
}

/// The General/root forum topic — never a candidate for deletion.
const GENERAL_TOPIC_ID: i64 = 1;

/// Is the session that owns `thread_id` currently alive? Used to build the skip-set so we
/// never delete a live conversation's topic. "Alive" = an active DB row whose tmux pane is
/// up and still running Claude (mirrors the daemon's reconcile policy).
fn session_is_alive(mgr: &SessionManager, session_id: &str) -> bool {
    let Ok(Some(s)) = mgr.get_session(session_id) else {
        return false; // no row → definitely not alive
    };
    if s.status != crate::types::SessionStatus::Active {
        return false;
    }
    match s.tmux_target.as_deref() {
        Some(target) => {
            let socket = s.tmux_socket.as_deref();
            InputInjector::is_pane_alive(target, socket)
                && InputInjector::pane_claude_state(target, socket)
                    == PaneClaudeState::RunningClaude
        }
        None => false,
    }
}

/// Entry point for `ctm prune-topics`.
pub async fn run_prune(args: PruneArgs) -> anyhow::Result<()> {
    let has_range = args.from.is_some() || args.to.is_some();
    let mode_count = [args.ledger, args.ids.is_some(), has_range]
        .iter()
        .filter(|b| **b)
        .count();
    if mode_count == 0 {
        anyhow::bail!(
            "specify a mode: `--ledger`, `--ids <file>`, or a range `--from <id> --to <id>`\n\
             (see `ctm prune-topics --help`)"
        );
    }
    if mode_count > 1 {
        anyhow::bail!("choose exactly one of --ledger / --ids / --from..--to");
    }

    let config = config::load_config(true)?;
    let mgr = SessionManager::new(&config.config_dir, config.session_timeout)?;
    let bot = TelegramBot::new(&config)?;

    // Build the protect-set: thread_ids of currently-alive sessions, plus General.
    let mut protected: HashSet<i64> = HashSet::new();
    protected.insert(GENERAL_TOPIC_ID);
    for s in mgr.get_active_sessions().unwrap_or_default() {
        if let Some(tid) = s.thread_id {
            if session_is_alive(&mgr, &s.id) {
                protected.insert(tid);
            }
        }
    }

    if args.ledger {
        run_ledger_mode(&bot, &mgr, &protected, args.dry_run, args.yes).await
    } else if let Some(path) = args.ids {
        run_ids_mode(&bot, &mgr, &protected, &path, args.dry_run, args.yes).await
    } else {
        let from = args.from.unwrap();
        let to = args.to.unwrap();
        run_range_mode(&bot, &mgr, &protected, from, to, args.dry_run, args.yes).await
    }
}

/// `--ids FILE`: delete exactly the topic ids listed in `path` (one per line). Blank lines
/// and `#` comments are ignored. This is the precise companion to scripts/list_topics.py,
/// which enumerates every existing topic via MTProto (the Bot API cannot list them).
async fn run_ids_mode(
    bot: &TelegramBot,
    mgr: &SessionManager,
    protected: &HashSet<i64>,
    path: &std::path::Path,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read --ids file {}: {e}", path.display()))?;

    let mut ids: Vec<i64> = Vec::new();
    let mut malformed = 0usize;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        match t.parse::<i64>() {
            Ok(id) => ids.push(id),
            Err(_) => malformed += 1,
        }
    }
    // De-dup and drop protected ids up front.
    ids.sort_unstable();
    ids.dedup();
    let before = ids.len();
    ids.retain(|id| !protected.contains(id));
    let skipped_protected = before - ids.len();

    println!(
        "Ids mode: {} id(s) from {} ({} malformed line(s) ignored, {} protected/active skipped).",
        ids.len(),
        path.display(),
        malformed,
        skipped_protected
    );
    if ids.is_empty() {
        println!("Nothing to prune.");
        return Ok(());
    }
    if dry_run {
        for id in &ids {
            println!("  would delete topic {id}");
        }
        println!("(dry run — nothing deleted)");
        return Ok(());
    }
    if !yes && !confirm(&format!("Delete {} topic(s)?", ids.len()))? {
        println!("Aborted.");
        return Ok(());
    }

    let mut deleted = 0usize;
    for id in &ids {
        if delete_one(bot, mgr, *id).await {
            deleted += 1;
        }
        // Rate-limit Telegram API calls.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    println!(
        "Done. Deleted/confirmed-gone {deleted} of {} topic(s).",
        ids.len()
    );
    Ok(())
}

/// `--ledger`: prune recorded topics whose session is dead.
async fn run_ledger_mode(
    bot: &TelegramBot,
    mgr: &SessionManager,
    protected: &HashSet<i64>,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let ledger = mgr.get_ledger_topics().unwrap_or_default();
    let candidates: Vec<(i64, Option<String>)> = ledger
        .into_iter()
        .filter(|(tid, sid)| {
            !protected.contains(tid)
                && !sid
                    .as_deref()
                    .map(|s| session_is_alive(mgr, s))
                    .unwrap_or(false)
        })
        .collect();

    println!(
        "Ledger mode: {} recorded topic(s), {} dead candidate(s) to prune.",
        mgr.get_ledger_topics().map(|v| v.len()).unwrap_or(0),
        candidates.len()
    );
    if candidates.is_empty() {
        println!("Nothing to prune.");
        return Ok(());
    }
    if dry_run {
        for (tid, sid) in &candidates {
            println!(
                "  would delete topic {tid} (session {})",
                sid.as_deref().unwrap_or("?")
            );
        }
        println!("(dry run — nothing deleted)");
        return Ok(());
    }
    if !yes && !confirm(&format!("Delete {} topic(s)?", candidates.len()))? {
        println!("Aborted.");
        return Ok(());
    }

    let mut deleted = 0usize;
    for (tid, _sid) in &candidates {
        if delete_one(bot, mgr, *tid).await {
            deleted += 1;
        }
    }
    println!(
        "Done. Deleted/confirmed-gone {deleted} of {} topic(s).",
        candidates.len()
    );
    Ok(())
}

/// `--from`/`--to`: sweep a numeric topic-id range.
async fn run_range_mode(
    bot: &TelegramBot,
    mgr: &SessionManager,
    protected: &HashSet<i64>,
    from: i64,
    to: i64,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    if from > to || from < 1 {
        anyhow::bail!("invalid range: --from must be >= 1 and <= --to");
    }
    let span = (to - from + 1) as u64;
    let skipped_protected = (from..=to).filter(|id| protected.contains(id)).count();

    println!(
        "Range mode: scanning topic ids {from}..={to} ({span} id(s); {skipped_protected} protected/active will be skipped).",
    );
    println!(
        "Note: ids that are not forum topics return a harmless error and are skipped. \
         This issues up to {span} rate-limited Telegram calls."
    );
    if dry_run {
        println!("(dry run — nothing deleted)");
        return Ok(());
    }
    if !yes && !confirm(&format!("Sweep and delete forum topics in {from}..={to}?"))? {
        println!("Aborted.");
        return Ok(());
    }

    let mut deleted = 0usize;
    let mut scanned = 0u64;
    for id in from..=to {
        if protected.contains(&id) {
            continue;
        }
        if delete_one(bot, mgr, id).await {
            deleted += 1;
            println!("  deleted topic {id}");
        }
        scanned += 1;
        if scanned.is_multiple_of(500) {
            println!("  ...scanned {scanned}/{span}, deleted {deleted} so far");
        }
    }
    println!("Done. Deleted {deleted} topic(s) across {scanned} scanned id(s).");
    Ok(())
}

/// Delete one topic, applying the confirmed-gone rule: on `Ok(_)` (deleted or HTTP-400
/// already-gone) drop the ledger entry and return true; on a transient `Err` keep the
/// ledger entry (so a later run retries) and return false.
async fn delete_one(bot: &TelegramBot, mgr: &SessionManager, thread_id: i64) -> bool {
    match bot.delete_forum_topic(thread_id).await {
        Ok(true) => {
            let _ = mgr.forget_topic(thread_id);
            true
        }
        Ok(false) => {
            // HTTP-400: not a topic / already gone. Drop any ledger entry; not counted as
            // a fresh deletion in range mode (it wasn't a live topic).
            let _ = mgr.forget_topic(thread_id);
            false
        }
        Err(e) => {
            tracing::warn!(thread_id, error = %e, "prune-topics: transient delete failure — retained for retry");
            false
        }
    }
}

/// Interactive y/N confirmation on stdin.
fn confirm(prompt: &str) -> Result<bool> {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Ok(false);
    }
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
