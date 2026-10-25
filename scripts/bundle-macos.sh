#!/usr/bin/env bash
# Build "AI8 Converter.app" for macOS.
#
# Usage: scripts/bundle-macos.sh [--universal] [--with-gs | --gs-dir DIR] [--dmg]
#   --universal   build for both Apple Silicon and Intel and merge with lipo
#                 (needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin)
#   --with-gs     build (or reuse the cached build of) a self-contained Ghostscript
#                 with scripts/macos/build-ghostscript.sh and bundle it at
#                 Contents/Resources/gs, so the app needs nothing installed
#   --gs-dir DIR  bundle an existing self-contained Ghostscript (DIR/bin/gs) instead
#   --dmg         also create dist/AI8-Converter-<version>-macos-<arch>.dmg
#
# Output: target/release/bundle/AI8 Converter.app (and a .zip next to it).
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="AI8 Converter"
BUNDLE_ID="no.iotek.ai8converter"
BIN="ai8-converter"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)"

UNIVERSAL=0
GS_DIR=""
WITH_GS=0
DMG=0
while [ $# -gt 0 ]; do
    case "$1" in
        --universal) UNIVERSAL=1 ;;
        --gs-dir) GS_DIR="$2"; shift ;;
        --with-gs) WITH_GS=1 ;;
        --dmg) DMG=1 ;;
        -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

if [ "$UNIVERSAL" = 1 ]; then
    ARCH_LABEL="universal"
else
    ARCH_LABEL="$(uname -m)"
    [ "$ARCH_LABEL" = "aarch64" ] && ARCH_LABEL=arm64
fi

if [ "$WITH_GS" = 1 ]; then
    if [ -n "$GS_DIR" ]; then
        echo "--with-gs and --gs-dir are mutually exclusive" >&2
        exit 2
    fi
    GS_VERSION="$(scripts/macos/build-ghostscript.sh --version)"
    GS_DIR="target/ghostscript/gs-${GS_VERSION}-macos-${ARCH_LABEL}"
    # Cached: returns immediately when GS_DIR already holds a matching build.
    scripts/macos/build-ghostscript.sh --arch "$ARCH_LABEL" --prefix "$GS_DIR"
fi

if [ "$UNIVERSAL" = 1 ]; then
    cargo build --release --bins --target aarch64-apple-darwin
    cargo build --release --bins --target x86_64-apple-darwin
    mkdir -p target/release/universal
    for b in "$BIN" ai8conv; do
        lipo -create -output "target/release/universal/$b" \
            "target/aarch64-apple-darwin/release/$b" "target/x86_64-apple-darwin/release/$b"
    done
    BIN_DIR="target/release/universal"
else
    cargo build --release --bins
    BIN_DIR="target/release"
fi

OUT_DIR="target/release/bundle"
APP="$OUT_DIR/$APP_NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN_DIR/$BIN" "$APP/Contents/MacOS/$BIN"
cp "$BIN_DIR/ai8conv" "$APP/Contents/MacOS/ai8conv"
cp "assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"

# MIT requires the notice to travel with binaries; so do the crate licences.
cp LICENSE "$APP/Contents/Resources/LICENSE.txt"
if [ -f THIRD-PARTY-NOTICES.txt ]; then
    cp THIRD-PARTY-NOTICES.txt "$APP/Contents/Resources/THIRD-PARTY-NOTICES.txt"
else
    echo "warning: THIRD-PARTY-NOTICES.txt missing; run scripts/gen-third-party-notices.sh" >&2
fi

if [ -n "$GS_DIR" ]; then
    if [ ! -x "$GS_DIR/bin/gs" ]; then
        echo "no executable $GS_DIR/bin/gs" >&2
        exit 1
    fi
    GS_DEST="$APP/Contents/Resources/gs"
    mkdir -p "$GS_DEST"
    cp -R "$GS_DIR/." "$GS_DEST/"
    # Refuse to ship a gs that depends on Homebrew/MacPorts libraries.
    if otool -L "$GS_DEST/bin/gs" | grep -E '^[[:space:]]' | grep -v -E '^[[:space:]]+(/usr/lib/|/System/)' | grep -q .; then
        echo "ERROR: $GS_DIR/bin/gs links against non-system libraries; use --with-gs" >&2
        otool -L "$GS_DEST/bin/gs" >&2
        exit 1
    fi
    if [ ! -f "$GS_DEST/COPYING" ]; then
        echo "warning: $GS_DIR has no COPYING (AGPL) file" >&2
    fi
    cat > "$APP/Contents/Resources/GHOSTSCRIPT-NOTICE.txt" <<'EOF'
The following applies ONLY to Contents/Resources/gs, not to AI8 Converter
itself, which is MIT licensed; see LICENSE.txt.

This build of AI8 Converter bundles Ghostscript (Artifex Software, Inc.) in
Contents/Resources/gs. It is used as a separate program to read EPS/PostScript
input and is licensed under the GNU Affero General Public License v3; see
Contents/Resources/gs/COPYING and Contents/Resources/gs/LICENSE.

The exact upstream source tarball and its checksum are listed in
Contents/Resources/gs/BUILDINFO.txt; the build recipe is
scripts/macos/build-ghostscript.sh in the AI8 Converter source tree.
Ghostscript source: https://github.com/ArtifexSoftware/ghostpdl-downloads/releases
EOF
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundleExecutable</key><string>$BIN</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>NSHumanReadableCopyright</key><string>Copyright (c) Asbjørn Pettersen. All rights reserved.</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.graphics-design</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

printf 'APPL????' > "$APP/Contents/PkgInfo"

# Ad-hoc signature so the bundle runs on Apple Silicon after copying. Helper
# executables outside Contents/MacOS (the bundled gs) are not reached by
# --deep, so sign them explicitly first, then seal the whole bundle.
if command -v codesign >/dev/null 2>&1; then
    if [ -f "$APP/Contents/Resources/gs/bin/gs" ]; then
        codesign --force --sign - --identifier "$BUNDLE_ID.ghostscript" "$APP/Contents/Resources/gs/bin/gs"
    fi
    codesign --force --sign - "$APP/Contents/MacOS/ai8conv"
    codesign --force --deep --sign - "$APP"
    codesign --verify --deep --strict "$APP"
fi

ZIP="$OUT_DIR/AI8-Converter-macOS.zip"
rm -f "$ZIP"
(cd "$OUT_DIR" && ditto -c -k --keepParent "$APP_NAME.app" "$(basename "$ZIP")")

echo "Built: $APP"
echo "Zip:   $ZIP"

if [ "$DMG" = 1 ]; then
    scripts/macos/make-dmg.sh --app "$APP" --arch "$ARCH_LABEL"
fi
