#!/bin/bash
#
# Global Claude Code Hooks Runner
#
# This script runs all global hooks and should be called from project-level settings.
# It reads stdin (hook JSON) and passes it to each global hook.
#
# Usage in project .claude/settings.json:
#   "hooks": {
#     "UserPromptSubmit": [
#       { "type": "command", "command": "~/.claude/global-hooks.sh" }
#     ]
#   }
#

set -e

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Read stdin once and store it
INPUT=$(cat)

# Global hooks to run (add more as needed)
GLOBAL_HOOKS=(
    "/opt/claude-telegram-mirror/scripts/telegram-hook.sh"
    # Add more global hooks here:
    # "/path/to/another-hook.sh"
)

# Run each global hook, passing the same input
for hook in "${GLOBAL_HOOKS[@]}"; do
    if [[ -x "$hook" ]]; then
        echo "$INPUT" | "$hook" 2>/dev/null || true
    fi
done

# Pass through the original input for downstream processing
echo "$INPUT"
