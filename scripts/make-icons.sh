#!/usr/bin/env bash
# Regenerate every app icon from the SVG masters in assets/logo/:
#   icon.svg        - full logo (64 px and up)
#   icon-small.svg  - simplified glyph for 16-48 px
# Outputs (committed, so packaging needs no extra tools):
#   assets/icon-256.png      window icon (embedded in the app)
#   assets/AppIcon.icns      macOS bundle icon (needs iconutil, macOS only)
#   scripts/windows/ai8.ico  Windows exe + installer icon
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo build -q --release --example render_svg
R="target/release/examples/render_svg"
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT

render() { # svg size out
  "$R" "assets/logo/$1" "$3" "$2"
}
pick() { if [[ $1 -le 48 ]]; then echo icon-small.svg; else echo icon.svg; fi; }

render icon.svg 256 assets/icon-256.png

# Windows ICO: PNG-compressed entries (supported since Windows Vista).
for s in 16 20 24 32 40 48 64 128 256; do render "$(pick $s)" $s "$WORK/ico_$s.png"; done
python3 -I - "$WORK" scripts/windows/ai8.ico <<'PY'
import os, struct, sys
work, out = sys.argv[1], sys.argv[2]
sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
blobs = [open(os.path.join(work, f"ico_{s}.png"), "rb").read() for s in sizes]
header = struct.pack("<HHH", 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
entries = b""
for s, b in zip(sizes, blobs):
    entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(b), offset)
    offset += len(b)
open(out, "wb").write(header + entries + b"".join(blobs))
PY

# macOS ICNS via iconutil.
if command -v iconutil >/dev/null; then
  SET="$WORK/AppIcon.iconset"; mkdir -p "$SET"
  for s in 16 32 128 256 512; do
    render "$(pick $s)" $s "$SET/icon_${s}x${s}.png"
    render "$(pick $((s * 2)))" $((s * 2)) "$SET/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns "$SET" -o assets/AppIcon.icns
else
  echo "iconutil not found (not macOS): assets/AppIcon.icns left unchanged" >&2
fi
echo "icons written"
