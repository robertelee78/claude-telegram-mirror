# ADR-004: flock(2) for Atomic PID Locking

## Status
Accepted

## Context
The TypeScript version uses check-then-write for PID locking: read PID file, check if process exists, write new PID. This has a TOCTOU race condition (MEDIUM severity) where two processes can both pass the check and both write their PID.

## Decision
Use `flock(2)` via the `nix` crate for atomic PID file locking. The lock file is held open for the daemon's lifetime. `flock(LOCK_EX | LOCK_NB)` fails immediately if another process holds the lock, eliminating the race.

## Consequences
- **Positive**: Atomic, race-free, kernel-enforced locking; auto-released on process crash
- **Negative**: Linux/macOS only (no Windows support, which we don't need)
- **Note**: The lock fd must be kept open for the daemon's lifetime; dropping it releases the lock
