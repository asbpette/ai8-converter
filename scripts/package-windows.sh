#!/usr/bin/env bash
# Cross-compile the Windows build on macOS/Linux and package it with a bundled
# Ghostscript (needed for EPS/PS input) and the MSVC runtime DLLs Ghostscript
# needs (app-local deployment, so nothing else has to be installed). Produces:
#   DIR/AI8-Converter-<ver>-windows-x64.zip            portable folder
#   DIR/AI8-Converter-<ver>-windows-x64-setup.exe      NSIS installer
# Both exes get the icon (scripts/windows/ai8.ico) and a version resource.
#
# Requirements: rustup target x86_64-pc-windows-gnu, mingw-w64 (incl. windres),
# 7zz (sevenzip), gh (GitHub CLI, for downloading Ghostscript), python3, zip,
# makensis (NSIS 3, `brew install makensis` / `apt install nsis`).
#
# Usage: scripts/package-windows.sh [--gs-version 10080] [--out DIR] [--no-gs]
#                                   [--no-installer] [--no-exe-resource]
set -euo pipefail

GS_VERSION=10080
OUT_DIR="$(pwd)/dist"
WITH_GS=1
WITH_INSTALLER=1
WITH_RES=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --gs-version) GS_VERSION="$2"; shift 2 ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    --no-gs) WITH_GS=0; shift ;;
    --no-installer) WITH_INSTALLER=0; shift ;;
    --no-exe-resource) WITH_RES=0; shift ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# 7-Zip is "7zz" on macOS/Homebrew and newer Debian/Ubuntu, "7z" with p7zip.
SEVENZIP="$(command -v 7zz || command -v 7z || true)"
TARGET=x86_64-pc-windows-gnu
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | cut -d'"' -f2)"
NAME="AI8-Converter-${VERSION}-windows-x64"
WINDIR="$ROOT/scripts/windows"
# Windows wants a 4-part numeric version (strip any -pre/+build suffix).
IFS=. read -r V1 V2 V3 _ <<<"${VERSION%%[-+]*}"
VERSION4="${V1:-0}.${V2:-0}.${V3:-0}.0"
VERSIONC="${V1:-0},${V2:-0},${V3:-0},0"
if [[ $WITH_INSTALLER == 1 ]] && ! command -v makensis >/dev/null; then
  echo "error: makensis not found (brew install makensis / apt install nsis)," >&2
  echo "       or pass --no-installer to build only the zip." >&2
  exit 1
fi
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Compile a Windows resource object (icon + VERSIONINFO) for one exe.
# Args: <out.o> <FileDescription> <OriginalFilename>
make_res() {
  local rc="$WORK/res/$(basename "$1" .o).rc"
  mkdir -p "$WORK/res"
  cp "$WINDIR/ai8.ico" "$WORK/res/ai8.ico"
  cat > "$rc" <<RC
#pragma code_page(65001)
#include <winver.h>
1 ICON "ai8.ico"
1 VERSIONINFO
FILEVERSION $VERSIONC
PRODUCTVERSION $VERSIONC
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "IOTEK"
      VALUE "FileDescription", "$2"
      VALUE "FileVersion", "$VERSION"
      VALUE "InternalName", "$3"
      VALUE "LegalCopyright", "Copyright (c) Asbjørn Pettersen. All rights reserved."
      VALUE "OriginalFilename", "$3"
      VALUE "ProductName", "AI8 Converter"
      VALUE "ProductVersion", "$VERSION"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
RC
  (cd "$WORK/res" && x86_64-w64-mingw32-windres -O coff -i "$(basename "$rc")" -o "$1")
}

echo "==> Building release binaries for $TARGET"
if [[ $WITH_RES == 1 ]]; then
  # The resource object is linked in via an extra link-arg passed only to the
  # final rustc invocation of each bin (cargo rustc), so nothing in src/ or
  # Cargo.toml is needed and dependencies are not rebuilt. Skip this with
  # --no-exe-resource if the crate ever gets its own build.rs resources
  # (duplicate ICON/VERSIONINFO resources would fail to link).
  make_res "$WORK/res/gui.o" "AI8 Converter" "AI8 Converter.exe"
  make_res "$WORK/res/cli.o" "AI8 Converter command-line tool" "ai8conv.exe"
  (cd "$ROOT" && cargo rustc --release --target "$TARGET" --bin ai8-converter -- -C "link-arg=$WORK/res/gui.o")
  (cd "$ROOT" && cargo rustc --release --target "$TARGET" --bin ai8conv -- -C "link-arg=$WORK/res/cli.o")
else
  (cd "$ROOT" && cargo build --release --target "$TARGET" --bins)
fi
BIN="$ROOT/target/$TARGET/release"

PKG="$WORK/$NAME"
mkdir -p "$PKG"
cp "$BIN/ai8-converter.exe" "$PKG/AI8 Converter.exe"
cp "$BIN/ai8conv.exe" "$PKG/ai8conv.exe"

if [[ $WITH_GS == 1 ]]; then
  [[ -n "$SEVENZIP" ]] || { echo "error: 7-Zip (7zz or 7z) is required to unpack Ghostscript" >&2; exit 1; }
  echo "==> Downloading Ghostscript gs${GS_VERSION}w64.exe"
  gh release download "gs${GS_VERSION}" -R ArtifexSoftware/ghostpdl-downloads \
    -p "gs${GS_VERSION}w64.exe" -D "$WORK" --clobber
  mkdir -p "$WORK/gsx"
  "$SEVENZIP" x -y -o"$WORK/gsx" "$WORK/gs${GS_VERSION}w64.exe" >/dev/null

  mkdir -p "$PKG/gs/bin"
  cp "$WORK/gsx/bin/gswin64c.exe" "$WORK/gsx/bin/gsdll64.dll" "$PKG/gs/bin/"
  cp -R "$WORK/gsx/Resource" "$WORK/gsx/lib" "$WORK/gsx/iccprofiles" "$PKG/gs/"
  cp "$WORK/gsx/doc/COPYING" "$PKG/gs/COPYING-Ghostscript-AGPL.txt"

  echo "==> Extracting the x64 MSVC runtime from Ghostscript's vcredist_x64.exe"
  # The redistributable is a WiX Burn bundle; carve out its embedded cabinets
  # and pick the x64 runtime cab containing msvcp140.dll.
  python3 -I - "$WORK/gsx/vcredist_x64.exe" "$WORK/cabs" <<'PY'
import os, struct, sys
data = open(sys.argv[1], "rb").read()
out = sys.argv[2]
os.makedirs(out, exist_ok=True)
i = n = 0
while (i := data.find(b"MSCF\0\0\0\0", i)) >= 0:
    size = struct.unpack("<I", data[i + 8:i + 12])[0]
    if 0 < size <= len(data) - i:
        open(os.path.join(out, f"c{n}.cab"), "wb").write(data[i:i + size])
        n += 1
    i += 4
PY
  found=0
  for cab in "$WORK"/cabs/*.cab; do
    d="${cab%.cab}"; mkdir -p "$d"; "$SEVENZIP" x -y -o"$d" "$cab" >/dev/null || continue
    for inner in "$d"/*; do
      [[ -f "$inner" ]] || continue
      listing="$("$SEVENZIP" l "$inner" 2>/dev/null || true)"
      if [[ "$listing" == *msvcp140.dll* ]]; then
        rt="$inner.x"; mkdir -p "$rt"; "$SEVENZIP" x -y -o"$rt" "$inner" >/dev/null
        if file "$rt/msvcp140.dll" | grep -q "x86-64"; then
          cp "$rt"/msvcp140.dll "$rt"/vcruntime140.dll "$rt"/vcruntime140_1.dll "$PKG/gs/bin/"
          found=1; break 2
        fi
      fi
    done
  done
  if [[ $found == 0 ]]; then
    echo "warning: could not extract the MSVC runtime; Ghostscript may need the" >&2
    echo "         Visual C++ Redistributable installed on the target PC." >&2
  fi
fi

cat > "$PKG/README.txt" <<'TXT'
AI8 Converter
=============

Converts EPS, PS, PDF, AI SVG and DXF vector files to Adobe Illustrator 8 (.ai).

How to use
----------
1. Unzip this folder anywhere (keep the "gs" folder next to the program).
2. Double-click "AI8 Converter.exe".
3. Drag files onto the window (or click "Add files..."), then click "Convert".
   The .ai files are written next to the originals unless you choose an
   output folder.

Command line:  ai8conv.exe --help

Windows may show a SmartScreen warning the first time because the program is
not code-signed: click "More info" -> "Run anyway".

Requires 64-bit Windows 10 or 11.

Ghostscript (in the "gs" folder) is used to read EPS/PostScript files. It is
licensed under the GNU AGPL; see gs\COPYING-Ghostscript-AGPL.txt. Source:
https://github.com/ArtifexSoftware/ghostpdl
TXT
# Windows line endings for Notepad.
sed -i '' 's/$/\r/' "$PKG/README.txt" 2>/dev/null || sed -i 's/$/\r/' "$PKG/README.txt"

mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"
rm -f "$OUT_DIR/$NAME.zip"
(cd "$WORK" && zip -qr -X "$OUT_DIR/$NAME.zip" "$NAME")
echo "==> $OUT_DIR/$NAME.zip ($(du -h "$OUT_DIR/$NAME.zip" | cut -f1))"

if [[ $WITH_INSTALLER == 1 ]]; then
  # The installer packs the very same staged folder ($PKG). Note: the
  # uninstall section in installer.nsi lists the top-level files explicitly;
  # keep it in sync if files are added to the package root.
  echo "==> Building NSIS installer"
  LICENSE="$WORK/installer-license.txt"
  {
    cat <<TXT
AI8 Converter $VERSION
Copyright (c) Asbjørn Pettersen. All rights reserved.

This installs AI8 Converter, which converts EPS, PS, PDF, AI SVG and DXF vector
files to Adobe Illustrator 8 (.ai) files.

The software is provided "as is", without warranty of any kind.
TXT
    if [[ $WITH_GS == 1 ]]; then
      cat <<'TXT'

------------------------------------------------------------------------
Ghostscript
------------------------------------------------------------------------
AI8 Converter includes an unmodified copy of Artifex Ghostscript (installed
in the "gs" subfolder), which is used to read EPS/PostScript files and runs
as a separate program. Ghostscript is licensed under the GNU Affero General
Public License v3, reproduced below. Source code:
https://github.com/ArtifexSoftware/ghostpdl

TXT
      cat "$PKG/gs/COPYING-Ghostscript-AGPL.txt"
    fi
  } > "$LICENSE"
  SETUP="$OUT_DIR/$NAME-setup.exe"
  rm -f "$SETUP"
  makensis -V2 -INPUTCHARSET UTF8 \
    -DVERSION="$VERSION" -DVERSION4="$VERSION4" \
    -DSRCDIR="$PKG" -DLICENSEFILE="$LICENSE" \
    -DICONFILE="$WINDIR/ai8.ico" -DOUTFILE="$SETUP" \
    -DPANELBMP="$WINDIR/installer-panel.bmp" -DHEADERBMP="$WINDIR/installer-header.bmp" \
    "$WINDIR/installer.nsi"
  echo "==> $SETUP ($(du -h "$SETUP" | cut -f1))"
fi
