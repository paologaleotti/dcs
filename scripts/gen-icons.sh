#!/usr/bin/env bash
# Regenerate all platform icon files from assets/icon.svg.
#
# Outputs (committed to the repo so CI consumes them directly):
#   assets/icon.png        1024px master raster (Linux .deb/AppImage + packager)
#   assets/icon-256.png    embedded in the binary as the runtime window icon
#   assets/icon.icns       macOS .app / .dmg
#   assets/icon.ico        Windows .exe / installers
#
# Deps: rsvg-convert (librsvg), iconutil (macOS), python3 + Pillow.
set -euo pipefail
cd "$(dirname "$0")/.."

SVG=assets/icon.svg
test -f "$SVG" || { echo "missing $SVG"; exit 1; }
command -v rsvg-convert >/dev/null || { echo "need rsvg-convert (brew install librsvg)"; exit 1; }

echo "==> PNG masters"
rsvg-convert -w 1024 -h 1024 "$SVG" -o assets/icon.png
rsvg-convert -w 256  -h 256  "$SVG" -o assets/icon-256.png

echo "==> macOS .icns"
ICONSET=$(mktemp -d)/icon.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  rsvg-convert -w "$s"          -h "$s"          "$SVG" -o "$ICONSET/icon_${s}x${s}.png"
  rsvg-convert -w "$((s*2))"    -h "$((s*2))"    "$SVG" -o "$ICONSET/icon_${s}x${s}@2x.png"
done
if command -v iconutil >/dev/null; then
  iconutil -c icns "$ICONSET" -o assets/icon.icns
  echo "    wrote assets/icon.icns"
else
  echo "    iconutil not found (macOS only) — skipping .icns"
fi

echo "==> Windows .ico"
python3 - "$SVG" <<'PY'
import subprocess, sys, tempfile, os
from PIL import Image
svg = sys.argv[1]
sizes = [16, 32, 48, 64, 128, 256]
tmp = tempfile.mkdtemp()
master = os.path.join(tmp, "256.png")
subprocess.run(["rsvg-convert", "-w", "256", "-h", "256", svg, "-o", master], check=True)
Image.open(master).convert("RGBA").save(
    "assets/icon.ico", format="ICO", sizes=[(s, s) for s in sizes])
print("    wrote assets/icon.ico")
PY

echo "done."
