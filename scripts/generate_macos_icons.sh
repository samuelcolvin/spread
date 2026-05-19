#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_svg="$repo_root/spread-logo.svg"
output_icns="$repo_root/packaging/macos/Spread.icns"
output_splash_png="$repo_root/packaging/macos/Spread.png"

if [[ ! -f "$source_svg" ]]; then
  echo "error: SVG not found: $source_svg" >&2
  exit 1
fi

if ! command -v magick >/dev/null 2>&1; then
  echo "error: ImageMagick 'magick' command not found" >&2
  echo "install with: brew install imagemagick" >&2
  exit 1
fi

if ! command -v iconutil >/dev/null 2>&1; then
  echo "error: macOS 'iconutil' command not found" >&2
  exit 1
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/spread-icons.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

iconset_dir="$tmp_dir/Spread.iconset"
splash_svg="$tmp_dir/SpreadSplash.svg"
mkdir -p "$iconset_dir" "$(dirname "$output_icns")"
sed 's/<rect x="22" y="22" width="212" height="212" rx="26.5" fill="#fff"\/>/<rect x="22" y="22" width="212" height="212" rx="26.5" fill="none"\/>/' "$source_svg" > "$splash_svg"

render_icon() {
  local logical_size="$1"
  local scale="$2"
  local suffix="$3"
  local pixel_size=$((logical_size * scale))
  local output_png="$iconset_dir/icon_${logical_size}x${logical_size}${suffix}.png"

  magick \
    -background none \
    -density 1024 \
    "$source_svg" \
    -resize "${pixel_size}x${pixel_size}" \
    -depth 8 \
    "PNG32:$output_png"
}

for size in 16 32 128 256 512; do
  render_icon "$size" 1 ""
  render_icon "$size" 2 "@2x"
done

iconutil -c icns "$iconset_dir" -o "$output_icns"
magick \
  -background none \
  -density 1024 \
  "$splash_svg" \
  -resize "512x512" \
  -depth 8 \
  "PNG32:$output_splash_png"
echo "Generated $output_icns"
echo "Generated $output_splash_png"
