//! ADR-023: the daemon watches itself and recovers without being asked.
//!
//! Reported: "ctm is dead now on this machine — neither direction working." It was
//! not dead. Hook events kept arriving at ~80/min for 42 minutes and not one handler
//! logged anything: no error, no warning, the process healthy, every thread parked.
//! A parked async task has no OS stack, so `sample` could not name the culprit either.
//! Two things were missing and both are here: a definition of "making progress" the
//! daemon checks itself, and a recovery it performs itself.
//!
//! **Why an OS thread.** The obvious watchdog — a `tokio::spawn` loop — cannot report
//! the one failure that matters most: a runtime with no free worker. This watchdog is
//! a plain `std::thread` reading atomics, so it runs when nothing else does.
//!
//! **What "progress" means.** Not "the process is alive" (it was) and not "a task
//! ticks" (the runtime was fine). Progress is *work arriving and work finishing*:
//!   - the socket layer counts every event it accepts (`received`);
//!   - the event loop counts every handler it spawns (`dispatched`) and stamps when;
//!   - every handler stamps its completion (`completed`, `progress_ms`) through a
//!     guard that runs on drop — so a panicked or aborted handler counts too;
//!   - the bot stamps each successful send and the queue depth.
//!
//! A daemon with nothing to do is healthy and silent; one with work in hand and
//! nothing finishing is stalled. That distinction is the whole design.
//!
//! **Recovery, in order.** Abort the handlers that are stuck, because dropping their
//! futures releases the locks and semaphore permits they hold — that fixes a deadlock
//! in place, without losing the process or the sessions. Only if that does not restore
//! progress does the daemon end its own process, which launchd (`KeepAlive`,
//! `ThrottleInterval` 10 s) and systemd (`Restart=on-failure`, `RestartSec=10s`) turn
//! into a restart — both verified, not assumed. A budget stops a crash loop, and the
//! next start tells the user in Telegram what happened.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Exit status the daemon uses to ask its service manager for a fresh process.
/// `EX_TEMPFAIL`: non-zero, so `KeepAlive{SuccessfulExit:false}` and
/// `Restart=on-failure` both restart it; distinct, so the log says who chose it.
pub const EXIT_STALLED: i32 = 75;

/// Name of the marker the stalled process leaves for its successor.
const MARKER: &str = "last-stall.json";
/// Name of the self-restart ledger (bounded, one line per restart).
const LEDGER: &str = "self-restarts.json";

/// One handler currently running, as the watchdog sees it.
struct InFlight {
    kind: &'static str,
    detail: String,
    started: Instant,
    abort: Option<tokio::task::AbortHandle>,
}

/// Counters and the in-flight registry. Cheap: two atomics per handler, and a
/// `std::sync::Mutex` (never held across an await) for the registry itself.
pub struct Health {
    start: Instant,
    received: AtomicU64,
    dispatched: AtomicU64,
    completed: AtomicU64,
    /// Last handler completion, ms since start. 0 = none yet.
    progress_ms: AtomicU64,
    /// Last handler spawn, ms since start.
    dispatch_ms: AtomicU64,
    /// Async heartbeat, ms since start.
    beat_ms: AtomicU64,
    /// Last successful Telegram send, ms since start.
    send_ok_ms: AtomicU64,
    queue_depth: AtomicUsize,
    permits: AtomicUsize,
    aborted: AtomicU64,
    in_flight: Mutex<HashMap<u64, InFlight>>,
    next_id: AtomicU64,
}

impl Default for Health {
    fn default() -> Self {
        Self::new()
    }
}

impl Health {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            received: AtomicU64::new(0),
            dispatched: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            progress_ms: AtomicU64::new(0),
            dispatch_ms: AtomicU64::new(0),
            beat_ms: AtomicU64::new(0),
            send_ok_ms: AtomicU64::new(0),
            queue_depth: AtomicUsize::new(0),
            permits: AtomicUsize::new(0),
            aborted: AtomicU64::new(0),
            in_flight: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        }
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// An event arrived from a hook client (counted where it is parsed, before any
    /// handler exists — this is what makes "arriving but never dispatched" visible).
    pub fn event_received(&self) {
        self.received.fetch_add(1, Ordering::Relaxed);
    }

    pub fn beat(&self) {
        self.beat_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    pub fn send_ok(&self) {
        self.send_ok_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    pub fn set_queue_depth(&self, n: usize) {
        self.queue_depth.store(n, Ordering::Relaxed);
    }

    pub fn set_permits(&self, n: usize) {
        self.permits.store(n, Ordering::Relaxed);
    }

    /// Register a handler about to be spawned. The returned id is attached to its
    /// `AbortHandle` by [`Self::attach_abort`] and released by [`InFlightGuard`].
    pub fn reserve(&self, kind: &'static str, detail: impl Into<String>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.dispatched.fetch_add(1, Ordering::Relaxed);
        self.dispatch_ms.store(self.now_ms(), Ordering::Relaxed);
        self.lock().insert(
            id,
            InFlight {
                kind,
                detail: detail.into(),
                started: Instant::now(),
                abort: None,
            },
        );
        id
    }

    /// Give the watchdog the means to cancel this handler. A no-op if the handler
    /// already finished (the common case for fast handlers).
    pub fn attach_abort(&self, id: u64, abort: tokio::task::AbortHandle) {
        if let Some(e) = self.lock().get_mut(&id) {
            e.abort = Some(abort);
        }
    }

    /// Guard whose drop marks the handler finished — on return, panic or abort.
    pub fn guard(self: &Arc<Self>, id: u64) -> InFlightGuard {
        InFlightGuard {
            health: Arc::clone(self),
            id,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, InFlight>> {
        // A panic inside a handler must not make the registry unusable: the data is
        // a plain map, so the poisoned contents are still exactly what we want.
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn finish(&self, id: u64) {
        self.lock().remove(&id);
        self.completed.fetch_add(1, Ordering::Relaxed);
        self.progress_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        let now_ms = self.now_ms();
        let reg = self.lock();
        let oldest_ms = reg
            .values()
            .map(|e| e.started.elapsed().as_millis() as u64)
            .max()
            .unwrap_or(0);
        Snapshot {
            now_ms,
            received: self.received.load(Ordering::Relaxed),
            dispatched: self.dispatched.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            in_flight: reg.len(),
            oldest_in_flight_ms: oldest_ms,
            progress_ms: self.progress_ms.load(Ordering::Relaxed),
            dispatch_ms: self.dispatch_ms.load(Ordering::Relaxed),
            beat_ms: self.beat_ms.load(Ordering::Relaxed),
            send_ok_ms: self.send_ok_ms.load(Ordering::Relaxed),
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            permits: self.permits.load(Ordering::Relaxed),
            aborted: self.aborted.load(Ordering::Relaxed),
        }
    }

    /// The in-flight table, oldest first — the line the previous incident lacked.
    pub fn in_flight_report(&self, limit: usize) -> String {
        let reg = self.lock();
        let mut rows: Vec<(u128, &'static str, &str)> = reg
            .values()
            .map(|e| (e.started.elapsed().as_millis(), e.kind, e.detail.as_str()))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        let total = rows.len();
        let mut out = String::new();
        for (age, kind, detail) in rows.into_iter().take(limit) {
            out.push_str(&format!(
                "\n    {:>7.1}s  {kind}  {detail}",
                age as f64 / 1000.0
            ));
        }
        if total > limit {
            out.push_str(&format!("\n    … and {} more", total - limit));
        }
        out
    }

    /// Cancel every handler older than `age`, releasing the locks and permits they
    /// hold. Returns what was cancelled, for the log.
    pub fn abort_older_than(&self, age: Duration) -> Vec<String> {
        let mut killed = Vec::new();
        let mut reg = self.lock();
        let ids: Vec<u64> = reg
            .iter()
            .filter(|(_, e)| e.started.elapsed() >= age)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(e) = reg.get(&id) {
                let Some(abort) = &e.abort else { continue };
                abort.abort();
                killed.push(format!(
                    "{} ({:.1}s, {})",
                    e.kind,
                    e.started.elapsed().as_secs_f64(),
                    e.detail
                ));
            }
            // The guard's drop will remove the entry once the runtime polls the task;
            // drop it here too so a task the runtime never polls again cannot make the
            // registry lie forever.
            reg.remove(&id);
        }
        self.aborted
            .fetch_add(killed.len() as u64, Ordering::Relaxed);
        killed
    }
}

/// Marks a handler finished when dropped — including when the watchdog aborts it.
pub struct InFlightGuard {
    health: Arc<Health>,
    id: u64,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.health.finish(self.id);
    }
}

/// The numbers the watchdog judges, sampled at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub now_ms: u64,
    pub received: u64,
    pub dispatched: u64,
    pub completed: u64,
    pub in_flight: usize,
    pub oldest_in_flight_ms: u64,
    pub progress_ms: u64,
    pub dispatch_ms: u64,
    pub beat_ms: u64,
    pub send_ok_ms: u64,
    pub queue_depth: usize,
    pub permits: usize,
    pub aborted: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// The async heartbeat must land at least this often.
    pub beat: Duration,
    /// Longest a handler may run. The longest legitimate wait in a handler is
    /// `wait_for_topic` (45 s), so this is comfortably above it.
    pub stall: Duration,
    /// Telegram outages are not ctm's fault: delivery is reported, never restarted for.
    pub delivery: Duration,
    pub check_every: Duration,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            beat: Duration::from_secs(30),
            stall: Duration::from_secs(120),
            delivery: Duration::from_secs(300),
            check_every: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Healthy,
    /// No worker ran the heartbeat: the runtime itself is wedged. Aborting a task
    /// requires the runtime to poll it, so this cannot be recovered in place.
    RuntimeWedged,
    /// Handlers are running and none is finishing.
    HandlersStuck,
    /// Events are arriving and the event loop is not spawning handlers for them.
    DispatchStuck,
    /// Messages are queued and none is going out.
    DeliveryStuck,
}

impl Verdict {
    pub fn is_stall(self) -> bool {
        self != Verdict::Healthy
    }

    /// Can dropping stuck futures plausibly fix this? Only when the runtime can
    /// still poll them.
    pub fn recoverable_in_place(self) -> bool {
        matches!(self, Verdict::HandlersStuck | Verdict::DeliveryStuck)
    }

    /// Is a fresh process the right answer if recovery does not take?
    pub fn warrants_restart(self) -> bool {
        matches!(
            self,
            Verdict::RuntimeWedged | Verdict::HandlersStuck | Verdict::DispatchStuck
        )
    }

    pub fn reason(self) -> &'static str {
        match self {
            Verdict::Healthy => "healthy",
            Verdict::RuntimeWedged => "the async runtime stopped running tasks",
            Verdict::HandlersStuck => "handlers are running but none is finishing",
            Verdict::DispatchStuck => "events are arriving but none is being handled",
            Verdict::DeliveryStuck => "messages are queued but none is going out",
        }
    }
}

/// The whole rule set, as one pure function — so every rule is unit-tested and the
/// thread below holds no judgement of its own.
pub fn assess(s: &Snapshot, t: &Thresholds) -> Verdict {
    let since = |stamp: u64| Duration::from_millis(s.now_ms.saturating_sub(stamp));
    // The beat is only meaningful once it has landed at least once.
    if s.beat_ms > 0 && since(s.beat_ms) > t.beat {
        return Verdict::RuntimeWedged;
    }
    // Work in hand, nothing finishing. Both parts are required: an idle daemon has
    // no in-flight work and must never be called stalled for being quiet.
    if s.in_flight > 0
        && since(s.progress_ms) > t.stall
        && Duration::from_millis(s.oldest_in_flight_ms) > t.stall
    {
        return Verdict::HandlersStuck;
    }
    // Arriving but not dispatched: the event loop itself is not running.
    if s.received > s.dispatched && since(s.dispatch_ms) > t.stall {
        return Verdict::DispatchStuck;
    }
    if s.queue_depth > 0 && s.send_ok_ms > 0 && since(s.send_ok_ms) > t.delivery {
        return Verdict::DeliveryStuck;
    }
    Verdict::Healthy
}

/// What a stalled process leaves behind for its successor to report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StallMarker {
    pub at: String,
    pub reason: String,
    pub in_flight: usize,
    pub received: u64,
    pub dispatched: u64,
    pub completed: u64,
    pub restarted: bool,
}

pub fn read_marker(config_dir: &Path) -> Option<StallMarker> {
    let p = config_dir.join(MARKER);
    let text = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::remove_file(&p);
    serde_json::from_str(&text).ok()
}

fn write_marker(config_dir: &Path, m: &StallMarker) {
    if let Ok(text) = serde_json::to_string_pretty(m) {
        let _ = std::fs::write(config_dir.join(MARKER), text);
    }
}

/// Self-restart timestamps within the last hour. A wedge that returns immediately
/// must not become a restart loop: past the budget the daemon keeps recovering in
/// place and says so, which is strictly better than exiting every ten seconds.
fn restarts_last_hour(config_dir: &Path) -> Vec<i64> {
    let now = chrono::Utc::now().timestamp();
    std::fs::read_to_string(config_dir.join(LEDGER))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<i64>>(&t).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|t| now - t < 3600)
        .collect()
}

fn record_restart(config_dir: &Path) {
    let mut v = restarts_last_hour(config_dir);
    v.push(chrono::Utc::now().timestamp());
    if let Ok(text) = serde_json::to_string(&v) {
        let _ = std::fs::write(config_dir.join(LEDGER), text);
    }
}

pub struct Watchdog {
    pub health: Arc<Health>,
    pub config_dir: PathBuf,
    pub thresholds: Thresholds,
    pub max_restarts_per_hour: usize,
    /// False in tests: a stall is recovered in place and reported, never by exiting
    /// the test binary.
    pub may_restart: bool,
}

impl Watchdog {
    /// Run the watchdog on its own OS thread. Returns its handle; the thread lives
    /// as long as the process.
    pub fn spawn(self) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("ctm-watchdog".into())
            .spawn(move || self.run())
            .expect("watchdog thread")
    }

    fn run(self) {
        // Nothing to judge until the daemon has had a moment to start.
        std::thread::sleep(self.thresholds.check_every);
        let mut episode: Option<Episode> = None;
        loop {
            std::thread::sleep(self.thresholds.check_every);
            let snap = self.health.snapshot();
            let verdict = assess(&snap, &self.thresholds);
            if !verdict.is_stall() {
                if let Some(e) = episode.take() {
                    tracing::info!(
                        reason = e.verdict.reason(),
                        aborted = e.aborted,
                        held_secs = e.started.elapsed().as_secs(),
                        "ADR-023: the daemon recovered itself; progress has resumed"
                    );
                }
                continue;
            }
            match &mut episode {
                None => {
                    tracing::error!(
                        reason = verdict.reason(),
                        received = snap.received,
                        dispatched = snap.dispatched,
                        completed = snap.completed,
                        in_flight = snap.in_flight,
                        oldest_in_flight_secs = snap.oldest_in_flight_ms / 1000,
                        since_progress_secs = (snap.now_ms.saturating_sub(snap.progress_ms)) / 1000,
                        queue_depth = snap.queue_depth,
                        free_permits = snap.permits,
                        "ADR-023: STALLED — {}{}",
                        verdict.reason(),
                        self.health.in_flight_report(15)
                    );
                    let mut ep = Episode {
                        verdict,
                        started: Instant::now(),
                        aborted: 0,
                    };
                    if verdict.recoverable_in_place() {
                        let killed = self.health.abort_older_than(self.thresholds.stall);
                        ep.aborted = killed.len();
                        if killed.is_empty() {
                            tracing::error!(
                                "ADR-023: nothing to cancel — the stall is not in a handler"
                            );
                        } else {
                            tracing::warn!(
                                "ADR-023: cancelled {} stuck handler(s) to release their locks and permits: {}",
                                killed.len(),
                                killed.join("; ")
                            );
                        }
                    }
                    episode = Some(ep);
                }
                Some(ep) => {
                    // Still stalled after a full stall window with recovery attempted:
                    // the process cannot fix itself.
                    if ep.started.elapsed() < self.thresholds.stall {
                        continue;
                    }
                    if !verdict.warrants_restart() {
                        continue;
                    }
                    self.restart(&snap, verdict);
                    ep.started = Instant::now();
                }
            }
        }
    }

    fn restart(&self, snap: &Snapshot, verdict: Verdict) {
        let recent = restarts_last_hour(&self.config_dir);
        let marker = StallMarker {
            at: chrono::Utc::now().to_rfc3339(),
            reason: verdict.reason().to_string(),
            in_flight: snap.in_flight,
            received: snap.received,
            dispatched: snap.dispatched,
            completed: snap.completed,
            restarted: self.may_restart && recent.len() < self.max_restarts_per_hour,
        };
        write_marker(&self.config_dir, &marker);
        if !self.may_restart {
            tracing::error!(
                reason = verdict.reason(),
                "ADR-023: would restart the process now (disabled in this build/test)"
            );
            return;
        }
        if recent.len() >= self.max_restarts_per_hour {
            tracing::error!(
                restarts_last_hour = recent.len(),
                budget = self.max_restarts_per_hour,
                "ADR-023: stalled again but the self-restart budget is spent — staying up and retrying recovery in place. `ctm restart` (and the log above) is the next step."
            );
            return;
        }
        record_restart(&self.config_dir);
        tracing::error!(
            reason = verdict.reason(),
            exit_code = EXIT_STALLED,
            "ADR-023: recovery did not take — ending this process so the service manager starts a healthy one"
        );
        // tracing's writer is not flushed by `exit`; say it on stderr too, because
        // this line is the last thing the operator will see from this process.
        eprintln!(
            "ctm: stalled ({}) — restarting the daemon (exit {EXIT_STALLED})",
            verdict.reason()
        );
        std::process::exit(EXIT_STALLED);
    }
}

struct Episode {
    verdict: Verdict,
    started: Instant,
    aborted: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Thresholds {
        Thresholds {
            beat: Duration::from_secs(30),
            stall: Duration::from_secs(120),
            delivery: Duration::from_secs(300),
            check_every: Duration::from_millis(10),
        }
    }

    fn healthy() -> Snapshot {
        Snapshot {
            now_ms: 600_000,
            received: 100,
            dispatched: 100,
            completed: 100,
            in_flight: 0,
            oldest_in_flight_ms: 0,
            progress_ms: 599_000,
            dispatch_ms: 599_000,
            beat_ms: 599_500,
            send_ok_ms: 599_000,
            queue_depth: 0,
            permits: 50,
            aborted: 0,
        }
    }

    #[test]
    fn an_idle_daemon_is_healthy_however_long_it_has_been_quiet() {
        // The failure this guards against is a watchdog that restarts a daemon for
        // having nothing to do — which would be worse than the bug it replaces.
        let mut s = healthy();
        s.now_ms = 86_400_000;
        s.progress_ms = 1_000;
        s.dispatch_ms = 1_000;
        s.send_ok_ms = 1_000;
        s.beat_ms = 86_399_500;
        assert_eq!(assess(&s, &t()), Verdict::Healthy);
        // Still healthy with work in flight that has not yet aged out.
        let mut s = healthy();
        s.in_flight = 3;
        s.oldest_in_flight_ms = 30_000;
        s.progress_ms = s.now_ms - 30_000;
        assert_eq!(assess(&s, &t()), Verdict::Healthy);
    }

    #[test]
    fn the_incident_shape_is_detected() {
        // 2026-09-22: ~80 events/min arriving, handlers in flight, nothing completing
        // for 42 minutes. Both signals present: no completion, and the oldest handler
        // is older than the stall window.
        let mut s = healthy();
        s.now_ms = 3_000_000; // 50 min up
        s.received = 3400;
        s.dispatched = 3400;
        s.completed = 3350;
        s.in_flight = 50;
        s.permits = 0;
        s.oldest_in_flight_ms = 2_520_000; // 42 min, the observed silence
        s.progress_ms = s.now_ms - 2_520_000;
        s.beat_ms = s.now_ms - 500; // the runtime was fine; that is the point
        let v = assess(&s, &t());
        assert_eq!(v, Verdict::HandlersStuck);
        assert!(
            v.recoverable_in_place(),
            "dropping their futures frees the locks"
        );
        assert!(v.warrants_restart(), "and a fresh process if that fails");
    }

    #[test]
    fn a_dead_runtime_is_not_recoverable_in_place() {
        let mut s = healthy();
        s.beat_ms = s.now_ms - 60_000;
        let v = assess(&s, &t());
        assert_eq!(v, Verdict::RuntimeWedged);
        assert!(
            !v.recoverable_in_place(),
            "aborting needs a runtime to poll the task"
        );
        assert!(v.warrants_restart());
        // Before the first beat lands there is nothing to judge.
        let mut s = healthy();
        s.beat_ms = 0;
        assert_eq!(assess(&s, &t()), Verdict::Healthy);
    }

    #[test]
    fn events_arriving_but_never_handled_is_its_own_verdict() {
        // The blind spot a handler-only rule would have: the event loop is stuck, so
        // nothing is ever in flight and nothing ever completes.
        let mut s = healthy();
        s.received = 500;
        s.dispatched = 100;
        s.in_flight = 0;
        s.dispatch_ms = s.now_ms - 200_000;
        let v = assess(&s, &t());
        assert_eq!(v, Verdict::DispatchStuck);
        assert!(!v.recoverable_in_place());
        assert!(v.warrants_restart());
    }

    #[test]
    fn a_telegram_outage_is_reported_and_never_restarted_for() {
        let mut s = healthy();
        s.queue_depth = 12;
        s.send_ok_ms = s.now_ms - 400_000;
        let v = assess(&s, &t());
        assert_eq!(v, Verdict::DeliveryStuck);
        assert!(
            !v.warrants_restart(),
            "restarting cannot fix Telegram being down"
        );
        // A queue with a healthy last send is fine (it is draining).
        let mut s = healthy();
        s.queue_depth = 12;
        assert_eq!(assess(&s, &t()), Verdict::Healthy);
    }

    #[tokio::test]
    async fn a_guard_marks_completion_even_when_the_handler_is_aborted() {
        let h = Arc::new(Health::new());
        let id = h.reserve("test", "unit");
        let h2 = Arc::clone(&h);
        let task = tokio::spawn(async move {
            let _g = h2.guard(id);
            // Never returns on its own.
            futures_util::future::pending::<()>().await;
        });
        h.attach_abort(id, task.abort_handle());
        // Let the task start and register.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(h.snapshot().in_flight, 1);
        assert_eq!(h.snapshot().completed, 0);
        let killed = h.abort_older_than(Duration::ZERO);
        assert_eq!(killed.len(), 1);
        assert!(killed[0].contains("test") && killed[0].contains("unit"));
        // The runtime polls the aborted task, drops its guard, and completion is
        // recorded — which is what lets the watchdog see recovery take effect.
        let _ = task.await;
        let s = h.snapshot();
        assert_eq!(s.in_flight, 0);
        assert_eq!(s.completed, 1);
        assert_eq!(s.aborted, 1);
    }

    #[test]
    fn the_in_flight_report_names_the_oldest_first_and_is_bounded() {
        let h = Arc::new(Health::new());
        h.reserve("handler", "the-stuck-one");
        std::thread::sleep(Duration::from_millis(20));
        for i in 0..19 {
            h.reserve("handler", format!("session-{i}"));
        }
        let r = h.in_flight_report(5);
        assert_eq!(r.lines().count() - 1, 6, "5 rows plus the overflow line");
        assert!(r.contains("… and 15 more"));
        assert!(
            r.lines().nth(1).unwrap().contains("the-stuck-one"),
            "oldest first — the row an operator needs to see: {r}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_watchdog_cancels_a_stuck_handler_and_progress_resumes() {
        // End to end on the real thread: a handler that never returns, a watchdog
        // with a short window, and the daemon back to work — without a restart.
        let dir = tempfile::tempdir().unwrap();
        let health = Arc::new(Health::new());
        health.beat();

        let id = health.reserve("event", "agent_response stuck-1");
        let h2 = Arc::clone(&health);
        let stuck = tokio::spawn(async move {
            let _g = h2.guard(id);
            futures_util::future::pending::<()>().await;
        });
        health.attach_abort(id, stuck.abort_handle());

        // Keep the heartbeat landing, as the daemon's beat task does: the runtime is
        // healthy here; it is the handler that is not.
        let beating = Arc::clone(&health);
        let beat = tokio::spawn(async move {
            loop {
                beating.beat();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        Watchdog {
            health: Arc::clone(&health),
            config_dir: dir.path().to_path_buf(),
            thresholds: Thresholds {
                beat: Duration::from_secs(5),
                stall: Duration::from_millis(300),
                delivery: Duration::from_secs(60),
                check_every: Duration::from_millis(100),
            },
            max_restarts_per_hour: 5,
            may_restart: false,
        }
        .spawn();

        // The stuck handler is cancelled and counted as finished.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if health.snapshot().aborted > 0 && stuck.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let s = health.snapshot();
        assert_eq!(s.aborted, 1, "the watchdog cancelled the stuck handler");
        assert!(stuck.is_finished(), "and the task is actually gone");
        assert_eq!(s.in_flight, 0, "so its permit and locks are released");
        assert_eq!(s.completed, 1, "and it counts as finished");

        // A new handler runs to completion afterwards — the daemon is working again.
        let id2 = health.reserve("event", "after recovery");
        {
            let _g = health.guard(id2);
        }
        assert_eq!(health.snapshot().completed, 2);

        beat.abort();
    }

    #[test]
    fn the_restart_budget_and_marker_survive_the_process() {
        let dir = tempfile::tempdir().unwrap();
        assert!(restarts_last_hour(dir.path()).is_empty());
        record_restart(dir.path());
        record_restart(dir.path());
        assert_eq!(restarts_last_hour(dir.path()).len(), 2);
        // An old entry ages out.
        std::fs::write(
            dir.path().join(LEDGER),
            serde_json::to_string(&vec![chrono::Utc::now().timestamp() - 7200]).unwrap(),
        )
        .unwrap();
        assert!(restarts_last_hour(dir.path()).is_empty());

        let m = StallMarker {
            at: "2026-09-22T10:00:00Z".into(),
            reason: Verdict::HandlersStuck.reason().into(),
            in_flight: 50,
            received: 3400,
            dispatched: 3400,
            completed: 3350,
            restarted: true,
        };
        write_marker(dir.path(), &m);
        let read = read_marker(dir.path()).expect("marker is readable once");
        assert_eq!(read.in_flight, 50);
        assert!(read.restarted);
        assert!(
            read_marker(dir.path()).is_none(),
            "consumed, so the user is told exactly once"
        );
    }
}
