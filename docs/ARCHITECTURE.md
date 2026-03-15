# Architecture Guide

## System Overview

Claude Code Rust Telegram (CTM) is a bidirectional bridge between Claude Code CLI sessions and Telegram. It captures Claude Code hook events, forwards them to Telegram, and injects Telegram replies back into the CLI via tmux.

```mermaid
graph TB
    subgraph "Developer Machine"
        CC[Claude Code CLI]
        TMUX[tmux session]
        CC -->|runs inside| TMUX
    end

    subgraph "CTM Bridge Daemon"
        HOOK[Hook Handler<br/>ctm hook]
        SOCK[Socket Server<br/>Unix socket + flock]
        BRIDGE[Bridge Orchestrator]
        BOT[Telegram Bot<br/>teloxide + governor]
        SESSION[Session Manager<br/>SQLite + rusqlite]
        INJ[Input Injector<br/>Command::arg]

        HOOK -->|NDJSON| SOCK
        SOCK -->|mpsc channel| BRIDGE
        BRIDGE --> BOT
        BRIDGE --> SESSION
        BRIDGE --> INJ
    end

    subgraph "Telegram"
        TG[Telegram API]
        GROUP[Supergroup with Topics]
        PHONE[Mobile App]

        TG --> GROUP
        GROUP --> PHONE
    end

    CC -->|hooks stdin/stdout| HOOK
    BOT -->|send messages| TG
    TG -->|long polling| BOT
    INJ -->|tmux send-keys| TMUX
    PHONE -->|user replies| TG

    style BRIDGE fill:#f96,stroke:#333,stroke-width:2px
    style SOCK fill:#69f,stroke:#333,stroke-width:2px
    style BOT fill:#9f6,stroke:#333,stroke-width:2px
```

## Module Dependency Graph

```mermaid
graph LR
    MAIN[main.rs] --> BRIDGE[bridge.rs]
    MAIN --> CONFIG[config.rs]
    MAIN --> HOOK[hook.rs]
    MAIN --> SESSION[session.rs]
    MAIN --> INJ[injector.rs]

    BRIDGE --> BOT[bot.rs]
    BRIDGE --> SOCKET[socket.rs]
    BRIDGE --> SESSION
    BRIDGE --> INJ
    BRIDGE --> CONFIG
    BRIDGE --> FMT[formatting.rs]

    HOOK --> TYPES[types.rs]
    HOOK --> INJ

    BOT --> TYPES
    SOCKET --> TYPES
    SESSION --> TYPES

    BRIDGE --> TYPES
    CONFIG --> ERROR[error.rs]
    SESSION --> ERROR
    SOCKET --> ERROR
    BOT --> ERROR

    style BRIDGE fill:#f96,stroke:#333
    style TYPES fill:#ff9,stroke:#333
    style ERROR fill:#f99,stroke:#333
```

## Message Flow

### CLI to Telegram (Outbound)

```mermaid
sequenceDiagram
    participant CC as Claude Code
    participant Hook as ctm hook
    participant Socket as Socket Server
    participant Bridge as Bridge
    participant Bot as Telegram Bot
    participant TG as Telegram

    CC->>Hook: Hook event (stdin JSON)
    Hook->>Hook: Parse HookEvent
    Hook->>Hook: Add tmux metadata
    Hook->>Socket: Connect + send NDJSON
    Hook->>CC: Pass through (stdout)

    Socket->>Bridge: mpsc::Receiver<BridgeMessage>
    Bridge->>Bridge: Route by MessageType

    alt SessionStart
        Bridge->>Bridge: Create/reactivate session
        Bridge->>Bot: Create forum topic
        Bot->>TG: createForumTopic
        TG-->>Bot: topic_id
        Bridge->>Bot: Send start notification
    else AgentResponse
        Bridge->>Bot: Send formatted response
    else ToolStart
        Bridge->>Bot: Send tool preview + Details button
    else TurnComplete
        Bridge->>Bridge: Check compaction state
    end

    Bot->>TG: sendMessage (thread_id)
```

### Telegram to CLI (Inbound)

```mermaid
sequenceDiagram
    participant User as User (Phone)
    participant TG as Telegram
    participant Bot as Telegram Bot
    participant Bridge as Bridge
    participant INJ as InputInjector
    participant TMUX as tmux

    User->>TG: Send message in topic
    TG->>Bot: getUpdates (long poll)
    Bot->>Bridge: Update event

    Bridge->>Bridge: Validate chat_id (Security #5)
    Bridge->>Bridge: Find session by thread_id

    alt Regular text
        Bridge->>Bridge: Dedup check
        Bridge->>INJ: inject(text)
        INJ->>TMUX: send-keys -l "text"
        INJ->>TMUX: send-keys Enter
    else "stop" / "esc"
        Bridge->>INJ: send_key("Escape")
        INJ->>TMUX: send-keys Escape
    else "kill" / "ctrl-c"
        Bridge->>INJ: send_key("Ctrl-C")
        INJ->>TMUX: send-keys C-c
    else "cc clear"
        Bridge->>INJ: send_slash_command("/clear")
        INJ->>TMUX: send-keys -l "/clear" Enter
    end
```

### Tool Approval Flow

```mermaid
sequenceDiagram
    participant CC as Claude Code
    participant Hook as ctm hook
    participant Bridge as Bridge
    participant DB as SQLite
    participant Bot as Telegram Bot
    participant User as User (Phone)

    CC->>Hook: PreToolUse (stdin)
    Hook->>Bridge: ToolStart message
    Bridge->>DB: create_approval()
    Bridge->>Bot: Send with inline keyboard
    Bot->>User: [Approve] [Reject] [Abort]

    User->>Bot: Click "Approve"
    Bot->>Bridge: CallbackQuery
    Bridge->>DB: resolve_approval("approved")
    Bridge->>Bot: Edit message + answer query

    Note over CC: Approval is non-blocking.<br/>Claude falls back to CLI<br/>if no response in 5 min.
```

## Session Lifecycle

```mermaid
stateDiagram-v2
    [*] --> Active: First hook event<br/>(auto-created)

    Active --> Active: Hook events<br/>update activity

    Active --> Ended: Stop event<br/>(turn complete)

    Ended --> Active: New hook event<br/>(reactivate)

    Ended --> TopicClosed: auto_delete=false
    Ended --> PendingDeletion: auto_delete=true

    PendingDeletion --> TopicDeleted: delay expires
    PendingDeletion --> Active: New event<br/>(cancel deletion)

    Active --> StaleCleanup: No tmux: 1h idle<br/>With tmux: 24h + pane dead

    StaleCleanup --> TopicDeleted: auto_delete=true
    StaleCleanup --> TopicClosed: auto_delete=false

    TopicClosed --> [*]
    TopicDeleted --> [*]
```

## Socket Protocol

CTM uses **NDJSON** (Newline-Delimited JSON) over a Unix domain socket:

```mermaid
graph LR
    subgraph "Client (ctm hook)"
        C1[Connect to socket]
        C2[Write JSON lines]
        C3[Shutdown write]
    end

    subgraph "Server (bridge)"
        S1[Accept connection]
        S2[Read lines via BufReader]
        S3[Parse BridgeMessage]
        S4[Route to handler]
    end

    C1 --> S1
    C2 -->|"NDJSON\n"| S2
    S2 --> S3
    S3 --> S4
```

### BridgeMessage Format

```json
{
  "msgType": "tool_start",
  "sessionId": "session-abc123",
  "timestamp": "2025-01-15T10:30:00Z",
  "content": "Tool: Bash",
  "metadata": {
    "tool": "Bash",
    "input": { "command": "cargo test" },
    "tmuxTarget": "workspace:0.0",
    "hostname": "dev-machine"
  }
}
```

### Message Types

| Type | Direction | Description |
|------|-----------|-------------|
| `session_start` | CLI -> TG | New session detected |
| `session_end` | CLI -> TG | Session terminated |
| `agent_response` | CLI -> TG | Claude's text response |
| `tool_start` | CLI -> TG | Tool execution beginning |
| `tool_result` | CLI -> TG | Tool output (verbose mode) |
| `user_input` | CLI -> TG | User typed in CLI |
| `approval_request` | CLI -> TG | Tool needs approval |
| `error` | CLI -> TG | Error notification |
| `turn_complete` | CLI -> TG | Claude finished a turn |
| `pre_compact` | CLI -> TG | Context compaction starting |

## Security Architecture

```mermaid
graph TB
    subgraph "Input Boundary"
        STDIN[stdin JSON]
        TGAPI[Telegram Updates]
    end

    subgraph "Validation Layer"
        PARSE[serde_json::from_str<br/>Result, no panic]
        CHATID[Chat ID filter<br/>ALL update types]
        KEYWHITE[Tmux key whitelist<br/>ALLOWED_TMUX_KEYS]
    end

    subgraph "Execution Layer"
        CMD["Command::new('tmux').arg()<br/>No shell interpolation"]
        FLOCK["flock(2)<br/>Atomic PID lock"]
        PERMS["File permissions<br/>0o600 files, 0o700 dirs"]
        RATE["governor rate limiter<br/>25 req/sec"]
    end

    STDIN --> PARSE
    TGAPI --> CHATID
    CHATID --> KEYWHITE

    PARSE --> CMD
    KEYWHITE --> CMD
    CMD --> FLOCK
    FLOCK --> PERMS
    PERMS --> RATE

    style PARSE fill:#faa,stroke:#333
    style CHATID fill:#faa,stroke:#333
    style KEYWHITE fill:#faa,stroke:#333
    style CMD fill:#afa,stroke:#333
    style FLOCK fill:#afa,stroke:#333
```

### Vulnerability Matrix

| # | Severity | Vulnerability | Fix |
|---|----------|--------------|-----|
| 1 | CRITICAL | Command injection in tmux slash commands | `Command::new("tmux").arg()` |
| 2 | CRITICAL | FIFO path shell interpolation | Eliminated entirely |
| 3 | CRITICAL | World-readable config/secrets | `OpenOptions::mode(0o600)` |
| 4 | HIGH | Logs in world-readable /tmp | Logs in config dir with 0o600 |
| 5 | HIGH | Chat ID bypass on callbacks | Filter on ALL update types |
| 6 | HIGH | Config dir insecure permissions | `mkdir` + `chmod 0o700` |
| 7 | HIGH | tmux target interpolation | Passed as `.arg()` only |
| 8 | MEDIUM | TOCTOU race in PID locking | `flock(2)` atomic lock |
| 9 | MEDIUM | No input rate limiting | `governor` token-bucket |
| 10 | MEDIUM | Panic on malformed JSON | `serde_json` returns `Result` |

## Concurrency Model

```mermaid
graph TB
    subgraph "Main Thread"
        START[bridge.start()]
        START --> SPAWN
    end

    subgraph "Spawned Tasks (tokio::spawn)"
        SPAWN --> SOCKET_TASK[Socket Handler<br/>while msg_rx.recv()]
        SPAWN --> POLL_TASK[Telegram Poller<br/>get_updates loop]
        SPAWN --> CLEANUP_TASK[Cleanup Timer<br/>every 5 minutes]
    end

    subgraph "Shared State (Arc)"
        SESSIONS["Arc&lt;Mutex&lt;SessionManager&gt;&gt;"]
        INJECTOR["Arc&lt;Mutex&lt;InputInjector&gt;&gt;"]
        THREADS["Arc&lt;RwLock&lt;HashMap&gt;&gt;<br/>session -> thread_id"]
        TARGETS["Arc&lt;RwLock&lt;HashMap&gt;&gt;<br/>session -> tmux_target"]
        CACHE["Arc&lt;RwLock&lt;HashMap&gt;&gt;<br/>tool input cache"]
    end

    SOCKET_TASK --> SESSIONS
    SOCKET_TASK --> THREADS
    SOCKET_TASK --> TARGETS
    SOCKET_TASK --> CACHE
    POLL_TASK --> SESSIONS
    POLL_TASK --> INJECTOR
    POLL_TASK --> THREADS
    CLEANUP_TASK --> SESSIONS
    CLEANUP_TASK --> THREADS

    style SESSIONS fill:#69f,stroke:#333
    style INJECTOR fill:#69f,stroke:#333
```

### Task Communication

| Channel | Type | Purpose |
|---------|------|---------|
| `msg_rx` | `mpsc::Receiver<BridgeMessage>` | Socket -> Bridge (incoming hook events) |
| `broadcast_tx` | `broadcast::Sender<BridgeMessage>` | Bridge -> Socket clients (outgoing) |

## Database Schema

```mermaid
erDiagram
    sessions {
        TEXT id PK
        INTEGER chat_id
        INTEGER thread_id
        TEXT status
        TEXT hostname
        TEXT project_dir
        TEXT tmux_target
        TEXT tmux_socket
        TEXT started_at
        TEXT last_activity
        TEXT ended_at
    }

    pending_approvals {
        TEXT id PK
        TEXT session_id FK
        TEXT prompt
        TEXT status
        TEXT created_at
        TEXT expires_at
        TEXT resolved_at
    }

    sessions ||--o{ pending_approvals : "has"
```

## Configuration Priority

```mermaid
graph TD
    ENV[Environment Variables<br/>TELEGRAM_BOT_TOKEN etc.] --> MERGE
    FILE[Config File<br/>~/.config/ctm/config.json] --> MERGE
    DEFAULTS[Defaults<br/>verbose=false, threads=true] --> MERGE

    MERGE[Merge with Priority] --> CONFIG[Final Config]

    ENV -.->|highest priority| MERGE
    FILE -.->|medium| MERGE
    DEFAULTS -.->|lowest| MERGE

    style ENV fill:#9f9,stroke:#333
    style CONFIG fill:#f96,stroke:#333
```

## Forum Topic Management

```mermaid
graph TB
    NEW_SESSION[New Session Event] --> CHECK_THREAD{Thread exists<br/>in DB?}

    CHECK_THREAD -->|Yes| REUSE[Reuse existing topic]
    CHECK_THREAD -->|No| CHECK_THREADS{use_threads<br/>enabled?}

    CHECK_THREADS -->|Yes| CREATE[Create forum topic]
    CHECK_THREADS -->|No| GENERAL[Use General topic]

    CREATE --> UNPIN[Unpin auto-pinned<br/>first message]
    UNPIN --> CACHE[Cache thread_id<br/>in memory + DB]

    REUSE --> CACHE
    CACHE --> SEND[Send messages<br/>to thread]

    SEND --> SESSION_END{Session ends?}

    SESSION_END -->|auto_delete=true| DELAY[Wait delete_delay minutes]
    SESSION_END -->|auto_delete=false| CLOSE[Close topic]

    DELAY --> DELETE[Delete topic]
    DELAY -->|new event arrives| CANCEL[Cancel deletion<br/>reactivate session]
```
