# Race Condition Safety Analysis: BUG-010 Fix

## Executive Summary

**CRITICAL RACE CONDITIONS DETECTED**: The BUG-010 fix (calling `handleSessionStart()` directly from `ensureSessionExists()`) introduces **3 critical race conditions** and **1 potential deadlock scenario**.

**Risk Level**: 🔴 **HIGH** - Duplicate topics, dropped messages, and database corruption possible under concurrent load.

---

## The Change (Lines 678-700)

**OLD FLOW (BUG-002 fix):**
```typescript
// ensureSessionExists() created session and Promise
topicCreationPromises.set(sessionId, new Promise(...))
// Later: handleSessionStart() resolved the Promise
resolver(threadId)
```

**NEW FLOW (BUG-010 fix):**
```typescript
// ensureSessionExists() calls handleSessionStart() directly
await this.handleSessionStart(msg);
```

**Problem**: The Promise-based synchronization was **REMOVED** but `waitForTopic()` still **EXPECTS IT**.

---

## Race Condition #1: Duplicate Topics for Same Session

### Scenario
Two hook events arrive simultaneously for a **new** session (e.g., `tool_result` and `agent_response`):

```
Time  Thread A                           Thread B
----  ---------------------------------  ---------------------------------
T0    ensureSessionExists(session-123)   ensureSessionExists(session-123)
T1    getSession() → null                getSession() → null
T2    handleSessionStart() START         handleSessionStart() START
T3    createSession() → OK               createSession() → OK (idempotent)
T4    bot.createForumTopic() → 42       bot.createForumTopic() → 43
T5    setSessionThread(42)               setSessionThread(43) ← OVERWRITES
```

### Root Cause
- **No locking** between lines 679-699
- `createSession()` is idempotent but `bot.createForumTopic()` is **NOT**
- Both threads create different Telegram topics
- Database stores **whichever writes last** (line 606)

### Impact
- **2 Telegram topics** for the same session
- Messages randomly routed to wrong topic
- User sees split conversation

### Evidence in Code
```typescript
// daemon.ts:678-691
private async ensureSessionExists(msg: BridgeMessage): Promise<void> {
  const existing = this.sessions.getSession(msg.sessionId);
  if (existing) {  // ← CHECK
    // ... reactivate logic
    return;
  }

  // ⚠️ RACE WINDOW: Another thread can pass the check above
  logger.info('Creating session on-the-fly', { sessionId: msg.sessionId });
  await this.handleSessionStart(msg);  // ← EXPENSIVE async operation
}
```

```typescript
// daemon.ts:600-609 (inside handleSessionStart)
} else if (this.config.useThreads) {
  const topicName = this.formatTopicName(sessionId, hostname, projectDir);
  threadId = await this.bot.createForumTopic(topicName, 0);  // ← NOT idempotent

  if (threadId) {
    this.sessions.setSessionThread(sessionId, threadId);  // ← Last write wins
    this.sessionThreads.set(sessionId, threadId);
  }
}
```

### Likelihood
**HIGH** - Multi-hook systems (Claude Code with observers/plugins) fire events in parallel.

---

## Race Condition #2: Message Ordering Violation

### Scenario
`tool_result` arrives **BEFORE** `tool_start` for the same session:

```
Time  Event
----  -------------------------
T0    tool_result arrives
T1    ensureSessionExists() → creates session + topic
T2    handleToolResult() displays message in topic 42
T3    tool_start arrives
T4    ensureSessionExists() → session exists, returns
T5    handleToolStart() displays message in topic 42
```

### Result
User sees **RESULT before START** (temporal paradox in UI)

### Root Cause
- Socket events are **not ordered** (network can reorder)
- No event sequencing logic
- `ensureSessionExists()` creates topic immediately on first event

### Impact
- Confusing UI: "✅ File written" appears before "🔧 Running: Write"
- Cannot reconstruct operation timeline
- Debugging becomes impossible

### Evidence
```typescript
// daemon.ts:203-210 (setupSocketHandlers)
case 'tool_start':
  await this.ensureSessionExists(msg);  // ← May be second
  await this.handleToolStart(msg);
  break;

case 'tool_result':
  await this.ensureSessionExists(msg);  // ← May be first!
  await this.handleToolResult(msg);
  break;
```

### Likelihood
**MEDIUM** - Depends on network jitter between hook events.

---

## Race Condition #3: Promise Deadlock

### Scenario
Messages arrive **while** topic is being created:

```
Time  Thread A                           Thread B
----  ---------------------------------  ---------------------------------
T0    ensureSessionExists() START
T1    handleSessionStart() START
T2    bot.createForumTopic() → waiting   handleToolResult() arrives
T3                                        ensureSessionExists() → exists
T4                                        waitForTopic() called
T5                                        Check topicCreationPromises
T6                                        → Map is EMPTY (never set!)
T7                                        Return undefined
T8    Topic created (threadId=42)
T9    Resolve promise (BUT NO PROMISE!)  Message DROPPED
```

### Root Cause
**BUG-010 removed Promise creation** but **kept Promise consumption**:

```typescript
// daemon.ts:614-620 (handleSessionStart - NEVER creates Promise anymore)
const resolver = this.topicCreationResolvers.get(sessionId);
if (resolver) {
  resolver(threadId || undefined);  // ← resolver will be undefined
  this.topicCreationPromises.delete(sessionId);
  this.topicCreationResolvers.delete(sessionId);
}
```

```typescript
// daemon.ts:729-773 (waitForTopic - still EXPECTS Promise)
const promise = this.topicCreationPromises.get(sessionId);
if (!promise) {
  // ⚠️ No pending creation - assume topic doesn't exist
  return undefined;  // ← FALSE NEGATIVE!
}

// Wait for Promise that will NEVER be created
const result = await Promise.race([promise, timeout]);
```

### Impact
- **Messages silently dropped** during topic creation (lines 804-806, 823-825, etc.)
- No error logging (logger.error but message lost)
- User doesn't know why messages are missing

### Evidence
```typescript
// daemon.ts:801-807 (handleAgentResponse)
private async handleAgentResponse(msg: BridgeMessage): Promise<void> {
  const threadId = await this.waitForTopic(msg.sessionId);
  if (threadId === undefined && this.config.useThreads) {
    logger.error('Topic creation timeout - dropping agent_response', { sessionId: msg.sessionId });
    return;  // ← MESSAGE LOST
  }
  await this.bot.sendMessage(...);
}
```

**All message handlers use this pattern**:
- `handleAgentResponse()` - line 803
- `handleToolStart()` - line 822
- `handleToolResult()` - line 897
- `handleUserInput()` - line 928
- `handleApprovalRequest()` - line 947
- `handleError()` - line 967
- `handlePreCompact()` - line 982

### Likelihood
**VERY HIGH** - Guaranteed on fast networks where events arrive during `bot.createForumTopic()`.

---

## Deadlock Scenario: Circular Wait (Theoretical)

### Scenario
If `handleSessionStart()` ever calls code that invokes `ensureSessionExists()`:

```
Thread A: ensureSessionExists() → handleSessionStart() → [some code] → ensureSessionExists()
```

### Current Status
**✅ SAFE** - No circular call path found in current code.

### Code Inspection
- `handleSessionStart()` calls:
  - `this.sessions.*` (SessionManager - no daemon calls)
  - `this.bot.*` (TelegramBot - no daemon calls)
  - `this.socket.broadcast()` (SocketServer - no daemon calls)
- No path back to `ensureSessionExists()`

### Future Risk
**MEDIUM** - Refactoring could introduce circular dependency.

---

## Root Cause Analysis

### Design Flaw: Mixed Synchronization Models

| Component | Model | Issue |
|-----------|-------|-------|
| `ensureSessionExists()` | **Direct call** | Creates topics immediately |
| `waitForTopic()` | **Promise-based** | Expects async coordination |
| `handleSessionStart()` | **Hybrid** | Resolves promises that don't exist |

**The BUG-010 fix removed Promise creation but kept Promise consumption.**

### Why BUG-002 Fix Was Correct

The original Promise pattern **WAS** solving a real problem:

```
Scenario: tool_result arrives BEFORE session_start
----------------------------------------------------
T0: tool_result arrives
T1: ensureSessionExists() creates session (NO topic yet)
T2: ensureSessionExists() creates Promise
T3: waitForTopic() waits on Promise
T4: session_start arrives
T5: handleSessionStart() creates topic
T6: handleSessionStart() resolves Promise
T7: waitForTopic() returns threadId
T8: Message displayed in correct topic
```

**BUG-010 broke this by making topic creation synchronous.**

---

## Impact Assessment

### Severity Matrix

| Race Condition | Likelihood | Impact | Severity |
|----------------|-----------|---------|----------|
| **#1: Duplicate Topics** | High | Medium | 🔴 **CRITICAL** |
| **#2: Message Ordering** | Medium | Low | 🟡 **MODERATE** |
| **#3: Promise Deadlock** | Very High | High | 🔴 **CRITICAL** |
| **Circular Deadlock** | Low | Critical | 🟢 **LOW** |

### User-Facing Issues

1. **Duplicate Topics** (RC#1)
   - Multiple topics for same session
   - Messages split across topics
   - Cannot follow conversation

2. **Missing Messages** (RC#3)
   - Agent responses dropped
   - Tool executions invisible
   - Approval requests lost

3. **Temporal Confusion** (RC#2)
   - Results appear before commands
   - Debugging timeline broken
   - Cannot reproduce issues

### System Issues

1. **Database Corruption** (RC#1)
   - `sessionThreads` Map ≠ `sessions.db`
   - Cache invalidation impossible
   - Daemon restart = wrong routing

2. **Silent Failures** (RC#3)
   - No retry mechanism
   - No user notification
   - No recovery path

---

## Code Evidence: Call Flow Analysis

### Complete Flow (New Code)

```
Message Arrives
  ↓
setupSocketHandlers() (line 176)
  ↓
switch(msg.type) {
  case 'agent_response':
    ↓
    ensureSessionExists(msg) ← LINE 199
      ↓
      existing = getSession(msg.sessionId) ← LINE 679
      ↓
      if (!existing) {
        ↓
        handleSessionStart(msg) ← LINE 699 (DIRECTLY CALLED)
          ↓
          createSession() ← LINE 577
          ↓
          bot.createForumTopic() ← LINE 603 (ASYNC - RACE WINDOW)
          ↓
          setSessionThread(threadId) ← LINE 606
          ↓
          resolver = topicCreationResolvers.get() ← LINE 614
          ↓
          if (resolver) { ← FALSE (never set)
            resolver(threadId)
          }
      }
    ↓
    handleAgentResponse(msg) ← LINE 200
      ↓
      waitForTopic(msg.sessionId) ← LINE 803
        ↓
        existing = getSessionThreadId() ← LINE 731
        ↓
        if (existing) return existing ← FAST PATH (if lucky)
        ↓
        promise = topicCreationPromises.get() ← LINE 735
        ↓
        if (!promise) return undefined ← PROMISE NEVER CREATED!
        ↓
        ⚠️ RESULT: Message dropped (line 805)
```

### Critical Lines

| Line | Code | Issue |
|------|------|-------|
| 679 | `existing = getSession(...)` | No lock before check |
| 699 | `await handleSessionStart(msg)` | Async = race window |
| 603 | `await bot.createForumTopic(...)` | Not idempotent |
| 606 | `setSessionThread(sessionId, threadId)` | Last write wins |
| 614 | `resolver = topicCreationResolvers.get(...)` | Always undefined |
| 735 | `promise = topicCreationPromises.get(...)` | Always undefined |
| 805 | `return; // MESSAGE LOST` | No recovery |

---

## Recommended Fixes

### Option 1: Restore Promise-Based Coordination (RECOMMENDED)

**Re-implement BUG-002 fix** with one change:

```typescript
private async ensureSessionExists(msg: BridgeMessage): Promise<void> {
  const existing = this.sessions.getSession(msg.sessionId);
  if (existing) {
    if (existing.status !== 'active') {
      this.sessions.reactivateSession(msg.sessionId);
    }
    return;
  }

  // ✅ FIX: Create Promise BEFORE checking if another thread is creating
  const existingPromise = this.topicCreationPromises.get(msg.sessionId);
  if (existingPromise) {
    // Another thread is creating topic - wait for it
    await existingPromise;
    return;
  }

  // ✅ FIX: Create Promise before starting async work
  let resolver!: (threadId: number | undefined) => void;
  const promise = new Promise<number | undefined>((resolve) => {
    resolver = resolve;
  });
  this.topicCreationPromises.set(msg.sessionId, promise);
  this.topicCreationResolvers.set(msg.sessionId, resolver);

  logger.info('Creating session on-the-fly', { sessionId: msg.sessionId });

  // ✅ Now call handleSessionStart - it will resolve the Promise
  await this.handleSessionStart(msg);
}
```

**Benefits:**
- ✅ Prevents duplicate topics
- ✅ Preserves message ordering
- ✅ No dropped messages
- ✅ Compatible with existing code

**Complexity:** Low (restore existing pattern)

---

### Option 2: Mutex-Based Locking

```typescript
private sessionCreationLocks: Map<string, Promise<void>> = new Map();

private async ensureSessionExists(msg: BridgeMessage): Promise<void> {
  const existing = this.sessions.getSession(msg.sessionId);
  if (existing) {
    if (existing.status !== 'active') {
      this.sessions.reactivateSession(msg.sessionId);
    }
    return;
  }

  // Check if another thread is creating this session
  const existingLock = this.sessionCreationLocks.get(msg.sessionId);
  if (existingLock) {
    // Wait for other thread to finish
    await existingLock;
    return;
  }

  // Create lock
  let resolveLock!: () => void;
  const lock = new Promise<void>((resolve) => {
    resolveLock = resolve;
  });
  this.sessionCreationLocks.set(msg.sessionId, lock);

  try {
    logger.info('Creating session on-the-fly', { sessionId: msg.sessionId });
    await this.handleSessionStart(msg);
  } finally {
    // Release lock
    this.sessionCreationLocks.delete(msg.sessionId);
    resolveLock();
  }
}
```

**Benefits:**
- ✅ Prevents duplicate topics
- ✅ Simple mutual exclusion
- ✅ Auto-cleanup on exception

**Drawbacks:**
- ❌ `waitForTopic()` still broken (needs separate fix)
- ❌ Doesn't solve message ordering

---

### Option 3: Event Sequencing Layer

Add sequence numbers to hook events:

```typescript
interface BridgeMessage {
  type: MessageType;
  sessionId: string;
  timestamp: string;
  content: string;
  metadata?: Record<string, unknown>;
  sequence?: number;  // ← NEW: Event sequence number
}

private messageQueues: Map<string, BridgeMessage[]> = new Map();
private nextExpectedSequence: Map<string, number> = new Map();

private async processMessage(msg: BridgeMessage): Promise<void> {
  if (msg.sequence === undefined) {
    // No sequencing - process immediately (backward compat)
    await this.routeMessage(msg);
    return;
  }

  const queue = this.messageQueues.get(msg.sessionId) || [];
  queue.push(msg);
  queue.sort((a, b) => (a.sequence || 0) - (b.sequence || 0));
  this.messageQueues.set(msg.sessionId, queue);

  // Process in-order messages
  const expected = this.nextExpectedSequence.get(msg.sessionId) || 0;
  while (queue.length > 0 && queue[0].sequence === expected) {
    const next = queue.shift()!;
    await this.routeMessage(next);
    this.nextExpectedSequence.set(msg.sessionId, expected + 1);
  }
}
```

**Benefits:**
- ✅ Solves message ordering
- ✅ Handles network reordering
- ✅ Graceful degradation (no sequence = immediate)

**Drawbacks:**
- ❌ Requires hook changes (sequence tracking)
- ❌ Doesn't solve duplicate topics
- ❌ Complex state management

---

## Testing Strategy

### Race Condition Tests

```typescript
describe('Race Condition Safety', () => {
  test('RC#1: Concurrent ensureSessionExists() creates single topic', async () => {
    const daemon = new BridgeDaemon();
    await daemon.start();

    const sessionId = 'test-session-123';
    const messages = [
      { type: 'tool_result', sessionId, timestamp: new Date().toISOString(), content: 'result' },
      { type: 'agent_response', sessionId, timestamp: new Date().toISOString(), content: 'response' }
    ];

    // Fire both messages concurrently
    await Promise.all(messages.map(msg => daemon['socket'].emit('message', msg)));

    // Wait for processing
    await new Promise(resolve => setTimeout(resolve, 1000));

    // Verify: Only ONE topic created
    const session = daemon['sessions'].getSession(sessionId);
    expect(session).toBeDefined();

    const threadId = daemon['sessions'].getSessionThread(sessionId);
    expect(threadId).toBeDefined();

    // Check in-memory cache matches database
    const cachedThreadId = daemon['sessionThreads'].get(sessionId);
    expect(cachedThreadId).toBe(threadId);

    // Verify: Only one topic exists in Telegram
    const topics = await daemon['bot'].getForumTopics();
    const matchingTopics = topics.filter(t => t.name.includes(sessionId.slice(0, 8)));
    expect(matchingTopics).toHaveLength(1);
  });

  test('RC#3: Messages during topic creation are not dropped', async () => {
    const daemon = new BridgeDaemon();
    await daemon.start();

    const sessionId = 'test-session-456';
    const spy = jest.spyOn(daemon['bot'], 'sendMessage');

    // Fire 10 messages rapidly for new session
    const messages = Array.from({ length: 10 }, (_, i) => ({
      type: 'agent_response',
      sessionId,
      timestamp: new Date().toISOString(),
      content: `Message ${i}`
    }));

    await Promise.all(messages.map(msg => daemon['socket'].emit('message', msg)));

    // Wait for all processing
    await new Promise(resolve => setTimeout(resolve, 2000));

    // Verify: ALL messages were sent (none dropped)
    expect(spy).toHaveBeenCalledTimes(11); // 10 messages + 1 session start

    // Verify: All messages in correct order
    const calls = spy.mock.calls.map(call => call[0]);
    expect(calls[0]).toContain('Session registered'); // Session start
    messages.forEach((msg, i) => {
      expect(calls[i + 1]).toContain(`Message ${i}`);
    });
  });
});
```

---

## Conclusion

The BUG-010 fix (removing Promise-based coordination) introduces **critical race conditions** that will cause:

1. **Duplicate Telegram topics** under concurrent load
2. **Dropped messages** during topic creation
3. **Inconsistent database state** between cache and DB

**Recommended Action**: **Revert to Option 1** (Promise-based coordination) with proper Promise creation BEFORE async work.

**Timeline**:
- **Immediate**: Revert to BUG-002 pattern
- **Short-term**: Add RC#1 test to prevent regression
- **Long-term**: Consider event sequencing layer for ordering guarantees

---

## Appendix: Promise Pattern Correctness Proof

### Why Promises Work

**Invariant**: At most one thread creates a topic for a session.

**Proof by cases**:

**Case 1**: Session doesn't exist, no Promise exists
- Thread A: Creates Promise, sets in Map, calls `handleSessionStart()`
- Thread B (concurrent): Sees Promise in Map, waits on it
- Thread C (concurrent): Sees Promise in Map, waits on it
- Result: Thread A creates topic, B and C wait

**Case 2**: Session doesn't exist, Promise exists
- Thread A: Sees Promise, waits
- Result: Joins existing creation

**Case 3**: Session exists
- All threads: Fast path return (line 680)
- Result: No topic creation

**QED**: Only one topic created per session.

---

**Document Version**: 1.0
**Date**: 2025-12-09
**Analyzer**: Code Quality Analyzer (Claude Sonnet 4.5)
