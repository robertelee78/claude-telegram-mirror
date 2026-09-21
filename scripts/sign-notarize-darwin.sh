#!/usr/bin/env bash
# ADR-018: produce one Developer-ID-signed, notarized ctm executable for a
# darwin target, plus a bounded proof receipt. Ported from hf2q's
# sign_notarize_standalone_release.sh (ADR-045 there), parameterised on target.
#
# The ZIP exists only to submit the standalone Mach-O to Apple's notary
# service; Apple cannot staple tickets to standalone binaries, so consumers
# verify the ticket online (`codesign --check-notarization`).
#
# This script fails closed. There is no ad-hoc fallback: a release without
# these credentials must not exist.
set -euo pipefail

if [[ $# -ne 7 ]]; then
  echo "usage: $0 INPUT_BINARY OUTPUT_DIRECTORY VERSION SOURCE_SHA TEAM_ID IDENTIFIER TARGET" >&2
  exit 2
fi

input_binary=$1
output_directory=$2
version=$3
source_sha=$4
team_id=$5
identifier=$6
target=$7
asset_name="ctm-$target"

sha256_file() { shasum -a 256 "$1" | awk '{print $1}'; }
fail() {
  echo "darwin release signing: $*" >&2
  exit 1
}

case "$target" in
  aarch64-apple-darwin) expected_arch=arm64 ;;
  x86_64-apple-darwin) expected_arch=x86_64 ;;
  *) fail "target must be aarch64-apple-darwin or x86_64-apple-darwin" ;;
esac

[[ $(uname -s) == Darwin ]] || fail "signing requires a macOS host"
[[ -f "$input_binary" && -x "$input_binary" && ! -L "$input_binary" ]] || \
  fail "input must be a regular executable"
[[ "$output_directory" == /* ]] || fail "output directory must be absolute"
[[ ! -e "$output_directory" && ! -L "$output_directory" ]] || \
  fail "output directory already exists"
[[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || \
  fail "version must be canonical stable SemVer"
[[ "$source_sha" =~ ^[0-9a-f]{40}$ ]] || fail "source SHA must be exact Git SHA-1"
[[ "$team_id" =~ ^[A-Z0-9]{10}$ ]] || fail "Apple Team ID is not canonical"
[[ "$identifier" =~ ^[A-Za-z0-9.-]+$ ]] || fail "signing identifier is not canonical"
[[ $(/usr/bin/lipo -archs "$input_binary" 2>/dev/null) == "$expected_arch" ]] || \
  fail "input is not an exact thin $expected_arch Mach-O"
input_sha=$(sha256_file "$input_binary")

signing_identity=${APPLE_DEVELOPER_ID_APPLICATION:?APPLE_DEVELOPER_ID_APPLICATION is required}
p12_base64=${APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64:?APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64 is required}
p12_password=${APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD:?APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD is required}
notary_key_base64=${APPLE_NOTARY_KEY_P8_BASE64:?APPLE_NOTARY_KEY_P8_BASE64 is required}
notary_key_id=${APPLE_NOTARY_KEY_ID:?APPLE_NOTARY_KEY_ID is required}
notary_issuer_id=${APPLE_NOTARY_ISSUER_ID:?APPLE_NOTARY_ISSUER_ID is required}
unset APPLE_DEVELOPER_ID_APPLICATION APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64 \
  APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD APPLE_NOTARY_KEY_P8_BASE64 \
  APPLE_NOTARY_KEY_ID APPLE_NOTARY_ISSUER_ID

[[ "$signing_identity" == "Developer ID Application: "*" ($team_id)" ]] || \
  fail "Developer ID identity does not bind the expected Team ID"
[[ "$signing_identity" != *$'\n'* && ${#signing_identity} -le 255 ]] || \
  fail "Developer ID identity is not canonical"
[[ "$notary_key_id" =~ ^[A-Z0-9]{10}$ ]] || fail "notary key ID is not canonical"
[[ "$notary_issuer_id" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]] || \
  fail "notary issuer ID is not canonical"
# Recorded in the proof rather than pinned: cargo's default per target. Newer
# linkers emit LC_BUILD_VERSION (`minos`); an SDK 14 linker still emits
# LC_VERSION_MIN_MACOSX (`version`) for x86_64 targets below 10.14 — both are
# the minimum OS, and the first such line is it.
minimum_macos=$(/usr/bin/vtool -show-build "$input_binary" 2>/dev/null | \
  awk '$1 == "minos" || $1 == "version" {print $2; exit}')
[[ "$minimum_macos" =~ ^[0-9]+\.[0-9]+$ ]] || fail "input has no readable minimum macOS version"

runner_temp=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
runner_temp=${runner_temp%/}
[[ "$runner_temp" == /* && -d "$runner_temp" && ! -L "$runner_temp" ]] || \
  fail "RUNNER_TEMP must be an existing absolute directory"
if [[ -n ${APPLE_RELEASE_SECRET_DIRECTORY:-} ]]; then
  secret_directory=$APPLE_RELEASE_SECRET_DIRECTORY
  case "$secret_directory" in
    "$runner_temp"/ctm-apple-release-secrets-*) ;;
    *) fail "Apple secret directory is outside the dedicated runner-temp namespace" ;;
  esac
  [[ ! -e "$secret_directory" && ! -L "$secret_directory" ]] || \
    fail "Apple secret directory already exists"
  mkdir -m 0700 "$secret_directory"
else
  secret_directory=$(mktemp -d "$runner_temp/ctm-apple-release-secrets.XXXXXX")
fi
keychain="$secret_directory/release.keychain-db"
p12="$secret_directory/developer-id.p12"
notary_key="$secret_directory/AuthKey_${notary_key_id}.p8"
candidate="$secret_directory/ctm"
submission_archive="$secret_directory/ctm-notary.zip"
submission_json="$output_directory/notary-submission.json"
notary_wait="$output_directory/notary-wait.json"
notary_log="$secret_directory/notary-log.json"
codesign_log="$secret_directory/codesign.txt"
notarization_check_log="$secret_directory/notarization-check.txt"
keychain_password="$(/usr/bin/uuidgen)-$(/usr/bin/uuidgen)"
original_user_keychains=()
while IFS= read -r original_keychain; do
  original_keychain=${original_keychain#"${original_keychain%%[![:space:]]*}"}
  original_keychain=${original_keychain#\"}
  original_keychain=${original_keychain%\"}
  [[ "$original_keychain" == /* ]] || \
    fail "user keychain search list contains a noncanonical path"
  original_user_keychains+=("$original_keychain")
done < <(/usr/bin/security list-keychains -d user)
[[ ${#original_user_keychains[@]} -gt 0 ]] || \
  fail "user keychain search list is empty"
user_keychain_list_modified=false

cleanup() {
  if [[ "$user_keychain_list_modified" == true ]]; then
    /usr/bin/security list-keychains -d user -s \
      "${original_user_keychains[@]}" >/dev/null 2>&1 || true
  fi
  /usr/bin/security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -f -- "$p12" "$notary_key" "$candidate" "$submission_archive" \
    "$notary_log" "$codesign_log" "$notarization_check_log"
  rmdir "$secret_directory" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

umask 077
printf '%s' "$p12_base64" | /usr/bin/base64 -D >"$p12" || \
  fail "Developer ID secret is not valid base64"
printf '%s' "$notary_key_base64" | /usr/bin/base64 -D >"$notary_key" || \
  fail "notary key secret is not valid base64"
[[ -s "$p12" && -s "$notary_key" ]] || fail "Apple credential material is empty"
unset p12_base64 notary_key_base64

/usr/bin/security create-keychain -p "$keychain_password" "$keychain"
/usr/bin/security set-keychain-settings -lut 21600 "$keychain"
/usr/bin/security unlock-keychain -p "$keychain_password" "$keychain"
/usr/bin/security import "$p12" -k "$keychain" -P "$p12_password" \
  -T /usr/bin/codesign >/dev/null
/usr/bin/security set-key-partition-list -S apple-tool:,apple: -s \
  -k "$keychain_password" "$keychain" >/dev/null
unset p12_password
user_keychain_list_modified=true
/usr/bin/security list-keychains -d user -s "$keychain" \
  "${original_user_keychains[@]}"

identities=$(/usr/bin/security find-identity -v -p codesigning "$keychain")
[[ $(grep -Fc -- "\"$signing_identity\"" <<<"$identities") -eq 1 ]] || \
  fail "ephemeral keychain does not contain exactly the intended signing identity"
[[ $(grep -Ec '^[[:space:]]*1 valid identities found$' <<<"$identities") -eq 1 ]] || \
  fail "ephemeral keychain contains an ambiguous signing identity set"
signing_fingerprint=$(grep -F -- "\"$signing_identity\"" <<<"$identities" | awk '{print $2}')
[[ "$signing_fingerprint" =~ ^[0-9A-F]{40}$ ]] || \
  fail "Developer ID signing fingerprint is not canonical"

/bin/cp "$input_binary" "$candidate"
/bin/chmod 0755 "$candidate"
[[ $(sha256_file "$candidate") == "$input_sha" ]] || \
  fail "private signing copy changed the exact unsigned input"
/usr/bin/codesign --force --sign "$signing_fingerprint" --keychain "$keychain" \
  --identifier "$identifier" --options runtime --timestamp "$candidate"
/usr/bin/codesign --verify --strict --all-architectures --verbose=2 "$candidate"
/usr/bin/codesign --display --verbose=4 "$candidate" 2>"$codesign_log"
[[ $(grep -Fxc -- "Identifier=$identifier" "$codesign_log") -eq 1 ]] || \
  fail "signed identifier does not match"
[[ $(grep -Fxc -- "TeamIdentifier=$team_id" "$codesign_log") -eq 1 ]] || \
  fail "signed Team ID does not match"
[[ $(grep -Fxc -- "Authority=$signing_identity" "$codesign_log") -eq 1 ]] || \
  fail "signed authority does not match"
# `codesign --display --verbose=4` emits flags inside its CodeDirectory line,
# for example `CodeDirectory ... flags=0x10000(runtime) hashes=...`.
grep -Eq '^CodeDirectory .* flags=0x[0-9a-f]+\(runtime\)( |$)' \
  "$codesign_log" || \
  fail "hardened runtime is absent from the signature"
grep -Eq '^Timestamp=.+$' "$codesign_log" || \
  fail "secure timestamp is absent from the signature"
cdhash=$(sed -n 's/^CDHash=//p' "$codesign_log")
[[ "$cdhash" =~ ^[0-9a-f]{40,64}$ ]] || fail "signed CDHash is not canonical"
[[ $(grep -c '^CDHash=' "$codesign_log") -eq 1 ]] || fail "signed CDHash is ambiguous"
/usr/bin/ditto -c -k --keepParent "$candidate" "$submission_archive"
archive_sha=$(sha256_file "$submission_archive")
mkdir -m 0700 "$output_directory"
output_binary="$output_directory/$asset_name"
/bin/cp "$candidate" "$output_binary"
/bin/chmod 0555 "$output_binary"
binary_size=$(stat -f '%z' "$output_binary")
binary_sha=$(sha256_file "$output_binary")

if ! /usr/bin/xcrun notarytool submit "$submission_archive" \
  --key "$notary_key" --key-id "$notary_key_id" --issuer "$notary_issuer_id" \
  --output-format json >"$submission_json"; then
  fail "Apple notarization upload did not complete successfully"
fi
submission_id=$(jq -er .id "$submission_json") || \
  fail "Apple notarization upload did not return a submission ID"
[[ "$submission_id" =~ ^[0-9a-fA-F-]{36}$ ]] || fail "notary submission ID is not canonical"
if ! /usr/bin/xcrun notarytool wait "$submission_id" \
  --key "$notary_key" --key-id "$notary_key_id" --issuer "$notary_issuer_id" \
  --timeout 30m --output-format json >"$notary_wait"; then
  fail "Apple notarization wait did not complete; resume the recorded submission ID"
fi
jq -e --arg submission_id "$submission_id" \
  'select(.id == $submission_id and .status == "Accepted")' \
  "$notary_wait" >/dev/null || \
  fail "Apple notarization status was not Accepted"
/usr/bin/xcrun notarytool log "$submission_id" \
  --key "$notary_key" --key-id "$notary_key_id" --issuer "$notary_issuer_id" \
  "$notary_log"
jq -e --arg cdhash "$cdhash" '
  .status == "Accepted"
  and ((.issues // []) | length) == 0
  and any(.ticketContents[]?;
    .digestAlgorithm == "SHA-256" and .cdhash == $cdhash)
' "$notary_log" >/dev/null || fail "notary log does not bind the accepted binary"

# `spctl --assess --type execute` is an app-bundle assessment and rejects a
# valid raw CLI as "not an app".  For a standalone Mach-O, combine the
# purpose-built online-ticket check with the explicit notarized code
# requirement.  Neither option is sufficient alone: `--check-notarization`
# can accept platform/ad-hoc code, while the requirement alone need not force
# an online ticket lookup for this unstapled executable.
notarization_verified=0
for _ in $(seq 1 12); do
  if /usr/bin/codesign --verify --strict --all-architectures \
    --check-notarization --test-requirement '=notarized' --verbose=4 "$candidate" \
    >"$notarization_check_log" 2>&1; then
    notarization_verified=1
    break
  fi
  sleep 5
done
[[ $notarization_verified -eq 1 ]] || \
  fail "online notarization ticket verification did not accept the binary"

submission_sha=$(sha256_file "$submission_json")
notary_wait_sha=$(sha256_file "$notary_wait")
notary_log_sha=$(sha256_file "$notary_log")
codesign_log_sha=$(sha256_file "$codesign_log")
notarization_check_log_sha=$(sha256_file "$notarization_check_log")

/bin/mv "$notary_log" "$output_directory/notary-log.json"
/bin/mv "$codesign_log" "$output_directory/codesign.txt"
/bin/mv "$notarization_check_log" "$output_directory/notarization-check.txt"
jq -nS \
  --arg source_sha "$source_sha" \
  --arg version "$version" \
  --arg target "$target" \
  --arg minimum_macos "$minimum_macos" \
  --arg unsigned_sha256 "$input_sha" \
  --arg asset_name "$asset_name" \
  --arg sha256 "$binary_sha" \
  --arg team_id "$team_id" \
  --arg identifier "$identifier" \
  --arg cdhash "$cdhash" \
  --arg submission_id "$submission_id" \
  --arg submission_archive_sha256 "$archive_sha" \
  --arg submission_sha256 "$submission_sha" \
  --arg wait_sha256 "$notary_wait_sha" \
  --arg notary_log_sha256 "$notary_log_sha" \
  --arg codesign_log_sha256 "$codesign_log_sha" \
  --arg notarization_check_log_sha256 "$notarization_check_log_sha" \
  --argjson size "$binary_size" \
  '{
    kind:"ctm.darwin-release-proof",
    schema_version:1,
    package:"ctm",
    target:$target,
    minimum_macos:$minimum_macos,
    source_sha:$source_sha,
    version:$version,
    input:{unsigned_sha256:$unsigned_sha256},
    asset:{name:$asset_name,size:$size,sha256:$sha256},
    signing:{
      authority:"Developer ID Application",
      team_id:$team_id,
      identifier:$identifier,
      cdhash:$cdhash,
      hardened_runtime:true,
      secure_timestamp:true
    },
    notarization:{
      status:"Accepted",
      submission_id:$submission_id,
      submission_archive_sha256:$submission_archive_sha256,
      submission_sha256:$submission_sha256,
      wait_sha256:$wait_sha256,
      log_sha256:$notary_log_sha256,
      standalone_ticket_stapled:false
    },
    verification:{
      codesign:"accepted",
      codesign_log_sha256:$codesign_log_sha256,
      online_notarization:"accepted",
      online_notarization_log_sha256:$notarization_check_log_sha256,
      notary_ticket_cdhash_matches:true
    }
  }' >"$output_directory/proof.json"
/bin/chmod 0444 "$output_directory"/{proof.json,notary-submission.json,notary-wait.json,notary-log.json,codesign.txt,notarization-check.txt}
printf '%s  %s\n' "$binary_sha" "$asset_name" \
  >"$output_directory/${asset_name}.sha256"
/bin/chmod 0444 "$output_directory/${asset_name}.sha256"

for required in \
  "$asset_name" \
  "${asset_name}.sha256" \
  proof.json \
  notary-submission.json \
  notary-wait.json \
  notary-log.json \
  codesign.txt \
  notarization-check.txt; do
  [[ -f "$output_directory/$required" && ! -L "$output_directory/$required" ]] || \
    fail "signed release output is incomplete or unsafe: $required"
done
[[ $(find "$output_directory" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ') == 8 ]] || \
  fail "signed release output contains unexpected entries"

printf '%s\n' "$output_binary"
