#!/usr/bin/env bash
# Regenerate the mobile app icon so it matches the desktop (PC) app icon.
#
# Canonical brand mark: apps/desktop/src-tauri/icons/maju.png (2000x2000, RGBA).
# `tauri icon` regenerates every desktop artifact from it; this script derives
# the Expo source icon plus the prebuild-managed Android mipmaps from the same
# file, so the two platforms can never drift apart again.
#
# Outputs (re-run after the brand mark changes, then rebuild the app):
#   apps/mobile/assets/icon.png                         1024x1024 Expo source
#   apps/mobile/android/app/src/main/res/mipmap-*/       legacy + adaptive layers
#
# Mirrors @expo/prebuild-config's withAndroidIcons: legacy ic_launcher is
# 48dp * scale, ic_launcher_foreground is 108dp * scale, both resizeMode cover.
# android.adaptiveIcon is not configured in app.config.ts, so this script also
# removes any round/background layers prebuild would delete.
#
# Requires macOS `sips` and libwebp's `cwebp`.
# Windows twin: scripts/generate-mobile-icons.ps1 — same brand mark, sizes and
# outputs; keep the two in lockstep when either changes.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/apps/desktop/src-tauri/icons/maju.png"
MOBILE="$ROOT/apps/mobile"
RES="$MOBILE/android/app/src/main/res"

[ -f "$SRC" ] || { echo "missing brand mark: $SRC" >&2; exit 1; }
command -v sips >/dev/null || { echo "sips not found (macOS only)" >&2; exit 1; }
command -v cwebp >/dev/null || { echo "cwebp not found (brew install webp)" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Expo source icon (transparency kept; iOS flattening happens at prebuild time).
sips -z 1024 1024 "$SRC" --out "$MOBILE/assets/icon.png" >/dev/null
echo "wrote apps/mobile/assets/icon.png (1024x1024)"

densities=(mdpi hdpi xhdpi xxhdpi xxxhdpi)
legacy_sizes=(48 72 96 144 192)
foreground_sizes=(108 162 216 324 432)

for i in "${!densities[@]}"; do
  d="${densities[$i]}"
  dir="$RES/mipmap-$d"
  mkdir -p "$dir"

  sips -z "${legacy_sizes[$i]}" "${legacy_sizes[$i]}" "$SRC" --out "$tmp/legacy.png" >/dev/null
  cwebp -q 90 -alpha_q 100 -quiet "$tmp/legacy.png" -o "$dir/ic_launcher.webp"

  sips -z "${foreground_sizes[$i]}" "${foreground_sizes[$i]}" "$SRC" --out "$tmp/foreground.png" >/dev/null
  cwebp -q 90 -alpha_q 100 -quiet "$tmp/foreground.png" -o "$dir/ic_launcher_foreground.webp"

  rm -f "$dir/ic_launcher_round.webp" "$dir/ic_launcher_background.webp"
  echo "wrote res/mipmap-$d/ic_launcher.webp + ic_launcher_foreground.webp"
done

echo "done — rebuild the mobile app (npx expo run:android) to pick it up"
