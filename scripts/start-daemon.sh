#!/bin/bash
#
# Start the Claude Code Telegram Mirror daemon
#
# This script sources environment variables from ~/.telegram-env
# to work around .bashrc's non-interactive shell exit.
#
# Usage:
#   ./start-daemon.sh              # Run in foreground
#   nohup ./start-daemon.sh &      # Run in background
#

set -e

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(dirname "$SCRIPT_DIR")"

# Source environment file (bypasses .bashrc non-interactive exit)
if [[ -f "$HOME/.telegram-env" ]]; then
    source "$HOME/.telegram-env"
elif [[ -f "$PACKAGE_DIR/.env" ]]; then
    source "$PACKAGE_DIR/.env"
else
    echo "⚠️  No environment file found."
    echo ""
    echo "Create ~/.telegram-env with:"
    echo '  export TELEGRAM_BOT_TOKEN="your-token"'
    echo '  export TELEGRAM_CHAT_ID="your-chat-id"'
    echo '  export TELEGRAM_MIRROR=true'
    echo ""
    echo "Or create $PACKAGE_DIR/.env with the same exports."
    exit 1
fi

# Verify required variables
if [[ -z "$TELEGRAM_BOT_TOKEN" ]]; then
    echo "❌ TELEGRAM_BOT_TOKEN is not set"
    exit 1
fi

if [[ -z "$TELEGRAM_CHAT_ID" ]]; then
    echo "❌ TELEGRAM_CHAT_ID is not set"
    exit 1
fi

# Change to package directory and start
cd "$PACKAGE_DIR"
exec node dist/cli.js start
