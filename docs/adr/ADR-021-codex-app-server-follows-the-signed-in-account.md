# ADR-021: The Codex app-server ctm keeps alive must follow the signed-in account

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Implemented (2026-09-21)
**Date:** 2026-09-21
**Authors:** Robert, Claude
**Tags:** codex, auth, app-server, host
**Related:** ADR-016 (Codex host; the app-server keeper), the `codex` shell function (`shell.rs`, `--remote`)

## Context

Reported: in a Codex session, ran out of quota, quit, signed in to another account
(`codex auth login --device-auth`), resumed — and was still on the old account.

Without ctm this cannot happen: every bare `codex` is a fresh process that reads
`~/.codex/auth.json` at start. With ctm it happens every time, because of two ctm
decisions from ADR-016: the `codex` shell function starts every session with
`--remote` **inside Codex's app-server daemon**, and ctm's keeper keeps that daemon
alive indefinitely (restarting it if it dies). The TUI is only a client; the model
calls — and the credentials — live in the daemon.

## Spike (Kata step 2 — 2026-09-21)

1. **The daemon caches auth and its reload is guarded by account id.** The codex
   0.155.1 binary carries the logic verbatim: `"Reloading auth"`, `"Skipping auth
   reload due to account id mismatch (expected: …"`, `"Skipping auth reload because
   no account id is available."`, `"Your access token could not be refreshed because
   you have since logged out or signed in to another account."` A running codex
   re-reads `auth.json` only for the account it already holds (token refresh); a
   *different* account on disk is deliberately ignored.
2. **Reproduced against an isolated daemon** (`CODEX_HOME` with a copy of
   `auth.json`, standalone install linked): `account/read` → account A; `auth.json`
   rewritten to account B; `account/read` → **A** after 3 s and after 13 s; daemon
   stop + start → **B**.
3. **The live daemon on this machine started 2026-09-20 00:30; `auth.json` was
   rewritten by the login at 2026-09-21 22:32** — 46 hours later. Same shape.
4. **`daemon start`/`stop` do not leak helpers** (`app-server daemon
   pid-update-loop` count constant across start/start/stop). Five orphans (ppid 1)
   dated 2026-09-19/20 were left by earlier spikes that killed daemons directly;
   removed by hand.
5. The app-server exposes `account/read` (`{account:{email,planType,type}}`),
   `account/logout`, `account/login/start` and `thread/loaded/list`. There is no
   "reload auth from disk" RPC.

**Reformulated hypothesis:** confirmed. Root cause is the combination *ctm-managed
long-lived daemon* + *Codex's account-guarded reload*; a CLI login only edits a
file that the running daemon has decided not to trust for a different account.

## Decision

1. **ctm reconciles the daemon's account with `auth.json`.** `host/codex_account.rs`
   parses the on-disk identity (`auth_mode`, the id token's `email`, `account_id` —
   parsed for display, never verified) and the daemon's (`account/read`), and when
   they differ **restarts the app-server daemon** (`daemon stop` → `daemon start`,
   verified by `account/read` afterwards), because a restart is the only way the
   daemon adopts a different account.
2. **Never underneath a live session.** If ctm's session store shows Codex sessions
   still live, the restart is deferred and the mismatch is reported: the terminal
   line at the next `codex` launch, `ctm doctor` check 12, and the daemon log. The
   deferred restart happens on the keeper's next tick once those sessions end.
3. **Two triggers, one function.** The daemon's Codex keeper reconciles every 60 s
   (so the account is already right by the time the user runs `codex`), and the
   `codex` shell function runs `ctm codex-preflight` immediately before launching a
   `--remote` session (so a login followed straight by `codex` gets the new account
   without waiting for the tick). The preflight prints one line when it restarted
   or when it could not, and never blocks `codex` from starting.
4. **`ctm doctor` reports the daemon's account** and whether it matches disk.
5. Proof is an `--ignored` e2e against a real isolated daemon
   (`codex_account_switch_restarts_the_idle_app_server`): swap the account on disk,
   reconcile, and the daemon reports the new one; with a live session recorded, it
   is deferred and the daemon is untouched.

## Consequences

- Switching accounts under ctm is: `codex login` (any form) → `codex`. The first
  `codex` after a login on an idle daemon pauses ~2 s for the restart and says so.
- Loaded-but-idle threads in the daemon are unloaded by the restart; they are
  persisted rollouts and resume normally.
- ctm reads `auth.json` for identity only; it never writes it and never logs tokens.
