# ADR-014: Lifecycle Hooks, Approval UX, and Human-Input Reliability

**Status:** accepted, in progress — scope finalized via deep-research, a verification spike, and a per-work-item product interview (2026-05-28). **PR-A landed 2026-05-28** (see Implementation log).
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

PR-B, PR-D: pending.

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
