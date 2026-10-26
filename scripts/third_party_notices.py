#!/usr/bin/env python3
"""Turn `cargo metadata` JSON (on stdin) into a THIRD-PARTY-NOTICES text file.

Walks the resolve graph from the root package over normal and build edges only,
so dev-dependencies are left out. For every crate it records the SPDX licence
and the copyright lines from the crate's own licence files, then prints each
distinct licence text once at the end.
"""
import json
import os
import re
import sys

# Matched case-insensitively against file names. OFL/UFL catch the font
# licences that crates such as epaint_default_fonts keep under fonts/.
LICENSE_PATTERNS = (
    "license", "licence", "copying", "notice", "unlicense", "ofl", "ufl",
)
SKIP_DIRS = {"target", "tests", "benches", "examples", "src", ".git"}
MAX_DEPTH = 2
# Real notices say "Copyright (c) 2024 Someone". Deliberately case-sensitive
# and anchored on a year/(c)/by, so Apache-2.0 body prose such as
# "copyright license to reproduce" is not mistaken for a notice.
COPYRIGHT_RE = re.compile(
    r"^[^\w\n]{0,4}(?:Copyright|COPYRIGHT)\b"
    r"(?=[^\n]*?(?:\(c\)|\(C\)|\u00a9|\bby\b|(?:19|20)\d\d))[^\n]*",
    re.M,
)


def crate_dir(pkg):
    """Path to the unpacked crate source, or None for path/git sources."""
    mani = pkg.get("manifest_path") or ""
    d = os.path.dirname(mani)
    return d if os.path.isdir(d) and "/registry/src/" in d else (d or None)


def license_files(d):
    """Licence/notice files in the crate root and up to MAX_DEPTH below it."""
    out = []
    if not d:
        return out
    base = d.rstrip(os.sep)
    for cur, dirs, files in os.walk(base):
        depth = cur[len(base):].count(os.sep)
        if depth >= MAX_DEPTH:
            dirs[:] = []
        else:
            dirs[:] = [x for x in dirs if x not in SKIP_DIRS
                       and not x.startswith(".")]
        for f in sorted(files):
            low = f.lower()
            if not any(k in low for k in LICENSE_PATTERNS):
                continue
            if low.endswith((".rs", ".toml", ".ttf", ".otf", ".py")):
                continue
            p = os.path.join(cur, f)
            if os.path.isfile(p) and 0 < os.path.getsize(p) < 200_000:
                out.append(p)
    return sorted(out)


def main():
    md = json.load(sys.stdin)
    pkgs = {p["id"]: p for p in md["packages"]}
    nodes = {n["id"]: n for n in md["resolve"]["nodes"]}
    roots = set(md.get("workspace_members") or [])

    # Walk normal + build edges; skip "dev".
    keep, stack = set(), list(roots)
    while stack:
        pid = stack.pop()
        if pid in keep:
            continue
        keep.add(pid)
        for dep in nodes.get(pid, {}).get("deps", []):
            kinds = {k.get("kind") for k in dep.get("dep_kinds", [{"kind": None}])}
            if kinds <= {"dev"}:
                continue
            stack.append(dep["pkg"])

    shipped = sorted(
        (pkgs[i] for i in keep - roots),
        key=lambda p: (p["name"].lower(), p["version"]),
    )

    texts = {}   # licence text -> set of "name version"
    rows = []
    for p in shipped:
        label = f"{p['name']} {p['version']}"
        spdx = p.get("license") or "(see licence file)"
        d = crate_dir(p)
        holders, found = [], False
        for f in license_files(d):
            found = True
            body = open(f, encoding="utf-8", errors="replace").read()
            holders += [m.group(0).strip() for m in COPYRIGHT_RE.finditer(body)]
            texts.setdefault(body.strip(), set()).add(label)
        rows.append((label, spdx, sorted(dict.fromkeys(holders)), found))

    w = sys.stdout.write
    w("THIRD-PARTY NOTICES for AI8 Converter\n")
    w("=" * 72 + "\n\n")
    w("AI8 Converter itself is MIT licensed; see LICENSE.\n\n")
    w("The compiled program includes code from the %d packages listed below.\n"
      % len(rows))
    w("Each package's licence and copyright notices are reproduced here as\n"
      "those licences require. Licence texts shared by several packages are\n"
      "printed once, at the end, with the packages they apply to.\n\n")
    w("Ghostscript is NOT covered by this file: it is a separate program under\n"
      "the GNU AGPL v3 and is not bundled with published releases. When a build\n"
      "does bundle it, its COPYING file and source location ship alongside it.\n\n")

    w("\nPACKAGES\n" + "-" * 72 + "\n\n")
    for label, spdx, holders, found in rows:
        w(f"{label}\n    License: {spdx}\n")
        for h in holders[:6]:
            w(f"    {h}\n")
        if not found:
            w("    (no licence file in the published crate; SPDX above governs)\n")
        w("\n")

    w("\nLICENSE TEXTS\n" + "-" * 72 + "\n")
    for i, (body, users) in enumerate(
        sorted(texts.items(), key=lambda kv: (-len(kv[1]), sorted(kv[1])[0])), 1
    ):
        u = sorted(users)
        w(f"\n\n[{i}] Applies to {len(u)} package(s): ")
        w(", ".join(u[:40]) + (", ..." if len(u) > 40 else "") + "\n")
        w("-" * 72 + "\n")
        w(body + "\n")


if __name__ == "__main__":
    main()
