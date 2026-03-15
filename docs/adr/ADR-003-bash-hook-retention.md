# ADR-003: Retain Bash Hook Script for Fire-and-Forget

## Status
Accepted

## Context
Claude Code hooks come in two flavors: fire-and-forget (PostToolUse, Stop, UserPromptSubmit) and approval (PreToolUse requiring stdin/stdout JSON protocol). The bash script `telegram-hook.sh` handles fire-and-forget by writing to the Unix socket and exiting. The approval flow requires reading stdin, sending to daemon, waiting for response, and writing to stdout.

## Decision
Keep `telegram-hook.sh` for fire-and-forget hooks (minimal latency, no binary startup). Use `ctm hook` (Rust binary) for approval flow where the stdin/stdout protocol requires structured JSON handling.

## Consequences
- **Positive**: Fire-and-forget hooks have <1ms overhead (no binary startup), approval flow gets type-safe JSON handling
- **Negative**: Two hook entry points to maintain
- **Mitigation**: Hook script is ~20 lines and stable; approval logic is the complex part that benefits from Rust
