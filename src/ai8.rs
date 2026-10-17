//! Writer for the Adobe Illustrator 8 native file format (PostScript based,
//! "AI5_FileFormat 4.0"), as described in the Adobe Illustrator File Format
//! Specification and as emitted by Illustrator 8.0 itself.
//!
//! The output contains no procset bodies (like Illustrator's own
//! "include: none" saves). Illustrator and AI-importing applications (cutters,
//! RIPs, CorelDRAW, etc.) parse the operators directly.

use crate::ir::{Color, Document, Element, LineCap, LineJoin, PathItem, Rect, Segment, Stroke, Subpath};
use std::fmt::Write as _;

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
    let mut out = String::from("(");
    for ch in s.chars() {
        match ch {
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

/// Tracks the last emitted paint state so redundant operators are skipped.
#[derive(Default)]
struct State {
    fill: Option<String>,
    stroke: Option<String>,
    style: Option<String>,
    fill_rule: Option<bool>,
    fill_op: Option<bool>,
    stroke_op: Option<bool>,
}

struct Writer {
    out: String,
    opts: WriteOptions,
    /// Translation applied to every coordinate so the page starts at 0,0.
    dx: f64,
    dy: f64,
    state: State,
}

/// Serialize a document as an Illustrator 8 file.
pub fn write_ai8(doc: &Document, opts: &WriteOptions) -> String {
    let mut w = Writer { out: String::new(), opts: opts.clone(), dx: -doc.page.llx, dy: -doc.page.lly, state: State::default() };
    w.header(doc);
    w.out.push_str("%AI5_BeginLayer\n1 1 1 1 0 0 1 0 79 128 255 0 50 Lb\n(Layer 1) Ln\n0 A\n");
    for e in &doc.elements {
        w.element(e);
    }
    w.out.push_str("LB\n%AI5_EndLayer--\n");
    w.trailer();
    w.out
}

/// Collect unique spot colours (name -> CMYK alternate), in first-use order.
fn collect_spots(els: &[Element], out: &mut Vec<(String, [f64; 4])>) {
    fn add(c: &Color, out: &mut Vec<(String, [f64; 4])>) {
        if let Color::Spot { name, cmyk, .. } = c
            && !out.iter().any(|(n, _)| n == name) {
                out.push((name.clone(), *cmyk));
            }
    }
    for e in els {
        match e {
            Element::Path(p) => {
                if let Some(f) = &p.fill {
                    add(f, out);
                }
                if let Some(s) = &p.stroke {
                    add(&s.color, out);
                }
            }
            Element::Group(c) => collect_spots(c, out),
            Element::ClipGroup { children, .. } => collect_spots(children, out),
        }
    }
}

impl Writer {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn header(&mut self, doc: &Document) {
        let w = doc.page.width().max(1.0);
        let h = doc.page.height().max(1.0);
        let bounds = doc
            .content_bounds()
            .map(|r| Rect { llx: r.llx + self.dx, lly: r.lly + self.dy, urx: r.urx + self.dx, ury: r.ury + self.dy })
            .unwrap_or(Rect { llx: 0.0, lly: 0.0, urx: w, ury: h });
        let mut spots = Vec::new();
        collect_spots(&doc.elements, &mut spots);

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
        self.line("%AI5_Begin_NonPrinting");
        self.line("Np");
        self.line("%AI5_End_NonPrinting--");
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
    fn color_op(&self, c: &Color, stroke: bool) -> String {
        // TODO(native patterns/gradients): flat approximation for now.
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
            Color::Spot { name, cmyk, tint } => format!(
                "{} {} {} {} {} {} {}",
                n(cmyk[0]),
                n(cmyk[1]),
                n(cmyk[2]),
                n(cmyk[3]),
                ps_string(&name),
                n(1.0 - clamp01(tint)),
                if stroke { "X" } else { "x" }
            ),
        }
    }

    fn set_paint(&mut self, p: &PathItem) {
        if let Some(f) = &p.fill {
            let op = self.color_op(f, false);
            if self.state.fill.as_deref() != Some(&op) {
                self.line(&op);
                self.state.fill = Some(op);
            }
            if self.state.fill_op != Some(p.fill_overprint) {
                self.line(if p.fill_overprint { "1 O" } else { "0 O" });
                self.state.fill_op = Some(p.fill_overprint);
            }
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
        if p.fill.is_some() && self.state.fill_rule != Some(p.even_odd) {
            self.line(if p.even_odd { "1 XR" } else { "0 XR" });
            self.state.fill_rule = Some(p.even_odd);
        }
    }

    fn pt(&self, p: &crate::ir::Point) -> (String, String) {
        (num(p.x + self.dx), num(p.y + self.dy))
    }

    /// Emit the construction operators of one subpath (no paint operator).
    fn subpath(&mut self, sp: &Subpath) {
        let (x, y) = self.pt(&sp.start);
        self.line(&format!("{} {} m", x, y));
        let mut segs: &[Segment] = &sp.segments;
        // A closing line back to the start point is redundant for closed paths.
        if sp.closed
            && let Some(Segment::Line(p)) = segs.last()
                && (p.x - sp.start.x).abs() < 1e-6 && (p.y - sp.start.y).abs() < 1e-6 {
                    segs = &segs[..segs.len() - 1];
                }
        for s in segs {
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

    fn path(&mut self, p: &PathItem) {
        let subs: Vec<&Subpath> = p.subpaths.iter().filter(|s| !s.segments.is_empty()).collect();
        if subs.is_empty() {
            return;
        }
        self.set_paint(p);
        let compound = subs.len() > 1;
        if compound {
            self.line("*u");
        }
        for sp in subs {
            self.subpath(sp);
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
        assert!(ai.contains("10 10 m\n60 10 L\n60 60 L\n10 60 L\nf\n"));
        assert!(ai.contains("%%BoundingBox: 10 10 60 60"));
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
}
