# Claude Code Telegram Mirror

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
![Rust](https://img.shields.io/badge/Built_with-Rust-dea584.svg)

Run your coding agents from your phone. ctm mirrors **Claude Code**, **OpenCode** and
**Codex** sessions into Telegram — each session gets its own topic, you see what the agent
is doing, and you can reply, answer its questions and approve its tool calls from either
side. Install it and all three are mirrored; there is nothing to enable per host.

**Supported platforms:** Linux x64, Linux arm64, macOS ARM64, macOS Intel x64

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/install.sh | sh
ctm setup    # Interactive setup wizard
```

One static binary, installed to `~/.local/bin/ctm` from the [GitHub Release](https://github.com/robertelee78/claude-telegram-mirror/releases/latest) for your platform (macOS arm64/x64, Linux x64/arm64), size- and SHA-256-verified against the release record before it is installed. The installer also puts `~/.local/bin` first on your `PATH` and installs tab completion for bash, zsh and fish — one marker-delimited block at the end of your shell rc, removable with `ctm shell-setup --remove` (set `CTM_NO_SHELL_SETUP=1` to skip). Open a new shell afterwards.

```bash
ctm update            # upgrade to the latest release (restarts the service if installed)
ctm update --check    # just report
ctm update --rollback # put the previous binary back
```

Prefer to verify by hand? Every release ships `ctm-<target>`, `ctm-<target>.sha256`, and a `stable-<target>.json` record; `sha256sum -c ctm-<target>.sha256`.


## Features

- **Three hosts, one bridge, nothing to enable** — Claude Code (hooks + tmux), **OpenCode**
  (a plugin ctm installs) and **Codex** (its app-server, plus hooks ctm installs). See
  [Other agent hosts](#other-agent-hosts-opencode-codex).
- **A topic per session** — including sub-agents, which report into their parent's topic
  instead of opening one of their own.
- **Both directions** — the agent's replies, tool calls and questions go out; your text,
  photos and files go in.
- **Approvals from either surface** — tool-permission prompts appear as inline buttons in
  Telegram *and* stay answerable at the terminal. Whichever answers first wins; the other
  side is retired.
- **Multiple-choice questions on both surfaces** — Claude's `AskUserQuestion` renders
  natively in the CLI *and* as buttons in Telegram. A Telegram answer drives that live
  widget, paced against `capture-pane` so a keystroke only fires once the expected screen
  has rendered (ADR-015).
- **Confirmed delivery** — an injected message is verified to have left the composer, and
  the Enter is retried if the TUI swallowed it.
- **Human-readable tool summaries** — "Running tests" rather than "Bash", for 30+ command
  shapes, with a **Details** button that still works days later.
- **Stop/interrupt** — `stop` sends Escape, `kill` sends Ctrl-C, `/abort` ends the session.
- **Self-updating** — `ctm update` swaps the binary atomically and restarts the service;
  `ctm doctor --fix` reconciles hooks, service and hosts.
- **Multi-machine** — one daemon and one bot per host, all posting into one supergroup.
- **Built to fail visibly** — bot tokens scrubbed from logs, `flock(2)` against duplicate
  daemons, canonicalized transcript paths, Unicode-safe chunking, and a doctor that reports
  what it cannot verify rather than assuming.

## Quick Start

```bash
# 1. Install
curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/install.sh | sh

# 2. Create the bot and configure everything (interactive)
ctm setup

# 3. Start the daemon (or `ctm service install` to run it at login)
ctm start
```

Then use your agents exactly as you already do:

```bash
tmux new -s claude && claude   # Claude Code needs tmux, so replies can reach its pane
codex                          # mirrored as-is
opencode                       # mirrored as-is
```

`ctm doctor` reports what is wired, per host.

## Trust Model (read before adding anyone)

**Anyone you add to this Telegram channel can drive your shell and approve tool
calls.** Treat the channel like a shared shell: only add people you would already
trust with git-commit access. Semi-trusted or public channels are **not
supported** — authorization is intentionally chat-level, not per-user. The setup
wizard requires you to acknowledge this before writing any configuration.

If a trusted-user list is ever added, it will be a **whitelist** (and its inverse
a **blacklist**).

## CLI Commands

```bash
# Setup & diagnostics
ctm setup              # Interactive setup wizard
ctm doctor             # Diagnose configuration issues
ctm doctor --fix       # Auto-fix detected issues

# Daemon control
ctm start              # Start daemon (foreground mode)
ctm stop               # Stop running daemon
ctm stop --force       # Force stop if graceful shutdown fails
ctm restart            # Restart daemon
ctm status             # Show daemon status, config, and hooks
ctm config --test      # Test Telegram connection
ctm toggle             # Toggle mirroring on/off
ctm toggle --on        # Force mirroring ON
ctm toggle --off       # Force mirroring OFF

# Hook management
ctm install-hooks      # Install global hooks
ctm install-hooks -p   # Install to current project's .claude/
ctm uninstall-hooks    # Remove hooks
ctm hooks              # Show hook status

# OS service management (optional, for auto-start on boot)
ctm service install    # Install as systemd/launchd service
ctm service uninstall  # Remove system service
ctm service start      # Start via service manager (installs the unit if missing)
ctm service stop       # Stop via service manager
ctm service restart    # Restart via service manager
ctm service status     # Show service status

# Updates and shell integration
ctm update             # Update to the latest release, then restart the service
ctm update --check     # Report the newest release without changing anything
ctm update --rollback  # Put the previous binary back
ctm shell-setup        # (Re)install PATH + completions; --remove undoes it
ctm completions zsh    # Print a completion script (bash, zsh, fish)

# Housekeeping
ctm prune-topics --ledger --dry-run   # Show topics whose session is over
ctm prune-topics --ledger             # ...and delete them
```

**Note:** `ctm stop` and `ctm restart` auto-detect whether the daemon is running directly or via a system service and use the appropriate method.

## Telegram Commands

| Command | Action |
|---------|--------|
| Any text | Sent to the agent as input |
| `stop` | Interrupt the current turn (Escape on Claude Code, the host's own interrupt elsewhere) |
| `kill` | Abort harder (Ctrl-C on Claude Code, the host's abort elsewhere) |
| `cc <cmd>` | Send `/<cmd>` to the agent as a slash command |
| `/status` | Show active sessions and mirroring state |
| `/sessions` | List active sessions with age and project dir |
| `/rename <name>` | Rename the session and its topic |
| `/attach <id>` | Attach to a session for updates |
| `/detach` | Detach from current session |
| `/mute` / `/unmute` | Suppress/resume agent response notifications |
| `/toggle` | Toggle mirroring on/off |
| `/abort` | Abort the attached session |
| `/ping` | Measure round-trip latency |
| `/help` | Show all commands |

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for additional details and command aliases.

### Tool Approval Buttons

When an agent asks permission to run something, the request appears in Telegram with
buttons — and stays answerable at the terminal. Whichever surface answers first wins, and
the other is retired.

| Button | Action |
|--------|--------|
| **Approve** | Allow this tool call |
| **Reject** | Deny this tool call |
| **Abort** | Stop the session |
| **Details** | Show the full tool input |

**Details** is recorded in the database and answerable for seven days, so it still works
when you open Telegram later, or after the daemon has restarted.

Per host:

- **Claude Code** — the `PreToolUse` hook waits up to five minutes for a Telegram answer,
  then falls back to the CLI prompt. Nothing appears under
  `--dangerously-skip-permissions`, because nothing asks.
- **Codex** — approvals arrive over its app-server as requests carrying ids, so a Telegram
  tap resolves exactly that request. ctm's shell integration is what puts a session there
  (see [Other agent hosts](#other-agent-hosts-opencode-codex)).
- **OpenCode** — answered through its API; a reply from either side dismisses the other.

## Architecture

One binary in three modes: the **daemon**, the **hook** the agents invoke (`ctm hook`,
`ctm codex-hook`), and the **CLI** you type. Everything downstream of a `BridgeMessage` is
host-neutral — the hosts differ only in how events get out and how your replies get in.

```
                          ┌──────────────┐   Bot API    ┌──────────────┐
                          │  ctm daemon  │ ◀──────────▶ │   Telegram   │
                          │ (tokio loop) │  long poll   │ forum topics │
                          └──────┬───────┘              └──────────────┘
                                 │
                   events up ▲   │   ▼ replies, approvals, answers
                             │   │
                 ┌───────────┴───┴────────────┐
                 │   NDJSON over Unix socket  │
                 └───┬──────────┬─────────┬───┘
                     │          │         │
        ┌────────────┴──┐ ┌─────┴──────┐ ┌┴─────────────┐
        │  Claude Code  │ │  OpenCode  │ │    Codex     │
        ├───────────────┤ ├────────────┤ ├──────────────┤
        │ out: hooks    │ │ out + in:  │ │ out: hooks   │
        │ in:  tmux     │ │ the plugin │ │ in+approval: │
        │   send-keys,  │ │ ctm writes │ │  app-server  │
        │   verified    │ │ (bus up,   │ │  (requests   │
        │   submitted   │ │  API in)   │ │   carry ids) │
        └───────────────┘ └────────────┘ └──────────────┘
```

**What ctm installs, and keeps current:**

| Host | Outbound | Inbound & approvals | ctm writes |
|---|---|---|---|
| Claude Code | `PreToolUse` / `PostToolUse` / `Stop` / … hooks | `tmux send-keys` into the live pane | hooks in `~/.claude/settings.json` |
| OpenCode | the plugin's event hook, piped up a Unix socket | the same plugin, through OpenCode's in-process API | `~/.config/opencode/plugins/ctm.js` |
| Codex | Codex's own hooks | its app-server (`turn/start`, `turn/steer`, approval requests) | `~/.codex/hooks.json`, plus a `codex` shell function |

The daemon keeps all of that in repair: it rewrites the OpenCode plugin and the Codex
hooks when they are missing or stale (so `ctm update` rolls them forward), trusts the
Codex hooks with the hash Codex itself reports, and starts Codex's app-server if it is not
already running.

> **Failing closed beats guessing.** The daemon resolves a Claude session's tmux pane from
> its cache, then SQLite, and gives up rather than picking one (ROUTING-001) — a guessed
> pane means keystrokes in someone else's session. The same instinct governs the rest: a
> Codex approval is answered by its request id, never by typing at a screen ctm cannot
> verify.

## Multi-System Architecture

When you run agents on more than one machine, each machine needs its own bot: Telegram allows only one polling connection per bot token (error 409).

**The model:**
- **One daemon per host** - Each machine runs its own bridge daemon
- **One bot per daemon** - Each daemon uses a unique Telegram bot
- **Multiple sessions per host** - One daemon handles every session on that machine, across all three agents
- **Shared supergroup** - All bots post to the same Telegram supergroup

### Setup for Multiple Systems

1. **Create one bot per system** via [@BotFather](https://t.me/botfather)
2. **Add all bots to the same supergroup** with admin permissions
3. **Configure each system** with its own bot token:
   ```bash
   # On System A (~/.telegram-env)
   export TELEGRAM_BOT_TOKEN="token-for-system-a-bot"
   export TELEGRAM_CHAT_ID="-100shared-group-id"

   # On System B (~/.telegram-env)
   export TELEGRAM_BOT_TOKEN="token-for-system-b-bot"
   export TELEGRAM_CHAT_ID="-100shared-group-id"  # Same group!
   ```
4. **Each daemon creates topics for its own sessions** - Replies route correctly because a daemon only processes the topics it created.

## Prerequisites

- A Telegram account and a supergroup with Topics enabled — `ctm setup` walks you through
  creating the bot and finding the chat id
- At least one agent: Claude Code, OpenCode or Codex
- **tmux**, for Claude Code only: its replies are typed into the pane. OpenCode and Codex
  are driven through their own APIs and need no tmux.

## Telegram Setup

### 1. Create a Bot

1. Message [@BotFather](https://t.me/botfather) -> `/newbot`
2. Choose name and username (must end in `bot`)
3. Save the API token

### 2. Create Supergroup with Topics

1. Create a new group in Telegram
2. Add your bot to the group
3. Group Settings -> Enable **Topics**

### 3. Make Bot an Admin

1. Group Settings -> Administrators -> Add your bot
2. Enable: **Manage Topics**, **Post Messages**

### 4. Get Chat ID

1. Send any message in the group
2. Visit `https://api.telegram.org/botYOUR_TOKEN/getUpdates`
3. Copy the chat ID (supergroups start with `-100`)

### 5. Disable Privacy Mode

1. [@BotFather](https://t.me/botfather) -> `/mybots` -> Select bot
2. Bot Settings -> Group Privacy -> **Turn off**

## Configuration

### Environment Variables

Create `~/.telegram-env`:

```bash
export TELEGRAM_BOT_TOKEN="123456789:ABCdefGHIjklMNOpqrsTUVwxyz"
export TELEGRAM_CHAT_ID="-1001234567890"
export TELEGRAM_MIRROR=true
# Optional:
# export TELEGRAM_MIRROR_VERBOSE=true
# export TELEGRAM_BRIDGE_SOCKET=~/.config/claude-telegram-mirror/bridge.sock
# export TELEGRAM_STALE_SESSION_TIMEOUT_HOURS=72  # Auto-cleanup dead sessions (default: 72)

# Hosts are on by default; these turn pieces off:
# export CTM_CODEX_ENABLED=0       # do not mirror Codex
# export CTM_OPENCODE_ENABLED=0    # do not mirror OpenCode
# export CTM_CODEX_REMOTE=0        # keep `codex` plain (loses Telegram approvals)
```

Source in your shell profile (`~/.bashrc` or `~/.zshrc`):

```bash
[[ -f ~/.telegram-env ]] && source ~/.telegram-env
```

### Config File (Alternative)

The `ctm setup` wizard creates `~/.config/claude-telegram-mirror/config.json`:

```json
{
  "botToken": "your-token",
  "chatId": -1001234567890,
  "enabled": true,
  "verbose": true
}
```

Environment variables take precedence over config file values.

Hosts need no configuration — they are on when the agent is installed. To turn one off, or
to point ctm at an OpenCode server it could not otherwise reach:

```json
{
  "hosts": {
    "codex":    { "enabled": false },
    "opencode": { "enabled": true, "baseUrl": "http://127.0.0.1:4096", "password": "…" }
  }
}
```

### Test Connection

```bash
ctm doctor
# Checks: config, hooks, socket, tmux, systemd/launchd, Telegram API, hosts
```

## Other agent hosts (OpenCode, Codex)

ctm mirrors **OpenCode** and **Codex** sessions alongside Claude Code (ADR-016), and
it is **on by default — there is nothing to enable.** Install ctm, then run `claude`,
`codex` or `opencode` as you always do. Neither host needs tmux, a port, a password or
a special flag: the same Telegram UI — approvals, multiple-choice questions, replies,
`stop`/`kill`, `/rename` — works unchanged. Answering from Telegram dismisses the
prompt in the terminal, and answering at the terminal retires the Telegram keyboard
(both verified live against each binary).

How the daemon wires each host, automatically, at start and re-checked every minute:

- **OpenCode** — ctm drops one plugin file into OpenCode's global config
  (`~/.config/opencode/plugins/ctm.js`, honouring `XDG_CONFIG_HOME`), the same way it
  installs hooks into Claude Code's `settings.json`. Every OpenCode process loads it;
  it pipes OpenCode's event bus to the daemon over a local socket and runs the
  daemon's replies through OpenCode's own in-process API. A bare `opencode` therefore
  opens no network listener at all. The file is generated and regenerated by ctm
  (`ctm update` rolls it forward; `ctm doctor --fix` rewrites it) — don't edit it.
- **Codex** — three pieces, all automatic:
  - *Outbound*: ctm adds its own entries to `~/.codex/hooks.json`, merging with any hooks
    you already have, and trusts them using the hash Codex itself reports — so there is no
    "Hooks need review" prompt for you to answer.
  - *Inbound*: ctm keeps Codex's app-server running (`codex app-server daemon start`,
    idempotent) using Codex's native binary, and a `codex` started while it runs joins it.
  - *Approvals*: ctm's shell block defines a `codex` function adding
    `--remote unix://<socket> -C "$PWD"`, which puts your session in that app-server —
    where each approval is a request with an id ctm can resolve from Telegram. It passes
    through every subcommand and any explicit `--remote`/`-C`, does nothing while the
    daemon is down, and `CTM_CODEX_REMOTE=0` turns it off. Run `type codex` to see it.

`ctm doctor` check 12/13 "Hosts" shows what was detected and whether it is wired; a
host that is not installed is simply reported as such and watched for.

Turning a host off is a one-line config change or an environment variable — see
[Configuration](#config-file-alternative).

<details>
<summary>Also mirroring an <em>external</em> <code>opencode serve</code> over HTTP (optional)</summary>

The plugin covers every OpenCode process on this machine. For a server the plugin
cannot reach — another user's, or one in a container — the daemon can additionally
observe it over HTTP. It needs the explicit port (there is no port discovery) and the
server password (without one the server exposes `/pty` and shell endpoints to any local
process; `ctm doctor` makes a missing password a hard failure):

```json
{ "hosts": { "opencode": { "baseUrl": "http://127.0.0.1:4096", "password": "choose-a-secret" } } }
```

```bash
OPENCODE_SERVER_PASSWORD='choose-a-secret' opencode serve --port 4096
```

The daemon runs under launchd/systemd and does not see your shell, which is why the
password lives in `config.json` (mode 0600, like the bot token); if
`OPENCODE_SERVER_PASSWORD` *is* set in the daemon's environment it takes precedence.
</details>

Host limits, honestly reported by `ctm doctor`:

- **Codex sessions started while the ctm daemon is down are mirrored out but their
  approvals must be answered at the terminal.** ctm's shell block makes `codex` join the
  app-server, which is what makes an approval answerable from Telegram (it carries a
  request id that ctm resolves atomically); without it there is no safe way to answer a
  prompt remotely, and ctm will not fake one by typing into your terminal.
- **Codex assistant text for such sessions arrives once per turn** (its hooks expose the
  final message, not a stream); tool activity still appears as it happens. Sessions in
  app-server mode stream normally.
- Multiple-choice questions on Codex exist only in plan mode (`/plan`), a Codex limit.
- Neither host prints "answered from Telegram" in its own TUI after a remote decision —
  the prompt simply clears; ctm shows a toast on OpenCode and documents it on Codex.

## Project-Level Hooks

If your project has `.claude/settings.json` with custom hooks, global hooks are ignored. Install hooks to the project:

```bash
cd /path/to/your/project
ctm install-hooks --project
```

## How Messages Flow

| Direction | Event | Display |
|-----------|-------|---------|
| CLI -> Telegram | User types | User (cli): ... |
| CLI -> Telegram | Tool starts | Running tests (summarized) |
| CLI -> Telegram | The agent responds | Claude / Codex / OpenCode: ... |
| CLI -> Telegram | Session starts | New Forum Topic created |
| CLI -> Telegram | Context compacting | Notification sent (Claude Code) |
| CLI <-> Telegram | AskUserQuestion | Buttons in Telegram, native widget in the CLI; a Telegram answer drives that widget with paced keystrokes (ADR-015) |
| Telegram -> CLI | User sends message | Typed into the pane (Claude Code) or sent over the host API (OpenCode, Codex), and confirmed delivered |
| Telegram -> CLI | User sends photo | Downloaded, path injected |
| Telegram -> CLI | User types "stop" | Escape (Claude Code) or an interrupt over the host API |
| Host -> Telegram | Sub-agent activity | Shown inside the parent session's topic, tagged with the agent |

## Technical Details

- **Binary**: one native Rust executable (`ctm`), ~10 MB, no runtime dependencies
- **State**: SQLite at `~/.config/claude-telegram-mirror/sessions.db` — sessions, the topic
  ledger, pending approvals, and the tool details behind the **Details** button (kept 7 days)
- **Sockets**: `bridge.sock` (hooks and host observers) and `opencode.sock` (the OpenCode
  plugin), both 0600 inside a 0700 directory
- **PID file**: `bridge.pid`, `flock`-guarded so two daemons cannot race
- **Downloads**: `~/.config/claude-telegram-mirror/downloads/` (0700)
- **Agent output**: Claude's transcript `.jsonl` on Stop; OpenCode and Codex report their
  own text over their APIs
- **Topic routing**: each daemon only handles the topics it created, so several machines
  can share one supergroup
- **Rate limiting**: Governor-based, with a retry queue and exponential backoff
- **Token scrubbing**: every log line is filtered so a bot token cannot leak
- **Tests**: 878 passing across 48 source files and 13 integration test files. Nine more
  run only on request (`cargo test -- --ignored`) because they drive the real `codex`,
  `opencode` and Claude Code binaries end to end.

## Troubleshooting

Run the diagnostic tool first:

```bash
ctm doctor
ctm doctor --fix   # Auto-fix common issues
```

### Common Issues

**Hooks not firing?**
- Check if project has local `.claude/settings.json` overriding globals
- Run `ctm install-hooks -p` from project directory
- Restart Claude Code after installing hooks

**409 Conflict error?**
- Only one polling connection per bot token is allowed
- If running multiple systems, each needs its own bot (see Multi-System Architecture)
- Kill duplicate daemons: `ctm stop --force`

**Bridge not receiving events?**
- Check socket: `ls -la ~/.config/claude-telegram-mirror/bridge.sock`
- Check daemon logs for errors
- Run `ctm status` to verify daemon is running

**tmux injection not working? (Claude Code)**
- Verify the session: `tmux list-sessions`
- Check daemon logs for "Session tmux target stored"
- A reply that lands in the composer but is never submitted was fixed in 0.2.44 — ctm now
  confirms the submit and retries the Enter

**Codex: replies do not arrive, or approvals have no buttons?**
- `ctm doctor` (check 12/13) reports whether Codex's app-server is reachable and whether
  ctm's hooks are installed and trusted; `ctm doctor --fix` does both
- Approvals require the session to live in the app-server, which ctm's shell block arranges
  by adding `--remote` to a plain `codex`. Open a new shell after installing or updating,
  and check with `type codex`. `CTM_CODEX_REMOTE=0` turns it off
- A session started while the daemon was down still mirrors out, but cannot be replied to

**OpenCode: nothing mirrored?**
- ctm installs `~/.config/opencode/plugins/ctm.js`; `ctm doctor --fix` rewrites it
- Plugins load at startup, so restart `opencode` after installing ctm

**Too many topics?**
- Sub-agents stopped getting their own topics in 0.2.42 — update first
- Clear a backlog: `ctm prune-topics --ledger --dry-run`, then without `--dry-run`

**Messages going to wrong topic?**
- Clear session DB: `rm ~/.config/claude-telegram-mirror/sessions.db`

**Service not starting (Linux)?**
- `ctm service start` installs the unit first if it is missing
- Check status: `systemctl --user status claude-telegram-mirror`
- View logs: `journalctl --user -u claude-telegram-mirror -f`
- `Failed to connect to bus`? A plain SSH login has no systemd user manager. Run
  `sudo loginctl enable-linger $USER` and log in again — that is also what keeps the
  daemon alive after you log out.

**Service not starting (macOS)?**
- Check status: `launchctl list | grep claude`
- View logs: `cat ~/Library/Logs/claude-telegram-mirror.*.log`

## Build from Source

<details>
<summary>Click to expand</summary>

For developers who want to build from source or contribute:

```bash
# 1. Clone and build
git clone https://github.com/robertelee78/claude-telegram-mirror.git
cd claude-telegram-mirror/rust-crates
cargo build --release
# Binary at: rust-crates/target/release/ctm

# 2. Run tests (878 of them)
cargo test

# ...and the end-to-end ones, which drive real codex/opencode/tmux binaries
cargo test -- --ignored

# 3. Use the binary directly
./target/release/ctm setup
./target/release/ctm start
```

### Project Structure (48 source files)

```
rust-crates/ctm/src/
  main.rs · cli.rs    # CLI entry point and command definitions (clap)
  lib.rs              # Library re-exports
  hook.rs             # Claude Code hook event processing
  config.rs           # Configuration (env > file > defaults), including hosts
  error.rs · types.rs # Error types; wire types, validation, security constants
  session.rs          # SQLite: sessions, topic ledger, approvals, tool details
  socket.rs           # Unix socket server/client (flock, NDJSON)
  injector.rs         # tmux injection, with submit verification
  formatting.rs       # Formatting, chunking, ANSI stripping
  summarize.rs        # Tool action summarizer (30+ patterns)
  liveness.rs         # Pane/host liveness policy for topic reconciliation
  prune.rs            # prune-topics (host-aware liveness)
  update.rs           # Self-update: release record, verified download, atomic swap
  shell.rs            # PATH, completions, and the `codex` remote-mode function
  doctor.rs           # Diagnostics with --fix
  installer.rs        # Claude Code hook installer
  setup.rs            # Interactive setup wizard
  colors.rs           # ANSI helpers
  bot/                # Telegram API client (client, queue, types)
  daemon/             # Event loop; socket/telegram/callback handlers; cleanup;
                      #   reconcile; files; host_dispatch — the only host-aware seam
  host/               # ADR-016 hosts:
                      #   link            observer <-> daemon socket link
                      #   opencode        translator + HTTP observer
                      #   opencode_pipe   plugin transport (a bare `opencode`)
                      #   opencode_plugin the plugin ctm provisions
                      #   codex           app-server observer (JSON-RPC over WebSocket)
                      #   codex_rpc       one-shot RPC client (hooks/list, config write)
                      #   codex_hooks     the hooks ctm installs and trusts
                      #   codex_hook_cmd  `ctm codex-hook`, the forwarder
                      #   codex_daemon    keeps Codex's app-server alive
                      #   detect          finds host installs from a service PATH
  service/            # systemd, launchd, env file

rust-crates/ctm/tests/   # 13 integration test files, including host_e2e.rs (real
                         # codex/opencode) and injector_tmux.rs (real tmux)
```

</details>

## License

MIT

## Credits

Built so a coding agent — Claude Code, OpenCode or Codex — can be watched and driven from a phone.
