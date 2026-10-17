//! SVG writer: the same cleaned-up geometry as the AI8 output (see
//! [`Subpath::cutter_segments`]), as a plain SVG 1.1 file for laser and
//! cutter software that reads SVG.
//!
//! One SVG user unit is one point; the page size is given in points. Paths,
//! groups, clipping, solid colours, strokes, tiling patterns and linear and
//! radial gradients are written natively. Colours are written as sRGB; CMYK
//! and spot colours are converted the way Ghostscript displays them (see
//! [`cmyk_to_srgb`]).

use crate::ai8::num;
use crate::patterns;
use crate::ir::{
    Color, Document, Element, Gradient, GradientKind, GradientPaint, LineCap, LineJoin, Matrix, Pattern, PatternPaint,
    PathItem, Segment, Stroke, Subpath,
};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

/// Write `doc` as an SVG document.
pub fn write_svg(doc: &Document) -> String {
    let page = doc.page;
    let (w, h) = (page.width().max(1.0), page.height().max(1.0));
    // IR page space (y up, origin at the page's lower left) to SVG (y down).
    let flip = Matrix::new(1.0, 0.0, 0.0, -1.0, -page.llx, page.ury);
    let mut wr = Writer::default();
    let mut body = String::new();
    for e in &doc.elements {
        wr.element(&mut body, e, &flip, 1);
    }
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\" width=\"{w}pt\" height=\"{h}pt\" viewBox=\"0 0 {w} {h}\">",
        w = num(w),
        h = num(h)
    );
    if !wr.defs.is_empty() {
        out.push_str("  <defs>\n");
        out.push_str(&wr.defs);
        out.push_str("  </defs>\n");
    }
    out.push_str(&body);
    out.push_str("</svg>\n");
    out
}

#[derive(Default)]
struct Writer {
    defs: String,
    next_id: usize,
    /// Pattern definitions already written, keyed by the shared definition
    /// and the SVG transform of the use (an SVG pattern carries both).
    patterns: HashMap<(usize, String), String>,
}

fn indent(n: usize) -> String {
    "  ".repeat(n)
}

fn transform_attr(m: &Matrix) -> String {
    format!("matrix({} {} {} {} {} {})", num(m.a), num(m.b), num(m.c), num(m.d), num(m.e), num(m.f))
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;")
}

/// Path data for `subpaths` mapped through `m`.
fn path_data(subpaths: &[Subpath], m: &Matrix) -> String {
    let mut d = String::new();
    for sp in subpaths {
        let segs = sp.cutter_segments();
        if segs.is_empty() {
            continue;
        }
        let p = m.apply(sp.start);
        if !d.is_empty() {
            d.push(' ');
        }
        let _ = write!(d, "M{} {}", num(p.x), num(p.y));
        for s in &segs {
            match s {
                Segment::Line(p) => {
                    let p = m.apply(*p);
                    let _ = write!(d, " L{} {}", num(p.x), num(p.y));
                }
                Segment::Curve(a, b, c) => {
                    let (a, b, c) = (m.apply(*a), m.apply(*b), m.apply(*c));
                    let _ = write!(d, " C{} {} {} {} {} {}", num(a.x), num(a.y), num(b.x), num(b.y), num(c.x), num(c.y));
                }
            }
        }
        if sp.closed {
            d.push_str(" Z");
        }
    }
    d
}

/// sRGB hex colour of a solid paint.
fn hex(c: &Color) -> String {
    let [r, g, b] = match c {
        Color::Gray(g) => [*g, *g, *g],
        Color::Rgb(r, g, b) => [*r, *g, *b],
        Color::Cmyk(c, m, y, k) => cmyk_to_srgb([*c, *m, *y, *k]),
        Color::Spot { cmyk, tint, .. } => cmyk_to_srgb(cmyk.map(|v| v * tint)),
        other => return hex(&other.flatten()),
    };
    let to = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", to(r), to(g), to(b))
}

/// Ghostscript's CMYK to sRGB conversion (default ICC profiles) sampled on
/// a 9x9x9x9 grid by `scripts/make-cmyk-lut.sh`: RGB bytes, index
/// `((c*9 + m)*9 + y)*9 + k`.
static CMYK_LUT: &[u8; 6561 * 3] = include_bytes!("../assets/cmyk-srgb.lut");

/// CMYK (0..=1) to sRGB (0..=1), interpolated from [`CMYK_LUT`], so that
/// for example 100% cyan becomes the familiar #00aeef rather than #00ffff.
pub fn cmyk_to_srgb(cmyk: [f64; 4]) -> [f64; 3] {
    const N: usize = 9;
    let mut base = [0usize; 4];
    let mut frac = [0f64; 4];
    for i in 0..4 {
        let v = if cmyk[i].is_finite() { cmyk[i].clamp(0.0, 1.0) } else { 0.0 } * (N - 1) as f64;
        base[i] = (v.floor() as usize).min(N - 2);
        frac[i] = v - base[i] as f64;
    }
    let mut rgb = [0f64; 3];
    for corner in 0..16usize {
        let mut w = 1.0;
        let mut idx = 0;
        for i in 0..4 {
            let bit = (corner >> i) & 1;
            w *= if bit == 1 { frac[i] } else { 1.0 - frac[i] };
            idx = idx * N + base[i] + bit;
        }
        if w == 0.0 {
            continue;
        }
        for (ch, out) in rgb.iter_mut().enumerate() {
            *out += w * CMYK_LUT[idx * 3 + ch] as f64;
        }
    }
    rgb.map(|v| v / 255.0)
}

impl Writer {
    fn id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}{}", self.next_id)
    }

    fn element(&mut self, out: &mut String, e: &Element, m: &Matrix, depth: usize) {
        let ind = indent(depth);
        match e {
            Element::Path(p) => self.path(out, p, m, depth),
            Element::Group(children) => {
                if children.is_empty() {
                    return;
                }
                let _ = writeln!(out, "{ind}<g>");
                for c in children {
                    self.element(out, c, m, depth + 1);
                }
                let _ = writeln!(out, "{ind}</g>");
            }
            Element::ClipGroup { clip, even_odd, children } => {
                let d = path_data(clip, m);
                if d.is_empty() {
                    // Degenerate clip: nothing would be visible.
                    return;
                }
                let id = self.id("clip");
                let rule = if *even_odd { " clip-rule=\"evenodd\"" } else { "" };
                let _ = writeln!(self.defs, "    <clipPath id=\"{id}\"><path d=\"{d}\"{rule}/></clipPath>");
                let _ = writeln!(out, "{ind}<g clip-path=\"url(#{id})\">");
                for c in children {
                    self.element(out, c, m, depth + 1);
                }
                let _ = writeln!(out, "{ind}</g>");
            }
        }
    }

    fn path(&mut self, out: &mut String, p: &PathItem, m: &Matrix, depth: usize) {
        let d = path_data(&p.subpaths, m);
        if d.is_empty() {
            return;
        }
        let mut attrs = String::new();
        match &p.fill {
            Some(c) => {
                let paint = self.paint(c, m);
                let _ = write!(attrs, " fill=\"{paint}\"");
                if p.even_odd {
                    attrs.push_str(" fill-rule=\"evenodd\"");
                }
            }
            None => attrs.push_str(" fill=\"none\""),
        }
        if let Some(s) = &p.stroke {
            self.stroke_attrs(&mut attrs, s, m);
        }
        let _ = writeln!(out, "{}<path d=\"{d}\"{attrs}/>", indent(depth));
    }

    fn stroke_attrs(&mut self, attrs: &mut String, s: &Stroke, m: &Matrix) {
        let paint = self.paint(&s.color, m);
        let _ = write!(attrs, " stroke=\"{paint}\" stroke-width=\"{}\"", num(s.width));
        match s.cap {
            LineCap::Butt => {}
            LineCap::Round => attrs.push_str(" stroke-linecap=\"round\""),
            LineCap::Square => attrs.push_str(" stroke-linecap=\"square\""),
        }
        match s.join {
            LineJoin::Miter => {
                if (s.miter_limit - 4.0).abs() > 1e-9 {
                    let _ = write!(attrs, " stroke-miterlimit=\"{}\"", num(s.miter_limit.max(1.0)));
                }
            }
            LineJoin::Round => attrs.push_str(" stroke-linejoin=\"round\""),
            LineJoin::Bevel => attrs.push_str(" stroke-linejoin=\"bevel\""),
        }
        if !s.dash.is_empty() && s.dash.iter().any(|v| *v > 0.0) {
            let dash: Vec<String> = s.dash.iter().map(|v| num(*v)).collect();
            let _ = write!(attrs, " stroke-dasharray=\"{}\"", dash.join(" "));
            if s.dash_phase != 0.0 {
                let _ = write!(attrs, " stroke-dashoffset=\"{}\"", num(s.dash_phase));
            }
        }
    }

    /// `fill`/`stroke` attribute value for a paint used in space `m`.
    fn paint(&mut self, c: &Color, m: &Matrix) -> String {
        match c {
            Color::Pattern(pp) => format!("url(#{})", self.pattern(pp, m)),
            Color::Gradient(g) => format!("url(#{})", self.gradient(g, m)),
            c => hex(c),
        }
    }

    fn pattern(&mut self, pp: &PatternPaint, m: &Matrix) -> String {
        let t = pp.matrix.then(m);
        let key = (Arc::as_ptr(&pp.pattern) as usize, transform_attr(&t));
        if let Some(id) = self.patterns.get(&key) {
            return id.clone();
        }
        let pat: &Pattern = &pp.pattern;
        let id = self.id("pattern");
        self.patterns.insert(key, id.clone());
        // The tile art is written in pattern space; the pattern transform
        // maps it into the space of the shape that uses it. SVG clips the art
        // at the repeat cell, so art reaching into neighbouring cells (bbox
        // larger than the step) is wrapped into the cell as copies, the same
        // way as for the AI8 output.
        let (xs, ys) = patterns::tile_steps(pat).unwrap_or((1.0, 1.0));
        let art = patterns::tile_cell_art(pat, xs, ys);
        // The cell is moved to the origin (x = y = 0), with the offset in the
        // pattern transform: some SVG readers mishandle a tile at x/y != 0.
        let (ox, oy) = (pat.bbox.llx, pat.bbox.lly);
        let to_cell = Matrix::translate(-ox, -oy);
        let mut tile = String::new();
        for e in &art {
            self.element(&mut tile, e, &to_cell, 3);
        }
        let t = Matrix::translate(ox, oy).then(&t);
        let _ = writeln!(
            self.defs,
            "    <pattern id=\"{id}\" data-name=\"{}\" patternUnits=\"userSpaceOnUse\" width=\"{}\" height=\"{}\" patternTransform=\"{}\">",
            escape_attr(&pat.name),
            num(xs),
            num(ys),
            transform_attr(&t)
        );
        self.defs.push_str(&tile);
        self.defs.push_str("    </pattern>\n");
        id
    }

    fn gradient(&mut self, g: &GradientPaint, m: &Matrix) -> String {
        let id = self.id("gradient");
        let t = g.matrix.then(m);
        let grad: &Gradient = &g.gradient;
        let head = match grad.kind {
            GradientKind::Linear => format!(
                "<linearGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" gradientTransform=\"{}\">",
                num(g.start.x),
                num(g.start.y),
                num(g.end.x),
                num(g.end.y),
                transform_attr(&t)
            ),
            GradientKind::Radial => format!(
                "<radialGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" cx=\"{}\" cy=\"{}\" r=\"{}\" fx=\"{}\" fy=\"{}\" gradientTransform=\"{}\">",
                num(g.end.x),
                num(g.end.y),
                num(g.radius),
                num(g.start.x),
                num(g.start.y),
                transform_attr(&t)
            ),
        };
        let _ = writeln!(self.defs, "    {head}");
        for s in &grad.stops {
            let _ = writeln!(self.defs, "      <stop offset=\"{}\" stop-color=\"{}\"/>", num(s.offset), hex(&s.color));
        }
        let close = match grad.kind {
            GradientKind::Linear => "</linearGradient>",
            GradientKind::Radial => "</radialGradient>",
        };
        let _ = writeln!(self.defs, "    {close}");
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Point, Rect};

    fn page() -> Document {
        Document::new(Rect { llx: 0.0, lly: 0.0, urx: 100.0, ury: 50.0 })
    }

    #[test]
    fn cmyk_conversion_matches_ghostscript() {
        let h = |c: [f64; 4]| hex(&Color::Cmyk(c[0], c[1], c[2], c[3]));
        assert_eq!(h([0.0; 4]), "#ffffff");
        assert_eq!(h([1.0, 0.0, 0.0, 0.0]), "#00aeef");
        assert_eq!(h([0.0, 0.0, 1.0, 0.0]), "#fff200");
        // Between grid points the result is interpolated.
        let mid = cmyk_to_srgb([0.0, 0.0, 0.0, 0.0625]);
        let (a, b) = (cmyk_to_srgb([0.0; 4]), cmyk_to_srgb([0.0, 0.0, 0.0, 0.125]));
        assert!((mid[0] - (a[0] + b[0]) / 2.0).abs() < 1e-9);
        assert_eq!(hex(&Color::Gray(0.5)), "#808080");
        assert_eq!(hex(&Color::Rgb(1.0, 0.5, 0.0)), "#ff8000");
    }

    #[test]
    fn closed_path_flipped_and_cleaned() {
        let mut doc = page();
        let mut sp = Subpath::new(Point::new(0.0, 0.0));
        sp.segments.push(Segment::Line(Point::new(10.0, 0.0)));
        sp.segments.push(Segment::Line(Point::new(10.0, 0.0)));
        sp.segments.push(Segment::Line(Point::new(10.0, 10.0)));
        sp.segments.push(Segment::Line(Point::new(0.02, 0.0)));
        sp.closed = true;
        let mut p = PathItem::new(vec![sp]);
        p.fill = Some(Color::Cmyk(0.0, 0.0, 0.0, 1.0));
        doc.elements.push(Element::Path(p));
        let svg = write_svg(&doc);
        assert!(svg.contains("width=\"100pt\" height=\"50pt\" viewBox=\"0 0 100 50\""));
        assert!(svg.contains("<path d=\"M0 50 L10 50 L10 40 L0 50 Z\" fill=\"#231f20\"/>"), "{svg}");
    }

    #[test]
    fn stroke_and_open_path() {
        let mut doc = page();
        let mut sp = Subpath::new(Point::new(0.0, 0.0));
        sp.segments.push(Segment::Line(Point::new(10.0, 0.0)));
        let mut p = PathItem::new(vec![sp]);
        let mut st = Stroke::solid(Color::Spot { name: "CutContour".into(), cmyk: [0.0, 1.0, 0.0, 0.0], tint: 1.0 }, 0.25);
        st.cap = LineCap::Round;
        st.dash = vec![2.0, 1.0];
        p.stroke = Some(st);
        doc.elements.push(Element::Path(p));
        let svg = write_svg(&doc);
        assert!(
            svg.contains("<path d=\"M0 50 L10 50\" fill=\"none\" stroke=\"#ec008c\" stroke-width=\"0.25\" stroke-linecap=\"round\" stroke-miterlimit=\"10\" stroke-dasharray=\"2 1\"/>"),
            "{svg}"
        );
    }

    #[test]
    fn pattern_and_clip_defs() {
        let mut doc = page();
        let mut dot = PathItem::new(vec![{
            let mut s = Subpath::new(Point::new(0.0, 0.0));
            s.segments.push(Segment::Line(Point::new(2.0, 0.0)));
            s.segments.push(Segment::Line(Point::new(2.0, 2.0)));
            s.closed = true;
            s
        }]);
        dot.fill = Some(Color::Rgb(1.0, 0.0, 0.0));
        let pat = Arc::new(Pattern {
            name: "Dots & co".into(),
            bbox: Rect { llx: 0.0, lly: 0.0, urx: 4.0, ury: 4.0 },
            x_step: 4.0,
            y_step: 4.0,
            elements: vec![Element::Path(dot)],
        });
        let square = |s: f64| {
            let mut sp = Subpath::new(Point::new(0.0, 0.0));
            sp.segments.push(Segment::Line(Point::new(s, 0.0)));
            sp.segments.push(Segment::Line(Point::new(s, s)));
            sp.segments.push(Segment::Line(Point::new(0.0, s)));
            sp.closed = true;
            sp
        };
        let mut a = PathItem::new(vec![square(20.0)]);
        a.fill = Some(Color::Pattern(PatternPaint { pattern: pat.clone(), matrix: Matrix::default() }));
        let mut b = a.clone();
        b.even_odd = true;
        doc.elements.push(Element::ClipGroup {
            clip: vec![square(10.0)],
            even_odd: false,
            children: vec![Element::Path(a), Element::Path(b)],
        });
        let svg = write_svg(&doc);
        // One pattern definition for both uses, art in pattern space.
        assert_eq!(svg.matches("<pattern ").count(), 1, "{svg}");
        assert!(svg.contains("data-name=\"Dots &amp; co\" patternUnits=\"userSpaceOnUse\" width=\"4\" height=\"4\" patternTransform=\"matrix(1 0 0 -1 0 50)\">"), "{svg}");
        assert!(svg.contains("<path d=\"M0 0 L2 0 L2 2 L0 0 Z\" fill=\"#ff0000\"/>"), "{svg}");
        assert!(svg.contains("<clipPath id=\"clip1\"><path d=\"M0 50 L10 50 L10 40 L0 40 L0 50 Z\"/></clipPath>"), "{svg}");
        assert!(svg.contains("<g clip-path=\"url(#clip1)\">"));
        assert_eq!(svg.matches("fill=\"url(#pattern2)\"").count(), 2);
        assert!(svg.contains("fill-rule=\"evenodd\""));
    }

    #[test]
    fn radial_gradient() {
        let mut doc = page();
        let mut sp = Subpath::new(Point::new(0.0, 0.0));
        sp.segments.push(Segment::Line(Point::new(10.0, 0.0)));
        sp.segments.push(Segment::Line(Point::new(10.0, 10.0)));
        sp.closed = true;
        let mut p = PathItem::new(vec![sp]);
        p.fill = Some(Color::Gradient(GradientPaint {
            gradient: Arc::new(Gradient {
                kind: GradientKind::Radial,
                stops: vec![
                    crate::ir::GradientStop { offset: 0.0, color: Color::Gray(1.0) },
                    crate::ir::GradientStop { offset: 1.0, color: Color::Rgb(0.0, 0.0, 1.0) },
                ],
            }),
            start: Point::new(5.0, 5.0),
            end: Point::new(5.0, 5.0),
            radius: 5.0,
            matrix: Matrix::default(),
        }));
        doc.elements.push(Element::Path(p));
        let svg = write_svg(&doc);
        assert!(svg.contains("<radialGradient id=\"gradient1\" gradientUnits=\"userSpaceOnUse\" cx=\"5\" cy=\"5\" r=\"5\" fx=\"5\" fy=\"5\" gradientTransform=\"matrix(1 0 0 -1 0 50)\">"), "{svg}");
        assert!(svg.contains("<stop offset=\"1\" stop-color=\"#0000ff\"/>"));
        assert!(svg.contains("fill=\"url(#gradient1)\""));
    }
}
