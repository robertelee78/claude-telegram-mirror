# ADR-015: Restore AskUserQuestion to the Bidirectional Mirror (Supersede ADR-014 PR-E)

**Status:** accepted — empirical spike (2026-05-31) resolved the two load-bearing unknowns; single-user invariant simplifies the race handling. Implementation in progress on `master`.
**Date:** 2026-05-31
**Authors:** Robert, Claude
**Tags:** askuserquestion, tmux-injection, bidirectional-mirror, regression, supersedes-adr-014-pr-e

DO NOT BE LAZY. Understand the current state fully before changing it (Chesterton's fence). The thing this ADR restores was working; the regression is git-proven, not inferred. Measure 3x, cut once. No stubs, no fallback-as-design.

## Context

### What ctm IS (the invariant this ADR defends)

Per `docs/ARCHITECTURE.md` line 3, ctm is *"a bidirectional bridge... The system **mirrors Claude Code activity to Telegram** forum topics and **routes Telegram replies back into the CLI via tmux**."* The operative model, stated plainly by the operator:

> "I might ssh to a machine, get into tmux, and start claude. At any point I should be able to open Telegram and see everything from that session. At any point everything from the session should be shown from the Claude Code CLI **AND** Telegram. At any point I should be able to type from either the CLI or Telegram. At any point if a question comes up it should render for **both** the CLI and Telegram, and I should be able to answer it from **either**. Anything short of this is a violation of what ctm is meant to be."

The CLI is not a surface ctm "renders to" — it is the live session; **Claude Code itself draws to the terminal.** ctm only (a) mirrors that activity *out* to Telegram and (b) injects input *back* via `tmux send-keys`. Every interaction in ctm rides those two rails. "Both surfaces" is therefore the *default* — it happens for free for everything Claude renders, as long as ctm does not pre-empt the render.

### The regression (git-proven)

ADR-014 **PR-E** (`dec6e30`, 2026-05-28, shipped in 0.2.19) was intended to eliminate the genuinely fragile multi-select keystroke-injection path. It did so by **intercepting AskUserQuestion in the hook**, blocking on a `QuestionRequest`/`send_and_wait`, and returning the answer via `hookSpecificOutput.updatedInput`. The PR-E code comment states the effect outright: *"Claude proceeds with **no TUI**."*

Two consequences, both verified:

1. **`updatedInput` suppresses the local CLI widget by design.** Claude Code only forces the AskUserQuestion widget when *no* answer is supplied (the tool declares `requiresUserInteraction()`; see anthropics/claude-code#29547). Supplying `updatedInput` tells Claude "already answered — do not prompt." So the question stopped rendering in the CLI.
2. **PR-E also suppressed the AskUserQuestion `tool_start`** (commit: *"AskUserQuestion ToolStart suppressed"*), removing the very signal that drove the Telegram render in the working design.

Net: a working **both-surfaces** feature became **Telegram-only**.

### Proof that it previously worked (git history)

In `dec6e30^` (the commit immediately before PR-E):

- **`hook.rs` had ZERO AskUserQuestion handling** — no `get_question_hook_output`, no `QuestionRequest`, no `updatedInput`. The hook returned fast, so **Claude rendered its native question widget in the terminal.**
- **`socket_handlers.rs:566` `handle_tool_start`** intercepted the question from the *normal, fire-and-forget* `tool_start` mirror (line 571: `if tool_name == "AskUserQuestion" → handle_ask_user_question`, the ADR-012 "Epic 3" render, `eec8b0f`).
- **`callback_handlers.rs:1160`** answered by **injecting into the live CLI widget**: `inj.inject(text)` for single/free-text (1180), `send_key(digit)`/`send_key("Down")`/`send_key("Enter")` for multi-select (1201–1216), then `auto_submit_answers` (1255) — commits `372eefe`, `8874908`.

That is exactly the operator's model: **CLI renders (native) + Telegram renders (mirror) + answerable from either (native at the CLI, injection from Telegram).**

### The one legitimate concern PR-E had

The multi-select injection *was* racy: `auto_submit_answers` slept a fixed **2s** while a multi-select could take **~3s** of 300ms-spaced keystrokes, so the confirming Enter could fire before Claude's review screen existed; `tmux send-keys` readiness was assumed, not verified. **This bug is real and must be fixed** — but the fix is to make injection *reliable*, not to delete the CLI render.

### Verified hard constraint (so no future session re-litigates "just use updatedInput, but better")

A clean "both surfaces, either answers, **no keystrokes**" is **impossible** under the current Claude Code contract:
- `updatedInput` *is* render-suppression (docs + #29547).
- `async`/`asyncRewake` are **unsupported on PreToolUse** (PostToolUse-only).
- MCP `Elicitation` fires only for MCP-server tools, not native AskUserQuestion.
- Anthropic closed the dedicated-AskUserQuestion-hook requests (#12605, #15872, #28273) and the multi-hook `updatedInput` bug (#15897) as **not planned**.

Therefore the only design that satisfies the model is: **CLI renders the native widget + remote answer delivered via injection.** Injection is not a regrettable fallback here — it is ctm's defining mechanism (ADR-004).

### Empirical spike results (2026-05-31, on the operator's dev Mac, CLI 2.1.159, ctm 0.2.19)

Two load-bearing unknowns the audit flagged were resolved by observation, not inference:

1. **AskUserQuestion fires PostToolUse → the completion signal exists.** This session's own transcript shows both `AskUserQuestion` `tool_use` entries have matching `tool_result` entries (2/2), correlatable by `tool_use_id` (`toolu_018yTS…`, `toolu_01S38f…`). The tool completes like any other → PostToolUse fires. **This refutes the earlier draft's "PostToolUse is unreliable" claim** (which was asserted, not demonstrated; the codebase already builds a `ToolResult` from every PostToolUse, `hook.rs:327`). The daemon can therefore detect a CLI-answered question by matching `tool_use_id` on PostToolUse and stale the Telegram buttons.

2. **A fast / no-`updatedInput` hook return renders the native CLI widget on 2.1.159.** The ctm socket was Missing during the spike, so the question hook returned `None` (couldn't connect) — and AskUserQuestion still worked interactively in the CLI. That is exactly the restored code path, proven live on the operator's version; the #52822 render-regression risk does not bite here.

### Single-user invariant (simplifies the design — do not re-add race machinery)

**The operator is one person and never answers the same question from two surfaces at once.** Therefore there is no cross-surface contention: at most one of {CLI, Telegram} is ever answering. This removes the need for any "is the widget still on screen / did the other surface already answer?" capture-pane *guard*. The only screen-timing concern that remains is **intra-injection pacing** — ctm's own multi-keystroke multi-select sequence must stay in step with Claude's UI (the bug PR-E over-reacted to), which is a readiness check between ctm's *own* injected steps, not a guard against a concurrent human.

## Decision

**Supersede ADR-014 PR-E.** Restore AskUserQuestion to ctm's bidirectional mirror architecture (the ADR-012 shape), and fix the multi-select race *properly* instead of by deletion. This is a **surgical hand-edit, not `git revert dec6e30`** (which conflicts — D1–D8 are layered across hook routing, callbacks, daemon state, message types, config, and the installer timeout).

1. **Stop intercepting AskUserQuestion in the hook.** Remove the blocking `get_question_hook_output` path, the `QuestionRequest`/`QuestionResponse` round-trip for questions, the `pending_question_clients` map, the `updatedInput` answer return, and the question-specific `send_and_wait`/timeout machinery (incl. the ADR-014 D8 `question_wait_secs` apparatus). The hook returns fast for AskUserQuestion, so **Claude renders its native widget in the CLI.**

2. **Un-suppress the AskUserQuestion `tool_start`.** Restore the `handle_tool_start → handle_ask_user_question` trigger (`socket_handlers.rs`) so the daemon renders the question to Telegram from the standard fire-and-forget mirror — the same rail every other tool uses.

3. **Restore the injection-based answer path.** "Submit All" (and any single-tap answer) injects into the live CLI widget via `InputInjector` — the same Telegram→CLI rail as every other reply. Answering at the CLI is then native; answering from Telegram is injection.

4. **First-responder-wins via PostToolUse (housekeeping, not a race guard).** When the operator answers at the **CLI**, the daemon learns of it from the **PostToolUse** event for AskUserQuestion (spike-confirmed signal, correlated by `tool_use_id`/`session_id`) and edits the Telegram question message to "✅ answered at terminal," removing the buttons. When the operator answers from **Telegram**, the daemon injected it and already knows. Because of the **single-user invariant**, these never collide — there is no concurrent-answer race and therefore **no capture-pane "is the widget still live?" guard** is needed; a tap that arrives after PostToolUse already staled the message is simply a no-op on a removed keyboard.

5. **Fix the multi-select race that PR-E was chasing — for real (intra-injection pacing only).** When ctm injects a *multi-select* answer it sends several option keystrokes then a confirming Enter; the old code used fixed `300ms`-per-key + a blind `2000ms` `auto_submit` sleep, so the Enter could fire before Claude's "Review your answers" screen rendered. Replace the blind sleeps with **readiness detection** (`tmux capture-pane` to confirm the expected screen — the multi-select checkbox, then the review screen — is present before sending the next key / the confirming Enter). This is pacing *ctm's own* keystroke sequence against Claude's UI, **not** guarding against a concurrent human. Single-select and free-text answers inject as literal text + Enter and do not need the multi-step pacing.

6. **Preserve the standalone field fixes that are not tied to interception** — e.g. ADR-014 **D2** plain-text question rendering (Markdown-v1 400s), **D6** mirror-storm queue tiering. These are independent of the PR-E mechanism and stay.

7. **Explicitly do NOT pursue the ADR-014 "open finding" follow-up** (make free-text *also* structural via `updatedInput`, "eliminate the last injection path"). Under this ADR that follow-up would remove the one remaining CLI-rendering path and re-violate the model. It is rejected, not deferred.

## Consequences

### Positive
- Restores the operator's defining requirement: AskUserQuestion renders in **both** the CLI and Telegram and is answerable from **either** — the behavior that demonstrably worked before `dec6e30`.
- Re-aligns AskUserQuestion with ctm's two-rail architecture (mirror out / inject in); deletes PR-E's bespoke blocking apparatus, the only interaction in the system that resolved a tool out-of-band.
- Fixes the *actual* bug (multi-select race) via readiness detection — more robust than either the old fixed-sleep injection or PR-E's deletion.
- Eliminates the class of defects PR-E's interception introduced (e.g. the bypass-mode swallow that required ADR-014 D1): the fire-and-forget `tool_start` mirror is not gated by permission mode.

### Negative
- Reinstates keystroke injection for the Telegram-answer case. This is architecturally correct (rail #2) but inherently terminal-dependent; the readiness-detection work is non-trivial and must be done well (the prior fixed-sleep approach is exactly what failed).
- First-responder-wins requires a reliable CLI-answered completion signal; AskUserQuestion's PostToolUse behavior is not guaranteed, so the signal source must be pinned empirically.
- A net revert of a shipped feature path; requires `ctm install-hooks` re-run only if the registered hook surface changes (it should not — AskUserQuestion stops being a special PreToolUse blocker).

### Neutral
- "Both surfaces with **zero** keystrokes" remains impossible by the upstream contract; this ADR accepts injection as the mechanism rather than chasing a non-existent keystroke-free dual-surface path.
- ADR-012's tentative-selection / "Submit All" Telegram UX is retained; only the delivery returns to injection.
- Behavior must be pinned on the operator's CLI version (2.1.159): confirm a fast/no-`updatedInput` return reliably renders and leaves the widget interactive (the suppression contract has regressed across 2.1.x — see #52822). ctm's pre-PR-E behavior is the empirical existence proof.

## Implementation log

**Landed 2026-05-31** on `master` (impl `e513e69`, merge `7785d11`; ADR accept `41544ff`). Implemented in an isolated worktree, then merged to master. **595 tests pass, `clippy -- -D warnings` clean.**

What shipped (matches the Decision):
- **hook.rs** — removed `get_question_hook_output`, `question_hook_output_from_response`, and the `HookRoute::Question` arm; AskUserQuestion PreToolUse no longer blocks (falls through `ApprovalCheck`→`None`). Un-suppressed the AskUserQuestion `tool_start`; always emits a `ToolResult` for AskUserQuestion on PostToolUse (the CLI-answered completion signal).
- **daemon/socket_handlers.rs** — `handle_tool_start` renders AskUserQuestion to Telegram via `handle_ask_user_question`; `handle_question_request` deleted; `resolve_pending_question` stales the Telegram buttons on the PostToolUse `ToolResult` (idempotent, `Arc::ptr_eq` identity-checked removal). Render store-back is lifecycle-aware (`store_back_action`) so a CLI answer during the render window can't orphan buttons.
- **daemon/callback_handlers.rs** — "Submit All" injects all answers via tmux (`inject_answers -> bool`, fails closed: no auto-submit / no "Submitted" on keystroke failure); `auto_submit_answers` polls `capture_pane` for the review screen instead of a blind sleep. A `QuestionLifecycle { Active, Submitting, Resolved }` state machine (on `PendingQuestion`) is the single concurrency arbiter; every mutating/keyboard-arming path gates on `Active`, and `restale_if_resolved` follows every keyboard-arming edit. No `pending_q`/entry lock is held across Telegram I/O or injection.
- **daemon/telegram_handlers.rs** — `handle_free_text_answer` gated on `Active`, no lock across I/O, `restale_if_resolved` after both the option-rearm and the no-markup edit.
- **injector.rs** — `capture_pane()`. **config.rs/installer.rs** — `DEFAULT_APPROVAL_WAIT_SECS` (300) + buffer; PreToolUse hook timeout stays 310s; the `question_wait_secs` apparatus removed. **types.rs/daemon** — `QuestionRequest`/`QuestionResponse`/`pending_question_clients`/`FREETEXT_FALLBACK_SENTINEL` removed.

**Review (CFA + Codex, multi-pass adversarial).** Codex review caught and we closed, in order: (1) silent injection-failure delivery; (2) Submit-All dropping the pending entry before confirming tmux; (3) an AB/BA **lock-order deadlock** between the `pending_q` map guard and the per-entry mutex; (4) a render-window TOCTOU orphaning buttons; (5) a class of **lock-held-across-Telegram-I/O** sites (8 handlers, fixed via outcome-enum refactor); (6) lifecycle-gating + `Arc::ptr_eq` identity removal across all mutators; (7) post-I/O re-stale on every keyboard-arming edit incl. the free-text no-markup branch. Final Codex verdict: **approve** — no single-user-realistic blocker (wrong injection / hang / message corruption / orphaned live buttons) remains. None of these were caught by the compiler or the 595 unit tests; they were concurrency/UX defects surfaced only by adversarial review.

**Benchmark — auto-submit latency & correctness.** The deleted PR-E/ADR-012 keystroke path paid a **fixed ~3,500 ms** of blind sleeps on every multi-select submit (`QUESTION_TUI_RENDER_WAIT_MS` 1500 + `auto_submit` 2000) and could still fire the confirming Enter *before* Claude's review screen rendered (unbounded race). The restored path replaces both with adaptive `capture_pane` readiness polling — `READY_POLL_INTERVAL_MS = 150` up to a `REVIEW_READY_POLL_MS = 3000` cap — sending Enter the moment the "review your answers"+"submit answers" signature appears. Common case (review screen draws in ~100–300 ms): **~150–450 ms vs 3,500 ms fixed (~8–20× faster)** *and* race-free; slow case: waits adaptively to 3 s instead of firing blind at 2 s. The win is correctness + adaptivity, not a tuned constant.

**Residual items for live validation (single-user-acceptable, noted not blocked):** (a) multi-select `downs_needed = total_options + 2` assumes Claude's Ink checkbox clamps the cursor at the bottom (no focusable row after Submit); (b) single-select injects the option *label* as literal text + Enter (historical ADR-012 behavior) — confirm Claude's widget matches it to the option rather than treating it as free-text. Both are inherent to driving the native TUI by keystrokes and want a live single-/multi-select check on CLI 2.1.159.

### N-generic multi-question injection rewrite (v4, 2026-06-01, 0.2.22)

The 0.2.20/0.2.21 path modelled a multi-question widget as N independently-submitted single-question widgets (it called navigate-to-`Submit` after *each* question). Field-testing two questions from Telegram left Q2+ unanswered and leaked a stray digit into the prompt. **Empirical investigation (binary string-mining of the 2.1.159 bundle + live `tmux capture-pane` frame capture at N=1/2/3, single + multi + free-text + all-single-select)** established the real model: a multi-question AskUserQuestion is **ONE tabbed widget** (`←  ☐ Q1  ☒ Q2  ✔ Submit  →`, state `currentQuestionIndex`/`questionStates`/`allQuestionsAnswered`). Per question the focusable rows are `1..M` options, `Type something`, an advance row labelled **`Next`** (non-final) / **`Submit`** (final), then `Chat about this`. **Advance is a property of the question's `multiSelect`, not the answer type:** single-select (option *or* free-text) auto-advances on commit; multi-select (toggles *or* free-text) must navigate to the advance row + Enter. A single end-of-widget confirm screen (`Ready to submit your answers?` → `Submit answers`/`Cancel`) appears for any N≥2 and for N=1 multi-select; N=1 single-select submits directly.

What shipped:
- **callback_handlers.rs** — `inject_answers` rewritten to drive that one widget, reading the live pane at every step (`parse_widget` → `WidgetView`). Per question: `place_cursor_on` (down-only, verified by re-capture, **fail-closed — no best-effort Enter**) onto each option/`Type something`/advance row; single-select selection or free-text commit auto-advances; multi-select toggles then activates `Next`/`Submit`; the final confirm screen is awaited and `Submit answers` Enter'd only with the cursor verified on it. `wait_for_widget`/`wait_for_transition`/`wait_for_confirm` replace blind sleeps (transition verified by a status-line-independent focusable-row signature). Free-text uses a new literal-only `injector.inject_literal` (no trailing Enter; the Enter-appending `inject` would double-fire) and is sanitized (control chars stripped, length capped).
- **Parsers scoped to the live widget** — anchored on the **bottom-most** tab row (rejects scrollback / stale tab rows), counting only *focusable* rows (option descriptions, the status line/clock, and anything above the tab row are excluded). Strict `parse_option_number` so status-line tokens (`$181.46`, `5/5`) never parse as options.
- **Typed `InjectOutcome`** — `Success` / `FailedClean` (no keystroke acked → safe to restore `Active` for retry) / `FailedDirty` (≥1 keystroke landed → live widget partially advanced → **terminalize**, edit Telegram to "finish at the terminal," never blind-retry from Q0). `mutated` is set only on an acknowledged keystroke.
- **Submit-All guards (before `Active→Submitting`)** — option indices validated against `options.len()`; an empty multi-select (no toggles, no free-text) is rejected with a toast.
- **14 parser unit tests** from the real captured frames (N=1/2/3, single/multi/free-text/confirm, bottom-most-tab-row, scrollback rejection, missing-footer/clipped, strict number parsing). `cargo build/test/clippy/fmt` clean.

**Review (Codex, 3 adversarial plan rounds + 2 diff-review rounds).** Read-only `codex exec` reviews drove the design from v1→v3, closing in order: branch on answer-variant vs `multiSelect` (free-text); delete best-effort Enter (fail-closed); scope parsers to the live widget + focusable rows; explicit wait-for-state transitions; safe partial-delivery (`FailedDirty` terminalization incl. N=1 multi-select); empirical verification of all-single-select N≥2 (confirm appears) and free-text advance; literal-only free-text (no double-Enter); index validation; zero-select guard; bottom-most-tab-row anchoring; label constants. Empirical state machine recorded in the auto-memory note `askuserquestion-widget-state-machine`.

**Diff review (Codex, on the landed commit) → hardening.** Two blockers were raised; one was in-scope for a single user and is fixed, the other is covered by the single-user invariant:
- **Stale-button validation (fixed).** The "all answered" gate was `tentative.len() == questions.len()` (a count), but `tentative` is keyed by callback-provided question indices — after a same-key supersede a *stale* button (an old question's message tapped) could leave a real question unanswered while the count passed, letting injection finalize a partial/empty submit as a false `Success`; and `handle_answer_callback` directly indexed `pending.questions[q_idx]`, panicking on an out-of-range stale index. Both are normal single-user behaviours (scroll up, tap an old button). Fixed by extracting `collect_and_validate_answers` (pure, unit-tested): it iterates **every** question index and requires a valid tentative for each (rejecting unanswered/out-of-range/empty before `Active→Submitting`), and the callback handler now bounds-checks `q_idx`/`o_idx` and no-ops a stale tap instead of panicking.
- **Stale-widget-as-live (invariant-covered, not fixed by code).** A static `capture_pane` cannot prove the widget is *live* vs a stale-but-still-visible render; injecting could then key into a normal prompt. This requires answering the same question in the CLI **and** tapping Telegram Submit All — excluded by the **single-user, first-responder-wins invariant** (a single human answers one surface per question). Deliberately not hardened with a liveness probe (over-engineering for an excluded case); the lifecycle gate + invariant cover it.

**Residual (single-user-acceptable):** a very long option list that clips the footer/target below the viewport relies on Ink scrolling the focused row into view as we step (bounded, fail-closed if not) — verify in live re-test.

## Links
- ADR-004 — tmux-only injection (the mechanism this ADR re-embraces for questions)
- ADR-012 — AskUserQuestion tentative selection (the working render+inject shape this restores; PR-E superseded its injection — this ADR un-supersedes it)
- ADR-013 — Session hierarchy and tmux reliability (injection-failure warnings, three-tier tmux detection reused here)
- ADR-014 — Lifecycle Hooks, Approval UX & Input Reliability (**PR-E superseded by this ADR**; PR-A/PR-B/PR-D and field fixes D2/D6 retained)
- Regression commit: `dec6e30` (ADR-014 PR-E). Last-working state: `dec6e30^`.
- Upstream contract (verified): anthropics/claude-code #29547 (`requiresUserInteraction`), #52822 (allow-no-suppress regression), #12605 / #15872 / #28273 (AskUserQuestion hook requests, not planned), #15897 (multi-hook `updatedInput`, not planned).
- Reference implementation cross-check: github.com/jsayubi/ccgram (`updatedInput`-only, single-surface — what NOT to copy for this model).
