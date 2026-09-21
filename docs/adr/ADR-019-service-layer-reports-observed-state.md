# ADR-019: The service layer reports observed state, never issued intent

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
**Tags:** service, systemd, launchd, reliability, testing
**Related:** `ctm-false-running-defect` (memory), ADR-017 §service restart, ADR-018 (whose 0.2.44→0.2.46 hop relies on `service restart` telling the truth)

## Context

Three shipped defects in `src/service/` share one shape:

1. **"running" for a loaded-but-dead launchd job** (`ctm-false-running-defect`):
   status was derived from the label *appearing* in `launchctl list`, not from the
   PID column. `ctm start` became a no-op that could never recover a dead service.
2. **"Service installed" on a box with no user manager** (ADR-017 follow-up, the
   Linux install): `daemon-reload` and `enable` ran with `let _ =` and the result
   said installed regardless; the next `ctm service start` failed with systemd's
   bare "Unit not found".
3. **`kickstart -k` restarted the old binary after a path change** (ADR-017,
   0.2.31): launchd never re-reads the plist; restart "succeeded" and `ctm status`
   truthfully reported the *wrong* binary running.

Each was fixed in place. The 2026-09-21 sweep found nine more `let _ = Command::new(...)`
sites in the same files (uninstall on both platforms; launchd start/restart's
`load`/`bootstrap`/`bootout`/`stop`), and — more importantly — the sites that *do*
check an exit status are not much better, because of what the spike found.

## Spike (Kata step 2 — GitHub runs 35593898424 and 35594043024, 2026-09-21)

Real `systemctl --user` (ubuntu-24.04, systemd 255) and real `launchctl` (macOS
26.6, gui/501 domain) on GitHub-hosted runners, so a CI e2e is possible on both.

**Exit status is not evidence of outcome on either manager:**

| operation | exit | what actually happened |
|---|---|---|
| `systemctl --user start` of a unit whose program exits 1 | **0** | `ActiveState=activating SubState=auto-restart`, `MainPID=0` |
| `systemctl --user restart` after the unit file changed on disk | **0** | runs the **old** `ExecStart` with a warning; `NeedDaemonReload=yes` is queryable |
| `launchctl load` of an already-loaded job | **0** | prints "Load failed: 5" |
| `launchctl unload` of an unloaded job | **0** | prints "Unload failed: 5" |
| `launchctl start` of a job whose program exits 1 | **0** | `state = spawn scheduled`, `last exit code = 1`, list shows `-  1` |
| `launchctl stop` | 0 | PID gone immediately (SIGTERM); asynchronous by contract |
| `launchctl kickstart -k` | 0 | new PID, but blocks ~10 s (throttle) |

**What *is* evidence:**

| probe | meaning |
|---|---|
| `systemctl --user show <unit> -p LoadState,ActiveState,SubState,MainPID,UnitFileState,NeedDaemonReload,Result` | `Key=Value` lines: load `loaded`/`not-found`; active `active`/`activating`/`inactive`/`failed`; a non-zero MainPID; `NeedDaemonReload=yes` after an on-disk change |
| `launchctl print gui/<uid>/<label>` | exit **113** ⇒ not loaded; otherwise `state = running`, `pid = N`, `program = /path`, `last exit code = N` |
| `launchctl bootstrap` when already loaded | exit 5 — not idempotent, so *check first* |
| `launchctl bootout` when not loaded | exit 3 |

Post-conditions that hold after a genuine success: systemd `stop` → `inactive`,
`MainPID=0`; `disable` removes the `default.target.wants` symlink; remove + reload →
`LoadState=not-found`. launchd `bootout` → `print` exits 113; MainPID/pid change
across a restart on both.

## Decision

1. **Every mutating operation is act → observe → report from the observation.**
   `ServiceResult` is computed from a probe of the manager taken *after* the
   operation (bounded wait where the manager is asynchronous), never from the exit
   status of the command issued. Command stderr is captured and attached to the
   report only when the observation says the goal was not met.

2. **Observation is a pure, tested layer.** `systemd_state.rs` parses `systemctl
   show` into `UnitState`; `launchd_state.rs` parses `launchctl print` (and
   `launchctl list`) into `JobState`. Neither shells out; the callers do, through one
   function each. "Manager unreachable" is a distinct observation, reported with the
   existing `user_manager_hint`.

3. **Goals, per operation, identical on both platforms:**
   - *install*: unit definition on disk **and** known to the manager (systemd:
     `enabled`; launchd: plist present — loading is `start`'s job).
   - *start*: **running with a stable PID** — the same non-zero PID observed twice,
     within a budget longer than the manager's restart throttle (10 s on both, so
     14 s). `activating/auto-restart`, `spawn scheduled`, or `failed` at the deadline
     is a failure, reported with the program's last exit code and where its logs are.
   - *stop*: **not running** — no PID within 10 s (launchd `stop` is asynchronous;
     systemd's is synchronous but is verified anyway).
   - *restart*: **running with a stable PID that differs from the PID before**
     (when there was one), **and the manager is running the definition on disk**:
     systemd — `daemon-reload` first if `NeedDaemonReload=yes`; launchd — if the
     loaded `program` differs from the plist's, `bootout` and verify unloaded (exit
     113), then `bootstrap` and verify loaded, then `kickstart -k`. A restart that
     leaves the old process running is a failure even though every command exited 0.
   - *uninstall*: **not loaded, not running, no unit file, no enable symlink.**
     systemd: `LoadState=not-found` after reload. launchd: `print` exits 113. If the
     manager is unreachable the files are still removed and the result is
     `success: false` with what was and was not done.

4. **`launchctl load`/`unload` are not used.** Their exit status is meaningless
   (spike). `bootstrap`/`bootout` are used, gated by `print`, and verified by `print`.

5. **The layer is parameterised by a `ServiceSpec`** (label, program, args, log
   paths). `ServiceSpec::ctm()` is what the CLI uses; a test can install a spec that
   runs `/bin/sleep` under a throwaway label in the real manager. This is what makes
   an honest end-to-end test possible without touching the operator's service.

6. **Proof is a real e2e on both managers in CI** (`tests/service_managers.rs`,
   ubuntu + macOS jobs): install → start (stable PID) → restart (PID changed, on-disk
   definition honoured after a program change) → stop (no PID) → uninstall (not
   loaded, files gone); and a program that exits immediately makes *start* report
   failure with the exit code, where before it reported "Service started." Pure
   tests cover the parsers and the verdict functions.

7. **No `let _ = Command::new(...)` remains in `src/service/`**, and the contract
   is pinned by a test that greps for it.

## Consequences

- `ctm service restart` on macOS takes as long as launchd's throttle when the
  process must be killed (~10 s); it did already, silently. It now says so.
- `ctm service stop` on macOS waits up to 10 s for the process to exit instead of
  returning immediately; the message is now true when it prints.
- `launchd.rs`/`systemd.rs` split into ops + state modules to stay under the
  500-line rule.
- The `ServiceStatus` API is unchanged; `main.rs`, `doctor`, `setup` and `update`
  need no changes and inherit truthful results.
