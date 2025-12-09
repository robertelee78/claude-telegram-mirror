# Bash vs Node.js Hook Handler - Detailed Analysis

## Executive Summary

This document provides a comprehensive analysis of the two hook handler implementations in the `claude-telegram-mirror` package:
- **Bash Hook**: `/opt/claude-mobile/packages/claude-telegram-mirror/scripts/telegram-hook.sh`
- **Node.js Handler**: `/opt/claude-mobile/packages/claude-telegram-mirror/src/hooks/handler.ts`

**Critical Findings**:
1. **Session ID Extraction**: Both handlers correctly use Claude's native `session_id`, but with different fallback strategies
2. **Stop Event Handling**: MAJOR DIFFERENCE - Node handler clears session tracking, bash does NOT
3. **Handler Coexistence**: When both are installed, creates conflicting behavior around session lifecycle
4. **Message Duplication**: Both handlers send messages independently, causing duplicates

---

## 1. Session ID Extraction (BUG-008 Analysis)

### Bash Hook Implementation (Lines 66-108)

```bash
# Lines 66-69: Extract Claude's native session_id
CLAUDE_SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null || echo "")
debug_log "Claude session_id: $CLAUDE_SESSION_ID"

# Lines 72-89: Session tracking path generation
get_session_tracking_path() {
  mkdir -p "$CONFIG_DIR" 2>/dev/null || true
  # Use Claude's session_id as the key - this is stable for the entire session
  if [[ -n "$CLAUDE_SESSION_ID" ]]; then
    echo "$CONFIG_DIR/.session_active_${CLAUDE_SESSION_ID}"
  else
    # Fallback to tmux pane if no Claude session_id
    local session_key=""
    if [[ -n "$TMUX" ]]; then
      session_key=$(tmux display-message -p '#{session_id}_#{window_id}_#{pane_id}' 2>/dev/null || echo "")
    fi
    if [[ -z "$session_key" ]]; then
      session_key=$(tty 2>/dev/null | tr '/' '_' || echo "default")
    fi
    local safe_id=$(echo "$session_key" | tr -cd '[:alnum:]_')
    echo "$CONFIG_DIR/.session_active_${safe_id}"
  fi
}

# Lines 107-108: Final session ID selection
SESSION_ID="${CLAUDE_SESSION_ID:-$(date +%s)-$$}"
debug_log "Using session ID: $SESSION_ID"
```

**Bash Fallback Chain**:
1. **Primary**: Claude's `session_id` from hook event (stable across all events)
2. **Fallback 1**: tmux session/window/pane composite ID
3. **Fallback 2**: TTY device path (sanitized)
4. **Fallback 3**: timestamp + process ID

### Node.js Handler Implementation (Lines 527-530)

```typescript
// Line 525: Parse event from stdin
const event = JSON.parse(input) as AnyHookEvent;

// Lines 527-531: Create handler with Claude's session_id
const handler = new HookHandler({
  sessionId: event.session_id  // Always uses Claude's native ID
});

// Lines 46-50: Fallback generation (only if no sessionId provided)
private generateSessionId(): string {
  const timestamp = Date.now().toString(36);
  const random = Math.random().toString(36).slice(2, 8);
  return `hook-${timestamp}-${random}`;
}
```

**Node.js Fallback Chain**:
1. **Primary**: Claude's `session_id` from hook event (passed to constructor)
2. **Fallback**: Generated `hook-{timestamp}-{random}` (only if sessionId not provided)

### Key Differences

| Aspect | Bash Hook | Node.js Handler |
|--------|-----------|-----------------|
| **Primary Source** | Claude's `event.session_id` | Claude's `event.session_id` |
| **Fallback Strategy** | Multi-tier (tmux → TTY → generated) | Single-tier (generated ID) |
| **Session Tracking File** | `.session_active_{CLAUDE_SESSION_ID}` | `.session_active_{sessionId}` |
| **Consistency** | More complex fallback logic | Simpler, cleaner fallback |
| **Edge Cases** | Handles missing Claude ID gracefully | Assumes Claude ID always present |

**BUG-008 Relevance**:
- Both implementations correctly prioritize Claude's native `session_id`
- Bash has more robust fallback handling for edge cases (no Claude ID)
- Node.js assumes Claude always provides `session_id` (safer assumption for modern Claude versions)
- **Potential Issue**: If Claude doesn't provide `session_id`, bash creates complex fallback IDs, while Node.js creates simple generated IDs - these won't match if both handlers run

---

## 2. Stop Event Handling (BUG-006 Analysis)

### Bash Hook Implementation (Lines 443-449)

```bash
# Lines 443-449: Comment says "clear session tracking" but DOESN'T ACTUALLY DO IT!
# Clear session tracking on Stop event
# Note: We keep the session active even after Stop because Notification events
# may come after Stop and should still go to the same session thread
if [[ "$HOOK_TYPE" == "Stop" ]]; then
  debug_log "Stop event received (keeping session for potential follow-up events)"
  # Don't clear immediately - let the session timeout naturally or clear on next UserPromptSubmit
fi
```

**What Actually Happens**:
- ❌ **NO** call to `clear_session_tracking()`
- ❌ Session tracking file remains: `~/.config/claude-telegram-mirror/.session_active_{SESSION_ID}`
- ✅ Session stays "active" for future events
- ✅ Sends `turn_complete` message (NOT `session_end`)

**Stop Event Message Format (Lines 305-383)**:
```bash
case "Stop":
  # Extract transcript content...

  # Send agent_response with new text
  echo "$agent_msg"

  # Send turn_complete (NOT session_end!)
  local turn_msg=$(jq -cn \
    --arg type "turn_complete" \
    --arg sessionId "$SESSION_ID" \
    '{type: $type, sessionId: $sessionId, content: "Turn complete"}')
  echo "$turn_msg"
  ;;
```

### Node.js Handler Implementation (Lines 549-552, 155-186)

```typescript
// Lines 549-552: DOES clear session tracking on Stop event
// Clean up session tracking on Stop event
if (event.hook_event_name === 'Stop') {
  clearSessionTracking(event.session_id);
}

// Lines 155-186: handleStop method
handleStop(event: StopHookEvent): void {
  if (!this.connected) return;

  const timestamp = event.timestamp || new Date().toISOString();
  const tmuxInfo = this.detectTmuxSession();

  // Send final response if available
  if (event.transcript_summary) {
    this.send({
      type: 'agent_response',
      sessionId: this.sessionId,
      timestamp,
      content: event.transcript_summary
    });
  }

  // Send session_end (NOT turn_complete!)
  this.send({
    type: 'session_end',
    sessionId: this.sessionId,
    timestamp,
    content: 'Session stopped'
  });
}

// Lines 496-505: clearSessionTracking function
function clearSessionTracking(sessionId: string): void {
  const trackingPath = getSessionTrackingPath(sessionId);
  try {
    if (existsSync(trackingPath)) {
      unlinkSync(trackingPath);  // DELETE the tracking file!
    }
  } catch {
    // Ignore cleanup errors
  }
}
```

**What Actually Happens**:
- ✅ **DOES** call `clearSessionTracking(event.session_id)`
- ✅ Deletes session tracking file: `~/.config/claude-telegram-mirror/.session_active_{SESSION_ID}`
- ✅ Session marked as "ended"
- ❌ Sends `session_end` message (WRONG - should be `turn_complete`)

### Critical Difference Table

| Behavior | Bash Hook | Node.js Handler | Impact |
|----------|-----------|-----------------|--------|
| **Clears Tracking File** | ❌ NO | ✅ YES | Conflicting lifecycle management |
| **Message Type** | `turn_complete` | `session_end` | Different semantic meaning |
| **Session State** | Active (persistent) | Ended (cleared) | Breaking change for multi-turn sessions |
| **Follow-up Events** | ✅ Can continue | ❌ Treated as new session | BUG-006: Next turn creates new topic! |
| **Comment Accuracy** | ❌ Misleading | ✅ Accurate | Code vs comments mismatch in bash |

---

## 3. Handler Coexistence Issues

### Installation Scenarios

**Scenario 1: Both Handlers Installed**
```bash
# Both handlers registered in Claude Code hooks
~/.config/claude-code/hooks/pre_tool_use.sh -> telegram-hook.sh
~/.config/claude-code/hooks/pre_tool_use.ts -> node handler
```

**What Happens on Stop Event**:

1. **First Handler (Bash)**:
   - Reads stdin JSON
   - Sends `agent_response` with transcript content
   - Sends `turn_complete` message
   - Keeps session tracking file INTACT
   - Passes JSON to stdout (for next handler)

2. **Second Handler (Node.js)**:
   - Reads stdin JSON (from bash stdout)
   - Sends `agent_response` with transcript_summary
   - Sends `session_end` message
   - **DELETES** session tracking file
   - Session effectively ended

3. **Result**:
   - ❌ Duplicate messages sent to Telegram (both handlers send)
   - ❌ Session tracking file deleted by Node handler
   - ❌ Next user prompt creates NEW session (tracking file gone)
   - ❌ Conflicting `turn_complete` vs `session_end` messages

### Message Flow Diagram

```
Claude Code Hook Event: Stop
       │
       ├─→ telegram-hook.sh
       │   ├─ Send: agent_response
       │   ├─ Send: turn_complete
       │   ├─ Keep: .session_active_abc123
       │   └─ Pass: JSON to stdout
       │
       └─→ node handler (reads from stdin)
           ├─ Send: agent_response (DUPLICATE!)
           ├─ Send: session_end (CONFLICTS!)
           └─ Delete: .session_active_abc123 (BREAKS SESSION!)
```

**Telegram Thread Impact**:
```
Topic #42 for session-abc123:
  [10:00] 👋 Session Started
  [10:01] User: "analyze this code"
  [10:02] 🤖 [bash] Agent response text
  [10:02] ✅ [bash] Turn complete
  [10:02] 🤖 [node] Agent response text (DUPLICATE!)
  [10:02] 👋 [node] Session Ended (WRONG!)

  [10:05] User: "continue with that"
  [ERROR] Session tracking file missing!
  [RESULT] Creates NEW session topic #43 instead of continuing #42
```

---

## 4. tmux Target Detection and Handling

### Bash Hook Implementation (Lines 112-137)

```bash
get_tmux_info() {
  if [[ -z "$TMUX" ]]; then
    echo "{}"
    return
  fi

  # $TMUX format: /path/to/socket,pid,index
  # Extract socket path (everything before first comma)
  local socket_path="${TMUX%%,*}"

  local session=$(tmux display-message -p "#S" 2>/dev/null || echo "")
  local pane=$(tmux display-message -p "#P" 2>/dev/null || echo "")
  local window=$(tmux display-message -p "#I" 2>/dev/null || echo "")

  if [[ -n "$session" && -n "$window" && -n "$pane" ]]; then
    local target="${session}:${window}.${pane}"
    jq -cn \
      --arg session "$session" \
      --arg pane "$pane" \
      --arg target "$target" \
      --arg socket "$socket_path" \
      '{tmuxSession: $session, tmuxPane: $pane, tmuxTarget: $target, tmuxSocket: $socket}'
  else
    echo "{}"
  fi
}
```

**Bash Features**:
- ✅ Extracts socket path from `$TMUX` environment variable
- ✅ Uses `display-message` for current session/window/pane
- ✅ Constructs full target: `session:window.pane`
- ✅ Returns structured JSON object
- ✅ Handles missing tmux gracefully (returns empty JSON)

### Node.js Handler Implementation (Lines 112-133)

```typescript
private detectTmuxSession(): {
  session: string | null;
  pane: string | null;
  target: string | null;
  socket: string | null
} {
  // Check if we're inside tmux
  if (!process.env.TMUX) {
    return { session: null, pane: null, target: null, socket: null };
  }

  try {
    // Extract socket path from $TMUX env var (format: /path/to/socket,pid,index)
    const socket = process.env.TMUX.split(',')[0] || null;

    const session = execSync('tmux display-message -p "#S"', { encoding: 'utf8' }).trim();
    const pane = execSync('tmux display-message -p "#P"', { encoding: 'utf8' }).trim();
    const windowIndex = execSync('tmux display-message -p "#I"', { encoding: 'utf8' }).trim();

    // Full target for send-keys: session:window.pane
    const target = `${session}:${windowIndex}.${pane}`;

    return { session, pane, target, socket };
  } catch {
    return { session: null, pane: null, target: null, socket: null };
  }
}
```

**Node.js Features**:
- ✅ Extracts socket path from `$TMUX` environment variable
- ✅ Uses `execSync` with `display-message` for session/window/pane
- ✅ Constructs full target: `session:window.pane`
- ✅ Returns structured TypeScript object
- ✅ Handles missing tmux gracefully (returns nulls)

### Comparison

| Feature | Bash Hook | Node.js Handler | Notes |
|---------|-----------|-----------------|-------|
| **Socket Extraction** | `${TMUX%%,*}` | `TMUX.split(',')[0]` | Equivalent logic |
| **Session Detection** | `display-message -p "#S"` | `display-message -p "#S"` | Identical |
| **Window Detection** | `#I` | `#I` | Identical |
| **Pane Detection** | `#P` | `#P` | Identical |
| **Target Format** | `session:window.pane` | `session:window.pane` | Identical |
| **Error Handling** | Returns `{}` | Returns object with nulls | Different but equivalent |
| **Execution** | Native shell | `execSync` (subprocess) | Bash is faster |
| **Extra Fields** | Includes `tmuxSession`, `tmuxPane` | Only in metadata | Bash more verbose |

**Performance**: Bash implementation is slightly faster (native shell vs spawning subprocesses)
**Compatibility**: Both work identically on Linux and macOS

---

## 5. Message Formatting and Forwarding

### Bash Hook Message Flow (Lines 237-418)

```bash
format_message() {
  local hook_type="$1"
  local input="$2"
  local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

  # Get current tmux info for EVERY message (enables auto-refresh on pane changes)
  local tmux_info=$(get_tmux_info)

  case "$hook_type" in
    "PreToolUse")
      jq -cn --arg type "tool_start" --argjson tmux "$tmux_info" '{...}'
      ;;
    "PostToolUse")
      jq -cn --arg type "tool_result" --argjson tmux "$tmux_info" '{...}'
      ;;
    "Notification")
      # Skip idle_prompt notifications
      if [[ "$notification_type" == "idle_prompt" ]]; then
        return
      fi
      jq -cn --arg type "agent_response" --argjson tmux "$tmux_info" '{...}'
      ;;
    "Stop")
      # Extract transcript, send agent_response + turn_complete
      ;;
    "UserPromptSubmit")
      jq -cn --arg type "user_input" --argjson tmux "$tmux_info" '{...}'
      ;;
    "PreCompact")
      jq -cn --arg type "pre_compact" --argjson tmux "$tmux_info" '{...}'
      ;;
  esac
}

# Lines 452-464: Send all formatted messages
MESSAGES=$(format_message "$HOOK_TYPE" "$INPUT")
if [[ -n "$MESSAGES" ]]; then
  while IFS= read -r msg; do
    if [[ -n "$msg" ]]; then
      send_to_bridge "$msg"  # Uses netcat to Unix socket
    fi
  done <<< "$MESSAGES"
fi
```

**Bash Characteristics**:
- ✅ Includes tmux info in EVERY message (for auto-refresh - BUG-001 fix)
- ✅ Filters out `idle_prompt` notifications (noise reduction)
- ✅ Supports multiple hook types: PreToolUse, PostToolUse, Notification, Stop, UserPromptSubmit, PreCompact
- ✅ Handles multiple messages per hook (Stop returns 2 messages)
- ✅ Uses netcat for fast socket communication
- ✅ Fallback to direct write if netcat unavailable

### Node.js Handler Message Flow (Lines 431-473)

```typescript
async processEvent(event: AnyHookEvent): Promise<string | null> {
  switch (event.hook_event_name) {
    case 'Stop':
      this.handleStop(event);
      return null;

    case 'SubagentStop':
      return null;  // Just log for now

    case 'PreToolUse':
      const result = await this.handlePreToolUse(event);
      if (result) {
        return JSON.stringify({
          hookSpecificOutput: {
            hookEventName: 'PreToolUse',
            permissionDecision: result.permissionDecision,
            permissionDecisionReason: result.permissionDecisionReason
          }
        });
      }
      return null;

    case 'PostToolUse':
      this.handlePostToolUse(event);
      return null;

    case 'Notification':
      this.handleNotification(event);
      return null;

    case 'UserPromptSubmit':
      this.handleUserPromptSubmit(event);
      return null;

    default:
      if (this.config.verbose) {
        console.error('[telegram-hook] Unknown event type:', event.hook_event_name);
      }
      return null;
  }
}

// Each handler includes tmux info (examples):
handlePostToolUse(event: PostToolUseHookEvent): void {
  const tmuxInfo = this.detectTmuxSession();
  this.send({
    type: 'tool_result',
    metadata: {
      tmuxTarget: tmuxInfo.target,
      tmuxSocket: tmuxInfo.socket
    }
  });
}
```

**Node.js Characteristics**:
- ✅ Includes tmux info in EVERY message (for auto-refresh - BUG-001 fix)
- ✅ Supports SubagentStop (not in bash)
- ✅ Returns hookSpecificOutput for PreToolUse (approval system integration)
- ✅ Type-safe event handling with TypeScript
- ✅ Uses SocketClient class for reliable communication
- ❌ No explicit idle_prompt filtering (relies on daemon)

### Message Format Differences

**Bash Format** (Stop event):
```json
{
  "type": "agent_response",
  "sessionId": "session-abc123",
  "timestamp": "2025-12-08T10:00:00Z",
  "content": "Full transcript text from .jsonl file",
  "metadata": {
    "tmuxTarget": "1:0.0",
    "tmuxSocket": "/tmp/tmux-1000/default"
  }
}
{
  "type": "turn_complete",
  "sessionId": "session-abc123",
  "timestamp": "2025-12-08T10:00:00Z",
  "content": "Turn complete",
  "metadata": {
    "tmuxTarget": "1:0.0",
    "tmuxSocket": "/tmp/tmux-1000/default"
  }
}
```

**Node.js Format** (Stop event):
```json
{
  "type": "agent_response",
  "sessionId": "session-abc123",
  "timestamp": "2025-12-08T10:00:00Z",
  "content": "Transcript summary from event.transcript_summary",
  "metadata": {
    "tmuxTarget": "1:0.0",
    "tmuxSocket": "/tmp/tmux-1000/default"
  }
}
{
  "type": "session_end",
  "sessionId": "session-abc123",
  "timestamp": "2025-12-08T10:00:00Z",
  "content": "Session stopped",
  "metadata": {
    "tmuxTarget": "1:0.0",
    "tmuxSocket": "/tmp/tmux-1000/default"
  }
}
```

**Key Differences**:
1. **Content Source**: Bash reads full `.jsonl` transcript, Node uses `event.transcript_summary`
2. **Second Message**: Bash sends `turn_complete`, Node sends `session_end`
3. **Semantic Meaning**: Bash treats Stop as "turn complete", Node treats as "session over"

---

## 6. Real-Time Transcript Sync (Bash Only)

### Disabled Feature (Lines 174-234)

```bash
# ============================================
# REAL-TIME TRANSCRIPT SYNC
# Extract new assistant text on EVERY hook event
# This provides real-time mirroring of Claude's responses
# ============================================
sync_new_assistant_text() {
  local transcript_path=$(echo "$INPUT" | jq -r '.transcript_path // ""' 2>/dev/null)

  # Track last processed line to avoid duplicates
  local state_file="$CONFIG_DIR/.sync_${SESSION_ID}"
  local last_line=0
  if [[ -f "$state_file" ]]; then
    last_line=$(cat "$state_file" 2>/dev/null || echo 0)
  fi

  local current_line=$(wc -l < "$transcript_path")

  # Only process new lines
  if [[ $current_line -gt $last_line ]]; then
    # Extract assistant text from new lines
    # Send as agent_response messages
  fi

  # Update state file
  echo "$current_line" > "$state_file"
}

# ============================================
# REAL-TIME SYNC: DISABLED - causing replay issues
# TODO: Fix race conditions before re-enabling
# ============================================
# sync_new_assistant_text &
```

**Features**:
- 📝 Tracks last processed line of transcript for incremental sync
- 📝 Would send agent responses in real-time (not just on Stop)
- ❌ **DISABLED** due to race conditions and replay issues
- ❌ Background process would cause complexity

**Why Node.js Handler Doesn't Have This**:
- Node.js relies on `event.transcript_summary` provided by Claude
- Simpler approach without file I/O
- No race condition issues
- Works for end-of-turn summaries

---

## 7. Key Differences Summary Table

| Feature | Bash Hook | Node.js Handler | Winner |
|---------|-----------|-----------------|--------|
| **Session ID Extraction** | Multi-tier fallback | Simple fallback | Bash (more robust) |
| **Stop Event Handling** | Keeps session alive | Ends session | Bash (correct behavior) |
| **Session Tracking Cleanup** | ❌ Doesn't clear | ✅ Clears | Bash (for multi-turn) |
| **Message Type on Stop** | `turn_complete` | `session_end` | Bash (semantically correct) |
| **Transcript Extraction** | Full `.jsonl` parsing | `event.transcript_summary` | Node (simpler) |
| **tmux Detection** | Native shell | `execSync` subprocess | Bash (faster) |
| **Message Filtering** | Filters `idle_prompt` | No filtering | Bash (cleaner) |
| **Approval System** | Basic forwarding | Full hookSpecificOutput | Node (feature-complete) |
| **Type Safety** | ❌ Bash script | ✅ TypeScript | Node (safer) |
| **Performance** | ⚡ Fast (native) | 🐢 Slower (Node startup) | Bash |
| **Maintainability** | 📝 Comments + logic mismatch | ✅ Clear code | Node |
| **SubagentStop Support** | ❌ No | ✅ Yes | Node |
| **PreCompact Support** | ✅ Yes | ❌ No | Bash |
| **Real-time Sync** | 🔧 Disabled feature | ❌ N/A | N/A |

---

## 8. Recommended Configuration

### Option 1: Use Bash Hook Only (Recommended for Multi-Turn Sessions)

**Pros**:
- ✅ Correct `turn_complete` semantics
- ✅ Session persistence across turns
- ✅ Faster execution (native shell)
- ✅ Filters noise (idle_prompt)

**Cons**:
- ❌ No approval system integration
- ❌ No TypeScript type safety
- ❌ Comments don't match behavior (lines 443-449)

**Installation**:
```bash
# Install only bash hook
ln -sf "$(pwd)/scripts/telegram-hook.sh" ~/.config/claude-code/hooks/
```

### Option 2: Use Node.js Handler Only (Current Default)

**Pros**:
- ✅ Full approval system support
- ✅ TypeScript type safety
- ✅ Cleaner codebase
- ✅ SubagentStop support

**Cons**:
- ❌ **CRITICAL BUG**: Treats Stop as session end (breaks multi-turn)
- ❌ Slower startup (Node.js process)
- ❌ Wrong semantic meaning for Stop event

**Installation**:
```bash
# Install only Node handler
npm run hook:install
```

### Option 3: Use Both (NOT RECOMMENDED)

**Issues**:
- ❌ Duplicate messages
- ❌ Conflicting session lifecycle
- ❌ Bash keeps session alive, Node kills it
- ❌ Creates new topics for continuing conversations

**Only Use If**:
- You need approval system (Node) AND correct Stop handling (Bash)
- You're willing to accept message duplication
- You implement deduplication in daemon

---

## 9. Critical Bugs Identified

### BUG-006: Node Handler Ends Session on Every Stop Event

**File**: `/opt/claude-mobile/packages/claude-telegram-mirror/src/hooks/handler.ts`
**Lines**: 549-552, 175-186

**Issue**:
```typescript
// Clean up session tracking on Stop event
if (event.hook_event_name === 'Stop') {
  clearSessionTracking(event.session_id);  // DELETES tracking file!
}

handleStop(event: StopHookEvent): void {
  // Send session_end
  this.send({
    type: 'session_end',  // WRONG! Should be turn_complete
    sessionId: this.sessionId,
    content: 'Session stopped'
  });
}
```

**Impact**:
1. Every Claude turn (Stop event) deletes session tracking file
2. Next user prompt creates NEW session (file missing)
3. Creates NEW Telegram topic instead of continuing in existing topic
4. Multi-turn conversations broken

**Fix Required**:
```typescript
// DON'T clear on Stop - Claude fires Stop after every turn!
if (event.hook_event_name === 'Stop') {
  // Session still active - don't clear tracking
  // Only clear on explicit session end (process exit, user /exit, etc.)
}

handleStop(event: StopHookEvent): void {
  // Send turn_complete (not session_end!)
  this.send({
    type: 'turn_complete',  // FIXED
    sessionId: this.sessionId,
    content: 'Turn complete'
  });
}
```

### BUG-008: Session ID Fallback Mismatch

**Issue**: When both handlers run and Claude doesn't provide `session_id`:
- Bash generates: `{tmux_session_id}_{window_id}_{pane_id}` or TTY-based ID
- Node generates: `hook-{timestamp}-{random}`
- **Result**: Two different session IDs for the same Claude session!

**Impact**:
- Messages split across two Telegram topics
- Session state fragmented
- Confusing user experience

**Fix Required**: Use consistent fallback generation or ensure Claude always provides `session_id`

### BUG-COMMENT: Misleading Bash Comment

**File**: `/opt/claude-mobile/packages/claude-telegram-mirror/scripts/telegram-hook.sh`
**Lines**: 443-449

**Issue**:
```bash
# Clear session tracking on Stop event  <-- MISLEADING COMMENT!
# Note: We keep the session active even after Stop because Notification events
# may come after Stop and should still go to the same session thread
if [[ "$HOOK_TYPE" == "Stop" ]]; then
  debug_log "Stop event received (keeping session for potential follow-up events)"
  # Don't clear immediately - let the session timeout naturally or clear on next UserPromptSubmit
fi
```

**Problem**: Comment title says "Clear session tracking" but code explicitly DOESN'T clear it!

**Fix Required**:
```bash
# Keep session tracking active on Stop event
# Note: We keep the session active even after Stop because Notification events
# may come after Stop and should still go to the same session thread
if [[ "$HOOK_TYPE" == "Stop" ]]; then
  debug_log "Stop event received (keeping session for potential follow-up events)"
  # Don't clear - Claude fires Stop after every turn, not when session ends
fi
```

---

## 10. Recommendations

### Immediate Actions

1. **Fix Node.js Handler** (Priority: CRITICAL):
   - Change `session_end` to `turn_complete` in handleStop
   - Remove `clearSessionTracking()` call on Stop event
   - Only clear session on explicit session end events

2. **Fix Bash Hook Comments** (Priority: LOW):
   - Update misleading comment at line 443
   - Clarify session lifecycle expectations

3. **Choose Single Handler** (Priority: HIGH):
   - Recommend bash hook for now (correct Stop behavior)
   - Fix Node handler before making it default
   - Document installation choice clearly

4. **Document Semantic Difference** (Priority: MEDIUM):
   - Clarify Stop event ≠ session end
   - Stop event = turn complete (Claude response finished)
   - Session end = user exits or connection drops

### Long-Term Improvements

1. **Unified Handler**:
   - Port bash hook logic to TypeScript
   - Keep bash performance with Node safety
   - Single source of truth

2. **Session Lifecycle Events**:
   - Add explicit `SessionEnd` hook event to Claude Code
   - Separate turn completion from session termination
   - Better semantic clarity

3. **Deduplication Layer**:
   - If both handlers must coexist, add dedup in daemon
   - Track message hashes to prevent duplicate sends
   - Still fix underlying issues

4. **Testing**:
   - Add integration tests for multi-turn conversations
   - Verify session persistence across turns
   - Test handler coexistence scenarios

---

## Appendix A: get-chat-id.sh Analysis

**File**: `/opt/claude-mobile/packages/claude-telegram-mirror/scripts/get-chat-id.sh`

**Purpose**: Helper script to retrieve Telegram chat IDs for bot configuration

**Key Features**:
```bash
# Call Telegram API
RESPONSE=$(curl -s "https://api.telegram.org/bot${TOKEN}/getUpdates")

# Extract unique chat IDs
CHAT_IDS=$(echo "$RESPONSE" | grep -o '"chat":{"id":-\?[0-9]*' | grep -o '\-\?[0-9]*' | sort -u)

# Determine chat type based on ID format
if [[ "$ID" =~ ^-100 ]]; then
  TYPE="supergroup"  # Required for Topics/Forums
elif [[ "$ID" =~ ^- ]]; then
  TYPE="group"
else
  TYPE="private chat"
fi
```

**Supergroup Detection**:
- IDs starting with `-100` are supergroups (support Topics/Forums)
- Regular groups start with `-` (no Topic support)
- Private chats have positive IDs

**Relation to Main System**:
- Required first step before running bridge daemon
- `TELEGRAM_CHAT_ID` must be supergroup ID for Topics feature
- No direct interaction with hook handlers
- Standalone utility script

---

## Appendix B: File Reference Table

| File Path | Purpose | Key Lines |
|-----------|---------|-----------|
| `/opt/claude-mobile/packages/claude-telegram-mirror/scripts/telegram-hook.sh` | Bash hook handler | 66-108 (session ID), 305-383 (Stop), 443-449 (BUG-006) |
| `/opt/claude-mobile/packages/claude-telegram-mirror/src/hooks/handler.ts` | Node.js hook handler | 527-530 (session ID), 155-186 (Stop), 549-552 (BUG-006) |
| `/opt/claude-mobile/packages/claude-telegram-mirror/scripts/get-chat-id.sh` | Chat ID helper | 20-40 (API call), 49-60 (type detection) |
| `/opt/claude-mobile/packages/claude-telegram-mirror/docs/SESSION_MAPPING_ARCHITECTURE.md` | Session mapping docs | Complete architecture reference |
| `/opt/claude-mobile/packages/claude-telegram-mirror/src/bridge/daemon.ts` | Bridge daemon | 186-187 (session_end handler), 574-596 (handleSessionEnd) |

---

**Document Version**: 1.0
**Date**: 2025-12-08
**Author**: Code Quality Analyzer (Claude Sonnet 4.5)
**Related Issues**: BUG-006 (Stop event session clearing), BUG-008 (Session ID extraction)
