# Illustrator 8 pattern and gradient syntax: notes and sources

These notes record the syntax `src/ai8.rs` writes for patterns and gradients, and where each part comes from. Each entry is either a short quotation from the published format specification, or an abridged excerpt of a file produced by Illustrator, cited for interoperability.

**On the other implementations cited below.** [SCRIBUS], [XARA] and [VC] are listed because reading them established *facts about the format* — which operands a reader actually parses, in what order, and how it clamps values. No code from any of them was copied, translated or adapted; `src/ai8.rs` and `assets/ai8-procset.ps` are independent implementations written from [SPEC] and from inspecting Illustrator output. This matters because Scribus and Xara Xtreme are GPL-licensed, while this project is MIT.

## Sources

| Ref | Source | What it gives |
|-----|--------|---------------|
| **[SPEC]** | *Adobe Illustrator File Format Specification*, version 7.0, 23 Feb 1998 (published by Adobe as part of its developer documentation; copies circulate as `AI7FileFormat.pdf`). It covers formats up to AI7; AI8 adds nothing documented for patterns or gradients. | BNF of the setup section (§3), patterns (§3.4), gradients (§3.5), and `p`/`P` (§5.4.1) |
| **[AI8]** | A genuine Illustrator 8.0.1 save: `DOEbwlogo.eps` (`%%Creator: Adobe Illustrator(R) 8.0`, `%%AI8_CreatorVersion: 8.0.1`, `%AI5_FileFormat 4.0`). It is embedded in <https://github.com/trilinos/github_move_patches/blob/master/master/0005-Remove-epetraext-inout-docs.patch> as the deleted file `packages/epetraext/doc/inout/DOEbwlogo.eps`. | Exact AI8 setup layout, pattern definition, and gradient definitions |
| **[AI55]** | Illustrator 5.5 and 6.0 saves: `library/images/logo.eps` in Tk (<https://github.com/tcltk/tk/blob/HEAD/library/images/logo.eps>), and `events/altw2005/poster/usyd.eps` in <https://github.com/altaexec/alta-website> | The same structure in older versions, `(name) … [1 0 0 1 0 0] p` in the palette, and `Bb … Bg … BB` |
| **[SCRIBUS]** | Scribus AI importer, <https://github.com/scribusproject/scribus/blob/master/scribus/plugins/import/ai/importai.cpp> | What an independent reader expects (see below) |
| **[XARA]** | Xara Xtreme `Kernel/ai5_eps.cpp`, `ai_grad.cpp` and `ai_epsrr.cpp` (an AI importer **and** exporter), <https://github.com/samuell/xara-xtreme> | Stop operand order, midpoint clamping, and the instance `1 (name) x y angle len 1 0 0 1 0 0 Bg` |
| **[VC]** | vectorcraft AI reader, `crates/eps/src/import/ai/{read,paint}.rs`, <https://github.com/storytold/vectorcraft> | A modern reader's view of `p` (the matrix is the whole transform) and of `Bg`/`Bm`/`Bh` |

What each independent reader expects:

- **[SCRIBUS]** reads only the first five numbers of `p` (px py sx sy angle). It parses `%_Bs` and `%_BS` stops by colour style, and reads `Bg` as `(name) x y angle length m1…m6`.
- **[XARA]**'s `Bg` reader drops the six matrix numbers and uses only the origin, angle and length. Its writer lists linear stops from 100 down to 0 and radial stops from 0 up to 100, the same order Illustrator uses.

## Where the definitions go (setup section)

Below is what [AI8] contains, abridged. Font encodings and the palette are left out.

```
%%BeginSetup
… /initialize get exec
%AI5_Begin_NonPrinting
Np
%AI3_BeginPattern: (Yellow Stripe)
…
%AI3_EndPattern
%AI5_End_NonPrinting--
%AI5_Begin_NonPrinting
Np
3 Bn
%AI5_BeginGradient: (Black & White)
…
%AI5_EndGradient
…
%AI5_End_NonPrinting--
%AI5_BeginPalette            (optional; we do not write one)
…
%%EndSetup
```

[SPEC] §3.5.1 says that `nblends Bn` "is required only if the document contains gradients". The writer follows this exact layout:

- Pattern definitions go in the first non-printing block.
- `n Bn` and the gradient definitions go in a second block.
- When the document has no gradients, the second block is left out.

## Patterns

### Definition ([SPEC] §3.4, and [AI8] verbatim)

```
%AI3_BeginPattern: (Yellow Stripe)
(Yellow Stripe) 8.4499 4.6 80.4499 76.6 [
%AI3_Tile
(0 O 0 R 0 0.4 1 0 k
 0 0.4 1 0 K
) @
(
%AI6_BeginPatternLayer
800 Ar
0 J 0 j 3.6 w 4 M []0 d
%AI3_Note:
0 D
0 XR
8.1999 8.1999 m
80.6999 8.1999 L
S
…
%AI6_EndPatternLayer
) &
] E
%AI3_EndPattern
```

- **Syntax:** `(name) llx lly urx ury [ layer* ] E`. Each layer is written as `(colour definition) @ (tile definition) &`.
- **Strings:** both parts are PostScript **strings**. [SPEC] says "The use of strings limits the size of a pattern layer definition to 64K bytes."
- **Inner strings:** in [AI8], the art inside the strings is ordinary art script, and strings nested inside it, such as `(PANTONE 468 CV)` in the [SPEC] figure, stay unescaped as balanced parentheses.
- **Art outside the tile box:** in [AI8], the stroked lines run from x = 8.1999 to 80.6999, which is outside the tile box (8.4499 to 80.4499). Illustrator tiles the box edge to edge and clips the art to it. This also explains the well-known "copy the edge art to the opposite side" technique for seamless tiles.

### What we write

```
%AI3_BeginPattern: (name)
(name) 0 0 xs ys [
%AI3_Tile
(0 O 0 R 0 g
0 G
) @
(
%AI6_BeginPatternLayer
800 Ar
…tile art (paths, groups, colour operators)…
%AI6_EndPatternLayer
) &
] E
%AI3_EndPattern
```

**Tile space is normalised.** The tile's lower-left corner is moved to (0,0), and the box is `0 0 xs ys`, with xs and ys being the IR `x_step` and `y_step`. The `p` matrix takes the offset `(llx, lly)` instead. This is safe under both readings of "tile origin":

- the lattice is anchored at the ruler origin and the art keeps its absolute coordinates;
- the lattice is anchored at the tile's lower-left corner ([VC]: "The tile's lower left is the pattern's origin wherever the definition drew it").

**Step and box size differ.** When `x_step` or `y_step` differ from the box size (PDF patterns with gaps or overlaps), the tile is the step-sized cell. Copies of the art from the neighbouring lattice positions that reach into the cell are added. If the art also extends past its own PDF box, each copy is wrapped in a `q … Q` clip group to that box (see "Uncertain" below). When the step equals the box size, the art is written as is, because Illustrator clips at the box.

**Nested paints.** Patterns nested inside tile art are expanded inside the tile (`patterns::expand_nested`): the inner pattern-filled path becomes a `q … Q` clip group holding copies of the inner tile, exactly as in expand mode, so the outer pattern stays a swatch and looks right. Gradients nested inside tile art are flattened to solid colours (`Color::flatten`), with no warning.

**Colour mode.** The writer applies `ColorMode` (Cmyk or Rgb) inside tiles as everywhere else.

**String escaping.** Any string inside tile art, such as a spot colour name, has `(`, `)` and `\` written as octal escapes (`\050`, `\051`, `\134`). The writer then doubles every backslash when it embeds the art in the outer `( … ) &` string. Decoding the outer string once gives valid inner art, and the outer string never contains unbalanced parentheses.

**Large tiles.** Art over about 60,000 bytes is split into several `@`/`&` layers. Each layer starts with a fresh paint state.

**Duplicates and names.**

- One definition is written per distinct pattern: the same `Arc`, or a different `Arc` with equal content.
- Names are made unique by appending " 2", " 3", and so on.
- An empty name becomes `Pattern`.

### Use: `p` / `P` ([SPEC] §5.4.1, and [AI55] verbatim)

```
(patternname) px py sx sy angle rf r k ka [a b c d tx ty] p      % fill
(patternname) px py sx sy angle rf r k ka [a b c d tx ty] P      % stroke
```

The swatch palette in [AI8] and [AI55] contains `(Yellow Stripe) 0 0 1 1 0 0 0 0 0 [1 0 0 1 0 0] p`.

[SPEC] describes the matrix as "the initial matrix to which all other pattern transformations are to be applied". We always write `0 0 1 1 0 0 0 0 0` for the named transforms and put the whole transform in the matrix. The matrix maps normalised tile space to file space:

```
[M] = translate(llx, lly) · PatternPaint.matrix · translate(dx, dy)
```

where `(dx, dy)` is the writer's page offset (`-page.llx`, `-page.lly`). With these values, a reader that composes "named transforms ∘ matrix" ([SPEC]) and a reader that treats the matrix as the whole transform ([VC]) get the same result.

## Gradients

### Definition ([SPEC] §3.5.2, and [AI8] verbatim)

```
%AI5_BeginGradient: (Red & Yellow)
(Red & Yellow) 0 2 Bd
[
0
<
F0F1…FF
>
<
9F9F…99
>
0
1 %_Br
[
0 1 0.6 0 1 50 100 %_BS
%_0 1 0.6 0 1 50 100 Bs
0 0 1 0 1 50 0 %_BS
%_0 0 1 0 1 50 0 Bs
BD
%AI5_EndGradient
```

**Bd.** The syntax is `(name) type nColors Bd`, where type 0 is linear and 1 is radial.

**Stops.** Each stop has the form `colorSpec colorStyle midPoint rampPoint`. Illustrator 8 writes each stop twice: once as `… %_BS`, and once as a `%_… Bs` line. [SPEC] and AI5 to AI7 files use the single form `… %_Bs`. We write the AI8 pair exactly as [AI8] does.

**colorStyle values** ([SPEC] tables 7 and 8):

| colorStyle | Meaning | colorSpec |
|------------|---------|-----------|
| 0 | Gray | `gray` |
| 1 | CMYK | `c m y k` |
| 2 | RGB | `c m y k r g b` (the CMYK equivalent comes first; [XARA] writes the same) |
| 3 | CMYK custom (spot) | `c m y k (name) tint`, tint inverted as for `x` (0 = 100%). The [SPEC] example is `0.05 0.2 0.95 0 (Gold) 0 3 50 0 %_Bs`. |
| 4 | RGB custom | not used |

**midPoint.** It lies between 13 and 87. We always write 50 ([XARA] clamps to 13..87).

**rampPoint.** It is a percentage from 0 to 100. We write it with up to 2 decimals.

**Stop order.** Illustrator lists linear ramps last stop first (100 → 0) and radial ramps first stop first. This holds in [AI8], [AI55] ("Black & White" 100/0, "Red & Yellow" 100/0, "Yellow & Blue Radial" 14/100), and in [XARA]'s exporter. We do the same. [VC] notes that readers must accept either order.

**`%_Br` ramp data.** [SPEC] says this is only needed "if the file is being written for imaging … If the file is being written for use within Adobe Illustrator or as common file format, this information can be ignored". We omit it, so the stops follow a single `[`, as in the [SPEC] example "White & Purple Radial".

**Duplicates and names.** Gradients that end up with identical stop lines after colour-mode conversion share one definition. Names are `Linear Gradient N` or `Radial Gradient N`, made unique against pattern names as well.

**Spot colours.** Spot colours used in stops and in tiles are added to `%%DocumentCustomColors` and `%%CMYKCustomColor`.

### Instance ([SPEC] §3.5.3–3.5.5)

```
… path construction (m/L/C) …
Bb
[xHilight yHilight angle length Bh]          % radial only; "0 0 0 0 Bh" when centred
1 (name) xOrigin yOrigin angle length a b c d tx ty Bg
[a b c d tx ty Bm]                            % only for non-uniformly scaled radials (see below)
f | F
0 BB | 1 BB | 2 BB                            % 0 no stroke, 1 stroke, 2 close + stroke
```

[SPEC] figure 6 shows this form:

```
Bb
1 (Purple, Red & Yellow) 36 696 -30 243 1 0 0 1 0 0 Bg
f
0 BB
```

[SPEC] gives this radial example:

```
Bb
60 58 44 0.5901 Bh
1 (Yellow & Blue Radial) 208 474 45 141 1 0 0 1 0 0 Bg
f
0 BB
```

The operands work as follows:

- **Bg flag.** It is 1 ("issue a clip; for simple filled paths, flag takes the value 1").
- **Fill and stroke inside the instance.** The fill operator sits between `Bb` and `BB`. A stroke on the same path is drawn with the `BB` flag: `f` + `2 BB` for closed paths, `F` + `1 BB` for open paths.
- **Compound paths.** Each subpath inside `*u … *U` gets its own `Bb … BB` instance. [SPEC]'s BNF makes every compound element a full path object.
- **Gradient strokes.** AI8 cannot stroke with a gradient. Such strokes are flattened to the middle colour (`Color::flatten`).

**Linear gradients** are encoded exactly with an **identity** `Bg` matrix. Any affine image of a linear gradient is again a linear gradient. With t(q) = (q − start)·g, where g = (end − start)/|end − start|², the page-space gradient of t is u = M⁻ᵀg. The writer therefore uses:

- origin = M(start) + page offset;
- angle = atan2(u);
- length = 1/|u|.

This handles rotation, non-uniform scale and skew, and is checked numerically in `tests/ai8_patterns.rs`.

**Radial gradients under a similarity transform** (uniform scale, rotation, reflection) also use an identity matrix:

- origin = M(centre), length = r·√|det M|;
- `Bh` holds the focal point's offset, angle, and distance as a fraction of the radius, clamped to the circle (≤ 1).

**Radial gradients under a non-uniform or skewed transform** need the `Bg` matrix. [SPEC] §3.5.4 gives its semantics about the canvas corner (xo, yo) = (W/2 − 4320, H/2 + 4320), with b and c negated:

```
x_new =  a·(x − xo) − c·(y − yo) + tx + xo
y_new = −b·(x − xo) + d·(y − yo) + ty + yo
```

The 8640 pt canvas centred on the artboard matches the palette instances in [AI8], whose origins are `-4014 4716` on a 612×792 artboard (612/2 − 4320 = −4014 and 792/2 + 4320 = 4716). We solve for (a b c d tx ty) so that the circle around the page-space centre maps to the correct ellipse. We also write the imaging matrix `r·La r·Lb −r·Lc −r·Ld cx cy Bm`, which maps the unit circle onto the ellipse. This matches the [SPEC] example `172.27 0 0 -172.27 284 263 Bm`, and [VC] uses it as the radial geometry.

**Degenerate cases.** These are written as the flat colour:

- fewer than two stops;
- start equal to end for a linear gradient;
- a zero radius;
- a singular matrix.

## Expand mode (`src/patterns.rs`)

`expand_patterns(doc)` replaces each pattern-filled path with:

- a clip group whose clip is the path's subpaths and fill rule;
- inside it, copies of the tile art, transformed by `translate(i·xs, j·ys) · matrix`, for every lattice cell whose transformed visible tile (a parallelogram, tested by separating axes) overlaps the path's bounds;
- the path's stroke as a separate path after the clip group.

Some details:

- **Tile clipping.** If the tile art extends past the tile's box, each copy is clipped to it.
- **Pattern strokes** are expanded inside the stroke's outline: `stroke_outline` builds it with tiny-skia's stroker (dashes, caps, joins and miter limit applied), and the outline is then filled like a pattern-filled path (non-zero rule), after the path's own fill. Outlines with more than 5,000 segments fall back to the flat colour, with a warning.
- **Visible tile art** is measured with `painted_bounds`, which includes stroke widths, so a tile whose art is a single stroked straight line (a DXF ANSI31 hatch) is not mistaken for empty.
- **Nested patterns** are expanded recursively, up to 4 levels deep.
- **Caps.** The limit is 20,000 tiles per path and 400,000 per document. Beyond that, the fill falls back to the flat colour and a warning is added to `doc.warnings`.
- **Gradients** stay native. `flatten_gradients(doc)` is available separately.

## Verified vs. uncertain

**Verified** against the spec and/or a real Illustrator 8 file:

- the setup layout (non-printing blocks, `Bn`);
- the pattern definition syntax (`[`, `%AI3_Tile`, `@`, `&`, `%AI6_BeginPatternLayer`, `800 Ar`, `] E`);
- the `p`/`P` operand list;
- `Bd`, the `%_BS` + `%_ … Bs` pair, colour styles 0–3, and stop order;
- the `Bb`/`Bh`/`Bg`/`f`/`BB` sequence;
- `Bh` operand meaning.

**Uncertain** (no Illustrator available to test, and no real file found showing it):

1. **Colour operators inside a pattern layer's art.** We write them so that tiles can have several colours. [SPEC] says the art in `&` "is identical to the representation of objects in the document script". The [AI8] example has only one colour, which the `@` string sets.
2. **`q … Q` clip groups inside tiles.** These are written when the step differs from the box size **and** the art overflows its box, and when a pattern nested in the tile is expanded. Older Illustrator versions refused masks in pattern swatches in the UI.
3. **The `Bg` matrix convention.** It matters only for non-uniformly scaled or skewed radial gradients, because every other case uses the identity matrix. We follow the [SPEC] formula literally. [VC] instead applies the `Bg` matrix as a plain art-space matrix, but it prefers `Bm` when present, which we also write.
4. **Fractional rampPoints** (for example `37.5`). [SPEC] examples and [AI8] only show integers. [XARA]'s reader pops integers.
5. **One `Bb…BB` per subpath of a compound path.** This is inferred from the BNF.
6. **Omitting `%_Br`.** [SPEC] allows it, but every real file we found includes it.

## The preview procset (`assets/ai8-procset.ps`)

The app previews its output, and the visual tests (`tests/visual.rs`) check it, by running the AI8 file through Ghostscript with our own prolog. The prolog implements [SPEC], not our writer's habits. These are the readings it takes:

- **Paint state** (`g k x Xa p`, `XR`, overprint) is document state that lasts until it changes (§5: "Fill and stroke attributes are state-based"). `q`/`Q` save and restore only the clip; the line style lives in the graphics state and is carried across `Q`.
- **Compound paths** are one PostScript path (§5.3.5). The painting operators inside `*u … *U` only record how the path is painted, the last one wins, and the whole path is painted once at `*U`. Masks made of compound paths become the union of their subpaths, clipped with the fill rule.
- **Masks.** `h`/`H` + `W` intersect the clip with the path when the path is ended (§5.6.2); `Q` restores it.
- **Gradient stops** are the operands that the `… %_BS` (AI8) or `… %_Bs` (AI5–7) lines leave on the stack. To PostScript the rest of those lines, and the whole `%_… Bs` lines, are comments. `BD` collects everything down to the `[` mark, parses the stops from the end (the colorStyle fixes the number of colorSpec operands), and drops any `%_Br` ramp data below. Stops are put in ramp order. A list written last stop first (linear ramps) is reversed before a stable sort, so coincident stops keep their order. A midPoint other than 50 adds a node with the average colour. Custom colours are imaged as their CMYK values times the tint, and a ramp is imaged in DeviceCMYK if any stop is CMYK or custom, DeviceRGB if any is RGB, else DeviceGray.
- **Gradient instances.** Between `Bb` and `BB`, `f`/`F` fills with the gradient and keeps the path; `BB` strokes it (flag 1, or 2 after closing) and ends it. Linear ramps run from the origin at `angle` over `length`, and radial ramps from the hilight (angle and fraction of the radius, as Illustrator does; the x/y offsets are ignored) to the circle; both pad. The `Bg` matrix is applied with the §3.5.4 formula about the canvas corner (W/2 − 4320, H/2 + 4320), with W×H from `%AI5_ArtSize`. `Bm`, `Bc` and `Xm` are imaging hints and are ignored. A `Bg` flag of 2 disables the fill.
- **Patterns.** `E` stores the layer strings. `p`/`P` build a Level 2 coloured tiling pattern: BBox = the tile box and steps = its size. The pattern matrix is `translate(−llx, −lly)` (tile corner to the origin), then the `[a b c d tx ty]` matrix ("the initial matrix to which all other pattern transformations are applied"), then the named transforms (scale, rotation, reflection, shear, offset). The PaintProc runs each layer's colour string and art string with a fresh paint state.
- Unknown operators are skipped in the app's preview; the tests run in strict mode, where they are errors and leftover operands fail the test.

Visual test results are compared against Ghostscript's rendering of the original (EPS, PDF) or resvg's (SVG). Both are rendered at 3× and box-filtered down, because Ghostscript does not anti-alias clip paths or pattern fills. SVG fixtures are rendered at 96 dpi with tile sizes that are whole pixels, because resvg rasterises pattern tiles at a rounded pixel size.
