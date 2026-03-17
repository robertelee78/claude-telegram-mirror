#!/usr/bin/env bash
#
# bump-version.sh — update the version string across all 6 package files.
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

echo "Bumping version to $VERSION in all package files..."

# 1. Root package.json — "version" field
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$VERSION\"/" "$REPO_ROOT/package.json"
echo "  Updated package.json"

# 1b. Root package.json — optionalDependencies versions
sed -i "s/\"@agidreams\/ctm-linux-x64\": \"[^\"]*\"/\"@agidreams\/ctm-linux-x64\": \"$VERSION\"/" "$REPO_ROOT/package.json"
sed -i "s/\"@agidreams\/ctm-linux-arm64\": \"[^\"]*\"/\"@agidreams\/ctm-linux-arm64\": \"$VERSION\"/" "$REPO_ROOT/package.json"
sed -i "s/\"@agidreams\/ctm-darwin-arm64\": \"[^\"]*\"/\"@agidreams\/ctm-darwin-arm64\": \"$VERSION\"/" "$REPO_ROOT/package.json"
sed -i "s/\"@agidreams\/ctm-darwin-x64\": \"[^\"]*\"/\"@agidreams\/ctm-darwin-x64\": \"$VERSION\"/" "$REPO_ROOT/package.json"
echo "  Updated optionalDependencies in package.json"

# 2. Cargo.toml
sed -i "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" "$REPO_ROOT/rust-crates/ctm/Cargo.toml"
echo "  Updated rust-crates/ctm/Cargo.toml"

# 3-6. Platform npm packages
for pkg in ctm-linux-x64 ctm-linux-arm64 ctm-darwin-arm64 ctm-darwin-x64; do
  sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$VERSION\"/" "$REPO_ROOT/npm-packages/$pkg/package.json"
  echo "  Updated npm-packages/$pkg/package.json"
done

echo ""
echo "Version bumped to $VERSION in all 6 files."
echo "Don't forget to commit and tag: git tag v$VERSION"
