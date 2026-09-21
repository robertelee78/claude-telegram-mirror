# ADR-022: `codex resume` re-applies your flags to the app-server thread before attaching

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Implemented (2026-09-22; amended the same day after review)
**Date:** 2026-09-22
**Authors:** Robert, Claude
**Tags:** codex, app-server, resume, permissions, ux
**Related:** ADR-016 (Codex host; `--remote`), ADR-021 (account follows login), `shell.rs` (`codex` function)

## Context

Reported, verbatim: "a lot of fuckery for something that used to just work". The
user's normal invocation is `csp='codex --dangerously-bypass-approvals-and-sandbox'`,
and `csp resume <id>` from a worktree used to resume the session there with full
access. Since ADR-016 every Codex session starts with `--remote`, inside the
app-server ctm keeps alive — the only way Telegram can answer its questions and
carry replies, which is what the user actually wants from the mirror. A thread that
lives in the app-server obeys rules a plain `codex` process never had:

1. `resume` over `--remote` **ignores `--cd`** and **refuses every permission flag**:
   `Error: Permission overrides are not supported when resuming a remote task.`
2. A plain remote resume **does not restore the flags the session was started with**:
   a thread started as `Full Access` came back `Custom (custom permissions, never)`;
   the Stage-1 thread came back `Workspace (never)` rooted in the directory it was
   created in, so every write into its worktree was silently denied.
3. While the app-server holds a thread, a **local** `codex resume` of it is refused
   ("This conversation is open in another app").

So the old command could not work through any path — not remote, not local.

## Spike (2026-09-22, isolated `CODEX_HOME`, real app-server 0.153/CLI 0.155)

- A session started through `--remote` with `--dangerously-bypass-approvals-and-sandbox`
  reports `Permissions: Full Access`; after a plain remote resume: `Custom (custom
  permissions, never)`.
- `thread/settings/update` (requires the `experimentalApi` capability at `initialize`)
  accepts `cwd`, `permissions` (a profile id: `:read-only`, `:workspace`,
  `:danger-full-access`, or a named profile the daemon can resolve from config for
  that cwd) and `approvalPolicy`. After `{permissions: ":danger-full-access",
  approvalPolicy: "never"}` the same remote resume reports **Full Access**; after
  `{cwd: <worktree>}` `thread/read` and `/status` report the worktree.
- A named profile is resolved from the directory's own `.codex/config.toml`
  (`default_permissions` + `[permissions.<name>]`, top-level key before any table);
  `permissionProfile/list {cwd}` lists it. Ad-hoc `-c permissions…` overrides cannot
  be applied to an app-server thread at all (the validator wants a config table).
- There is no RPC to unload a thread from the app-server.

## Review (2026-09-22, Codex and GLM-5.3, independently)

Both would have blocked the first implementation. The findings that changed it:

- **Attaching "as-is" after a failed apply** ran the thread under its *previous*
  settings with the typed flags already stripped — for `resume <id> -s read-only` on
  a Full-Access thread, wider than typed. → A failed or unverified apply is exit 1.
- **`--last` resolved twice** (ctm picked a thread by `thread/list`, then codex picked
  its own) with no directory scoping → settings on the wrong thread. → Resolved
  once, with codex's own scoping, and the selector replaced by the exact id.
- **Help mutated the thread** (`csp resume <id> --help` applied Full Access, then
  printed help); invalid `-a` values were applied blind; `-C ../wt` was sent
  relative; `--model=o3` was forwarded as `--model=o3 o3`; `-sread-only`, `--yolo`,
  `--`, session names were unhandled; the value-flag list was frozen. → Pure parser
  with codex's own `--help` as the source of value flags; every rule unit-tested.
- **No attachment guard**: settings applied underneath a running turn or another
  terminal (hot-apply is instant on an attached TUI — spike). → Refused while a turn
  is in progress or ctm's store shows the session live elsewhere.
- **Exit reported by directory** could end another session (the store keeps the
  directory a thread was *created* in; a worktree resume never matched it), and a
  SIGINT killed the launcher before it could report. → Reported by thread id; the
  launcher absorbs INT/HUP and returns codex's status (128+signal on a signal).
- `experimentalApi` was requested on every ctm connection → only the launcher's.
- The shell and the launcher had different ideas of "is there a daemon" → the shell
  no longer checks; the launcher decides against ctm's configured socket.
- The help probe appended `--help` after `--` → only the words before `--` are probed.
- `fork` was "passed through" on an untested claim → spiked (below) and routed.
- ADR-021's preflight swallowed a failed restart and treated an unreadable session
  store as "no live session" → `Reconciled::Failed` is printed; unknown liveness never
  restarts.

## Spike 2 (2026-09-22, isolated daemon, app-server 0.155.1) — what the runner relies on

- `thread/read` finds a persisted-but-unloaded thread (`status.type: notLoaded`);
  it errors only for an unknown id. `status.type` is `active` (with `activeFlags`
  `waitingOnApproval` / `waitingOnUserInput`), `idle`, or `notLoaded`.
- **`thread/settings/update` on an unloaded thread fails: "thread not found".** After
  an ADR-021 restart every thread is unloaded, so the launcher loads first:
  `thread/resume {threadId, excludeTurns: true}` loads (or rejoins) and returns the
  effective `cwd`, `sandbox.type`, `approvalPolicy` — the values `/status` shows.
  A second `thread/resume` after the update is the verification.
- A thread with no turns is not listed by `thread/list` (nor by codex's picker).
- `thread/list` items carry the original `cwd` and, for a moved thread, the current
  one in `environments[0].cwd`; `--last` matches either.
- **Local `codex fork <id>` of a daemon-held thread works** (chooser: session dir vs
  current) but runs unmirrored. **Remote `fork` refuses permission flags** exactly as
  resume ("Permission overrides are not supported when forking a remote task") and
  inherits the parent's applied settings. `thread/fork {threadId, cwd, sandbox,
  approvalPolicy, excludeTurns}` creates the fork with the typed settings and returns
  them; the TUI then attaches with `resume <new id>` and still prints "Thread forked
  from …".

## Decision

1. **`ctm codex-launch <codex args…>`** is the one launcher. The `codex` shell
   function sends it the bare TUI usage, `codex resume …` and `codex fork …`
   (probe rule from 0.2.51, on the words before `--`); every other subcommand and an
   explicit `--remote` run exactly as typed. Whether there is an app-server to attach
   to is the launcher's decision, against ctm's configured socket; without one it
   runs codex locally and says so in one line. `CTM_CODEX_REMOTE=0` opts out.
2. **`plan()` is pure** (`host/codex_launch.rs`): `--help`/`--version` anywhere →
   verbatim, no side effects; `--` ends options; for `resume`/`fork` the flags become
   settings — `--dangerously-bypass-approvals-and-sandbox`/`--yolo` →
   `:danger-full-access` + `never`; `-s/--sandbox` → the built-in profile;
   `-a/--ask-for-approval` → the policy (validated); `-C/--cd` (resolved against the
   shell's directory, must exist), else that directory → `cwd`;
   `-c default_permissions=<name>` → the named profile. Value-taking flags come from
   codex's own `--help`. What cannot be applied (`--add-dir`, `-c permissions*`,
   `-c sandbox*`, `-c approval_policy`, a missing value, a bad directory) is refused
   with codex's exit 2 and a line naming `<cwd>/.codex/config.toml` as the place.
   New sessions are left exactly as typed. `-p/--profile` is forwarded untouched: it
   is a *config* profile, not a permission profile.
3. **The runner fails closed** (`host/codex_launch_run.rs`): read → guard → load →
   apply → verify → attach, as in Spike 2. It refuses (exit 1, one line) when the
   thread is running a turn or waiting on an approval/answer, when ctm's store shows
   it live in another terminal (escape hatch if that terminal is gone:
   `ctm codex-exited --thread <id>`; attach as-is: `codex --remote unix://… resume
   <id>`), when a named profile is not in `permissionProfile/list {cwd}`, when the
   update fails, or when the daemon's report after the update differs from what was
   asked. `--last` and a name resolve through `thread/list` to one exact id; a
   `resume`/`fork` with no session named uses ctm's own numbered picker (codex's
   picker runs after any settings could be applied), or a listing when stdin is not
   a terminal.
4. **It reports what it did** — `ctm: session <id8>: directory …, Full Access,
   approval never  (was: …)` — and the exit **by thread id** (`ctm codex-exited
   --thread`), after absorbing INT/HUP so the report always runs; exit status is
   codex's.
5. **`fork` goes through `thread/fork`** with the typed settings; codex attaches to
   the new thread with `resume`. The parent is read, never modified.
6. The launcher runs the ADR-021 preflight first; only its connection requests
   `experimentalApi`.

## Consequences

- `csp resume <id>` and `csp fork <id>` from a worktree run there with full access,
  attached to the app-server, with Telegram questions and replies working — proven
  by `codex_resume_and_fork_reapply_the_typed_flags_before_attaching` (`--ignored`,
  isolated daemon + real TUI: unloaded thread, `/status`, exit report by id).
- Threads carry whatever settings were last applied; the launcher re-applies on
  every resume, so a session started one way and resumed another follows the later
  command. Recording the settings a session was *started* with and re-applying them
  on a flagless resume (both reviewers' suggestion) is the natural next step; it
  would complement, not replace, this translation.
- A stale "live" row in ctm's store (the daemon was down when a TUI quit) blocks a
  resume of that thread until `ctm codex-exited --thread <id>`; the alternative —
  hot-applying under a terminal ctm cannot see — is the failure both reviews ranked
  first.
- The wrapper's "explicit `-C` passes through" rule is gone: `-C` is a directory,
  not a request to leave the app-server; only an explicit `--remote` is.
