# Input Injector Deep Analysis Report

**File**: `/opt/claude-mobile/packages/claude-telegram-mirror/src/bridge/injector.ts`
**Analysis Date**: 2025-12-08
**Purpose**: Comprehensive security and reliability analysis of tmux input injection mechanism

---

## Executive Summary

The Input Injector module handles critical functionality: injecting user commands from Telegram into the Claude Code CLI running in tmux. This analysis reveals **4 critical bugs** and **multiple edge case failures** that could result in command injection to wrong sessions, silent failures, and security vulnerabilities.

### Critical Issues Found
1. **BUG-003**: No validation of tmux target existence before injection (PARTIALLY FIXED)
2. **BUG-004**: `sendKey()` method missing socket flag (CRITICAL)
3. **Multiple tmux server handling is incomplete** (NEW)
4. **Race conditions in session detection** (NEW)
5. **Inadequate error recovery** (NEW)

---

## 1. tmux Input Injection Mechanism Analysis

### 1.1 Core Injection Flow (`injectViaTmux` - Lines 135-194)

```typescript
private injectViaTmux(text: string): boolean {
  if (!this.tmuxSession) {
    logger.warn('No tmux session');
    return false;
  }

  // BUG-001 fix: Validate target exists before attempting injection
  const validation = this.validateTarget();
  if (!validation.valid) {
    logger.warn('Target validation failed', {
      session: this.tmuxSession,
      socket: this.tmuxSocket,
      reason: validation.reason
    });
    return false;
  }

  try {
    // Escape special characters for tmux
    const escapedText = this.escapeTmuxText(text);

    // Build tmux command with explicit socket if available
    // -S specifies the socket path, -t specifies the target session:window.pane
    const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';
    const sendKeysCmd = `tmux ${socketFlag} send-keys -t "${this.tmuxSession}" -l "${escapedText}"`;
    const enterCmd = `tmux ${socketFlag} send-keys -t "${this.tmuxSession}" Enter`;

    logger.debug('Running tmux command', {
      cmd: sendKeysCmd,
      session: this.tmuxSession,
      socket: this.tmuxSocket,
      textLength: text.length
    });

    execSync(sendKeysCmd, {
      stdio: 'pipe',
      encoding: 'utf8'
    });

    // Send Enter key separately to submit
    execSync(enterCmd, {
      stdio: 'pipe',
      encoding: 'utf8'
    });

    logger.debug('Injected via tmux', { session: this.tmuxSession, socket: this.tmuxSocket });
    return true;
  } catch (error: unknown) {
    const execError = error as { stderr?: string; message?: string };
    logger.error('Failed to inject via tmux', {
      error,
      stderr: execError.stderr,
      message: execError.message,
      session: this.tmuxSession,
      socket: this.tmuxSocket,
      textLength: text.length
    });
    return false;
  }
}
```

**Strengths:**
- ✅ Uses `-l` flag for literal text interpretation (safe from shell injection)
- ✅ Validates target existence before injection (lines 142-150)
- ✅ Includes socket path in commands (lines 158, 160)
- ✅ Comprehensive error logging (lines 182-193)
- ✅ Separates text injection from Enter key (lines 169-178)

**Weaknesses:**
- ⚠️ No retry mechanism on transient failures
- ⚠️ No detection of Claude Code actually receiving the input
- ⚠️ Sends Enter immediately after text (no delay for processing)
- ⚠️ No feedback mechanism to confirm command execution

---

### 1.2 Target Validation (`validateTarget` - Lines 108-130)

```typescript
validateTarget(): { valid: boolean; reason?: string } {
  if (!this.tmuxSession) {
    return { valid: false, reason: 'No tmux session configured' };
  }

  try {
    // Build tmux command with explicit socket if available
    const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';
    const checkCmd = `tmux ${socketFlag} list-panes -t "${this.tmuxSession}" 2>/dev/null`;

    execSync(checkCmd, {
      stdio: 'pipe',
      encoding: 'utf8'
    });

    return { valid: true };
  } catch {
    return {
      valid: false,
      reason: `Pane "${this.tmuxSession}" not found. Claude may have moved to a different pane.`
    };
  }
}
```

**Analysis:**
- ✅ **PARTIALLY FIXES BUG-003**: Validates pane existence before injection
- ✅ Uses socket flag for multi-server environments (line 115)
- ✅ Provides user-friendly error message (line 127)
- ⚠️ Validation is called but result only logged, not acted upon in critical flows
- ⚠️ No caching of validation results (validates on every injection)
- ⚠️ Doesn't verify the pane is running Claude Code specifically

---

## 2. BUG-004: sendKey() Missing Socket Flag (Lines 316-337)

### Current Implementation:

```typescript
async sendKey(key: 'Enter' | 'Escape' | 'Tab' | 'Ctrl-C'): Promise<boolean> {
  if (this.method !== 'tmux' || !this.tmuxSession) {
    return false;
  }

  try {
    const keyMap: Record<string, string> = {
      'Enter': 'Enter',
      'Escape': 'Escape',
      'Tab': 'Tab',
      'Ctrl-C': 'C-c'
    };

    execSync(`tmux send-keys -t "${this.tmuxSession}" ${keyMap[key]}`, {
      stdio: 'ignore'
    });
    return true;
  } catch (error) {
    logger.error('Failed to send key', { key, error });
    return false;
  }
}
```

### Critical Security Issue:

**Line 329: Missing `-S` socket flag!**

```typescript
// CURRENT (BROKEN):
execSync(`tmux send-keys -t "${this.tmuxSession}" ${keyMap[key]}`, {
  stdio: 'ignore'
});

// SHOULD BE (consistent with injectViaTmux):
const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';
execSync(`tmux ${socketFlag} send-keys -t "${this.tmuxSession}" ${keyMap[key]}`, {
  stdio: 'ignore'
});
```

### Impact Analysis:

**SEVERITY: CRITICAL**

When multiple tmux servers are running:
1. User has tmux server A on socket `/tmp/tmux-1000/default`
2. User has tmux server B on socket `/tmp/tmux-1000/work`
3. Claude Code runs in server B, pane `1:0.0`
4. User sends Ctrl-C from Telegram
5. `sendKey()` targets **default tmux server** (server A)
6. **Ctrl-C is sent to WRONG session** (potentially interrupting unrelated work)
7. Claude Code in server B **continues running** (user thinks they stopped it)

**Real-world scenarios:**
- **Developer workflow**: Work tmux in one socket, personal in another
- **CI/CD servers**: Multiple tmux instances per build job
- **SSH sessions**: Different sockets for different remote hosts
- **Container environments**: Isolated tmux instances per container

### Comparison with Other Methods:

**injectViaTmux (Lines 158, 160):**
```typescript
const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';
const sendKeysCmd = `tmux ${socketFlag} send-keys -t "${this.tmuxSession}" -l "${escapedText}"`;
const enterCmd = `tmux ${socketFlag} send-keys -t "${this.tmuxSession}" Enter`;
```
✅ **CORRECT**: Includes socket flag

**validateTarget (Line 115):**
```typescript
const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';
const checkCmd = `tmux ${socketFlag} list-panes -t "${this.tmuxSession}" 2>/dev/null`;
```
✅ **CORRECT**: Includes socket flag

**sendKey (Line 329):**
```typescript
execSync(`tmux send-keys -t "${this.tmuxSession}" ${keyMap[key]}`, {
  stdio: 'ignore'
});
```
❌ **BROKEN**: Missing socket flag

---

## 3. Socket Path Handling Analysis

### 3.1 Socket Storage and Propagation

**Storage Flow:**
```
telegram-hook.sh (Line 136)
  ↓ Extracts from $TMUX
  ↓ socket_path="${TMUX%%,*}"
  ↓
handler.ts (Lines 170, 183, 288, 313, 336, 357)
  ↓ Includes in metadata
  ↓ tmuxSocket: tmuxInfo.socket
  ↓
daemon.ts (Lines 296-314, 395, 427, 536)
  ↓ Stores in database and memory
  ↓ this.injector.setTmuxSession(tmuxTarget, tmuxSocket)
  ↓
injector.ts (Line 358)
  ↓ Stores in instance variable
  ↓ this.tmuxSocket = socket || null
```

**Usage in Commands:**
- ✅ **validateTarget** (Line 115): `tmux ${socketFlag} list-panes`
- ✅ **injectViaTmux** (Lines 158, 160): `tmux ${socketFlag} send-keys`
- ❌ **sendKey** (Line 329): `tmux send-keys` ← **MISSING SOCKET FLAG**

### 3.2 Socket Path Extraction (telegram-hook.sh)

```bash
# $TMUX format: /path/to/socket,pid,index
# Example: "/tmp/tmux-1000/default,12345,0"
local socket_path="${TMUX%%,*}"  # Extract everything before first comma
```

**Analysis:**
- ✅ Correctly parses `$TMUX` environment variable
- ✅ Extracts socket path before first comma
- ⚠️ No validation that socket file actually exists
- ⚠️ No handling of `$TMUX` being unset (falls back to empty string)

---

## 4. Edge Case Analysis

### 4.1 What Happens When the tmux Pane Doesn't Exist?

**Scenario 1: Pane Deleted During Session**
```
1. User starts Claude Code in tmux pane 1:0.0
2. Telegram bridge connects, stores "1:0.0"
3. User kills pane with Ctrl-D or `tmux kill-pane`
4. User sends message from Telegram
```

**Current Behavior:**
```typescript
// Lines 142-150: validateTarget() is called
const validation = this.validateTarget();
if (!validation.valid) {
  logger.warn('Target validation failed', {
    session: this.tmuxSession,
    socket: this.tmuxSocket,
    reason: validation.reason
  });
  return false;  // ✅ FAILS GRACEFULLY
}
```

**Result:** ✅ **SAFE** - Injection fails with clear error message

**BUT:** User in Telegram receives no feedback! They don't know the message wasn't delivered.

---

**Scenario 2: Pane Renumbered**
```
1. Claude Code runs in pane 1:0.0
2. User creates new pane 1:0.1, kills old 1:0.0
3. Panes renumber: 1:0.1 → 1:0.0 (new pane takes old number)
4. User sends message from Telegram
```

**Current Behavior:**
```typescript
// validateTarget() checks if pane EXISTS
// It does NOT check if pane is running Claude Code
execSync(`tmux ${socketFlag} list-panes -t "${this.tmuxSession}" 2>/dev/null`);
// This succeeds! Pane 1:0.0 exists (different process)
```

**Result:** ⚠️ **DANGEROUS** - Message sent to WRONG pane!

**Example Attack Vector:**
```
1. Claude Code in pane 1:0.0 exits
2. User opens vim in new pane (renumbered to 1:0.0)
3. User sends "rm -rf /important/data" to Claude via Telegram
4. Command is executed in vim pane instead!
```

---

### 4.2 What Happens with Multiple tmux Servers?

**Scenario: Developer with Multiple tmux Instances**
```
Terminal 1: tmux -S /tmp/tmux-1000/work new -s work
  └─ Pane 1:0.0: Claude Code running

Terminal 2: tmux -S /tmp/tmux-1000/personal new -s personal
  └─ Pane 1:0.0: bash shell

Telegram user sends: "delete old files"
```

**Current Behavior:**

**injectViaTmux (Lines 158-160):** ✅ Correct
```typescript
const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';
const sendKeysCmd = `tmux ${socketFlag} send-keys -t "1:0.0" -l "delete old files"`;
// Targets: /tmp/tmux-1000/work session (CORRECT)
```

**sendKey (Line 329):** ❌ Wrong
```typescript
execSync(`tmux send-keys -t "1:0.0" Ctrl-C`);
// Targets: Default tmux server (could be /tmp/tmux-1000/personal)
// WRONG SERVER! Sends Ctrl-C to bash shell instead of Claude Code!
```

**Real-World Impact:**
- User thinks they stopped Claude Code with Ctrl-C
- Claude Code keeps running in work server
- Bash shell in personal server receives Ctrl-C (harmless, but wrong)
- User has **no indication** that stop command failed

---

### 4.3 Socket Path Usage Across Methods

**Comprehensive Socket Flag Analysis:**

| Method | Line(s) | Socket Flag? | Impact if Missing |
|--------|---------|--------------|-------------------|
| `validateTarget()` | 115-116 | ✅ YES | Would validate wrong server's pane |
| `injectViaTmux()` | 158, 160 | ✅ YES | Would inject to wrong server |
| `sendKey()` | 329 | ❌ **NO** | **Sends keys to wrong server** |
| `detectTmuxSession()` | 253 | N/A | Runs inside tmux, no socket needed |
| `findClaudeCodeSession()` | 269, 284 | ❌ **NO** | **Searches only default server** |

**findClaudeCodeSession Analysis (Lines 265-298):**

```typescript
private findClaudeCodeSession(): string | null {
  try {
    // List all tmux sessions and panes
    const output = execSync(
      'tmux list-panes -a -F "#{session_name}:#{pane_current_command}" 2>/dev/null',
      { encoding: 'utf8' }
    );
    // ❌ NO SOCKET FLAG! Only searches default tmux server
```

**Impact:**
- If Claude Code runs on non-default tmux server: **NOT FOUND**
- Injector falls back to `method: 'none'`
- All input injection **SILENTLY FAILS**

---

## 5. Error Handling and Recovery Patterns

### 5.1 Current Error Handling

**Pattern 1: Boolean Return Values**
```typescript
async inject(text: string): Promise<boolean>
async sendKey(key: string): Promise<boolean>
private injectViaTmux(text: string): boolean
```

**Strengths:**
- ✅ Clear success/failure indication
- ✅ Caller can handle errors appropriately

**Weaknesses:**
- ❌ No distinction between error types (network vs. pane not found vs. permission denied)
- ❌ User receives no feedback on failure (daemon.ts doesn't notify user)
- ❌ No retry logic for transient failures

---

**Pattern 2: Try-Catch with Logging**
```typescript
try {
  execSync(sendKeysCmd, { stdio: 'pipe', encoding: 'utf8' });
  execSync(enterCmd, { stdio: 'pipe', encoding: 'utf8' });
  return true;
} catch (error: unknown) {
  const execError = error as { stderr?: string; message?: string };
  logger.error('Failed to inject via tmux', {
    error,
    stderr: execError.stderr,
    message: execError.message,
    session: this.tmuxSession,
    socket: this.tmuxSocket,
    textLength: text.length
  });
  return false;
}
```

**Strengths:**
- ✅ Comprehensive error logging
- ✅ Includes context (session, socket, text length)
- ✅ Safely handles error object typing

**Weaknesses:**
- ❌ Errors only logged, never surfaced to user
- ❌ No differentiation between recoverable and permanent failures
- ❌ No automatic retry on transient errors

---

### 5.2 Recovery Mechanisms

**Current Recovery:**
- NONE (injections fail permanently on error)

**Missing Recovery Patterns:**
1. **Retry with exponential backoff**: For transient tmux server issues
2. **Target auto-healing**: Detect when pane moves, update session mapping
3. **User notification**: Alert user in Telegram when injection fails
4. **Fallback detection**: Re-scan for Claude Code if target becomes invalid

**Example from daemon.ts (Lines 393-419):**
```typescript
private checkAndUpdateTmuxTarget(msg: BridgeMessage): void {
  const newTmuxTarget = msg.metadata?.tmuxTarget as string | undefined;
  const newTmuxSocket = msg.metadata?.tmuxSocket as string | undefined;

  if (!newTmuxTarget) return;

  const currentTarget = this.sessionTmuxTargets.get(msg.sessionId);

  if (currentTarget === newTmuxTarget) return;

  // Target has changed! Update cache and database
  logger.info('Tmux target changed, auto-updating', {
    sessionId: msg.sessionId,
    oldTarget: currentTarget || 'none',
    newTarget: newTmuxTarget,
    socket: newTmuxSocket
  });

  this.sessionTmuxTargets.set(msg.sessionId, newTmuxTarget);
  this.sessions.setTmuxInfo(msg.sessionId, newTmuxTarget, newTmuxSocket || undefined);
}
```

**This is auto-healing, but only triggered by incoming hook messages!**
- ✅ Updates when Claude Code sends new events
- ❌ Doesn't help if Claude Code exits and pane is reused
- ❌ Doesn't detect if user manually reorganizes panes

---

## 6. Security Implications

### 6.1 Command Injection Vulnerabilities

**Text Escaping (Lines 305-311):**
```typescript
private escapeTmuxText(text: string): string {
  // Only escape double quotes and backslashes for the shell
  // Single quotes, $, ` are all fine with -l flag
  return text
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"');
}
```

**Analysis:**
- ✅ **SAFE**: Uses `-l` flag (literal mode) in tmux send-keys
- ✅ Only escapes quotes needed for shell string wrapping
- ✅ tmux `-l` flag treats all input as literal text (no key interpretation)

**Potential Attack:**
```
User sends: `rm -rf /`; echo "pwned"
Escaped to: `rm -rf /`\\; echo \\"pwned\\"
Sent as: tmux send-keys -l "rm -rf /`\; echo \"pwned\""
Result: Literal text sent to Claude Code (SAFE, Claude decides what to execute)
```

**Verdict:** ✅ **NO COMMAND INJECTION VULNERABILITY**

---

### 6.2 Session Hijacking Risks

**Risk Scenario:**
```
1. Attacker creates tmux session "1:0.0" on default server
2. Legitimate Claude Code runs on different server (socket B)
3. sendKey() targets default server (socket A)
4. Attacker receives Ctrl-C, Enter, Tab meant for Claude
```

**Current Mitigation:**
- ✅ Socket path stored and used in `injectViaTmux`
- ❌ Socket path **NOT** used in `sendKey` (BUG-004)

**Severity:**
- **injectViaTmux**: Low risk (uses socket flag)
- **sendKey**: **HIGH RISK** (no socket flag, targets default server)

---

### 6.3 Permission and Access Control

**File Permissions:**
- Socket path: Managed by tmux (typically 0700 on socket directory)
- No additional access controls in injector

**Process Permissions:**
- Injector runs as same user as Claude Code (required for tmux access)
- No elevation of privileges

**Multi-User Risks:**
- ✅ tmux sockets are user-specific (cannot access other users' sessions)
- ✅ Each user has isolated tmux servers
- ⚠️ User can have multiple sessions (potential for wrong-session targeting)

---

## 7. Recommendations

### 7.1 Critical Fixes (Must Have)

**FIX-001: Add Socket Flag to sendKey() (BUG-004)**
```typescript
async sendKey(key: 'Enter' | 'Escape' | 'Tab' | 'Ctrl-C'): Promise<boolean> {
  if (this.method !== 'tmux' || !this.tmuxSession) {
    return false;
  }

  try {
    const keyMap: Record<string, string> = {
      'Enter': 'Enter',
      'Escape': 'Escape',
      'Tab': 'Tab',
      'Ctrl-C': 'C-c'
    };

    // FIX: Add socket flag (consistent with injectViaTmux)
    const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';
    execSync(`tmux ${socketFlag} send-keys -t "${this.tmuxSession}" ${keyMap[key]}`, {
      stdio: 'ignore'
    });
    return true;
  } catch (error) {
    logger.error('Failed to send key', { key, error });
    return false;
  }
}
```

**Priority:** 🔴 CRITICAL
**Effort:** 5 minutes
**Impact:** Prevents sending keys to wrong tmux server

---

**FIX-002: Validate Target in sendKey()**
```typescript
async sendKey(key: 'Enter' | 'Escape' | 'Tab' | 'Ctrl-C'): Promise<boolean> {
  if (this.method !== 'tmux' || !this.tmuxSession) {
    return false;
  }

  // Validate target before sending keys
  const validation = this.validateTarget();
  if (!validation.valid) {
    logger.warn('Cannot send key - target validation failed', {
      key,
      reason: validation.reason
    });
    return false;
  }

  try {
    const keyMap: Record<string, string> = {
      'Enter': 'Enter',
      'Escape': 'Escape',
      'Tab': 'Tab',
      'Ctrl-C': 'C-c'
    };

    const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';
    execSync(`tmux ${socketFlag} send-keys -t "${this.tmuxSession}" ${keyMap[key]}`, {
      stdio: 'ignore'
    });
    return true;
  } catch (error) {
    logger.error('Failed to send key', { key, error });
    return false;
  }
}
```

**Priority:** 🟠 HIGH
**Effort:** 5 minutes
**Impact:** Prevents sending keys to non-existent panes

---

**FIX-003: Add Socket Flag to findClaudeCodeSession()**
```typescript
private findClaudeCodeSession(): string | null {
  try {
    // If we have a socket path, use it explicitly
    const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';

    // List all tmux sessions and panes
    const output = execSync(
      `tmux ${socketFlag} list-panes -a -F "#{session_name}:#{pane_current_command}" 2>/dev/null`,
      { encoding: 'utf8' }
    );

    const lines = output.trim().split('\n');

    for (const line of lines) {
      const [session, command] = line.split(':');
      if (command && (command.includes('claude') || command.includes('node'))) {
        return session;
      }
    }

    // Fallback: look for any session with "claude" in the name
    const sessions = execSync(
      `tmux ${socketFlag} list-sessions -F "#{session_name}" 2>/dev/null`,
      { encoding: 'utf8' }
    ).trim().split('\n');

    for (const session of sessions) {
      if (session.toLowerCase().includes('claude') || session.toLowerCase().includes('code')) {
        return session;
      }
    }

    return null;
  } catch {
    return null;
  }
}
```

**Priority:** 🟠 HIGH
**Effort:** 10 minutes
**Impact:** Enables detection of Claude Code on non-default tmux servers

---

### 7.2 High Priority Enhancements

**ENH-001: Add Process Validation**
```typescript
validateTarget(): { valid: boolean; reason?: string; process?: string } {
  if (!this.tmuxSession) {
    return { valid: false, reason: 'No tmux session configured' };
  }

  try {
    const socketFlag = this.tmuxSocket ? `-S "${this.tmuxSocket}"` : '';

    // Check pane exists
    const checkCmd = `tmux ${socketFlag} list-panes -t "${this.tmuxSession}" 2>/dev/null`;
    execSync(checkCmd, { stdio: 'pipe', encoding: 'utf8' });

    // Check what process is running
    const processCmd = `tmux ${socketFlag} display-message -t "${this.tmuxSession}" -p "#{pane_current_command}"`;
    const process = execSync(processCmd, {
      stdio: 'pipe',
      encoding: 'utf8'
    }).trim();

    // Verify it's Claude Code or node
    if (!process.includes('claude') && !process.includes('node')) {
      return {
        valid: false,
        reason: `Pane "${this.tmuxSession}" is running "${process}", not Claude Code`,
        process
      };
    }

    return { valid: true, process };
  } catch {
    return {
      valid: false,
      reason: `Pane "${this.tmuxSession}" not found`
    };
  }
}
```

**Priority:** 🟡 MEDIUM
**Effort:** 15 minutes
**Impact:** Prevents injection to wrong process after pane renumbering

---

**ENH-002: Add User Feedback on Injection Failure**

In `daemon.ts`, modify `handleUserMessage()`:
```typescript
// After injection attempt
const injected = await this.injector.inject(text);
if (!injected) {
  // Send error message to user
  const sessionThreadId = this.getSessionThreadId(session.id);
  const validation = this.injector.validateTarget();

  await this.bot.sendMessage(
    `⚠️ *Failed to send message to Claude*\n\n` +
    `Reason: ${validation.reason || 'Unknown error'}\n\n` +
    `_Please check that Claude Code is still running in the expected tmux pane._`,
    { parseMode: 'Markdown' },
    sessionThreadId
  );
}
```

**Priority:** 🟡 MEDIUM
**Effort:** 10 minutes
**Impact:** User knows when messages aren't delivered

---

### 7.3 Future Improvements

**IMP-001: Retry Logic**
```typescript
private async injectViaTmuxWithRetry(
  text: string,
  maxRetries: number = 3,
  delayMs: number = 100
): Promise<boolean> {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    const success = this.injectViaTmux(text);
    if (success) return true;

    if (attempt < maxRetries) {
      logger.debug(`Injection attempt ${attempt} failed, retrying...`);
      await new Promise(resolve => setTimeout(resolve, delayMs * attempt));
    }
  }

  logger.error(`Injection failed after ${maxRetries} attempts`);
  return false;
}
```

---

**IMP-002: Target Auto-Recovery**
```typescript
async redetectTarget(): Promise<boolean> {
  logger.info('Attempting to redetect tmux target...');

  const newSession = this.detectTmuxSession();
  if (newSession) {
    const oldSession = this.tmuxSession;
    this.tmuxSession = newSession;

    logger.info('Tmux target updated', {
      old: oldSession,
      new: newSession
    });

    // Notify daemon to update database
    this.emit('target-changed', { oldSession, newSession });
    return true;
  }

  return false;
}
```

---

## 8. Test Coverage Gaps

**Critical Gaps Identified:**
1. ❌ No tests for `sendKey()` with socket flag
2. ❌ No tests for multiple tmux server scenarios
3. ❌ No tests for pane renumbering edge case
4. ❌ No tests for validation failure handling
5. ❌ No tests for error recovery

**Recommended Test Cases:**

```typescript
describe('InputInjector - Multi-Server Tests', () => {
  it('should use socket flag in sendKey()', async () => {
    const injector = new InputInjector();
    injector.setTmuxSession('1:0.0', '/tmp/tmux-1000/work');

    const spy = jest.spyOn(execSync);
    await injector.sendKey('Ctrl-C');

    expect(spy).toHaveBeenCalledWith(
      expect.stringContaining('-S "/tmp/tmux-1000/work"'),
      expect.any(Object)
    );
  });

  it('should validate target before sendKey()', async () => {
    const injector = new InputInjector();
    injector.setTmuxSession('nonexistent', '/tmp/socket');

    const result = await injector.sendKey('Enter');
    expect(result).toBe(false);
  });

  it('should detect Claude Code on non-default tmux server', async () => {
    // Mock tmux commands for non-default server
    // Verify findClaudeCodeSession uses socket flag
  });
});
```

---

## 9. Conclusion

The Input Injector has a **solid foundation** but suffers from **critical consistency issues** in socket path handling. The primary bug (BUG-004) is a **simple fix** but has **severe security implications** in multi-server environments.

### Priority Matrix:

| Fix | Priority | Effort | Risk if Not Fixed |
|-----|----------|--------|-------------------|
| BUG-004: Add socket flag to sendKey() | 🔴 CRITICAL | 5 min | Keys sent to wrong server |
| FIX-002: Validate target in sendKey() | 🟠 HIGH | 5 min | Keys sent to non-existent pane |
| FIX-003: Socket flag in findClaudeCodeSession() | 🟠 HIGH | 10 min | Cannot find Claude on non-default server |
| ENH-001: Process validation | 🟡 MEDIUM | 15 min | Injection to wrong process after renumber |
| ENH-002: User feedback | 🟡 MEDIUM | 10 min | Silent failures confuse users |

**Total Critical Path Time:** ~45 minutes to fix all high/critical issues

---

## 10. Code Quality Metrics

### Positive Aspects:
- ✅ Well-documented with inline comments
- ✅ Comprehensive error logging
- ✅ Safe escaping (no command injection)
- ✅ Validation before injection (added in recent commits)
- ✅ EventEmitter pattern for extensibility

### Areas for Improvement:
- ⚠️ Inconsistent socket flag usage across methods
- ⚠️ No retry logic
- ⚠️ No user feedback on failures
- ⚠️ Limited test coverage
- ⚠️ Validation not enforced in all code paths

**Overall Quality Score: 6.5/10**

---

## Appendix A: tmux Command Reference

**Socket Flag Usage:**
```bash
# Without socket (targets default server)
tmux send-keys -t "1:0.0" "hello"

# With explicit socket
tmux -S /tmp/tmux-1000/work send-keys -t "1:0.0" "hello"
```

**Target Formats:**
- `session_name`: Targets first window, first pane
- `session_name:window_index`: Targets first pane in window
- `session_name:window_index.pane_index`: Explicit pane
- `%pane_id`: Absolute pane ID (e.g., `%1`)

**Environment Variable:**
```bash
$TMUX format: "/path/to/socket,pid,session_index"
Example: "/tmp/tmux-1000/default,12345,0"
```

---

## Appendix B: Related Files

**Key Dependencies:**
- `/opt/claude-mobile/packages/claude-telegram-mirror/src/bridge/daemon.ts` (Lines 290-329, 393-419)
- `/opt/claude-mobile/packages/claude-telegram-mirror/src/bridge/session.ts` (Lines 124-188)
- `/opt/claude-mobile/packages/claude-telegram-mirror/src/hooks/handler.ts` (Lines 165-186)
- `/opt/claude-mobile/packages/claude-telegram-mirror/scripts/telegram-hook.sh` (Line 136)

**Documentation:**
- `/opt/claude-mobile/packages/claude-telegram-mirror/docs/SESSION_MAPPING_ARCHITECTURE.md`
- `/opt/claude-mobile/packages/claude-telegram-mirror/docs/ARCHITECTURE.md`
- `/opt/claude-mobile/packages/claude-telegram-mirror/docs/macos-compatibility-report.md`

---

**End of Analysis Report**
