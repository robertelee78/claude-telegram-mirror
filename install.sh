#!/bin/sh
# ctm installer — ADR-017.
#
#   curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/install.sh | sh
#
# Installs the latest ctm release for this machine into ~/.local/bin (override with
# CTM_INSTALL_DIR). Fetches the per-target release record via GitHub's
# releases/latest/download redirect, downloads the matching binary from the same
# release, verifies size and SHA-256, the Ed25519 release signature every binary
# carries (ADR-020, `ssh-keygen -Y verify`) — and on macOS the Developer ID
# signature and Apple notarization ticket (ADR-018) — then installs it atomically, then runs
# `ctm shell-setup` so PATH and tab completion work in your next shell (one
# marker-delimited block at the end of your rc; CTM_NO_SHELL_SETUP=1 skips it).
#
# POSIX sh only (no bashisms) so it runs under dash, macOS sh, and busybox.
set -eu

REPO="robertelee78/claude-telegram-mirror"
# ADR-018: every darwin release is signed by this team under this identifier. Pinned
# here, as in the binary's own updater, so a re-signed asset is refused even if the
# record that names it was replaced too.
APPLE_TEAM_ID="3T2D2YNTVW"
APPLE_IDENTIFIER="us.ctm.cli"
# ADR-020: every release binary is signed (OpenSSH signature format) by one of these
# Ed25519 keys — the signing key and an offline standby — in this namespace. Pinned
# here and in the binary's own updater (src/release_trust.rs); the contract test keeps
# the two lists identical.
RELEASE_SIGNING_KEYS="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG7D2ChtUbftl2H92GnPLA4ol3Wws2ksy0zzhKdByWtj
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFIwO9PqTyjmlysEDMhb9GEhiREqxGUlDRDYCXkOvWAd"
RELEASE_NAMESPACE="ctm.release"
RELEASE_PRINCIPAL="release@ctm.cli"
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
# `ssh-keygen -Y` (OpenSSH 8.1, 2019) verifies the release signature; it is part of
# the OpenSSH client on every Linux and macOS, so its absence is worth naming.
command -v ssh-keygen >/dev/null 2>&1 \
  || fail "ssh-keygen is required to verify the release signature (install openssh-client / openssh-clients)"
ssh-keygen -Y 2>&1 | grep -q 'verify' \
  || fail "ssh-keygen is too old to verify signatures (needs OpenSSH 8.1 or newer)"
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

# --- release signature (every platform) — ADR-020 -----------------------------------
# The signature is fetched from the same release as the binary, bounded, and
# verified with the pinned keys over the exact bytes that passed the sha256 check.
sig_url="${BASE}/download/v${version}/${ASSET}.sshsig"
curl -fsSL $CURL_PROTO --max-filesize 4096 \
  -w '%{url_effective}\n' -o "$tmp/$ASSET.sshsig" "$sig_url" >"$tmp/sig_final_url" \
  || fail "could not fetch the release signature: $sig_url (every release since 0.2.48 carries one)"
sig_final=$(cat "$tmp/sig_final_url")
case "$sig_final" in
  https://github.com/*|https://release-assets.githubusercontent.com/*|https://objects.githubusercontent.com/*) ;;
  "$BASE"/*) ;;
  *) fail "signature download redirected off GitHub: $sig_final" ;;
esac
: >"$tmp/allowed_signers"
printf '%s\n' "$RELEASE_SIGNING_KEYS" | while IFS= read -r key; do
  [ -n "$key" ] && printf '%s namespaces="%s" %s\n' "$RELEASE_PRINCIPAL" "$RELEASE_NAMESPACE" "$key" >>"$tmp/allowed_signers"
done
if ! verdict=$(ssh-keygen -Y verify -f "$tmp/allowed_signers" -I "$RELEASE_PRINCIPAL" \
      -n "$RELEASE_NAMESPACE" -s "$tmp/$ASSET.sshsig" <"$tmp/$ASSET" 2>&1); then
  fail "release signature verification failed: $verdict"
fi
say "verified: release signature — $(printf '%s' "$verdict" | sed -n 's/.*with ED25519 key //p')"

# --- Apple Developer ID + notarization (macOS) ------------------------------------
# The bytes (already pinned by the record's sha256) must carry a valid Developer ID
# signature by our team, as our identifier, with the hardened runtime and a secure
# timestamp, and Apple must confirm the notary ticket online. No step is optional.
if [ "$os" = Darwin ]; then
  /usr/bin/codesign --verify --strict --all-architectures "$tmp/$ASSET" 2>/dev/null \
    || fail "Apple code-signature verification failed"
  info=$(/usr/bin/codesign --display --verbose=4 "$tmp/$ASSET" 2>&1)
  printf '%s\n' "$info" | grep -qx "TeamIdentifier=$APPLE_TEAM_ID" \
    || fail "binary is not signed by team $APPLE_TEAM_ID"
  printf '%s\n' "$info" | grep -qx "Identifier=$APPLE_IDENTIFIER" \
    || fail "binary is not signed as $APPLE_IDENTIFIER"
  printf '%s\n' "$info" | grep -q "^Authority=Developer ID Application: .* ($APPLE_TEAM_ID)\$" \
    || fail "binary is not signed with a Developer ID Application certificate"
  printf '%s\n' "$info" | grep -q '^CodeDirectory .*flags=0x[0-9a-f]*(runtime' \
    || fail "binary signature lacks the hardened runtime"
  printf '%s\n' "$info" | grep -q '^Timestamp=.' \
    || fail "binary signature lacks a secure timestamp"
  /usr/bin/codesign --verify --strict --all-architectures --check-notarization \
    --test-requirement '=notarized' "$tmp/$ASSET" 2>/dev/null \
    || fail "Apple did not confirm the notarization ticket (is this machine online?)"
  say "verified: Developer ID $APPLE_TEAM_ID as $APPLE_IDENTIFIER, notarized"
fi

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
# Keep the verified signature beside the binary so `ctm doctor` can re-verify it.
cp "$tmp/$ASSET.sshsig" "$INSTALL_DIR/.ctm-signature" && chmod 0644 "$INSTALL_DIR/.ctm-signature"

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
# --- retire a previous npm install -----------------------------------------------
# ctm used to ship as the npm package `claude-telegram-mirror` with a Node shim. If one
# is still installed, its shim can shadow this binary on PATH and the service and hooks
# may still point at it, so remove it here rather than leaving the user two ctms.
npm_removed=""
if command -v npm >/dev/null 2>&1; then
  if npm ls -g --depth=0 claude-telegram-mirror >/dev/null 2>&1; then
    say ""
    say "removing the old npm package (claude-telegram-mirror) …"
    if npm uninstall -g claude-telegram-mirror >/dev/null 2>&1; then
      npm_removed=1
      say "removed: npm claude-telegram-mirror"
    else
      say "warning: could not remove it automatically — run: npm uninstall -g claude-telegram-mirror"
    fi
  fi
fi

say ""
if [ -f "$HOME/.config/claude-telegram-mirror/config.json" ]; then
  # Existing install: the service unit and the Claude Code hooks hold an absolute path,
  # which is stale after a migration (and always after removing the npm copy). `doctor
  # --fix` re-points them, reloads the service, and wires the OpenCode/Codex hosts.
  say "existing configuration found — reconciling the service, hooks and hosts …"
  if "$INSTALL_DIR/ctm" doctor --fix </dev/null; then
    :
  else
    say "warning: some checks still need attention — run: ctm doctor"
  fi
else
  say "next:"
  say "  ctm setup"
fi
[ -n "$npm_removed" ] && say "" && say "the old npm binary is gone; open a new shell so PATH finds $INSTALL_DIR/ctm"
