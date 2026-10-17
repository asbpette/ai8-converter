#!/usr/bin/env bash
# Build a self-contained Ghostscript for macOS from the official source tarball.
#
# The result depends only on macOS system libraries (/usr/lib, /System): all
# third-party libraries (jpeg, libpng, zlib, lcms2mt, freetype, openjpeg,
# jbig2dec) are compiled from the copies shipped in the Ghostscript source
# tree, and the PostScript init files / Resource directory are compiled into
# the executable (COMPILE_INITS), so a lone bin/gs works anywhere.
#
# Usage: scripts/macos/build-ghostscript.sh [options]
#   --arch A      universal (default), arm64 or x86_64
#   --prefix DIR  output directory (default: target/ghostscript/gs-<ver>-macos-<arch>)
#                 receives bin/gs, COPYING (AGPL-3.0), LICENSE and BUILDINFO.txt
#   --work DIR    download/build directory (default: target/ghostscript/work)
#   --jobs N      parallel make jobs (default: number of CPUs)
#   --force       rebuild even if PREFIX/bin/gs already exists
#
# Requirements: Xcode command line tools (clang, lipo), curl, shasum.
# x86_64 is cross-compiled on Apple Silicon (no Rosetta needed) and vice versa.
set -euo pipefail

GS_VERSION="10.08.0"
GS_TAG="gs10080"
GS_TARBALL="ghostscript-${GS_VERSION}.tar.xz"
GS_SHA256="c20492bc8ebb96c87fa2e52a0926e1cda8cde95d66145e018ac713fed5da38cf"
GS_URL="https://github.com/ArtifexSoftware/ghostpdl-downloads/releases/download/${GS_TAG}"
MIN_MACOS="11.0"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ARCH="universal"
PREFIX=""
WORK=""
JOBS="$(sysctl -n hw.ncpu 2>/dev/null || echo 4)"
FORCE=0
while [ $# -gt 0 ]; do
    case "$1" in
        --arch) ARCH="$2"; shift ;;
        --prefix) PREFIX="$2"; shift ;;
        --work) WORK="$2"; shift ;;
        --jobs) JOBS="$2"; shift ;;
        --force) FORCE=1 ;;
        --version) echo "$GS_VERSION"; exit 0 ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

case "$ARCH" in
    universal) ARCHS="arm64 x86_64" ;;
    arm64|x86_64) ARCHS="$ARCH" ;;
    *) echo "--arch must be universal, arm64 or x86_64" >&2; exit 2 ;;
esac

PREFIX="${PREFIX:-$ROOT/target/ghostscript/gs-${GS_VERSION}-macos-${ARCH}}"
WORK="${WORK:-$ROOT/target/ghostscript/work}"
mkdir -p "$PREFIX" "$WORK"
PREFIX="$(cd "$PREFIX" && pwd)"
WORK="$(cd "$WORK" && pwd)"

if [ "$FORCE" = 0 ] && [ -x "$PREFIX/bin/gs" ] && grep -q "version=$GS_VERSION arch=$ARCH" "$PREFIX/BUILDINFO.txt" 2>/dev/null; then
    echo "Ghostscript $GS_VERSION ($ARCH) already built in $PREFIX (use --force to rebuild)"
    exit 0
fi

# Keep Homebrew/MacPorts and user overrides out of the build so nothing
# non-system is picked up by configure or the linker.
export PATH="/usr/bin:/bin:/usr/sbin:/sbin"
unset CPATH C_INCLUDE_PATH LIBRARY_PATH PKG_CONFIG_PATH PKG_CONFIG_LIBDIR CFLAGS CPPFLAGS LDFLAGS LIBS CC
export MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS"
export PKG_CONFIG=/usr/bin/false

# ---- download + verify -------------------------------------------------------
TARBALL="$WORK/$GS_TARBALL"
if [ ! -f "$TARBALL" ] || [ "$(shasum -a 256 "$TARBALL" | cut -d' ' -f1)" != "$GS_SHA256" ]; then
    echo "Downloading $GS_URL/$GS_TARBALL"
    curl -fL --retry 3 -o "$TARBALL.part" "$GS_URL/$GS_TARBALL"
    mv "$TARBALL.part" "$TARBALL"
fi
ACTUAL="$(shasum -a 256 "$TARBALL" | cut -d' ' -f1)"
if [ "$ACTUAL" != "$GS_SHA256" ]; then
    echo "SHA-256 mismatch for $GS_TARBALL: expected $GS_SHA256, got $ACTUAL" >&2
    exit 1
fi
# Also cross-check against the SHA512SUMS published with the release (best effort).
if curl -fsSL --retry 2 -o "$WORK/SHA512SUMS" "$GS_URL/SHA512SUMS" 2>/dev/null; then
    (cd "$WORK" && grep " $GS_TARBALL\$" SHA512SUMS | shasum -a 512 -c -) || { echo "SHA-512 check failed" >&2; exit 1; }
fi
echo "Verified $GS_TARBALL (sha256 $GS_SHA256)"

HOST_ARCH="$(uname -m)"
[ "$HOST_ARCH" = "aarch64" ] && HOST_ARCH=arm64
SDK="$(xcrun --sdk macosx --show-sdk-path)"

build_one() {
    local arch="$1"
    local src="$WORK/build-$arch/ghostscript-$GS_VERSION"
    rm -rf "$WORK/build-$arch"
    mkdir -p "$WORK/build-$arch"
    tar -xf "$TARBALL" -C "$WORK/build-$arch"

    local triple="${arch/arm64/aarch64}-apple-darwin"
    local build_triple="${HOST_ARCH/arm64/aarch64}-apple-darwin"
    local cross=()
    if [ "$arch" != "$HOST_ARCH" ]; then
        # Cross build: aux tools (genconf, mkromfs, ...) are built for the host
        # with CCAUX; macOS uses a pre-generated multi-arch arch.h.
        cross=(--host="$triple" --build="$build_triple" CCAUX="clang -isysroot $SDK")
    fi

    echo "==> Configuring Ghostscript $GS_VERSION for $arch"
    (
        cd "$src"
        ./configure \
            CC="clang -arch $arch -isysroot $SDK -mmacosx-version-min=$MIN_MACOS" \
            ${cross[@]+"${cross[@]}"} \
            --prefix=/usr/local \
            --without-x --disable-gtk --disable-cups --without-pdftoraster \
            --disable-dbus --disable-fontconfig \
            --without-libidn --without-libpaper --without-tesseract \
            --without-libtiff --without-ijs --without-urf \
            --with-libiconv=no --disable-contrib \
            --with-local-zlib \
            --with-drivers=PS
    ) > "$WORK/configure-$arch.log" 2>&1 || { tail -40 "$WORK/configure-$arch.log"; exit 1; }

    echo "==> Building Ghostscript for $arch ($JOBS jobs)"
    (cd "$src" && make -j"$JOBS") > "$WORK/make-$arch.log" 2>&1 || { tail -60 "$WORK/make-$arch.log"; exit 1; }
    lipo -info "$src/bin/gs"
}

START=$(date +%s)
BINS=()
for a in $ARCHS; do
    build_one "$a"
    BINS+=("$WORK/build-$a/ghostscript-$GS_VERSION/bin/gs")
done

rm -rf "$PREFIX/bin"
mkdir -p "$PREFIX/bin"
if [ "${#BINS[@]}" -gt 1 ]; then
    lipo -create -output "$PREFIX/bin/gs" "${BINS[@]}"
else
    cp "${BINS[0]}" "$PREFIX/bin/gs"
fi
strip -x "$PREFIX/bin/gs" 2>/dev/null || true
chmod 755 "$PREFIX/bin/gs"

SRC0="$WORK/build-${ARCHS%% *}/ghostscript-$GS_VERSION"
cp "$SRC0/doc/COPYING" "$PREFIX/COPYING"
cp "$SRC0/LICENSE" "$PREFIX/LICENSE"

# ---- sanity checks ------------------------------------------------------------
if otool -L "$PREFIX/bin/gs" | grep -E '^[[:space:]]' | grep -v -E '^[[:space:]]+(/usr/lib/|/System/)' | grep -q .; then
    echo "ERROR: gs links against non-system libraries:" >&2
    otool -L "$PREFIX/bin/gs" >&2
    exit 1
fi
"$PREFIX/bin/gs" -q -dNODISPLAY -dBATCH -dNOPAUSE -c 'product == revision ==' >/dev/null
TMPD="$(mktemp -d)"
printf '%%!PS-Adobe-3.0 EPSF-3.0\n%%%%BoundingBox: 0 0 100 100\n/Helvetica 12 selectfont 10 10 moveto (ok) show 0 0 moveto 100 100 lineto stroke showpage\n' > "$TMPD/t.eps"
(cd "$TMPD" && env -i PATH=/usr/bin:/bin "$PREFIX/bin/gs" -q -dSAFER -dBATCH -dNOPAUSE -dNoOutputFonts -dEPSCrop \
    -sDEVICE=pdfwrite -sOutputFile=t.pdf t.eps) && head -c 5 "$TMPD/t.pdf" | grep -q '%PDF-'
rm -rf "$TMPD"

cat > "$PREFIX/BUILDINFO.txt" <<EOF
Ghostscript $GS_VERSION built from $GS_URL/$GS_TARBALL
sha256 $GS_SHA256
version=$GS_VERSION arch=$ARCH
architectures: $(lipo -archs "$PREFIX/bin/gs")
minimum macOS: $MIN_MACOS
built: $(date -u +%Y-%m-%dT%H:%M:%SZ) in $(( $(date +%s) - START ))s
Ghostscript is licensed under the GNU Affero General Public License v3
(see COPYING). Source: https://github.com/ArtifexSoftware/ghostpdl-downloads/releases/tag/$GS_TAG
EOF

echo "Ghostscript ready: $PREFIX/bin/gs ($(lipo -archs "$PREFIX/bin/gs"), $(du -h "$PREFIX/bin/gs" | cut -f1))"
