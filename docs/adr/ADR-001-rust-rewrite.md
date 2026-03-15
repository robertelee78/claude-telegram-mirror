# ADR-001: Rust Over TypeScript

## Status
Accepted

## Context
The TypeScript implementation has 3 CRITICAL command injection vulnerabilities stemming from `execSync` with string interpolation, world-readable file permissions due to Node.js `fs` defaults, and runtime panics from unhandled promise rejections. Node.js requires a ~50MB runtime and has no compile-time memory safety guarantees.

## Decision
Rewrite in Rust using `std::process::Command::arg()` for all subprocess invocation, `std::fs::OpenOptions::mode()` for all file creation, and `thiserror`/`anyhow` for exhaustive error handling.

## Consequences
- **Positive**: Eliminates entire vulnerability classes (command injection, type coercion bugs), produces a single static binary, zero runtime dependency, compile-time memory safety
- **Negative**: Longer initial development time, smaller ecosystem for Telegram bots, team must know Rust
- **Risk**: teloxide API surface is smaller than grammY; some features may need manual implementation
