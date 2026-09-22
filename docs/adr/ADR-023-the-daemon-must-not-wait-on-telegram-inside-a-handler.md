# ADR-023: The daemon must never wait on Telegram inside a handler — and must notice when it stops making progress

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Implemented (2026-09-22)
**Date:** 2026-09-22
**Authors:** Robert, Claude
**Tags:** daemon, reliability, telegram, rate-limiting, watchdog
**Related:** ADR-011 (rate limiting), ADR-014 (priority queue, backpressure), ADR-019 (observed state, not assumed)

## Context

Reported, verbatim: *"ctm is dead now on this machine? I'm not seeing any updates from
my telegram app on phone — did we fuck something up?"* and then *"neither direction
working currently"*.

It was not dead. The daemon was running, its socket was accepting connections, and
hook events were arriving at **~80 per minute for 42 minutes** — during which **not one
handler logged anything**. No error, no warning, no Telegram call attempted. Every
thread in the process was parked. A `sample` of the live process named nothing, because
a parked async task has no OS stack to sample.

Two separate failures made this possible: one that caused it, and one that let it go
unnoticed for 42 minutes until the user happened to look at their phone.

## Spike — what the evidence actually said (2026-09-22)

From the daemon log, the session store, and a `sample` of the wedged process:

1. **The daemon restarted at 11:40** (SIGTERM from outside), re-announced four Codex
   sessions, and created their topics.
2. **Telegram rate-limited it** from 11:42: five `429`s with `retry_after` 19–43 s.
3. **The last handler output was at 11:46:45.** After that: nothing but
   `Socket client connected` / `disconnected`, 70–94 per minute, for 42 minutes. Those
   lines come from the socket layer, which takes no lock the handlers need — so events
   were being *accepted* and never *handled*.
4. **The session store confirms it**: a hook event fed in at 12:27 did not update any
   row until the daemon was restarted.
5. **All 20 threads were parked** (`__psynch_cvwait`), one in `kevent`. No thread was in
   a syscall, in SQLite, or on a file lock. So: not a blocking-code deadlock, not the
   database (`busy_timeout` is 5 s and would have errored), not the clients map (the
   connect/disconnect lines prove that lock was free).

**Reformulated hypothesis:** every one of the 50 handler permits was held by a task
that was *politely asleep inside Telegram's `retry_after`*, and the 51st event onward
had nowhere to run.

**Confirmed by reproduction.** `CTM_TELEGRAM_API_BASE` (new) points the bot at a
stand-in Telegram; `tests/daemon_stall.rs` runs the real daemon — real socket, real
store, real event loop — against a Telegram that answers `429 retry_after: 40` to
everything, while 60 sessions announce and events pour in. Against 0.2.53 the run ends:

```
received=180  dispatched=180  completed=0  in_flight=180  free_permits=0
in flight:  60.0s event session_start … (and 160 more)
```

Nothing completed. That is the incident, reproduced in sixty seconds.

**The arithmetic.** `handle_session_start` → `create_forum_topic_resilient`
(4 attempts, 1+2+4 s backoff) → `create_forum_topic` → `api_call`, which on a 429 slept
`retry_after` and retried **three times**: 3 × 40 s = 120 s per attempt, ×4 attempts ≈
**8 minutes of one handler permit for one topic**. Meanwhile `send_message` →
`enqueue` → `process_queue` made the *calling handler* drain the entire queue,
inheriting every `retry_after` pause in it. At 80 events/min the 50 permits were gone
in under a minute, and inbound Telegram updates were starved too — they share the same
pool. Nothing was logged because nothing had failed.

## Decision

### 1. A handler may never wait on Telegram for an unbounded time

Telegram is slow, third-party and rate-limited; the handler pool is small and shared by
both directions. Coupling them was the defect.

- **Every API call carries a 20 s budget** covering all of its retries
  (`CALL_BUDGET`). A `429` is absorbed only if the wait fits inside it; past that the
  call returns `RateLimited { retry_after_secs }` to its caller **without waiting**.
- **`create_forum_topic_resilient` no longer retries a rate limit.** Its retries exist
  for transient network faults. A `429` is returned at once, and the caller treats it
  exactly like "the topic is not ready yet" — the event is buffered by the existing
  no-silent-loss path (ADR-014 BUG-002) and flushed when the topic exists.
- **The queue drains in a task of its own** (`run_queue_drainer`, started by the
  daemon). `enqueue` now does what its name says: push, wake, return. The drainer is
  the *only* place allowed to sleep out a `retry_after`, and it holds no permit and
  blocks nobody.
- **No lock is held across a Telegram call.** Four sites held `injector.lock()` across
  the reply that follows an injection; tmux injection is synchronous and instant, so
  the guard is now scoped to it. `tests/lock_discipline.rs` scans the source and fails
  if the shape returns — the audit that found these, kept.

Under a total Telegram outage the daemon now handles events at full speed; messages
accumulate in the bounded, priority-shedding queue and go out when Telegram recovers.

### 2. The daemon watches itself, and recovers itself

The user should never be the monitoring system. `daemon/health.rs`:

- **Progress is defined and counted**: events received (at the socket, before any
  handler exists), handlers dispatched, handlers completed (through a guard that runs
  on drop, so a panicked or cancelled handler counts too), the last successful send,
  the queue depth, free permits, and an in-flight registry of what is running and for
  how long.
- **The watchdog is a plain OS thread**, not a task, so it still runs when no tokio
  worker can — the one failure a watchdog living inside the runtime could never report.
- **Four verdicts, one pure function** (`assess`, fully unit-tested): `RuntimeWedged`
  (the heartbeat stopped), `HandlersStuck` (work in hand, nothing finishing),
  `DispatchStuck` (events arriving, none dispatched — the blind spot a handler-only
  rule would have), `DeliveryStuck` (queued messages, nothing going out). An idle
  daemon is healthy however long it has been quiet; that distinction is the whole
  design.
- **Recovery, in order.** Log the in-flight table — the line this incident lacked —
  then **cancel the stuck handlers**: dropping their futures releases their locks and
  permits, which fixes a deadlock *in place*, keeping the process and its sessions.
  Only if progress does not resume does the daemon **end its own process** with exit
  75, which launchd (`KeepAlive`, `ThrottleInterval` 10 s) and systemd
  (`Restart=on-failure`, `RestartSec=10s`) turn into a restart — both verified by
  running them, per ADR-019. A budget of 5 restarts/hour prevents a crash loop; past it
  the daemon stays up, keeps recovering in place, and says so.
- **The user is told.** The stalled process leaves a marker; the next start sends one
  Telegram message naming what happened and when. `CTM_WATCHDOG=0` disables the whole
  thing.

### 3. Telegram's own failure modes are testable from now on

`CTM_TELEGRAM_API_BASE` overrides the API origin (production default unchanged). The
inputs that break this daemon come from Telegram, and until now none of them could be
written down as a test. Two now exist and run in CI: rate-limit saturation, and deleted
topics with slow replies.

## Consequences

- A Telegram outage or rate limit costs the daemon **no handler capacity**: the
  reproduction ends `received=180 completed=180 in_flight=0 free_permits=50`.
- A stall of any other cause is detected within ~2 minutes, its in-flight table logged,
  its handlers cancelled, and — if that does not take — the process replaced, with one
  Telegram message explaining it. The user learns from their phone, not from `sample`.
- Messages are delayed, never lost, while Telegram is refusing: they sit in the bounded
  queue, which sheds oldest-Low-first under pressure exactly as before.
- `send_message` now returns as soon as the message is queued. It never blocked
  *usefully* before — it blocked on whatever backlog happened to exist — but anything
  wanting delivery confirmation must use the direct, id-returning calls, as the
  approval and question widgets already do.
