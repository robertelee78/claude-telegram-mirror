#!/bin/bash
#
# Claude Telegram Mirror - Diagnostic Script
#
# Usage:
#   ./scripts/doctor.sh
#   ctm doctor  (if installed)
#
# Checks system health and helps troubleshoot issues.
#

# Colors (respect NO_COLOR)
if [[ -z "$NO_COLOR" ]]; then
  GREEN='\033[0;32m'
  RED='\033[0;31m'
  YELLOW='\033[1;33m'
  BLUE='\033[0;34m'
  BOLD='\033[1m'
  NC='\033[0m'
else
  GREEN=''
  RED=''
  YELLOW=''
  BLUE=''
  BOLD=''
  NC=''
fi

# Output helpers
ok() { echo -e "  ${GREEN}✓${NC} $1"; }
fail() { echo -e "  ${RED}✗${NC} $1"; ERRORS=$((ERRORS + 1)); }
warn() { echo -e "  ${YELLOW}⚠${NC} $1"; WARNINGS=$((WARNINGS + 1)); }
info() { echo -e "  ${BLUE}ℹ${NC} $1"; }

# Counters
ERRORS=0
WARNINGS=0

# Configuration paths
CONFIG_DIR="$HOME/.config/claude-telegram-mirror"
ENV_FILE="$HOME/.telegram-env"
INSTALL_DIR="${TELEGRAM_MIRROR_INSTALL_DIR:-$HOME/.local/share/claude-telegram-mirror}"
SOCKET_PATH="$CONFIG_DIR/bridge.sock"
DB_PATH="$CONFIG_DIR/sessions.db"
LOG_FILE="$CONFIG_DIR/hook-debug.log"

# ============================================
# HEADER
# ============================================

echo ""
echo -e "${BOLD}Claude Telegram Mirror - Diagnostic Report${NC}"
echo "==========================================="
echo "Generated: $(date)"
echo "Platform:  $(uname -s) $(uname -m)"
echo "User:      $USER"
echo ""

# ============================================
# 1. PREREQUISITES
# ============================================

echo -e "${BOLD}Prerequisites:${NC}"

check_cmd() {
  local cmd="$1"
  local min_version="${2:-}"

  if command -v "$cmd" &>/dev/null; then
    local ver=$("$cmd" --version 2>&1 | head -1 | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)
    ok "$cmd ($ver)"
    return 0
  else
    fail "$cmd: NOT FOUND"
    return 1
  fi
}

check_cmd node
check_cmd npm
check_cmd git
check_cmd tmux
check_cmd jq
check_cmd nc
check_cmd curl

# Node version check
NODE_VER=$(node -v 2>/dev/null | cut -d'v' -f2 | cut -d'.' -f1)
if [[ -n "$NODE_VER" && "$NODE_VER" -lt 18 ]]; then
  warn "Node.js 18+ required (found v$NODE_VER)"
fi

echo ""

# ============================================
# 2. CONFIGURATION
# ============================================

echo -e "${BOLD}Configuration:${NC}"

if [[ -f "$ENV_FILE" ]]; then
  ok "~/.telegram-env exists"
  source "$ENV_FILE"

  if [[ -n "$TELEGRAM_BOT_TOKEN" ]]; then
    # Show first 10 chars only (security)
    ok "TELEGRAM_BOT_TOKEN: ${TELEGRAM_BOT_TOKEN:0:10}..."
  else
    fail "TELEGRAM_BOT_TOKEN: NOT SET"
  fi

  if [[ -n "$TELEGRAM_CHAT_ID" ]]; then
    ok "TELEGRAM_CHAT_ID: $TELEGRAM_CHAT_ID"
    if [[ ! "$TELEGRAM_CHAT_ID" =~ ^-100 ]]; then
      warn "Chat ID should start with -100 for supergroups"
    fi
  else
    fail "TELEGRAM_CHAT_ID: NOT SET"
  fi

  info "TELEGRAM_MIRROR: ${TELEGRAM_MIRROR:-false}"
else
  fail "~/.telegram-env: NOT FOUND"
  info "Run installer or create manually"
fi

echo ""

# ============================================
# 3. INSTALLATION
# ============================================

echo -e "${BOLD}Installation:${NC}"

# Check common install locations
FOUND_INSTALL=""
for check_dir in "$INSTALL_DIR" "/opt/claude-mobile/packages/claude-telegram-mirror" "$HOME/.local/share/claude-telegram-mirror"; do
  if [[ -d "$check_dir" && -f "$check_dir/package.json" ]]; then
    FOUND_INSTALL="$check_dir"
    break
  fi
done

if [[ -n "$FOUND_INSTALL" ]]; then
  ok "Install dir: $FOUND_INSTALL"
  INSTALL_DIR="$FOUND_INSTALL"

  if [[ -f "$INSTALL_DIR/dist/cli.js" ]]; then
    ok "CLI built (dist/cli.js)"
  else
    fail "CLI not built - run: npm run build"
  fi

  if [[ -f "$INSTALL_DIR/package.json" ]]; then
    ver=$(jq -r '.version' "$INSTALL_DIR/package.json" 2>/dev/null || echo "unknown")
    info "Version: $ver"
  fi

  # Check if node_modules exists
  if [[ -d "$INSTALL_DIR/node_modules" ]]; then
    ok "Dependencies installed"
  else
    fail "Dependencies missing - run: npm install"
  fi
else
  fail "Not installed (checked $INSTALL_DIR)"
fi

# Check ctm symlink
if command -v ctm &>/dev/null; then
  ok "ctm command available"
else
  warn "ctm command not in PATH"
  info "Add ~/.local/bin to PATH or use: node $INSTALL_DIR/dist/cli.js"
fi

echo ""

# ============================================
# 4. HOOKS
# ============================================

echo -e "${BOLD}Hooks:${NC}"

SETTINGS="$HOME/.claude/settings.json"

if [[ -f "$SETTINGS" ]]; then
  ok "~/.claude/settings.json exists"

  if grep -q "telegram-hook" "$SETTINGS"; then
    ok "Telegram hooks configured"

    # Extract and check hook script path
    HOOK_PATH=$(grep -o '/[^"]*telegram-hook[^"]*' "$SETTINGS" | head -1)
    if [[ -n "$HOOK_PATH" ]]; then
      if [[ -f "$HOOK_PATH" ]]; then
        ok "Hook script exists: $HOOK_PATH"
        if [[ -x "$HOOK_PATH" ]]; then
          ok "Hook script is executable"
        else
          fail "Hook script not executable"
          info "Fix: chmod +x $HOOK_PATH"
        fi
      else
        fail "Hook script missing: $HOOK_PATH"
      fi
    fi

    # Check which hooks are registered
    for hook in PreToolUse PostToolUse Notification Stop UserPromptSubmit PreCompact; do
      if grep -q "\"$hook\"" "$SETTINGS"; then
        ok "$hook hook registered"
      fi
    done
  else
    fail "Telegram hooks not configured"
    info "Run: ctm install-hooks"
  fi
else
  warn "No ~/.claude/settings.json found"
  info "Claude Code creates this on first run"
fi

# Check for project-level hooks that might override
if [[ -n "$PWD" && -f "$PWD/.claude/settings.json" ]]; then
  warn "Project-level settings found in $PWD/.claude/"
  if grep -q "telegram-hook" "$PWD/.claude/settings.json"; then
    ok "Project hooks include telegram-hook"
  else
    warn "Project hooks do NOT include telegram-hook!"
    info "Run: ctm install-hooks --project (from project directory)"
  fi
fi

echo ""

# ============================================
# 5. SERVICE STATUS
# ============================================

echo -e "${BOLD}Service:${NC}"

if [[ "$(uname -s)" == "Linux" ]]; then
  if systemctl --user is-active claude-telegram-mirror &>/dev/null; then
    ok "systemd service: RUNNING"

    # Get PID
    pid=$(systemctl --user show claude-telegram-mirror --property=MainPID --value 2>/dev/null)
    if [[ -n "$pid" && "$pid" != "0" ]]; then
      info "PID: $pid"
    fi
  elif systemctl --user is-enabled claude-telegram-mirror &>/dev/null; then
    warn "systemd service: ENABLED but NOT RUNNING"
    info "Start with: systemctl --user start claude-telegram-mirror"
  else
    info "systemd service: NOT INSTALLED"
    info "Install with: ctm service install"
  fi

  # Check linger (required for user services to run after logout)
  if loginctl show-user "$USER" 2>/dev/null | grep -q "Linger=yes"; then
    ok "loginctl linger: ENABLED"
  else
    warn "loginctl linger: DISABLED"
    info "Service won't run when logged out"
    info "Enable with: loginctl enable-linger $USER"
  fi

elif [[ "$(uname -s)" == "Darwin" ]]; then
  if launchctl list 2>/dev/null | grep -q "claude-telegram-mirror"; then
    ok "launchd agent: LOADED"
  else
    info "launchd agent: NOT LOADED"
    info "Install with: ctm service install"
  fi

  # Check for log files
  if ls ~/Library/Logs/claude-telegram-mirror.*.log &>/dev/null; then
    ok "Log files exist in ~/Library/Logs/"
  fi
fi

echo ""

# ============================================
# 6. RUNTIME STATUS
# ============================================

echo -e "${BOLD}Runtime:${NC}"

# Check socket
if [[ -S "$SOCKET_PATH" ]]; then
  ok "Bridge socket: EXISTS"
  info "Daemon is running"
else
  info "Bridge socket: NOT PRESENT"
  info "Daemon is not running (start with: ctm start)"
fi

# Check PID file
PID_FILE="$CONFIG_DIR/daemon.pid"
if [[ -f "$PID_FILE" ]]; then
  daemon_pid=$(cat "$PID_FILE")
  if kill -0 "$daemon_pid" 2>/dev/null; then
    ok "Daemon process: RUNNING (PID $daemon_pid)"
  else
    warn "Stale PID file (process $daemon_pid not running)"
  fi
fi

# Check sessions database
if [[ -f "$DB_PATH" ]]; then
  if command -v sqlite3 &>/dev/null; then
    active_sessions=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM sessions WHERE status='active'" 2>/dev/null || echo "?")
    total_sessions=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM sessions" 2>/dev/null || echo "?")
    ok "Sessions DB: $active_sessions active / $total_sessions total"
  else
    ok "Sessions DB: EXISTS"
    info "(install sqlite3 for details)"
  fi
else
  info "Sessions DB: NOT CREATED YET"
fi

# Check tmux sessions
if command -v tmux &>/dev/null; then
  tmux_sessions=$(tmux list-sessions 2>/dev/null | wc -l)
  if [[ $tmux_sessions -gt 0 ]]; then
    ok "tmux sessions: $tmux_sessions active"
  else
    info "tmux sessions: NONE"
  fi
fi

echo ""

# ============================================
# 7. TELEGRAM API CONNECTIVITY
# ============================================

echo -e "${BOLD}Telegram API:${NC}"

if [[ -n "$TELEGRAM_BOT_TOKEN" ]]; then
  RESP=$(curl -s --max-time 5 "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/getMe")

  if echo "$RESP" | jq -e '.ok == true' &>/dev/null; then
    BOT_USER=$(echo "$RESP" | jq -r '.result.username')
    ok "Bot connected: @$BOT_USER"

    # Test posting permission (if chat ID is set)
    if [[ -n "$TELEGRAM_CHAT_ID" ]]; then
      # Just check if we can get chat info (doesn't spam the chat)
      CHAT_RESP=$(curl -s --max-time 5 "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/getChat?chat_id=${TELEGRAM_CHAT_ID}")

      if echo "$CHAT_RESP" | jq -e '.ok == true' &>/dev/null; then
        CHAT_TITLE=$(echo "$CHAT_RESP" | jq -r '.result.title // "Private chat"')
        ok "Chat accessible: $CHAT_TITLE"
      else
        CHAT_ERR=$(echo "$CHAT_RESP" | jq -r '.description // "Unknown error"')
        fail "Chat error: $CHAT_ERR"
        info "Make sure bot is added to the group"
      fi
    fi
  else
    ERR=$(echo "$RESP" | jq -r '.description // "Connection failed"')
    fail "API Error: $ERR"

    if [[ "$ERR" == *"Unauthorized"* ]]; then
      info "Token may be invalid - check TELEGRAM_BOT_TOKEN"
    fi
  fi
else
  warn "Cannot test (no token configured)"
fi

echo ""

# ============================================
# 8. RECENT LOGS
# ============================================

echo -e "${BOLD}Recent Logs:${NC}"

if [[ -f "$LOG_FILE" ]]; then
  log_size=$(wc -l < "$LOG_FILE" 2>/dev/null || echo 0)
  info "Hook debug log: $log_size lines"
  echo "  (last 5 entries):"
  tail -5 "$LOG_FILE" 2>/dev/null | sed 's/^/    /'
else
  info "No hook debug log"
  info "Enable with: export TELEGRAM_HOOK_DEBUG=1"
fi

echo ""

# Service logs
if [[ "$(uname -s)" == "Linux" ]]; then
  if systemctl --user is-active claude-telegram-mirror &>/dev/null; then
    echo "  (last 5 journal entries):"
    journalctl --user -u claude-telegram-mirror -n 5 --no-pager 2>/dev/null | sed 's/^/    /'
    echo ""
  fi
elif [[ "$(uname -s)" == "Darwin" ]]; then
  latest_log=$(ls -t ~/Library/Logs/claude-telegram-mirror.*.log 2>/dev/null | head -1)
  if [[ -n "$latest_log" ]]; then
    echo "  (last 5 lines from $latest_log):"
    tail -5 "$latest_log" 2>/dev/null | sed 's/^/    /'
    echo ""
  fi
fi

# ============================================
# 9. SUMMARY
# ============================================

echo "==========================================="

if [[ $ERRORS -eq 0 && $WARNINGS -eq 0 ]]; then
  echo -e "${GREEN}All checks passed!${NC}"
elif [[ $ERRORS -eq 0 ]]; then
  echo -e "${YELLOW}$WARNINGS warning(s), no errors${NC}"
else
  echo -e "${RED}$ERRORS error(s), $WARNINGS warning(s)${NC}"
fi

echo ""
echo "Commands:"
echo "  ctm status           # Daemon status"
echo "  ctm service status   # Service status"
echo "  ctm config --test    # Test Telegram connection"
echo ""

exit $ERRORS
