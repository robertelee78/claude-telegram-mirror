# Security Policy

## Threat Model

The claude-telegram-mirror bridges a local Claude Code CLI session to a remote
Telegram chat. The system has five trust boundaries, shown below.

```mermaid
graph TB
    subgraph "Untrusted Network"
        TG["Telegram Bot API<br/>(HTTPS, external)"]
    end

    subgraph "Local Machine — Same User"
        BOT["Bot Process<br/>(reqwest long-poll)"]
        DAEMON["Bridge Daemon"]
        SOCK["Unix Domain Socket<br/>(bridge.sock, 0o600)"]
        HOOKS["Hook Handler<br/>(ctm hook subcommand)"]
        TMUX["tmux Sessions<br/>(send-keys injection)"]
        FS["File System<br/>(~/.config/claude-telegram-mirror/)"]
        DB["SQLite DB<br/>(sessions.db, 0o600)"]
    end

    TG -- "HTTPS poll" --> BOT
    BOT -- "in-process" --> DAEMON
    DAEMON -- "listen/accept" --> SOCK
    HOOKS -- "connect/send NDJSON" --> SOCK
    DAEMON -- "Command::new" --> TMUX
    DAEMON -- "read/write" --> FS
    DAEMON -- "rusqlite" --> DB
    HOOKS -- "stdin pipe" --> HOOKS
```

### Trust boundaries

| Boundary | Trust level | Threat |
|----------|-------------|--------|
| Telegram Bot API to Bot process | Untrusted network | Spoofed updates, message injection from unauthorized chats |
| Unix domain socket (bridge.sock) | Same-user local IPC | Other local users or processes connecting |
| tmux send-keys | Same-user process control | Command injection via unsanitized text |
| File system (config dir) | Same-user file access | World-readable secrets, path traversal |
| Hook scripts (stdin) | Subprocess execution | Oversized payloads, malformed JSON |

## Security Mitigations

### 1. No Shell Interpolation in tmux Injection

**File:** `rust-crates/ctm/src/injector.rs`

All tmux commands use `Command::new("tmux")` with `.arg()` chains. The process
binary is the first argument and all subsequent arguments are passed directly to
the kernel without shell interpretation. The `-l` (literal) flag on `send-keys`
ensures tmux treats the injected string as literal keystrokes. No escaping or
quoting is needed.

Dead code for FIFO and PTY injection methods has been removed. The only
injection method is `tmux`. See [ADR-004](docs/adr/ADR-004-tmux-only-injection.md).

Slash commands (e.g., `/clear`, `/rename`) are validated against a character
whitelist (`[a-zA-Z0-9_- /]`) before injection. Commands containing shell
metacharacters are rejected.

### 2. Bot Token Scrubbing

**File:** `rust-crates/ctm/src/bot/client.rs`

A `tracing` subscriber layer applies `scrub_bot_token()` to log output.
The regex `bot\d+:[A-Za-z0-9_-]+/` matches the Telegram bot token pattern
in API URLs and replaces it with `bot<REDACTED>`.

All log output goes to stderr via the `tracing` subscriber. There is no
file transport, so tokens cannot leak into log files on disk.

The error handler in `rust-crates/ctm/src/bot/client.rs` also scrubs bot
tokens from error messages before logging.

### 3. Chat Authorization (Anti-IDOR)

**File:** `rust-crates/ctm/src/daemon/telegram_handlers.rs`

A chat ID check verifies `chat.id` against the configured `chat_id` on
every incoming update. Updates from unauthorized chats receive a static
"Unauthorized" reply and are not processed further.

Approval callback handlers (`approve:`, `reject:`, `abort:`), answer
handlers (`answer:`, `toggle:`, `submit:`), all verify the chat ID
matches the configured `chat_id` before processing. This prevents
IDOR attacks where a user who knows an approval ID could respond from a
different chat.

### 4. Session ID Validation

**File:** `rust-crates/ctm/src/daemon/socket_handlers.rs`

Session IDs from hook events are validated before any database operation:
- Maximum length: 128 characters
- Character set: `[a-zA-Z0-9_-]` only
- Empty/null values are rejected

Messages with invalid session IDs are dropped with a warning log.

### 5. Socket Security

**File:** `rust-crates/ctm/src/socket.rs`

The socket server enforces three limits:
- **NDJSON line limit:** 1 MiB (1,048,576 bytes) per line. Oversized lines
  are dropped and logged.
- **Connection limit:** 64 concurrent connections. New connections beyond
  this limit are destroyed immediately.
- **Directory permissions:** The socket directory is created with mode 0o700
  (owner-only) and the socket file is set to 0o600 after binding.

A PID file lock prevents multiple daemon instances from racing on the same
socket.

### 6. Socket Path Validation

**File:** `rust-crates/ctm/src/config.rs`

`validateSocketPath()` rejects socket paths that:
- Contain `..` (directory traversal)
- Are not absolute (do not start with `/`)
- Exceed 256 characters

Invalid paths fall back to the default socket path in the config directory.

### 7. Config Directory Permissions

**File:** `rust-crates/ctm/src/config.rs`

`ensure_config_dir()` creates `~/.config/claude-telegram-mirror/` with mode
0o700. If the directory already exists, it enforces 0o700 via
`fs::set_permissions()`. All config directory creation goes through this
single function.

### 8. Database File Permissions

**File:** `rust-crates/ctm/src/session.rs`

Immediately after opening the SQLite database, `fs::set_permissions(db_path, 0o600)`
is called to ensure the database file is owner-readable/writable only.

### 9. Hook Stdin Size Limit

**File:** `rust-crates/ctm/src/hook.rs`

The hook handler reads stdin in chunks and enforces a 1 MiB
(1,048,576 byte) limit. If the accumulated input exceeds this limit, the
handler logs a warning and exits cleanly without processing the payload.

### 10. Download File Handling

**File:** `rust-crates/ctm/src/daemon/telegram_handlers.rs`

Downloaded files from Telegram are handled with several protections:
- The downloads directory is created with mode 0o700
- Downloaded files are written with mode 0o600
- Filenames are sanitized: path separators replaced, `..` removed, dotfile
  prefixes neutralized, length capped at 200 characters
- Every filename is prefixed with a UUID for uniqueness
- Files older than 24 hours are automatically cleaned up
- Telegram enforces a 20 MB file size limit at the API level

## File Permission Summary

| File/Directory | Mode | Rationale |
|---|---|---|
| `~/.config/claude-telegram-mirror/` | 0o700 | Contains bot token in config, session database, socket |
| `~/.telegram-env` | 0o600 | Contains bot token and chat ID for shell sourcing |
| `config.json` | 0o600 | Contains bot token |
| `sessions.db` | 0o600 | Contains session metadata, approval records |
| `bridge.sock` | 0o600 | IPC socket, same-user access only |
| `bridge.pid` | default | PID lock file, no sensitive content |
| `downloads/` | 0o700 | User-uploaded files from Telegram |
| Downloaded files | 0o600 | User-uploaded content, owner-only |

## Input Validation Summary

| Boundary | Validation | Enforcement |
|---|---|---|
| Socket messages: session ID | 128 char max, `[a-zA-Z0-9_-]` | `socket_handlers.rs` — `is_valid_session_id()` |
| Socket lines | 1 MiB max per NDJSON line | `socket.rs` — `MAX_LINE_BYTES` |
| Socket connections | 64 concurrent max | `socket.rs` — `MAX_CONNECTIONS` |
| Hook stdin | 1 MiB max | `hook.rs` — `MAX_STDIN_BYTES` |
| Socket paths | No `..`, absolute only, 256 char max | `config.rs` — `validate_socket_path()` |
| Slash commands | Character whitelist: `[a-zA-Z0-9_- /]` | `injector.rs` — `is_valid_slash_command()` |
| Download filenames | Sanitized, UUID-prefixed, no `..`, 200 char max | `telegram_handlers.rs` — `sanitize_filename()` |
| Download file size | 20 MB max | Telegram Bot API server-side limit |
| Telegram chat ID | Exact match against configured `chat_id` | `telegram_handlers.rs` — chat ID check |

## Security Checklist for Contributors

Before modifying security-sensitive code, verify:

- [ ] **No shell interpolation.** All subprocess calls use `Command::new()`
  with `.arg()` chains, never string concatenation into a shell command.
- [ ] **No hardcoded secrets.** Bot tokens come from environment variables or
  the config file, never from source code.
- [ ] **File permissions enforced.** Any new file or directory in the config
  directory uses 0o600 (files) or 0o700 (directories).
- [ ] **Input validated at boundary.** Any data arriving from the socket, stdin,
  or Telegram is validated before use. Session IDs, file paths, and command
  strings are checked against their respective whitelists.
- [ ] **Bot token not logged.** Any error message that might contain a URL is
  passed through `scrub_bot_token()` before logging.
- [ ] **Chat ID checked.** Any new callback handler or message handler verifies
  the chat ID matches the configured `chat_id`.
- [ ] **Tests pass.** Run `cargo test` and confirm no regressions.
- [ ] **No new file logging.** All log output goes to stderr. Do not add file
  transports to the logger.
- [ ] **Path traversal blocked.** Any user-controlled path component is
  validated to reject `..` and non-absolute paths.

## Responsible Disclosure

If you discover a security vulnerability in claude-telegram-mirror, please
report it responsibly:

1. **Do not** open a public GitHub issue for security vulnerabilities.
2. Email the maintainers at the address listed in `package.json`, or use
   GitHub's private vulnerability reporting feature on the repository.
3. Include a description of the vulnerability, steps to reproduce, and the
   potential impact.
4. Allow up to 90 days for a fix before public disclosure.

We will acknowledge receipt within 48 hours and aim to release a fix within
30 days of confirmation.
