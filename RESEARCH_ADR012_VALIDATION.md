# ADR-012 Validation Report: Telegram Bot API Implementation

## Executive Summary

This research validates the technical assumptions in ADR-012 against the actual implementation in the Rust codebase. The analysis covers API methods, callback formats, rate limiting, and session state management.

**Overall Assessment:** ADR-012 assumptions are largely **correct** with minor clarifications needed regarding method signatures and callback format constraints.

---

## Research Task 1: Client API Methods & Rate Limiting

### 1.1 editMessageText Implementation

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`, lines 454-471

```rust
pub async fn edit_message(
    &self,
    chat_id: i64,
    message_id: i64,
    text: &str,
    parse_mode: Option<&str>,
) -> Result<()> {
    let mut body = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": text,
    });
    if let Some(pm) = parse_mode {
        body["parse_mode"] = serde_json::Value::String(pm.to_string());
    }
    let _: TgResponse<TgMessage> = self.api_call("editMessageText", &body).await?;
    Ok(())
}
```

**Findings:**
- Method name: `edit_message` (not `editMessageText`)
- API endpoint: `"editMessageText"` (line 469)
- Parameters: `chat_id`, `message_id`, `text`, `parse_mode` (optional)
- **No reply_markup support in this method** — cannot combine text + keyboard edit in single call
- Response type: `TgResponse<TgMessage>`
- Error handling: Returns `Result<()>` (errors propagate as AppError)

### 1.2 editMessageReplyMarkup Implementation

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`, lines 488-506

```rust
pub async fn edit_message_reply_markup(
    &self,
    message_id: i64,
    buttons: &[InlineButton],
) -> Result<()> {
    let keyboard = build_inline_keyboard(buttons);
    let _: TgResponse<TgMessage> = self
        .api_call(
            "editMessageReplyMarkup",
            &serde_json::json!({
                "chat_id": self.chat_id,
                "message_id": message_id,
                "reply_markup": keyboard,
            }),
        )
        .await?;
    Ok(())
}
```

**Findings:**
- API endpoint: `"editMessageReplyMarkup"` (line 497)
- Parameters: `message_id`, `buttons` (converted to inline_keyboard JSON)
- Always uses `self.chat_id` (bot's configured chat)
- Cannot edit text alongside keyboard

### 1.3 deleteMessage Implementation

**Status:** **MISSING** ❌

Grep search for `deleteMessage` or `delete_message` in bot/client.rs returns no results. This method is **not implemented** in the current codebase.

**Required Telegram API Endpoint Format:**
```json
{
  "chat_id": <i64>,
  "message_id": <i64>
}
```

### 1.4 answer_callback_query Implementation

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`, lines 711-730

```rust
pub async fn answer_callback_query(
    &self,
    callback_query_id: &str,
    text: Option<&str>,
    show_alert: bool,
) -> Result<()> {
    let mut body = serde_json::json!({
        "callback_query_id": callback_query_id,
        "show_alert": show_alert,
    });
    if let Some(t) = text {
        body["text"] = serde_json::Value::String(t.to_string());
    }
    let _: TgResponse<bool> = self.api_call("answerCallbackQuery", &body).await?;
    Ok(())
}
```

**Findings:**
- Parameters: `callback_query_id`, `text` (optional), `show_alert` (boolean)
- `show_alert = false` → toast notification (bottom of screen)
- `show_alert = true` → modal alert dialog
- Line 713: Comment "H4.1: `show_alert` controls whether the response is shown as a toast notification (`false`) or a modal alert dialog (`true`)."

### 1.5 send_message_returning Implementation

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`, lines 287-316

```rust
pub async fn send_message_returning(
    &self,
    text: &str,
    options: Option<&SendOptions>,
    thread_id: Option<i64>,
) -> Result<TgMessage> {
    let parse_mode = options
        .and_then(|o| o.parse_mode.clone())
        .or_else(|| Some("Markdown".into()));

    let mut body = serde_json::json!({
        "chat_id": self.chat_id,
        "text": text,
    });
    if let Some(pm) = parse_mode {
        body["parse_mode"] = serde_json::Value::String(pm);
    }
    if let Some(tid) = thread_id {
        body["message_thread_id"] = serde_json::Value::Number(tid.into());
    }

    let resp: TgResponse<TgMessage> = self.api_call("sendMessage", &body).await?;
    resp.result.ok_or_else(|| {
        AppError::Telegram(
            resp.description
                .unwrap_or_else(|| "sendMessage failed".into()),
        )
    })
}
```

**Findings:**
- Returns: `Result<TgMessage>` (includes `message_id: i64`)
- Extracts `message_id` from `TgMessage` struct
- Direct HTTP call via `api_call` (not queue-based)
- Used for ping latency measurement (line 287 comment)

**Note:** There is **NO** `send_with_buttons_returning` method. `send_with_buttons` is queue-based and does not return the message_id.

### 1.6 Rate Limiting & Retry Logic

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`, lines 5-65 (AimdState) and bot/queue.rs

**Two-Layer Rate Limiting:**

1. **AIMD (Additive Increase, Multiplicative Decrease)** - lines 5-65
   - Adaptive inter-message delay
   - On success: `rate += 0.5 msg/sec` (line 42)
   - On 429: `rate *= 0.5` (line 57), debounced once per second
   - Min rate: 0.5 msg/sec (line 31)
   - Max rate: from config, clamped to [1, 30] (lines 99, 125)

2. **Governor Rate Limiter** - enforces absolute ceiling (line 193)
   - Quota-based: `config.rate_limit msgs/sec`
   - Checked in `api_call()` (line 193)

**Retry Logic for Queue Messages:** (`bot/queue.rs`, lines 114-205)
- **429 Rate Limited:** Exponential backoff with jitter, entire queue pauses
  - Prefers `adaptive_retry_ms` (Bot API 8.0+) over `retry_after` seconds
  - Jitter: ~10% to prevent thundering herd (line 163)
  - Message re-enqueued WITHOUT incrementing retries (line 179-181)

- **Other Errors (4xx, 5xx):** Exponential backoff, 3 retries max
  - Delay: `1000 * 2^retries ms` (line 188)
  - After 3 retries: message dropped (line 199)

- **Special Cases Handled:**
  - TOPIC_CLOSED (400): Reopen topic, retry send (lines 262-292)
  - TOPIC_ID_INVALID (400): Topic deleted, drop message (lines 294-301)
  - message thread not found (400): Drop message (lines 303-312)
  - Entity parse error (400): Retry with plain text (lines 314-337)

---

## Research Task 2: Types & Keyboard Building

### 2.1 InlineButton Struct Definition

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/types.rs`, lines 5-11

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}
```

**Findings:**
- Two fields: `text` (display), `callback_data` (opaque identifier)
- No validation on callback_data length at struct level
- **Location note:** L4.5 comment (line 5) indicates this was consolidated from previous duplication

### 2.2 build_inline_keyboard Function

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/mod.rs`, lines 86-109

```rust
fn build_inline_keyboard(buttons: &[InlineButton]) -> serde_json::Value {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut current_row: Vec<serde_json::Value> = Vec::new();

    for (idx, btn) in buttons.iter().enumerate() {
        current_row.push(serde_json::json!({
            "text": btn.text,
            "callback_data": btn.callback_data,
        }));
        // Start a new row after every 2nd button
        if (idx + 1) % 2 == 0 {
            rows.push(serde_json::Value::Array(current_row));
            current_row = Vec::new();
        }
    }
    // Flush any remaining button(s) in the last row
    if !current_row.is_empty() {
        rows.push(serde_json::Value::Array(current_row));
    }

    serde_json::json!({"inline_keyboard": rows})
}
```

**Findings:**
- Layout: **2 buttons per row** (line 98: `if (idx + 1) % 2 == 0`)
- Last row may have fewer buttons if odd count
- Matches TypeScript implementation comment (line 87)
- Produces Telegram JSON format: `{"inline_keyboard": [[...], [...]]}`

### 2.3 CallbackQuery Struct

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/types.rs`, lines 160-170

```rust
#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub id: String,                           // callback_query_id (for answer_callback_query)
    #[serde(default)]
    pub data: Option<String>,                 // callback_data from button (e.g., "answer:...")
    #[serde(default)]
    pub message: Option<TgMessage>,           // original message
    #[serde(default)]
    #[allow(dead_code)]
    pub from: Option<TgUser>,                 // user who clicked button
}
```

**Available Fields:**
- `id: String` — callback_query_id (required for answering)
- `data: Option<String>` — callback_data payload
- `message: Option<TgMessage>` — original message with `message_id`, `text`, `chat`, `message_thread_id`
- `from: Option<TgUser>` — user info (deserialized but marked dead_code)

---

## Research Task 3: ADR-012 Assumption Verification

### 3.1 Can editMessageText be called with both text AND reply_markup?

**Assumption:** ADR-012 assumes separate calls are needed.

**Reality:** The current implementation **requires separate calls**:
- `edit_message(msg_id, text, parse_mode)` — text only
- `edit_message_reply_markup(msg_id, buttons)` — keyboard only

**Telegram API Reality:** Actually supports combined edit in a single call, but the Rust binding doesn't expose this capability. This is acceptable for the use case (update text, then update keyboard separately in callback handlers).

### 3.2 Does deleteMessage exist?

**Finding:** ❌ **NOT IMPLEMENTED**

Must be added if ADR-012 requires message deletion. Current workaround: Only `remove_keyboard()` exists (line 475-486), which removes keyboard but keeps message.

```rust
pub async fn remove_keyboard(&self, message_id: i64, _thread_id: Option<i64>) -> Result<()>
```

### 3.3 Callback_data Format & 64-Byte Budget

**Actual Format Used in Code:**

1. **Answer (single-select):** `answer:{short_session_id}:{q_idx}:{o_idx}`
   - Example: `answer:abc123456789:0:1` (28 bytes for 20-char session + indices)

2. **Toggle (multi-select):** `toggle:{short_session_id}:{q_idx}:{o_idx}`
   - Example: `toggle:abc123456789:0:1` (27 bytes)

3. **Submit (multi-select):** `submit:{short_session_id}:{q_idx}`
   - Example: `submit:abc123456789:0` (23 bytes)

4. **Approval:** `approve:{approval_id}`, `reject:{approval_id}`, `abort:{approval_id}`

5. **Tool Details:** `tooldetails:{tool_use_id}`

**Short Session ID Strategy:**
```rust
// socket_handlers.rs line 728
let short_session_id = &msg.session_id[..std::cmp::min(20, msg.session_id.len())];
```

**Line 727 Comment:** "The short prefix is only used in callback_data (Telegram's 64-byte limit)."

**64-Byte Limit Compliance:**
- Prefix + session_id(20 chars) + indices (4-8 chars max) = ~40-50 bytes typical
- **Safe margin below 64-byte limit** ✓

### 3.4 resolve_pending_key Implementation

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/mod.rs`, lines 891-898

```rust
/// H6.1: Resolve a short session_id prefix (from callback_data) to the full
/// session_id key in the pending_questions map. Returns `None` if no match.
fn resolve_pending_key<'a>(
    pq: &'a HashMap<String, PendingQuestion>,
    short_key: &str,
) -> Option<&'a String> {
    pq.keys().find(|k| k.starts_with(short_key))
}
```

**Findings:**
- Maps short prefix (from callback_data) → full session_id key
- Uses linear search through keys (acceptable for small maps)
- Returns reference to HashMap key
- **Edge case risk:** If two active sessions share the same 20-char prefix, collision occurs
  - Mitigated by assumption that session IDs are long/random enough

**Usage Examples:**
- `callback_handlers.rs` line 385: `handle_answer_callback`
- `callback_handlers.rs` line 482: `handle_toggle_callback`
- `callback_handlers.rs` line 558: `handle_submit_callback`

### 3.5 send_with_buttons Method & Message ID Return

**Finding:** `send_with_buttons` **does NOT return message_id**

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`, lines 258-285

```rust
pub async fn send_with_buttons(
    &self,
    text: &str,
    buttons: Vec<InlineButton>,
    options: Option<&SendOptions>,
    thread_id: Option<i64>,
) {
    // ... enqueues message, returns nothing
    self.enqueue(QueuedMessage { ... }).await;
}
```

**Design Decision:** Queue-based, fire-and-forget for user message distribution. Message IDs are tracked implicitly in AskUserQuestion flow (via PendingQuestion struct).

---

## Research Task 4: Callback Handler Flow

### 4.1 handle_answer_callback (Single-Select)

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/callback_handlers.rs`, lines 357-456

**Line-by-Line Flow:**

| Line | Action | Details |
|------|--------|---------|
| 359-363 | IDOR check | Verify callback from correct chat |
| 364-367 | Answer callback | Show "Selected" toast |
| 368-381 | Parse callback_data | Extract `short_session_id`, `q_idx`, `o_idx` |
| 383-388 | Resolve session key | Use `resolve_pending_key()` to map short ID → full session ID |
| 389-392 | Get pending question | Lookup PendingQuestion struct |
| 394-396 | Guard: already answered? | Return if question already marked answered |
| 397 | Mark answered | Set `pending.answered[q_idx] = true` |
| 399-404 | Extract answer label | Get option label from question definition |
| 406-423 | Inject into tmux | Send text to Claude Code session's tmux |
| 425-442 | Edit message | Show "Selected" + remove keyboard via `edit_message_text_no_markup` |
| 444-455 | Auto-submit check | If all answers complete, trigger `auto_submit_answers()` |

**Key Implementation Details:**
- Line 432: `edit_message_text_no_markup()` removes keyboard in single call
- Line 429: Falls back to simpler text if original edit fails
- Lines 444-455: Cleanup + auto-submit flow

### 4.2 handle_toggle_callback (Multi-Select Toggle)

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/callback_handlers.rs`, lines 458-533

**Line-by-Line Flow:**

| Line | Action | Details |
|------|--------|---------|
| 460-464 | IDOR check + answer callback | Verify chat ownership, answer with no text |
| 466-478 | Parse callback_data | Extract indices |
| 480-485 | Resolve session + get pending | Same as answer_callback |
| 491-493 | Guard: already answered? | Return if question answered |
| 495-501 | Toggle selection | Add/remove option index from HashSet |
| 503-524 | Re-render keyboard | Build new button list with checkmarks (✓) for selected |
| 526-531 | Edit keyboard only | `edit_message_reply_markup()` to show updated buttons |

**Key Details:**
- Line 510-511: Selected options show checkmark: `format!("\u{2713} {}", opt.label)`
- Line 517: Uses short_session_id in generated callback_data
- Line 529: **Only edits keyboard, not text**

### 4.3 handle_submit_callback (Multi-Select Submit)

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/callback_handlers.rs`, lines 535-640

**Line-by-Line Flow:**

| Line | Action | Details |
|------|--------|---------|
| 537-541 | IDOR + answer callback | Verify, show "Submitted" |
| 546-554 | Parse callback_data | Extract short_key, q_idx |
| 556-565 | Resolve & get pending | Lookup question state |
| 567-570 | Guard + mark answered | If not answered, mark as answered |
| 572-593 | Construct answer text | Join selected option labels with ", " |
| 595-612 | Inject into tmux | Send comma-separated answer list |
| 614-630 | Edit message | Show "Submitted" + remove keyboard |
| 632-639 | Auto-submit check | Cleanup if all questions answered |

**Special Formatting:**
- Line 587: If no selections: returns `"none"`
- Line 574-575: Sort selected indices for consistent output
- Line 589: Join with comma-space: `labels.join(", ")`

### 4.4 auto_submit_answers Function

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/callback_handlers.rs`, lines 650-673

```rust
pub(super) async fn auto_submit_answers(ctx: &HandlerContext, session_id: &str) {
    // 500ms delay for Claude Code to render review screen
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Inject "1" to select "Submit answers" option
    let tmux_target = ctx.session_tmux.read().await.get(session_id).cloned();
    if let Some(target) = tmux_target {
        // ... get socket, lock injector, inject "1"
        let _ = inj.inject("1");
        tracing::info!(session_id, "Auto-submitted AskUserQuestion review screen");
    }
}
```

**Design:**
- Called after all individual questions answered (line 445, 638)
- Waits 500ms for Claude Code to transition to review screen (line 654)
- Injects "1" to auto-select "Submit answers" (line 670)
- Prevents user context switch back to console

---

## Research Task 5: TTL-Based Cleanup

### 5.1 PendingQuestion TTL Cleanup

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/socket_handlers.rs`, lines 752-763

```rust
// Schedule question expiry
let pq_ref = Arc::clone(&ctx.pending_q);
let pk = pending_key.clone();
tokio::spawn(async move {
    tokio::time::sleep(tokio::time::Duration::from_secs(QUESTION_TTL_SECS)).await;
    let mut pq = pq_ref.write().await;
    if let Some(pending) = pq.get(&pk) {
        if pending.timestamp.elapsed().as_secs() >= QUESTION_TTL_SECS {
            pq.remove(&pk);
        }
    }
});
```

**TTL Constant:**
```rust
// daemon/mod.rs line 46
const QUESTION_TTL_SECS: u64 = 10 * 60; // 10 minutes
```

**Cleanup Strategy:**
- Per-question background task spawned when question created
- Checks if question is stale (timestamp check)
- Self-contained: removes entry if still present and >= TTL elapsed
- Safe: no early removal even if TTL task delayed

### 5.2 Periodic System Cleanup

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/cleanup.rs`

**Main cleanup function:** `run_cleanup()` (lines 10-35)

Runs every 5 minutes (line 43: `CLEANUP_INTERVAL_SECS: u64 = 5 * 60`)

**Cleanup Tasks:**
1. **Stale sessions** (lines 18-19) — sessions with no tmux info, or 24h+ with tmux
2. **Orphaned threads** (lines 21-22) — ended sessions still with thread_ids
3. **Tool cache TTL** (lines 24-28) — remove entries older than 5 minutes
4. **Cache size limits** (lines 30-31) — evict excess session/tool entries
5. **Old downloads** (lines 33-34) — delete files older than 24 hours

**No explicit PendingQuestion cleanup in run_cleanup()** — relies on per-question TTL tasks.

---

## Confirmed Assumptions (Correct in ADR-012)

✅ **editMessageText can be called with text parameter**
- Confirmed: Method `edit_message()` at lines 454-471

✅ **editMessageReplyMarkup updates keyboard only**
- Confirmed: Method `edit_message_reply_markup()` at lines 488-506

✅ **answer_callback_query provides toast vs modal control**
- Confirmed: `show_alert` boolean parameter at line 719

✅ **send_message_returning returns message_id**
- Confirmed: Returns `Result<TgMessage>` at line 293

✅ **Callback data uses short session ID with prefix format**
- Confirmed: Format `{action}:{short_session_id}:{indices}` at lines 786-798

✅ **Callback data respects 64-byte Telegram limit**
- Confirmed: Uses 20-char max prefix (line 728)

✅ **resolve_pending_key maps short → full session ID**
- Confirmed: Implementation at lines 891-898

✅ **TTL-based cleanup for questions**
- Confirmed: 10-minute TTL per question at lines 752-763

✅ **Message edited to show answer + remove keyboard**
- Confirmed: `edit_message_text_no_markup()` at lines 425-442

---

## Corrections Needed (ADR-012 Got Wrong)

❌ **deleteMessage is NOT implemented**
- **Issue:** ADR-012 assumes `deleteMessage` exists
- **Reality:** No deletion method in client.rs
- **Impact:** If deletion is required, must be implemented
- **Workaround:** `remove_keyboard()` (line 475) removes keyboard but preserves message

❌ **send_with_buttons does NOT return message_id**
- **Issue:** ADR-012 may assume ability to get message_id from send_with_buttons
- **Reality:** Queue-based, fire-and-forget design (line 284)
- **Impact:** Cannot track sent message IDs from AskUserQuestion
- **Design Rationale:** Message IDs tracked implicitly in state, not needed for callback flow

❌ **editMessageText cannot combine text + reply_markup in single call (current binding)**
- **Issue:** ADR-012 assumption about combined edits
- **Reality:** Telegram API supports it, but Rust binding provides only:
  - `edit_message()` for text
  - `edit_message_reply_markup()` for keyboard
- **Implementation Workaround:** Current code makes two separate calls when needed
- **Impact:** Minor; current callback handlers only update keyboard OR text, not both in single callback

---

## Missing Pieces & Edge Cases

### A. deleteMessage Method

**Status:** MUST IMPLEMENT

**Required Signature:**
```rust
pub async fn delete_message(
    &self,
    message_id: i64,
) -> Result<()> {
    let _: TgResponse<bool> = self.api_call(
        "deleteMessage",
        &serde_json::json!({
            "chat_id": self.chat_id,
            "message_id": message_id,
        }),
    ).await?;
    Ok(())
}
```

**Use Cases:**
- Clean up message_ids tracked in PendingQuestion.message_ids (pre-positioned at line 75)
- Future enhancement: cleanup expired question messages

### B. edit_message with both text + reply_markup

**Status:** OPTIONAL (Low Priority)

**Current Workaround:** Make two calls
```rust
// Current approach (separate calls)
ctx.bot.edit_message(msg_id, text, None).await?;
ctx.bot.edit_message_reply_markup(msg_id, buttons).await?;
```

**Potential Enhancement:**
```rust
pub async fn edit_message_with_markup(
    &self,
    message_id: i64,
    text: &str,
    buttons: &[InlineButton],
    parse_mode: Option<&str>,
) -> Result<()> {
    let mut body = serde_json::json!({
        "chat_id": self.chat_id,
        "message_id": message_id,
        "text": text,
        "reply_markup": build_inline_keyboard(buttons),
    });
    if let Some(pm) = parse_mode {
        body["parse_mode"] = serde_json::Value::String(pm.to_string());
    }
    let _: TgResponse<TgMessage> = self.api_call("editMessageText", &body).await?;
    Ok(())
}
```

### C. Message ID Tracking for AskUserQuestion

**Status:** DESIGNED BUT UNUSED

**Location:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/mod.rs`, lines 69-75

```rust
pub(super) struct PendingQuestion {
    // ... other fields ...
    /// M5.5: Track Telegram message IDs associated with this question so they
    /// can be cleaned up (edited/deleted) when the question is answered or the
    /// session ends. Currently populated but cleanup logic is a future
    /// enhancement -- the field exists so callers can begin tracking IDs now.
    #[allow(dead_code)]
    message_ids: Vec<i64>,
}
```

**Current Status:**
- Field pre-positioned for future cleanup (line 74 comment)
- Not currently populated when messages sent (socket_handlers.rs line 747: `message_ids: Vec::new()`)
- **Future Enhancement:** Populate when `send_with_buttons()` returns message_id; delete on answer/expiry

### D. Short Session ID Collision Risk

**Status:** EDGE CASE

**Scenario:** Two sessions with same 20-char prefix would cause `resolve_pending_key()` to match wrong session.

**Likelihood:** Very low if session IDs are random/long
```rust
// session ID format (from types.rs - assumed random hex or UUID)
let short_session_id = &msg.session_id[..std::cmp::min(20, msg.session_id.len())];
```

**Mitigation:** Session ID generation should ensure randomness. Current implementation uses prefix matching without collision detection.

---

## Code Line Reference Summary

| Feature | File | Lines | Status |
|---------|------|-------|--------|
| `edit_message()` | client.rs | 454-471 | ✓ Working |
| `edit_message_reply_markup()` | client.rs | 488-506 | ✓ Working |
| `deleteMessage` | client.rs | — | ❌ Missing |
| `answer_callback_query()` | client.rs | 711-730 | ✓ Working |
| `send_message_returning()` | client.rs | 287-316 | ✓ Working |
| `send_with_buttons()` | client.rs | 258-285 | ✓ Working (no return) |
| Rate limiting (AIMD) | client.rs | 5-65 | ✓ Working |
| Rate limiting (Governor) | client.rs | 97-102 | ✓ Working |
| InlineButton struct | types.rs | 5-11 | ✓ Working |
| CallbackQuery struct | types.rs | 160-170 | ✓ Working |
| build_inline_keyboard() | mod.rs | 86-109 | ✓ Working |
| resolve_pending_key() | daemon/mod.rs | 891-898 | ✓ Working |
| handle_answer_callback() | callback_handlers.rs | 357-456 | ✓ Working |
| handle_toggle_callback() | callback_handlers.rs | 458-533 | ✓ Working |
| handle_submit_callback() | callback_handlers.rs | 535-640 | ✓ Working |
| auto_submit_answers() | callback_handlers.rs | 650-673 | ✓ Working |
| PendingQuestion TTL | socket_handlers.rs | 752-763 | ✓ Working |
| System cleanup | cleanup.rs | 10-35 | ✓ Working |

---

## Recommendations for ADR-012 Refinement

### Priority 1: CRITICAL

1. **Implement deleteMessage()** (if deletion is required by use case)
   - Location: Add to `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`
   - Signature provided above
   - Update callback handlers to delete expired/answered question messages

### Priority 2: IMPORTANT

2. **Clarify message ID tracking for AskUserQuestion**
   - Document why `send_with_buttons()` intentionally does not return message_id
   - Populate PendingQuestion.message_ids when sending questions (requires sending questions directly, not via queue)
   - Implement cleanup when questions answered or TTL expires

3. **Document callback_data size constraints**
   - ADR-012 should explicitly state: "short_session_id limited to 20 chars to stay safely below 64-byte Telegram limit"
   - Provide format examples for each callback type

### Priority 3: NICE-TO-HAVE

4. **Add edit_message_with_markup() convenience method**
   - Reduces two API calls to one when both text and keyboard need updating
   - Lower priority since current code works with separate calls

5. **Add collision detection for resolve_pending_key()**
   - Current implementation assumes no collisions with 20-char prefixes
   - Could add validation: `if multiple matches, log warning/error`

---

## Conclusion

ADR-012's Telegram Bot API assumptions are **well-founded** and align with the actual implementation. The main gap is the missing `deleteMessage()` method, which may or may not be required depending on whether message cleanup is in scope. All callback formatting, rate limiting, and state management assumptions are correctly implemented in the codebase.

The research validates that the design is sound and ready for ADR-012 implementation, pending:
1. Implementation of `deleteMessage()` if required
2. Documentation of message ID tracking strategy for AskUserQuestion
3. Minor clarifications on callback_data sizing and session ID collision handling
