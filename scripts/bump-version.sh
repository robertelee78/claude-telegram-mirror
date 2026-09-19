#!/usr/bin/env bash
#
# bump-version.sh — update the crate version (Cargo.toml + Cargo.lock).
#
# Usage: ./scripts/bump-version.sh 0.3.0
#
set -euo pipefail

VERSION="${1:-}"

if [ -z "$VERSION" ]; then
  echo "Usage: $0 <semver-version>" >&2
  echo "Example: $0 0.3.0" >&2
  exit 1
fi

# Validate semver format (major.minor.patch, optional pre-release/build)
if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?(\+[a-zA-Z0-9.]+)?$'; then
  echo "Error: '$VERSION' is not a valid semver version." >&2
  echo "Expected format: MAJOR.MINOR.PATCH (e.g. 0.3.0)" >&2
  exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Portable in-place sed: GNU sed takes `-i` with no argument, BSD/macOS sed requires a
# backup suffix (a bare `-i` there consumes the script as the suffix and fails with
# "undefined label"). Detect once and route every edit through this helper.
if sed --version >/dev/null 2>&1; then
  sedi() { sed -i "$@"; }
else
  sedi() { sed -i '' "$@"; }
fi

echo "Bumping version to $VERSION in all package files..."

# ADR-017: Cargo.toml is the single source of truth for the version. The release
# workflow reads it to name the tag-pinned assets and to write each release record.
sedi "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" "$REPO_ROOT/rust-crates/ctm/Cargo.toml"
echo "  Updated rust-crates/ctm/Cargo.toml"
( cd "$REPO_ROOT/rust-crates" && cargo update -p ctm --offline >/dev/null 2>&1 || cargo update -p ctm >/dev/null 2>&1 ) && echo "  Updated rust-crates/Cargo.lock"

echo ""
echo "Version bumped to $VERSION."
echo "Don't forget to commit and tag: git tag v$VERSION"
