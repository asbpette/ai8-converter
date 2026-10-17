//! Writer for the Adobe Illustrator 8 native file format (PostScript based,
//! "AI5_FileFormat 4.0"), as described in the Adobe Illustrator File Format
//! Specification (version 7.0, 1998) and as emitted by Illustrator 8.0 itself.
//! See `docs/ai8-format-notes.md` for the sources of the pattern and gradient
//! syntax.
//!
//! The output contains no procset bodies (like Illustrator's own
//! "include: none" saves). Illustrator and AI-importing applications (cutters,
//! RIPs, CorelDRAW, etc.) parse the operators directly.
//!
//! Patterns and gradients are written natively:
//! * every distinct [`Pattern`] becomes one `%AI3_BeginPattern` definition in
//!   the setup section (tile art normalised so the tile starts at 0,0;
//!   patterns nested inside tile art are expanded into clipped tile copies,
//!   gradients inside tile art are flattened to solid colours), used
//!   with `(name) 0 0 1 1 0 0 0 0 0 [matrix] p` / `P`;
//! * every distinct gradient becomes one `%AI5_BeginGradient` definition
//!   (`Bd`, `%_BS`/`Bs` stops, `BD`) after an `n Bn` count, and gradient fills
//!   are written as `Bb` … `Bg` … `f` … `BB` instances. Gradient strokes do not
//!   exist in AI8 and are flattened.

use crate::ir::{
    Color, Document, Element, GradientKind, GradientPaint, LineCap, LineJoin, Matrix, PathItem, Pattern, PatternPaint, Point,
    Rect, Segment, Stroke, Subpath,
};
use crate::patterns;
use std::fmt::Write as _;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct WriteOptions {
    pub title: String,
    pub creator_for: String,
    pub color_mode: ColorMode,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self { title: "Untitled.ai".into(), creator_for: String::new(), color_mode: ColorMode::Preserve }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// Keep RGB as RGB, CMYK as CMYK, spot as spot.
    #[default]
    Preserve,
    /// Convert all process colours to CMYK (spot colours are kept).
    Cmyk,
    /// Convert all process colours to RGB (spot colours are kept).
    Rgb,
}

/// A closed path whose end point is this close to its start point (in
/// points, about 0.035 mm) is closed by moving the end point onto the start.
const CLOSE_SNAP: f64 = 0.1;

fn near(a: Point, b: Point, tol: f64) -> bool {
    (a.x - b.x).abs() <= tol && (a.y - b.y).abs() <= tol
}

fn end_of(s: &Segment) -> Point {
    match s {
        Segment::Line(p) | Segment::Curve(_, _, p) => *p,
    }
}

/// Format a number compactly: at most 4 decimals, no trailing zeros, no "-0".
pub fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let mut s = format!("{:.4}", v);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".into() } else { s }
}

/// Encode a PostScript string literal including the parentheses.
pub fn ps_string(s: &str) -> String {
    ps_string_impl(s, false)
}

/// Like [`ps_string`], but parentheses and backslashes are written as octal
/// escapes so the literal contains no `(`, `)` of its own. Used inside pattern
/// tile definitions, which are themselves PostScript strings.
fn ps_string_nested(s: &str) -> String {
    ps_string_impl(s, true)
}

fn ps_string_impl(s: &str, nested: bool) -> String {
    let mut out = String::from("(");
    for ch in s.chars() {
        match ch {
            '(' | ')' | '\\' if nested => {
                let _ = write!(out, "\\{:03o}", ch as u32);
            }
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let code = c as u32;
                let byte = if code <= 0xFF { code } else { b'?' as u32 };
                let _ = write!(out, "\\{:03o}", byte);
            }
        }
    }
    out.push(')');
    out
}

fn clamp01(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

pub fn rgb_to_cmyk(r: f64, g: f64, b: f64) -> [f64; 4] {
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 - 1e-9 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let d = 1.0 - k;
    [(1.0 - r - k) / d, (1.0 - g - k) / d, (1.0 - b - k) / d, k]
}

pub fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> [f64; 3] {
    [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)]
}

/// Half the side of Illustrator's 8640 pt (120 in) canvas. The canvas is
/// centred on the artboard; gradient instance matrices are expressed about its
/// top-left corner (AI7 spec §3.5.4).
const CANVAS_HALF: f64 = 4320.0;

/// Pattern tile layers are PostScript strings, which Illustrator limits to
/// 64 KB; tiles with more art are split into several layers.
const MAX_TILE_LAYER_BYTES: usize = 60_000;

/// Tracks the last emitted paint state so redundant operators are skipped.
#[derive(Default, Clone)]
struct State {
    fill: Option<String>,
    stroke: Option<String>,
    style: Option<String>,
    fill_rule: Option<bool>,
    fill_op: Option<bool>,
    stroke_op: Option<bool>,
}

struct PatternEntry {
    pattern: Arc<Pattern>,
    name: String,
}

struct GradientEntry {
    name: String,
    kind: GradientKind,
    /// Stop lines (`… %_BS` / `%_… Bs`), already in the output colour model.
    stops: Vec<String>,
}

/// Global objects referenced by the art, registered while it is written.
#[derive(Default)]
struct Resources {
    patterns: Vec<PatternEntry>,
    gradients: Vec<GradientEntry>,
    names: Vec<String>,
    /// Spot colours (name -> CMYK alternate), in first-use order.
    spots: Vec<(String, [f64; 4])>,
}

impl Resources {
    fn unique_name(&mut self, base: &str) -> String {
        let base: String = base.trim().chars().take(60).collect();
        let base = if base.is_empty() { "Pattern".to_string() } else { base };
        let mut name = base.clone();
        let mut n = 2;
        while self.names.contains(&name) {
            name = format!("{base} {n}");
            n += 1;
        }
        self.names.push(name.clone());
        name
    }

    fn add_spot(&mut self, name: &str, cmyk: [f64; 4]) {
        if !self.spots.iter().any(|(n, _)| n == name) {
            self.spots.push((name.to_string(), cmyk));
        }
    }
}

/// Geometry of one gradient instance (`Bh`, `Bg`, `Bm` operands) in file space.
struct GradientInstance {
    name: String,
    radial: bool,
    origin: Point,
    angle: f64,
    length: f64,
    /// `Bh`: x/y offset of the highlight from the origin, its angle and its
    /// distance as a fraction of the radius.
    hilight: [f64; 4],
    /// `Bg` matrix (identity unless a radial gradient is non-uniformly scaled).
    matrix: [f64; 6],
    /// Optional `Bm` (radial gradients with a non-uniform matrix only).
    bm: Option<[f64; 6]>,
}

struct Writer {
    out: String,
    opts: WriteOptions,
    /// Translation applied to every coordinate so the page starts at 0,0.
    dx: f64,
    dy: f64,
    page_w: f64,
    page_h: f64,
    state: State,
    /// Writing pattern tile art: nested gradients are flattened and
    /// strings must not contain bare parentheses.
    in_tile: bool,
    res: Resources,
}

/// Serialize a document as an Illustrator 8 file.
pub fn write_ai8(doc: &Document, opts: &WriteOptions) -> String {
    let mut w = Writer {
        out: String::new(),
        opts: opts.clone(),
        dx: -doc.page.llx,
        dy: -doc.page.lly,
        page_w: doc.page.width().max(1.0),
        page_h: doc.page.height().max(1.0),
        state: State::default(),
        in_tile: false,
        res: Resources::default(),
    };
    // Art first: it registers the patterns, gradients and spot colours the
    // header and setup section have to declare.
    w.out.push_str("%AI5_BeginLayer\n1 1 1 1 0 0 1 0 79 128 255 0 50 Lb\n(Layer 1) Ln\n0 A\n");
    for e in &doc.elements {
        w.element(e);
    }
    w.out.push_str("LB\n%AI5_EndLayer--\n");
    let body = std::mem::take(&mut w.out);
    let pattern_defs = w.pattern_defs();
    let gradient_defs = w.gradient_defs();
    w.header(doc, &pattern_defs, &gradient_defs);
    w.out.push_str(&body);
    w.trailer();
    w.out
}

impl Writer {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn pstr(&self, s: &str) -> String {
        if self.in_tile { ps_string_nested(s) } else { ps_string(s) }
    }

    fn header(&mut self, doc: &Document, pattern_defs: &str, gradient_defs: &str) {
        let w = self.page_w;
        let h = self.page_h;
        let bounds = doc
            .content_bounds()
            .map(|r| Rect { llx: r.llx + self.dx, lly: r.lly + self.dy, urx: r.urx + self.dx, ury: r.ury + self.dy })
            .unwrap_or(Rect { llx: 0.0, lly: 0.0, urx: w, ury: h });
        let spots = self.res.spots.clone();

        let date = chrono_like_now();
        self.line("%!PS-Adobe-3.0 ");
        self.line("%%Creator: Adobe Illustrator(R) 8.0");
        self.line("%%AI8_CreatorVersion: 8.0");
        let for_ = ps_string(&self.opts.creator_for);
        self.line(&format!("%%For: {} ()", for_));
        let title = ps_string(&self.opts.title);
        self.line(&format!("%%Title: {}", title));
        self.line(&format!("%%CreationDate: ({}) ()", date));
        self.line(&format!(
            "%%BoundingBox: {} {} {} {}",
            bounds.llx.floor() as i64,
            bounds.lly.floor() as i64,
            bounds.urx.ceil() as i64,
            bounds.ury.ceil() as i64
        ));
        self.line(&format!(
            "%%HiResBoundingBox: {} {} {} {}",
            num(bounds.llx),
            num(bounds.lly),
            num(bounds.urx),
            num(bounds.ury)
        ));
        self.line("%%DocumentProcessColors: Cyan Magenta Yellow Black");
        if !spots.is_empty() {
            let names: Vec<String> = spots.iter().map(|(n, _)| ps_string(n)).collect();
            self.line(&format!("%%DocumentCustomColors: {}", names[0]));
            for n in &names[1..] {
                self.line(&format!("%%+ {}", n));
            }
            for (i, (name, c)) in spots.iter().enumerate() {
                let prefix = if i == 0 { "%%CMYKCustomColor:" } else { "%%+" };
                self.line(&format!(
                    "{} {} {} {} {} {}",
                    prefix,
                    num(c[0]),
                    num(c[1]),
                    num(c[2]),
                    num(c[3]),
                    ps_string(name)
                ));
            }
        }
        self.line("%%DocumentNeededResources: procset Adobe_level2_AI5 1.2 0");
        self.line("%%+ procset Adobe_ColorImage_AI6 1.3 0");
        self.line("%%+ procset Adobe_Illustrator_AI5 1.3 0");
        self.line("%AI5_FileFormat 4.0");
        self.line("%AI3_ColorUsage: Color");
        self.line("%AI7_ImageSettings: 0");
        self.line("%%CMYKProcessColor: 1 1 1 1 ([Registration])");
        self.line(&format!("%AI3_TemplateBox: {} {} {} {}", num(w / 2.0), num(h / 2.0), num(w / 2.0), num(h / 2.0)));
        self.line(&format!("%AI3_TileBox: 0 0 {} {}", num(w), num(h)));
        self.line("%AI3_DocumentPreview: None");
        self.line(&format!("%AI5_ArtSize: {} {}", num(w), num(h)));
        self.line("%AI5_RulerUnits: 2");
        self.line("%AI5_ArtFlags: 0 0 0 1 0 0 1 0 0");
        self.line("%AI5_TargetResolution: 800");
        self.line("%AI5_NumLayers: 1");
        self.line(&format!("%AI8_OpenToView: {} {} 1 1000 750 26 0 1 1 1 1 1", num(-w * 0.1), num(h * 1.1)));
        self.line("%AI5_OpenViewLayers: 7");
        self.line("%%PageOrigin:0 0");
        self.line("%AI7_GridSettings: 72 8 72 8 1 0 0.8 0.8 0.8 0.9 0.9 0.9");
        self.line("%%EndComments");
        self.line("%%BeginProlog");
        self.line("%%IncludeResource: procset Adobe_level2_AI5 1.2 0");
        self.line("%%IncludeResource: procset Adobe_ColorImage_AI6 1.3 0");
        self.line("%%IncludeResource: procset Adobe_Illustrator_AI5 1.3 0");
        self.line("%%EndProlog");
        self.line("%%BeginSetup");
        self.line("Adobe_level2_AI5 /initialize get exec");
        self.line("Adobe_ColorImage_AI6 /initialize get exec");
        self.line("Adobe_Illustrator_AI5 /initialize get exec");
        // Same layout as Illustrator 8: patterns in the first non-printing
        // block, then the gradient count and definitions in a second one.
        self.line("%AI5_Begin_NonPrinting");
        self.line("Np");
        self.out.push_str(pattern_defs);
        self.line("%AI5_End_NonPrinting--");
        if !gradient_defs.is_empty() {
            self.line("%AI5_Begin_NonPrinting");
            self.line("Np");
            self.line(&format!("{} Bn", self.res.gradients.len()));
            self.out.push_str(gradient_defs);
            self.line("%AI5_End_NonPrinting--");
        }
        self.line("%%EndSetup");
    }

    fn trailer(&mut self) {
        self.line("%%PageTrailer");
        self.line("gsave annotatepage grestore showpage");
        self.line("%%Trailer");
        self.line("Adobe_Illustrator_AI5 /terminate get exec");
        self.line("Adobe_ColorImage_AI6 /terminate get exec");
        self.line("Adobe_level2_AI5 /terminate get exec");
        self.line("%%EOF");
    }

    /// Convert a colour according to the colour mode.
    fn map_color(&self, c: &Color) -> Color {
        match (self.opts.color_mode, c) {
            (_, Color::Spot { .. }) | (ColorMode::Preserve, _) => c.clone(),
            (ColorMode::Cmyk, Color::Rgb(r, g, b)) => {
                let [c, m, y, k] = rgb_to_cmyk(*r, *g, *b);
                Color::Cmyk(c, m, y, k)
            }
            (ColorMode::Cmyk, Color::Gray(g)) => Color::Cmyk(0.0, 0.0, 0.0, 1.0 - g),
            (ColorMode::Rgb, Color::Cmyk(c, m, y, k)) => {
                let [r, g, b] = cmyk_to_rgb(*c, *m, *y, *k);
                Color::Rgb(r, g, b)
            }
            (ColorMode::Rgb, Color::Gray(g)) => Color::Rgb(*g, *g, *g),
            _ => c.clone(),
        }
    }

    /// AI colour operator text. `stroke` selects the upper-case variant.
    /// Patterns become `p`/`P` (flattened inside tiles); gradients are
    /// flattened here (fills are handled by [`Writer::gradient_instance`]).
    fn color_op(&mut self, c: &Color, stroke: bool) -> String {
        if let Color::Pattern(pp) = c
            && !self.in_tile
            && let Some(op) = self.pattern_op(pp, stroke)
        {
            return op;
        }
        let c = self.map_color(&c.flatten());
        let n = |v: f64| num(clamp01(v));
        match c {
            Color::Gray(g) => format!("{} {}", n(g), if stroke { "G" } else { "g" }),
            Color::Cmyk(c, m, y, k) => {
                format!("{} {} {} {} {}", n(c), n(m), n(y), n(k), if stroke { "K" } else { "k" })
            }
            // AI 8 RGB process colour: "r g b Xa" (fill) / "XA" (stroke).
            // Illustrator also expects a CMYK equivalent before it for older readers.
            Color::Rgb(r, g, b) => {
                let [cc, mm, yy, kk] = rgb_to_cmyk(clamp01(r), clamp01(g), clamp01(b));
                format!(
                    "{} {} {} {} {}\n{} {} {} {}",
                    n(cc),
                    n(mm),
                    n(yy),
                    n(kk),
                    if stroke { "K" } else { "k" },
                    n(r),
                    n(g),
                    n(b),
                    if stroke { "XA" } else { "Xa" }
                )
            }
            // Custom colour: "c m y k (name) tint x". In the AI format the tint
            // operand is inverted: 0 = 100% of the colour, 1 = 0%.
            Color::Pattern(_) | Color::Gradient(_) => unreachable!("flattened above"),
            Color::Spot { name, cmyk, tint } => {
                self.res.add_spot(&name, cmyk);
                format!(
                    "{} {} {} {} {} {} {}",
                    n(cmyk[0]),
                    n(cmyk[1]),
                    n(cmyk[2]),
                    n(cmyk[3]),
                    self.pstr(&name),
                    n(1.0 - clamp01(tint)),
                    if stroke { "X" } else { "x" }
                )
            }
        }
    }

    // ----- patterns ------------------------------------------------------

    /// Name of the swatch for `p`, registering a definition on first use.
    /// Identical definitions (same Arc or equal contents) share one swatch.
    fn pattern_name(&mut self, p: &Arc<Pattern>) -> String {
        if let Some(e) = self.res.patterns.iter().find(|e| Arc::ptr_eq(&e.pattern, p) || *e.pattern == **p) {
            return e.name.clone();
        }
        let name = self.res.unique_name(&p.name);
        self.res.patterns.push(PatternEntry { pattern: p.clone(), name: name.clone() });
        name
    }

    /// `(name) px py sx sy angle rf r k ka [a b c d tx ty] p` (AI7 spec §5.4.1).
    /// The whole transform is carried by the matrix (offset 0 0, scale 1 1,
    /// no rotation/reflection/shear), which maps the normalised tile space
    /// (tile lower-left at 0,0) to file space.
    fn pattern_op(&mut self, pp: &PatternPaint, stroke: bool) -> Option<String> {
        patterns::tile_steps(&pp.pattern)?;
        let m = Matrix::translate(pp.pattern.bbox.llx, pp.pattern.bbox.lly)
            .then(&pp.matrix)
            .then(&Matrix::translate(self.dx, self.dy));
        if !patterns::is_finite_matrix(&m) || patterns::invert(&m).is_none() {
            return None;
        }
        let name = self.pattern_name(&pp.pattern);
        Some(format!(
            "{} 0 0 1 1 0 0 0 0 0 [{} {} {} {} {} {}] {}",
            ps_string(&name),
            num(m.a),
            num(m.b),
            num(m.c),
            num(m.d),
            num(m.e),
            num(m.f),
            if stroke { "P" } else { "p" }
        ))
    }

    /// All registered pattern definitions (`%AI3_BeginPattern` … `%AI3_EndPattern`).
    fn pattern_defs(&mut self) -> String {
        let mut out = String::new();
        let mut i = 0;
        while i < self.res.patterns.len() {
            let (pat, name) = (self.res.patterns[i].pattern.clone(), self.res.patterns[i].name.clone());
            out.push_str(&self.pattern_def(&pat, &name));
            i += 1;
        }
        out
    }

    fn pattern_def(&mut self, pat: &Pattern, name: &str) -> String {
        let (xs, ys) = patterns::tile_steps(pat).unwrap_or((1.0, 1.0));
        // Patterns nested in the tile become plain (clipped) tile copies;
        // gradients in tiles are flattened when the art is written.
        let (art, _) = patterns::expand_nested(patterns::tile_cell_art(pat, xs, ys));

        // Write the tile art in tile space (lower-left of the tile at 0,0).
        let saved = (std::mem::take(&mut self.out), self.dx, self.dy, std::mem::take(&mut self.state), self.in_tile);
        self.dx = -pat.bbox.llx;
        self.dy = -pat.bbox.lly;
        self.in_tile = true;
        let mut layers: Vec<String> = Vec::new();
        let mut current = String::new();
        // Work list (reversed): an element whose art alone exceeds a layer is
        // split, a (clip) group into several with the same clip and halves of
        // the children, which paints the same.
        let mut work: Vec<Element> = art.into_iter().rev().collect();
        while let Some(e) = work.pop() {
            let e = &e;
            self.out.clear();
            let saved_state = self.state.clone();
            self.element(e);
            if self.out.len() > MAX_TILE_LAYER_BYTES
                && let Some((a, b)) = split_element(e)
            {
                self.state = saved_state;
                work.push(b);
                work.push(a);
                continue;
            }
            if !current.is_empty() && current.len() + self.out.len() > MAX_TILE_LAYER_BYTES {
                layers.push(std::mem::take(&mut current));
                // A new layer starts with a fresh paint state.
                self.state = State::default();
                self.out.clear();
                self.element(e);
            }
            current.push_str(&self.out);
        }
        layers.push(current);
        let at_color = format!("0 O 0 R {}\n{}", self.color_op(&Color::Gray(0.0), false), self.color_op(&Color::Gray(0.0), true));
        (self.out, self.dx, self.dy, self.state, self.in_tile) = saved;

        let mut s = String::new();
        let _ = writeln!(s, "%AI3_BeginPattern: {}", ps_string(name));
        let _ = writeln!(s, "{} 0 0 {} {} [", ps_string(name), num(xs), num(ys));
        s.push_str("%AI3_Tile\n");
        for art in layers {
            let _ = writeln!(s, "({}\n) @", at_color);
            s.push_str("(\n%AI6_BeginPatternLayer\n800 Ar\n");
            // The layer is a PostScript string: its content contains no bare
            // parentheses of its own (see `ps_string_nested`), so only
            // backslashes need escaping.
            s.push_str(&art.replace('\\', "\\\\"));
            s.push_str("%AI6_EndPatternLayer\n) &\n");
        }
        s.push_str("] E\n%AI3_EndPattern\n");
        s
    }

    // ----- gradients -----------------------------------------------------

    /// `colorSpec colorStyle` of a gradient stop (AI7 spec §3.5.2, table 8).
    fn stop_spec(&mut self, c: &Color) -> String {
        let n = |v: f64| num(clamp01(v));
        match self.map_color(&c.flatten()) {
            Color::Gray(g) => format!("{} 0", n(g)),
            Color::Cmyk(c, m, y, k) => format!("{} {} {} {} 1", n(c), n(m), n(y), n(k)),
            Color::Rgb(r, g, b) => {
                let [c, m, y, k] = rgb_to_cmyk(clamp01(r), clamp01(g), clamp01(b));
                format!("{} {} {} {} {} {} {} 2", n(c), n(m), n(y), n(k), n(r), n(g), n(b))
            }
            Color::Spot { name, cmyk, tint } => {
                self.res.add_spot(&name, cmyk);
                format!(
                    "{} {} {} {} {} {} 3",
                    n(cmyk[0]),
                    n(cmyk[1]),
                    n(cmyk[2]),
                    n(cmyk[3]),
                    ps_string(&name),
                    n(1.0 - clamp01(tint))
                )
            }
            Color::Pattern(_) | Color::Gradient(_) => unreachable!("flattened"),
        }
    }

    /// Name of the gradient definition for `g`, registering it on first use.
    fn gradient_name(&mut self, g: &crate::ir::Gradient) -> String {
        // Illustrator 8 lists linear ramps from the last stop to the first and
        // radial ones from the first to the last; follow it.
        let order: Vec<&crate::ir::GradientStop> = match g.kind {
            GradientKind::Linear => g.stops.iter().rev().collect(),
            GradientKind::Radial => g.stops.iter().collect(),
        };
        let mut stops = Vec::new();
        for st in order {
            let spec = self.stop_spec(&st.color);
            let ramp = num((clamp01(st.offset) * 100.0 * 100.0).round() / 100.0);
            stops.push(format!("{spec} 50 {ramp} %_BS"));
            stops.push(format!("%_{spec} 50 {ramp} Bs"));
        }
        if let Some(e) = self.res.gradients.iter().find(|e| e.kind == g.kind && e.stops == stops) {
            return e.name.clone();
        }
        let base = match g.kind {
            GradientKind::Linear => format!("Linear Gradient {}", self.res.gradients.len() + 1),
            GradientKind::Radial => format!("Radial Gradient {}", self.res.gradients.len() + 1),
        };
        let name = self.res.unique_name(&base);
        self.res.gradients.push(GradientEntry { name: name.clone(), kind: g.kind, stops });
        name
    }

    fn gradient_defs(&mut self) -> String {
        let mut s = String::new();
        for g in &self.res.gradients {
            let name = ps_string(&g.name);
            let _ = writeln!(s, "%AI5_BeginGradient: {name}");
            let kind = if g.kind == GradientKind::Radial { 1 } else { 0 };
            let _ = writeln!(s, "{name} {kind} {} Bd", g.stops.len() / 2);
            s.push_str("[\n");
            for l in &g.stops {
                s.push_str(l);
                s.push('\n');
            }
            s.push_str("BD\n%AI5_EndGradient\n");
        }
        s
    }

    /// Instance geometry for a gradient fill, or `None` when it cannot be
    /// represented (degenerate geometry, fewer than two stops).
    fn gradient_instance(&mut self, gp: &GradientPaint) -> Option<GradientInstance> {
        let g = &gp.gradient;
        if g.stops.len() < 2 || !patterns::is_finite_matrix(&gp.matrix) {
            return None;
        }
        let m = &gp.matrix;
        let det = m.a * m.d - m.b * m.c;
        let scale = m.a.abs().max(m.b.abs()).max(m.c.abs()).max(m.d.abs());
        if scale == 0.0 || det.abs() <= 1e-12 * scale * scale {
            return None;
        }
        let off = |p: Point| Point::new(p.x + self.dx, p.y + self.dy);
        let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let inst = match g.kind {
            GradientKind::Linear => {
                // The ramp value t(q) = (q - start)·g with g = (end-start)/|end-start|²
                // in gradient space; in page space its gradient is M⁻ᵀ g. Any affine
                // image of a linear gradient is a linear gradient, so origin,
                // angle and length describe it exactly (identity Bg matrix).
                let (vx, vy) = (gp.end.x - gp.start.x, gp.end.y - gp.start.y);
                let len2 = vx * vx + vy * vy;
                if len2.is_nan() || len2 <= 1e-18 {
                    return None;
                }
                let (gx, gy) = (vx / len2, vy / len2);
                let ux = (m.d * gx - m.b * gy) / det;
                let uy = (-m.c * gx + m.a * gy) / det;
                let ulen = ux.hypot(uy);
                if !ulen.is_finite() || ulen <= 1e-12 {
                    return None;
                }
                GradientInstance {
                    name: String::new(),
                    radial: false,
                    origin: off(m.apply(gp.start)),
                    angle: uy.atan2(ux).to_degrees(),
                    length: 1.0 / ulen,
                    hilight: [0.0; 4],
                    matrix: identity,
                    bm: None,
                }
            }
            GradientKind::Radial => {
                let r = gp.radius;
                if !r.is_finite() || r <= 1e-9 {
                    return None;
                }
                let tol = 1e-6 * scale;
                let similarity = ((m.a - m.d).abs() <= tol && (m.b + m.c).abs() <= tol)
                    || ((m.a + m.d).abs() <= tol && (m.b - m.c).abs() <= tol);
                let centre = off(m.apply(gp.end));
                let hilight = |dx: f64, dy: f64, radius: f64| {
                    let d = dx.hypot(dy);
                    if d <= 1e-9 * radius.max(1.0) {
                        return [0.0; 4];
                    }
                    // The highlight must lie inside the circle.
                    let k = if d > radius { radius / d } else { 1.0 };
                    let (hx, hy) = (dx * k, dy * k);
                    [hx, hy, hy.atan2(hx).to_degrees(), (d * k / radius).min(1.0)]
                };
                if similarity {
                    let radius = r * det.abs().sqrt();
                    let focal = off(m.apply(gp.start));
                    GradientInstance {
                        name: String::new(),
                        radial: true,
                        origin: centre,
                        angle: 0.0,
                        length: radius,
                        hilight: hilight(focal.x - centre.x, focal.y - centre.y, radius),
                        matrix: identity,
                        bm: None,
                    }
                } else {
                    // A circle of radius r around the page-space centre C,
                    // mapped by F(p) = C + L(p - C) where L is the linear part
                    // of the gradient matrix. Bg's matrix is expressed about the
                    // canvas corner (xo, yo) with b and c negated (AI7 spec
                    // §3.5.4): x' = a(x-xo) - c(y-yo) + tx + xo,
                    //          y' = -b(x-xo) + d(y-yo) + ty + yo.
                    let (xo, yo) = (self.page_w / 2.0 - CANVAS_HALF, self.page_h / 2.0 + CANVAS_HALF);
                    let (tx, ty) = (centre.x - (m.a * centre.x + m.c * centre.y), centre.y - (m.b * centre.x + m.d * centre.y));
                    let (ba, bb, bc, bd) = (m.a, -m.b, -m.c, m.d);
                    let btx = tx - xo + ba * xo - bc * yo;
                    let bty = ty - yo - bb * xo + bd * yo;
                    GradientInstance {
                        name: String::new(),
                        radial: true,
                        origin: centre,
                        angle: 0.0,
                        length: r,
                        hilight: hilight(gp.start.x - gp.end.x, gp.start.y - gp.end.y, r),
                        matrix: [ba, bb, bc, bd, btx, bty],
                        // Imaging matrix: the unit circle onto the ellipse.
                        bm: Some([r * m.a, r * m.b, -r * m.c, -r * m.d, centre.x, centre.y]),
                    }
                }
            }
        };
        let all_finite = [inst.origin.x, inst.origin.y, inst.angle, inst.length]
            .iter()
            .chain(inst.matrix.iter())
            .chain(inst.hilight.iter())
            .all(|v| v.is_finite());
        if !all_finite {
            return None;
        }
        let name = self.gradient_name(g);
        Some(GradientInstance { name, ..inst })
    }

    // ----- paths ---------------------------------------------------------

    /// Emit paint state. `fill` is the colour to set (`None`: no fill colour,
    /// e.g. unfilled or gradient-filled); `filled` says whether the path is
    /// filled at all (fill overprint / fill rule apply).
    fn set_paint(&mut self, p: &PathItem, fill: Option<&Color>, filled: bool) {
        if let Some(f) = fill {
            let op = self.color_op(f, false);
            if self.state.fill.as_deref() != Some(&op) {
                self.line(&op);
                self.state.fill = Some(op);
            }
        } else if filled {
            // Gradient fill: the next solid fill must be re-emitted.
            self.state.fill = None;
        }
        if filled && self.state.fill_op != Some(p.fill_overprint) {
            self.line(if p.fill_overprint { "1 O" } else { "0 O" });
            self.state.fill_op = Some(p.fill_overprint);
        }
        if let Some(s) = &p.stroke {
            let op = self.color_op(&s.color, true);
            if self.state.stroke.as_deref() != Some(&op) {
                self.line(&op);
                self.state.stroke = Some(op);
            }
            if self.state.stroke_op != Some(p.stroke_overprint) {
                self.line(if p.stroke_overprint { "1 R" } else { "0 R" });
                self.state.stroke_op = Some(p.stroke_overprint);
            }
            let style = stroke_style(s);
            if self.state.style.as_deref() != Some(&style) {
                self.line(&style);
                self.state.style = Some(style);
            }
        }
        if filled && self.state.fill_rule != Some(p.even_odd) {
            self.line(if p.even_odd { "1 XR" } else { "0 XR" });
            self.state.fill_rule = Some(p.even_odd);
        }
    }

    fn pt(&self, p: &Point) -> (String, String) {
        (num(p.x + self.dx), num(p.y + self.dy))
    }

    /// Emit the construction operators of one subpath (no paint operator).
    fn subpath(&mut self, sp: &Subpath) {
        let (x, y) = self.pt(&sp.start);
        self.line(&format!("{} {} m", x, y));
        // Zero-length segments (SVG `h0`, PDF `l` to the current point) are
        // dropped: laser/cutter programs may discard paths containing them.
        let mut segs: Vec<Segment> = Vec::with_capacity(sp.segments.len() + 1);
        let mut cur = sp.start;
        for s in &sp.segments {
            let degenerate = match s {
                Segment::Line(p) => near(*p, cur, 1e-4),
                Segment::Curve(a, b, c) => near(*a, cur, 1e-4) && near(*b, cur, 1e-4) && near(*c, cur, 1e-4),
            };
            if !degenerate {
                segs.push(s.clone());
                cur = end_of(s);
            }
        }
        // The closing edge is always written out, even though the lowercase
        // paint operator closes the path: some laser/cutter programs only
        // draw the segments present in the file and drop the implicit edge.
        // An end point that misses the start by a hair is moved onto it
        // instead of adding a tiny closing segment.
        if sp.closed && !near(cur, sp.start, 1e-4) {
            match segs.last_mut() {
                Some(Segment::Line(p)) | Some(Segment::Curve(_, _, p)) if near(*p, sp.start, CLOSE_SNAP) => *p = sp.start,
                _ => segs.push(Segment::Line(sp.start)),
            }
        }
        for s in &segs {
            match s {
                Segment::Line(p) => {
                    let (x, y) = self.pt(p);
                    self.line(&format!("{} {} L", x, y));
                }
                Segment::Curve(a, b, c) => {
                    let (ax, ay) = self.pt(a);
                    let (bx, by) = self.pt(b);
                    let (cx, cy) = self.pt(c);
                    self.line(&format!("{} {} {} {} {} {} C", ax, ay, bx, by, cx, cy));
                }
            }
        }
    }

    /// `Bb` … `Bg` … `f` … `BB` for one (sub)path (AI7 spec §3.5.3).
    fn gradient_paint(&mut self, gi: &GradientInstance, closed: bool, stroked: bool) {
        self.line("Bb");
        if gi.radial {
            let h = gi.hilight;
            self.line(&format!("{} {} {} {} Bh", num(h[0]), num(h[1]), num(h[2]), num(h[3])));
        }
        let m = gi.matrix;
        self.line(&format!(
            "1 {} {} {} {} {} {} {} {} {} {} {} Bg",
            ps_string(&gi.name),
            num(gi.origin.x),
            num(gi.origin.y),
            num(gi.angle),
            num(gi.length),
            num(m[0]),
            num(m[1]),
            num(m[2]),
            num(m[3]),
            num(m[4]),
            num(m[5])
        ));
        if let Some(b) = gi.bm {
            self.line(&format!("{} {} {} {} {} {} Bm", num(b[0]), num(b[1]), num(b[2]), num(b[3]), num(b[4]), num(b[5])));
        }
        self.line(if closed { "f" } else { "F" });
        // 0: no stroke, 1: stroke, 2: close and stroke.
        self.line(match (stroked, closed) {
            (false, _) => "0 BB",
            (true, false) => "1 BB",
            (true, true) => "2 BB",
        });
    }

    fn path(&mut self, p: &PathItem) {
        let subs: Vec<&Subpath> = p.subpaths.iter().filter(|s| !s.segments.is_empty()).collect();
        if subs.is_empty() {
            return;
        }
        let gradient = match &p.fill {
            Some(Color::Gradient(g)) if !self.in_tile => self.gradient_instance(g),
            _ => None,
        };
        // Gradient strokes do not exist in AI8: flatten them.
        let flat_stroke;
        let p = match &p.stroke {
            Some(s) if matches!(s.color, Color::Gradient(_)) => {
                let mut q = p.clone();
                if let Some(s) = q.stroke.as_mut() {
                    s.color = s.color.flatten();
                }
                flat_stroke = q;
                &flat_stroke
            }
            _ => p,
        };
        let fill = if gradient.is_some() { None } else { p.fill.clone() };
        self.set_paint(p, fill.as_ref(), p.fill.is_some());
        let compound = subs.len() > 1;
        if compound {
            self.line("*u");
        }
        for sp in subs {
            self.subpath(sp);
            if let Some(gi) = &gradient {
                self.gradient_paint(gi, sp.closed, p.stroke.is_some());
                continue;
            }
            let op = match (p.fill.is_some(), p.stroke.is_some(), sp.closed) {
                (true, true, true) => "b",
                (true, true, false) => "B",
                (true, false, true) => "f",
                (true, false, false) => "F",
                (false, true, true) => "s",
                (false, true, false) => "S",
                (false, false, true) => "n",
                (false, false, false) => "N",
            };
            self.line(op);
        }
        if compound {
            self.line("*U");
        }
    }

    fn element(&mut self, e: &Element) {
        match e {
            Element::Path(p) => self.path(p),
            Element::Group(children) => {
                if children.is_empty() {
                    return;
                }
                self.line("u");
                for c in children {
                    self.element(c);
                }
                self.line("U");
            }
            Element::ClipGroup { clip, even_odd, children } => {
                let subs: Vec<&Subpath> = clip.iter().filter(|s| !s.segments.is_empty()).collect();
                if subs.is_empty() {
                    // Degenerate clip: nothing would be visible.
                    return;
                }
                self.line("q");
                if self.state.fill_rule != Some(*even_odd) {
                    self.line(if *even_odd { "1 XR" } else { "0 XR" });
                    self.state.fill_rule = Some(*even_odd);
                }
                let compound = subs.len() > 1;
                if compound {
                    self.line("*u");
                }
                for sp in subs {
                    self.subpath(sp);
                    self.line("h W");
                    self.line("n");
                }
                if compound {
                    self.line("*U");
                }
                for c in children {
                    self.element(c);
                }
                self.line("Q");
                // Paint state inside a clip group is local in Illustrator's
                // reader; force re-emission afterwards.
                self.state = State::default();
            }
        }
    }
}

/// Split a group or clip group into two with halves of its children (the
/// clip repeated), for pattern layers that would exceed the string limit.
fn split_element(e: &Element) -> Option<(Element, Element)> {
    match e {
        Element::Group(c) if c.len() > 1 => {
            let (a, b) = c.split_at(c.len() / 2);
            Some((Element::Group(a.to_vec()), Element::Group(b.to_vec())))
        }
        Element::ClipGroup { clip, even_odd, children } => {
            let mk = |c: Vec<Element>| Element::ClipGroup { clip: clip.clone(), even_odd: *even_odd, children: c };
            if children.len() > 1 {
                let (a, b) = children.split_at(children.len() / 2);
                Some((mk(a.to_vec()), mk(b.to_vec())))
            } else {
                let (a, b) = split_element(children.first()?)?;
                Some((mk(vec![a]), mk(vec![b])))
            }
        }
        Element::Group(c) if c.len() == 1 => split_element(&c[0]),
        _ => None,
    }
}

fn stroke_style(s: &Stroke) -> String {
    let cap = match s.cap {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
    };
    let join = match s.join {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
    };
    let dash: Vec<String> = s.dash.iter().map(|d| num(*d)).collect();
    format!(
        "{} J {} j {} w {} M [{}]{} d",
        cap,
        join,
        num(s.width.max(0.0)),
        num(s.miter_limit.max(1.0)),
        dash.join(" "),
        num(s.dash_phase)
    )
}

/// "MM/DD/YY" style date without pulling in a date crate.
fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:02}/{:02}/{:02}", m, d, y % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::*;

    fn square(x: f64, y: f64, s: f64) -> Subpath {
        let mut sp = Subpath::new(Point::new(x, y));
        sp.segments.push(Segment::Line(Point::new(x + s, y)));
        sp.segments.push(Segment::Line(Point::new(x + s, y + s)));
        sp.segments.push(Segment::Line(Point::new(x, y + s)));
        sp.closed = true;
        sp
    }

    fn doc_with(els: Vec<Element>) -> Document {
        let mut d = Document::new(Rect { llx: 0.0, lly: 0.0, urx: 200.0, ury: 100.0 });
        d.elements = els;
        d
    }

    #[test]
    fn number_formatting() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(0.5), "0.5");
        assert_eq!(num(-0.00001), "0");
        assert_eq!(num(12.345678), "12.3457");
        assert_eq!(num(f64::NAN), "0");
    }

    #[test]
    fn string_escaping() {
        assert_eq!(ps_string("a(b)c\\"), "(a\\(b\\)c\\\\)");
        assert_eq!(ps_string("æ"), "(\\346)");
    }

    #[test]
    fn header_and_trailer() {
        let ai = write_ai8(&doc_with(vec![]), &WriteOptions::default());
        assert!(ai.starts_with("%!PS-Adobe-3.0"));
        assert!(ai.contains("%%Creator: Adobe Illustrator(R) 8.0"));
        assert!(ai.contains("%AI5_FileFormat 4.0"));
        assert!(ai.contains("%AI5_ArtSize: 200 100"));
        assert!(ai.contains("%%BoundingBox: 0 0 200 100"));
        assert!(ai.contains("%AI5_BeginLayer"));
        assert!(ai.trim_end().ends_with("%%EOF"));
    }

    #[test]
    fn filled_cmyk_square() {
        let mut p = PathItem::new(vec![square(10.0, 10.0, 50.0)]);
        p.fill = Some(Color::Cmyk(0.0, 1.0, 1.0, 0.0));
        let ai = write_ai8(&doc_with(vec![Element::Path(p)]), &WriteOptions::default());
        assert!(ai.contains("0 1 1 0 k\n"));
        assert!(ai.contains("10 10 m\n60 10 L\n60 60 L\n10 60 L\n10 10 L\nf\n"));
        assert!(ai.contains("%%BoundingBox: 10 10 60 60"));
    }

    /// Closed paths always carry an explicit closing edge (laser software
    /// that ignores the implicit close would drop it), without duplicating
    /// one that the path already has.
    #[test]
    fn closing_edge_written_once() {
        let mut open_end = Subpath::new(Point::new(0.0, 0.0));
        open_end.segments.push(Segment::Line(Point::new(10.0, 0.0)));
        open_end.segments.push(Segment::Line(Point::new(10.0, 10.0)));
        open_end.closed = true;
        let mut explicit = open_end.clone();
        explicit.segments.push(Segment::Line(Point::new(0.0, 0.0)));
        for sp in [open_end, explicit] {
            let mut p = PathItem::new(vec![sp]);
            p.stroke = Some(Stroke::solid(Color::Gray(0.0), 1.0));
            let ai = write_ai8(&doc_with(vec![Element::Path(p)]), &WriteOptions::default());
            assert!(ai.contains("0 0 m\n10 0 L\n10 10 L\n0 0 L\ns\n"), "{ai}");
        }
    }

    /// Zero-length segments (SVG `h0`) are dropped and an end point a hair
    /// away from the start is moved onto it, as in an Illustrator SVG export
    /// whose shape went missing in a laser program's outline view.
    #[test]
    fn degenerate_segments_and_near_closure() {
        let mut sp = Subpath::new(Point::new(0.0, 0.0));
        sp.segments.push(Segment::Line(Point::new(10.0, 0.0)));
        sp.segments.push(Segment::Line(Point::new(10.0, 0.0)));
        sp.segments.push(Segment::Line(Point::new(10.0, 10.0)));
        sp.segments.push(Segment::Line(Point::new(-0.02, 0.01)));
        sp.segments.push(Segment::Line(Point::new(-0.02, 0.01)));
        sp.closed = true;
        let mut p = PathItem::new(vec![sp]);
        p.fill = Some(Color::Gray(0.0));
        let ai = write_ai8(&doc_with(vec![Element::Path(p)]), &WriteOptions::default());
        assert!(ai.contains("0 0 m\n10 0 L\n10 10 L\n0 0 L\nf\n"), "{ai}");
    }

    #[test]
    fn stroke_rgb_and_style() {
        let mut p = PathItem::new(vec![{
            let mut s = Subpath::new(Point::new(0.0, 0.0));
            s.segments.push(Segment::Curve(Point::new(1.0, 2.0), Point::new(3.0, 4.0), Point::new(5.0, 6.0)));
            s
        }]);
        let mut st = Stroke::solid(Color::Rgb(1.0, 0.0, 0.0), 2.5);
        st.cap = LineCap::Round;
        st.dash = vec![3.0, 1.0];
        p.stroke = Some(st);
        let ai = write_ai8(&doc_with(vec![Element::Path(p)]), &WriteOptions::default());
        assert!(ai.contains("1 0 0 XA"));
        assert!(ai.contains("0 1 1 0 K"));
        assert!(ai.contains("1 J 0 j 2.5 w 10 M [3 1]0 d"));
        assert!(ai.contains("1 2 3 4 5 6 C\nS\n"));
    }

    #[test]
    fn compound_path_and_evenodd() {
        let mut p = PathItem::new(vec![square(0.0, 0.0, 100.0), square(25.0, 25.0, 50.0)]);
        p.fill = Some(Color::Gray(0.0));
        p.even_odd = true;
        let ai = write_ai8(&doc_with(vec![Element::Path(p)]), &WriteOptions::default());
        assert!(ai.contains("1 XR"));
        let u = ai.find("*u").unwrap();
        let uu = ai.find("*U").unwrap();
        assert!(u < uu);
        assert_eq!(ai[u..uu].matches("\nf\n").count(), 2);
    }

    #[test]
    fn spot_colour_declared_and_used() {
        let mut p = PathItem::new(vec![square(0.0, 0.0, 10.0)]);
        p.stroke = Some(Stroke::solid(
            Color::Spot { name: "CutContour".into(), cmyk: [0.0, 1.0, 0.0, 0.0], tint: 1.0 },
            0.25,
        ));
        let ai = write_ai8(&doc_with(vec![Element::Path(p)]), &WriteOptions::default());
        assert!(ai.contains("%%DocumentCustomColors: (CutContour)"));
        assert!(ai.contains("%%CMYKCustomColor: 0 1 0 0 (CutContour)"));
        assert!(ai.contains("0 1 0 0 (CutContour) 0 X"));
        assert!(ai.contains("\ns\n"));
    }

    #[test]
    fn groups_and_clip_groups() {
        let mut p = PathItem::new(vec![square(0.0, 0.0, 10.0)]);
        p.fill = Some(Color::Gray(0.5));
        let els = vec![Element::Group(vec![Element::ClipGroup {
            clip: vec![square(0.0, 0.0, 5.0)],
            even_odd: false,
            children: vec![Element::Path(p)],
        }])];
        let ai = write_ai8(&doc_with(els), &WriteOptions::default());
        let body = &ai[ai.find("0 A\n").unwrap()..ai.find("LB\n").unwrap()];
        let ops: Vec<&str> = body.lines().filter(|l| ["u", "U", "q", "Q", "h W", "n", "f"].contains(l)).collect();
        assert_eq!(ops, vec!["u", "q", "h W", "n", "f", "Q", "U"]);
    }

    #[test]
    fn page_offset_is_normalised() {
        let mut d = doc_with(vec![]);
        d.page = Rect { llx: 100.0, lly: 50.0, urx: 300.0, ury: 150.0 };
        let mut p = PathItem::new(vec![square(100.0, 50.0, 10.0)]);
        p.fill = Some(Color::Gray(0.0));
        d.elements.push(Element::Path(p));
        let ai = write_ai8(&d, &WriteOptions::default());
        assert!(ai.contains("0 0 m\n10 0 L"));
        assert!(ai.contains("%AI5_ArtSize: 200 100"));
    }

    #[test]
    fn colour_mode_conversion() {
        let mut p = PathItem::new(vec![square(0.0, 0.0, 10.0)]);
        p.fill = Some(Color::Rgb(0.0, 0.0, 1.0));
        let opts = WriteOptions { color_mode: ColorMode::Cmyk, ..Default::default() };
        let ai = write_ai8(&doc_with(vec![Element::Path(p.clone())]), &opts);
        assert!(ai.contains("1 1 0 0 k\n"));
        assert!(!ai.contains("Xa"));

        p.fill = Some(Color::Cmyk(0.0, 0.0, 0.0, 1.0));
        let opts = WriteOptions { color_mode: ColorMode::Rgb, ..Default::default() };
        let ai = write_ai8(&doc_with(vec![Element::Path(p)]), &opts);
        assert!(ai.contains("0 0 0 Xa"));
    }

    #[test]
    fn redundant_state_not_repeated() {
        let mk = |x| {
            let mut p = PathItem::new(vec![square(x, 0.0, 5.0)]);
            p.fill = Some(Color::Cmyk(1.0, 0.0, 0.0, 0.0));
            Element::Path(p)
        };
        let ai = write_ai8(&doc_with(vec![mk(0.0), mk(10.0)]), &WriteOptions::default());
        assert_eq!(ai.matches("1 0 0 0 k").count(), 1);
    }
    #[test]
    fn nested_string_escaping() {
        assert_eq!(ps_string_nested("a(b)c\\"), "(a\\050b\\051c\\134)");
        assert_eq!(ps_string_nested("plain"), "(plain)");
    }

    fn pattern_doc(fill: Color) -> Document {
        let mut p = PathItem::new(vec![square(0.0, 0.0, 50.0)]);
        p.fill = Some(fill);
        doc_with(vec![Element::Path(p)])
    }

    #[test]
    fn pattern_definition_and_use() {
        let mut dot = PathItem::new(vec![square(2.0, 3.0, 4.0)]);
        dot.fill = Some(Color::Gray(0.25));
        let pat = std::sync::Arc::new(Pattern {
            name: "Dot".into(),
            bbox: Rect { llx: 2.0, lly: 3.0, urx: 10.0, ury: 11.0 },
            x_step: 8.0,
            y_step: 8.0,
            elements: vec![Element::Path(dot)],
        });
        let ai = write_ai8(
            &pattern_doc(Color::Pattern(PatternPaint { pattern: pat, matrix: Matrix::scale(2.0, 2.0) })),
            &WriteOptions::default(),
        );
        assert!(ai.contains("%AI3_BeginPattern: (Dot)\n(Dot) 0 0 8 8 [\n%AI3_Tile\n(0 O 0 R 0 g\n0 G\n) @\n(\n%AI6_BeginPatternLayer\n800 Ar\n0.25 g\n"));
        assert!(ai.contains("0 0 m\n4 0 L\n4 4 L\n0 4 L\n0 0 L\nf\n%AI6_EndPatternLayer\n) &\n] E\n%AI3_EndPattern\n"));
        assert!(ai.contains("(Dot) 0 0 1 1 0 0 0 0 0 [2 0 0 2 4 6] p\n"));
        // No gradients: no gradient count.
        assert!(!ai.contains(" Bn\n"));
    }

    #[test]
    fn gradient_definition_layout() {
        let g = std::sync::Arc::new(Gradient {
            kind: GradientKind::Linear,
            stops: vec![
                GradientStop { offset: 0.0, color: Color::Gray(1.0) },
                GradientStop { offset: 0.9, color: Color::Gray(0.0) },
            ],
        });
        let ai = write_ai8(
            &pattern_doc(Color::Gradient(GradientPaint {
                gradient: g,
                start: Point::new(0.0, 0.0),
                end: Point::new(0.0, 50.0),
                radius: 0.0,
                matrix: Matrix::IDENTITY,
            })),
            &WriteOptions::default(),
        );
        // Same stop layout as Illustrator 8's "Black & White" swatch.
        assert!(ai.contains("1 Bn\n%AI5_BeginGradient: (Linear Gradient 1)\n(Linear Gradient 1) 0 2 Bd\n[\n0 0 50 90 %_BS\n%_0 0 50 90 Bs\n1 0 50 0 %_BS\n%_1 0 50 0 Bs\nBD\n%AI5_EndGradient\n"));
        assert!(ai.contains("Bb\n1 (Linear Gradient 1) 0 0 90 50 1 0 0 1 0 0 Bg\nf\n0 BB\n"));
    }
}
