#!/bin/bash
# Get Telegram Chat ID helper
# Usage: ./get-chat-id.sh [BOT_TOKEN]

set -e

# Get token from argument or environment
TOKEN="${1:-$TELEGRAM_BOT_TOKEN}"

if [[ -z "$TOKEN" ]]; then
    echo "Usage: $0 <BOT_TOKEN>"
    echo "   or: export TELEGRAM_BOT_TOKEN=... && $0"
    exit 1
fi

echo "Fetching updates from Telegram API..."
echo ""

# Call getUpdates
RESPONSE=$(curl -s "https://api.telegram.org/bot${TOKEN}/getUpdates")

# Check if valid response
if echo "$RESPONSE" | grep -q '"ok":false'; then
    ERROR=$(echo "$RESPONSE" | grep -o '"description":"[^"]*"' | cut -d'"' -f4)
    echo "Error: $ERROR"
    echo ""
    echo "Make sure your bot token is correct."
    exit 1
fi

# Extract unique chat IDs
CHAT_IDS=$(echo "$RESPONSE" | grep -o '"chat":{"id":-\?[0-9]*' | grep -o '\-\?[0-9]*' | sort -u)

if [[ -z "$CHAT_IDS" ]]; then
    echo "No messages found yet."
    echo ""
    echo "To get your chat ID:"
    echo "  1. Add your bot to a group (or start a chat with it)"
    echo "  2. Send any message in that chat"
    echo "  3. Run this script again"
    echo ""
    echo "Tip: For supergroups with Topics, the ID starts with -100"
    exit 0
fi

echo "Found chat IDs:"
echo ""

for ID in $CHAT_IDS; do
    # Determine chat type based on ID format
    if [[ "$ID" =~ ^-100 ]]; then
        TYPE="supergroup"
    elif [[ "$ID" =~ ^- ]]; then
        TYPE="group"
    else
        TYPE="private chat"
    fi

    echo "  $ID  ($TYPE)"
done

echo ""
echo "Add to ~/.telegram-env:"
echo "  export TELEGRAM_CHAT_ID=\"$( echo "$CHAT_IDS" | head -1 )\""
