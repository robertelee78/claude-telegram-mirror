# ADR-012: AskUserQuestion Tentative Selection — "Change Your Mind" Before Submit

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
**Authors:** Robert E. Lee
**Supersedes:** None
**Related:** ADR-002 (Phased Rust Migration), ADR-011 (Resilience Architecture)

---

## Context

### Problem: Irrevocable Single-Select Answers

When Claude Code invokes `AskUserQuestion` with multiple-choice questions, the Telegram
bridge renders each question as a separate message with inline keyboard buttons. For
single-select questions, tapping any option immediately:

1. Sets `pending.answered[q_idx] = true` — permanently locking the question
2. Injects the answer text into tmux via the input injector
3. Strips the inline keyboard from the message via `edit_message_text_no_markup`
4. Appends "Selected" to the message text

Once all questions are answered, `auto_submit_answers()` fires after a 500ms delay,
injecting `"1"` to select "Submit answers" on Claude Code's review screen. The entire
flow from first tap to submission is irreversible and happens in under a second.

**User impact:** If you misclick, realize you want a different answer, or simply want to
review your choices before they reach Claude, there is no recourse. The answer is already
injected and submitted. On a phone screen with small touch targets, this is a significant
UX problem.

### Existing Precedent: Multi-Select Already Has Tentative State

The codebase already implements a tentative selection model for `multiSelect: true`
questions:

- `handle_toggle_callback` toggles options on/off without setting `answered = true`
- The keyboard is re-rendered with checkmark prefixes via `edit_message_reply_markup`
- The user must explicitly tap "Submit" to finalize
- Selections can be toggled any number of times before submission

This ADR proposes extending that same pattern to single-select questions and adding
a confirmation step for the entire question set.

---

## Decision

### D1: Tentative Selection Model for Single-Select Questions

Single-select button taps become **tentative** — they record the user's choice in
server-side state but do NOT inject text into tmux or mark the question as answered.

**Current flow (to be replaced):**
```
tap → answered[q] = true → inject text → strip keyboard → auto-submit
```

**New flow:**
```
tap → tentative_selections[q] = option_idx → re-render with checkmark → wait
tap different → tentative_selections[q] = new_idx → re-render → wait
... (user can change as many times as they want)
Submit All → inject all answers → auto-submit review screen
```

### D2: Checkmark Prefix on Selected Option

When a user taps a single-select option, the message is updated:

- **Message text:** Updated to show the current selection (e.g., appends
  `"\n\nSelected: Option A"`)
- **Inline keyboard:** All buttons remain. The selected option gets a `✓` prefix
  (Unicode U+2713), matching the existing multi-select toggle rendering.
- **Other buttons:** Remain untouched — user can tap any of them to change selection.

This requires a new bot client method `edit_message_text_with_markup` that calls the
Telegram `editMessageText` API with both `text` and `reply_markup` parameters. The
current `edit_message_text_no_markup` deliberately omits `reply_markup` to strip the
keyboard — we need the opposite behavior.

**Telegram API constraint:** If `editMessageText` is called without `reply_markup`, the
inline keyboard is automatically removed. This is why a new method is required rather
than modifying the existing one.

### D3: Summary Confirmation Message

After every question has a tentative selection (or free-text answer), the bot sends a
**summary message** that shows all selections and provides final confirmation:

```
Review your answers:

1. Company: Anthropic
2. Model: Claude
3. Safety: Constitutional AI
4. CLI Tool: Claude Code

[Submit All]  [Change Q1]  [Change Q2]  [Change Q3]  [Change Q4]
```

- **Submit All** — Locks all answers, injects them into tmux in sequence, then
  auto-submits the Claude Code review screen after 500ms.
- **Change QN** — Deletes the summary message and re-enables the keyboard on question
  N's message (if the keyboard was visually de-emphasized, restore it). The user can
  then tap a different option, which triggers a new summary once all questions are
  tentatively answered again.

### D4: Free-Text Answers Are Also Tentative

Currently, typing a free-text answer in the topic immediately locks the question
(`answered[q_idx] = true`) and injects the text. Under the new model:

- Free-text answers are stored as tentative selections (stored as the literal text
  string rather than an option index).
- Typing a new message for the same question **replaces** the previous free-text answer.
- The question's message is updated to show the current free-text selection.
- Free-text answers participate in the same summary/confirmation flow.

**Matching logic:** Free-text is matched to the first question that either (a) has no
tentative selection yet, or (b) already has a free-text tentative selection (allowing
replacement). Questions with button-based tentative selections are not overwritten by
free-text — the user must use "Change QN" to clear a button selection before free-text
can target that question.

### D5: No Timeout — Questions Persist Until Answered or Session Ends

The current 10-minute TTL (`QUESTION_TTL_SECS`) is removed. Pending questions persist
in memory until:

- All questions are submitted via "Submit All"
- The Claude Code session ends (session cleanup)
- The daemon is restarted (in-memory state is lost — this is acceptable per user
  decision; disk persistence is not in scope)

**Rationale:** The user may step away from their phone and return later. A 10-minute
timeout would silently expire the questions, leaving Claude Code hanging. Since questions
are in-memory only and scoped to a session, there is no resource leak concern.

### D6: In-Memory State Only

Pending question state is NOT persisted to disk. If the daemon restarts, pending
questions are lost. This is acceptable because:

- Daemon restarts are rare during active sessions
- Claude Code will re-prompt if the previous `AskUserQuestion` call times out
- Disk persistence adds complexity (serialization, file locking, schema versioning)
  for a scenario that almost never occurs

---

## Implementation Plan

### Phase 1: Data Model Changes

**File:** `rust-crates/ctm/src/daemon/mod.rs`

Modify `PendingQuestion` struct:

```rust
pub(super) struct PendingQuestion {
    session_id: String,
    questions: Vec<QuestionDef>,
    // REMOVED: answered: Vec<bool>
    // REMOVED: selected_options: HashMap<usize, HashSet<usize>>  (multi-select only)

    /// Tentative selections for each question.
    /// - Single-select: TentativeAnswer::Option(option_idx)
    /// - Multi-select: TentativeAnswer::MultiOption(HashSet<option_idx>)
    /// - Free-text: TentativeAnswer::FreeText(String)
    /// - Unanswered: absent from the map
    tentative: HashMap<usize, TentativeAnswer>,

    /// Whether each question has been finalized (submitted to Claude).
    /// Only set to true during the Submit All flow.
    finalized: Vec<bool>,

    /// Telegram message_id for each question's message.
    /// Needed to edit messages when selections change.
    question_message_ids: Vec<i64>,

    /// Telegram message_id of the summary/confirmation message, if sent.
    summary_message_id: Option<i64>,

    // REMOVED: timestamp + TTL fields
}

pub(super) enum TentativeAnswer {
    Option(usize),                  // Single-select: index into options array
    MultiOption(HashSet<usize>),    // Multi-select: set of selected indices
    FreeText(String),               // Free-text typed by user
}
```

Remove `QUESTION_TTL_SECS` constant and any TTL-based cleanup logic.

### Phase 2: Bot Client — New Edit Method

**File:** `rust-crates/ctm/src/bot/client.rs`

Add `edit_message_text_with_markup`:

```rust
/// Edit a message's text while preserving or replacing its inline keyboard.
/// Unlike `edit_message_text_no_markup`, this keeps the keyboard visible.
pub async fn edit_message_text_with_markup(
    &self,
    chat_id: i64,
    message_id: i64,
    text: &str,
    parse_mode: Option<&str>,
    buttons: &[Vec<InlineButton>],
    message_thread_id: Option<i64>,
) -> Result<()> {
    let keyboard = build_inline_keyboard(buttons);
    let mut body = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": text,
        "reply_markup": keyboard,
    });
    if let Some(pm) = parse_mode {
        body["parse_mode"] = serde_json::Value::String(pm.to_string());
    }
    self.post("editMessageText", &body).await?;
    Ok(())
}
```

### Phase 3: Single-Select Callback — Tentative Mode

**File:** `rust-crates/ctm/src/daemon/callback_handlers.rs`

Rewrite `handle_answer_callback` (~lines 357–456):

```
1. Parse callback data: answer:{short_sid}:{q_idx}:{o_idx}
2. Resolve full session_id via resolve_pending_key()
3. Lock pending_q, get mutable ref to PendingQuestion
4. If finalized[q_idx] == true → answer_callback_query("Already submitted") → return
5. Update tentative selection:
   pending.tentative.insert(q_idx, TentativeAnswer::Option(o_idx))
6. Rebuild question message text with "✓ Selected: {option_label}" appended
7. Rebuild inline keyboard with ✓ prefix on selected option
8. Call edit_message_text_with_markup to update message text + keyboard in place
9. Call answer_callback_query with toast: "Selected: {option_label}"
10. If all questions now have a tentative answer → send/update summary message
11. If summary was previously sent but a selection changed → delete old summary, send new
```

### Phase 4: Multi-Select Toggle — Align With New Model

**File:** `rust-crates/ctm/src/daemon/callback_handlers.rs`

Modify `handle_toggle_callback` (~lines 458–533) and `handle_submit_callback`
(~lines 535–640):

- Replace `selected_options` usage with `tentative[q_idx] = TentativeAnswer::MultiOption(set)`
- `handle_submit_callback` for multi-select should now just mark the multi-select
  question's tentative answer as finalized for summary purposes (the set is already
  captured via toggles)
- Multi-select "Submit" button becomes "Done" — it signals that the user is finished
  toggling options for that question, but does not inject anything yet
- Summary message logic applies to multi-select questions the same way

### Phase 5: Summary Message

**File:** `rust-crates/ctm/src/daemon/callback_handlers.rs` (new function)

Add `send_or_update_summary`:

```
1. Check if all questions have a tentative answer
2. Build summary text:
   "📋 Review your answers:\n\n"
   For each question:
     "{idx}. {header}: {selected_label_or_freetext}\n"
3. Build inline keyboard:
   Row 1: [Submit All] with callback_data "submitall:{short_sid}"
   Row 2+: [Change Q1] [Change Q2] ... with callback_data "change:{short_sid}:{q_idx}"
   (Group Change buttons 2–3 per row for phone-friendly layout)
4. If summary_message_id exists → edit that message
5. If not → send new message, store message_id in pending.summary_message_id
```

### Phase 6: Submit All Callback

**File:** `rust-crates/ctm/src/daemon/callback_handlers.rs` (new handler)

Add `handle_submitall_callback`:

```
1. Parse callback data: submitall:{short_sid}
2. Resolve full session_id
3. Lock pending_q, get PendingQuestion
4. For each question in order:
   a. Get the tentative answer
   b. Convert to answer text:
      - Option(idx) → questions[q].options[idx].label
      - MultiOption(set) → sorted labels joined with ", "
      - FreeText(s) → s
   c. Inject answer text into tmux via input injector
   d. Set finalized[q_idx] = true
5. Edit each question message to append "✅ Submitted" and strip keyboard
6. Edit summary message to "✅ All answers submitted" and strip keyboard
7. Wait 500ms → inject "1" to auto-submit Claude Code review screen
8. Remove PendingQuestion from pending_q
```

### Phase 7: Change QN Callback

**File:** `rust-crates/ctm/src/daemon/callback_handlers.rs` (new handler)

Add `handle_change_callback`:

```
1. Parse callback data: change:{short_sid}:{q_idx}
2. Resolve full session_id
3. Lock pending_q, get mutable PendingQuestion
4. Clear tentative answer: pending.tentative.remove(q_idx)
5. Delete the summary message (Telegram deleteMessage API)
6. Set summary_message_id = None
7. Re-render question Q's message:
   - Original question text (no "Selected:" line)
   - Full keyboard with no checkmarks
   - Use edit_message_text_with_markup
8. answer_callback_query with toast: "Tap to re-select"
```

### Phase 8: Free-Text Tentative Flow

**File:** `rust-crates/ctm/src/daemon/telegram_handlers.rs`

Modify `handle_free_text_answer` (~lines 867–929):

```
1. Find PendingQuestion for this session
2. Find target question:
   a. First question with no tentative answer, OR
   b. First question that already has a FreeText tentative answer (allow replacement)
   c. Do NOT target questions with Option/MultiOption tentative answers
3. Store: pending.tentative.insert(q_idx, TentativeAnswer::FreeText(user_text))
4. Edit question message to show "📝 Your answer: {user_text}" (keep keyboard if present)
5. answer_callback_query not applicable (this is a text message, not a callback)
6. If all questions now have a tentative answer → send/update summary
```

### Phase 9: Callback Router Update

**File:** `rust-crates/ctm/src/daemon/callback_handlers.rs`

Update the callback router (~line 30–50) to recognize new prefixes:

```rust
if data.starts_with("submitall:") {
    handle_submitall_callback(ctx, bot, cb).await
} else if data.starts_with("change:") {
    handle_change_callback(ctx, bot, cb).await
} else if data.starts_with("answer:") {
    handle_answer_callback(ctx, bot, cb).await  // now tentative
} else if data.starts_with("toggle:") {
    handle_toggle_callback(ctx, bot, cb).await
} else if data.starts_with("submit:") {
    handle_submit_callback(ctx, bot, cb).await  // multi-select "Done"
}
```

### Phase 10: Question Message ID Tracking

**File:** `rust-crates/ctm/src/daemon/socket_handlers.rs`

In `handle_ask_user_question` (~lines 765–813), after sending each question message
via `send_with_buttons`, capture the returned `message_id` and store it in
`pending.question_message_ids`.

This requires either:
- A new `send_with_buttons_returning` method on the bot client, or
- Using the existing message queue but capturing the response

The `send_message_returning` method already exists at `client.rs:197` and returns
`Option<i64>` (the message_id). The question rendering should use this path instead
of the queue-based `send_with_buttons`.

---

## Callback Data Budget

Telegram enforces a **64-byte limit** on `callback_data`. Budget analysis for new
prefixes:

| Prefix | Format | Max Length |
|--------|--------|------------|
| `answer:` | `answer:{20}:{1}:{1}` | 30 bytes |
| `toggle:` | `toggle:{20}:{1}:{1}` | 30 bytes |
| `submit:` | `submit:{20}:{1}` | 28 bytes |
| `submitall:` | `submitall:{20}` | 31 bytes |
| `change:` | `change:{20}:{1}` | 28 bytes |

All formats fit comfortably within 64 bytes. The 20-character session ID prefix
provides sufficient uniqueness for concurrent sessions.

---

## Migration & Backwards Compatibility

### Breaking Changes

- The `answered: Vec<bool>` field is replaced by `finalized: Vec<bool>` with different
  semantics (only set during Submit All, not on first tap).
- The `selected_options` field is replaced by the unified `tentative` HashMap.
- The 10-minute TTL is removed.

### No Wire Protocol Changes

- Hook event format is unchanged — the daemon still receives the same JSON from
  `ctm hook`.
- Telegram message format changes are purely additive (new button prefixes, summary
  message).
- Existing callback prefixes (`answer:`, `toggle:`, `submit:`) are preserved with
  modified behavior.

### Upgrade Path

- No data migration required — `PendingQuestion` is in-memory only.
- Daemon restart clears all pending state, so old-format state never coexists with
  new-format state.

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Summary message deleted by user | Submit All button lost | Detect missing message on next selection change; re-send summary |
| Race between Change QN and Submit All | Partial submission | Hold a write lock on `pending_q` for the entire submit flow; check `finalized` before each injection |
| Telegram `editMessageText` fails with "message not modified" | Harmless but noisy | Compare new text/keyboard with current before calling edit; skip if identical |
| User never taps Submit All | Claude Code hangs waiting | Acceptable per D5 (no timeout). User can always type `/stop` or start a new session. Future work could add a reminder after N minutes. |
| Many questions (4) create a wide Change button row | Buttons too small on phone | Group Change buttons 2 per row: `[Change Q1] [Change Q2]` / `[Change Q3] [Change Q4]` |
| Free-text targeting ambiguity with multiple unanswered questions | Wrong question gets the answer | Clear targeting rules in D4: first unanswered, then first with existing free-text. Show which question received the text in the reply. |

---

## Testing Strategy

### Unit Tests

1. `TentativeAnswer` serialization and equality
2. `PendingQuestion` state transitions: tentative → change → tentative → finalize
3. Summary text rendering for various answer combinations
4. Callback data parsing for all new prefixes
5. Free-text targeting logic with multiple unanswered questions

### Integration Tests

1. Single-select: tap option A → verify checkmark → tap option B → verify checkmark
   moved → Submit All → verify injection sequence
2. Multi-select: toggle A, toggle B, toggle A (deselect) → Done → verify summary
   shows only B
3. Mixed: Q1 single-select, Q2 multi-select, Q3 free-text → all tentative → summary
   → submit
4. Change flow: answer all → summary → Change Q2 → re-select → new summary → submit
5. Free-text replacement: type "foo" → type "bar" → verify only "bar" in summary
6. Concurrent sessions: two sessions with pending questions, verify isolation

### Manual Testing

1. Phone (small screen): verify button tap targets are usable
2. Rapid tapping: tap multiple options quickly, verify no race conditions
3. Network interruption during Submit All: verify partial state is recoverable
4. Daemon restart with pending questions: verify graceful degradation

---

## Alternatives Considered

### A1: Per-Question Confirm Button

Each question gets its own "Confirm" button after selection. Once confirmed, that
question locks individually.

**Rejected because:** Adds an extra tap per question (N questions = N extra taps).
The summary confirmation provides the same safety with a single extra tap regardless
of question count.

### A2: Auto-Submit After Last Pick With Delay

Keep auto-submit but add a 5-second countdown after the last question is answered.
Show "Submitting in 5s... [Cancel]" message.

**Rejected because:** Time pressure creates anxiety. Users shouldn't have to race a
countdown to change their mind.

### A3: Persist Pending State to Disk

Save `PendingQuestion` to a JSON file so it survives daemon restarts.

**Rejected because:** Daemon restarts during active question flows are rare. The
complexity of file-based state management (locking, schema versioning, cleanup) is
not justified by the marginal reliability gain.

### A4: Single Message With All Questions

Render all questions in one Telegram message with a combined inline keyboard.

**Rejected because:** Telegram inline keyboards have no visual section dividers. With
4 questions x 4 options = 16 buttons plus control buttons, the keyboard becomes
unusable on mobile. The 4096-character message text limit is also constraining for
detailed option descriptions.

---

## References

- Telegram Bot API: `editMessageText` — https://core.telegram.org/bots/api#editmessagetext
- Telegram Bot API: `editMessageReplyMarkup` — https://core.telegram.org/bots/api#editmessagereplymarkup
- Telegram Bot API: `answerCallbackQuery` — https://core.telegram.org/bots/api#answercallbackquery
- Telegram Bot API: `deleteMessage` — https://core.telegram.org/bots/api#deletemessage
- Existing implementation: `callback_handlers.rs:357–673`
- Existing implementation: `socket_handlers.rs:668–814`
- Existing implementation: `telegram_handlers.rs:867–929`
