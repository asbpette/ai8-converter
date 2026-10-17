#!/usr/bin/env bash
# Build "AI8 Converter.app" for macOS.
#
# Usage: scripts/bundle-macos.sh [--universal] [--gs-dir DIR]
#   --universal   build for both Apple Silicon and Intel and merge with lipo
#                 (needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin)
#   --gs-dir DIR  copy a self-contained Ghostscript install (DIR/bin/gs + its
#                 resources) into the bundle at Contents/Resources/gs
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
while [ $# -gt 0 ]; do
    case "$1" in
        --universal) UNIVERSAL=1 ;;
        --gs-dir) GS_DIR="$2"; shift ;;
        -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

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

if [ -n "$GS_DIR" ]; then
    cp -R "$GS_DIR" "$APP/Contents/Resources/gs"
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

# Ad-hoc signature so the bundle runs on Apple Silicon after copying.
if command -v codesign >/dev/null 2>&1; then
    codesign --force --deep --sign - "$APP"
fi

ZIP="$OUT_DIR/AI8-Converter-macOS.zip"
rm -f "$ZIP"
(cd "$OUT_DIR" && ditto -c -k --keepParent "$APP_NAME.app" "$(basename "$ZIP")")

echo "Built: $APP"
echo "Zip:   $ZIP"
