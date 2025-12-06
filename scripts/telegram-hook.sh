#!/bin/bash
#
# Claude Code Telegram Hook
# Captures hook events and forwards to bridge daemon
#
# Usage: This script is called by Claude Code's hook system
# It reads JSON from stdin and forwards to the bridge
#
# The hook is ENABLED when the bridge daemon is running (socket exists).
# No environment variable needed - just start the bridge!
#

# Debug logging disabled by default - set TELEGRAM_HOOK_DEBUG=1 to enable
TELEGRAM_HOOK_DEBUG="${TELEGRAM_HOOK_DEBUG:-0}"

set -e

# Trap to ensure clean exit and helpful error message
trap 'debug_log "Script exiting with code $?"' EXIT

# Get the directory of this script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(dirname "$SCRIPT_DIR")"
CONFIG_DIR="$HOME/.config/claude-telegram-mirror"

# Socket path for bridge communication (now in user config dir, not /tmp)
SOCKET_PATH="${TELEGRAM_BRIDGE_SOCKET:-$CONFIG_DIR/bridge.sock}"

# Debug logging (set TELEGRAM_HOOK_DEBUG=1 to enable)
debug_log() {
  if [[ "${TELEGRAM_HOOK_DEBUG}" == "1" ]]; then
    # Log to config dir instead of world-readable /tmp
    mkdir -p "$CONFIG_DIR" 2>/dev/null || true
    echo "[telegram-hook] $(date '+%Y-%m-%d %H:%M:%S') $1" >> "$CONFIG_DIR/hook-debug.log"
  fi
}

debug_log "Hook called, checking socket at $SOCKET_PATH"

# Check if bridge is running (socket exists = enabled)
if [[ ! -S "$SOCKET_PATH" ]]; then
  # Bridge not running, pass through silently
  debug_log "Bridge not running (no socket), passing through"
  cat
  exit 0
fi

debug_log "Bridge socket found, processing..."

# Read stdin into variable
INPUT=$(cat)

# If empty, just exit
if [[ -z "$INPUT" ]]; then
  debug_log "Empty input, exiting"
  exit 0
fi

# Log raw input for debugging
debug_log "Raw input: $INPUT"

# Parse hook type from input (field is "hook_event_name" not "type")
HOOK_TYPE=$(echo "$INPUT" | jq -r '.hook_event_name // .type // empty' 2>/dev/null || echo "")
debug_log "Hook type: $HOOK_TYPE"

# Use Claude's native session_id from the hook input - this is the canonical session identifier
# This ensures all events from the same Claude session go to the same Telegram topic
CLAUDE_SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null || echo "")
debug_log "Claude session_id: $CLAUDE_SESSION_ID"

# Session tracking - detect first event per Claude session
get_session_tracking_path() {
  mkdir -p "$CONFIG_DIR" 2>/dev/null || true
  # Use Claude's session_id as the key - this is stable for the entire session
  if [[ -n "$CLAUDE_SESSION_ID" ]]; then
    echo "$CONFIG_DIR/.session_active_${CLAUDE_SESSION_ID}"
  else
    # Fallback to tmux pane if no Claude session_id
    local session_key=""
    if [[ -n "$TMUX" ]]; then
      session_key=$(tmux display-message -p '#{session_id}_#{window_id}_#{pane_id}' 2>/dev/null || echo "")
    fi
    if [[ -z "$session_key" ]]; then
      session_key=$(tty 2>/dev/null | tr '/' '_' || echo "default")
    fi
    local safe_id=$(echo "$session_key" | tr -cd '[:alnum:]_')
    echo "$CONFIG_DIR/.session_active_${safe_id}"
  fi
}

is_first_event() {
  local tracking_path=$(get_session_tracking_path)
  if [[ -f "$tracking_path" ]]; then
    return 1  # Not first event
  fi
  # Mark session as started
  echo "$SESSION_ID" > "$tracking_path"
  return 0  # First event
}

clear_session_tracking() {
  local tracking_path=$(get_session_tracking_path)
  rm -f "$tracking_path" 2>/dev/null || true
}

# Use Claude's native session_id, or generate one as fallback
SESSION_ID="${CLAUDE_SESSION_ID:-$(date +%s)-$$}"
debug_log "Using session ID: $SESSION_ID"

# Get tmux info if available
get_tmux_info() {
  if [[ -z "$TMUX" ]]; then
    echo "{}"
    return
  fi

  local session=$(tmux display-message -p "#S" 2>/dev/null || echo "")
  local pane=$(tmux display-message -p "#P" 2>/dev/null || echo "")
  local window=$(tmux display-message -p "#I" 2>/dev/null || echo "")

  if [[ -n "$session" && -n "$window" && -n "$pane" ]]; then
    local target="${session}:${window}.${pane}"
    jq -cn \
      --arg session "$session" \
      --arg pane "$pane" \
      --arg target "$target" \
      '{tmuxSession: $session, tmuxPane: $pane, tmuxTarget: $target}'
  else
    echo "{}"
  fi
}

# Send message to bridge via netcat (fast)
send_to_bridge() {
  local message="$1"
  debug_log "Sending to bridge: ${message:0:100}..."

  if command -v nc &> /dev/null; then
    local nc_stderr
    nc_stderr=$(echo "$message" | nc -U -q0 "$SOCKET_PATH" 2>&1)
    local nc_exit=$?
    debug_log "nc result: exit=$nc_exit, stderr=$nc_stderr"
    if [[ $nc_exit -ne 0 ]]; then
      debug_log "nc FAILED! socket=$SOCKET_PATH"
    fi
  elif [[ -S "$SOCKET_PATH" ]]; then
    echo "$message" > "$SOCKET_PATH" 2>/dev/null || true
    debug_log "Used direct write to socket"
  else
    debug_log "No nc and no socket!"
  fi
}

# ============================================
# REAL-TIME TRANSCRIPT SYNC
# Extract new assistant text on EVERY hook event
# This provides real-time mirroring of Claude's responses
# ============================================
sync_new_assistant_text() {
  local transcript_path=$(echo "$INPUT" | jq -r '.transcript_path // ""' 2>/dev/null)

  if [[ -z "$transcript_path" || ! -f "$transcript_path" ]]; then
    return
  fi

  # Use separate state file for continuous sync (different from Stop hook)
  local state_file="$CONFIG_DIR/.sync_${SESSION_ID}"
  local last_line=0
  if [[ -f "$state_file" ]]; then
    last_line=$(cat "$state_file" 2>/dev/null || echo 0)
  fi

  local current_line=$(wc -l < "$transcript_path")

  # No new lines
  if [[ $current_line -le $last_line ]]; then
    return
  fi

  local new_count=$((current_line - last_line))
  local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
  local found_text=0

  # Process new lines for assistant text messages
  while IFS= read -r line; do
    local msg_type=$(echo "$line" | jq -r '.type // empty' 2>/dev/null)

    if [[ "$msg_type" == "assistant" ]]; then
      # Extract text content (skip tool_use blocks)
      local text=$(echo "$line" | jq -r '.message.content[]? | select(.type == "text") | .text' 2>/dev/null)

      if [[ -n "$text" && ${#text} -gt 10 ]]; then
        debug_log "Sync: Found assistant text (${#text} chars)"
        # Use printf + pipe to handle large content (avoids "Argument list too long")
        local msg=$(printf '%s' "$text" | jq -Rsc \
          --arg type "agent_response" \
          --arg sessionId "$SESSION_ID" \
          --arg timestamp "$timestamp" \
          '{type: $type, sessionId: $sessionId, timestamp: $timestamp, content: .}')
        send_to_bridge "$msg"
        found_text=1
      fi
    fi
  done < <(tail -n "$new_count" "$transcript_path")

  # Update state (always, even if no text found - to track line position)
  echo "$current_line" > "$state_file"

  if [[ $found_text -eq 1 ]]; then
    debug_log "Sync: Sent new assistant text"
  fi
}

# ============================================
# REAL-TIME SYNC: DISABLED - causing replay issues
# TODO: Fix race conditions before re-enabling
# ============================================
# sync_new_assistant_text &

# Format bridge message based on hook type
format_message() {
  local hook_type="$1"
  local input="$2"
  local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

  case "$hook_type" in
    "PreToolUse")
      local tool_name=$(echo "$input" | jq -r '.tool_name // "unknown"')
      local tool_input=$(echo "$input" | jq -c '.tool_input // {}')

      # Send tool info (not just dangerous ones - for visibility)
      jq -cn \
        --arg type "tool_start" \
        --arg sessionId "$SESSION_ID" \
        --arg timestamp "$timestamp" \
        --arg tool "$tool_name" \
        --argjson input "$tool_input" \
        '{type: $type, sessionId: $sessionId, timestamp: $timestamp, content: "Tool: \($tool)", metadata: {tool: $tool, input: $input}}'
      ;;

    "PostToolUse")
      local tool_name=$(echo "$input" | jq -r '.tool_name // "unknown"')
      local tool_output=$(echo "$input" | jq -r '.tool_output // ""' | head -c 2000)

      # Only send significant outputs
      if [[ ${#tool_output} -gt 10 ]]; then
        jq -cn \
          --arg type "tool_result" \
          --arg sessionId "$SESSION_ID" \
          --arg timestamp "$timestamp" \
          --arg tool "$tool_name" \
          --arg output "$tool_output" \
          '{type: $type, sessionId: $sessionId, timestamp: $timestamp, content: $output, metadata: {tool: $tool}}'
      fi
      ;;

    "Notification")
      local message=$(echo "$input" | jq -r '.message // ""')
      local notification_type=$(echo "$input" | jq -r '.notification_type // ""')
      local level=$(echo "$input" | jq -r '.level // "info"')

      # Skip idle_prompt notifications - they're just noise
      if [[ "$notification_type" == "idle_prompt" ]]; then
        debug_log "Skipping idle_prompt notification"
        return
      fi

      if [[ -n "$message" ]]; then
        jq -cn \
          --arg type "agent_response" \
          --arg sessionId "$SESSION_ID" \
          --arg timestamp "$timestamp" \
          --arg message "$message" \
          --arg level "$level" \
          '{type: $type, sessionId: $sessionId, timestamp: $timestamp, content: $message, metadata: {level: $level}}'
      fi
      ;;

    "Stop")
      local transcript_path=$(echo "$input" | jq -r '.transcript_path // ""')
      debug_log "Stop: transcript_path='$transcript_path'"
      debug_log "Stop: file exists=$(test -f "$transcript_path" && echo yes || echo no)"

      # Extract NEW text content since last Stop
      # Track what we've sent to avoid duplicates
      if [[ -n "$transcript_path" && -f "$transcript_path" ]]; then
        debug_log "Reading transcript: $transcript_path"

        # Track last processed line using a state file
        local state_file="$CONFIG_DIR/.last_line_${SESSION_ID}"
        local last_line=0
        if [[ -f "$state_file" ]]; then
          last_line=$(cat "$state_file" 2>/dev/null || echo 0)
        fi

        local current_line=$(wc -l < "$transcript_path")
        debug_log "Last processed: $last_line, Current: $current_line"

        # Only process new lines
        if [[ $current_line -gt $last_line ]]; then
          local new_lines=$((current_line - last_line))
          local all_text=""

          # Process only NEW lines
          while IFS= read -r line; do
            local text=$(echo "$line" | jq -r 'select(.type == "assistant") | .message.content[]? | select(.type == "text") | .text' 2>/dev/null)
            if [[ -n "$text" ]]; then
              if [[ -n "$all_text" ]]; then
                all_text="${all_text}

${text}"
              else
                all_text="$text"
              fi
            fi
          done < <(tail -n "$new_lines" "$transcript_path")

          debug_log "New text length: ${#all_text}"
          debug_log "New text preview: ${all_text:0:200}"

          if [[ -n "$all_text" ]]; then
            # Use printf + pipe to handle large content (avoids "Argument list too long")
            local agent_msg=$(printf '%s' "$all_text" | jq -Rsc \
              --arg type "agent_response" \
              --arg sessionId "$SESSION_ID" \
              --arg timestamp "$timestamp" \
              '{type: $type, sessionId: $sessionId, timestamp: $timestamp, content: .}')
            debug_log "Stop: sending agent_response: ${agent_msg:0:150}"
            echo "$agent_msg"
          else
            debug_log "Stop: NO text extracted from new lines"
          fi

          # Update state
          echo "$current_line" > "$state_file"
        else
          debug_log "Stop: no new lines (current=$current_line, last=$last_line)"
        fi
      else
        debug_log "Stop: transcript not accessible (path='$transcript_path', exists=$(test -f "$transcript_path" && echo yes || echo no))"
      fi

      # DON'T send session_end on every Stop - Claude fires Stop after every turn!
      # The session is still active. Only send a turn_complete notification.
      # Session end should happen when user explicitly exits or connection drops.
      local turn_msg=$(jq -cn \
        --arg type "turn_complete" \
        --arg sessionId "$SESSION_ID" \
        --arg timestamp "$timestamp" \
        '{type: $type, sessionId: $sessionId, timestamp: $timestamp, content: "Turn complete"}')
      debug_log "Stop: sending turn_complete: $turn_msg"
      echo "$turn_msg"
      ;;

    "UserPromptSubmit")
      local prompt=$(echo "$input" | jq -r '.prompt // ""')

      if [[ -n "$prompt" ]]; then
        jq -cn \
          --arg type "user_input" \
          --arg sessionId "$SESSION_ID" \
          --arg timestamp "$timestamp" \
          --arg prompt "$prompt" \
          '{type: $type, sessionId: $sessionId, timestamp: $timestamp, content: $prompt, metadata: {source: "cli"}}'
      fi
      ;;
  esac
}

# Check if this is the first event of this session
if is_first_event; then
  debug_log "First event of session - sending session_start"

  # Get tmux info and hostname
  TMUX_INFO=$(get_tmux_info)
  HOSTNAME=$(hostname)
  PROJECT_DIR=$(pwd)
  TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

  # Send session_start message (compact JSON for NDJSON protocol)
  SESSION_START=$(jq -cn \
    --arg type "session_start" \
    --arg sessionId "$SESSION_ID" \
    --arg timestamp "$TIMESTAMP" \
    --arg hostname "$HOSTNAME" \
    --arg projectDir "$PROJECT_DIR" \
    --argjson tmux "$TMUX_INFO" \
    '{type: $type, sessionId: $sessionId, timestamp: $timestamp, content: "Claude Code session started", metadata: ({hostname: $hostname, projectDir: $projectDir} + $tmux)}')

  send_to_bridge "$SESSION_START"
fi

# Clear session tracking on Stop event
# Note: We keep the session active even after Stop because Notification events
# may come after Stop and should still go to the same session thread
if [[ "$HOOK_TYPE" == "Stop" ]]; then
  debug_log "Stop event received (keeping session for potential follow-up events)"
  # Don't clear immediately - let the session timeout naturally or clear on next UserPromptSubmit
fi

# Format and send message(s)
if [[ -n "$HOOK_TYPE" ]]; then
  # format_message may return multiple JSON lines (e.g., Stop returns responses + session_end)
  MESSAGES=$(format_message "$HOOK_TYPE" "$INPUT")

  if [[ -n "$MESSAGES" ]]; then
    # Send each line as a separate message
    while IFS= read -r msg; do
      if [[ -n "$msg" ]]; then
        send_to_bridge "$msg"
      fi
    done <<< "$MESSAGES"
  fi
fi

# Pass through original input for Claude Code
echo "$INPUT"
