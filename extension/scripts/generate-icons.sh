#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
extension_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
source_png="$extension_dir/icons/source.png"
tmp_png="$extension_dir/icons/.source-512.png"

if ! command -v sips >/dev/null 2>&1; then
  echo "error: sips is required to generate extension icons" >&2
  exit 1
fi

trap 'rm -f "$tmp_png"' EXIT HUP INT TERM

sips -s format png "$source_png" --out "$tmp_png" >/dev/null

for size in 16 32 48 128 1024; do
  sips -z "$size" "$size" "$tmp_png" --out "$extension_dir/icons/icon-$size.png" >/dev/null
done

echo "Generated extension icons in $extension_dir/icons"
