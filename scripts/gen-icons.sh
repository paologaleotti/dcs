#!/usr/bin/env bash
# Regenerate all platform icon files from the two committed masters:
#   assets/icon.svg     flat, full-bleed squircle art
#   assets/icon.icon    Icon Composer package (layered, macOS 26+ liquid glass);
#                       same geometry as icon.svg, authored by hand
#
# Outputs (committed to the repo so CI consumes them directly):
#   assets/icon.png        1024px full-bleed (Linux .deb/AppImage + packager)
#   assets/linux/icon-*.png  hicolor size ladder for the Linux desktop entry
#   assets/icon-256.png    embedded in the binary as the runtime window icon
#   assets/icon.icns       macOS <= 15 .app/.dmg — Apple-grid padding (824/1024)
#                          is applied here at render time, per size
#   assets/icon.ico        Windows .exe / installers
#   assets/Assets.car      macOS 26+ layered Dock icon compiled from icon.icon;
#                          referenced by CFBundleIconName in assets/macos-info.plist
#
# Deps: rsvg-convert (librsvg), python3 + Pillow; macOS-only: iconutil (.icns),
# Xcode's actool (Assets.car — skipped with a warning when Xcode is absent).
set -euo pipefail
cd "$(dirname "$0")/.."

SVG=assets/icon.svg
ICON_PKG=assets/icon.icon
test -f "$SVG" || { echo "missing $SVG"; exit 1; }
test -d "$ICON_PKG" || { echo "missing $ICON_PKG"; exit 1; }
command -v rsvg-convert >/dev/null || { echo "need rsvg-convert (brew install librsvg)"; exit 1; }
command -v python3 >/dev/null || { echo "need python3 + Pillow"; exit 1; }

echo "==> PNG masters (full-bleed)"
rsvg-convert -w 1024 -h 1024 "$SVG" -o assets/icon.png
rsvg-convert -w 256  -h 256  "$SVG" -o assets/icon-256.png

# cargo-packager installs each PNG from its `icons` config into
# hicolor/<WxH>/apps/dcs.png by measured size; without the ladder, desktops
# would downscale 1024px for every slot.
echo "==> Linux hicolor sizes (full-bleed)"
mkdir -p assets/linux
for s in 32 48 64 128 256 512; do
  rsvg-convert -w "$s" -h "$s" "$SVG" -o "assets/linux/icon-${s}.png"
done

# Render one padded PNG: content occupies 824/1024 of the canvas (Apple icon
# grid), centered. Content size is rounded to the canvas's parity so the
# margins stay symmetric at every size.
render_padded() { # <canvas px> <out file>
  local s=$1 out=$2 c off
  c=$(((s * 103 + 64) / 128))
  if (((s - c) % 2)); then c=$((c + 1)); fi
  off=$(((s - c) / 2))
  rsvg-convert -w "$c" -h "$c" --page-width "$s" --page-height "$s" \
    --left "$off" --top "$off" "$SVG" -o "$out"
}

echo "==> macOS .icns (padded)"
ICONSET=$(mktemp -d)/icon.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  render_padded "$s"          "$ICONSET/icon_${s}x${s}.png"
  render_padded "$((s * 2))"  "$ICONSET/icon_${s}x${s}@2x.png"
done
if command -v iconutil >/dev/null; then
  iconutil -c icns "$ICONSET" -o assets/icon.icns
  echo "    wrote assets/icon.icns"
else
  echo "    iconutil not found (macOS only) — skipping .icns"
fi

echo "==> Windows .ico (full-bleed)"
# Every size is its own vector render: crisper small frames than letting
# Pillow downscale one 256px raster.
python3 - "$SVG" <<'PY'
import subprocess, sys, tempfile, os
from PIL import Image
svg = sys.argv[1]
sizes = [16, 32, 48, 64, 128, 256]
tmp = tempfile.mkdtemp()
frames = []
for s in sizes:
    png = os.path.join(tmp, f"{s}.png")
    subprocess.run(["rsvg-convert", "-w", str(s), "-h", str(s), svg, "-o", png], check=True)
    frames.append(Image.open(png).convert("RGBA"))
frames[-1].save(
    "assets/icon.ico", format="ICO",
    sizes=[(s, s) for s in sizes], append_images=frames[:-1])
print("    wrote assets/icon.ico")
PY

echo "==> macOS 26+ Assets.car (liquid glass)"
# actool ships with full Xcode, not the Command Line Tools. Fall back to the
# default Xcode install when the active developer dir is CLT-only.
ACTOOL_DEV=""
HAVE_ACTOOL=yes
if ! xcrun --find actool >/dev/null 2>&1; then
  if DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun --find actool >/dev/null 2>&1; then
    ACTOOL_DEV=/Applications/Xcode.app/Contents/Developer
  else
    HAVE_ACTOOL=no
    echo "    actool not found (needs Xcode) — skipping Assets.car"
  fi
fi
if [ "$HAVE_ACTOOL" = yes ]; then
  CAR_OUT=$(mktemp -d)
  (
    cd assets
    env ${ACTOOL_DEV:+DEVELOPER_DIR="$ACTOOL_DEV"} xcrun actool icon.icon \
      --compile "$CAR_OUT" \
      --output-format human-readable-text --notices --warnings --errors \
      --output-partial-info-plist "$CAR_OUT/partial.plist" \
      --app-icon icon --include-all-app-icons \
      --enable-on-demand-resources NO --development-region en \
      --target-device mac --minimum-deployment-target 26.0 --platform macosx \
      >/dev/null
  )
  # actool also emits a legacy .icns and a partial Info.plist; both are
  # discarded — the committed icon.icns above covers old macOS (actool's stops
  # at 256px), and CFBundleIconName lives in assets/macos-info.plist.
  cp "$CAR_OUT/Assets.car" assets/Assets.car
  echo "    wrote assets/Assets.car"
fi

echo "done."
