# ADR-011: Resilience Architecture — Rate Limiting, Backpressure, and Failure Recovery

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Proposed
**Date:** 2026-03-17
**Authors:** Robert E. Lee, with BMAD multi-agent analysis
**Supersedes:** None
**Related:** ADR-006 (Rust Migration Gap Audit), ADR-010 (Deep Release Readiness Evaluation)

---

## Context

### Incident: 2026-03-17 — Silent Message Loss

During a live session on 2026-03-17, bidirectional mirroring degraded for approximately
15 minutes. Claude-to-Telegram (outbound) continued functioning, but Telegram-to-Claude
(inbound) silently dropped all messages. The user had no indication that their input was
not reaching Claude Code.

**Root cause chain:**

1. Telegram API returned transient errors on `getUpdates` (long-poll) starting at ~13:09 UTC-7.
2. The daemon's HTTP client has a 30-second timeout — identical to the Telegram long-poll
   `timeout` parameter. Network latency caused the HTTP client to timeout before Telegram
   responded, creating a race condition.
3. On `getUpdates` failure, the daemon sleeps 5 seconds and retries with no exponential
   backoff, hammering the already-struggling API every 5 seconds.
4. Concurrently, outbound messages hit Telegram's 429 rate limit with `retry_after` values
   escalating from 12 to 42 seconds. The daemon's retry logic uses fixed 2s/4s/8s backoff,
   ignoring `retry_after` entirely. Each retry worsened the rate limit penalty.
5. During the rate-limit storm, `sendMessage` calls with valid `message_thread_id` values
   returned "Bad Request: message thread not found" (the topic existed but Telegram's
   internal state was inconsistent under load). This error is not handled in `send_item` —
   it falls through to generic retry, fails 3 times, then `ensure_session_exists` creates
   a duplicate topic.
6. The message queue filled to its 500-message cap and began silently dropping oldest
   messages with no backpressure signal to the socket clients and no user-visible notification.

**Observed `retry_after` escalation:** 12 → 16 → 18 → 20 → 21 → 25 → 27 → 35 → 40 → 42 seconds. Telegram's rate limiter escalates penalties when the client continues hitting it — our fixed backoff made each retry worsen the situation.

### Systematic Audit

A comprehensive codebase audit identified **66 reliability issues** across the Rust
codebase:

| Severity | Count | Examples |
|----------|-------|----------|
| Critical | 9 | HTTP timeout race, queue silent drops, unbounded task spawning |
| High | 19 | Retry ignores `retry_after`, rate limiter vs retry fight, cache growth |
| Medium | 28 | Lock ordering violations, TOCTOU races, no poll backoff |
| Low | 10 | Silent error swallowing, stale cache entries |

### Research: State of the Art (March 2026)

Three parallel deep-research agents investigated Telegram Bot API specifics, Rust
resilience patterns, and the full codebase. Key findings:

#### Telegram Rate Limits (API Layer 167+, Bot API 8.0)

Telegram intentionally does not publish exact limits. Community consensus from empirical
data (2.3M request corpus, December 2025):

| Scope | Limit | Notes |
|-------|-------|-------|
| Single private chat | ~1 msg/sec | Brief bursts tolerated |
| Single group chat | 20 msg/min | Hard limit for bots in groups |
| Broadcast (different chats) | ~30 msg/sec | Global cap across all `send*` methods |
| `editMessage` | 20/sec burst, 600/30s sustained | Separate bucket from sends |
| `answerCallbackQuery` | 60/sec burst | Own bucket |

**Critical:** As of Bot API 8.0, limits are **per-method buckets**, not a single global
counter. `sendMessage` and `editMessage` have separate buckets.

**Critical:** During a `retry_after` window, the bot is **completely blocked globally**.
No API calls succeed — not just to the rate-limited chat, but for that entire bot token.
Ignoring `retry_after` triggers deterministic IP bans lasting 900 seconds.

429 responses include:
- `parameters.retry_after` — integer seconds (range 0-60s observed)
- `parameters.adaptive_retry` — milliseconds, capped exponential curve (Bot API 8.0+,
  November 2025). More granular than `retry_after`.

#### Long Polling

- HTTP client timeout **must be strictly greater** than `getUpdates` timeout parameter.
  Minimum 5s buffer recommended; 10-15s for safety.
- On connection drop, Telegram does NOT mark updates as delivered. Next `getUpdates`
  returns them again (at-least-once semantics).
- Telegram rejects concurrent `getUpdates` from the same bot token. Only one polling
  instance can operate at a time. This makes long polling a **single point of failure**.
- Recommended reconnection: 5 attempts, 1s min delay, 10s max delay, 0.3 jitter factor.

#### Forum Topic Failure Modes

| Error | Recovery |
|-------|----------|
| `TOPIC_CLOSED` (400) | Reopen via `reopenForumTopic` (requires admin + `can_manage_topics`) |
| `TOPIC_ID_INVALID` (400) | Topic permanently deleted. Clear cached thread_id, create new topic. |
| "message thread not found" (400) | Stale thread_id, forum mode toggled, or General topic (ID=1) passed explicitly. Clear cache. |
| General topic (ID=1) | **Never pass `message_thread_id=1` explicitly.** Omit the parameter entirely. |

Additional: when a forum exceeds 1 million messages, older topics become archived and
inaccessible. Bot API 9.3 improved this.

#### Webhook vs Long Polling — Clear Winner

Community consensus as of 2026 is **unambiguous: webhooks for production**. Long polling
is appropriate only for local development and simple single-instance bots.

| Dimension | Long Polling | Webhook |
|-----------|-------------|---------|
| Latency | Up to 30s delay | Instant push |
| Idle resources | Constant HTTP calls | Zero when idle |
| Horizontal scaling | Impossible (1 instance/token) | Load balancer across N pods |
| Max throughput | Single connection | Up to 100 simultaneous connections |
| Failure recovery | Updates queue until restart | Infrastructure handles retries |

**Implication for CTM:** A future major version should migrate from long polling to
webhooks. This is out of scope for this ADR but noted as the architectural direction.

#### Rust Resilience Ecosystem

- `circuitbreaker-rs` — lock-free FSM circuit breaker, no Tower dependency
- `governor` / `leaky-bucket` — static rate limiting (current: `governor`)
- Hand-rolled AIMD — adaptive rate control (recommended over crate dependency)
- `metrics` + `metrics-exporter-prometheus` — lightweight observability
- `tokio::sync::Semaphore` — bounded concurrency for spawned tasks
- `tokio::sync::watch` — broadcast circuit breaker / rate state to handlers

---

## Decision

Implement a nine-part resilience architecture across three sprints, prioritized by
user impact and implementation risk.

---

## Sprint 1: Emergency Fixes (Fixes #1–#4)

These four changes would have prevented the 2026-03-17 incident entirely.

### Fix #1: Separate HTTP Clients

**Problem:** Single `reqwest::Client` with 30s timeout for both short API calls
(`sendMessage`, ~200ms typical) and 30s long-poll (`getUpdates`). When Telegram takes 29s
to respond to `getUpdates` plus 1s network latency, the HTTP client times out at 30s
before the response arrives.

**Solution:** Two `reqwest::Client` instances on `TelegramBot`:

```rust
pub struct TelegramBot {
    /// For sendMessage, editMessage, createForumTopic, etc.
    /// 15s total timeout, 5s connect timeout.
    client: Client,

    /// For getUpdates long-polling only.
    /// 45s total timeout (30s Telegram timeout + 15s buffer).
    poll_client: Client,

    // ... existing fields unchanged
}
```

**Configuration rationale:**

| Client | Total timeout | Connect timeout | Pool idle | Keep-alive | Why |
|--------|-------------|-----------------|-----------|-----------|-----|
| `client` | 15s | 5s | 90s (default) | 60s | Telegram API calls complete in <2s normally. 15s catches slow responses without blocking the queue. |
| `poll_client` | 45s | 5s | 90s | 30s | 30s Telegram poll + 15s buffer for network latency, TLS negotiation, and server processing time. |

**Files changed:** `bot/client.rs` (~15 LOC)

**Why not per-request timeout override?** `RequestBuilder::timeout()` exists in reqwest,
but using separate clients also gives us independent connection pools. The long-poll
connection stays warm independently of the API connection pool. Under HTTP/2, each client
maintains its own multiplexed connection, preventing the long-poll from blocking API
request streams.

### Fix #2: Honor `retry_after` from 429 Responses

**Problem:** The retry logic in `queue.rs` uses fixed exponential backoff (2s, 4s, 8s)
regardless of what Telegram tells us. A 429 response with `retry_after: 42` gets retried
after 2 seconds, worsening the penalty.

**Solution:**

**Critical context from research:** During a `retry_after` window, the bot is completely
blocked globally — no API calls of any type succeed for that token. This means when we
get a 429, the entire queue must pause, not just the failed message. Continuing to send
other messages during the window will extend the penalty.

**Step 2a:** Add `parameters` field to `TgResponse`:

```rust
// bot/types.rs
#[derive(Debug, Deserialize)]
pub(super) struct TgResponse<T> {
    pub(super) ok: bool,
    pub(super) result: Option<T>,
    pub(super) description: Option<String>,
    pub(super) error_code: Option<i32>,
    pub(super) parameters: Option<ResponseParameters>,  // NEW
}

#[derive(Debug, Deserialize)]
pub(super) struct ResponseParameters {
    pub(super) retry_after: Option<u64>,
    /// Bot API 8.0+ (November 2025): millisecond-precision adaptive retry signal.
    /// More granular than retry_after. Use this when available, fall back to retry_after.
    #[serde(default)]
    pub(super) adaptive_retry: Option<u64>,
    #[serde(default)]
    pub(super) migrate_to_chat_id: Option<i64>,
}
```

**Step 2b:** In `send_item` (queue.rs), detect 429 and extract `retry_after`:

```rust
// After getting TgResponse...
if code == 429 {
    let wait = resp.parameters
        .as_ref()
        .and_then(|p| p.retry_after)
        .unwrap_or(30); // Conservative fallback

    tracing::warn!(
        retry_after = wait,
        "Telegram rate limited (429), honoring retry_after"
    );

    return Err(AppError::RateLimited { retry_after: wait });
}
```

**Step 2c:** In `process_queue`, handle `RateLimited` specially:

```rust
Err(AppError::RateLimited { retry_after_secs, adaptive_retry_ms }) => {
    // Don't count as a retry — rate limits aren't failures.
    let mut requeue = item.clone();
    // DO NOT increment retries — this is flow control, not an error.

    // Use adaptive_retry (ms) when available, fall back to retry_after (s).
    // Add 10% jitter to prevent thundering herd.
    let wait_ms = adaptive_retry_ms
        .unwrap_or(retry_after_secs * 1000);
    let jitter = (wait_ms as f64 * 0.1 * simple_jitter_fraction()) as u64;
    let total_wait = Duration::from_millis(wait_ms + jitter);

    tracing::warn!(
        retry_after_secs,
        adaptive_retry_ms = ?adaptive_retry_ms,
        total_wait_ms = total_wait.as_millis(),
        queue_depth = q_len,
        "429 rate limited — pausing ENTIRE queue (global block)"
    );

    // CRITICAL: During retry_after, ALL API calls fail for this token.
    // The entire queue must pause, not just this message.
    tokio::time::sleep(total_wait).await;

    let mut q = self.queue.lock().await;
    q.push_front(requeue);
}
```

**Critical design decisions:**

1. **429 responses do NOT count against the 3-retry limit.** Rate limiting is Telegram
   telling us to slow down — it's flow control, not an error. The message will eventually
   be sent. Only transport errors (network failure, timeout) count as retries.

2. **The entire queue pauses on 429.** Research confirmed that during `retry_after`, the
   bot token is globally blocked — not just for the rate-limited chat. Sending other
   messages during this window extends the penalty and risks a 900-second IP ban.

3. **Use `adaptive_retry` (ms) when available.** Bot API 8.0+ provides this
   millisecond-precision signal alongside the integer `retry_after` (seconds). The
   `adaptive_retry` value follows a capped exponential curve tuned by Telegram's servers.

**Files changed:** `bot/types.rs` (+10 LOC), `bot/queue.rs` (+25 LOC), `error.rs` (+5 LOC)

### Fix #3: Handle "message thread not found"

**Problem:** `send_item` handles `TOPIC_CLOSED` and `TOPIC_ID_INVALID` but not "message
thread not found". This error falls through to generic retry, fails 3 times, then
`ensure_session_exists` creates a duplicate topic with a new thread_id while the old
thread_id remains in the cache.

**Observed behavior:** During the 2026-03-17 incident, thread_id changed from 197153 to
197251 — a duplicate topic was created when the original was still valid.

**Solution:** Add a handler in `send_item` alongside the existing topic error handlers:

```rust
// "message thread not found" — topic was deleted or Telegram state is inconsistent.
// Don't retry — the thread_id is stale. Clear it and let ensure_session_exists
// create a new topic on the next message.
if code == 400 && desc.contains("message thread not found") {
    tracing::warn!(
        thread_id = ?item.thread_id,
        "Topic not found (deleted or stale), dropping message"
    );
    return Err(AppError::Telegram("Topic not found".into()));
}
```

**Why not retry?** Unlike `TOPIC_CLOSED` (which can be reopened), "message thread not
found" means the topic doesn't exist at all. Retrying with the same thread_id will fail
identically. The correct recovery is:

1. Let the message drop (it was going to a non-existent topic anyway).
2. The stale thread_id will be detected by `ensure_session_exists` on the next message.
3. A new topic is created cleanly, without a duplicate.

**Additional safeguard:** When this error is detected, proactively clear the stale
thread_id from the in-memory cache to prevent further messages from attempting the dead
topic:

```rust
// Proactively invalidate the stale thread_id.
// The next handler that calls get_thread_id() will fall through to
// ensure_session_exists() which creates a new topic.
if let Some(tid) = item.thread_id {
    // Notify daemon to clear this thread_id.
    // (Requires a callback mechanism or shared invalidation channel.)
}
```

**Open question:** The `send_item` method on `TelegramBot` doesn't have access to the
daemon's `session_threads` cache. Two options:

- **Option A:** Add an `Arc<RwLock<HashSet<i64>>>` "invalidated thread_ids" set to
  `TelegramBot`. The daemon checks this set in `get_thread_id()` and clears stale entries.
- **Option B:** Return a structured error (`TopicNotFound { thread_id }`) and let the
  daemon handler clear the cache when it observes this error.

**Recommendation:** Option B — it keeps the bot layer stateless and lets the daemon own
all session state management.

**Files changed:** `bot/queue.rs` (+10 LOC)

### Fix #4: Exponential Backoff with Jitter on Poll Failures

**Problem:** When `getUpdates` fails, the event loop sleeps 5s and retries immediately.
During a sustained outage, this hammers Telegram every 5 seconds with no backoff. The
audit found this contributes to the rate-limit cascade.

**Solution:** Track consecutive poll failures in the event loop. Apply capped exponential
backoff with jitter:

```rust
// In run_event_loop:
let mut consecutive_poll_failures: u32 = 0;

// In the Err(e) branch of get_updates:
Err(e) => {
    consecutive_poll_failures += 1;
    let base_delay = 5u64.saturating_mul(
        1u64 << consecutive_poll_failures.min(4)  // Cap at 80s
    );
    // Add jitter: ±20% to prevent thundering herd
    let jitter = (base_delay as f64 * 0.2 * rand_f64()) as u64;
    let delay = base_delay + jitter;

    tracing::error!(
        error = %e,
        consecutive_failures = consecutive_poll_failures,
        next_retry_secs = delay,
        "Failed to get Telegram updates"
    );
    tokio::time::sleep(Duration::from_secs(delay)).await;
}

// In the Ok(updates) branch:
Ok(updates) => {
    consecutive_poll_failures = 0;  // Reset on success
    // ... existing logic
}
```

**Backoff schedule:**

| Consecutive failures | Base delay | With jitter range |
|---------------------|-----------|-------------------|
| 1 | 10s | 10-12s |
| 2 | 20s | 20-24s |
| 3 | 40s | 40-48s |
| 4+ | 80s (cap) | 80-96s |

**Why cap at 80s?** Long-poll timeout is 30s. If we back off longer than ~90s, we
accumulate multiple update batches on Telegram's side. Telegram stores updates for up to
24 hours, so no data is lost — but the user experiences increased latency. 80s strikes the
balance between not hammering and not being too slow to recover.

**Jitter implementation:** Since we don't want to add `rand` as a dependency, use a
simple deterministic jitter based on the current timestamp:

```rust
fn simple_jitter_fraction() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos % 1000) as f64 / 1000.0  // 0.0..1.0
}
```

**Files changed:** `daemon/event_loop.rs` (+20 LOC)

---

## Sprint 2: Structural Resilience (Fixes #5–#7)

### Fix #5: Priority Message Queue

**Problem:** Single FIFO `VecDeque<QueuedMessage>`. Under load, time-critical approval
requests queue behind hundreds of verbose tool previews. The 500-message cap drops oldest
messages indiscriminately — an approval request is equally likely to be dropped as a debug
log line.

**Solution:** Replace the single `VecDeque` with a three-tier priority queue:

```rust
// bot/types.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum MessagePriority {
    Critical = 0,  // Drained first
    Normal = 1,
    Low = 2,       // Drained last, dropped first
}

// bot/queue.rs
struct PriorityMessageQueue {
    critical: VecDeque<QueuedMessage>,
    normal: VecDeque<QueuedMessage>,
    low: VecDeque<QueuedMessage>,
}

impl PriorityMessageQueue {
    const MAX_CRITICAL: usize = 50;    // Should never hit this
    const MAX_NORMAL: usize = 300;
    const MAX_LOW: usize = 150;

    fn enqueue(&mut self, msg: QueuedMessage) {
        let (queue, max) = match msg.priority {
            MessagePriority::Critical => (&mut self.critical, Self::MAX_CRITICAL),
            MessagePriority::Normal => (&mut self.normal, Self::MAX_NORMAL),
            MessagePriority::Low => (&mut self.low, Self::MAX_LOW),
        };
        if queue.len() >= max {
            let dropped = queue.pop_front();
            tracing::warn!(
                priority = ?msg.priority,
                dropped_text = ?dropped.map(|d| d.text.chars().take(50).collect::<String>()),
                "Queue full for priority level, dropping oldest"
            );
            metrics::counter!("ctm_queue_dropped_total",
                "priority" => format!("{:?}", msg.priority)).increment(1);
        }
        queue.push_back(msg);
    }

    fn pop_next(&mut self) -> Option<QueuedMessage> {
        self.critical.pop_front()
            .or_else(|| self.normal.pop_front())
            .or_else(|| self.low.pop_front())
    }

    fn total_len(&self) -> usize {
        self.critical.len() + self.normal.len() + self.low.len()
    }
}
```

**Priority assignment rules:**

| Priority | Message types | Rationale |
|----------|-------------|-----------|
| Critical | `ApprovalRequest`, `SessionStart`, `SessionEnd`, `Error`, approval callbacks | User-blocking or safety-critical. Approvals gate tool execution. |
| Normal | `AgentResponse`, `UserInput`, `ToolResult`, `SessionRename` | User-visible output that maintains conversation coherence. |
| Low | `ToolStart` (verbose previews), `PreCompact`, `TurnComplete` | Informational. Loss is tolerable without breaking UX. |

**The `QueuedMessage` struct** gains a `priority: MessagePriority` field. Priority is
determined at enqueue time by the daemon handler based on the `MessageType` of the source
`BridgeMessage`.

**Why not a BinaryHeap?** Three VecDeques give O(1) push and pop with trivial priority
logic. A BinaryHeap adds O(log n) overhead and doesn't preserve FIFO ordering within a
priority level (messages of the same priority would be arbitrarily reordered).

**Files changed:** `bot/types.rs` (+15 LOC), `bot/queue.rs` (+50 LOC), `bot/client.rs`
(+5 LOC to propagate priority), `daemon/socket_handlers.rs` (+20 LOC for priority
assignment)

### Fix #6: Bounded Task Spawning

**Problem:** The event loop spawns a new `tokio::spawn` for every socket message, Telegram
update, and cleanup tick without any concurrency limit. Under a burst of 1000 socket
messages, 1000 tasks are spawned simultaneously. Each task holds an `Arc<HandlerContext>`
with references to all shared state. Memory consumption is proportional to concurrent task
count with no upper bound.

**Solution:** Add a `tokio::sync::Semaphore` to bound concurrent handler tasks:

```rust
// In run_event_loop:
let handler_semaphore = Arc::new(Semaphore::new(50)); // Max 50 concurrent handlers

// In the socket message arm:
Ok(msg) => {
    let ctx = base_ctx.clone();
    let permit = handler_semaphore.clone();
    tokio::spawn(async move {
        let _permit = permit.acquire().await;
        handle_socket_message(ctx, msg).await;
    });
}
```

**Why 50?** Telegram's rate limit is ~30 msg/sec. Most handlers result in 1-2 API calls.
50 concurrent handlers provides headroom for slow handlers (database queries, file I/O)
without allowing unbounded growth. Under sustained load, the 51st message waits for a
permit — this is the backpressure mechanism.

**Cleanup tasks exempt:** Cleanup runs on a 5-minute interval and must not be starved.
Spawn it without acquiring a permit.

**Files changed:** `daemon/event_loop.rs` (+10 LOC)

### Fix #7: Cache Size Limits

**Problem:** Six `HashMap` caches in `DaemonState` grow without bound:
`session_threads`, `session_tmux_targets`, `tool_input_cache`, `session_custom_titles`,
`pending_questions`, `topic_creation_locks`. Over long-running daemon lifetimes (days,
weeks), these accumulate entries from ended sessions.

**Current cleanup:** The 5-minute cleanup cycle in `cleanup.rs` removes entries for ended
sessions, but only if the session was explicitly ended. Sessions that become stale without
an explicit `session_end` message leave orphaned cache entries.

**Solution:** Add a max-size check to each cache with LRU-style eviction. Rather than
adding an LRU crate dependency, leverage the existing cleanup cycle:

```rust
// In cleanup::run_cleanup, after existing stale session cleanup:

// Cache size limits — evict oldest entries if over threshold.
// These are conservative limits that should never be hit in normal operation.
const MAX_SESSION_THREADS: usize = 200;
const MAX_SESSION_TMUX: usize = 200;
const MAX_TOOL_CACHE: usize = 500;
const MAX_CUSTOM_TITLES: usize = 200;

// session_threads: evict entries not in active sessions
{
    let active_ids: HashSet<String> = ctx.db_op(|sess| {
        sess.get_active_sessions()
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.id)
            .collect()
    }).await;

    let mut threads = ctx.session_threads.write().await;
    if threads.len() > MAX_SESSION_THREADS {
        threads.retain(|k, _| active_ids.contains(k));
    }
}
// Repeat pattern for session_tmux, custom_titles
```

**Tool cache:** Already has TTL-based expiry via spawned tasks. Add a hard cap as a safety
net:

```rust
// In socket_handlers::handle_tool_start, before inserting:
{
    let mut cache = ctx.tool_cache.write().await;
    if cache.len() > MAX_TOOL_CACHE {
        // Remove oldest 20% by timestamp
        let cutoff = cache.values()
            .map(|v| v.timestamp)
            .min()
            .map(|t| t + Duration::from_secs(60));
        if let Some(cutoff) = cutoff {
            cache.retain(|_, v| v.timestamp > cutoff);
        }
    }
    cache.insert(tool_use_id.clone(), cached);
}
```

**Files changed:** `daemon/cleanup.rs` (+40 LOC), `daemon/socket_handlers.rs` (+10 LOC)

---

## Sprint 3: Adaptive Control (Fixes #8–#9)

### Fix #8: AIMD Adaptive Rate Control

**Problem:** The `governor` rate limiter uses a static quota (configurable, default 20
msg/sec). This is correct as a ceiling but doesn't adapt to Telegram's dynamic per-chat
rate limits. When Telegram throttles us (429), we need to reduce our send rate — not just
retry the failed message.

**Research finding:** AIMD (Additive Increase, Multiplicative Decrease) is the correct
pattern. It's the same algorithm that drives TCP congestion control, and it's been
validated for API rate limiting by production systems (Vector, Envoy, AWS SDK).

**Solution:** Add an `AimdState` to `TelegramBot` that adjusts the effective send rate:

```rust
struct AimdState {
    /// Current effective send rate (messages per second).
    rate: f64,
    /// Floor rate — never go below this.
    min_rate: f64,      // 0.5 msg/sec
    /// Ceiling rate — never exceed this (from config.rate_limit).
    max_rate: f64,      // 20-30 msg/sec
    /// Additive increase per successful send.
    increase: f64,      // 0.5 msg/sec
    /// Multiplicative decrease factor on 429.
    decrease_factor: f64, // 0.5 (halve the rate)
    /// Last 429 timestamp for debouncing.
    last_decrease: Option<Instant>,
}

impl AimdState {
    fn on_success(&mut self) {
        self.rate = (self.rate + self.increase).min(self.max_rate);
    }

    fn on_rate_limit(&mut self, retry_after: u64) {
        // Debounce: don't decrease more than once per second
        if self.last_decrease.map(|t| t.elapsed() < Duration::from_secs(1)).unwrap_or(false) {
            return;
        }
        self.rate = (self.rate * self.decrease_factor).max(self.min_rate);
        self.last_decrease = Some(Instant::now());
    }

    fn inter_message_delay(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.rate)
    }
}
```

**Integration with queue processing:** Replace the `rate_limiter.until_ready().await`
call in `api_call` with a combined check:

```rust
// Wait for AIMD-adjusted delay
tokio::time::sleep(self.aimd.lock().await.inter_message_delay()).await;
// Then wait for governor ceiling check
self.rate_limiter.until_ready().await;
```

This provides two layers: AIMD adapts the effective rate downward during throttling, while
`governor` enforces the absolute ceiling.

**Recovery behavior:** After a 429 with `retry_after: 42s`, the rate drops from (e.g.)
20 → 10 msg/sec. Then it increases by 0.5 msg/sec per successful send, reaching 20 again
after 20 successful messages (~2 seconds at the 10 msg/sec rate). This is fast enough to
recover but slow enough to avoid immediately re-triggering the rate limit.

**Files changed:** `bot/client.rs` (+50 LOC), `bot/queue.rs` (+15 LOC)

### Fix #9: Observability

**Problem:** No metrics, no health endpoint. The only way to detect degradation is to read
daemon logs. During the 2026-03-17 incident, the user had no indication that messages were
being dropped for 15 minutes.

**Solution:** Add the `metrics` facade crate with Prometheus exporter.

**Dependencies:**

```toml
# Cargo.toml
metrics = "0.24"
metrics-exporter-prometheus = "0.16"
```

**Initialization** (in `daemon::start`):

```rust
let builder = PrometheusBuilder::new();
let handle = builder
    .with_http_listener(([127, 0, 0, 1], 9091))
    .install()
    .expect("Failed to install metrics exporter");
```

**Key metrics to instrument:**

| Metric | Type | Labels | Location |
|--------|------|--------|----------|
| `ctm_queue_depth` | Gauge | `priority` | `queue.rs` — on enqueue/dequeue |
| `ctm_queue_dropped_total` | Counter | `priority`, `reason` | `queue.rs` — on overflow drop |
| `ctm_telegram_requests_total` | Counter | `method`, `status` | `client.rs` — after each API call |
| `ctm_telegram_request_duration_seconds` | Histogram | `method` | `client.rs` — around API call |
| `ctm_rate_limit_429_total` | Counter | — | `queue.rs` — on 429 detection |
| `ctm_rate_limiter_current_rate` | Gauge | — | `client.rs` — after AIMD adjustment |
| `ctm_poll_success_total` | Counter | — | `event_loop.rs` — on successful poll |
| `ctm_poll_error_total` | Counter | — | `event_loop.rs` — on failed poll |
| `ctm_poll_gap_seconds` | Gauge | — | `event_loop.rs` — time since last success |
| `ctm_socket_clients_connected` | Gauge | — | `socket.rs` — on connect/disconnect |
| `ctm_messages_injected_total` | Counter | `success` | `injector.rs` — on inject result |

**Alerting rules** (for users running Prometheus/Alertmanager):

```yaml
# Critical: polling has stopped working
- alert: CTMPollingStopped
  expr: ctm_poll_gap_seconds > 120
  for: 2m
  annotations:
    summary: "CTM has not received Telegram updates for {{ $value }}s"

# Warning: rate limited
- alert: CTMRateLimited
  expr: rate(ctm_rate_limit_429_total[5m]) > 0.1
  annotations:
    summary: "CTM is being rate limited by Telegram"

# Critical: messages dropping
- alert: CTMMessagesDropping
  expr: rate(ctm_queue_dropped_total[5m]) > 0
  annotations:
    summary: "CTM is dropping messages due to queue overflow"
```

**Health endpoint:** The Prometheus exporter already serves `/metrics` on port 9091.
Add a simple `/health` endpoint that returns 200 if the daemon is running and the last
poll succeeded within 120 seconds:

```rust
// Simple health: check last_poll_success timestamp
if last_poll_success.elapsed() < Duration::from_secs(120) {
    "OK"
} else {
    "DEGRADED: polling stale"
}
```

**Files changed:** `Cargo.toml` (+2 deps), `main.rs` / `daemon/mod.rs` (+20 LOC init),
scattered instrumentation (+80 LOC across 6 files)

---

## Implementation Order and Risk Assessment

| Fix | Sprint | LOC | Risk | Prevents |
|-----|--------|-----|------|----------|
| #1 Separate HTTP clients | 1 | ~15 | Low | Timeout race (CRITICAL) |
| #2 Honor `retry_after` | 1 | ~40 | Low | Rate-limit cascade (CRITICAL) |
| #3 Thread-not-found handler | 1 | ~10 | Low | Topic duplication (HIGH) |
| #4 Poll backoff with jitter | 1 | ~20 | Low | Poll hammering (MEDIUM) |
| #5 Priority queue | 2 | ~90 | Medium | Approval loss under load (HIGH) |
| #6 Bounded task spawning | 2 | ~10 | Low | OOM under burst (CRITICAL) |
| #7 Cache size limits | 2 | ~50 | Low | Memory leak (HIGH) |
| #8 AIMD rate control | 3 | ~65 | Medium | Future rate-limit storms (MEDIUM) |
| #9 Observability | 3 | ~100 | Low | Blind degradation (HIGH) |

**Total estimated LOC:** ~400 across all three sprints.

---

## Future: Webhook Migration (Out of Scope)

Research conclusively shows webhooks are the SOTA for production Telegram bots. Long
polling is a single point of failure (one instance per token, no horizontal scaling,
updates queue on crash). A future ADR should evaluate migrating CTM from long polling
to webhooks.

**Key considerations for the webhook migration:**
- Requires HTTPS endpoint (ports 443/80/88/8443) with valid SSL cert
- `secret_token` for authenticating incoming requests
- Handler must respond within ~10s or Telegram re-delivers the update
- Enables horizontal scaling (up to 100 simultaneous connections per bot)
- Eliminates the HTTP timeout race entirely (no long-poll to timeout on)
- Requires architectural shift: currently the daemon pulls updates; with webhooks,
  an HTTP server receives pushes

**This is the correct long-term direction** but represents a significant architectural
change beyond the scope of this resilience ADR.

---

## Bugs Found During Investigation

### BUG: Doctor checks wrong database filename

**File:** `doctor.rs:601`
**Severity:** Low (diagnostic only — does not affect runtime behavior)
**Status:** Fix applied locally, pending build + test + release

The `check_database()` function looked for `bridge.db` but the actual database file
created by `session.rs:107` is `sessions.db`. This caused `ctm doctor` to always report
"Database file not found" even when the database was present and functioning.

```rust
// Before (wrong):
let db_path = config_dir().join("bridge.db");

// After (correct):
let db_path = config_dir().join("sessions.db");
```

A full codebase mismatch audit confirmed this was the **only** naming inconsistency.
All other file names, environment variables, config keys, API method names, and
serialization formats are consistent across the codebase.

**Action required:**
1. Build: `cd rust-crates && cargo build --release`
2. Run tests: `cargo test`
3. Verify fix: `ctm doctor --fix` should now show `[10/10] OK: Database: OK (N sessions)`
4. Bump patch version and release to npm so existing users get the fix

---

## What This ADR Does NOT Cover

The following issues from the audit are acknowledged but deferred:

1. **Circuit breaker pattern:** Considered but deferred. AIMD + `retry_after` handling
   provides the critical adaptive behavior. A circuit breaker adds value for sustained
   multi-minute outages but adds complexity. Revisit if AIMD proves insufficient.

2. **Backpressure to socket clients:** The bounded semaphore (Fix #6) provides indirect
   backpressure by slowing handler processing. Full backpressure propagation (replacing
   `broadcast` with bounded `mpsc`, signaling socket writers) is deferred to a future ADR.

3. **Persistent deletion queue:** Topic deletions are scheduled in-memory and lost on
   daemon crash. The existing cleanup cycle recovers orphaned topics within 5 minutes.
   Persisting the deletion queue to SQLite is a correctness improvement but not urgent.

4. **Lock ordering enforcement:** The audit identified potential lock ordering violations.
   Current code rarely holds multiple locks simultaneously, and all lock acquisitions are
   short-lived. A formal lock ordering audit with `#[must_use]` guard types is deferred.

5. **20+ silently swallowed errors:** Many `let _ = ...` patterns suppress error
   information. A systematic pass to convert these to `tracing::debug!` calls is
   worthwhile but orthogonal to the resilience architecture.

---

## Consequences

### Positive

- **No more silent message loss.** Priority queue ensures critical messages survive load.
  Observability metrics make degradation visible.
- **Self-healing under rate limits.** AIMD + `retry_after` honoring means the system
  automatically finds the sustainable send rate instead of cascading into worse throttling.
- **Faster recovery from outages.** Exponential backoff on poll failures prevents
  hammering a recovering API. Separate HTTP clients prevent long-poll timeouts from
  affecting API calls.
- **No duplicate topics.** Proper handling of "message thread not found" prevents the
  create-new-topic-on-error pattern.
- **Bounded resource consumption.** Semaphore limits concurrent tasks. Cache limits prevent
  unbounded memory growth.

### Negative

- **Two new dependencies** (`metrics`, `metrics-exporter-prometheus`) for Sprint 3.
  Both are well-maintained, widely used in the Rust ecosystem.
- **Port 9091 required** for the Prometheus metrics endpoint. Must be documented and
  configurable.
- **Slight latency increase** during rate-limit recovery as AIMD ramps up. Acceptable
  tradeoff — the alternative is a cascading failure.
- **Complexity increase** in the queue and bot modules. Mitigated by comprehensive
  tests for each new behavior.

### Neutral

- **No API changes.** All fixes are internal to the daemon. The Unix socket protocol,
  hook interface, and Telegram bot commands are unchanged.
- **No configuration changes required** for Sprint 1 fixes. Sprint 2-3 add optional
  configuration (metrics port, cache limits) with sensible defaults.

---

## Testing Strategy

### Unit Tests (per fix)

| Fix | Test |
|-----|------|
| #1 | Verify two clients created with different timeouts. Mock test that poll_client timeout > client timeout. |
| #2 | Parse 429 response with `retry_after` field. Verify `RateLimited` error returned. Verify retry count NOT incremented. |
| #3 | Parse "message thread not found" response. Verify message dropped (not retried). |
| #4 | Verify backoff schedule: 10s, 20s, 40s, 80s cap. Verify reset on success. |
| #5 | Priority queue drains critical before normal before low. Overflow drops lowest priority first. FIFO within priority. |
| #6 | Semaphore limits concurrent handlers to 50. 51st task waits for permit. |
| #7 | Cache eviction fires when size exceeds threshold. Active sessions retained. |
| #8 | AIMD rate increases on success, halves on 429. Rate clamped to [min, max]. Debounce prevents double-decrease. |
| #9 | Metrics increment on expected events. Prometheus endpoint serves valid exposition format. |

### Integration Tests

- **Rate limit simulation:** Send 100 messages in 1 second. Verify queue doesn't drop
  critical messages. Verify AIMD reduces rate. Verify recovery to full rate after
  throttling ends.
- **Poll failure recovery:** Mock `getUpdates` to fail N times. Verify backoff delays
  match expected schedule. Verify recovery on first success.
- **Topic lifecycle:** Create session, delete topic externally, send message. Verify new
  topic created cleanly without duplicate.

---

## References

### Telegram Bot API
- [Telegram Bot API Official Documentation](https://core.telegram.org/bots/api)
- [Telegram Bot FAQ — Rate Limits](https://core.telegram.org/bots/faq#my-bot-is-hitting-limits-how-do-i-avoid-this)
- [Telegram Forum Topics API](https://core.telegram.org/api/forum)
- [Telegram API Error Handling](https://core.telegram.org/api/errors)

### Rate Limits & Flood Control
- [GramIO Rate Limits — Empirical Data](https://gramio.dev/rate-limits)
- [grammY Flood Control Guide](https://grammy.dev/advanced/flood)
- [BullMQ Rate Limiting for Telegram](https://docs.bullmq.io/guide/rate-limiting)

### Webhook vs Long Polling
- [grammY Deployment Types](https://grammy.dev/guide/deployment-types)
- [GramIO Webhook vs Long Polling](https://gramio.dev/updates/webhook)

### Forum Topic Issues
- [python-telegram-bot #4739 — message_thread_id=1 fails](https://github.com/python-telegram-bot/python-telegram-bot/issues/4739)
- [tdlib/telegram-bot-api #596 — Forward Thread Not Found](https://github.com/tdlib/telegram-bot-api/issues/596)
- [tdlib/telegram-bot-api #447 — Wrong message_thread_id](https://github.com/tdlib/telegram-bot-api/issues/447)

### Rust Resilience Patterns
- [circuitbreaker-rs](https://github.com/copyleftdev/circuitbreaker-rs)
- [rate_limiter_aimd](https://docs.rs/rate_limiter_aimd/latest/rate_limiter_aimd/)
- [metrics crate ecosystem](https://rustprojectprimer.com/ecosystem/metrics.html)
- [reqwest per-request timeouts](https://docs.rs/reqwest/latest/reqwest/struct.RequestBuilder.html#method.timeout)
- [AIMD for API Rate Limiting — Vector Blog](https://vector.dev/blog/adaptive-request-concurrency/)
- [Backpressure in Rust Async Systems](https://www.slingacademy.com/article/handling-backpressure-in-rust-async-systems-with-bounded-channels/)
- [Rust Observability with OpenTelemetry](https://dasroot.net/posts/2026/01/rust-observability-opentelemetry-tokio/)
