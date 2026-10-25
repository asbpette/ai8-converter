#!/usr/bin/env bash
# Package "AI8 Converter.app" into a compressed drag-to-install DMG.
#
# Usage: scripts/macos/make-dmg.sh [--app PATH] [--arch LABEL] [--out DIR]
#   --app PATH    the .app to package (default: target/release/bundle/AI8 Converter.app)
#   --arch LABEL  label used in the file name (default: derived from the app binary:
#                 universal, arm64 or x86_64)
#   --out DIR     output directory (default: dist)
#
# Output: DIR/AI8-Converter-<version>-macos-<arch>.dmg containing the app, an
# /Applications symlink and "Read Me First.txt" (scripts/macos/README-install.txt).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

APP_NAME="AI8 Converter"
APP="target/release/bundle/$APP_NAME.app"
ARCH=""
OUT="dist"
while [ $# -gt 0 ]; do
    case "$1" in
        --app) APP="$2"; shift ;;
        --arch) ARCH="$2"; shift ;;
        --out) OUT="$2"; shift ;;
        -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

[ -d "$APP" ] || { echo "app not found: $APP (run scripts/bundle-macos.sh first)" >&2; exit 1; }

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
if [ -z "$ARCH" ]; then
    EXE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist")"
    ARCHS="$(lipo -archs "$APP/Contents/MacOS/$EXE")"
    case "$ARCHS" in
        *x86_64*arm64*|*arm64*x86_64*) ARCH=universal ;;
        *) ARCH="$ARCHS" ;;
    esac
fi
# No bundled Ghostscript is the normal case for a release: it is AGPL software
# owned by Artifex and the user installs it themselves. Only note it.
if [ ! -x "$APP/Contents/Resources/gs/bin/gs" ]; then
    echo "note: no bundled Ghostscript; EPS/PS input needs one installed" >&2
fi

mkdir -p "$OUT"
DMG="$OUT/AI8-Converter-$VERSION-macos-$ARCH.dmg"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

ditto "$APP" "$STAGE/$APP_NAME.app"
ln -s /Applications "$STAGE/Applications"
cp scripts/macos/README-install.txt "$STAGE/Read Me First.txt"
cp docs/installing-ghostscript.html "$STAGE/Installing Ghostscript.html"

rm -f "$DMG"
hdiutil create -volname "$APP_NAME $VERSION" -srcfolder "$STAGE" -fs HFS+ \
    -format UDZO -imagekey zlib-level=9 -ov "$DMG" >/dev/null
hdiutil verify -quiet "$DMG"

echo "DMG:   $DMG ($(du -h "$DMG" | cut -f1))"
