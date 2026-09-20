# Changelog

All notable changes to this project will be documented in this file.

## [0.2.41] - 2026-09-20

### Fixed (a machine reported "is current" minutes after a new release was published)
- **`ctm update` can no longer be answered from a cache.** The `releases/latest/download/…` record URL is a redirect whose target moves with every release, so an intermediary that caches it serves the previous version indefinitely. GitHub marks that redirect `no-cache` and ctm already sent `Cache-Control: no-cache`, but a proxy is free to ignore both; the URL now carries a unique timestamp (which GitHub ignores) so it is uncacheable by construction, and the request also sends `no-store` and `Pragma`.
- **"is current" now names what it compared against** — `ctm 0.2.41 is current (newest published release: 0.2.41)`. Previously an up-to-date install and a stale lookup printed the same sentence.

## [0.2.40] - 2026-09-20

### Fixed (reported from a Linux box: "Reply failed — the Codex observer is not connected")
- **A Telegram reply could have nowhere to go, depending on which path announced the session first.** Both of ctm's paths announce a Codex session — the hooks and the app-server observer — and the `session_start` dispatcher deduplicated the second one *before* binding the session to its observer. When the hook won that race (consistently so on the reporting machine) the observer was never bound, so injection had no destination even though the observer was connected and working. The binding now happens on every `session_start`, before the dedup; it is idempotent, and the rule for what may be bound (a long-lived native observer, never a hook process, never Claude Code) is now one tested function shared by both call sites.

## [0.2.39] - 2026-09-20

### Fixed (both reported from real use of a mirrored Codex session)
- **Every message appeared twice.** A Codex session in app-server mode is reported by *both* of ctm's paths: the hooks run inside the app-server process and the observer streams the same thread. The observer now claims a session (`hostTransport: "protocol"`) once its subscription actually succeeds, and the daemon drops hook-sourced duplicates for claimed sessions. While unsubscribed — a bare `codex`, or the opening moments of a session — the observer sees no content and does not claim, so the hooks remain the only source and still work. `SessionEnd` is never dropped.
- **Quitting Codex left the topic open.** A session that lives in the app-server outlives its terminal: quitting detaches the client, the thread stays loaded, and nothing is emitted — no `thread/closed`, and no `SessionEnd` hook, because the app-server's session has not ended. Verified directly: after quitting, the daemon logged nothing at all. ctm's shell function now reports the exit (`ctm codex-exited --cwd "$PWD"`, preserving codex's exit status), and the daemon ends the newest live Codex session for that directory.

## [0.2.38] - 2026-09-20

### Fixed (reported from a fresh Linux install)
- **`ctm service start` now installs the service if it is not installed.** On a new machine it failed with systemd's bare `Unit claude-telegram-mirror.service not found.` and no next step. It installs first, then starts. The launchd path does the same.
- **`ctm service install` no longer claims success it did not verify.** It wrote the unit file and then ran `systemctl --user daemon-reload` and `enable` with their results discarded, reporting "Service installed" either way. Both are checked now, and the real systemd error is shown.
- **A missing systemd *user manager* is explained rather than reported.** A plain SSH login without lingering has no user manager, so `systemctl --user` fails with `Failed to connect to bus`. ctm now says so and gives the fix (`sudo loginctl enable-linger <user>`, or exporting `XDG_RUNTIME_DIR`).
- **`ctm doctor --fix` installs and starts the service** when a configuration exists and the service is missing or stopped, instead of only printing the command to run.

## [0.2.37] - 2026-09-20

### Changed (the retry timer is gone — it was the wrong shape)
- **Codex subscription retries are event-driven, not timed.** 0.2.35 added a wall-clock retry window and 0.2.36 fixed how it was measured; both existed only because the obvious trigger, `turn/started`, is delivered to *subscribed* clients. But `thread/status/changed` **is** delivered to unsubscribed ones (spike-verified: 5 such events for a remote session), and a change to `active` is exactly when the rollout comes into being. ctm now retries a deferred `thread/resume` when the server says anything about a thread it is not subscribed to. No window to mis-measure, no give-up state, and a thread that can never be resumed simply stops being mentioned when it closes.

### Fixed (prune could offer to delete live conversations)
- **`ctm prune-topics --ledger` contradicted the daemon's own liveness policy.** `liveness.rs` says a session with no tmux route may only be declared dead by inactivity; `prune.rs` said "no tmux route → dead". With ADR-016 host sessions (no pane at all) and Claude Code started outside tmux, `--ledger` therefore listed live sessions as prunable — observed offering 11 deletions including two live ones. Prune now follows the same policy and, better, asks each host directly: Codex's app-server lists the threads it holds (`thread/loaded/list`), an OpenCode server lists its sessions. A host that cannot be reached falls back to inactivity, never to "dead". On the same machine the candidate list went from 11 (including live sessions) to 1 (a genuinely ended session).

### Fixed (test hygiene)
- The Codex approval end-to-end test ran against the *shared* app-server, so every run created a real thread that the operator's own daemon mirrored into a new Telegram topic. It now runs against an isolated `CODEX_HOME`, like the hooks test.

## [0.2.36] - 2026-09-20

### Fixed (found by running 0.2.35 as a user — the approval buttons never appeared)
- **The Codex observer gave up on subscribing one second after a session started.** 0.2.35 retried a deferred `thread/resume` on a timer, but measured the retry window from when the *observer connected* rather than per thread. A daemon that had been up for minutes — the normal case — treated the very first retry as already expired and abandoned the thread before its rollout could exist. The window is now per thread, from the first deferral, so a session started at any time gets subscribed and its approvals arrive in Telegram with buttons. The log line that fired in that case also claimed the session was a bare `codex`, which was wrong and is now only said after a genuine per-thread expiry.
- A thread awaiting subscription is tracked by the resume attempt itself rather than by whether it had been announced, so a thread discovered through `thread/loaded/list` is retried too.

## [0.2.35] - 2026-09-20

### Added (Codex approvals can be answered from Telegram — ADR-016 §Codex approvals)
- **Approvals on Codex are no longer terminal-only.** ctm's shell integration now makes a plain `codex` join the app-server (`--remote unix://<socket> -C "$PWD"`), where each approval is a JSON-RPC request with an id that ctm answers atomically and `serverRequest/resolved` retires the other surface. You still just type `codex`; the block passes through every subcommand (`exec`, `app-server`, `resume`, …), any explicit `--remote`/`-C`, and every run while the ctm daemon is down. `CTM_CODEX_REMOTE=0` opts out.
- `-C "$PWD"` is part of the rewrite because a remote session otherwise adopts the *daemon's* working directory — silently, which would have broken every project workflow.
- **The Codex observer retries a deferred subscription on a timer** (every 2 s for 90 s). ADR-016 hung that retry on `turn/started`, which only *subscribed* clients receive — so a deferred subscription could never recover. That is why a mirrored Codex session showed a topic but no replies.

### Changed
- ADR-016's claim that Codex rejects `permissionDecision: "ask"` is reinstated with evidence: the schema accepts it, the binary rejects it at runtime. Claude Code's fallback does not port to Codex.

### Not shipped, deliberately
- An earlier design answered Codex's own terminal prompt with a tmux keystroke. Every mechanic worked, but there is **no atomic link between a Telegram tap and the approval it is answering** — screen-capture and key-send are separate operations, so a keystroke can approve the *next* request. Codex's own review of the design refused it on the same grounds, and it is ADR-014's blind-injection failure applied to command authorization. No keystroke authorization path exists in ctm.

## [0.2.34] - 2026-09-20

### Added (Codex now mirrors OUT as well as in — ADR-016 §Codex outbound)
- **A bare `codex` is fully mirrored.** ctm installs its own entries in `~/.codex/hooks.json` (honouring `CODEX_HOME`), so Codex's agent replies, tool calls, prompts and session start/end reach Telegram. Until now only injection worked: the app-server cannot observe a thread a bare `codex` owns, so nothing came back.
- **No "Hooks need review" prompt, and no reverse-engineering.** ctm asks the app-server for its own hooks (`hooks/list` → `currentHash`) and persists that value through Codex's own config writer (`config/batchWrite`), then re-checks every 60 s — so `ctm update`, which changes the binary path and therefore the hash, re-trusts itself. Verified idempotent against Codex 0.155.1.
- **Merging, not clobbering**: existing hooks in that file are preserved (ctm's entries are marked `ctm:<event>` and run first); `ctm uninstall-hooks` removes only ctm's.
- `ctm doctor` 12/13 reports whether the hooks are installed and trusted, and `--fix` does both.

### Known limitations (both by design, both reported by `ctm doctor`)
- **Codex approvals stay terminal-only.** `PermissionRequest` carries no request id and an async hook cannot answer one later; a blocking hook that decided would suppress Codex's own prompt — the ADR-014 PR-E failure ctm exists to avoid. ctm registers no blocking hook.
- **Codex assistant text arrives per turn, not streamed** — `Stop` carries the final message only. Tool activity still streams via `Pre/PostToolUse`.

## [0.2.33] - 2026-09-20

### Fixed (found by running 0.2.32 as a user)
- **Codex topics never closed.** `thread/closed` (and the `notLoaded` status that precedes it) were not handled at all, so a Telegram topic stayed open after the Codex session exited and auto-delete never fired. Both are now translated to `SessionEnd`; both are broadcast to every app-server client, subscribed or not (verified against 0.155.1).
- **The "Session Started" card said "tmux: not detected — replies disabled" for OpenCode and Codex sessions.** Replies to those hosts go over their API and never needed tmux, so the card was false — it now names the host and its channel.
- **`install.sh` removes a previous npm install.** If the retired `claude-telegram-mirror` npm package is still present, the installer uninstalls it (its Node shim could otherwise shadow the new binary on `PATH`) and, when an existing configuration is found, runs `ctm doctor --fix` itself to re-point the service and the Claude Code hooks and wire the hosts — instead of printing that as a step for the user.
- README: no npm/Node references, and no suggestion to run `ctm completions` by hand (installs and updates do it).

### Known limitation (Codex, app-server 0.155.1)
- **A bare `codex` mirrors *into* Telegram but not *out* of it yet.** Injection works (verified: Telegram replies render in the Codex TUI and it answers), and session start/rename/end are mirrored. Agent messages, tool calls and approvals are not, because a second app-server client cannot subscribe to a thread another process owns: `thread/resume` answers `no rollout found` for the whole life of the thread (56 retries over 95 s, rollout file present on disk), `thread/items/list` is "not supported yet", and `thread/read` succeeds but opens no stream. `codex --remote unix://<socket>` sessions are unaffected. The fix is Codex's own hook system (same event vocabulary as Claude Code's), which ctm can install itself — tracked in ADR-016.

## [0.2.32] - 2026-09-19

### Changed (OpenCode and Codex are on by default — ADR-016 amendment §Default enablement)
- **Nothing to enable any more.** Install ctm and run `claude`, `codex` or `opencode`; all three are mirrored. `hosts.opencode` / `hosts.codex` in `config.json` are now opt-*out* (`"enabled": false`, or `CTM_OPENCODE_ENABLED=0` / `CTM_CODEX_ENABLED=0`).
- **OpenCode: a plugin ctm provisions itself.** A bare `opencode` has no network listener at all (spike-verified), so the daemon now writes `~/.config/opencode/plugins/ctm.js` (honouring `XDG_CONFIG_HOME`) the way it writes hooks into Claude Code's `settings.json`. The plugin forwards OpenCode's event bus to the daemon over a local socket (`opencode.sock` next to `bridge.sock`) and executes the daemon's replies through OpenCode's in-process API — no `--port`, no password. The daemon writes the file at start and re-checks every minute, so `ctm update` rolls it forward and an OpenCode installed later is picked up; `ctm doctor --fix` writes it too. When an OpenCode process exits its topics are closed instead of ageing out. Verified end-to-end against OpenCode 1.18.31: `permission.asked` arrives through the pipe, the reply dismisses the TUI prompt, the command runs.
- **Codex: ctm keeps the app-server daemon alive.** A bare `codex` started while `codex app-server daemon` runs joins it automatically (spike-verified), so the observer now runs `codex app-server daemon start` (idempotent) before each connect, through Codex's **native** binary — never the npm `codex.js` shim, which needs `node` that a launchd/systemd PATH does not reliably have. Not installed yet? Probed again every minute, quietly.
- **HTTP observation of an external `opencode serve`** (`hosts.opencode.baseUrl` + password) remains as an opt-in addition; it is no longer the way OpenCode gets mirrored.
- `ctm doctor` 12/13 "Hosts" now reports detection and wiring per host (plugin present/current, pipe socket, app-server socket, native binary) and `--fix` performs both wirings.

### Fixed
- **Tab completion in zsh when `compinit` already ran earlier in the rc** (oh-my-zsh, or another tool's block — the shipped 0.2.30/0.2.31 block skipped registration in that case, so `ctm <TAB>` completed file names). The block now registers `_ctm` directly with `compdef` when `compinit` has run, and no longer adds a duplicate `fpath` entry. Reproduced and verified in a clean-environment login zsh against a real rc. `ctm update` rewrites the block.
- Stale statements that a plain `codex` "never joins" the app-server daemon (README, ADR-016, `codex.rs`, doctor) corrected in place; `install.sh` and ADR-017 no longer claim the installer never touches the shell profile.

## [0.2.31] - 2026-09-19

### Fixed (found by running the 0.2.30 migration as a user)
- **`ctm service restart` now reloads the job when the binary path changed.** `launchctl kickstart -k` restarts launchd's *loaded* definition and never re-reads the plist, so after an npm→standalone migration `ctm doctor --fix` rewrote the plist, reported "restarted", and the old npm binary kept running. The restart path now compares the loaded `program` with the plist's `ProgramArguments` and, on mismatch, boots the job out and bootstraps it from disk. Verified live: loaded program flipped to `~/.local/bin/ctm`.
- **`ctm doctor` no longer reports phantom duplicate hooks when run from `$HOME`.** With the current directory equal to the home directory, the "project" scope resolves to the same `~/.claude/settings.json` as the global scope, and one file was counted under two scopes. Scopes that alias the same file are now collapsed.

## [0.2.30] - 2026-09-19

### Added (automatic PATH + tab completion — ADR-017 amendment)
- **`ctm shell-setup [--remove]`**, run by `install.sh` and after every `ctm update`: writes static completions for bash, zsh and fish to their per-user autoload dirs and maintains one idempotent, marker-delimited block at the *end* of your shell rc that puts `~/.local/bin` first on `PATH` (so it wins over fnm/nvm shims that prepend earlier) and, for zsh, wires the completion dir into `fpath`. Only the login shell's rc is created; other shells are touched only if their config exists. `--remove` restores the rc exactly. `CTM_NO_SHELL_SETUP=1` opts out.
- **`ctm completions <shell>`** prints the completion script.
- **Topic titles name the host** for OpenCode and Codex sessions (`OpenCode • host • project • id`), since all hosts share one forum. Claude Code topic names are unchanged.

### Fixed
- Release workflow: darwin-x64 packaging failed with `sha256sum: command not found` (a `shasum` shim defined in a nested subshell). macOS steps now call `shasum -a 256` directly. 0.2.29 therefore never published; 0.2.30 is the first release on the GitHub Releases channel.

## [0.2.29] - 2026-09-19 (tagged; not published — see 0.2.30)

### Changed (distribution moved to GitHub Releases — ADR-017)
- **npm is retired.** ctm is now one static binary per platform, published as GitHub Release assets and installed with `curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/install.sh | sh`. The installer resolves the per-target release record (`stable-<target>.json`) through GitHub's `releases/latest/download/` redirect, downloads `ctm-<target>` from that exact release, verifies size and SHA-256, and installs atomically to `~/.local/bin` with a channel marker. No Node.js anywhere. The 0.2.28 npm publish had failed on an expired registry token after all four binaries built — the last time that failure mode can happen.
- **`ctm update [--check] [--rollback]`** — self-update through the same record → verified download → atomic rename swap, keeping one `.ctm-previous` for rollback, refusing to overwrite a binary it did not install, restarting the service afterwards. Running it from an npm-installed binary performs the migration to the standalone channel and re-points the service and hooks.
- **`ctm doctor` check 13/13 "Update"**: install channel, latest release vs running version, and drift (service unit or Claude Code hooks pointing at a different binary), fixable with `--fix`.
- `npm-packages/`, `package.json`, the Node wrapper/postinstall shims and `.npmignore` are removed; `scripts/bump-version.sh` now updates only `Cargo.toml` + `Cargo.lock`. The release workflow uploads binaries, `.sha256` files, records and `install.sh` to the GitHub Release and no longer touches npm.

## [0.2.28] - 2026-09-19

### Added (OpenCode and Codex as agent hosts — ADR-016)
- **ctm now mirrors OpenCode and Codex sessions, not only Claude Code.** Each non-Claude host gets an in-daemon *observer* — a long-lived client of ctm's own Unix socket speaking the existing `BridgeMessage` protocol — so the daemon's approval routing, priority queue, topic buffering and the whole Telegram question UI are reused unchanged. Neither host needs tmux: OpenCode is driven over its HTTP/SSE API (`/event`, `/permission/{id}/reply`, `/question/{id}/reply`, `/session/{id}/prompt_async`) and Codex over its app-server JSON-RPC (WebSocket on the `codex app-server daemon` control socket: `thread/resume`, `turn/start`, `turn/steer`, approval and `requestUserInput` responses). The daemon dispatches on a new `sessions.host_kind` column at exactly three points (`daemon/host_dispatch.rs`); `callback_handlers.rs`' TUI scraper stays Claude-only.
- **Both surfaces, either answers — verified live on both hosts.** Answering an approval or multiple-choice question from Telegram clears the prompt in the attached terminal; answering at the terminal retires the Telegram keyboard (`permission.replied` / `serverRequest/resolved` → new `handle_approval_resolved_elsewhere`). Race resolution is exactly-once on both hosts; ctm finalizes on the host's own resolution signal, never on its own send.
- **Config**: `"hosts": {"opencode": {"baseUrl", "password", "passwordEnv"}, "codex": {"socketPath"}}` in `config.json`, or `CTM_OPENCODE_URL` / `CTM_CODEX_SOCKET`. The OpenCode password is resolved env-var-first then config-file, because a launchd/systemd-managed daemon does not inherit the operator's shell; it is redacted from `{:?}` output like the bot token. Defaults: `http://127.0.0.1:4096`, `OPENCODE_SERVER_PASSWORD`, `~/.codex/app-server-control/app-server-control.sock`.
- **`ctm doctor` check 12/12 "Hosts"**: reachability, auth (hard failure if the OpenCode password is unset — the server is otherwise unauthenticated while exposing `/pty`), Codex control-socket presence and mode, and each host's capabilities (structured questions, steer, always-allow, image injection).
- **`tests/host_e2e.rs`** (`cargo test --test host_e2e -- --ignored`): end-to-end against the real `opencode` and `codex` binaries, zero model spend.

### Changed
- ADR-004's "tmux is the sole injection method" now applies to Claude Code sessions only (amended, not reversed). ADR-003/005/011/012/013/015 status headers reconciled to what shipped.
- New dependencies: `tokio-tungstenite` (handshake only, no TLS) and `futures-util` for the Codex WebSocket transport.

### Fixed
- `scripts/bump-version.sh` now works on macOS: BSD `sed -i` requires a backup suffix, so the script's bare `-i` consumed the expression as the suffix and failed with `undefined label`. A `sedi` helper detects GNU vs BSD once.
- Five clippy lints newly enforced by Rust 1.98 (CI's current stable) — three in code predating this release.

## [0.2.27] - 2026-06-19

### Fixed (replies misrouted across concurrent sessions; first events lost on restart)
- **Telegram replies no longer land in the wrong Claude session (ROUTING-002).** With multiple Claude sessions in one tmux server, every session's stored tmux target collapsed onto whichever pane the user was looking at, so a reply meant for one session was injected into another. Root cause (proven via live tmux repro + tmux(1)): the hook captured a **positional** target (`session:window.pane`) from bare `tmux display-message`, which tmux resolves against the attached client's *active* pane — not the pane the hook ran in. The hook now routes on the **stable `$TMUX_PANE` pane id** (e.g. `%24`), which tmux passes to each pane's child processes and never reuses for the pane's lifetime. A one-time startup migration clears stale positional targets so they cannot misroute after upgrade.
- **No more silently-dropped messages when a session's topic is still being created.** During a bridge restart, a session's first events ("No topic — dropping …") were lost when forum-topic creation hit a transient Telegram error. Topic creation now retries with bounded backoff on transient failures (never on a genuine "not a forum"), and content events are buffered (bounded, per session) and flushed once the topic exists rather than dropped. `approval_request` resolves topic-readiness before creating any approval row, so a replay creates exactly one prompt.

### Added (hook-install duplicate protection)
- **`ctm doctor --fix` now detects and cleans duplicate / double-firing hooks.** Claude Code merges hooks across global, project, and `settings.local.json` scopes and only de-duplicates *byte-identical* commands at runtime — so the same ctm hook with a differing path/form across scopes, or duplicated within a file, executes more than once per event. `ctm doctor` flags this and `--fix` consolidates to a single canonical scope (broadest wins: global > project > local); `ctm hooks` reports presence per scope and warns on duplicates.
- **`ctm install-hooks -p` is now cross-scope aware:** it skips a hook type already present in another scope (use `--force` to override), and `ctm uninstall-hooks -p` removes from the project `settings.json` + `settings.local.json`.

### Changed (hook install correctness + setup guidance)
- Hook install consolidates pre-existing in-file duplicates and stale/old-format entries to a single canonical entry (operating at the inner-command level, always preserving non-ctm hooks), and treats a hook as "already correct" structurally (matcher/format/timeout), not by command string alone.
- The setup wizard no longer offers to install project-level hooks alongside global (which manufactured the double-fire) and corrects the previous, inaccurate "project settings override global hooks" guidance — global hooks apply to every project.

## [0.2.26] - 2026-06-18

### Fixed (macOS `ctm restart` left the daemon stopped)
- **`ctm restart` now reliably brings the daemon back up.** On macOS, restart was implemented as stop-then-start — but `launchctl stop` is asynchronous and returns *before* the process exits, so the following `launchctl start` was coalesced/ignored while launchd was still tearing the old instance down. The old process then exited cleanly (status 0), and because the service's `KeepAlive { SuccessfulExit: false }` policy does not relaunch a clean exit, the daemon was left **stopped** — `ctm restart` failed to recover it (reproducible deterministically, independent of the first-launch code-signing kill). Restart now uses `launchctl kickstart -k`, which kills any running instance and starts a fresh one as a single atomic operation — no race, no dependence on KeepAlive semantics (with a synchronous stop→wait→start fallback). Verified to return in ~1s with a new PID, confirming a real relaunch.

## [0.2.25] - 2026-06-18

### Fixed (macOS `ctm start` reported a false failure when launchd auto-recovered)
- **`ctm start` no longer cries "Service was started but is not running — it exited immediately" when the daemon actually comes up a few seconds later.** The 0.2.24 liveness check (added so a launch that gets SIGKILLed isn't falsely reported as "started") only waited ~2.6s. But when macOS kills a non-notarized binary at first launch, the service's `KeepAlive { Crashed: true }` policy relaunches it only after the 10s throttle interval — so the check saw no process in its short window, reported a code-signing failure, and *then* launchd quietly brought the daemon up (a subsequent `ctm status` showed it Running). The error was accurate for that instant but misleading. The check now polls for a **stable PID** for up to 14s (longer than the throttle window): a healthy start still returns in ~1s, a first-launch kill is ridden out across launchd's relaunch and reported as success once the daemon is actually up, and only a service that never stabilises is reported as a failure (with a note that launchd keeps retrying in the background). A one-line "waiting…" message is shown so a multi-second wait isn't silent.

## [0.2.24] - 2026-06-18

### Fixed (macOS service: false "running" + silent code-signing kills)
- **`ctm status` / `ctm doctor` / `ctm service status` no longer report a dead daemon as "Running", and `ctm start` can recover it.** `get_launchd_status()` decided "running" by checking whether the service label merely *appeared* in `launchctl list` — but a loaded-but-dead launchd job still appears there with `-` in the PID column. So a daemon that had cleanly stopped (a clean exit is not auto-restarted by the plist's `KeepAlive`) or been killed was reported as running, and `ctm start` short-circuited to **"Daemon is already running"** — a no-op that made recovery impossible. The status now parses the PID column (via a pure, unit-tested `parse_launchd_pid()`) and is "running" only when a real positive PID is present. (systemd's `is-active` check was already correct — this was launchd-only.)
- **A service that is SIGKILLed at launch is now reported as a failure instead of a false "Service started."** On macOS the kernel can kill a non-notarized binary milliseconds after `exec` for a code-signing / launch-constraint violation (`EXC_CRASH` / "Code Signature Invalid"). `launchctl start` only confirms launchd *accepted* the request, so the old code reported success for an already-dead daemon. `start_launchd_service()` now verifies the process actually came up and stayed up (~2s), and on failure prints actionable guidance (crash report, `codesign -dvvv`, quarantine removal, `ctm doctor`).

### Changed (macOS binary robustness — defense-in-depth for code signing)
- **postinstall** now hardens the native binary on macOS: strips any `com.apple.quarantine` flag, re-applies an ad-hoc signature if the existing one is invalid, then verifies and smoke-tests `ctm --version` — warning loudly (without failing the install) if the OS would refuse to run it.
- **release CI** now Developer-ID-signs and notarizes the darwin binaries (both arm64 and x64) with a `codesign --verify --strict` gate. Gated on `APPLE_*` secrets; until those are configured, builds fall back to ad-hoc signing with a warning. (Note: shipped binaries were previously only ad-hoc/linker-signed; npm provenance is a supply-chain attestation, not Apple notarization.)

## [0.2.23] - 2026-06-17

### Fixed (ROUTING-001 — cross-session misrouting of Telegram→CLI input)
- **A reply typed in one session's topic could be injected into another session's tmux pane** (e.g. text meant for session-2 landed in pane 0 / session-1). Two independent defects, both confirmed by manual real-tmux reproduction spikes and an adversarial Codex review:
  - **TOCTOU race on a shared, stateful injector.** The single process-wide `InputInjector` held a mutable `tmux_target`. The Telegram text handler set the target under one lock, dropped it, `await`ed, then re-acquired a fresh lock to inject — reading whatever target was current. Because every Telegram update is processed on its own task, a concurrent handler for another session could overwrite the shared target in that window.
  - **`get_tmux_target` fallback poisoning.** When a session's pane was missing from cache and DB, it guessed the *first* tmux pane running `claude` as `"{session}:0.0"` (pane 0) and persisted that guess — permanently binding the session to the wrong pane.
- **Fix:** `InputInjector` is now **stateless** — every action (`inject`, `inject_literal`, `send_key`, `send_slash_command`, `capture_pane`, `validate_target`) takes the `(target, socket)` pair explicitly, so the shared injector's `Mutex` serializes tmux execution only and can never bleed one session's pane onto another's. The live-detection fallback was removed: a missing per-session mapping now **fails closed** ("tmux not detected") instead of misrouting. The startup default target (which biased every session toward pane 0) was removed. Hardening: the topic→session lookup is now deterministic (`ORDER BY last_activity DESC`) when a parent and sub-agent share a topic, and a changed tmux socket is reconciled even when the pane string is unchanged. Covered by `tests/routing_spike.rs` (real-tmux: misroute reproduced pre-fix, zero crosstalk post-fix under concurrent stress).

### Fixed (STALE-TOPICS — liveness-driven topic reconciliation)
- **Dead sessions' Telegram topics no longer pile up.** Topic teardown previously relied on the `SessionEnd` hook (which does not fire on a terminal close, `kill -9`, `tmux kill-session`, or reboot) plus a 24h inactivity timer, so orphaned topics accumulated and a daemon restart never reconciled the backlog. A new reconciliation sweep (`daemon::reconcile`) makes pane/Claude liveness the prompt, authoritative signal, keyed on the *specific Claude session* (pane gone, pane reassigned to a newer session, or pane fell back to a shell prompt → topic pruned). The liveness policy (`liveness.rs`) is a pure, unit-tested rule shared by the daemon sweep and `ctm doctor --fix`, and a persistent topic ledger lets `doctor` reconcile topics created before it started. Also fixes a teardown leak.

### Added
- **`ctm prune-topics`** — clear an accumulated backlog of stale Telegram forum topics. `--ledger` deletes ledger-recorded topics whose Claude session is no longer alive; `--from`/`--to` (with `--ids` support) sweeps a numeric topic-id range to reach legacy orphans that predate the ledger (the Bot API cannot enumerate forum topics). Both modes refuse to touch the General topic and any currently-active session's topic, so a live conversation can never be pruned.

## [0.2.22] - 2026-06-01

### Fixed (ADR-015 — multi-question AskUserQuestion injection, N-generic)
- **Answering a multi-question AskUserQuestion from Telegram now works for any number of questions (1, 2, 3, … N).** The 0.2.20/0.2.21 path treated a multi-question widget as N separately-submitted single-question widgets, so a 2-question widget left Q2+ unanswered and leaked a stray keystroke into the prompt. Empirical capture of Claude Code 2.1.159 (binary string-mining + live `tmux capture-pane` at N=1/2/3) established the real model — **one tabbed widget** with a per-question advance row labelled `Next` (non-final) / `Submit` (final) and a single end-of-widget `Ready to submit your answers?` confirm screen — and `inject_answers` was rewritten to drive it: place the cursor on each row (verified by re-reading the pane), let single-select selections / free-text commits auto-advance, navigate multi-select to its `Next`/`Submit` row, then confirm once at the end. Works for single-select, multi-select, free-text (`Type something`), and any mix across N questions.
- **No more stray keystrokes / blind Enters.** Cursor placement is verified against the live pane and **fails closed** if it can't confirm the target (the old "best-effort Enter" that leaked keys is gone). Every screen transition is awaited (next question active / confirm screen) rather than slept through.
- **Free-text answers inject cleanly.** Free-text is typed literally into the `Type something` row via a new no-trailing-Enter injection path (the previous path appended an Enter that could submit prematurely) and is sanitized (control characters stripped, length-capped).
- **Safe partial-failure handling.** If injection fails after any keystroke has landed, ctm no longer blind-retries from question 1 (which would corrupt a half-advanced widget) — it marks the answers terminal and asks you to finish at the terminal, where the widget is still on screen. If nothing was delivered, it safely restores the Telegram buttons for a retry.
- **Submit-All validation & stale-button safety.** Answers are validated per-question before submitting: an unanswered question, an out-of-range option, or an empty multi-select is rejected with a clear prompt (previously a stale button — e.g. tapping an old question's message after a new question replaced it — could finalize a partial/empty submit as success). Tapping such a stale option button now shows "no longer available" instead of crashing the daemon.

## [0.2.21] - 2026-05-31

### Fixed (ADR-015 — multi-select AskUserQuestion injection)
- **Answering an AskUserQuestion from Telegram now submits cleanly.** Live validation on Claude Code 2.1.159 revealed the multi-select widget has **no separate "review your answers" screen** (submit is an inline "Submit" button) and has rows *below* Submit ("Type something", "Chat about this"). The 0.2.20 path used a fixed Down-count plus a second blind auto-submit Enter, which could overshoot or fire a stray post-submit keystroke (surfacing as a spurious "clarify"). Replaced with **capture-pane-driven navigation**: toggle the chosen options by digit key, then press Down until the cursor sits on "Submit", then a **single** Enter — bounded by the total option count so it never under- or over-shoots, and the redundant second Enter is gone.
- **Single-select answers from Telegram** now select by the option's digit key instead of injecting the label as literal text (which risked being treated as free-text by the widget).

## [0.2.20] - 2026-05-31

### Fixed (ADR-015 — restore AskUserQuestion to both surfaces)
- **AskUserQuestion renders in BOTH the CLI and Telegram again, answerable from either.** ADR-014 PR-E (0.2.18) delivered option answers via a blocking `PreToolUse` hook returning `updatedInput`, which suppressed Claude's native terminal widget — so questions appeared *only* in Telegram. This reverts that interception: Claude renders the question in the CLI as before, ctm mirrors it to Telegram from the fire-and-forget `tool_start`, answers inject back via tmux, and the Telegram buttons stale ("✅ Answered at terminal") when the question is answered at the keyboard (detected via the `PostToolUse` result). Restores ctm's bidirectional-mirror model.
- **Multi-select submit is race-free and faster.** The fixed ~3,500 ms of blind sleeps (1500 ms render wait + 2000 ms auto-submit) that could fire the confirming Enter *before* Claude's review screen rendered is replaced with adaptive `tmux capture-pane` readiness polling (150 ms steps, 3 s cap) — typically ~150–450 ms and never premature.
- **Concurrency-hardened question handlers.** A `QuestionLifecycle` state machine is the single arbiter across all answer paths; no daemon lock is held across Telegram I/O; `pending_q` removals are `Arc::ptr_eq` identity-checked; every keyboard-arming edit re-stales if the question resolved mid-edit. Closes the lock-order deadlock, render-window, lock-across-I/O, and orphaned-button classes surfaced by multi-pass adversarial (Codex) review.

### Changed
- The approval hook timeout is now its own constant (`DEFAULT_APPROVAL_WAIT_SECS`, 300s + 10s buffer); the `question_wait_secs` / `TELEGRAM_QUESTION_WAIT_SECS` knob is removed (the question hook no longer blocks).

## [0.2.19] - 2026-05-31

### Fixed (ADR-014 post-release field defects)
- **AskUserQuestion now reaches Telegram under `--dangerously-skip-permissions`** (D1, root cause) — the `PreToolUse` hook short-circuited on `bypassPermissions` *before* the AskUserQuestion branch, so in the operator's normal mode the question was never sent and Claude fell back to its terminal TUI. AskUserQuestion is now routed ahead of the bypass check (it's input collection, not a permission gate).
- **Question widgets render as plain text** (D2) — header/question/option/answer text is arbitrary model content; sending it under Telegram Markdown v1 (with backtick-only escaping) caused HTTP 400 "can't parse entities" that silently dropped the widget (including the "Submit All" message). All question-lifecycle messages now render as plain text via a shared renderer.
- **No more silent ~5-minute hangs** (D3/D4) — every render drop/error path (no topic, missing input, empty questions, render failure, supersede) now releases the blocked hook to the terminal instead of leaving it to time out; the misleading "retrying via queue" log (which never retried) is corrected.
- **Mirror-storm resilience** (D6) — `ToolStart` previews moved to a new Low queue priority so a tool-spam storm can no longer starve or evict substantive `ToolResult`s or approvals/questions; the per-drop log spam is replaced by a throttled 5-second aggregate.
- **`ctm doctor` warns about competing PreToolUse hooks** (D7) — Claude Code ignores hook `updatedInput` when multiple `PreToolUse` hooks are registered (anthropics/claude-code#15897), which would silently break structured AskUserQuestion answers.
- **Configurable AskUserQuestion wait** (D8) — `question_wait_secs` (env `TELEGRAM_QUESTION_WAIT_SECS` / config `questionWaitSecs`, default 300) replaces hardcoded timeouts; a pre-emptive "answer at the terminal" notice posts near expiry.
- **Resilient rate-limit handling** (D5) — Telegram 429s are now retried up to 3× (honoring `retry_after`) instead of once, so a rate-limit burst can't strand the direct question/summary sends.
- **Collision-safe answer routing** — an ambiguous 20-char session-id prefix in callback data is now refused (logged) rather than risking misdelivery to the wrong session.

## [0.2.18] - 2026-05-28

### Added (ADR-014)
- **Structured AskUserQuestion answers** — option/multi-select answers are now delivered to Claude Code via a blocking `PreToolUse` hook returning `updatedInput` (the same correlation the approval flow uses), replacing fragile 300ms-per-key tmux keystroke injection. ~6 orders of magnitude lower answer-delivery latency and no TUI readiness race. Free-text retains an isolated keystroke fallback.
- **Event-driven session teardown** — the `SessionEnd` hook is now registered (`session_exit_reason` parsed, `resume` special-cased so a suspending session is not torn down). True termination deletes the forum topic immediately and clears `thread_id` synchronously; custom titles persist across daemon restarts.
- **Approval reliability** — approval requests sent at Critical priority; double-taps are idempotent (no duplicate hook responses); the message is edited to a decision+time audit line with the keyboard removed; the approval→client map no longer leaks.
- **Setup trust acknowledgment** — the wizard now requires an explicit, recorded `y/N` acknowledgment of the chat-level trust model before writing config (new setups only); mirrored in the README.

### Fixed
- Removed fabricated `adaptive_retry` dead code (a Bot API field that does not exist); rate-limit backoff honors `retry_after` only.
- Multi-select answer labels are joined with a bare comma (Claude Code format), verified against the reference implementation.

See `docs/adr/ADR-014-lifecycle-hooks-approval-ux-and-input-reliability.md` for the full design, benchmark, and review log.

## [0.2.1] - 2026-03-17

### Changed
- Version bump from 0.2.0 due to partial npm publish (linux-x64 0.2.0 already on registry)

## [0.2.0] - 2026-03-17 (updated 2026-03-17)

### Release Readiness (post-initial)

**Security:**
- **Bounded NDJSON line reading** -- replaced `AsyncBufReadExt::read_line` (which accumulates without limit before the newline is found) with a new `read_bounded_line` helper that stops accumulating once `MAX_LINE_BYTES` are consumed, then drains to the next newline to keep the stream frame-aligned. Prevents a newline-free payload from exhausting memory before the size check fires.

**Type safety:**
- **`SessionStatus` and `ApprovalStatus` enums propagated to all call sites** -- `end_session` and `resolve_approval` now accept typed enum values instead of raw `&str`, eliminating the runtime string-validation step. `row_to_session` and `row_to_approval` parse DB strings into enums at deserialization time with a safe fallback for unknown values.

**Test coverage expanded to 512 tests:**
- **`bot_tests.rs`** -- new integration test file covering Telegram bot client: message sending, forum topic management, rate limiting, and callback query handling
- **`daemon_handlers.rs`** -- new integration test file covering socket and Telegram handler logic: session routing, approval flow, echo prevention, and cleanup sequences
- `concurrency.rs`, `config_validation.rs`, and `session_lifecycle.rs` expanded with additional cases

### Polish (ADR-009)

- **Eliminated process-global umask race** — socket permissions now set via `chmod` instead of `umask`, fixing thread-safety issue that caused intermittent test failures and could affect multi-threaded deployments
- **Socket path validation tightened to AF_UNIX limit** — 256 → 104 bytes to match actual kernel limit
- **Topic creation race condition fixed** — atomic check-and-insert prevents duplicate forum topics under concurrent session starts
- **Message queue bounded** — capped at 500 messages with oldest-eviction to prevent OOM under sustained send failures
- **Rate limiter clamped** — `[1, 30]` msgs/sec to stay within Telegram's API limits
- **Config parse logging** — invalid env var values now warn instead of silently falling back to defaults
- **Mirror status write errors logged** — previously silently discarded
- **Consistent char-count measurement** — `estimate_chunks`, `needs_chunking`, and `truncate` all use character count (not byte length)
- **Transcript state file cleanup** — `.last_line_*` files cleaned up on session end instead of accumulating indefinitely
- **Removed duplicate code** — consolidated `truncate_path` → `short_path`, removed duplicate test coverage
- **Retry backoff overflow-safe** — `saturating_mul` prevents integer overflow at high retry counts
- **Echo prevention key uses null separator** — `\0` instead of `:` eliminates the theoretical collision class between session IDs and text
- **Renamed `escape_markdown` to `escape_markdown_v1`** — clarifies this is Telegram Markdown v1 escaping (backticks only)

### Deep Audit Fixes (ADR-010)

**Security (Round 1 -- all resolved):**
- **S-1: Path traversal on `transcript_path` fixed** -- hook-supplied paths are now validated (absolute, canonicalized, safe-prefix check, no null bytes) before `fs::File::open()`
- **S-2: Approval response routing fixed** -- responses are routed to the specific socket client that submitted the request, not broadcast to all connected clients
- **S-3: `db_op` panic replaced with `Err`** -- `spawn_blocking` task cancellation during shutdown now returns an error instead of crashing the daemon
- **S-4: `Config` Debug redaction** -- custom `Debug` impl redacts `bot_token` to `"[REDACTED]"`, preventing token leakage through `{:?}` formatting

**Correctness (Round 1 -- all resolved):**
- **C-1: Echo key separator mismatch fixed** -- `add_echo_key` and `handle_user_input` now use the same `\0` separator (were using `\0` vs `:`)
- **C-2: RAII processing guard** -- `ProcessingGuard` drop guard prevents permanent queue stalls if an async task panics or is cancelled
- **C-3: TOPIC_CLOSED error return** -- failed reopen now returns `Err` immediately instead of falling through to unrelated retry logic
- **C-4: Atomic `end_session`** -- session status update and approval expiry wrapped in a single SQLite transaction
- **C-5: Session ID validation at persistence boundary** -- `is_valid_session_id()` called before all database writes
- **C-6: Status enum validation** -- `SessionStatus` and `ApprovalStatus` enums replace raw strings, preventing typo-induced data corruption

**Unicode / Formatting (Round 1 -- all resolved):**
- **U-1: Char-boundary-safe message chunking** -- all length checks use `.chars().count()`, split points use `char_indices()`, header size reserved before splitting
- **U-2: Char-safe truncation** -- topic name and filename truncation use `.chars().take(N)` instead of byte slicing

**Packaging (Round 1 -- all resolved):**
- **P-2: `prepublishOnly` guard** -- platform packages fail to publish if `bin/ctm` binary is missing
- **P-3: `setup-node` for npm provenance** -- `actions/setup-node@v4` added to release workflow for OIDC token injection

**Round 2 blockers identified (7 items):**
- **R2-B3: Rate limit default changed from 1 to 20** -- previous default caused extreme message delays under normal load
- **R2-B6: `flock()` advisory lock on PID file** -- prevents double-start race where two concurrent `ctm start` commands both create daemons
- **R2-B7: CI failure exit when platform packages unavailable** -- registry propagation loop now exits non-zero instead of silently publishing a broken main package

### Breaking Changes

- **TypeScript source removed** — the package now ships a pre-compiled native binary; there are no `.js` or `.ts` files to import
- **Node.js no longer required at runtime** — Node.js is used only during `npm install` to download the binary for the target platform; the daemon and hook binary run standalone
- **`telegram-hook` bin entry removed** — replaced by the unified `ctm hook` subcommand
- **Public library API removed** — `import { ... } from 'claude-telegram-mirror'` is no longer supported; the package is now a CLI/binary distribution only

### Added

- **Complete Rust rewrite** — 30 source files (14 top-level modules + 3 sub-module groups), 512 tests (unit + 10 integration test files), ~12,000 lines of Rust replacing the TypeScript implementation
- **Single static binary** — ~9 MB self-contained binary with sub-millisecond hook latency (<1 ms)
- **Tool summarizer** — 30+ regex patterns condense verbose tool output into compact Telegram messages
- **AskUserQuestion rendering** — inline keyboard buttons displayed in Telegram for interactive Claude prompts
- **Photo and document download** — files sent to a Telegram topic are downloaded and injected into the Claude session
- **Session rename via `/rename`** — renames both the Telegram forum topic and the active tmux window to keep labels in sync with Claude Code
- **`doctor --fix` auto-remediation** — detects and automatically corrects common configuration problems
- **Governor token-bucket rate limiter** — per-chat rate limiting with configurable burst and refill, including retry/backoff for Telegram API calls
- **`flock(2)` atomic PID locking** — eliminates the TOCTOU race present in the previous read-then-write PID-file scheme
- **Global regex-based token scrubbing** — bot tokens and other secrets are redacted from all log output before writing
- **SIGTERM signal handler** — daemon performs a clean shutdown (flushes queues, closes sockets) when it receives SIGTERM
- **`linux-arm64` platform support** — pre-built binary available for ARM64 Linux (e.g., Raspberry Pi, AWS Graviton)
- **Interactive setup wizard** — `ctm setup` uses `dialoguer` to guide first-time configuration without manual config editing
- **TypeScript detection in code blocks** — code blocks in Claude output are annotated with the detected language for syntax-highlighted display
- **Integration test suite** (ADR-008) — 10 test files covering CLI smoke tests, concurrency, config validation, formatting, hook pipeline, session lifecycle, socket roundtrip, summarizer, bot client, and daemon handlers
- **Binary integrity verification** (ADR-008) — `checksums.json` in the release workflow for verifiable artifact hashes
- **Structural decomposition** (ADR-008) — bot/, daemon/, and service/ modules split into focused sub-modules (e.g., `bot/client.rs`, `bot/queue.rs`, `daemon/event_loop.rs`, `daemon/cleanup.rs`, `service/systemd.rs`, `service/launchd.rs`, `service/env.rs`)

### Security

- **Shell injection eliminated** — all subprocess calls use `Command::arg` (no shell interpolation); `execSync` with user-controlled strings is gone
- **Bot token scrubbing** — token is redacted from logs and error messages at the point of emission
- **Session ID validation** — session identifiers are validated against `[a-zA-Z0-9._-]`, maximum 128 characters, before use in any file path or socket name
- **Socket path traversal prevention** — computed socket paths are checked to confirm they remain within the expected runtime directory
- **Config directory permissions enforced** — config directory is created with mode `0o700`; existing directories with wrong permissions are rejected
- **File permissions enforced** — config and PID files are created with mode `0o600`
- **NDJSON line size limits** — incoming NDJSON lines are capped at 1 MB to prevent memory exhaustion
- **Connection concurrency limits** — the Unix socket listener rejects connections beyond a limit of 64 concurrent clients
- **IDOR check on approval callbacks** — callback query payloads are validated to ensure the requesting Telegram user matches the session owner before approving a tool call
- **`chmod(0o600)` after socket bind** — socket file permissions set via post-bind `chmod` (ADR-009: replaced process-global `umask` which caused race conditions in multi-threaded contexts)

### Fixed

- **BUG-001: tmux target auto-refresh** — stale tmux pane targets are detected and refreshed automatically
- **BUG-002: Topic creation race prevention** — concurrent session-start events cannot create duplicate forum topics
- **BUG-003: Stale session cleanup with differentiated timeouts** — sessions without tmux info use a shorter inactivity timeout than sessions with a known-dead pane
- **BUG-004: Escape vs Ctrl-C distinction** — `/stop` sends Escape (pause Claude); `/kill` sends Ctrl-C (exit Claude)
- **BUG-005: Ignore General topic** — messages posted to the forum's General topic are silently dropped
- **BUG-006: Stateless hooks** — hooks contain no local state; the daemon is the single source of truth
- **BUG-009: Session reactivation** — sessions previously marked `ended` are reactivated when a new hook event arrives
- **BUG-010: On-the-fly session creation** — the daemon creates a forum topic on the first hook event for an unknown session without requiring a prior `session_start` signal
- **BUG-011: Echo prevention** — text injected from Telegram into tmux is not echoed back as a new Telegram message
- **BUG-012: Topic deletion cancellation** — deleting a forum topic from Telegram does not terminate the underlying Claude session

### Internal

- **10 Architecture Decision Records (ADRs)** documenting key design choices (binary distribution, rate limiting, PID locking, socket security, token scrubbing, session validation, migration gap audit, release readiness audit, broken windows elimination, deep release readiness evaluation)
- **SECURITY.md** with a full threat model covering all attack surfaces
- **CI pipeline updated to Rust-only** — `cargo check`, `clippy`, `fmt`, and `cargo test` replace the TypeScript build/lint/test steps
- **Release workflow** — GitHub Actions builds binaries for 4 platforms (`linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`) and publishes scoped npm packages alongside the root package
- **Binary distribution via scoped npm packages** — platform-specific packages (e.g., `@claude-telegram-mirror/linux-x64`) are installed as optional dependencies; the root package selects the correct one at install time

## [0.1.20] - 2025-12-09

### Fixed
- **BUG-012: Project hook installs missing PreToolUse** - `ctm install-hooks -p` now installs PreToolUse and PostToolUse hooks
  - Root cause: Installer intentionally skipped these for project installs, assuming global hooks would handle them
  - Problem: Claude Code's project hooks override global hooks (they don't merge)
  - If a project has its own PreToolUse hooks (e.g., claude-flow), the global telegram hooks never run
  - Fix: Project installs now include all hook types, same as global installs
  - After upgrading, run `ctm install-hooks -p` in affected projects to add the missing hooks

## [0.1.19] - 2025-12-09

### Fixed
- **BUG-011: Missing hostname in topic names** - Forum topics now include hostname for all sessions
  - Root cause: Bash hook script (`telegram-hook.sh`) didn't include hostname in metadata
  - Node handler (`handler.ts`) included hostname but bash hook handled most events
  - Fix: `get_tmux_info()` in bash hook now includes hostname in returned JSON
  - New sessions will have hostname in topic name (e.g., "agidreams | project-name")
  - Existing sessions need to be closed and recreated to get hostname in topic name

## [0.1.18] - 2025-12-09

### Fixed
- **BUG-010: Topic creation on clean install** - Forum topics are now created correctly on fresh installations
  - Root cause: BUG-006 removed `session_start` emission from hooks, but daemon still waited for it to create topics
  - Fix: `ensureSessionExists()` now calls `handleSessionStart()` directly instead of waiting
  - Topics are created immediately when the first hook event arrives for a new session
  - Race condition safety preserved: Promise-based locking prevents duplicate topics when concurrent events arrive
  - Verified no regressions to BUG-001 through BUG-009 fixes

## [0.1.17] - 2025-12-09

### Fixed
- **BUG-009: Reactivate ended sessions on new hook events** - Sessions marked as 'ended' are now automatically reactivated when new hook events arrive
  - Fixes issue where Telegram → CLI input silently failed after session was incorrectly marked ended
  - Added `reactivateSession()` method to SessionManager
  - `ensureSessionExists()` now checks session status and reactivates if needed

## [0.1.16] - 2025-12-09

### Added
- **FEAT-001: CLI lifecycle commands** - New `ctm stop` and `ctm restart` commands
  - `ctm stop` - Gracefully stop the running daemon (sends SIGTERM, waits up to 5s)
  - `ctm stop --force` - Force kill if graceful shutdown fails
  - `ctm restart` - Stop and restart the daemon in one command
  - Commands auto-detect if running as OS service and delegate appropriately
  - Cleans up stale PID and socket files automatically

- **Enhanced `ctm status` command** - Now shows daemon running state
  - Shows PID when daemon is running directly
  - Shows "(via system service)" when running under systemd/launchd
  - Shows socket file status
  - Detects stale PID files

### Changed
- `isServiceInstalled()` function exported from service manager for CLI use
- README.md updated with complete CLI command documentation

## [0.1.15] - 2025-12-09

### Fixed
- **BUG-005: Ignore General topic messages** - Messages in the forum's General topic are now ignored
  - Only messages in specific forum topics (with threadId) are routed to Claude sessions
  - Daemon can still write to General topic (startup/shutdown notifications)
  - Prevents confusion when user accidentally posts in General instead of session topic

- **BUG-006: Remove file-based session tracking** - Daemon SQLite is now single source of truth
  - Removed `.session_active_*` file tracking from both bash hook and Node handler
  - Hooks are now stateless - they just forward events to daemon
  - Eliminates inconsistency between bash (kept tracking on Stop) and Node (cleared on Stop)
  - Daemon's `ensureSessionExists()` handles all session creation via SQLite

## [0.1.14] - 2025-12-09

### Fixed
- **BUG-003: Stale session cleanup** - Sessions with dead tmux panes are now automatically cleaned up
  - New `staleSessionTimeoutHours` config (default 72 hours, configurable via env or config file)
  - Cleanup only triggers when: `lastActivity > 72h` AND (pane gone OR pane reassigned to another session)
  - Sends "Session ended (terminal closed)" message before closing forum topic
  - Prevents stale "active" sessions from accumulating indefinitely

- **BUG-004: Stop command sends wrong key** - Fixed interrupt behavior for Claude Code
  - `sendKey` method now includes `-S socket` flag for correct tmux server targeting
  - **Interrupt commands** (`stop`, `cancel`, `abort`, `esc`, `escape`) now send **Escape** to pause Claude
  - **Kill commands** (`kill`, `exit`, `quit`, `ctrl+c`, `ctrl-c`, `^c`) send **Ctrl-C** to exit Claude entirely
  - All commands work with or without leading `/` (e.g., `stop` or `/stop`)

### Added
- `TELEGRAM_STALE_SESSION_TIMEOUT_HOURS` environment variable for configuring stale session cleanup
- New kill command category for exiting Claude entirely (vs just interrupting)

## [0.1.13] - 2025-12-08

### Fixed
- **BUG-002: Race condition in topic creation** - Messages no longer leak to General topic when events arrive out-of-order
  - Added promise-based topic lock with 5-second timeout
  - All handlers now await topic creation before sending messages
  - Explicit failure (error log + drop message) on timeout instead of silent misdirection

- **Closed topic auto-reopen** - Bot automatically reopens topics closed by user in Telegram
  - Detects `TOPIC_CLOSED` error and calls `reopenForumTopic()`
  - Sends "Topic reopened" notification after recovery
  - Retries original message after successful reopen

- **PreToolUse regression: Missing tool details** - Restored detailed tool call information in Telegram
  - PreToolUse now runs BOTH bash script (tool details) AND Node.js handler (approvals) in parallel
  - Safe tools (ls, cat, pwd, etc.) now appear in Telegram - they were silently skipped before
  - Rich expandable context restored for all tool invocations

### Changed
- **Smart hook installer** - Auto-fixes configuration without `--force` flag
  - Compares existing CTM hooks with expected configuration
  - Only updates hooks that need changes, preserves user's other hooks
  - Reports what changed: `added`, `updated`, or `unchanged`
  - Removed `--force` option (no longer needed)

## [0.1.11] - 2025-12-08

### Fixed
- **Respect bypass permissions mode** - Skip Telegram approval prompts when Claude Code is in `bypassPermissions` mode
- Deployed with bypass fix included (0.1.10 was missing the fix)

## [0.1.9] - 2025-12-08

### Fixed
- **Critical: Telegram approval buttons now work correctly**
  - Fixed hook event type mismatch: Claude Code sends `hook_event_name` but handler was checking `type`
  - PreToolUse hooks now properly send `approval_request` messages to daemon
  - Approval buttons (Approve/Reject/Abort) now appear in Telegram for dangerous operations

- **Fixed message update after approval**
  - Changed to plain text mode to avoid Markdown parsing conflicts
  - Message now correctly updates to show decision after clicking approval button

### Changed
- Updated `types.ts` to use `hook_event_name` instead of `type` to match Claude Code's actual JSON format
- Added fallback timestamps for hook events where timestamp is optional
- Added additional Claude Code fields to hook types: `transcript_path`, `cwd`, `permission_mode`

## [0.1.8] - 2025-12-07

### Added
- Initial release with Telegram approval buttons feature
- Bidirectional Claude Code ↔ Telegram integration
- Session mirroring with forum topics
- Tool execution notifications
- Input injection from Telegram to CLI
