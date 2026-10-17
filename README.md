<p align="center"><img src="assets/icon-256.png" width="128" alt="AI8 Converter logo"></p>

# AI8 Converter

A small desktop app (Windows and macOS) and command-line tool that converts
vector files to the **Adobe Illustrator 8** `.ai` format. Many vinyl cutters,
plotters, engravers, RIPs and older design programs read AI8 but not newer
formats.

- **Inputs:** EPS, PS, PDF, AI (both PDF-based Illustrator 9+ files and legacy PostScript-based ones), SVG, SVGZ and DXF (AutoCAD R12–R2018, ASCII or binary)
- **Output:** one Illustrator 8 file per page (`name.ai`, or `name_p1.ai`, `name_p2.ai`, … for multi-page input)
- **What is kept:** paths, curves, compound paths, groups, clipping, fills and strokes (width, caps, joins, dashes), CMYK, RGB and grey colours, and spot colours (for example a `CutContour` cut line), plus overprint
- **Patterns and gradients:** tiling patterns (PDF tiling patterns, SVG `<pattern>`) and linear and radial gradients (PDF axial/radial shadings, SVG `<linearGradient>`/`<radialGradient>`) are carried over; see [Patterns and gradients](#patterns-and-gradients)
- **Text:** converted to outlines, so no fonts are needed on the receiving machine

## Ghostscript

EPS, PS and PostScript-based AI files are interpreted with
[Ghostscript](https://ghostscript.com), which has to be installed separately.
PDF and SVG files don't need it, but when Ghostscript is installed it's also
used to turn text in PDF files into outlines. You can switch that off in the
app, or with `--no-outline-text` on the command line.

| Platform | Install |
| --- | --- |
| macOS | `brew install ghostscript` (or MacPorts `port install ghostscript`) |
| Windows | Download the 64-bit installer from <https://ghostscript.com/releases/gsdnld.html>, or run `choco install ghostscript` |

The app looks for Ghostscript in this order:

1. The `AI8_GHOSTSCRIPT` environment variable (full path to the executable)
2. A copy bundled with the app: `<app folder>/gs/bin/gswin64c.exe` (Windows),
   `<app folder>/gs/bin/gs`, or `AI8 Converter.app/Contents/Resources/gs/bin/gs` (macOS)
3. `PATH` (`gswin64c.exe`, `gswin32c.exe` or `gs.exe` on Windows; `gs` elsewhere)
4. Standard install locations: `C:\Program Files\gs\gs*\bin\gswin64c.exe` (newest
   version wins) on Windows, and `/opt/homebrew/bin/gs`, `/usr/local/bin/gs` or
   `/opt/local/bin/gs` on macOS

You can also point the app at an executable with the **Browse…** button next to
the Ghostscript status line, or pass `--gs PATH` to the command-line tool. Each
Ghostscript run has a time limit of 120 seconds.

## Using the app

1. Drop files onto the window, or click **Add files…**. You can also drop a folder; the app adds the supported files directly inside it.
2. Pick the output location: **Same folder as source** or a folder you choose.
3. Pick the colour handling:
   - **Preserve:** keep colours as they are
   - **CMYK:** convert RGB and grey to CMYK
   - **RGB:** convert CMYK and grey to RGB

   Spot colours are always kept.
4. Pick the pattern handling (see [Patterns and gradients](#patterns-and-gradients)):
   - **Native Illustrator patterns:** pattern fills stay pattern swatches you can edit in Illustrator
   - **Expand to paths (for cutters):** each pattern fill is replaced by plain tile paths clipped to the filled shape
5. Click **Convert**. Each file shows *Pending*, *Converting*, *Done* (with its output paths and any warnings) or *Failed* (with the error). For a finished file, **Open output folder** shows the result.
6. Check the result: **Preview** next to an output file shows the converted `.ai` file, the original, or both side by side (zoomable). **Open as PDF** writes a PDF rendering of the `.ai` file to a temporary folder and opens it in the system PDF viewer. Both need Ghostscript.

### Previewing the output

AI8 files are PostScript programs that rely on Illustrator's own procedure
sets, which (like Illustrator's "include: none" saves) are not stored in the
file. That is why macOS Preview, and Ghostscript on its own, cannot open
them. The app ships its own implementation of the AI8 operators
(`assets/ai8-procset.ps`, written from the Illustrator 7 file format
specification) and runs it with Ghostscript to draw the preview and the PDF.
The PDF is only for looking at: send the `.ai` file itself to the cutter or
to Illustrator.

If an output file would overwrite its input (an `.ai` input written next to
itself), `_ai8` is added to the output name.

## Command line

```
ai8conv [--out DIR] [--color preserve|cmyk|rgb] [--patterns native|expand] [--no-outline-text] [--gs PATH] FILES...
```

```sh
ai8conv logo.eps                         # writes logo.ai next to logo.eps
ai8conv --out converted --color cmyk *.pdf *.svg
ai8conv --patterns expand pattern-fill.svg  # pattern fills as plain paths for a cutter
ai8conv --gs "C:\Program Files\gs\gs10.04.0\bin\gswin64c.exe" sticker.eps
```

The tool prints `input -> output` for each file written, and warnings and errors
on stderr. Exit status: `0` if every file converted, `1` if at least one failed,
`2` for a usage error.

## Building

You need a recent stable Rust toolchain (edition 2024, Rust 1.85 or later) from
<https://rustup.rs>.

```sh
cargo build --release            # GUI: target/release/ai8-converter(.exe)
                                 # CLI: target/release/ai8conv(.exe)
cargo test                       # Ghostscript tests are skipped if gs isn't installed
AI8_VISUAL_SAVE=1 cargo test --test visual   # also keep every rendering in target/visual-diff/
cargo run                        # start the GUI
cargo run --bin ai8conv -- --help
```

**Windows:** use the MSVC toolchain (the Visual Studio Build Tools with the
"Desktop development with C++" workload). Release builds of the GUI use the
Windows subsystem, so no console window appears. Ghostscript always runs
without a console window.

**macOS:** to build a double-clickable app bundle, run:

```sh
scripts/bundle-macos.sh               # target/release/bundle/AI8 Converter.app (+ .zip)
scripts/bundle-macos.sh --universal   # Apple Silicon + Intel (needs both rustup targets)
scripts/bundle-macos.sh --gs-dir DIR  # also bundle a self-contained Ghostscript
```

The bundle has an ad-hoc signature only. On another Mac, the first launch may
need right-click → **Open**, or `xattr -dr com.apple.quarantine "AI8 Converter.app"`.

## Patterns and gradients

Pattern fills and strokes are written in one of two ways, chosen with the
**Patterns** setting in the app or `--patterns` on the command line:

- `native` (default): each pattern becomes an Illustrator pattern swatch and
  the shapes are filled or stroked with it, so the result stays editable in
  Illustrator. A pattern used inside another pattern is expanded into plain
  tile paths inside the outer swatch, and gradients inside a pattern tile
  become flat colours.
- `expand`: each pattern fill is replaced by copies of the tile artwork,
  clipped to the shape. A pattern stroke is expanded the same way inside the
  stroke's outline (dashes, caps and joins included). Use this for cutters,
  plotters and RIPs that don't understand pattern fills.

Linear and radial gradients are written as Illustrator gradients with their
colour stops and geometry (start and end point, centre, radius and focal
point).

SVG details: `patternUnits`, `patternContentUnits`, `patternTransform`,
`viewBox` on patterns, `gradientUnits`, `gradientTransform` and `href`
inheritance are all supported. A `<pattern>` used by several shapes becomes
one swatch, named after its `id`.

**CI:** `.github/workflows/build.yml` builds and tests on `windows-latest` and
`macos-latest` with Ghostscript installed. It uploads the Windows `.exe` files
and the zipped macOS `.app` as artifacts.

## Limitations

- Raster images are not converted. They are dropped and a warning is shown.
- Transparency (opacity, soft masks, blend modes) is not converted, and a warning is shown. This includes gradient `stop-opacity`.
- SVG gradient `spreadMethod="reflect"`/`"repeat"` is written as `pad`, and a radial gradient's focal radius (`fr`) is ignored; both show a warning. Shading types other than linear and radial are approximated by a flat colour.
- Text always becomes outlines; no editable text is written. A PDF with live text that is converted without Ghostscript (or with outlining off) loses that text, and a warning is shown.
- SVG filters, masks and embedded bitmaps are not supported. CSS and `<use>` are supported. SVG `<text>` is outlined with the fonts installed on the computer, so a missing font falls back to a substitute.
- The output is a plain AI8 file: one layer, and no embedded preview or procsets. Illustrator and AI-importing applications read it directly, but sending it straight to a PostScript printer won't work, and macOS Preview can't open it (use **Preview** or **Open as PDF** in the app).
- DXF: text (TEXT, MTEXT, attribute and dimension text, SHX fonts) is not converted; 3D entities are projected flat (top view); hatch patterns become native pattern swatches when their line families repeat on a common lattice (ANSI31, crosshatches, bricks, …), otherwise clipped dashed lines; hatch gradients become linear or radial gradients. Drawings without units are taken as millimetres.
- In expand mode, a pattern stroke whose outline would have more than 5,000 segments (for example a very long dashed line), and patterns nested more than 4 deep, get a flat colour; a warning is shown.
- The packaged builds (`scripts/package-windows.sh`, `scripts/bundle-macos.sh --with-gs`) bundle Ghostscript, which is licensed under the GNU AGPL (source: https://github.com/ArtifexSoftware/ghostpdl). A plain `cargo build` does not bundle it.

## License

Copyright (c) Asbjørn Pettersen. All rights reserved. Bundled Ghostscript is © Artifex Software, licensed under the GNU AGPL v3.
