# ADR-004: Tmux-Only Input Injection

**Status:** Accepted
**Date:** 2026-03-16

## Context

The bridge needs to inject user input from Telegram into the running Claude
Code CLI session. Three injection methods were considered during development:

1. **PTY injection** (`process.stdin.push()`): Push data directly into the
   Node.js process stdin stream. This works in simple cases but fails when
   the terminal state changes (e.g., Claude Code uses raw mode, alternate
   screen, or readline). The stdin stream may not be connected to the
   controlling terminal at all when running as a background process.

2. **FIFO injection** (named pipe): Create a FIFO file and redirect input
   through it. This required constructing shell commands with string
   interpolation to set up the pipe, introducing a shell injection
   vulnerability. The FIFO approach was never fully implemented and had
   reliability issues with pipe blocking and EOF handling.

3. **tmux send-keys**: Send literal keystrokes to a tmux pane where Claude
   Code is running. tmux is already a dependency for the typical deployment
   (running Claude Code in a persistent session).

## Decision

tmux `send-keys` with the `-l` (literal) flag is the sole injection method.
PTY and FIFO code has been removed entirely from the codebase.

### Implementation details

**File:** `src/bridge/injector.ts`

The `InjectionMethod` type is `'tmux' | 'none'`. There is no `'fifo'` or
`'pty'` variant.

All tmux commands use `spawnSync()` with argument arrays:

```typescript
spawnSync('tmux', [...this.socketArgs, 'send-keys', '-t', this.tmuxSession, '-l', text], {
  stdio: 'pipe',
  encoding: 'utf8'
});
```

Key properties:

- **`spawnSync` with argument array:** The arguments are passed directly to
  the `execve` syscall. No shell is involved. There is no possibility of
  shell injection regardless of the content of `text`.

- **`-l` (literal) flag:** tmux treats the entire string as literal key
  input. Special characters like `$`, `;`, `|`, backticks, and newlines are
  not interpreted as tmux key names or shell metacharacters.

- **Enter sent separately:** After injecting the text with `-l`, a separate
  `send-keys Enter` call (without `-l`) submits the input. This separation
  ensures the literal text does not accidentally include an Enter
  interpretation.

- **Socket targeting:** When a tmux socket path is configured (extracted from
  the `$TMUX` environment variable), the `-S` flag targets the correct tmux
  server instance.

- **Target validation:** Before injecting, `validateTarget()` calls
  `tmux list-panes -t <session>` to verify the target pane exists. This
  prevents silent failures when a pane has been closed or renumbered.

### Slash command validation

Slash commands (e.g., `/clear`, `/rename new-name`) are validated against the
character whitelist `[a-zA-Z0-9_- /]` before injection via
`isValidSlashCommand()`. This is a defense-in-depth measure. Even though
`spawnSync` with argument arrays prevents shell injection, the whitelist
ensures that only intended command strings reach Claude Code.

## Consequences

### Positive

- Zero shell injection risk. `spawnSync` with argument arrays is the safest
  way to invoke external processes in Node.js.
- The `-l` flag handles all Unicode and special characters without any
  escaping logic in the application.
- tmux is universally available on Linux and macOS, which are the supported
  platforms for Claude Code.
- Reduced attack surface: two entire injection codepaths (FIFO, PTY) and
  their associated complexity have been removed.

### Negative

- tmux is a hard requirement. Claude Code sessions that are not running in
  tmux cannot receive input injection from Telegram.
- If the user's tmux pane changes (e.g., splits, renumbers), the target must
  be re-detected. The hook system handles this by including current tmux info
  in every event.

### Neutral

- The deprecated `escapeTmuxText()` method is retained with a `@deprecated`
  JSDoc tag for any external callers but is no longer used internally.
