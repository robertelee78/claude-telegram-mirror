# ADR-002: teloxide for Telegram Bot Framework

## Status
Accepted

## Context
Rust Telegram bot crates: teloxide (most mature, async, derive macros), frankenstein (lower-level), telegram-bot (abandoned). Need forum topic support, inline keyboards, callback queries, and message editing.

## Decision
Use teloxide 0.13 with its dispatching framework. Forum topic operations (create, close, reopen, delete) are supported via Bot API methods. Inline keyboards and callback queries are first-class.

## Consequences
- **Positive**: Mature ecosystem, strong typing for Bot API, built-in update filtering, middleware support for chat_id validation
- **Negative**: Opinionated dispatcher pattern may need adaptation for our socket-driven architecture
- **Mitigation**: Use teloxide's `Bot` struct directly for API calls, custom message routing via our bridge
