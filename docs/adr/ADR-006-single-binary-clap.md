# ADR-006: Single Binary with clap Subcommands

## Status
Accepted

## Context
The TypeScript version uses commander.js with multiple entry points (cli.ts, hooks/handler.ts). Distribution requires Node.js runtime and npm install.

## Decision
Single static binary `ctm` using clap derive macros for subcommands: `start`, `stop`, `hook`, `setup`, `doctor`, `status`. All functionality in one binary, no runtime dependencies.

## Consequences
- **Positive**: Single file deployment, no runtime needed, fast startup (<10ms), easy container integration
- **Negative**: Larger binary size (~15MB with static linking)
- **Distribution**: Copy binary to PATH, or install via cargo install
