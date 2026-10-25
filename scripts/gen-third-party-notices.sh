#!/usr/bin/env bash
# Generate THIRD-PARTY-NOTICES.txt from the resolved Cargo dependency graph.
#
# MIT, Apache-2.0, BSD and OFL all require that the copyright notice and the
# licence text travel with *binary* distributions, so the generated file ships
# in the Windows and macOS packages. Dev-dependencies are excluded: they are
# not linked into the shipped binaries.
#
# Usage: scripts/gen-third-party-notices.sh [out.txt]
# Requires: cargo, python3, and a populated ~/.cargo/registry (run `cargo fetch`).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/THIRD-PARTY-NOTICES.txt}"
cd "$ROOT"
cargo fetch --quiet
cargo metadata --format-version 1 --all-features \
  | python3 "$ROOT/scripts/third_party_notices.py" > "$OUT"
echo "==> $OUT ($(wc -l < "$OUT") lines, $(du -h "$OUT" | cut -f1))"
