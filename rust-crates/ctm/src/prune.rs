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

/// Is the session that owns `session_id` currently alive?
///
/// Used to build the skip-set so a live conversation's topic is never deleted. The
/// policy deliberately mirrors the daemon's own (`liveness::liveness_decision`), and
/// for a reason found the hard way: this function used to answer "no tmux route → dead",
/// the exact opposite of the daemon's "no tmux route → only inactivity can declare it
/// dead". With host-native sessions (ADR-016) that have no pane at all, and Claude Code
/// sessions started outside tmux, `--ledger` therefore offered to delete topics of
/// sessions that were plainly alive.
///
/// Liveness is established positively wherever the host can be asked:
/// - **Codex** — the app-server lists the threads it currently holds
///   (`thread/loaded/list`), so a listed thread is alive and an unlisted one is not.
/// - **OpenCode** — an HTTP server, when one is configured, lists its sessions.
/// - **Claude Code** — the tmux pane, as before.
///
/// Anything that cannot be asked falls back to inactivity, never to "dead".
fn session_is_alive(mgr: &SessionManager, session_id: &str, live: &LiveHostSessions) -> bool {
    let Ok(Some(s)) = mgr.get_session(session_id) else {
        return false; // no row → definitely not alive
    };
    if s.status != crate::types::SessionStatus::Active {
        return false;
    }
    // ADR-016 hosts: ask the host itself.
    match s.host_kind() {
        crate::types::HostKind::Codex => {
            return match &live.codex {
                Some(threads) => threads.contains(&host_session_id(&s)),
                // The app-server could not be reached: fall back to inactivity rather
                // than declaring a session dead because ctm cannot see it right now.
                None => !inactive_beyond_timeout(&s, live.stale_hours),
            };
        }
        crate::types::HostKind::OpenCode => {
            return match &live.opencode {
                Some(sessions) => sessions.contains(&host_session_id(&s)),
                None => !inactive_beyond_timeout(&s, live.stale_hours),
            };
        }
        crate::types::HostKind::ClaudeCode => {}
    }
    match s.tmux_target.as_deref() {
        Some(target) => {
            let socket = s.tmux_socket.as_deref();
            InputInjector::is_pane_alive(target, socket)
                && InputInjector::pane_claude_state(target, socket)
                    == PaneClaudeState::RunningClaude
        }
        // No tmux route (a session started outside tmux): the daemon's policy is that
        // only inactivity may declare this dead, and prune now agrees with it.
        None => !inactive_beyond_timeout(&s, live.stale_hours),
    }
}

/// The host's own id for this session (`metadata.hostSessionId`), falling back to the
/// ctm session id — for both native hosts they are the same value today.
fn host_session_id(s: &crate::session::Session) -> String {
    s.metadata
        .as_deref()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .and_then(|v| {
            v.get("hostSessionId")
                .and_then(|x| x.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| s.id.clone())
}

/// Has this session been silent longer than the stale-session timeout?
fn inactive_beyond_timeout(s: &crate::session::Session, stale_hours: u32) -> bool {
    let hours = stale_hours.max(1);
    let Ok(last) = chrono::DateTime::parse_from_rfc3339(&s.last_activity) else {
        return false; // unparseable timestamp: never a reason to delete
    };
    chrono::Utc::now().signed_duration_since(last.with_timezone(&chrono::Utc))
        > chrono::Duration::hours(hours as i64)
}

/// Sessions each non-Claude host currently holds. `None` means "could not ask", which
/// is treated as "do not claim it is dead".
#[derive(Debug, Default)]
struct LiveHostSessions {
    codex: Option<HashSet<String>>,
    opencode: Option<HashSet<String>>,
    /// Config's stale-session threshold, for the "cannot ask" fallback.
    stale_hours: u32,
}

impl LiveHostSessions {
    /// Ask each configured host which sessions it still has. Best-effort and bounded:
    /// prune must work with every host down.
    async fn probe(config: &config::Config) -> Self {
        let mut live = Self {
            stale_hours: config.stale_session_timeout_hours,
            ..Default::default()
        };
        if config.hosts.codex.enabled {
            if let Ok(mut rpc) =
                crate::host::codex_rpc::Rpc::connect(&config.hosts.codex.socket_path).await
            {
                if let Ok(v) = rpc.call("thread/loaded/list", serde_json::json!({})).await {
                    live.codex = Some(
                        v.get("data")
                            .and_then(|d| d.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str())
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    );
                }
            }
        }
        if let Some(base) = config.hosts.opencode.base_url.as_deref() {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(4))
                .build();
            if let Ok(c) = client {
                let mut rb = c.get(format!("{}/session", base.trim_end_matches('/')));
                if let Some(pw) = config.hosts.opencode.resolve_password() {
                    rb = rb.basic_auth("opencode", Some(pw));
                }
                if let Ok(r) = rb.send().await {
                    if let Ok(v) = r.json::<serde_json::Value>().await {
                        live.opencode = Some(
                            v.as_array()
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|s| s.get("id").and_then(|i| i.as_str()))
                                        .map(str::to_string)
                                        .collect()
                                })
                                .unwrap_or_default(),
                        );
                    }
                }
            }
        }
        live
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
    // Hosts are asked once, up front, rather than per session.
    let live = LiveHostSessions::probe(&config).await;
    let mut protected: HashSet<i64> = HashSet::new();
    protected.insert(GENERAL_TOPIC_ID);
    for s in mgr.get_active_sessions().unwrap_or_default() {
        if let Some(tid) = s.thread_id {
            if session_is_alive(&mgr, &s.id, &live) {
                protected.insert(tid);
            }
        }
    }

    if args.ledger {
        run_ledger_mode(&bot, &mgr, &protected, &live, args.dry_run, args.yes).await
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
    live: &LiveHostSessions,
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
                    .map(|s| session_is_alive(mgr, s, live))
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

#[cfg(test)]
mod liveness_tests {
    use super::*;
    use crate::session::Session;
    use crate::types::SessionStatus;

    fn session(host: Option<&str>, tmux: Option<&str>, last_activity: &str) -> Session {
        Session {
            id: "01a0bc25-960b-7941-80df-5c99b46679f7".into(),
            chat_id: -100,
            thread_id: Some(42),
            status: SessionStatus::Active,
            started_at: last_activity.into(),
            last_activity: last_activity.into(),
            hostname: None,
            project_dir: None,
            tmux_target: tmux.map(str::to_string),
            tmux_socket: None,
            host_kind: host.map(str::to_string),
            metadata: Some(r#"{"hostSessionId":"01a0bc25-960b-7941-80df-5c99b46679f7"}"#.into()),
            parent_session_id: None,
            agent_id: None,
            agent_type: None,
            custom_title: None,
        }
    }

    fn hours_ago(h: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::hours(h))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    #[test]
    fn a_session_with_no_tmux_route_is_not_declared_dead_while_it_is_recent() {
        // The bug that made `--ledger` offer to delete live conversations: prune said
        // "no pane → dead" while the daemon says only inactivity may decide.
        let s = session(None, None, &hours_ago(0));
        assert!(!inactive_beyond_timeout(&s, 24));
        let stale = session(None, None, &hours_ago(48));
        assert!(inactive_beyond_timeout(&stale, 24));
    }

    #[test]
    fn an_unparseable_timestamp_never_justifies_deletion() {
        let s = session(None, None, "not a date");
        assert!(!inactive_beyond_timeout(&s, 24));
    }

    #[test]
    fn host_session_id_prefers_the_hosts_own_id() {
        let s = session(Some("codex"), None, &hours_ago(0));
        assert_eq!(host_session_id(&s), "01a0bc25-960b-7941-80df-5c99b46679f7");
        let mut bare = s.clone();
        bare.metadata = None;
        assert_eq!(host_session_id(&bare), bare.id);
    }

    #[test]
    fn a_codex_thread_the_app_server_still_holds_is_alive() {
        let s = session(Some("codex"), None, &hours_ago(72));
        let listed: HashSet<String> = [s.id.clone()].into_iter().collect();
        // Listed by `thread/loaded/list` → alive, even though it has been idle for days
        // and has no tmux pane at all.
        assert!(listed.contains(&host_session_id(&s)));
        // Not listed → dead, regardless of how recent it is.
        let gone: HashSet<String> = HashSet::new();
        assert!(!gone.contains(&host_session_id(&s)));
    }
}
