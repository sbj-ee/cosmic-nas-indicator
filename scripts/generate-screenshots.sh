#!/usr/bin/env bash
# Regenerate PNG screenshots from the SVG sources in docs/screenshots/src/.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/docs/screenshots/src"
out="$root/docs/screenshots"

if ! command -v convert >/dev/null 2>&1; then
  echo "ImageMagick 'convert' is required." >&2
  exit 1
fi

mkdir -p "$out"

for svg in "$src"/*.svg; do
  name="$(basename "${svg%.svg}")"
  convert -background none -density 192 "$svg" -resize 1800x "$out/${name}.png"
  echo "Wrote $out/${name}.png"
done
