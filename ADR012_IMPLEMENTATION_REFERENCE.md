# ADR-012 Implementation Reference Guide

## Complete Line-by-Line Code References

### 1. Bot Client API Methods

#### 1.1 edit_message (editMessageText)
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`
**Lines:** 454-471
**Signature:**
```rust
pub async fn edit_message(
    &self,
    chat_id: i64,
    message_id: i64,
    text: &str,
    parse_mode: Option<&str>,
) -> Result<()>
```
**Limitation:** Text only, no keyboard parameter

#### 1.2 edit_message_reply_markup (editMessageReplyMarkup)
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`
**Lines:** 488-506
**Signature:**
```rust
pub async fn edit_message_reply_markup(
    &self,
    message_id: i64,
    buttons: &[InlineButton],
) -> Result<()>
```
**Limitation:** Keyboard only, no text parameter

#### 1.3 deleteMessage - NOT IMPLEMENTED ❌
**File:** N/A
**Add to:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs` (after line 506)

**Implementation template:**
```rust
/// Delete a message from the chat.
pub async fn delete_message(&self, message_id: i64) -> Result<()> {
    let _: TgResponse<bool> = self
        .api_call(
            "deleteMessage",
            &serde_json::json!({
                "chat_id": self.chat_id,
                "message_id": message_id,
            }),
        )
        .await?;
    Ok(())
}
```

#### 1.4 answer_callback_query (answerCallbackQuery)
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`
**Lines:** 711-730
**Signature:**
```rust
pub async fn answer_callback_query(
    &self,
    callback_query_id: &str,
    text: Option<&str>,
    show_alert: bool,
) -> Result<()>
```
**Key Detail (Line 713 comment):** "H4.1: `show_alert` controls whether the response is shown as a toast notification (`false`) or a modal alert dialog (`true`)."

#### 1.5 send_message_returning
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`
**Lines:** 287-316
**Signature:**
```rust
pub async fn send_message_returning(
    &self,
    text: &str,
    options: Option<&SendOptions>,
    thread_id: Option<i64>,
) -> Result<TgMessage>
```
**Returns:** `TgMessage` struct containing `message_id: i64`

#### 1.6 send_with_buttons
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`
**Lines:** 258-285
**Signature:**
```rust
pub async fn send_with_buttons(
    &self,
    text: &str,
    buttons: Vec<InlineButton>,
    options: Option<&SendOptions>,
    thread_id: Option<i64>,
)
```
**Important:** Returns `()`, not message_id (queue-based, fire-and-forget)

---

### 2. Type Definitions

#### 2.1 InlineButton Struct
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/types.rs`
**Lines:** 5-11
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}
```

#### 2.2 CallbackQuery Struct
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/types.rs`
**Lines:** 160-170
```rust
#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub id: String,                    // callback_query_id
    #[serde(default)]
    pub data: Option<String>,          // callback_data from button
    #[serde(default)]
    pub message: Option<TgMessage>,    // original message
    #[serde(default)]
    #[allow(dead_code)]
    pub from: Option<TgUser>,          // user who clicked
}
```

#### 2.3 PendingQuestion Struct
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/mod.rs`
**Lines:** 62-76
```rust
pub(super) struct PendingQuestion {
    session_id: String,
    questions: Vec<QuestionDef>,
    answered: Vec<bool>,
    selected_options: HashMap<usize, HashSet<usize>>,
    timestamp: std::time::Instant,
    #[allow(dead_code)]
    message_ids: Vec<i64>,  // Pre-positioned for future cleanup
}
```
**Note (Line 74):** "M5.5: Track Telegram message IDs associated with this question so they can be cleaned up... field exists so callers can begin tracking IDs now."

---

### 3. Helper Functions

#### 3.1 build_inline_keyboard
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/mod.rs`
**Lines:** 86-109
**Layout:** 2 buttons per row (line 98: `if (idx + 1) % 2 == 0`)

#### 3.2 resolve_pending_key (H6.1)
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/mod.rs`
**Lines:** 891-898
```rust
fn resolve_pending_key<'a>(
    pq: &'a HashMap<String, PendingQuestion>,
    short_key: &str,
) -> Option<&'a String> {
    pq.keys().find(|k| k.starts_with(short_key))
}
```
**Comment (Line 891-892):** "H6.1: Resolve a short session_id prefix (from callback_data) to the full session_id key in the pending_questions map."

---

### 4. Callback Handlers

#### 4.1 handle_answer_callback (Single-Select)
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/callback_handlers.rs`
**Lines:** 357-456

**Key steps:**
- Lines 359-363: IDOR check (ADR-006 M4.5)
- Lines 364-367: Answer callback query with "Selected" toast
- Lines 368-381: Parse callback_data (format: `answer:{short_key}:{q_idx}:{o_idx}`)
- Lines 383-388: Resolve session key
- Lines 394-396: Guard: prevent double answer
- Lines 397-404: Mark answered, extract label
- Lines 406-423: Inject answer text into tmux
- Lines 425-442: Edit message with `edit_message_text_no_markup()` (line 432)
  - Line 429: Fallback if main edit fails
- Lines 444-455: Auto-submit if all questions done

**Critical line 432:**
```rust
.edit_message_text_no_markup(msg.message_id, &updated, thread_id)
```
This removes keyboard in single call (implementation at client.rs:509-525)

#### 4.2 handle_toggle_callback (Multi-Select Toggle)
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/callback_handlers.rs`
**Lines:** 458-533

**Key steps:**
- Lines 460-465: IDOR check, answer callback (no text)
- Lines 466-478: Parse callback_data (format: `toggle:{short_key}:{q_idx}:{o_idx}`)
- Lines 480-485: Resolve session
- Lines 491-493: Guard: prevent if already answered
- Lines 495-501: Toggle option in HashSet
- Lines 503-524: Re-render keyboard with checkmarks
  - Line 510-511: Checkmark format: `"\u{2713} {}"`
  - Line 517: Generated callback_data uses short_session_id
- Lines 526-531: Edit keyboard only with `edit_message_reply_markup()` (line 529)

#### 4.3 handle_submit_callback (Multi-Select Submit)
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/callback_handlers.rs`
**Lines:** 535-640

**Key steps:**
- Lines 537-545: IDOR check, answer callback with "Submitted"
- Lines 546-554: Parse callback_data (format: `submit:{short_key}:{q_idx}`)
- Lines 556-565: Resolve session
- Lines 567-570: Mark answered
- Lines 572-593: Format answer text
  - Line 574-575: Sort selected indices for consistent output
  - Line 587: Default to "none" if no selections
  - Line 589: Join with comma: `labels.join(", ")`
- Lines 595-612: Inject into tmux
- Lines 614-630: Edit message with `edit_message_text_no_markup()`
- Lines 632-639: Auto-submit if all questions done

#### 4.4 auto_submit_answers
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/callback_handlers.rs`
**Lines:** 650-673

**Flow:**
- Line 654: Wait 500ms for Claude Code review screen
- Line 670: Inject "1" to auto-select "Submit answers"
- Line 671: Log with session_id

---

### 5. AskUserQuestion Setup

#### 5.1 handle_ask_user_question
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/socket_handlers.rs`
**Lines:** 668-814

**Key steps:**
- Lines 726-729: Generate short_session_id (max 20 chars, line 728 comment: "Telegram's 64-byte limit")
  ```rust
  let short_session_id = &msg.session_id[..std::cmp::min(20, msg.session_id.len())];
  let pending_key = msg.session_id.clone();
  ```
- Lines 731-750: Create PendingQuestion struct
- Lines 752-763: Spawn TTL cleanup task (10 minutes)
- Lines 765-812: Render questions and send via `send_with_buttons()`
  - Line 786: Answer callback format: `"answer:{short_session_id}:{q_idx}:{o_idx}"`
  - Line 791: Submit callback format: `"submit:{short_session_id}:{q_idx}"`
  - Line 797: Toggle callback format: `"toggle:{short_session_id}:{q_idx}:{o_idx}"`

#### 5.2 cleanup_pending_questions
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/socket_handlers.rs`
**Lines:** 817-827
Removes all pending questions for a session

---

### 6. Rate Limiting & Retry Logic

#### 6.1 AIMD Adaptive Rate Controller
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`
**Lines:** 5-65

- Line 28: Constructor sets initial rate to max_rate
- Lines 41-42: `on_success()` — increments by 0.5 msg/sec
- Lines 48-59: `on_rate_limit()` — halves rate, debounced to once per second
- Lines 62-64: Compute inter-message delay

#### 6.2 Queue Processing with Retry
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/queue.rs`
**Lines:** 114-205 (`process_queue` method)

- Lines 141-145: On success: `aimd.on_success()`
- Lines 146-182: On 429 rate-limit
  - Lines 156-160: Compute wait duration (prefer adaptive_retry_ms, fallback to retry_after)
  - Lines 161-164: Add ~10% jitter
  - Lines 176-181: Re-enqueue without incrementing retries
- Lines 183-201: On other errors
  - Lines 185-197: 3 retries max, exponential backoff (line 188: `1000 * 2^retries`)
  - Lines 198-200: Drop after 3 retries

#### 6.3 Special Case Handling
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/queue.rs`
**Lines:** 262-337

- Lines 262-292: TOPIC_CLOSED → reopen, retry
- Lines 294-301: TOPIC_ID_INVALID → drop (topic deleted)
- Lines 303-312: message thread not found → drop
- Lines 314-337: Entity parse error → retry as plain text

---

### 7. Cleanup & TTL

#### 7.1 Per-Question TTL Cleanup
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/socket_handlers.rs`
**Lines:** 752-763

- Line 46: TTL constant: `QUESTION_TTL_SECS = 600` (10 minutes)
- Lines 755-763: Background task spawned per question
  - Removes entry if timestamp >= TTL

#### 7.2 System Periodic Cleanup
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/cleanup.rs`
**Lines:** 10-35

- Line 43: Run interval: `CLEANUP_INTERVAL_SECS = 300` (5 minutes)
- Line 18-19: Stale session cleanup
- Line 21-22: Orphaned thread cleanup
- Line 26-27: Tool cache TTL cleanup (5 minutes)
- Line 31: Cache size limits enforcement
- Line 34: Old downloads cleanup (24 hours)

---

### 8. Configuration & Constants

#### 8.1 Rate Limit Configuration
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/client.rs`
**Lines:** 96-102
```rust
let rate = config.rate_limit.clamp(1, 30);
let quota = Quota::per_second(NonZeroU32::new(rate).unwrap());
```
**Range:** 1-30 msgs/sec (Telegram hard limit ~30)

#### 8.2 TTL Constants
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/daemon/mod.rs`
**Lines:** 43-51
- Line 44: `ECHO_TTL_SECS = 10`
- Line 45: `TOOL_CACHE_TTL_SECS = 300` (5 minutes)
- Line 46: `QUESTION_TTL_SECS = 600` (10 minutes)
- Line 47: `DOWNLOAD_MAX_AGE_SECS = 86400` (24 hours)

#### 8.3 Queue Capacity
**File:** `/opt/claude-telegram-mirror/rust-crates/ctm/src/bot/queue.rs`
**Lines:** 30-36
- Critical: 50 messages
- Normal: 300 messages
- Low: 150 messages

---

## Callback Data Format Specification

### Single-Select (Answer)
```
Format: answer:{short_session_id}:{q_idx}:{o_idx}
Example: answer:abc12345:0:1
Parsed at: callback_handlers.rs:368-381
```

### Multi-Select Toggle
```
Format: toggle:{short_session_id}:{q_idx}:{o_idx}
Example: toggle:abc12345:0:1
Parsed at: callback_handlers.rs:466-478
```

### Multi-Select Submit
```
Format: submit:{short_session_id}:{q_idx}
Example: submit:abc12345:0
Parsed at: callback_handlers.rs:546-554
```

### Size Constraints
- Short session ID: 20 chars max (socket_handlers.rs:728)
- Full payload: typically 30-50 bytes
- Telegram limit: 64 bytes per callback_data
- **Safety margin:** Well below limit ✓

---

## IDOR Protection References

**Defense-in-depth checks:** All callback handlers verify chat ownership (ADR-006 M4.5)

| Handler | File | Lines |
|---------|------|-------|
| answer | callback_handlers.rs | 359-363 |
| toggle | callback_handlers.rs | 460-464 |
| submit | callback_handlers.rs | 537-541 |
| confirm_abort | callback_handlers.rs | 46-50 |
| cancel_abort | callback_handlers.rs | 124-128 |
| approval | callback_handlers.rs | 153-157 |
| tool_details | callback_handlers.rs | 302-306 |

**Pattern:**
```rust
if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
    tracing::warn!("IDOR: callback from wrong chat");
    return;
}
```

---

## Testing Checklist for Implementation

- [ ] `deleteMessage()` implementation added and tested
- [ ] Callback data format matches specification (answer/toggle/submit)
- [ ] Short session ID generation tested for 20-char max
- [ ] resolve_pending_key() tested with multiple sessions
- [ ] Edit message + remove keyboard flow tested (line 432)
- [ ] Edit keyboard only flow tested (line 529)
- [ ] TTL cleanup removes questions after 10 minutes
- [ ] Rate limiting respects 429 signals
- [ ] IDOR checks validate callback chat ownership
- [ ] Message priority queuing works (critical > normal > low)
- [ ] Callback idempotency (double-click guard with `answered` array)
- [ ] Auto-submit delay (500ms) allows review screen render

---

## Summary: Ready for Implementation?

✅ **YES** — ADR-012 implementation can proceed with:
1. **MUST ADD:** `deleteMessage()` method (if deletion in scope)
2. **MUST DOCUMENT:** Message ID tracking strategy (field pre-positioned at daemon/mod.rs:75)
3. **MUST TEST:** Callback formatting, session resolution, IDOR guards

All other assumptions validated against working production code.
