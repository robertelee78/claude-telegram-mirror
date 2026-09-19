# ADR-016: Multi-Host Agent Abstraction (Claude Code, Codex, OpenCode)

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Implemented (2026-09-19) on `feat/adr-016-multi-host` — both host observers built, unit-tested against captured wire samples, and end-to-end tested against the real binaries; see Implementation log. Amended the same day with §Default enablement (0.2.32): both hosts are on by default with nothing to configure.
**Date:** 2026-09-19
**Authors:** Robert, Claude
**Tags:** multi-host, codex, opencode, host-abstraction, app-server, acp, supersedes-part-of-adr-004

## Context

### The invariant this ADR must not break

ADR-015 states it plainly, in the operator's own words: a question must render for
**both** the CLI and Telegram, and be answerable from **either**. "Anything short
of this is a violation of what ctm is meant to be."

ADR-014 PR-E violated it by accident. It replaced keystroke injection with a
structured `hookSpecificOutput.updatedInput` return — structurally cleaner in
every respect except that `updatedInput` tells Claude Code "already answered,
don't render," which suppressed both the native widget and the `tool_start` event
driving the Telegram render. A feature that worked on both surfaces became
Telegram-only. It shipped in 0.2.18 and was reverted in 0.2.20, three days later.

**That failure is the acceptance test for this ADR.** Any host integration must be
graded against it before implementation, not after.

### Where the current host coupling lives

ctm is already better factored for this than it looks. `MessageType` /
`BridgeMessage` (`types.rs:152-207`) is an internal, host-neutral wire protocol,
deliberately distinct from Claude Code's hook JSON, with `#[serde(other)] →
Unknown` (`types.rs:171`) providing forward compatibility. Everything downstream
of it — `socket.rs`, `session.rs`, `bot/*`, `formatting.rs`, `summarize.rs`,
`service/*`, `prune.rs`, `liveness.rs`, and all of `daemon/*` except
`callback_handlers.rs` — is reusable unchanged (~16.8k LOC, ~74%).

Coupled to Claude Code specifically, and therefore per-host:

1. The 8-event hook taxonomy and field names (`types.rs:4-40`).
2. `~/.claude/settings.json` registration across 3 scopes, deduped only by exact
   command string (`installer.rs:40-82`).
3. The `permissionDecision` / `updatedInput` return contract (`hook.rs:539-651`).
4. **`daemon/callback_handlers.rs` (2,595 lines)** — the AskUserQuestion engine,
   which recovers question structure *from pixels*: `LABEL_*` constants at
   `:23-28`, tab-row detection at `:123-128`, `❯`/`☒`/`☐` classification at
   `:61`/`:97`/`:138`, and the `InjectOutcome::{Success,FailedClean,FailedDirty}`
   recovery contract at `:54`. Derived by binary string-mining the Claude Code
   2.1.159 bundle and capturing live TUI frames; version-pinned by construction.
5. `CLAUDE_CODE_HEADLESS`, the `~/.claude/projects/.../subagents/agent-*.jsonl`
   transcript layout, and sub-agent hooks sharing the *parent's* `session_id`
   (`types.rs:570-609`).

### What the spikes established (executed, not inferred)

| Finding | Host | Grade |
|---|---|---|
| API-injected message renders in the attached interactive TUI | Codex | **High** — `thread/queue/add` from a separate process rendered as a user message; agent replied visibly; **a pre-existing unsent composer draft survived untouched** |
| API-injected message renders in the attached full TUI | OpenCode | **High** — verified under PTY: `/session/{id}/message`, `/tui/append-prompt`, `/tui/show-toast` all rendered |
| `attach --mini` renders none of the above | OpenCode | **High** — full TUI is mandatory |
| A `codex` started with **no** app-server daemon runs in-process and is invisible | Codex | **High** — live session absent from `thread/loaded/list`; no socket until `daemon start`. A `codex --remote unix://…` TUI registers, and — later spike, §Default enablement — so does a **bare `codex` started while the daemon is running** |
| Structured multiple-choice exists as typed JSON | both | **High** — Codex `item/tool/requestUserInput`; OpenCode `question` tool (confirmed in `/experimental/tool/ids`) |
| `permissionDecision: "ask"` is rejected by Codex | Codex | **High** — binary: `PreToolUse hook returned unsupported permissionDecision:ask` |
| Multi-client resolution is a first-class protocol message | Codex | **High** (schema) — `ServerRequestResolvedNotification {requestId, threadId}` |
| Permission replies are three-valued, no server timeout | OpenCode | **High** (spec) — `once｜always｜reject` |
| Per-session event replay (`?after=`) does not work | OpenCode | **High** — emitted zero bytes in every configuration; `durable.seq` is a gap detector only |
| Local server is unauthenticated by default while exposing `/pty` and `/session/{id}/shell` | OpenCode | **High** — verified auth matrix; `OPENCODE_SERVER_PASSWORD` works and covers SSE |

### The counterintuitive finding

**On both hosts the cheapest path is the wrong path.** Codex's hook system is
near-byte-compatible with Claude Code's — 12 events vs 8, same wire format, same
`{matcher, hooks:[{type, command, timeout}]}` registration shape — so `hook.rs`
ports ~85% of its code and "approvals on Codex" looks like a few hundred lines.

But a blocking hook runs *before* the TUI renders the prompt. For the duration of
ctm's 300-second approval wait (`hook.rs:605-610`) the terminal operator sees a
stalled agent with nothing to answer, and when ctm replies the prompt never
appears at all. That is PR-E's single-surface failure reached by a different
mechanism: PR-E suppressed the render by *mutating input*; a blocking hook
suppresses it by *pre-empting*. Codex then removes the escape hatch, because
`permissionDecision: "ask"` — ctm's hand-back-to-CLI fallback — is rejected.

The programmatic path (Codex app-server, OpenCode HTTP) costs more and is
correct, because on both hosts **the TUI is itself a client of the same bus**.
ctm becomes a peer alongside the operator rather than something writing into the
agent's decision. PR-E was mutation; this is membership.

## Options considered

**A. Hook-only, both hosts.** Cheapest (~600-800 LOC for Codex). Rejected for
anything that writes: reproduces PR-E. Retained for *read-only* mirroring, where
nothing blocks and nothing is suppressed — and uniquely valuable on Codex because
hooks work on any session, including non-daemon ones.

**B. Programmatic API per host.** Codex app-server JSON-RPC over
WebSocket-on-Unix-socket; OpenCode HTTP + SSE. Retires the TUI scraper entirely
on both. Costs a daemon/server dependency. **Recommended, pending the dismissal
spike.**

**C. Target ACP (Agent Client Protocol) instead of bespoke APIs.** Attractive for
breadth — 25-60+ implementers. **Rejected for ctm-as-mirror, on structure:** ACP
is stdio JSON-RPC where the editor spawns the agent as a subprocess, one client
per process (an HTTP transport is proposal-stage only). ctm would *be* the editor
holding the only pipe, so there would be no operator-driven TUI to mirror — the
same Telegram-only end state PR-E reached by accident, arrived at by architecture.
ACP also has **no structured multiple-choice primitive**; permissions are its only
choice-presenting mechanism, so adopting it would cost ctm the feature it is
distinctive for. Legitimate as a *separately-named* headless mode where no CLI
exists to mirror (the invariant is then vacuously satisfied, not violated), but
not as the path for this ADR.

**D. JS shim plugin for OpenCode** forwarding to ctm's Unix socket. Rejected as
the default: ctm ships a single static binary, and a Node/Bun runtime dependency
adds a second install surface to `installer.rs`. Revisit only if ctm ever needs to
*block* a tool call before it runs (`tool.execute.before` can mutate args; the
HTTP API cannot).

## Reformulated hypothesis (Kata step 3, on spike data)

The original hypothesis — *the programmatic API preserves the both-surfaces
invariant* — is **confirmed on both hosts by execution**, with preconditions and
operating rules the schema never revealed. These are the reformulation:

**Codex (0.155.1, app-server 0.153.2):**
- A second client is **blind and mute by default**. Seeing a thread in
  `thread/loaded/list`, even initiating its turn, yields only
  `thread/status/changed {activeFlags:["waitingOnApproval"]}` — no request, no
  `requestId`, nothing answerable. **`thread/resume {threadId}` is the subscribe
  call.** After it, both surfaces render the same prompt simultaneously.
- A `codex` started with no app-server daemon never joins one; a bare `codex`
  started while the daemon runs auto-joins it (§Default enablement). Native
  observation therefore requires the daemon to be up first — which ctm ensures.
- `item/tool/requestUserInput` fires **only in plan collaboration mode**
  (`default_mode_request_user_input` is an under-development flag). Codex's
  `HostCaps.structured_questions` is therefore *native, plan-mode only*.
- Decisions are **fire-and-forget**. The losing side of a race gets *no*
  response — no error, no ack, zero frames. ctm dismisses its keyboard on
  `serverRequest/resolved`, never on its own send succeeding.
- `thread/resume` mid-turn **re-delivers pending server requests** to the late
  joiner. Crash recovery is a protocol property, not something ctm engineers.
- `turn/interrupt {threadId, turnId}` cleanly dismisses a pending approval from
  RPC without subscribing — ctm's abort button.
- **Attribution gap:** a local answer narrates in the TUI (`✔ You approved…`,
  `answer: circle`); a remote answer makes the widget vanish with no trace.
  Confirmed for both approvals and questions.
- `thread/queue/add` requires `capabilities:{experimentalApi:true}` at
  `initialize`; the `thread/*`/`turn/*`/`item/*` core is stable (four
  first-party surfaces run on it). Generate the schema at build time and fail on
  drift.

**OpenCode (1.18.31):**
- Permission and question events fire **only on the legacy global `/event`
  stream**, never `/api/event`. `/event` is the runtime superset. The v2 engine
  (`/api/session`) is unusable in this build: wrong default model, provider
  500s, empty message lists. **Subscribe to `/event` only.** (This corrects the
  pre-spike research, which pointed the observer at `/api/event`.)
- Full TUI required; `attach --mini` renders nothing.
- No port discovery exists; default `--port 0` is random. ctm requires an
  explicit port and `OPENCODE_SERVER_PASSWORD` (the server is otherwise
  unauthenticated while exposing `/pty` and `/session/{id}/shell`).
- Race resolution is **exactly-once**; the loser gets a deterministic
  `404 {_tag: PermissionNotFoundError|QuestionNotFoundError, requestID}`.
- Question answers are **not validated** against options — free-form text passes
  through to the widget's "Type your own answer" path.
- The TUI is a peer subscriber, not a sink: every `*.asked` reached `/event`
  while the TUI displayed it, and operator answers are broadcast as
  `*.replied`, so ctm learns when the terminal answered.
- `PATCH /session/{id}` accepts a per-session `permission` ruleset — a product
  feature (gate tools while ctm is attached), with guardrails: opt-in, per-tool,
  reverted on detach, and `always` surfaced since it writes a durable rule.
- Rejected tool calls render unlabeled in the TUI **whether rejected locally or
  via API** — symmetric, OpenCode's own behaviour, compensated with
  `POST /tui/show-toast`.

**Both hosts:** the attribution gap is real and must be compensated by ctm, not
assumed away. The predicted first defect — a stale Telegram keyboard answering an
already-resolved request — is harmless on both (silently dropped / 404) but must
be rendered as "answered at terminal," driven by the host's resolution signal.

## Decision

**Option B, in the socket-client shape.** Each non-Claude host gets an
**observer** that is a client of ctm's own Unix socket, speaking `BridgeMessage`
— exactly as `ctm hook` does today, but long-lived. This is the load-bearing
choice, because it means:

1. **The approval pipeline is reused unchanged.** The observer sends
   `ApprovalRequest` and blocks for `ApprovalResponse` on its own connection,
   correlated by `session_id`, precisely as `hook.rs::send_and_wait` does. The
   daemon's `pending_approval_clients` routing, Critical-priority queueing,
   audit edits and expiry all apply without modification.
2. **The Telegram question UI is reused unchanged.** The observer emits
   `ToolStart{tool:"AskUserQuestion", input:{questions:[…]}}` in Claude's exact
   shape; the daemon renders tentative-select + Submit-All (ADR-012/015) as it
   does today. Only the final delivery dispatches on host.
3. **The daemon stays host-blind except at three dispatch points:** where it
   injects user text, where it delivers a Submit-All answer, and where it
   records a session's host. Everything else in `daemon/*` is untouched.

Concretely:

- `types.rs`: `HostKind {ClaudeCode, OpenCode, Codex}`; two new `MessageType`
  variants for the daemon→observer direction, `QuestionResponse` and
  `HostInject`; `HostKind` and `hostSessionId` in metadata.
- `session.rs`: `host_kind TEXT` column on `sessions` (migration mirrors
  `custom_title`), set at insert from `SessionStart` metadata — never inferred.
- `daemon/*`: a `deliver_input()` seam replacing direct `inj.inject()` calls,
  dispatching on `host_kind`; Submit-All dispatches likewise; a
  `session_host_clients` map (session → observer client id) mirroring
  `pending_approval_clients`.
- `src/host/mod.rs`: `HostKind`, `HostCaps`, and the observer runner contract.
- `src/host/opencode/`: SSE client on `/event`, event → `BridgeMessage` mapping,
  reply/inject calls, toast compensation.
- `src/host/codex/`: WebSocket-over-Unix JSON-RPC, `initialize` with
  `experimentalApi`, `thread/resume` subscription, request → `BridgeMessage`
  mapping, fire-and-forget responder keyed on `serverRequest/resolved`.
- Observers run as tokio tasks inside the daemon (no new service units) and
  connect to the daemon's own socket. `HostCaps` lets the Telegram layer degrade
  honestly: `structured_questions: ViaTuiScrape | Native | NativePlanModeOnly`.
- `doctor.rs`, `setup.rs`, `config.rs`: host selection and per-host checks
  (reachability, auth set, `--mini` warning, daemon-mode warning, version range).
- `summarize.rs`: tool-name normalisation table at the observer boundary, so the
  summarizer stays host-blind.

**Explicitly rejected in this decision:** answering approvals or questions over
either host's *hook* system (reproduces PR-E, and Codex rejects `"ask"`);
targeting ACP (no operator TUI to mirror, no multi-choice primitive); a JS shim
plugin (second install surface).

**Sequencing:** OpenCode first (no adoption blocker, ~half the effort, every
spike executed), then Codex.

## Open questions

Closed by execution: TUI render on both hosts; permission dismissal on both;
question dismissal on both; double-answer race on both; event starvation
(OpenCode: none); daemon-attached visibility (Codex: yes).

Still open, none blocking:
1. Codex: are `turn/start`/`turn/steer` behind `experimentalApi`? Only
   `thread/queue/add` was observed gated. Affects the mitigation, not the design.
2. Codex: is there any RPC-side mechanism to narrate a remote decision in the
   TUI (`thread/inject_items` is the candidate)? Determines whether the
   attribution gap is compensated or documented.
3. Codex: does a 300s `preToolUse` hook survive per-event timeout clamping?
   Moot for approvals (app-server path), relevant only to observability hooks.
4. Both: server-side approval timeout. None found; ctm keeps its own 300s timer
   and on expiry *stops tracking* rather than deciding — the prompt stays live
   in the TUI.

## Consequences

- **Supersedes part of ADR-004.** tmux `send-keys` stops being *the sole*
  injection method and becomes the Claude-Code-specific one. ADR-004 will be
  amended in the same commit as the first non-Claude injector, not before.
- **Amends ADR-005.** `sessions` gains `host_kind`, set at insert time so
  cross-host misrouting is impossible by construction rather than by luck.
- **`callback_handlers.rs` stays Claude-only.** It has no counterpart on either
  new host and must not grow one.
- **One daemon, two observers.** ADR-005's binding constraint is *one bot per
  daemon*, not single-writer SQLite — one daemon remains the sole writer however
  many hosts feed it, and `socket.rs` is already many-producer/one-consumer.
  Two daemons would need two bot tokens and would contend on the same DB.
- **New capability, Codex only:** `UserInput` has an `image` variant, so
  `MessageType::SendImage` gains an inbound direction that tmux injection cannot
  provide for Claude Code.
- **Honest degradation, not guessing.** A `HostCaps` descriptor lets the Telegram
  layer ask what a host can actually do (`structured_questions`, `steer`,
  `always_decision`) instead of assuming. This is the structural guard against a
  future PR-E.

## Implementation log

**2026-09-19 — landed on `feat/adr-016-multi-host`.**

Shared seam:
- `types.rs`: `HostKind`; `MessageType::{QuestionResponse, HostInject}`; metadata
  accessors `host_kind()`, `host_session_id()`, `question_id()`, `answers()`, `action()`.
- `session.rs`: `host_kind` column + migration; `set_host_kind()` writes it in the same
  `db_op` closure as `create_session` (insert-time, never inferred).
- `daemon/host_dispatch.rs` (new): the only host-aware code in the daemon —
  `session_host_kind`, `record_session_host`, `send_to_host_observer`, `host_inject`,
  `host_answer_question`, and `handle_native_host_text` (the native replacement for the
  tmux text path, same user-facing behaviour).
- `daemon/*`: dispatch on `host_kind` at the text-inject, file-inject, `/rename`,
  `/abort`, abort-callback and Submit-All sites; `handle_approval_resolved_elsewhere`
  retires the Telegram keyboard when the operator answers at the terminal.
- `config.rs`: `hosts: {opencode: {enabled, baseUrl?, passwordEnv, password?}, codex:
  {enabled, socketPath, binary?}}` — both `enabled` by default (§Default enablement);
  `CTM_OPENCODE_URL` opts an external server into HTTP observation, `CTM_CODEX_SOCKET`
  overrides the control socket, `CTM_*_ENABLED=0` opts out.
- `doctor.rs`: check 12/12 "Hosts" — reachability, auth (hard failure when the OpenCode
  password is unset), socket presence/mode, and `HostCaps` reported per host.

Observers (`src/host/`):
- `link.rs`: `ObserverLink` (socket client), `ApprovalFifo`, `Backoff`, `stamped()`.
- `opencode.rs`: SSE on legacy `/event` with Basic auth; pure `Translator`; replies via
  `/permission/{id}/reply`, `/question/{id}/reply`, `/session/{id}/prompt_async`,
  `/session/{id}/abort`, `PATCH /session/{id}`; toast compensation for the
  attribution gap.
- `codex.rs`: WebSocket-over-Unix JSON-RPC via `tokio-tungstenite`; `initialize`
  without `experimentalApi`; `thread/loaded/list` + `thread/resume {excludeTurns}`
  with retry on `turn/started`; ghost-thread filter; approvals/questions as JSON-RPC
  responses, dismissed on `serverRequest/resolved`; `turn/start`/`turn/steer` by
  thread state; `turn/interrupt`; `thread/name/set`; `item.status` trusted over the
  TUI verb.

Evidence (Kata step 6):
- 730 tests pass (unit 185 → 214; 29 host tests against verbatim spike samples).
- `tests/host_e2e.rs` (`--ignored`): real `opencode serve` and real
  `codex app-server daemon` each mirror a session-create/end through the observer to a
  stand-in daemon socket, zero model spend.
- CI gate green: `cargo check`, `clippy -D warnings`, `fmt --check`, `cargo test`.

Deferred, deliberately (each is a follow-up, none blocks the invariant):
- An "Allow always" button (both hosts support it; ctm's keyboard has approve/reject/abort).
- Per-session permission ruleset as an opt-in product feature on OpenCode
  (`PATCH /session/{id}` `permission`), reverted on detach.
- `setup.rs` wizard step for host selection — made moot by §Default enablement.
- The two pre-existing `clippy --all-targets` nits (`queue.rs:575`, `env.rs:125`) are
  untouched — out of scope.

## Default enablement (amendment, 2026-09-19 — shipped in 0.2.32)

The operator's requirement, verbatim: *"A user does not have to manually ctm enable
codex or opencode — they should be enabled by default. The only part that has to be
figured out is the configuration."* The first cut of this ADR made both hosts opt-in
(`hosts.opencode.baseUrl` + a password, `hosts.codex` + `codex --remote …`). That put
the wiring on the user. This amendment moves it into ctm.

### Spikes (executed against OpenCode 1.18.31 and Codex 0.155.1)

| # | Question | Result |
|---|---|---|
| 1 | Does a bare `opencode` (no `--port`) expose anything? | **No listener at all.** `--port` is the only way to get one; `server.port` in `opencode.json` applies to `serve`/`web` only. The HTTP observer alone can never see a bare TUI. |
| 2 | Do plugins load in the bare TUI, and can they reach the API? | **Yes.** `~/.config/opencode/plugins/*.js` (honouring `XDG_CONFIG_HOME`) loads in every process; the plugin receives an in-process `client` whose generic `_client.request({method,url,query,body})` works with no listener — but only after init returns (`setTimeout(…, 0)`); awaiting it inside init deadlocks the TUI. |
| 3 | Which event feed carries `permission.asked`/`question.asked` in-process? | The plugin **`event` hook** — the full bus superset, identical wire shapes to legacy `/event`. `client.event.subscribe()` yields nothing in-process. |
| 4 | Does a reply through the in-process client dismiss the TUI prompt? | **Yes** — `POST /permission/{id}/reply {reply:"once"}` via the pipe: prompt cleared, `permission.replied` broadcast, command ran. Same exactly-once semantics as HTTP. |
| 5 | `opencode --port` loads the plugin twice — duplicate events? | Twice in one pid, but **only one instance receives events** (137 vs 0); the other pipe idles. Per-connection routing makes this harmless. |
| 6 | Any startup blind spot? | **The first API request an instance serves is invisible to plugin hooks**, whatever it is and whenever it comes. A TUI issues many before the user's first prompt, and a used session announces lazily on its next event; the e2e test warms the instance with a list call. |
| 7 | Does a bare `codex` join a running app-server daemon? | **Yes** — its thread appeared in `thread/started` on ctm's connection with no flags. Keeping the daemon alive *is* enabling Codex. |
| 8 | Is `codex app-server daemon start` safe to run repeatedly, from a service? | Idempotent (`{"status":"alreadyRunning"}`, exit 0) and reports `socketPath`. The npm `codex` is a `codex.js` shim needing `node`, which launchd's PATH lacks; both `~/.codex/packages/standalone/current/bin/codex` and the npm package's `vendor/<triple>/bin/codex` are native and run without it. |

### Decision

1. **Both hosts are on by default.** `hosts.opencode.enabled` / `hosts.codex.enabled`
   default `true`; `false` (or `CTM_OPENCODE_ENABLED=0` / `CTM_CODEX_ENABLED=0`) is the
   only opt-out. There is no `ctm enable` command.
2. **OpenCode is wired by a plugin ctm provisions itself** (`host/opencode_plugin.js`,
   rendered with the pipe socket path into `<opencode config>/plugins/ctm.js`). It is a
   dumb pipe: the `event` hook forwards every bus event up a Unix socket next to the
   bridge socket (`opencode.sock`, 0600 in the 0700 config dir); the daemon sends
   `{type:"call",id,method,url,query,body}` down and the plugin executes it through the
   in-process client. The daemon end (`host/opencode_pipe.rs`) drives the unchanged pure
   `Translator`; each connection owns its own `ObserverLink`, so `host_dispatch`
   routing is untouched and several OpenCode processes coexist. When the process
   exits, its announced sessions get `SessionEnd`. The daemon writes the plugin at start
   and re-checks every 60 s (`run_keeper`), so `ctm update` rolls it forward and an
   OpenCode installed later is picked up; `ctm doctor --fix` writes it too.
3. **Codex is wired by keeping its daemon alive** (`host/codex_daemon.rs`): before each
   connect attempt the observer runs `codex app-server daemon start` through a native
   binary (`host/detect::codex_binary`: override → managed standalone → npm-bundled
   native → any other `codex`; the `.js` shim is never executed). Not installed → probe
   again in 60 s, silently.
4. **HTTP observation stays, opt-in**, for an external `opencode serve` the plugin cannot
   reach (`hosts.opencode.baseUrl` + password) — unchanged code, no longer the default.
5. **`ctm doctor` 12/13** reports detection and wiring per host and fixes both.

### Consequences

- The user story is: install ctm, run `claude`, `codex` or `opencode`. Nothing else.
- ctm now writes a file into OpenCode's config dir, exactly as it writes hooks into
  Claude Code's `settings.json`. The file is marked generated and is regenerated, not
  merged; a hand edit is overwritten within a minute while the daemon runs.
- No port, no password, no `--remote`. The bare-TUI path carries no network listener at
  all, which is strictly safer than the previous documented setup.
- `HostsConfig::enabled()` now means "not opted out", not "configured".
