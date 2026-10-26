#!/usr/bin/env bash
# Regenerate assets/cmyk-srgb.lut: Ghostscript's CMYK -> sRGB conversion
# (its default CMYK and sRGB ICC profiles) sampled on a 9x9x9x9 grid, used by
# the SVG writer. 6561 RGB triples, index ((c*9 + m)*9 + y)*9 + k, levels
# 0, 1/8, ..., 1.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GS="${AI8_GHOSTSCRIPT:-gs}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
# One 1x1 pt patch per sample: x = c*9 + m, y (from the top) = y*9 + k.
cat > "$WORK/grid.ps" <<'PS'
<< /PageSize [81 81] >> setpagedevice
0 1 8 { /c exch def
  0 1 8 { /m exch def
    0 1 8 { /y exch def
      0 1 8 { /k exch def
        c 8 div m 8 div y 8 div k 8 div setcmykcolor
        c 9 mul m add  80 y 9 mul k add sub  1 1 rectfill
      } for } for } for } for
showpage
PS
"$GS" -q -dNOPAUSE -dBATCH -dSAFER -sDEVICE=ppmraw -r72 -dGraphicsAlphaBits=1 \
  -sOutputFile="$WORK/grid.ppm" "$WORK/grid.ps"
python3 -I - "$WORK/grid.ppm" "$ROOT/assets/cmyk-srgb.lut" <<'PY'
import sys
data = open(sys.argv[1], "rb").read()
# Header: P6, width, height, maxval (with optional # comments), then pixels.
fields, pos = [], 0
while len(fields) < 4:
    while data[pos:pos + 1].isspace():
        pos += 1
    if data[pos:pos + 1] == b"#":
        pos = data.index(b"\n", pos) + 1
        continue
    end = pos
    while not data[end:end + 1].isspace():
        end += 1
    fields.append(data[pos:end])
    pos = end
assert fields[0] == b"P6" and int(fields[1]) == 81 and int(fields[2]) == 81, fields
px = data[pos + 1:]
out = bytearray()
for c in range(9):
    for m in range(9):
        for y in range(9):
            for k in range(9):
                col, row = c * 9 + m, y * 9 + k
                i = (row * 81 + col) * 3
                out += px[i:i + 3]
open(sys.argv[2], "wb").write(out)
PY
echo "wrote $ROOT/assets/cmyk-srgb.lut"
