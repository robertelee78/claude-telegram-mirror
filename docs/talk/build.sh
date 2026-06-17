#!/usr/bin/env bash
# Build the talk deck (Markdown -> PDF + HTML) with Marp.
#
# Usage:
#   ./build.sh                 # build PDF + HTML from the default deck
#   ./build.sh somedeck.md     # build a specific deck
#   ./build.sh --watch         # live-rebuild HTML on save (preview in a browser)
#
# Requires: node/npx (uses npx @marp-team/marp-cli, no global install needed)
#           a Chrome/Chromium for PDF export (auto-detected; override with CHROME_PATH)
set -euo pipefail

cd "$(dirname "$0")"

DECK="the-most-fragile-line.md"
WATCH=0
for arg in "$@"; do
  case "$arg" in
    --watch) WATCH=1 ;;
    *.md)    DECK="$arg" ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

if [[ ! -f "$DECK" ]]; then echo "deck not found: $DECK" >&2; exit 1; fi

# Find a Chrome/Chromium for PDF rendering if not already set.
if [[ -z "${CHROME_PATH:-}" ]]; then
  for c in \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium" \
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge" \
    "$(command -v google-chrome 2>/dev/null || true)" \
    "$(command -v chromium 2>/dev/null || true)"; do
    if [[ -n "$c" && -x "$c" ]]; then export CHROME_PATH="$c"; break; fi
  done
fi
echo "Deck:        $DECK"
echo "CHROME_PATH: ${CHROME_PATH:-<auto-detect / none found>}"

MARP=(npx -y @marp-team/marp-cli@latest --allow-local-files)

if [[ "$WATCH" == "1" ]]; then
  echo "Watching $DECK -> HTML preview (Ctrl-C to stop)…"
  exec "${MARP[@]}" --watch --preview "$DECK" -o "${DECK%.md}.html"
fi

OUT_PDF="${DECK%.md}.pdf"
OUT_HTML="${DECK%.md}.html"

echo "Building HTML -> $OUT_HTML"
"${MARP[@]}" "$DECK" -o "$OUT_HTML"

echo "Building PDF  -> $OUT_PDF"
"${MARP[@]}" --pdf "$DECK" -o "$OUT_PDF"

echo "Done:"
echo "  $(cd "$(dirname "$OUT_PDF")" && pwd)/$OUT_PDF"
echo "  $(cd "$(dirname "$OUT_HTML")" && pwd)/$OUT_HTML"
