#!/usr/bin/env bash
# ADR-018 contract test: the shape of the signing pipeline, pinned so it cannot
# quietly regress to "sign if we can". Static checks run anywhere; the signer's
# refusal probes need macOS (the script refuses non-Darwin hosts first).
#
# Literal probes below contain shell expressions that must not expand here.
# shellcheck disable=SC2016
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
SIGNER="$ROOT_DIR/scripts/sign-notarize-darwin.sh"
RELEASE="$ROOT_DIR/.github/workflows/release.yml"
INSTALL="$ROOT_DIR/install.sh"
TRUST="$ROOT_DIR/rust-crates/ctm/src/apple_trust.rs"

fail() { echo "signing contract: $*" >&2; exit 1; }

workflow_job() {
  awk -v start="  $1:" '
    $0 == start { capture = 1 }
    capture && /^  [A-Za-z0-9_-]+:$/ && $0 != start { exit }
    capture { print }
  ' "$RELEASE"
}

# --- the scripts parse ---------------------------------------------------------
bash -n "$SIGNER"
sh -n "$INSTALL"
test -x "$SIGNER" || fail "signer is not executable"

# --- one identity, pinned identically everywhere --------------------------------
team=$(sed -n 's/^pub const TEAM_ID: &str = "\([A-Z0-9]\{10\}\)";$/\1/p' "$TRUST")
ident=$(sed -n 's/^pub const IDENTIFIER: &str = "\([A-Za-z0-9.-]*\)";$/\1/p' "$TRUST")
[[ -n "$team" && -n "$ident" ]] || fail "apple_trust.rs does not pin TEAM_ID/IDENTIFIER"
grep -qx "APPLE_TEAM_ID=\"$team\"" "$INSTALL" || fail "install.sh pins a different team"
grep -qx "APPLE_IDENTIFIER=\"$ident\"" "$INSTALL" || fail "install.sh pins a different identifier"
grep -q "test \"\$APPLE_TEAM_ID\" = $team\$" "$RELEASE" || fail "release.yml does not pin the team"
grep -q "test \"\$APPLE_CODESIGN_IDENTIFIER\" = $ident\$" "$RELEASE" || fail "release.yml does not pin the identifier"
grep -q "test \"\$team\" = $team && test \"\$ident\" = $ident" "$RELEASE" || fail "publish does not pin the identity"

# --- no fallback anywhere --------------------------------------------------------
! grep -q -- '--sign -' "$RELEASE" || fail "release.yml still contains an ad-hoc signing fallback"
! grep -qi 'secrets not set' "$RELEASE" || fail "release.yml still tolerates missing secrets"
! grep -q 'continue-on-error' "$RELEASE" || fail "release.yml tolerates a failed job"
! grep -q 'codesign --force --sign -' "$SIGNER" || fail "signer contains an ad-hoc path"

# --- build jobs see no secrets; the signing job is protected --------------------
build=$(workflow_job build-darwin)
[[ -n "$build" ]] || fail "build-darwin job missing"
! grep -q 'secrets\.' <<<"$build" || fail "build-darwin job references secrets"
! grep -q 'environment:' <<<"$build" || fail "build-darwin job uses an environment"

sign=$(workflow_job sign-darwin)
[[ -n "$sign" ]] || fail "sign-darwin job missing"
grep -q '^    environment: apple-release$' <<<"$sign" || fail "sign-darwin is not bound to the apple-release environment"
grep -q 'persist-credentials: false' <<<"$sign" || fail "sign-darwin checkout persists credentials"
grep -q 'test "$(git rev-parse HEAD)" = "$GITHUB_SHA"' <<<"$sign" || fail "sign-darwin does not verify the source identity"
grep -q 'scripts/sign-notarize-darwin.sh' <<<"$sign" || fail "sign-darwin does not use the signer"
for secret in APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64 APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD APPLE_NOTARY_KEY_P8_BASE64; do
  grep -q "secrets\.$secret" <<<"$sign" || fail "sign-darwin lacks secret $secret"
done
for var in APPLE_DEVELOPER_ID_APPLICATION APPLE_NOTARY_KEY_ID APPLE_NOTARY_ISSUER_ID APPLE_TEAM_ID APPLE_CODESIGN_IDENTIFIER; do
  grep -q "vars\.$var" <<<"$sign" || fail "sign-darwin lacks variable $var"
done
# The unsigned candidate is data: never a command, never a command substitution.
! grep -Eq '^[[:space:]]*"\$unsigned"([[:space:]]|$)' <<<"$sign" || fail "sign-darwin executes the unsigned candidate"
! grep -Eq '\$\([[:space:]]*"\$unsigned"' <<<"$sign" || fail "sign-darwin executes the unsigned candidate"

publish=$(workflow_job publish)
grep -q 'runs-on: macos-latest' <<<"$publish" || fail "publish must run on macOS to re-verify with codesign"
grep -q -- "--check-notarization --test-requirement '=notarized'" <<<"$publish" || fail "publish does not verify notarization online"
grep -q 'needs: \[build-linux, sign-darwin\]' <<<"$publish" || fail "publish does not depend on the signed candidates"
grep -q 'pattern: release-\*' <<<"$publish" || fail "publish could pick up unsigned candidates"

# --- install.sh verifies on darwin -------------------------------------------------
grep -q -- "--check-notarization" "$INSTALL" || fail "install.sh does not verify notarization"
grep -q 'CDHash=\$r_cdhash' "$INSTALL" || fail "install.sh does not bind the record CDHash"
grep -q '(runtime' "$INSTALL" || fail "install.sh does not require the hardened runtime"

# --- the signer refuses before it touches credentials or a keychain ----------------
if [[ $(uname -s) == Darwin ]]; then
  probe=$(mktemp -d "${TMPDIR:-/tmp}/ctm-signing-contract.XXXXXX")
  trap 'rm -rf "$probe"' EXIT
  printf '#!/bin/sh\nexit 0\n' >"$probe/bin"; chmod 0755 "$probe/bin"
  sha=0123456789abcdef0123456789abcdef01234567
  out=$("$SIGNER" 2>&1) && fail "signer accepted no arguments"
  out=$("$SIGNER" "$probe/bin" "$probe/out" 0.2.45 "$sha" 3T2D2YNTVW us.ctm.cli riscv-apple-darwin 2>&1) \
    && fail "signer accepted an unknown target"
  grep -q 'target must be' <<<"$out" || fail "unknown target was not the reason: $out"
  out=$("$SIGNER" "$probe/bin" "$probe/out" 0.2.45 "$sha" 3T2D2YNTVW us.ctm.cli aarch64-apple-darwin 2>&1) \
    && fail "signer accepted a non-Mach-O input"
  grep -q 'not an exact thin arm64 Mach-O' <<<"$out" || fail "non-Mach-O was not the reason: $out"
  # A real Mach-O with no credentials in the environment: refused by name, and the
  # ephemeral keychain namespace stays empty.
  macho=/usr/bin/true
  arch=$(/usr/bin/lipo -archs "$macho" 2>/dev/null | tr ' ' '\n' | head -1)
  [[ -n "$arch" ]] || fail "could not read $macho's architectures"
  thin="$probe/thin"; /usr/bin/lipo "$macho" -thin "$arch" -output "$thin" 2>/dev/null || cp "$macho" "$thin"
  chmod 0755 "$thin"
  target=aarch64-apple-darwin; [[ "$arch" == x86_64 ]] && target=x86_64-apple-darwin
  env -u APPLE_DEVELOPER_ID_APPLICATION RUNNER_TEMP="$probe" \
    "$SIGNER" "$thin" "$probe/out" 0.2.45 "$sha" 3T2D2YNTVW us.ctm.cli "$target" >"$probe/log" 2>&1 \
    && fail "signer proceeded without credentials"
  grep -q 'APPLE_DEVELOPER_ID_APPLICATION is required' "$probe/log" || fail "missing credentials were not the reason: $(cat "$probe/log")"
  [[ -z "$(find "$probe" -maxdepth 1 -name 'ctm-apple-release-secrets*' -print -quit)" ]] \
    || fail "signer created a secret directory before validating credentials"
  [[ ! -e "$probe/out" ]] || fail "signer created output without credentials"
fi

echo "signing contract: ok"
