#!/usr/bin/env bash
# ADR-020: sign one final release binary with the ctm release key (Ed25519, OpenSSH
# signature format, namespace-bound) and emit a bounded proof receipt.
#
# The key's public half must equal the pin the consumers carry — checked here,
# against install.sh and release_trust.rs, before anything is signed: a release
# signed by a key the consumers do not pin would be refused by every one of them.
#
# This script fails closed. There is no unsigned fallback.
set -euo pipefail

if [[ $# -ne 7 ]]; then
  echo "usage: $0 INPUT_BINARY OUTPUT_DIRECTORY VERSION SOURCE_SHA TARGET NAMESPACE PRINCIPAL" >&2
  exit 2
fi

input_binary=$1
output_directory=$2
version=$3
source_sha=$4
target=$5
namespace=$6
principal=$7
asset_name="ctm-$target"
root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)

sha256_file() { shasum -a 256 "$1" | awk '{print $1}'; }
fail() {
  echo "release signing: $*" >&2
  exit 1
}

case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) fail "target is not one ctm releases" ;;
esac
[[ -f "$input_binary" && ! -L "$input_binary" ]] || fail "input must be a regular file"
[[ "$output_directory" == /* ]] || fail "output directory must be absolute"
[[ ! -e "$output_directory" && ! -L "$output_directory" ]] || fail "output directory already exists"
[[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || fail "version must be canonical stable SemVer"
[[ "$source_sha" =~ ^[0-9a-f]{40}$ ]] || fail "source SHA must be exact Git SHA-1"
[[ "$namespace" =~ ^[A-Za-z0-9.@-]+$ ]] || fail "namespace is not canonical"
[[ "$principal" =~ ^[A-Za-z0-9.@-]+$ ]] || fail "principal is not canonical"
command -v ssh-keygen >/dev/null || fail "ssh-keygen is required"
(ssh-keygen -Y 2>&1 || true) | grep -q verify || fail "ssh-keygen cannot sign (needs OpenSSH 8.1+)"
input_sha=$(sha256_file "$input_binary")

key_base64=${CTM_RELEASE_SIGNING_KEY_BASE64:?CTM_RELEASE_SIGNING_KEY_BASE64 is required}
passphrase=${CTM_RELEASE_SIGNING_KEY_PASSPHRASE:?CTM_RELEASE_SIGNING_KEY_PASSPHRASE is required}
expected_public=${CTM_RELEASE_SIGNING_PUBLIC_KEY:?CTM_RELEASE_SIGNING_PUBLIC_KEY is required}
unset CTM_RELEASE_SIGNING_KEY_BASE64 CTM_RELEASE_SIGNING_KEY_PASSPHRASE CTM_RELEASE_SIGNING_PUBLIC_KEY
expected_public=$(printf '%s' "$expected_public" | awk '{print $1" "$2}')
[[ "$expected_public" =~ ^ssh-ed25519\ [A-Za-z0-9+/=]+$ ]] || fail "expected public key is not an ssh-ed25519 line"

# The consumers' pins are the source of truth for what may sign a release.
install_pins=$(awk '/^RELEASE_SIGNING_KEYS="/{f=1} f{line=$0; end=(line ~ /"$/); sub(/^RELEASE_SIGNING_KEYS="/,"",line); sub(/"$/,"",line); print line; if(end) exit}' "$root_dir/install.sh")
rust_pins=$(grep -oE '"ssh-ed25519 [A-Za-z0-9+/=]+"' "$root_dir/rust-crates/ctm/src/release_trust.rs" | tr -d '"')
grep -qxF -- "$expected_public" <<<"$install_pins" || fail "the signing key is not pinned in install.sh"
grep -qxF -- "$expected_public" <<<"$rust_pins" || fail "the signing key is not pinned in release_trust.rs"
[[ "$(sort <<<"$install_pins")" == "$(sort <<<"$rust_pins")" ]] || fail "install.sh and release_trust.rs pin different key sets"

runner_temp=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
runner_temp=${runner_temp%/}
[[ "$runner_temp" == /* && -d "$runner_temp" && ! -L "$runner_temp" ]] || fail "RUNNER_TEMP must be an existing absolute directory"
if [[ -n ${CTM_RELEASE_SECRET_DIRECTORY:-} ]]; then
  secret_directory=$CTM_RELEASE_SECRET_DIRECTORY
  case "$secret_directory" in
    "$runner_temp"/ctm-release-signing-secrets-*) ;;
    *) fail "secret directory is outside the dedicated runner-temp namespace" ;;
  esac
  [[ ! -e "$secret_directory" && ! -L "$secret_directory" ]] || fail "secret directory already exists"
  mkdir -m 0700 "$secret_directory"
else
  secret_directory=$(mktemp -d "$runner_temp/ctm-release-signing-secrets.XXXXXX")
fi
key="$secret_directory/key"
candidate="$secret_directory/$asset_name"
cleanup() {
  rm -f -- "$key" "$key.pub" "$candidate" "$candidate.sig" "$secret_directory/allowed_signers"
  rmdir "$secret_directory" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

umask 077
printf '%s' "$key_base64" | base64 -d >"$key" 2>/dev/null || fail "signing key secret is not valid base64"
unset key_base64
[[ -s "$key" ]] || fail "signing key material is empty"
grep -q 'BEGIN OPENSSH PRIVATE KEY' "$key" || fail "signing key is not an OpenSSH private key"
actual_public=$(ssh-keygen -y -f "$key" -P "$passphrase" 2>/dev/null | awk '{print $1" "$2}') \
  || fail "signing key could not be read (wrong passphrase?)"
[[ "$actual_public" == "$expected_public" ]] || fail "signing key does not match the expected public key"
fingerprint=$(printf '%s\n' "$actual_public" | ssh-keygen -lf - | awk '{print $2}')
[[ "$fingerprint" == SHA256:* ]] || fail "could not fingerprint the signing key"

cp "$input_binary" "$candidate"
[[ $(sha256_file "$candidate") == "$input_sha" ]] || fail "private signing copy changed the input"
ssh-keygen -q -Y sign -f "$key" -P "$passphrase" -n "$namespace" "$candidate" \
  || fail "ssh-keygen -Y sign failed"
unset passphrase
[[ -s "$candidate.sig" ]] || fail "no signature was produced"

# Self-verify the way install.sh will: allowed_signers from install.sh's pins, not the key.
: >"$secret_directory/allowed_signers"
while IFS= read -r pin; do
  [[ -n "$pin" ]] && printf '%s namespaces="%s" %s\n' "$principal" "$namespace" "$pin" >>"$secret_directory/allowed_signers"
done <<<"$install_pins"
ssh-keygen -Y verify -f "$secret_directory/allowed_signers" -I "$principal" -n "$namespace" \
  -s "$candidate.sig" <"$candidate" >/dev/null 2>&1 || fail "the fresh signature does not verify against the install.sh pins"
found=$(ssh-keygen -Y find-principals -f "$secret_directory/allowed_signers" -s "$candidate.sig" 2>/dev/null)
[[ "$found" == "$principal" ]] || fail "find-principals returned '$found', expected '$principal'"

mkdir -m 0755 "$output_directory"
cp "$candidate" "$output_directory/$asset_name"
chmod 0555 "$output_directory/$asset_name"
cp "$candidate.sig" "$output_directory/$asset_name.sshsig"
chmod 0444 "$output_directory/$asset_name.sshsig"
# `wc -c` is the one size probe that is the same on GNU and BSD userlands
# (`stat -f` means "filesystem status" on GNU and prints junk before failing).
binary_size=$(wc -c <"$output_directory/$asset_name" | tr -d ' ')
[[ "$binary_size" =~ ^[0-9]+$ ]] || fail "could not measure the output"
binary_sha=$(sha256_file "$output_directory/$asset_name")
[[ "$binary_sha" == "$input_sha" ]] || fail "output bytes differ from the input"
printf '%s  %s\n' "$binary_sha" "$asset_name" >"$output_directory/$asset_name.sha256"
sig_sha=$(sha256_file "$output_directory/$asset_name.sshsig")
jq -nS \
  --arg source_sha "$source_sha" --arg version "$version" --arg target "$target" \
  --arg asset_name "$asset_name" --argjson size "$binary_size" --arg sha256 "$binary_sha" \
  --arg namespace "$namespace" --arg principal "$principal" --arg fingerprint "$fingerprint" \
  --arg public_key "$actual_public" --arg sig_sha256 "$sig_sha" \
  '{
    kind:"ctm.release-signature-proof",
    schema_version:1,
    package:"ctm",
    source_sha:$source_sha,
    version:$version,
    target:$target,
    asset:{name:$asset_name,size:$size,sha256:$sha256},
    signature:{format:"sshsig",version:1,key_type:"ssh-ed25519",hash:"sha512",
               namespace:$namespace,principal:$principal,fingerprint:$fingerprint,
               public_key:$public_key,file:($asset_name+".sshsig"),sha256:$sig_sha256},
    verification:{ssh_keygen_verify:"accepted",find_principals:$principal,
                  pins_checked:["install.sh","rust-crates/ctm/src/release_trust.rs"]}
  }' >"$output_directory/sigproof.json"
chmod 0444 "$output_directory/sigproof.json" "$output_directory/$asset_name.sha256"
[[ $(find "$output_directory" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ') == 4 ]] \
  || fail "signed output contains unexpected entries"
printf '%s\n' "$output_directory/$asset_name"
