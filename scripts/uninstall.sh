#!/bin/bash
#
# Claude Telegram Mirror - Uninstaller
#
# Usage:
#   ./scripts/uninstall.sh
#   ctm uninstall  (if installed)
#
# Removes all installed components cleanly.
#

# Colors (respect NO_COLOR)
if [[ -z "$NO_COLOR" ]]; then
  RED='\033[0;31m'
  GREEN='\033[0;32m'
  YELLOW='\033[1;33m'
  BLUE='\033[0;34m'
  BOLD='\033[1m'
  NC='\033[0m'
else
  RED=''
  GREEN=''
  YELLOW=''
  BLUE=''
  BOLD=''
  NC=''
fi

# Output helpers
info() { echo -e "${BLUE}ℹ${NC} $1"; }
success() { echo -e "${GREEN}✓${NC} $1"; }
warn() { echo -e "${YELLOW}⚠${NC} $1"; }
error() { echo -e "${RED}✗${NC} $1"; }

# Configuration paths
INSTALL_DIR="${TELEGRAM_MIRROR_INSTALL_DIR:-$HOME/.local/share/claude-telegram-mirror}"
CONFIG_DIR="$HOME/.config/claude-telegram-mirror"
ENV_FILE="$HOME/.telegram-env"
CTM_SYMLINK="$HOME/.local/bin/ctm"

# Check for alternate install locations
find_install_dir() {
  for check_dir in "$INSTALL_DIR" "/opt/claude-mobile/packages/claude-telegram-mirror" "$HOME/.local/share/claude-telegram-mirror"; do
    if [[ -d "$check_dir" && -f "$check_dir/package.json" ]]; then
      echo "$check_dir"
      return 0
    fi
  done
  echo "$INSTALL_DIR"
}

INSTALL_DIR=$(find_install_dir)

# ============================================
# CONFIRMATION
# ============================================

echo ""
echo -e "${BOLD}Claude Telegram Mirror Uninstaller${NC}"
echo "==================================="
echo ""
echo "This will remove:"
echo "  • System service (systemd/launchd)"
echo "  • Claude Code hooks"
echo "  • Installed files ($INSTALL_DIR)"
echo "  • Runtime data ($CONFIG_DIR)"
echo "  • CLI symlink ($CTM_SYMLINK)"
echo ""
echo "Configuration (~/.telegram-env) will be preserved unless you choose to remove it."
echo ""

read -p "Continue with uninstallation? [y/N]: " CONFIRM
if [[ ! "$CONFIRM" =~ ^[Yy] ]]; then
  echo "Cancelled."
  exit 0
fi

echo ""

# ============================================
# 1. STOP & REMOVE SERVICE
# ============================================

info "Stopping service..."

if [[ -f "$INSTALL_DIR/dist/cli.js" ]]; then
  # Use CLI for clean uninstall
  node "$INSTALL_DIR/dist/cli.js" service stop 2>/dev/null || true
  node "$INSTALL_DIR/dist/cli.js" service uninstall 2>/dev/null || true
  success "Service stopped via CLI"
else
  # Manual cleanup if CLI not available
  if [[ "$(uname -s)" == "Linux" ]]; then
    systemctl --user stop claude-telegram-mirror 2>/dev/null || true
    systemctl --user disable claude-telegram-mirror 2>/dev/null || true
    rm -f "$HOME/.config/systemd/user/claude-telegram-mirror.service"
    systemctl --user daemon-reload 2>/dev/null || true
    success "systemd service removed"
  elif [[ "$(uname -s)" == "Darwin" ]]; then
    launchctl unload "$HOME/Library/LaunchAgents/com.claude.claude-telegram-mirror.plist" 2>/dev/null || true
    rm -f "$HOME/Library/LaunchAgents/com.claude.claude-telegram-mirror.plist"
    success "launchd agent removed"
  fi
fi

# Kill any running daemon processes
if [[ -f "$CONFIG_DIR/daemon.pid" ]]; then
  daemon_pid=$(cat "$CONFIG_DIR/daemon.pid")
  if kill -0 "$daemon_pid" 2>/dev/null; then
    info "Killing daemon process ($daemon_pid)..."
    kill "$daemon_pid" 2>/dev/null || true
    sleep 1
  fi
fi

# ============================================
# 2. REMOVE HOOKS
# ============================================

info "Removing hooks..."

if [[ -f "$INSTALL_DIR/dist/cli.js" ]]; then
  node "$INSTALL_DIR/dist/cli.js" uninstall-hooks 2>/dev/null || true
  success "Hooks removed via CLI"
else
  # Manual hook removal
  SETTINGS="$HOME/.claude/settings.json"
  if [[ -f "$SETTINGS" ]] && command -v jq &>/dev/null; then
    # Create backup
    cp "$SETTINGS" "$SETTINGS.backup.$(date +%s)"

    # Remove telegram-hook entries from all hook types
    jq 'if .hooks then .hooks |= with_entries(
      .value |= map(select(
        if .hooks then (.hooks | all(.command | contains("telegram-hook") | not))
        elif .command then (.command | contains("telegram-hook") | not)
        else true end
      )) | select(length > 0)
    ) | if .hooks == {} then del(.hooks) else . end else . end' "$SETTINGS" > "$SETTINGS.tmp" && mv "$SETTINGS.tmp" "$SETTINGS"
    success "Hooks removed from settings.json"
  elif [[ -f "$SETTINGS" ]]; then
    warn "jq not available - edit ~/.claude/settings.json manually to remove telegram-hook entries"
  fi
fi

# ============================================
# 3. REMOVE FILES
# ============================================

info "Removing files..."

# Remove installation directory
if [[ -d "$INSTALL_DIR" ]]; then
  rm -rf "$INSTALL_DIR"
  success "Removed $INSTALL_DIR"
else
  info "Install dir not found: $INSTALL_DIR"
fi

# Remove config directory (socket, db, logs)
if [[ -d "$CONFIG_DIR" ]]; then
  rm -rf "$CONFIG_DIR"
  success "Removed $CONFIG_DIR"
else
  info "Config dir not found: $CONFIG_DIR"
fi

# Remove CLI symlink
if [[ -L "$CTM_SYMLINK" ]]; then
  rm -f "$CTM_SYMLINK"
  success "Removed ctm symlink"
elif [[ -f "$CTM_SYMLINK" ]]; then
  rm -f "$CTM_SYMLINK"
  success "Removed ctm script"
fi

# Remove macOS logs
if [[ "$(uname -s)" == "Darwin" ]]; then
  rm -f "$HOME/Library/Logs/claude-telegram-mirror."* 2>/dev/null && success "Removed macOS logs"
fi

# ============================================
# 4. OPTIONAL: REMOVE CONFIGURATION
# ============================================

echo ""
read -p "Also remove ~/.telegram-env configuration? [y/N]: " REMOVE_ENV

if [[ "$REMOVE_ENV" =~ ^[Yy] ]]; then
  if [[ -f "$ENV_FILE" ]]; then
    rm -f "$ENV_FILE"
    success "Removed ~/.telegram-env"
  fi

  # Clean shell profile
  info "Cleaning shell profile..."
  for RC in "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.zshrc" "$HOME/.profile"; do
    if [[ -f "$RC" ]] && grep -q "telegram-env" "$RC"; then
      # macOS sed requires backup extension (-i '')
      if [[ "$(uname -s)" == "Darwin" ]]; then
        sed -i '' '/# Claude Telegram Mirror/d' "$RC" 2>/dev/null
        sed -i '' '/telegram-env/d' "$RC" 2>/dev/null
      else
        sed -i '/# Claude Telegram Mirror/d' "$RC" 2>/dev/null
        sed -i '/telegram-env/d' "$RC" 2>/dev/null
      fi
      success "Cleaned $RC"
    fi
  done
else
  info "Kept ~/.telegram-env (you can remove it manually later)"
fi

# ============================================
# 5. CLEAN UP PROJECT-LEVEL HOOKS
# ============================================

echo ""
read -p "Check for project-level hooks to remove? [y/N]: " REMOVE_PROJECT_HOOKS

if [[ "$REMOVE_PROJECT_HOOKS" =~ ^[Yy] ]]; then
  info "Searching for project-level hooks..."

  # Find .claude/settings.json files with telegram-hook
  while IFS= read -r settings_file; do
    if [[ -f "$settings_file" ]] && grep -q "telegram-hook" "$settings_file" 2>/dev/null; then
      project_dir=$(dirname "$(dirname "$settings_file")")
      echo ""
      warn "Found hooks in: $project_dir"
      read -p "  Remove hooks from this project? [y/N]: " remove_this

      if [[ "$remove_this" =~ ^[Yy] ]]; then
        if command -v jq &>/dev/null; then
          cp "$settings_file" "$settings_file.backup.$(date +%s)"
          jq 'if .hooks then .hooks |= with_entries(
            .value |= map(select(
              if .hooks then (.hooks | all(.command | contains("telegram-hook") | not))
              elif .command then (.command | contains("telegram-hook") | not)
              else true end
            )) | select(length > 0)
          ) | if .hooks == {} then del(.hooks) else . end else . end' "$settings_file" > "$settings_file.tmp" && mv "$settings_file.tmp" "$settings_file"
          success "Removed hooks from $project_dir"
        else
          warn "jq not available - edit $settings_file manually"
        fi
      fi
    fi
  done < <(find "$HOME" -maxdepth 5 -path "*/.claude/settings.json" -type f 2>/dev/null)
fi

# ============================================
# 6. COMPLETION
# ============================================

echo ""
echo "==================================="
success "Uninstallation complete!"
echo ""
echo "If you want to reinstall later:"
echo "  curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/scripts/install.sh | bash"
echo ""
