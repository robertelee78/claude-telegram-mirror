#!/bin/bash
#
# Claude Telegram Mirror - Interactive Installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/scripts/install.sh | bash
#
# Or run locally:
#   ./scripts/install.sh
#
# Supports Linux and macOS
#

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# Configuration
REPO_URL="https://github.com/robertelee78/claude-telegram-mirror.git"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/share/claude-telegram-mirror}"
CONFIG_FILE="$HOME/.telegram-env"

# State variables (filled during installation)
BOT_TOKEN=""
BOT_USERNAME=""
CHAT_ID=""

# ============================================
# TTY HANDLING FOR curl | bash
# ============================================
# When run via "curl ... | bash", stdin is the curl output (the script itself).
# We need to read user input from /dev/tty instead.
#
# This is the standard pattern used by rustup, nvm, and other installers.

# Check if we have a TTY available for interactive input
if [ -t 0 ]; then
  # stdin is a terminal - normal execution
  TTY_INPUT="/dev/stdin"
else
  # stdin is NOT a terminal (likely piped from curl)
  # Try to use /dev/tty for interactive input
  # We need to verify /dev/tty actually works, not just exists
  if [ -e /dev/tty ] && [ -r /dev/tty ] && [ -w /dev/tty ]; then
    # Test that we can actually read from it
    if (echo "" > /dev/tty) 2>/dev/null; then
      TTY_INPUT="/dev/tty"
    else
      echo ""
      echo -e "${RED}ERROR: This installer requires interactive input.${NC}"
      echo ""
      echo "  You're running via 'curl | bash' but /dev/tty is not usable."
      echo ""
      echo "  Options:"
      echo "    1. Download and run the script directly:"
      echo "       curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/scripts/install.sh -o install.sh"
      echo "       bash install.sh"
      echo ""
      echo "    2. If in a Docker container, run with -it flags:"
      echo "       docker run -it ..."
      echo ""
      exit 1
    fi
  else
    echo ""
    echo -e "${RED}ERROR: This installer requires interactive input.${NC}"
    echo ""
    echo "  You're running via 'curl | bash' but /dev/tty is not available."
    echo ""
    echo "  Options:"
    echo "    1. Download and run the script directly:"
    echo "       curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/scripts/install.sh -o install.sh"
    echo "       bash install.sh"
    echo ""
    echo "    2. If in a Docker container, run with -it flags:"
    echo "       docker run -it ..."
    echo ""
    exit 1
  fi
fi

# Wrapper function for read that uses the correct input source
# Usage: prompt_read "prompt text" VARIABLE_NAME
prompt_read() {
  local prompt="$1"
  local varname="$2"
  local result

  # Print prompt to stderr (so it shows regardless of stdin redirection)
  # Use -n to avoid newline after prompt
  echo -n "$prompt" >&2

  # Read from TTY - use || true to prevent set -e from killing us
  # The -r flag prevents backslash interpretation
  if ! read -r result < "$TTY_INPUT"; then
    echo "" >&2
    echo -e "${RED}ERROR: Failed to read input from terminal${NC}" >&2
    echo "  TTY_INPUT=$TTY_INPUT" >&2
    exit 1
  fi

  # Assign to the named variable
  eval "$varname=\$result"
}

# Wrapper for simple "press enter to continue" prompts
prompt_continue() {
  local prompt="${1:-Press Enter to continue...}"
  local dummy

  echo -n "$prompt" >&2

  if ! read -r dummy < "$TTY_INPUT"; then
    echo "" >&2
    echo -e "${RED}ERROR: Failed to read input from terminal${NC}" >&2
    exit 1
  fi
}

# ============================================
# UTILITY FUNCTIONS
# ============================================

info() {
  echo -e "${BLUE}ℹ${NC} $1"
}

success() {
  echo -e "${GREEN}✓${NC} $1"
}

warn() {
  echo -e "${YELLOW}⚠${NC} $1"
}

error() {
  echo -e "${RED}✗${NC} $1"
}

header() {
  echo ""
  echo -e "${BOLD}═══════════════════════════════════════════════${NC}"
  echo -e "${BOLD}  $1${NC}"
  echo -e "${BOLD}═══════════════════════════════════════════════${NC}"
  echo ""
}

# ============================================
# STEP 1: PREREQUISITES CHECK
# ============================================

check_prerequisites() {
  header "STEP 1: CHECKING PREREQUISITES"

  local missing=()
  local os_type=$(uname -s)

  # Check each required tool
  for cmd in node npm git jq tmux nc curl; do
    if command -v "$cmd" &> /dev/null; then
      if [[ "$cmd" == "node" ]]; then
        local node_version=$(node --version | sed 's/v//' | cut -d. -f1)
        if [[ $node_version -ge 18 ]]; then
          success "$cmd $(node --version)"
        else
          error "$cmd $(node --version) - Need v18+"
          missing+=("node")
        fi
      else
        success "$cmd"
      fi
    else
      error "$cmd not found"
      missing+=("$cmd")
    fi
  done

  # If anything missing, show install instructions
  if [[ ${#missing[@]} -gt 0 ]]; then
    echo ""
    warn "Missing prerequisites: ${missing[*]}"
    echo ""

    if [[ "$os_type" == "Darwin" ]]; then
      echo "  Install with Homebrew:"
      echo "    brew install ${missing[*]}"
    else
      echo "  Install on Debian/Ubuntu:"
      echo "    sudo apt update && sudo apt install -y ${missing[*]}"
      echo ""
      echo "  Install on Fedora/RHEL:"
      echo "    sudo dnf install -y ${missing[*]}"
    fi

    if [[ " ${missing[*]} " =~ " node " ]]; then
      echo ""
      echo "  For Node.js 18+, consider using nvm:"
      echo "    curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.0/install.sh | bash"
      echo "    nvm install 18"
    fi

    echo ""
    error "Please install missing prerequisites and run again."
    exit 1
  fi

  success "All prerequisites met!"
}

# ============================================
# STEP 2: BOT TOKEN
# ============================================

get_bot_token() {
  header "STEP 2: CREATE TELEGRAM BOT"

  echo "  You need to create a Telegram bot via @BotFather."
  echo ""
  echo "  1. Open Telegram and search for @BotFather"
  echo "  2. Send /newbot"
  echo "  3. Choose a name (e.g., 'Claude Mirror')"
  echo "  4. Choose a username (must end in 'bot', e.g., 'claude_mirror_bot')"
  echo "  5. Copy the API token"
  echo ""

  while true; do
    prompt_read "Enter your bot token: " BOT_TOKEN

    if [[ -z "$BOT_TOKEN" ]]; then
      warn "Token cannot be empty"
      continue
    fi

    # Validate token format (should be like: 123456789:ABCdefGHIjklMNOpqrsTUVwxyz)
    if [[ ! "$BOT_TOKEN" =~ ^[0-9]+:[A-Za-z0-9_-]+$ ]]; then
      warn "Token format looks incorrect. Expected format: 123456789:ABCdefGHI..."
      prompt_read "Try again? [Y/n]: " retry
      [[ "$retry" =~ ^[Nn] ]] && exit 1
      continue
    fi

    # Verify token with Telegram API
    info "Verifying token with Telegram..."
    local response=$(curl -s "https://api.telegram.org/bot${BOT_TOKEN}/getMe")

    if echo "$response" | jq -e '.ok == true' &> /dev/null; then
      BOT_USERNAME=$(echo "$response" | jq -r '.result.username')
      success "Bot verified: @${BOT_USERNAME}"
      break
    else
      local error_msg=$(echo "$response" | jq -r '.description // "Unknown error"')
      error "Token validation failed: $error_msg"
      prompt_read "Try again? [Y/n]: " retry
      [[ "$retry" =~ ^[Nn] ]] && exit 1
    fi
  done
}

# ============================================
# STEP 3: DISABLE PRIVACY MODE
# ============================================

configure_privacy_mode() {
  header "STEP 3: DISABLE PRIVACY MODE"

  echo "  Your bot needs to see all group messages (not just commands)."
  echo ""
  echo "  1. Go back to @BotFather in Telegram"
  echo "  2. Send /mybots"
  echo "  3. Select @${BOT_USERNAME}"
  echo "  4. Click 'Bot Settings'"
  echo "  5. Click 'Group Privacy'"
  echo "  6. Click 'Turn off'"
  echo ""
  echo "  You should see: 'Privacy mode is disabled for @${BOT_USERNAME}'"
  echo ""

  while true; do
    prompt_read "Have you disabled privacy mode? [y/n]: " confirmed

    if [[ "$confirmed" =~ ^[Yy] ]]; then
      success "Privacy mode configured"
      break
    elif [[ "$confirmed" =~ ^[Nn] ]]; then
      warn "Privacy mode MUST be disabled for the bot to work."
      echo "  Please complete this step before continuing."
      prompt_read "Try again? [Y/n]: " retry
      [[ "$retry" =~ ^[Nn] ]] && exit 1
    else
      warn "Please enter 'y' or 'n'"
    fi
  done
}

# ============================================
# STEP 4: CREATE SUPERGROUP WITH TOPICS
# ============================================

setup_telegram_group() {
  header "STEP 4: CREATE SUPERGROUP WITH TOPICS"

  echo "  Create a Telegram supergroup with Topics enabled."
  echo ""
  echo "  1. In Telegram, create a new group"
  echo "  2. Add @${BOT_USERNAME} to the group"
  echo "  3. Go to group settings"
  echo "  4. Enable 'Topics' (this converts it to a supergroup)"
  echo "  5. Make the bot an admin with 'Manage Topics' permission"
  echo "  6. Send any message in the group (so we can detect it)"
  echo ""

  prompt_continue "Press Enter when you've completed these steps..."

  # Try to auto-detect the chat ID
  info "Looking for your group..."

  local updates=$(curl -s "https://api.telegram.org/bot${BOT_TOKEN}/getUpdates?limit=100")

  if ! echo "$updates" | jq -e '.ok == true' &> /dev/null; then
    error "Failed to get updates from Telegram API"
    manual_chat_id_entry
    return
  fi

  # Extract unique supergroups (chat_id starting with -100)
  local groups=$(echo "$updates" | jq -r '
    .result[]
    | .message.chat // .my_chat_member.chat // empty
    | select(.id < 0)
    | select(.id | tostring | startswith("-100"))
    | "\(.id)|\(.title // "Unknown")"
  ' | sort -u)

  if [[ -z "$groups" ]]; then
    warn "No supergroups found. This can happen if:"
    echo "  - The bot hasn't seen any messages yet"
    echo "  - The group wasn't converted to a supergroup (enable Topics!)"
    echo ""
    manual_chat_id_entry
    return
  fi

  # Count groups
  local group_count=$(echo "$groups" | wc -l | tr -d ' ')

  if [[ $group_count -eq 1 ]]; then
    # Only one group found
    CHAT_ID=$(echo "$groups" | cut -d'|' -f1)
    local group_title=$(echo "$groups" | cut -d'|' -f2)
    success "Found group: $group_title ($CHAT_ID)"

    prompt_read "Is this the correct group? [Y/n]: " confirm
    if [[ "$confirm" =~ ^[Nn] ]]; then
      manual_chat_id_entry
      return
    fi
  else
    # Multiple groups found
    echo ""
    echo "  Found multiple groups:"
    echo ""

    local i=1
    declare -a group_ids
    declare -a group_titles

    while IFS='|' read -r gid gtitle; do
      echo "    $i) $gtitle ($gid)"
      group_ids+=("$gid")
      group_titles+=("$gtitle")
      ((i++))
    done <<< "$groups"

    echo ""
    prompt_read "Select group number (1-$group_count): " selection

    if [[ "$selection" =~ ^[0-9]+$ ]] && [[ $selection -ge 1 ]] && [[ $selection -le $group_count ]]; then
      CHAT_ID="${group_ids[$((selection-1))]}"
      success "Selected: ${group_titles[$((selection-1))]} ($CHAT_ID)"
    else
      warn "Invalid selection"
      manual_chat_id_entry
      return
    fi
  fi
}

manual_chat_id_entry() {
  echo ""
  echo "  Enter the chat ID manually."
  echo "  You can find it by:"
  echo "    1. Send a message in the group"
  echo "    2. Visit: https://api.telegram.org/bot${BOT_TOKEN}/getUpdates"
  echo "    3. Look for 'chat':{'id': -100XXXXXXXXXX}"
  echo ""

  while true; do
    prompt_read "Enter chat ID (starts with -100): " CHAT_ID

    if [[ "$CHAT_ID" =~ ^-100[0-9]+$ ]]; then
      success "Chat ID format valid"
      break
    else
      warn "Chat ID should start with -100 (supergroup format)"
      prompt_read "Try again? [Y/n]: " retry
      [[ "$retry" =~ ^[Nn] ]] && exit 1
    fi
  done
}

# ============================================
# STEP 5: VERIFY BOT CAN POST
# ============================================

verify_bot_permissions() {
  header "STEP 5: VERIFY BOT PERMISSIONS"

  info "Testing if bot can post to the group..."

  local response=$(curl -s -X POST "https://api.telegram.org/bot${BOT_TOKEN}/sendMessage" \
    -H "Content-Type: application/json" \
    -d "{\"chat_id\": \"${CHAT_ID}\", \"text\": \"🤖 Claude Telegram Mirror - Installation test successful!\"}")

  if echo "$response" | jq -e '.ok == true' &> /dev/null; then
    success "Bot can post to the group!"
    echo ""
    echo "  Check your Telegram group - you should see a test message."
    prompt_continue "Press Enter to continue..."
  else
    local error_msg=$(echo "$response" | jq -r '.description // "Unknown error"')
    error "Bot cannot post: $error_msg"
    echo ""
    echo "  Common fixes:"
    echo "  - Make sure the bot is an admin in the group"
    echo "  - Ensure 'Post Messages' permission is enabled"
    echo "  - Check that 'Manage Topics' permission is enabled"
    echo ""
    prompt_continue "Fix the issue and press Enter to retry, or Ctrl+C to exit..."
    verify_bot_permissions
  fi
}

# ============================================
# STEP 6: INSTALL PACKAGE
# ============================================

install_package() {
  header "STEP 6: INSTALL PACKAGE"

  # Check if already installed
  if [[ -d "$INSTALL_DIR" ]]; then
    warn "Installation directory already exists: $INSTALL_DIR"
    prompt_read "Remove and reinstall? [y/N]: " reinstall
    if [[ "$reinstall" =~ ^[Yy] ]]; then
      info "Removing existing installation..."
      rm -rf "$INSTALL_DIR"
    else
      info "Keeping existing installation, updating..."
      cd "$INSTALL_DIR"
      git pull
      npm install
      npm run build
      success "Package updated"
      return
    fi
  fi

  # Clone repository
  info "Cloning repository..."
  mkdir -p "$(dirname "$INSTALL_DIR")"
  git clone "$REPO_URL" "$INSTALL_DIR"

  # Install dependencies
  cd "$INSTALL_DIR"
  info "Installing dependencies..."
  npm install

  # Build
  info "Building..."
  npm run build

  success "Package installed to $INSTALL_DIR"

  # Create ctm symlink
  local bin_dir="$HOME/.local/bin"
  mkdir -p "$bin_dir"

  if [[ ! -f "$bin_dir/ctm" ]]; then
    cat > "$bin_dir/ctm" << EOF
#!/bin/bash
node "$INSTALL_DIR/dist/cli.js" "\$@"
EOF
    chmod +x "$bin_dir/ctm"
    success "Created 'ctm' command in $bin_dir"

    # Check if bin_dir is in PATH
    if [[ ":$PATH:" != *":$bin_dir:"* ]]; then
      warn "$bin_dir is not in your PATH"
      echo "  Add this to your ~/.bashrc or ~/.zshrc:"
      echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
    fi
  fi
}

# ============================================
# STEP 7: CREATE CONFIG
# ============================================

create_config() {
  header "STEP 7: CREATE CONFIGURATION"

  info "Creating $CONFIG_FILE..."

  cat > "$CONFIG_FILE" << EOF
# Claude Telegram Mirror Configuration
# Generated by install.sh on $(date)

export TELEGRAM_BOT_TOKEN="$BOT_TOKEN"
export TELEGRAM_CHAT_ID="$CHAT_ID"
export TELEGRAM_MIRROR=true

# Optional settings:
# export TELEGRAM_MIRROR_VERBOSE=true
# export TELEGRAM_BRIDGE_SOCKET=~/.config/claude-telegram-mirror/bridge.sock
EOF

  chmod 600 "$CONFIG_FILE"
  success "Configuration saved to $CONFIG_FILE"

  # Add to shell profile
  local shell_profile=""
  if [[ -f "$HOME/.bashrc" ]]; then
    shell_profile="$HOME/.bashrc"
  elif [[ -f "$HOME/.zshrc" ]]; then
    shell_profile="$HOME/.zshrc"
  elif [[ -f "$HOME/.profile" ]]; then
    shell_profile="$HOME/.profile"
  fi

  if [[ -n "$shell_profile" ]]; then
    if ! grep -q "telegram-env" "$shell_profile" 2>/dev/null; then
      echo "" >> "$shell_profile"
      echo "# Claude Telegram Mirror" >> "$shell_profile"
      echo "[[ -f ~/.telegram-env ]] && source ~/.telegram-env" >> "$shell_profile"
      success "Added to $shell_profile"
    else
      info "Already in $shell_profile"
    fi
  fi

  # Source it now
  source "$CONFIG_FILE"
}

# ============================================
# STEP 8: INSTALL HOOKS
# ============================================

install_hooks() {
  header "STEP 8: INSTALL CLAUDE CODE HOOKS"

  cd "$INSTALL_DIR"

  # Install global hooks
  info "Installing global hooks..."
  node dist/cli.js install-hooks
  success "Global hooks installed to ~/.claude/settings.json"

  # CRITICAL: Warn about project-level settings
  echo ""
  echo "┌─────────────────────────────────────────────────────────────┐"
  echo "│  ⚠️  IMPORTANT: PROJECT-LEVEL HOOKS                         │"
  echo "├─────────────────────────────────────────────────────────────┤"
  echo "│                                                             │"
  echo "│  If you use Claude Code in projects that have their own    │"
  echo "│  .claude/settings.json file, the GLOBAL hooks we just      │"
  echo "│  installed will be IGNORED in those projects.              │"
  echo "│                                                             │"
  echo "│  To enable Telegram mirroring in a specific project:       │"
  echo "│                                                             │"
  echo "│    cd /path/to/your/project                                │"
  echo "│    ctm install-hooks --project                             │"
  echo "│                                                             │"
  echo "│  This adds hooks to that project's .claude/settings.json   │"
  echo "│                                                             │"
  echo "└─────────────────────────────────────────────────────────────┘"
  echo ""

  # Ask if they want to install to a project now
  prompt_read "Do you have a project with .claude/settings.json that needs hooks? [y/N]: " HAS_PROJECT

  if [[ "$HAS_PROJECT" =~ ^[Yy] ]]; then
    install_project_hooks
  fi
}

install_project_hooks() {
  while true; do
    echo ""
    prompt_read "Enter project path (or 'done' to finish): " PROJECT_PATH

    [[ "$PROJECT_PATH" == "done" || -z "$PROJECT_PATH" ]] && break

    # Expand ~ if present
    PROJECT_PATH="${PROJECT_PATH/#\~/$HOME}"

    # Check if it's a valid project with .claude/
    if [[ -d "$PROJECT_PATH/.claude" ]]; then
      info "Installing hooks to $PROJECT_PATH..."

      # Save current dir, go to project, install, come back
      pushd "$PROJECT_PATH" > /dev/null
      node "$INSTALL_DIR/dist/cli.js" install-hooks --project
      popd > /dev/null

      success "Hooks installed to $PROJECT_PATH/.claude/settings.json"
    elif [[ -d "$PROJECT_PATH" ]]; then
      warn "No .claude/ directory in $PROJECT_PATH"
      echo "  This project doesn't have custom Claude settings."
      echo "  Global hooks will work here - no action needed!"
    else
      warn "Directory not found: $PROJECT_PATH"
    fi

    prompt_read "Add another project? [y/N]: " ANOTHER
    [[ "$ANOTHER" =~ ^[Yy] ]] || break
  done
}

# ============================================
# STEP 9: INSTALL SERVICE
# ============================================

install_service() {
  header "STEP 9: INSTALL SYSTEM SERVICE"

  cd "$INSTALL_DIR"

  local os_type=$(uname -s)

  info "Installing service..."
  node dist/cli.js service install

  info "Starting service..."
  node dist/cli.js service start

  # Verify service is running
  sleep 2

  if node dist/cli.js service status 2>/dev/null | grep -q "running\|active"; then
    success "Service is running!"
  else
    warn "Service may not have started correctly"

    if [[ "$os_type" == "Linux" ]]; then
      echo "  Check status: systemctl --user status claude-telegram-mirror"
      echo "  View logs: journalctl --user -u claude-telegram-mirror -f"
      echo ""
      echo "  You may need to enable user lingering:"
      echo "    loginctl enable-linger $USER"
    else
      echo "  Check status: launchctl list | grep claude"
      echo "  View logs: cat ~/Library/Logs/claude-telegram-mirror.*.log"
    fi
  fi
}

# ============================================
# STEP 10: COMPLETION
# ============================================

show_completion() {
  echo ""
  echo "╔═════════════════════════════════════════════╗"
  echo "║   ✅ INSTALLATION COMPLETE                  ║"
  echo "╚═════════════════════════════════════════════╝"
  echo ""
  echo "  Bot: @${BOT_USERNAME}"
  echo "  Chat: ${CHAT_ID}"
  echo "  Config: ~/.telegram-env"
  echo "  Install: ${INSTALL_DIR}"
  echo ""
  echo "  Commands:"
  echo "    ctm start            # Start daemon (foreground)"
  echo "    ctm status           # Show status"
  echo "    ctm service status   # Service status"
  echo ""

  local os_type=$(uname -s)
  if [[ "$os_type" == "Linux" ]]; then
    echo "  Logs: journalctl --user -u claude-telegram-mirror -f"
  else
    echo "  Logs: cat ~/Library/Logs/claude-telegram-mirror.*.log"
  fi

  echo ""
  echo "  ┌─────────────────────────────────────────────────────────┐"
  echo "  │  📌 REMEMBER: Project-specific hooks                    │"
  echo "  │                                                         │"
  echo "  │  For projects with .claude/settings.json:              │"
  echo "  │    cd /path/to/project && ctm install-hooks -p         │"
  echo "  └─────────────────────────────────────────────────────────┘"
  echo ""
  echo "  Next steps:"
  echo "    1. Run 'source ~/.bashrc' or restart terminal"
  echo "    2. Start a Claude Code session in tmux:"
  echo "       tmux new -s claude"
  echo "       claude"
  echo ""
  echo "  Your Claude sessions will now be mirrored to Telegram!"
  echo ""
}

# ============================================
# CLEANUP ON ERROR
# ============================================

cleanup_on_error() {
  echo ""
  error "Installation failed!"
  echo ""
  echo "  Partial installation may exist at:"
  echo "    $INSTALL_DIR"
  echo "    $CONFIG_FILE"
  echo ""
  echo "  To clean up and retry:"
  echo "    rm -rf $INSTALL_DIR"
  echo "    rm -f $CONFIG_FILE"
  echo ""
  exit 1
}

# ============================================
# NONINTERACTIVE MODE (existing config)
# ============================================

check_existing_config() {
  if [[ -f "$CONFIG_FILE" ]]; then
    source "$CONFIG_FILE"

    if [[ -n "$TELEGRAM_BOT_TOKEN" && -n "$TELEGRAM_CHAT_ID" ]]; then
      echo ""
      info "Found existing configuration at $CONFIG_FILE"

      # Validate existing token
      local response=$(curl -s "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/getMe")
      if echo "$response" | jq -e '.ok == true' &> /dev/null; then
        BOT_USERNAME=$(echo "$response" | jq -r '.result.username')
        BOT_TOKEN="$TELEGRAM_BOT_TOKEN"
        CHAT_ID="$TELEGRAM_CHAT_ID"

        echo "  Bot: @${BOT_USERNAME}"
        echo "  Chat: ${CHAT_ID}"
        echo ""

        prompt_read "Use existing configuration? [Y/n]: " use_existing
        if [[ ! "$use_existing" =~ ^[Nn] ]]; then
          return 0  # Use existing
        fi
      else
        warn "Existing token is invalid"
      fi
    fi
  fi

  return 1  # Need new config
}

# ============================================
# MAIN
# ============================================

main() {
  echo ""
  echo "╔═══════════════════════════════════════════════════════════════╗"
  echo "║                                                               ║"
  echo "║   🤖 Claude Telegram Mirror Installer                        ║"
  echo "║                                                               ║"
  echo "║   Mirror your Claude Code sessions to Telegram               ║"
  echo "║   Control Claude from your phone!                            ║"
  echo "║                                                               ║"
  echo "╚═══════════════════════════════════════════════════════════════╝"
  echo ""

  # Set up error handler
  trap cleanup_on_error ERR

  # Step 1: Prerequisites
  check_prerequisites

  # Check for existing config
  if check_existing_config; then
    # Skip Telegram setup, go straight to install
    install_package
    install_hooks
    install_service
    show_completion
    exit 0
  fi

  # Steps 2-5: Telegram setup
  get_bot_token
  configure_privacy_mode
  setup_telegram_group
  verify_bot_permissions

  # Steps 6-9: Installation
  install_package
  create_config
  install_hooks
  install_service

  # Step 10: Done!
  show_completion
}

# Run main
main "$@"
