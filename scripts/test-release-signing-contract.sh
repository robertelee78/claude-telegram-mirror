#!/usr/bin/env bash
# ADR-020 contract test: the release-signature pipeline's shape and the pins that
# three files must agree on. Static checks run anywhere; the functional probes need
# ssh-keygen (present on every CI runner and every supported host).
# shellcheck disable=SC2016
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
SIGNER="$ROOT_DIR/scripts/sign-release.sh"
RELEASE="$ROOT_DIR/.github/workflows/release.yml"
INSTALL="$ROOT_DIR/install.sh"
TRUST="$ROOT_DIR/rust-crates/ctm/src/release_trust.rs"
UPDATE="$ROOT_DIR/rust-crates/ctm/src/update.rs"

fail() { echo "release signing contract: $*" >&2; exit 1; }

workflow_job() {
  awk -v start="  $1:" '
    $0 == start { capture = 1 }
    capture && /^  [A-Za-z0-9_-]+:$/ && $0 != start { exit }
    capture { print }
  ' "$RELEASE"
}

bash -n "$SIGNER"
sh -n "$INSTALL"
test -x "$SIGNER" || fail "signer is not executable"

# --- the pins: two keys, identical in install.sh and release_trust.rs -------------
install_pins=$(awk '/^RELEASE_SIGNING_KEYS="/{f=1} f{line=$0; end=(line ~ /"$/); sub(/^RELEASE_SIGNING_KEYS="/,"",line); sub(/"$/,"",line); print line; if(end) exit}' "$INSTALL")
rust_pins=$(grep -oE '"ssh-ed25519 [A-Za-z0-9+/=]+"' "$TRUST" | tr -d '"')
[[ $(wc -l <<<"$install_pins" | tr -d ' ') == 2 ]] || fail "install.sh must pin exactly two keys (signing + standby)"
[[ $(wc -l <<<"$rust_pins" | tr -d ' ') == 2 ]] || fail "release_trust.rs must pin exactly two keys"
[[ "$(sort <<<"$install_pins")" == "$(sort <<<"$rust_pins")" ]] || fail "install.sh and release_trust.rs pin different keys"
[[ "$(sort -u <<<"$install_pins" | wc -l | tr -d ' ')" == 2 ]] || fail "the two pinned keys must differ"
while IFS= read -r pin; do
  [[ "$pin" =~ ^ssh-ed25519\ [A-Za-z0-9+/=]+$ ]] || fail "pin is not a bare ssh-ed25519 line: $pin"
done <<<"$install_pins"
ns=$(sed -n 's/^RELEASE_NAMESPACE="\(.*\)"$/\1/p' "$INSTALL")
[[ -n "$ns" ]] || fail "install.sh has no RELEASE_NAMESPACE"
grep -q "pub const NAMESPACE: &str = \"$ns\";" "$TRUST" || fail "release_trust.rs pins a different namespace"
principal=$(sed -n 's/^RELEASE_PRINCIPAL="\(.*\)"$/\1/p' "$INSTALL")
[[ -n "$principal" ]] || fail "install.sh has no RELEASE_PRINCIPAL"
grep -q "CTM_RELEASE_NAMESPACE: $ns" "$RELEASE" || fail "release.yml signs in a different namespace"
grep -q "CTM_RELEASE_PRINCIPAL: $principal" "$RELEASE" || fail "release.yml uses a different principal"

# --- no fallback ------------------------------------------------------------------
! grep -q 'CTM_INSTALL_SKIP_SIGNATURE\|SKIP_SIGNATURE' "$INSTALL" || fail "install.sh has a signature skip"
! grep -qi 'skip.*signature' "$UPDATE" || fail "update.rs has a signature skip"
! grep -q 'continue-on-error' "$RELEASE" || fail "release.yml tolerates a failed job"
grep -q 'verify_release_signature(&candidate, &signature)' "$UPDATE" || fail "update.rs does not verify the release signature"
grep -q 'download_signature' "$UPDATE" || fail "update.rs does not fetch the signature"
grep -q 'ssh-keygen -Y verify' "$INSTALL" || fail "install.sh does not verify the signature"
grep -q 'namespaces=' "$INSTALL" || fail "install.sh does not bind the namespace in allowed_signers"
grep -q -- '--max-filesize 4096' "$INSTALL" || fail "install.sh does not bound the signature download"
# The signature check must come before the Apple block, so darwin gets both.
[[ $(grep -n 'ssh-keygen -Y verify' "$INSTALL" | head -1 | cut -d: -f1) -lt $(grep -n 'Apple Developer ID + notarization' "$INSTALL" | head -1 | cut -d: -f1) ]] \
  || fail "install.sh verifies the release signature after the Apple block"

# --- jobs ---------------------------------------------------------------------------
for job in build-linux build-darwin; do
  src=$(workflow_job "$job")
  ! grep -q 'secrets\.' <<<"$src" || fail "$job references secrets"
  ! grep -q 'environment:' <<<"$src" || fail "$job uses an environment"
done
sign=$(workflow_job sign-release)
[[ -n "$sign" ]] || fail "sign-release job missing"
grep -q '^    environment: release-signing$' <<<"$sign" || fail "sign-release is not bound to the release-signing environment"
grep -q 'needs: \[build-linux, sign-darwin\]' <<<"$sign" || fail "sign-release must run after every build and after Apple signing"
grep -q 'persist-credentials: false' <<<"$sign" || fail "sign-release checkout persists credentials"
grep -q 'test "$(git rev-parse HEAD)" = "$GITHUB_SHA"' <<<"$sign" || fail "sign-release does not verify the source identity"
grep -q 'scripts/sign-release.sh' <<<"$sign" || fail "sign-release does not use the signer"
for secret in CTM_RELEASE_SIGNING_KEY_BASE64 CTM_RELEASE_SIGNING_KEY_PASSPHRASE; do
  grep -q "secrets\.$secret" <<<"$sign" || fail "sign-release lacks secret $secret"
done
grep -q 'vars\.CTM_RELEASE_SIGNING_PUBLIC_KEY' <<<"$sign" || fail "sign-release lacks the public-key variable"
! grep -Eq '^[[:space:]]*"\$(bin|candidate|unsigned)"([[:space:]]|$)' <<<"$sign" || fail "sign-release executes a candidate"
! grep -Eq '\$\([[:space:]]*"\$(bin|candidate|unsigned)"' <<<"$sign" || fail "sign-release executes a candidate"
publish=$(workflow_job publish)
grep -q 'needs: \[sign-release\]' <<<"$publish" || fail "publish does not depend on sign-release"
grep -q 'pattern: signed-\*' <<<"$publish" || fail "publish could pick up unsigned artifacts"
grep -q 'ssh-keygen -Y verify' <<<"$publish" || fail "publish does not re-verify the signatures"
grep -q 'ssh-keygen -Y find-principals' <<<"$publish" || fail "publish does not check the principal"
grep -q 'attest-build-provenance' <<<"$publish" || fail "publish does not attest provenance"
grep -q '"kind,schema_version,package,channel,target,version,size,sha256"' <<<"$publish" || fail "publish does not pin the frozen record shape"
verify=$(workflow_job verify-install)
[[ -n "$verify" ]] || fail "verify-install job missing"
grep -q 'needs: \[publish\]' <<<"$verify" || fail "verify-install must run after publish"
grep -q 'install.sh' <<<"$verify" || fail "verify-install does not run the installer"
# Third-party actions pinned by commit SHA (mutable tags are the cheap way into a build).
if grep -E '^\s*-?\s*uses: [^#]*@' "$RELEASE" | grep -vE '@[0-9a-f]{40}( |$)' | grep -q .; then
  grep -E '^\s*-?\s*uses: [^#]*@' "$RELEASE" | grep -vE '@[0-9a-f]{40}( |$)' >&2
  fail "release.yml uses an action that is not pinned by commit SHA"
fi

# --- the signer refuses before it touches credentials or a key --------------------
if command -v ssh-keygen >/dev/null 2>&1; then
  probe=$(mktemp -d "${TMPDIR:-/tmp}/ctm-release-contract.XXXXXX")
  trap 'rm -rf "$probe"' EXIT
  printf 'binary bytes\n' >"$probe/bin"
  sha=0123456789abcdef0123456789abcdef01234567
  "$SIGNER" >/dev/null 2>&1 && fail "signer accepted no arguments"
  out=$("$SIGNER" "$probe/bin" "$probe/out" 0.2.48 "$sha" riscv64-unknown-linux-gnu "$ns" "$principal" 2>&1) && fail "signer accepted an unknown target"
  grep -q 'target is not one ctm releases' <<<"$out" || fail "unknown target was not the reason: $out"
  out=$(env -u CTM_RELEASE_SIGNING_KEY_BASE64 RUNNER_TEMP="$probe" \
    "$SIGNER" "$probe/bin" "$probe/out" 0.2.48 "$sha" x86_64-unknown-linux-gnu "$ns" "$principal" 2>&1) && fail "signer proceeded without credentials"
  grep -q 'CTM_RELEASE_SIGNING_KEY_BASE64 is required' <<<"$out" || fail "missing credentials were not the reason: $out"
  [[ -z "$(find "$probe" -maxdepth 1 -name 'ctm-release-signing-secrets*' -print -quit)" ]] || fail "signer created a secret directory before validating credentials"
  [[ ! -e "$probe/out" ]] || fail "signer created output without credentials"
  # A key that is NOT pinned must be refused by name, before signing.
  ssh-keygen -q -t ed25519 -N 'pw' -f "$probe/unpinned"
  out=$(RUNNER_TEMP="$probe" CTM_RELEASE_SIGNING_KEY_BASE64="$(base64 <"$probe/unpinned" | tr -d '\n')" \
    CTM_RELEASE_SIGNING_KEY_PASSPHRASE=pw CTM_RELEASE_SIGNING_PUBLIC_KEY="$(cat "$probe/unpinned.pub")" \
    "$SIGNER" "$probe/bin" "$probe/out" 0.2.48 "$sha" x86_64-unknown-linux-gnu "$ns" "$principal" 2>&1) && fail "signer signed with an unpinned key"
  grep -q 'not pinned in install.sh' <<<"$out" || fail "unpinned key was not the reason: $out"
  [[ ! -e "$probe/out" ]] || fail "signer produced output for an unpinned key"
  # The install.sh verification, run exactly as install.sh does it, against the
  # repository fixture signed by the real release key: accepts; and refuses tampering.
  fx="$ROOT_DIR/rust-crates/ctm/tests/fixtures/release-sig"
  : >"$probe/allowed"
  while IFS= read -r pin; do printf '%s namespaces="%s" %s\n' "$principal" "$ns" "$pin" >>"$probe/allowed"; done <<<"$install_pins"
  ssh-keygen -Y verify -f "$probe/allowed" -I "$principal" -n "$ns" -s "$fx/blob.bin.primary.sshsig" <"$fx/blob.bin" >/dev/null 2>&1 \
    || fail "the fixture signed by the release key does not verify against the install.sh pins"
  printf 'x' | cat "$fx/blob.bin" - >"$probe/tampered"
  ssh-keygen -Y verify -f "$probe/allowed" -I "$principal" -n "$ns" -s "$fx/blob.bin.primary.sshsig" <"$probe/tampered" >/dev/null 2>&1 \
    && fail "tampered bytes verified"
  ssh-keygen -Y verify -f "$probe/allowed" -I "$principal" -n "$ns" -s "$fx/blob.bin.sshsig" <"$fx/blob.bin" >/dev/null 2>&1 \
    && fail "a signature by an unpinned key verified"
  ssh-keygen -Y verify -f "$probe/allowed" -I "$principal" -n "$ns" -s "$fx/blob.bin.other-namespace.sshsig" <"$fx/blob.bin" >/dev/null 2>&1 \
    && fail "a signature in another namespace verified"
fi

echo "release signing contract: ok"
