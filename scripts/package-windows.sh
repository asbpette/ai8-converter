#!/usr/bin/env bash
# Cross-compile the Windows build on macOS/Linux and package it with a bundled
# Ghostscript (needed for EPS/PS input) and the MSVC runtime DLLs Ghostscript
# needs (app-local deployment, so nothing else has to be installed). Produces:
#   DIR/AI8-Converter-<ver>-windows-x64.zip            portable folder
#   DIR/AI8-Converter-<ver>-windows-x64-setup.exe      NSIS installer
# Both exes get the icon (scripts/windows/ai8.ico) and a version resource.
#
# Requirements: rustup target x86_64-pc-windows-gnu, mingw-w64 (incl. windres),
# zip, makensis (NSIS 3, `brew install makensis` / `apt install nsis`).
#
# Usage: scripts/package-windows.sh [--out DIR] [--no-installer]
#                                   [--no-exe-resource]
#
# Ghostscript is never bundled. It is AGPL-3.0 software owned by Artifex, so
# shipping it would make the distributor an AGPL distributor, obliged to supply
# the corresponding source for the exact build. The package and the installer
# instead offer to install it from Artifex, so the user receives it directly
# from upstream under upstream's terms.
set -euo pipefail

OUT_DIR="$(pwd)/dist"
WITH_INSTALLER=1
WITH_RES=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT_DIR="$2"; shift 2 ;;
    --no-installer) WITH_INSTALLER=0; shift ;;
    --no-exe-resource) WITH_RES=0; shift ;;
    -h|--help) sed -n '2,21p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
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
# MIT requires the notice to travel with binaries; the crate licences too.
cp "$ROOT/LICENSE" "$PKG/LICENSE.txt"
cp "$ROOT/THIRD-PARTY-NOTICES.txt" "$PKG/THIRD-PARTY-NOTICES.txt"

# Ghostscript is NOT shipped. It is AGPL-3.0 software owned by Artifex, and
# redistributing it would make whoever ships this package an AGPL distributor,
# obliged to supply the corresponding source for the exact build. Instead the
# user installs it themselves from Artifex, so they receive it directly from
# upstream under upstream's terms and we convey nothing.
#
# This helper is for people who took the plain .zip; the NSIS installer offers
# the same thing as an optional step.
cat > "$PKG/Install Ghostscript.cmd" <<'CMD'
@echo off
rem Installs Ghostscript, which AI8 Converter needs to read EPS and PS files.
rem Ghostscript is separate software by Artifex Software, Inc., licensed under
rem the GNU AGPL v3. It is obtained from Artifex, not from AI8 Converter.
rem
rem No parenthesised blocks below on purpose: %errorlevel% expands at parse
rem time inside a block, so "if errorlevel N" + goto is the reliable form.
setlocal
echo AI8 Converter needs Ghostscript to read EPS and PS files.
echo Ghostscript is separate software by Artifex Software, Inc. - GNU AGPL v3.
echo.

where winget >nul 2>&1
if errorlevel 1 goto page

echo Installing with the Windows Package Manager...
winget install --exact --id ArtifexSoftware.GhostScript --accept-source-agreements --accept-package-agreements
if errorlevel 1 goto page

echo.
echo Done. Start AI8 Converter, or press "Re-detect" on its Ghostscript
echo line, and it will be picked up.
pause
exit /b 0

:page
echo.
echo Could not install it automatically. Opening Artifex's download page.
start "" https://ghostscript.com/releases/gsdnld.html
echo.
echo Download the 64-bit Ghostscript installer for Windows and run it, then
echo start AI8 Converter or press "Re-detect" on its Ghostscript line.
pause
exit /b 1
CMD
# Windows line endings, or cmd.exe mis-parses the file.
sed -i '' 's/$/\r/' "$PKG/Install Ghostscript.cmd" 2>/dev/null \
  || sed -i 's/$/\r/' "$PKG/Install Ghostscript.cmd"

cat > "$PKG/README.txt" <<'TXT'
AI8 Converter
=============

Converts EPS, PS, PDF, AI SVG and DXF vector files to Adobe Illustrator 8 (.ai).

How to use
----------
1. Unzip this folder anywhere.
2. Double-click "AI8 Converter.exe".
3. Drag files onto the window (or click "Add files..."), then click "Convert".
   The .ai files are written next to the originals unless you choose an
   output folder.

Command line:  ai8conv.exe --help

EPS and PS files
----------------
Reading EPS and PS needs Ghostscript, which is not included. Run
"Install Ghostscript.cmd" in this folder and it will install it with the
Windows Package Manager, or open the download page if that is unavailable.
PDF, AI, SVG and DXF files convert without it.

Windows may show a SmartScreen warning the first time because the program is
not code-signed: click "More info" -> "Run anyway".

Requires 64-bit Windows 10 or 11.

Licences
--------
AI8 Converter is free and open source under the MIT Licence; see LICENSE.txt.
Copyright (c) 2026 Asbjorn Pettersen. Notices for the open source libraries it
is built with are in THIRD-PARTY-NOTICES.txt.

Ghostscript is NOT included and is not part of AI8 Converter. It is separate
software by Artifex Software, Inc., licensed under the GNU Affero General
Public License v3, and you obtain it directly from Artifex, under their
licence. AI8 Converter runs it as a child process over its command line and
contains no Ghostscript code.
Download page: https://ghostscript.com/releases/gsdnld.html
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
Copyright (c) 2026 Asbjorn Pettersen

This installs AI8 Converter, which converts EPS, PS, PDF, AI SVG and DXF vector
files to Adobe Illustrator 8 (.ai) files.

AI8 Converter is free and open source software under the MIT Licence:

  Permission is hereby granted, free of charge, to any person obtaining a
  copy of this software and associated documentation files (the "Software"),
  to deal in the Software without restriction, including without limitation
  the rights to use, copy, modify, merge, publish, distribute, sublicense,
  and/or sell copies of the Software, and to permit persons to whom the
  Software is furnished to do so, subject to the following conditions:

  The above copyright notice and this permission notice shall be included in
  all copies or substantial portions of the Software.

  THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
  IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
  FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
  AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
  LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
  FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
  DEALINGS IN THE SOFTWARE.

Notices for the open source libraries AI8 Converter is built with are
installed as THIRD-PARTY-NOTICES.txt.

------------------------------------------------------------------------
About Ghostscript
------------------------------------------------------------------------
Reading EPS and PostScript files needs Ghostscript, which is NOT part of AI8
Converter and is not installed by this installer. It is separate software by
Artifex Software, Inc., licensed under the GNU Affero General Public License
v3. AI8 Converter runs it as a child process over its command-line interface
and contains no Ghostscript code.

The last page of this installer can install it for you using the Windows
Package Manager, or open Artifex's download page. Either way you obtain
Ghostscript directly from Artifex, under Artifex's own licence, which is
presented by their installer.
TXT
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
