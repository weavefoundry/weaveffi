#!/usr/bin/env bash
# Render post-ready social images from committed SVG sources.
#
# Usage:
#   scripts/render-social-assets.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

if ! command -v rsvg-convert >/dev/null 2>&1; then
  echo "ERROR: rsvg-convert not found in PATH." >&2
  echo "Install librsvg, for example: brew install librsvg" >&2
  exit 1
fi

if ! command -v magick >/dev/null 2>&1; then
  echo "ERROR: magick not found in PATH." >&2
  echo "Install ImageMagick, for example: brew install imagemagick" >&2
  exit 1
fi

render_jpg() {
  local src="$1"
  local out="$2"
  local width="$3"
  local height="$4"
  local quality="${JPEG_QUALITY:-94}"
  local tmp

  tmp="$(mktemp "${TMPDIR:-/tmp}/weaveffi-social.XXXXXX.png")"
  trap 'rm -f "$tmp"' RETURN

  rsvg-convert -w "$width" -h "$height" -f png -o "$tmp" "$src"
  magick "$tmp" -strip -interlace Plane -sampling-factor 4:2:0 -quality "$quality" "$out"

  echo "Rendered $out (${width}x${height}, quality ${quality})"
}

render_jpg \
  "docs/src/assets/comparison-social.svg" \
  "docs/src/assets/comparison-social.jpg" \
  3840 \
  2160
