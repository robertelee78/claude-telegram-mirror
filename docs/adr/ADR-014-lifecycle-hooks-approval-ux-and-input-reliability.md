# ADR-014: Lifecycle Hooks, Approval UX, and Human-Input Reliability

**Status:** accepted — scope finalized via deep-research, a verification spike, and a per-work-item product interview (2026-05-28). **All PRs (A, E, B, D) landed 2026-05-28** on `feat/adr-014-lifecycle-approvals` (see Implementation log). PR-C was removed per the interview. **Post-release field-defect fixes (D1–D8 + cleanup) landed 2026-05-31** on `fix/adr-014-askuserquestion-bypass-and-render` after a field report that AskUserQuestion never reached Telegram under `--dangerously-skip-permissions` — see the 2026-05-31 implementation-log entry.
**Date:** 2026-05-28
**Authors:** Robert, Claude
**Tags:** hooks, session-lifecycle, approvals, askuserquestion, telegram-ux, onboarding, channels

DO NOT BE LAZY. We have plenty of time to do it right. No short cuts. Never make assumptions. Always dive deep and ensure you know the problem you're solving. Make use of search as needed. Measure 3x, cut once. No fallback. No stub (todo later) code. Just pure excellence, done the right way the entire time. Also recall Chesterton's fence; always understand current fully before changing it.

## Context

A deep-research campaign (codebase analysis + SOTA web research as of May 2026, cross-referenced and adversarially verified) examined three questions: what this repo is, which Claude Code hooks it should care about, and what to improve. A product-level interview then reframed priorities against the actual deployment reality:

- **Threat model (decided):** Solo use primarily (#1), must work for a small trusted group (#2). Semi-trusted/larger groups (#3) are **explicitly not supported**. The operating assumption is that **anyone with channel access already has git-commit access**. Therefore per-user (`from.id`) authorization is *out of scope by design* — the correct mitigation is documentation, not code.
- **Audience:** Built for self; has other users; broad adoption is welcome but the priority is **solid onboarding**, not hand-holding.
- **Real pain reported (outranks the theoretical audit):** (a) stale/orphaned topics, (b) approval/button glitches, (c) the experience when Claude needs input from the user (AskUserQuestion).
- **Goal for this work:** *Fix what's real* + *UX polish*. Not security-first, not hardening-toward-1.0 for its own sake.

### Strategic finding that frames this ADR — Claude Code "Channels"

As of ~March 2026, Anthropic shipped **Channels** (research preview, Claude Code ≥ v2.1.81): an official structured permission-relay protocol that mirrors approvals/questions to Telegram/Discord/iMessage **over MCP notifications with correlation IDs — not tmux keystroke injection** — with open-source reference implementations in `anthropics/claude-plugins-official`. This overlaps with `ctm`'s purpose and, crucially, **structurally solves the most fragile part of `ctm` (AskUserQuestion answer injection via keystrokes).**

A verification spike (see "Spike result" below) found that solving `ctm`'s most fragile path does **not** require migrating to Channels — the standard `PreToolUse` + `updatedInput` hook return already does it. So the fork is narrower than first thought:
- **Path A (chosen):** keep the hook + tmux architecture and fix the real bugs, **including a structured (no-keystroke) AskUserQuestion answer path via `updatedInput`**. Works on any plain tmux setup; preserves `ctm`'s independent value.
- **Path B (deferred, optional future):** migrate wholesale to Anthropic's Channels protocol. Higher ceiling but overlaps Anthropic's official offering and rides on a research-preview protocol that may change. **Not required** for any reliability win in this ADR.

### Spike result (resolved 2026-05-28) — structured AskUserQuestion answers without keystrokes

Verified via official docs **and** by reading `jsayubi/ccgram`'s production source:

- A `PreToolUse` hook matched on `AskUserQuestion` can **return the user's answer structurally** by emitting `hookSpecificOutput.permissionDecision: "allow"` with `updatedInput` carrying the original `questions` plus an `answers` map (question-text → selected label; multi-select labels comma-joined). Claude then proceeds as if the user answered — **no TUI, no keystrokes.**
- `ccgram` migrated *off* keystroke injection to exactly this mechanism (its CHANGELOG documents removing the AppleScript/`tmux send-keys` question path). Proven production behavior, not theory.
- **Caveat — free-text:** there is **no** structured path for free-text answers. `ccgram` still routes those through terminal injection. `ctm` must either keep keystroke injection for the free-text case only, or accept that limitation.
- **Caveat — refactor shape:** to use this, `ctm`'s hook must *block-and-return* for `AskUserQuestion` (the same `send_and_wait` correlation pattern the approval flow already uses), returning `updatedInput` instead of injecting keys daemon-side. The ADR-012 tentative-selection Telegram UI for *collecting* the answer is retained; only the *delivery* changes.

### Verified current-state facts (with code locations)

- **Installer registers only 6 hooks** (`installer.rs:14-21`): `PreToolUse, PostToolUse, Notification, Stop, UserPromptSubmit, PreCompact`.
- **`SessionEnd` is fully implemented but never registered.** The `SessionEndEvent` type (`types.rs:137`), hook arm (`hook.rs:481`), and daemon handler (`socket_handlers.rs:359`) all exist — but the installer omits `SessionEnd`, so the handler is **dead code today**. Session teardown leans on the 5-minute stale-cleanup timer.
- **Field-name bug:** `SessionEndEvent` deserializes `reason` (`types.rs:140`), but Claude Code emits **`session_exit_reason`** (confirmed against official docs). The reason would always be `None` even after registration.
- **`session_exit_reason` includes `resume`** — meaning a `SessionEnd` can fire for a session that is *coming back*. Tearing down unconditionally on every `SessionEnd` would wrongly destroy a resuming session's topic.
- **`adaptive_retry` is dead code** built on a fabricated Telegram API field (`queue.rs:148/160/251`, `types.rs:73`, `error.rs:48`). It is always `None`; the `retry_after` fallback is the only live path. Verified against the official Bot API changelog — no such field exists.
- **Approvals already use the structured hook path** (`permissionDecision: allow/deny` via `send_and_wait`) — no keystroke injection. But: approval sends do **not** use Critical queue priority (a code comment at `socket_handlers.rs:737` notes this is pending), `resolve_approval` success is not checked before replying (double-tap → double response), `pending_approval_clients` is **never evicted** (`socket_handlers.rs:722` insert; only removed on button tap), and buttons go stale after a daemon restart (in-memory map lost; DB persists → broadcast fallback).
- **AskUserQuestion is the only flow forced into keystroke injection** (ADR-012). There is **no dedicated hook for AskUserQuestion** and Elicitation does not fire for it (GitHub anthropics/claude-code#44326), which is *why* keystrokes are used. The multi-select injection is mathematically fragile: `auto_submit_answers` sleeps **2s** (`callback_handlers.rs:1426`) but a multi-select with several options takes **~3s** of 300ms-spaced keystrokes (`callback_handlers.rs:1196-1217`), so the confirming Enter can fire before Claude's review screen exists. `msg_id == 0` fallback silently hides render failures. `tmux send-keys` is independently documented-unreliable (bracketed-paste swallows the trailing Enter; readiness races).
- **`session_start` is synthesized on every hook invocation** (`hook.rs:282`); the real `SessionStart` `source`/`sessionTitle` is unused. `editForumTopic` exists (`bot/client.rs:497`); **`setMessageReaction` does not exist** in the bot client.
- **Authorization is chat-level only** (`telegram_handlers.rs:12`, ~10 sites in `callback_handlers.rs`) — by design, per the threat model above.

## Decision

Execute Path A as four sequenced PRs. Each item lists root cause, the change, and verified code anchors. No stubs; each PR ships complete with tests.

> **Interview decisions (2026-05-28, product-level):** topic teardown = **delete immediately** on true end (phone history not valued; the computer retains transcript history); stale-cleanup timer = **kept but at a longer interval**; trust acknowledgment = **new setups only**; approval buttons = **keep the existing 4** (no "Always"); approval timeout = **unchanged** (fall back to CLI — operator typically runs `--dangerously-skip-permissions`); resolved-message detail = **decision + time**; status reactions = **dropped**; auto-titles via SessionStart = **dropped** (keep `/rename` + transcript custom-title); free-text answers = **keep an isolated keystroke path**; AskUserQuestion = **keep the "Submit All" review step**. PR-C (UX reactions/titles) was fully declined and removed.

### PR-A — Lifecycle correctness, immediate teardown, resume robustness, dead-code removal

**A1. Register the `SessionEnd` hook.**
Add `"SessionEnd"` to `HOOK_TYPES` (`installer.rs:14-21`) via the standard `create_hook_entry()` (no special timeout, unlike `PreToolUse`). Activates the already-built handler so teardown is event-driven — directly addressing reported stale/orphaned topics.

**A2. Fix the `session_exit_reason` field.**
Add `#[serde(alias = "session_exit_reason")]` to the `reason` field on `SessionEndEvent` (`types.rs:140`). Add a test asserting deserialization from `session_exit_reason`.

**A3. Special-case `reason == "resume"`.**
In the `SessionEnd` path, do **not** tear down when the exit reason is `resume` (the session is suspended and will return). Treat only true terminations (`clear`, `logout`, `prompt_input_exit`, `bypass_permissions_disabled`, `other`) as teardown.

**A4. Delete the topic immediately on true SessionEnd (replaces two-stage close→delete).**
**Decision:** phone-side history is not valued; the computer retains transcript history. In `handle_session_end` (`socket_handlers.rs:359`), replace `schedule_topic_deletion` (the two-stage close-then-delete, `cleanup.rs:453-510`) with an immediate `delete_forum_topic`, then **synchronously** clear `thread_id` from **both** the DB (`clear_thread_id`, `session.rs:387`) and the `session_threads` cache — no fire-and-forget — before returning. Tolerate "topic already deleted" from the API. Remove the now-dead two-stage scheduling; `cancel_pending_topic_deletion` (`cleanup.rs:559`) becomes a safe no-op and is retained as a guard.

**A5. Make resume + new-topic creation robust (deep-dive findings).**
Because the topic is now always gone after a true end, a resuming session must always build a fresh topic cleanly (`ensure_session_exists`, `mod.rs:788-950`). Required guards (from the resume deep-dive):
- **Atomic clear:** A4's synchronous DB+cache clear closes the window where a late/concurrent resume reads a stale `thread_id` for an already-deleted topic.
- **Stale-id self-heal:** if a send ever targets a `thread_id` whose topic is gone (Telegram "topic not found"), null the `thread_id` and recreate the topic, then retry.
- **Persist the custom title to the DB.** Today the custom title lives only in the `custom_titles` in-memory cache, so a resume *after a daemon restart* loses the name. Persist it (e.g. `sessions.metadata` JSON or a column) so the resume topic name and the "Session resumed: {title}" message survive restarts.
- Concurrent resumes are already covered by the BUG-002 `TopicCreationState` lock (used in both the resume and first-time paths). Keep the ADR-013 E3 "Session resumed … previous topic was removed" context message.
- Note the child-orphan edge case: a resumed child whose parent ended has no parent topic — log and create its own topic (do not silently drop).

**A6. Lengthen the stale-cleanup timer interval.**
**Decision:** keep the timer as a safety net for crashed/killed sessions where no `SessionEnd` fires, but run it less often (raise `CLEANUP_INTERVAL_SECS`, `mod.rs:43`, from 5 min to ~15-30 min) since it is now rarely the primary teardown path.

**A7. Delete the `adaptive_retry` dead code.**
Remove the field and its "Bot API 8.0+" comments from `queue.rs` (148/160/251), `types.rs:73`, and `error.rs:48`. Keep the `retry_after`-based backoff (the only live path). Add a one-line comment documenting the grammY-style philosophy (honor `retry_after`; pause the chat's sends on 429 — already done).

### PR-B — Approval / button reliability

**Decided scope:** keep the existing four buttons (Approve / Reject / Abort / Details) — **no "Always" button**. Leave the timeout behavior **unchanged** (timeout → `ask` → CLI fallback; the operator typically runs `--dangerously-skip-permissions`). The changes below are reliability/UX only.

**B1. Send approval requests at Critical priority.**
Route the approval `send_with_buttons` through the Critical tier so a queue backlog cannot delay the prompt the user is blocked on (resolves the `socket_handlers.rs:737` TODO comment). Top-ranked glitch cause (message arriving late/never).

**B2. Check `resolve_approval` success before replying.**
Only emit the `ApprovalResponse` when `resolve_approval` actually transitioned a `pending` row (`session.rs:759-771`). Prevents a double-tap from sending two responses to the hook.

**B3. Edit the approval message on resolve (decision + time).**
After a decision, edit the message to a static resolved line showing **the decision and the time** (e.g. "✅ Approved · 14:03") and **remove the inline keyboard** (`editMessageReplyMarkup`). Structurally prevents re-taps and makes the topic a readable audit trail.

**B4. Evict `pending_approval_clients`.**
Remove the entry on session end and on socket-client disconnect; sweep expired entries in the (now longer-interval) cleanup cycle. Closes the unbounded-growth leak (`socket_handlers.rs:722`).

**B5. Tolerate stale/unknown approval IDs gracefully.**
On a callback for an ID with no open/pending approval (daemon restarted, expired, already handled): `answerCallbackQuery` with a `show_alert` such as "This request expired or was already handled," and edit the message to mark it stale. Never crash, never block, never mis-route. (SOTA idempotency-on-request-ID pattern.)

### PR-D — Onboarding + explicit trust model (wizard acknowledgment, new setups only)

**D1. Require an explicit, recorded trust acknowledgment in the setup wizard.**
The wizard (`setup.rs`) must surface the trust assumption at the point the chat is configured and require an active `y/N` acknowledgment **before it writes config** — not merely print a notice — and not proceed on `N`. **Scope: new setups only** — do not re-prompt already-configured users on upgrade, and do not nag in `ctm doctor`. Suggested text:

> "Anyone you add to this Telegram channel can drive your shell and approve tool calls. Treat the channel like a shared shell: only add people you would already trust with git-commit access. Semi-trusted or public channels are not supported. Do you understand and accept this? (y/N)"

Record acceptance; mirror the warning in the README. No per-user authorization code — the chat-level model is intentional (see Neutral consequences). Terminology: any future trusted-user list is a **whitelist** (and its inverse a **blacklist**).

### PR-E — Structured AskUserQuestion answers (eliminate option-question keystroke injection)

Spike-confirmed (above): option-based answers can be delivered via `PreToolUse` + `updatedInput`, no keystrokes. Largest reliability win; directly targets reported pain (c).

**E1. Block-and-return for `AskUserQuestion` in the hook; keep the "Submit All" review.**
Register `AskUserQuestion` as a `PreToolUse` matcher whose hook path blocks on the existing `send_and_wait` correlation machinery (the pattern approvals use), then emits:
```json
{ "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "updatedInput": { "questions": [ ... ], "answers": { "<question text>": "<label or comma-joined labels>" } } } }
```
**Decision:** keep the ADR-012 tentative-selection UI **including the explicit "Submit All" review step** (the hook returns all answers in one payload, so collect-then-submit is also the natural fit, and the review lets the user catch mistakes before committing). Replace only the *delivery* mechanism.

**E2. Delete the option/multi-select keystroke injection path.**
Once E1 lands, remove the 300ms-per-key toggle/Down/Enter sequence (`callback_handlers.rs:1196-1217`) and the fragile fixed `auto_submit_answers` 2s sleep (`callback_handlers.rs:1426`) for option-based answers. Net deletion of the most fragile code in the project.

**E3. Keep an isolated keystroke path for free-text answers only.**
**Decision:** no structured path exists for free-text, and free-text support is wanted — so retain keystroke injection **for the free-text case only**, clearly isolated and commented as the **sole remaining** injection path. All option/multi-select answers go structured via E1.

**E4. Surface render failures.**
Stop silently swallowing `msg_id == 0` send failures in the question render path (`socket_handlers.rs` question send); log/notify so a failed render is visible rather than a silent no-answer.

## Consequences

### Positive
- Event-driven teardown + immediate topic deletion eliminates the dominant stale/orphaned-topic cause and keeps the phone topic list clean; the `resume` special-case and the A5 guards prevent churn and stale-topic sends.
- Persisting the custom title fixes resume naming across daemon restarts (a latent gap surfaced by the deep-dive).
- Approval flow becomes robust against late delivery, double-taps, restarts, and stale buttons, with a readable decision+time audit trail — directly targeting reported glitches.
- The wizard acknowledgment makes the trust model an explicit, recorded operator decision (not a buried doc note) — appropriate for the #2 audience while keeping the intentional chat-level model.
- Removing fabricated `adaptive_retry` code stops it misleading future maintainers.
- **PR-E deletes the most fragile code in the project** (option/multi-select keystroke injection) and replaces it with the same robust block-and-return pattern approvals already use — the spike confirmed this is achievable on plain tmux without Channels.

### Negative
- Immediate deletion means a resumed session always loses its prior phone-side topic and starts fresh (accepted: computer retains history).
- Registering `SessionEnd`/`AskUserQuestion` changes the hook-installation surface; existing installs need `ctm install-hooks` re-run (and `ctm doctor` should detect the drift).
- **PR-E free-text gap:** there is no structured path for free-text answers, so an isolated keystroke-injection path survives for that one case (intentional — free-text support is wanted).
- Path A consciously declines a wholesale Channels migration; if Anthropic's Channels matures, parts of `ctm` may be superseded.

### Neutral
- Per-user authorization remains deliberately unimplemented (threat-model decision, not an oversight). Any future trusted-user list is a **whitelist**.
- Status reactions and SessionStart auto-titles were considered and **declined** (former PR-C) — reactions add allowed-emoji fragility for little value here, and the existing `/rename` + transcript custom-title path covers naming.
- Throughput items found in the audit (injector mutex held during free-text injection, single `Mutex<SessionManager>` DB serialization, unbounded cleanup-task spawning, accept-loop backoff, double-`start()` panic window) are **explicitly deferred** — not reported pain and below realistic load for this tool.

## Execution order

1. **PR-A** (lifecycle: register `SessionEnd`, immediate teardown + resume robustness, longer timer, dead-code removal — highest confidence, addresses top reported pain: stale topics) →
2. **PR-E** (structured AskUserQuestion answers — spike-confirmed; deletes the most fragile code, addresses reported pain (c)) →
3. **PR-B** (approval reliability) →
4. **PR-D** (onboarding + explicit trust acknowledgment).

(Spike S1 is resolved — see "Spike result" above. PR-E supersedes the former interim mitigation. Former PR-C removed per interview.)

## Implementation log

### PR-A — landed 2026-05-28 (all tests green: 399 passing, +7 new)

Verified each anchor against the live tree before changing it (Chesterton's fence). One **scope correction** surfaced during implementation:

- **A5 stale-id self-heal already existed.** The `topic_invalidated` channel (`bot/queue.rs` → `event_loop.rs::handle_topic_invalidated`) already nulls the `thread_id` in both the cache and the DB on `TOPIC_ID_INVALID` and "message thread not found". So A5 reduced to its two genuinely-missing pieces: **custom-title DB persistence** and the **child-orphan log**. The self-heal is documented, not rebuilt.

Changes shipped:
- **A1** `SessionEnd` added to `HOOK_TYPES` (`installer.rs`) via standard `create_hook_entry()` (no timeout). Test: `session_end_hook_registered_without_timeout`.
- **A2** `#[serde(alias = "session_exit_reason")]` on `SessionEndEvent::reason` (`types.rs`). Tests: `session_end_deserializes_session_exit_reason`, `…_resume_reason`, `…_without_reason_defaults_none`.
- **A3** `handle_session_end` returns early when the reason is `resume` (no teardown of a suspended session).
- **A4** Auto-delete branch now deletes the topic **immediately** and **synchronously** clears `thread_id` from the DB + `session_threads` cache before returning; tolerates "already deleted". Two-stage `schedule_topic_deletion` removed; `cancel_pending_topic_deletion` retained as a safe no-op guard. DB-invariant test: `clear_thread_id_drops_stale_topic_mapping`.
- **A5** New `custom_title` column + migration + `set_custom_title`; persisted on `/rename`; resume reads cache-then-DB and warms the cache; child-orphan logged. Tests: `custom_title_persists_and_survives_end`, `custom_title_visible_across_reopen`.
- **A6** `CLEANUP_INTERVAL_SECS` 5 min → 20 min (sweep is now a safety net).
- **A7** `adaptive_retry` dead code deleted from `error.rs`, `bot/types.rs`, `bot/queue.rs`; `retry_after`-only backoff retained with a grammY-philosophy comment.

### PR-E — landed 2026-05-28 (structured AskUserQuestion; all tests green, +10 new)

Reused the approval correlation backbone (`send_and_wait` + targeted client write-back) for questions, so the most fragile path in the project now uses the same proven mechanism.

- **New message types** `QuestionRequest` / `QuestionResponse` (`types.rs`); `send_and_wait` generalized to match any expected response type (the approval caller now passes `ApprovalResponse`).
- **E1** Hook side: `get_question_hook_output` blocks on a `QuestionRequest` and, on reply, emits `permissionDecision: allow` + `updatedInput { questions, answers }` (answers keyed by question text). The contract-critical builder is the pure, unit-tested `question_hook_output_from_response`. The AskUserQuestion `ToolStart` is suppressed in `build_messages` (it would be a duplicate, late render under the blocking model).
- Daemon side: `handle_question_request` registers the originating client (`pending_question_clients`, keyed by session_id) and renders via the existing tentative-selection UI (the ADR-012 "Submit All" review is **retained**). `handle_submitall_callback` now builds the answers map (`build_answers_map_content`, unit-tested) and writes a targeted `QuestionResponse` — **no keystrokes**.
- **E2** The 300ms toggle/Down/Enter dance + the 2s `auto_submit` sleep no longer run on the option path; they survive **only** inside the free-text fallback branch.
- **E3** Free-text has no structured contract, so a `QuestionResponse` carrying the `__freetext_fallback__` sentinel makes the hook return a bare `allow`; the daemon then drives the answer via the isolated keystroke path (the sole remaining injection path).
- **E4** `msg_id == 0` render failures are now surfaced in the topic ("N of M question(s) failed to send"), not silently swallowed.

**Design decision flagged for review (deviation from E2's literal wording):** a single AskUserQuestion call can mix option-questions and a free-text answer. Since free-text has no structured path, the whole set falls back to keystrokes when *any* answer is free-text — so the multi-select dance is *relocated into* the isolated fallback branch rather than deleted outright. When no free-text is present (the dominant case), zero keystrokes run. The free-text fallback also reintroduces a TUI-render wait (`QUESTION_TUI_RENDER_WAIT_MS = 1500ms`) that is inherently racy — accepted for free-text only, per E3.

### PR-B — landed 2026-05-28 (approval reliability; all tests green, +3 new)

- **B1** Approval requests now send at **Critical** priority. `bot::client` gained `send_with_buttons_critical`; `MessagePriority` stays bot-private (callers choose intent, not the enum). Resolves the ADR-011 Fix #9 TODO.
- **B2** `handle_approval_callback` now checks the `resolve_approval` boolean and only emits the `ApprovalResponse` when **this** tap actually transitioned the pending row — a double-tap returns `false` and is ignored (no second response to the hook). For abort, the session is ended only when the transition happened. Test: `resolve_approval_is_idempotent`.
- **B3** On resolve, the message is edited to a static resolved line **decision + time** (e.g. "✅ Approved · 14:03", local time) with the **keyboard removed** (`edit_message_text_no_markup`), structurally preventing re-taps and forming a readable audit trail.
- **B4** `pending_approval_clients` is now evicted: precisely at session end (the session's pending approval IDs are removed from the map) and via a **cleanup-cycle sweep** that retains only still-pending IDs (`SessionManager::pending_approval_ids`). The sweep also reaps disconnect-orphaned entries, so the deliberately approval-agnostic socket layer stays unchanged. Test: `pending_approval_ids_tracks_status`.
- **B5** A callback for an unknown/expired approval ID no longer silently returns: it answers with a `show_alert` ("This request expired or was already handled.") and edits the message to mark it stale — never crashes, blocks, or mis-routes. Test: `get_approval_unknown_id_is_none`.

### PR-D — landed 2026-05-28 (trust acknowledgment; all tests green, +1 new)

- **D1** The setup wizard (`setup.rs`) now requires an active `y/N` trust acknowledgment **before any config is written**, and aborts (writing nothing) on `N`. Scope is **new setups only**: `is_new_setup` is false when an existing token or chat ID is found, so configured users are not re-prompted on upgrade/reconfigure, and `ctm doctor` is untouched. Acceptance is recorded as `trustAcknowledged: true` in `config.json`. The notice is a single shared `TRUST_NOTICE` const (the exact ADR wording), mirrored in the README's new "Trust Model" section. Terminology: future trusted-user lists are a **whitelist** / **blacklist**. Test: `trust_notice_states_the_model` guards the load-bearing wording.

(D1 is otherwise an interactive `dialoguer` flow; the prompt/abort path is verified manually, the wording and new-setup gating are the unit-tested invariants.)

### Summary

All four PRs landed on `feat/adr-014-lifecycle-approvals`. Test count 392 → 414 (+22 across PRs). `cargo fmt` clean; `cargo clippy` clean except two pre-existing warnings unrelated to this work (`queue.rs:526` manual-range in a test, `service/env.rs:125` empty `writeln!`). Release build (LTO, production profile) verified.

### Benchmark — PR-E answer-delivery latency

The whole point of PR-E is replacing a multi-second, racy keystroke sequence with a structured reply. Measured (`bench_structured_delivery_is_negligible`):

| Path | Fixed latency (representative 4-option multi-select, 2–3 selected) |
|---|---|
| Old keystroke (E2 deleted) | ~4,700 ms — 9 keys × 300ms + 2,000ms `auto_submit` sleep, **plus** an unbounded TUI readiness race |
| New structured (E1) | **~3 µs** compute + one socket write; no sleeps, no race |

That is a ~6-order-of-magnitude reduction in answer-delivery compute and removal of the dominant fragility. The keystroke path now runs only on the free-text fallback (E3). This *is* the optimization — there was no faster way to deliver option answers, so the work was eliminating the slow path, not tuning it.

### Adversarial review (/cfa dual-mode: Claude reviewer + Codex 0.128) — 2026-05-28

Both reviewers ran read-only over the branch diff. Real bugs found and fixed (tests added):

- **Codex HIGH — mirroring toggle dropped blocking questions** (`mod.rs` `is_always_active`): `QuestionRequest`/`QuestionResponse` were not in the always-active set, so toggling mirroring off would drop the blocking question and hang the hook for 300s. Fixed — they join approvals/commands as always-active (a blocking path must never be gated). 
- **Codex HIGH — late Submit-All after timeout silently lost the answer**: a tap after the hook's 300s timeout took the structured path and wrote to the dead client with no feedback. Fixed — `send_question_response` now returns delivery success; on failure the user is alerted ("timed out … answer at the terminal"), never silent loss.
- **Claude HIGH — approval-client eviction ordered before `end_session`** (`socket_handlers.rs`): reordered to evict *after* `end_session` expires the approvals, so a racing tap resolves to a no-op (B2) instead of mis-routing.
- **Claude HIGH — teardown atomicity**: `clear_thread_id` (DB+cache) now runs *before* `delete_forum_topic`, so a crash between leaves a benign leaked topic instead of a stale mapping (the `topic_invalidated` self-heal already covered the residual case).
- **Claude MED — guard held across await** in `send_question_response`: now clones the writer Arc and drops the `socket_clients` map lock before the write.
- **Claude MED — blind keystroke injection with no hook client**: submit logic refactored to a unit-tested `classify_submit` (`Structured` / `FreeTextRelease` / `NoClient`); the no-client case now notifies instead of injecting into an unknown screen.
- **Claude MED — resume leaked the question-client entry**: the `resume` early-return now drops it immediately.

### Post-merge deep-research verification (2026-05-28) — `updatedInput` contract vs ground truth

A `/deep-research` pass cross-checked PR-E's `updatedInput` contract against official docs and **jsayubi/ccgram's current production `question-notify.ts`** (the source the original spike cited). Findings:

- **Confirmed:** answers map is keyed by **question text**, original `questions` echoed back, `permissionDecision: "allow"` — PR-E matches.
- **Fixed (was a real bug):** multi-select labels must be joined with a **bare comma, no space** (`labels.join(',')`, "Claude Code format" per ccgram). PR-E had `", "`; a comma+space risks a leading space on every label after the first (`" Go"` ≠ `"Go"`) if Claude splits on `,` without trimming. Corrected to `","`; tests updated.
- **Open finding (revisits an interview decision):** ccgram's current code delivers **free-text answers via `updatedInput` too** (`answers[questionText] = textAnswer`, no keystrokes) — contradicting this ADR's spike premise that free-text "has no structured path." That premise appears **outdated**. PR-E's E3 keystroke fallback (and its racy `QUESTION_TUI_RENDER_WAIT_MS`) may therefore be **unnecessary**: free-text could likely go structural by placing the typed string in the answers map, eliminating the last injection path and the race. **Not changed yet** — ccgram only demonstrates this for option-less questions; the arbitrary-"Other"-on-an-option-question case needs an empirical spike before ripping out a deliberately-decided fallback ("never guess"). Recommended as the top follow-up to reach a fully keystroke-free design.

Verified non-issues (documented, no change): `QuestionRequest`-before-`SessionStart` is handled by `ensure_session_exists` on-the-fly creation (same as the proven approval path); the `custom_title` column always exists by the time `row_to_session` runs (migration in `new()`); `SessionStart`-after-`get_hook_output` ordering is pre-existing approval-flow behavior, not introduced here. New tests: `classify_submit_decision_table` (+ the prior PR-E/PR-B coverage). Final: 416 tests passing, clippy clean, release build verified.

### Post-release field-defect fixes — 2026-05-31 (`fix/adr-014-askuserquestion-bypass-and-render`)

A field report ("AskUserQuestion never reached Telegram") triggered a deep-research investigation (codebase + Claude Code docs + GitHub issues), each finding independently re-verified by an adversarial **Codex** review against the live source, then implemented test-first with per-change Codex review. Live-deployed and proven on `io.loveathome.us` (systemd user service; binary built on-host).

- **D1 — bypass-permissions ordering swallowed every question (root cause).** `get_hook_output` returned `None` for `permission_mode == "bypassPermissions"` *before* the `AskUserQuestion` branch, so under `--dangerously-skip-permissions` (the normal operating mode) the `QuestionRequest` was never sent and Claude fell back to its terminal TUI. Extracted a pure, unit-tested `classify_hook_route` that routes `AskUserQuestion` ahead of the bypass short-circuit (a question is input collection, not a permission gate). Regression test locks the ordering.
- **D2 — Markdown-v1 400s broke the render.** Question/header/option/answer text (arbitrary model content with `*`, `_`, `[`) was sent under `parse_mode=Markdown` while `escape_markdown_v1` only stripped backticks → unbalanced-entity HTTP 400 → the message (often the "Submit All" carrier) silently failed. Introduced a shared plain-text `render_question_text` and routed **every** question-lifecycle message (initial render, per-tap edit, change/re-render, free-text edit, summary) through plain text.
- **D3 — honest render-failure log** (the old "retrying via queue" never retried) + hook released on failure.
- **D4 — no more silent 300s hangs.** New `release_question_hook` (sends the free-text sentinel → hook returns to its TUI) is called on every drop/error path: no-topic, missing input, empty questions, render failure, and supersede (the previously-blocked hook is now released instead of hanging).
- **D5 — resilient 429 retry.** `api_call` retried a 429 only once; now retries up to `MAX_429_RETRIES` (3) honoring `retry_after`, so a rate-limit storm cannot strand the direct (un-queued) question/summary sends. Residual: a transient summary-send failure now releases the hook (Phase-4).
- **D6 — mirror-storm resilience.** `ToolStart` previews demoted to a new Low queue priority (`send_message_low` / `send_with_buttons_low`), fulfilling the long-standing ADR-011 Fix #9 TODO. The queue drains Critical→Normal→Low, so previews are naturally deferred (sent only in lulls) and shed oldest-first under load while `ToolResult` (Normal) and approvals/questions (Critical) are preserved. Per-drop log spam replaced by a throttled 5s aggregate. (Per-session coalescing considered and **declined** — tiering already suppresses; would add a `QueuedMessage` key for no real gain.)
- **D7 — `ctm doctor` warns** when a non-ctm `PreToolUse` hook coexists, because Claude Code ignores hook `updatedInput` with multiple `PreToolUse` hooks ([anthropics/claude-code#15897](https://github.com/anthropics/claude-code/issues/15897), won't-fix). Reuses the installer's canonical token-based classifier (shared as `pub(crate)`), so embedded names like `xctm-linter` aren't misclassified.
- **D8 — timeout correctness.** The 300s wait + 310s registered timeout were hardcoded in two files; now `config.question_wait_secs` (default 300) + `QUESTION_HOOK_TIMEOUT_BUFFER_SECS` (10), single source of truth. Added a pre-emptive expiry nudge (one Low-priority "answer at terminal" notice at ~80% of the wait, guarded against the answered/superseded races via `Arc::ptr_eq`). **Analysis:** the ~60s figure is Claude's *terminal-TUI* timer, which only starts after the hook returns without `updatedInput`; the hook-block phase is bounded by the registered hook timeout (honored — proven by 24h of live hook traffic). ctm is structurally immune to the [#28508](https://github.com/anthropics/claude-code/issues/28508)-class silent infinite wait (bounded wait + TUI fallback + late-submit notice). The intrusive sleeping-hook probe was **not** run — it would require disabling the live global ctm hook, risking active sessions; the configurable knob is the escape hatch.
- **Collision-safe routing (Phase-4).** `resolve_pending_key` now refuses an ambiguous 20-char session-prefix (logs + returns `None`) instead of first-match misrouting — near-impossible for UUIDs, no longer silently unsafe.

Every change: `cargo build`/`fmt`/`clippy` clean and the full suite green (151 lib + all integration suites) at each commit, with an independent Codex read-only review (verdicts: APPROVE / APPROVE-WITH-NITS, all nits addressed; Phase-4 PASS). The structured-free-text follow-up flagged above (PR-E "open finding") remains the recommended next step toward a fully keystroke-free design.

## Links

- ADR-003 — Dual hook handlers (hook architecture this builds on)
- ADR-004 — tmux-only injection (the constraint PR-E removes for option-based answers)
- ADR-011 — Resilience architecture (rate-limit/queue context for A4, B1)
- ADR-012 — AskUserQuestion tentative selection (the flow PR-E refactors — UI retained, delivery changed)
- ADR-013 — Session hierarchy and tmux reliability (session lifecycle this extends)
- Claude Code Channels (research preview): https://code.claude.com/docs/en/channels — https://code.claude.com/docs/en/channels-reference
- Reference channel implementations: https://github.com/anthropics/claude-plugins-official (external_plugins)
- AskUserQuestion hook gap: https://github.com/anthropics/claude-code/issues/44326
- Comparable bridges: https://github.com/jsayubi/ccgram · https://github.com/six-ddc/ccbot · https://github.com/avivsinai/telclaude
