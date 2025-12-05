#!/bin/bash
#
# Claude Code Wrapper with Telegram Mirror
# Wraps existing claude command to enable Telegram mirroring
#
# Usage:
#   claude-wrapper <original-claude-args>
#
# Or create aliases:
#   alias dsp='TELEGRAM_MIRROR=true claude-wrapper --dangerously-skip-permissions'
#   alias dsp-c='TELEGRAM_MIRROR=true claude-wrapper --dangerously-skip-permissions -c'
#

set -e

# Get the directory of this script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(dirname "$SCRIPT_DIR")"

# Check if Telegram mirror is enabled
TELEGRAM_ENABLED="${TELEGRAM_MIRROR:-false}"

# Bridge daemon socket path
SOCKET_PATH="${TELEGRAM_BRIDGE_SOCKET:-/tmp/claude-telegram-bridge.sock}"

# Start bridge daemon if needed
start_bridge() {
  if [[ ! -S "$SOCKET_PATH" ]]; then
    echo "🔌 Starting Telegram bridge daemon..."

    # Start in background
    if command -v node &> /dev/null && [[ -f "$PACKAGE_DIR/dist/cli.js" ]]; then
      node "$PACKAGE_DIR/dist/cli.js" start &
      BRIDGE_PID=$!

      # Wait for socket to be created (max 5 seconds)
      for i in {1..50}; do
        if [[ -S "$SOCKET_PATH" ]]; then
          echo "✅ Bridge daemon started"
          break
        fi
        sleep 0.1
      done

      if [[ ! -S "$SOCKET_PATH" ]]; then
        echo "⚠️  Bridge daemon failed to start, continuing without Telegram mirror"
        TELEGRAM_ENABLED="false"
      fi
    else
      echo "⚠️  claude-telegram-mirror not installed properly"
      TELEGRAM_ENABLED="false"
    fi
  fi
}

# Notify session start
notify_start() {
  if [[ -S "$SOCKET_PATH" ]]; then
    local session_id="cli-$(date +%s)-$$"
    local project_dir="$(pwd)"

    echo "{\"type\":\"session_start\",\"sessionId\":\"$session_id\",\"timestamp\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"content\":\"Session started\",\"metadata\":{\"projectDir\":\"$project_dir\"}}" | \
      nc -U -q0 "$SOCKET_PATH" 2>/dev/null || true

    export TELEGRAM_SESSION_ID="$session_id"
  fi
}

# Notify session end
notify_end() {
  if [[ -S "$SOCKET_PATH" && -n "$TELEGRAM_SESSION_ID" ]]; then
    echo "{\"type\":\"session_end\",\"sessionId\":\"$TELEGRAM_SESSION_ID\",\"timestamp\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"content\":\"Session ended\"}" | \
      nc -U -q0 "$SOCKET_PATH" 2>/dev/null || true
  fi
}

# Cleanup on exit
cleanup() {
  notify_end

  # Kill bridge daemon if we started it
  if [[ -n "$BRIDGE_PID" ]]; then
    kill "$BRIDGE_PID" 2>/dev/null || true
  fi
}

# Main execution
main() {
  # Find the real claude command
  CLAUDE_CMD=$(which claude 2>/dev/null || echo "")

  if [[ -z "$CLAUDE_CMD" ]]; then
    echo "❌ Claude CLI not found"
    exit 1
  fi

  # Check if this is being called as a wrapper (avoid recursion)
  if [[ "$CLAUDE_WRAPPER_ACTIVE" == "true" ]]; then
    exec "$CLAUDE_CMD" "$@"
  fi

  export CLAUDE_WRAPPER_ACTIVE="true"

  if [[ "$TELEGRAM_ENABLED" == "true" || "$TELEGRAM_ENABLED" == "1" ]]; then
    # Start bridge if needed
    start_bridge

    # Setup cleanup trap
    trap cleanup EXIT

    # Notify session start
    notify_start

    echo "📱 Telegram mirror active"
    echo ""
  fi

  # Execute claude with all arguments
  exec "$CLAUDE_CMD" "$@"
}

main "$@"
