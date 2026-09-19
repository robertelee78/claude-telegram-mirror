#!/bin/sh
# ctm installer — ADR-017.
#
#   curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/install.sh | sh
#
# Installs the latest ctm release for this machine into ~/.local/bin (override with
# CTM_INSTALL_DIR). Fetches the per-target release record via GitHub's
# releases/latest/download redirect, downloads the matching binary from the same
# release, verifies size and SHA-256, then installs it atomically. Never edits your
# shell profile; prints the PATH line if ~/.local/bin is not already on PATH.
#
# POSIX sh only (no bashisms) so it runs under dash, macOS sh, and busybox.
set -eu

REPO="robertelee78/claude-telegram-mirror"
# CTM_RELEASE_BASE exists for the installer's own end-to-end test against a local
# stand-in server; the origin check below still applies to redirects from it.
BASE="${CTM_RELEASE_BASE:-https://github.com/${REPO}/releases}"
INSTALL_DIR="${CTM_INSTALL_DIR:-$HOME/.local/bin}"
MARKER=".ctm-channel"
# TLS is mandatory except against an explicit local test base.
case "$BASE" in
  https://*) CURL_PROTO="--proto =https --tlsv1.2" ;;
  http://127.0.0.1:*|http://localhost:*) CURL_PROTO="--proto =http" ;;
  *) printf 'ctm install: CTM_RELEASE_BASE must be https:// (or a loopback http:// test server)\n' >&2; exit 1 ;;
esac

say()  { printf '%s\n' "$*"; }
fail() { printf 'ctm install: %s\n' "$*" >&2; exit 1; }

# --- target triple -----------------------------------------------------------
os=$(uname -s) ; arch=$(uname -m)
case "$os" in
  Darwin) os_t="apple-darwin" ;;
  Linux)  os_t="unknown-linux-gnu" ;;
  *)      fail "unsupported OS: $os (ctm supports macOS and Linux)" ;;
esac
case "$arch" in
  arm64|aarch64) arch_t="aarch64" ;;
  x86_64|amd64)  arch_t="x86_64" ;;
  *)             fail "unsupported architecture: $arch" ;;
esac
TARGET="${arch_t}-${os_t}"
ASSET="ctm-${TARGET}"
RECORD="stable-${TARGET}.json"

# --- tools -------------------------------------------------------------------
command -v curl >/dev/null 2>&1 || fail "curl is required"
if command -v sha256sum >/dev/null 2>&1; then
  sha() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  fail "need sha256sum or shasum to verify the download"
fi

# --- release record ------------------------------------------------------------
# One small JSON per target on every release: {"kind","schema_version","package",
# "channel","target","version","size","sha256"}. `latest/download` 302s to the
# newest release's copy, so this line is the only "what is current" lookup.
tmp=$(mktemp -d "${TMPDIR:-/tmp}/ctm-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
curl -fsSL $CURL_PROTO --max-filesize 4096 \
  -o "$tmp/record.json" "${BASE}/latest/download/${RECORD}" \
  || fail "could not fetch release record ${RECORD} (no release for ${TARGET} yet?)"

# Minimal JSON field extraction without jq: values are simple scalars.
field() { sed -n "s/.*\"$1\":[[:space:]]*\"\{0,1\}\([^\",}]*\)\"\{0,1\}.*/\1/p" "$tmp/record.json" | head -1; }
kind=$(field kind); pkg=$(field package); rtarget=$(field target)
version=$(field version); size=$(field size); sha256=$(field sha256)
[ "$kind" = "ctm.standalone-release" ] && [ "$pkg" = "ctm" ] && [ "$rtarget" = "$TARGET" ] \
  || fail "release record identity mismatch (kind=$kind package=$pkg target=$rtarget)"
[ -n "$version" ] && [ -n "$size" ] && [ -n "$sha256" ] || fail "release record incomplete"
say "ctm ${version} for ${TARGET}"

# --- binary --------------------------------------------------------------------
# Pin to the exact release the record came from (not `latest`, which could move
# between the two requests), and refuse if the download leaves GitHub's origins.
url="${BASE}/download/v${version}/${ASSET}"
curl -fsSL $CURL_PROTO --max-filesize "$size" \
  -w '%{url_effective}\n' -o "$tmp/$ASSET" "$url" >"$tmp/final_url" \
  || fail "download failed: $url"
final=$(cat "$tmp/final_url")
case "$final" in
  https://github.com/*|https://release-assets.githubusercontent.com/*|https://objects.githubusercontent.com/*) ;;
  "$BASE"/*) ;;  # the configured base itself (test stand-in)
  *) fail "download redirected off GitHub: $final" ;;
esac
got_size=$(wc -c <"$tmp/$ASSET" | tr -d ' ')
[ "$got_size" = "$size" ] || fail "size mismatch: expected $size, got $got_size"
got_sha=$(sha "$tmp/$ASSET")
[ "$got_sha" = "$sha256" ] || fail "sha256 mismatch: expected $sha256, got $got_sha"
chmod 0755 "$tmp/$ASSET"

# --- install (atomic) -----------------------------------------------------------
mkdir -p "$INSTALL_DIR"
if [ -e "$INSTALL_DIR/ctm" ] && [ ! -e "$INSTALL_DIR/$MARKER" ]; then
  fail "$INSTALL_DIR/ctm exists but was not installed by this installer; remove it or set CTM_INSTALL_DIR"
fi
# Same filesystem as the destination so the final rename is atomic.
cp "$tmp/$ASSET" "$INSTALL_DIR/.ctm-candidate.partial"
chmod 0755 "$INSTALL_DIR/.ctm-candidate.partial"
if [ -e "$INSTALL_DIR/ctm" ]; then
  cp -p "$INSTALL_DIR/ctm" "$INSTALL_DIR/.ctm-previous"
fi
printf 'standalone\n' >"$INSTALL_DIR/$MARKER"
mv -f "$INSTALL_DIR/.ctm-candidate.partial" "$INSTALL_DIR/ctm"

installed=$("$INSTALL_DIR/ctm" --version 2>/dev/null || true)
[ -n "$installed" ] || fail "installed binary did not run (see: codesign -dv $INSTALL_DIR/ctm)"
say "installed: $INSTALL_DIR/ctm ($installed)"

# --- shell integration (PATH first, tab completion) ------------------------------
# Done by the installed binary so the rc block records the real install path. It
# appends ONE marker-delimited block at the end of your shell's rc (so it wins over
# version-manager shims that prepend earlier) and writes completion files.
# Idempotent; `ctm shell-setup --remove` undoes it; CTM_NO_SHELL_SETUP=1 skips it.
if [ -z "${CTM_NO_SHELL_SETUP:-}" ]; then
  "$INSTALL_DIR/ctm" shell-setup </dev/null || say "warning: shell setup reported a problem (ctm itself is installed)"
fi
say ""
if [ -f "$HOME/.config/claude-telegram-mirror/config.json" ]; then
  say "existing configuration found — reconcile the service and hooks to this binary:"
  say "  ctm doctor --fix"
else
  say "next:"
  say "  ctm setup"
fi
