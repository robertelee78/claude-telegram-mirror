# ADR-005: governor Token-Bucket Rate Limiting

## Status
Accepted

## Context
The TypeScript version has basic rate limiting (1 msg/sec timer) but no per-chat-id limiting on input injection. A malicious or buggy client could flood the tmux session.

## Decision
Use the `governor` crate with token-bucket algorithm for rate limiting. Two rate limiters: one for outbound Telegram messages (existing behavior), one for inbound input injection per chat_id.

## Consequences
- **Positive**: Per-chat-id isolation, configurable burst capacity, zero-allocation steady state
- **Negative**: Additional dependency (~50KB)
- **Configuration**: Default 1 msg/sec outbound, 5 inputs/sec inbound with burst of 10
