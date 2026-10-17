//! SVG input via usvg.
//!
//! usvg resolves CSS, `<use>`, shapes, units, `viewBox` and text layout into a
//! simple tree of groups and paths. This module walks that tree, accumulating
//! transforms itself, and maps it onto the IR:
//!
//! * user units (CSS px, 96/in) become points (×0.75) and the y axis is
//!   flipped so the result is in PostScript page space;
//! * quadratic segments are elevated to (exactly equivalent) cubics;
//! * gradients / patterns are approximated by a flat colour, opacity, masks
//!   and filters are dropped, images are skipped – each with a warning;
//! * `clip-path` becomes [`Element::ClipGroup`];
//! * `<text>` is converted to outlines using the system fonts.

use crate::ir::{
    Color, Document, Element, LineCap, LineJoin, Matrix, PathItem, Point, Rect, Segment, Stroke,
    Subpath,
};
use anyhow::{Result, anyhow};
use std::sync::{Arc, OnceLock};
use usvg::tiny_skia_path::PathSegment;

/// CSS px → PostScript points.
const PX_TO_PT: f64 = 72.0 / 96.0;

/// Parse an SVG (or gzip-compressed SVGZ) document into the IR.
/// SVG user units (CSS px, 96/in) are converted to points and the y axis flipped.
pub fn parse_svg(data: &[u8]) -> Result<Document> {
    // usvg is well tested, but a malformed file must never take the whole
    // application down: turn any panic inside the parser into an error.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse_svg_inner(data))) {
        Ok(r) => r,
        Err(_) => Err(anyhow!("internal error while parsing the SVG file (parser panicked)")),
    }
}

fn parse_svg_inner(data: &[u8]) -> Result<Document> {
    if data.iter().all(|b| b.is_ascii_whitespace()) {
        return Err(anyhow!("the SVG file is empty"));
    }
    let bytes: std::borrow::Cow<[u8]> = if data.starts_with(&[0x1f, 0x8b]) {
        usvg::decompress_svgz(data)
            .map(std::borrow::Cow::Owned)
            .map_err(|e| anyhow!("failed to decompress SVGZ data: {e}"))?
    } else {
        std::borrow::Cow::Borrowed(data)
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| anyhow!("SVG data is not valid UTF-8 text: {e}"))?;
    // Some editors write a UTF-8 BOM.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let text_elements = count_text_elements(text);

    let mut opt = usvg::Options::default();
    let mut warnings = Vec::new();
    if text_elements > 0 {
        let db = system_fonts();
        if db.is_empty() {
            warnings.push(
                "no system fonts were found; text in the SVG could not be converted to outlines and was dropped"
                    .to_string(),
            );
        }
        opt.fontdb = db;
    }

    let tree = usvg::Tree::from_str(text, &opt).map_err(|e| anyhow!("failed to parse SVG: {e}"))?;

    let size = tree.size();
    let (w, h) = (size.width() as f64, size.height() as f64);
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err(anyhow!("the SVG has an invalid size ({w} x {h})"));
    }
    let (w_pt, h_pt) = (w * PX_TO_PT, h * PX_TO_PT);
    let page_ts = Matrix::new(PX_TO_PT, 0.0, 0.0, -PX_TO_PT, 0.0, h_pt);

    let mut cv = Converter { warnings, ..Default::default() };
    let root = tree.root();
    let mut elements = Vec::new();
    for child in root.children() {
        // The implicit viewBox wrapper group usvg adds (anonymous, no effects)
        // carries no meaning for the user: inline its children.
        if let usvg::Node::Group(g) = child
            && g.id().is_empty()
            && is_plain_group(g)
        {
            let ts = to_matrix(g.transform()).then(&page_ts);
            cv.convert_children(g, &ts, &mut elements);
            continue;
        }
        cv.convert_node(child, &page_ts, &mut elements);
    }
    cv.finish(text_elements);

    let mut doc = Document::new(Rect { llx: 0.0, lly: 0.0, urx: w_pt, ury: h_pt });
    doc.elements = elements;
    doc.warnings = cv.warnings;
    Ok(doc)
}

/// System fonts, loaded once per process (scanning the font directories takes
/// a noticeable amount of time). Works on macOS, Windows and Linux via fontdb.
fn system_fonts() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        // fontdb's generic family defaults (Times New Roman / Arial / Courier
        // New) exist on both macOS and Windows; usvg falls back to any font
        // containing the glyph otherwise.
        Arc::new(db)
    })
    .clone()
}

/// Number of `<text>` elements with visible content outside `<defs>`/`<symbol>`.
/// Used to decide whether fonts are needed and to detect dropped text.
fn count_text_elements(text: &str) -> usize {
    let opts = usvg::roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let Ok(doc) = usvg::roxmltree::Document::parse_with_options(text, opts) else {
        return 0;
    };
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "text")
        .filter(|n| {
            !n.ancestors()
                .skip(1)
                .any(|a| matches!(a.tag_name().name(), "defs" | "symbol" | "text"))
        })
        .filter(|n| n.descendants().any(|d| d.is_text() && !d.text().unwrap_or("").trim().is_empty()))
        .count()
}

fn is_plain_group(g: &usvg::Group) -> bool {
    g.clip_path().is_none()
        && g.mask().is_none()
        && g.filters().is_empty()
        && g.opacity().get() >= 1.0
}

fn to_matrix(t: usvg::Transform) -> Matrix {
    Matrix::new(t.sx as f64, t.ky as f64, t.kx as f64, t.sy as f64, t.tx as f64, t.ty as f64)
}

#[derive(Default)]
struct Converter {
    warnings: Vec<String>,
    warned_gradient: bool,
    warned_pattern: bool,
    warned_opacity: bool,
    warned_mask: bool,
    warned_filter: bool,
    warned_blend: bool,
    images: usize,
    texts_converted: usize,
}

impl Converter {
    fn warn_once(&mut self, flag: fn(&mut Self) -> &mut bool, msg: &str) {
        let f = flag(self);
        if !*f {
            *f = true;
            self.warnings.push(msg.to_string());
        }
    }

    fn finish(&mut self, text_elements: usize) {
        if self.images > 0 {
            self.warnings.push(format!(
                "{} embedded raster image(s) skipped (images are not supported in AI8 output)",
                self.images
            ));
        }
        if text_elements > self.texts_converted {
            let missing = text_elements - self.texts_converted;
            let msg = format!(
                "{missing} text element(s) could not be converted to outlines (no suitable font found) and were dropped"
            );
            // Avoid duplicating the "no system fonts" warning.
            if !self.warnings.iter().any(|w| w.starts_with("no system fonts")) {
                self.warnings.push(msg);
            }
        }
    }

    fn convert_children(&mut self, g: &usvg::Group, ts: &Matrix, out: &mut Vec<Element>) {
        for child in g.children() {
            self.convert_node(child, ts, out);
        }
    }

    /// Convert one node. `ts` maps the node's (parent) user space to page points.
    fn convert_node(&mut self, node: &usvg::Node, ts: &Matrix, out: &mut Vec<Element>) {
        match node {
            usvg::Node::Group(g) => self.convert_group(g, ts, out),
            usvg::Node::Path(p) => self.convert_path(p, ts, out),
            usvg::Node::Image(_) => self.images += 1,
            usvg::Node::Text(t) => {
                let flat = t.flattened();
                let mut children = Vec::new();
                let fts = to_matrix(flat.transform()).then(ts);
                self.convert_children(flat, &fts, &mut children);
                if !children.is_empty() {
                    self.texts_converted += 1;
                }
                push_grouped(children, out);
            }
        }
    }

    fn convert_group(&mut self, g: &usvg::Group, parent_ts: &Matrix, out: &mut Vec<Element>) {
        let ts = to_matrix(g.transform()).then(parent_ts);
        if g.opacity().get() < 1.0 {
            self.warn_once(
                |c| &mut c.warned_opacity,
                "transparency (opacity) is not supported in AI8 and was ignored",
            );
        }
        if g.blend_mode() != usvg::BlendMode::Normal {
            self.warn_once(|c| &mut c.warned_blend, "blend modes are not supported in AI8 and were ignored");
        }
        if g.mask().is_some() {
            self.warn_once(|c| &mut c.warned_mask, "masks are not supported in AI8 and were ignored");
        }
        if !g.filters().is_empty() {
            self.warn_once(
                |c| &mut c.warned_filter,
                "filter effects (blur, drop shadow, ...) are not supported in AI8 and were ignored",
            );
        }

        let mut children = Vec::new();
        self.convert_children(g, &ts, &mut children);
        if children.is_empty() {
            return;
        }

        match g.clip_path() {
            Some(cp) => {
                // userSpaceOnUse clip paths live in the group's coordinate
                // system (objectBoundingBox is folded into cp.transform()).
                let el = self.clip_group(cp, &ts, children);
                if let Some(el) = el {
                    out.push(el);
                }
            }
            None => push_grouped(children, out),
        }
    }

    /// Wrap `children` in clip groups for `cp` (and the clip path's own nested
    /// clip-path, which intersects – expressed exactly by nesting).
    fn clip_group(&mut self, cp: &usvg::ClipPath, ts: &Matrix, children: Vec<Element>) -> Option<Element> {
        let cts = to_matrix(cp.transform()).then(ts);
        let mut clip = Vec::new();
        let mut rules = Vec::new();
        collect_clip_geometry(cp.root(), &cts, &mut clip, &mut rules);
        if clip.iter().all(|s| s.segments.is_empty()) {
            // An empty clip path hides everything.
            return None;
        }
        let even_odd = !rules.is_empty() && rules.iter().all(|&eo| eo);
        let mut el = Element::ClipGroup { clip, even_odd, children };
        if let Some(inner) = cp.clip_path() {
            el = self.clip_group(inner, ts, vec![el])?;
        }
        Some(el)
    }

    fn convert_path(&mut self, p: &usvg::Path, ts: &Matrix, out: &mut Vec<Element>) {
        if !p.is_visible() {
            return;
        }
        let fill = p.fill().and_then(|f| {
            let c = self.paint_color(f.paint(), f.opacity().get())?;
            Some((c, f.rule() == usvg::FillRule::EvenOdd))
        });
        let stroke = p.stroke().and_then(|s| self.stroke(s, ts));
        if fill.is_none() && stroke.is_none() {
            return;
        }
        let subpaths = convert_path_data(p.data(), ts);
        if subpaths.iter().all(|s| s.segments.is_empty()) {
            return;
        }
        let even_odd = fill.as_ref().is_some_and(|f| f.1);
        let make = |fill: Option<Color>, stroke: Option<Stroke>| {
            let mut item = PathItem::new(subpaths.clone());
            item.fill = fill;
            item.stroke = stroke;
            item.even_odd = even_odd;
            Element::Path(item)
        };
        match (fill, stroke) {
            // paint-order: stroke first → two objects, stroke underneath.
            (Some((f, _)), Some(s)) if p.paint_order() == usvg::PaintOrder::StrokeAndFill => {
                out.push(make(None, Some(s)));
                out.push(make(Some(f), None));
            }
            (f, s) => out.push(make(f.map(|f| f.0), s)),
        }
    }

    fn stroke(&mut self, s: &usvg::Stroke, ts: &Matrix) -> Option<Stroke> {
        let color = self.paint_color(s.paint(), s.opacity().get())?;
        let scale = ts.mean_scale();
        let width = s.width().get() as f64 * scale;
        let cap = match s.linecap() {
            usvg::LineCap::Butt => LineCap::Butt,
            usvg::LineCap::Round => LineCap::Round,
            usvg::LineCap::Square => LineCap::Square,
        };
        let join = match s.linejoin() {
            usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => LineJoin::Miter,
            usvg::LineJoin::Round => LineJoin::Round,
            usvg::LineJoin::Bevel => LineJoin::Bevel,
        };
        let (dash, dash_phase) = match s.dasharray() {
            Some(d) if !d.is_empty() => (
                d.iter().map(|v| *v as f64 * scale).collect(),
                s.dashoffset() as f64 * scale,
            ),
            _ => (Vec::new(), 0.0),
        };
        Some(Stroke { color, width, cap, join, miter_limit: s.miterlimit().get() as f64, dash, dash_phase })
    }

    /// Flat colour for a paint, or `None` if it paints nothing.
    fn paint_color(&mut self, paint: &usvg::Paint, opacity: f32) -> Option<Color> {
        if opacity <= 0.0 {
            return None;
        }
        if opacity < 1.0 {
            self.warn_once(
                |c| &mut c.warned_opacity,
                "transparency (opacity) is not supported in AI8 and was ignored",
            );
        }
        match paint {
            usvg::Paint::Color(c) => Some(rgb(*c)),
            usvg::Paint::LinearGradient(g) => self.gradient(g.stops()),
            usvg::Paint::RadialGradient(g) => self.gradient(g.stops()),
            usvg::Paint::Pattern(_) => {
                self.warn_once(
                    |c| &mut c.warned_pattern,
                    "pattern fills are not supported and were replaced by mid gray",
                );
                Some(Color::Rgb(0.5, 0.5, 0.5))
            }
        }
    }

    fn gradient(&mut self, stops: &[usvg::Stop]) -> Option<Color> {
        self.warn_once(
            |c| &mut c.warned_gradient,
            "gradients are not supported and were replaced by the average of their colours",
        );
        if stops.is_empty() {
            return None;
        }
        let n = stops.len() as f64;
        let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
        for s in stops {
            let Color::Rgb(sr, sg, sb) = rgb(s.color()) else { unreachable!() };
            r += sr;
            g += sg;
            b += sb;
        }
        Some(Color::Rgb(r / n, g / n, b / n))
    }
}

fn rgb(c: usvg::Color) -> Color {
    Color::Rgb(c.red as f64 / 255.0, c.green as f64 / 255.0, c.blue as f64 / 255.0)
}

/// Push converted children: nothing if empty, inline if single, else a group.
fn push_grouped(mut children: Vec<Element>, out: &mut Vec<Element>) {
    match children.len() {
        0 => {}
        1 => out.push(children.pop().expect("len checked")),
        _ => out.push(Element::Group(children)),
    }
}

/// Union of all paths inside a clip path (paint ignored), transformed by `ts`.
/// `rules` receives one entry per path: whether it uses the even-odd clip-rule.
fn collect_clip_geometry(g: &usvg::Group, ts: &Matrix, clip: &mut Vec<Subpath>, rules: &mut Vec<bool>) {
    for node in g.children() {
        match node {
            usvg::Node::Group(cg) => {
                let cts = to_matrix(cg.transform()).then(ts);
                collect_clip_geometry(cg, &cts, clip, rules);
            }
            usvg::Node::Path(p) => {
                if !p.is_visible() {
                    continue;
                }
                let sp = convert_path_data(p.data(), ts);
                if sp.iter().any(|s| !s.segments.is_empty()) {
                    rules.push(p.fill().is_some_and(|f| f.rule() == usvg::FillRule::EvenOdd));
                    clip.extend(sp.into_iter().filter(|s| !s.segments.is_empty()));
                }
            }
            usvg::Node::Text(t) => {
                let flat = t.flattened();
                let fts = to_matrix(flat.transform()).then(ts);
                collect_clip_geometry(flat, &fts, clip, rules);
            }
            usvg::Node::Image(_) => {}
        }
    }
}

/// Convert tiny-skia path data to IR subpaths, transforming every point by `ts`.
/// Quadratic segments are elevated to the exactly equivalent cubic.
fn convert_path_data(path: &usvg::tiny_skia_path::Path, ts: &Matrix) -> Vec<Subpath> {
    let tp = |p: usvg::tiny_skia_path::Point| ts.apply(Point::new(p.x as f64, p.y as f64));
    let mut subpaths: Vec<Subpath> = Vec::new();
    let mut current: Option<Subpath> = None;
    // Current point (page space), for quad elevation and implicit move-tos.
    let mut last = Point::new(0.0, 0.0);
    let flush = |cur: &mut Option<Subpath>, subpaths: &mut Vec<Subpath>| {
        if let Some(sp) = cur.take()
            && !sp.segments.is_empty()
        {
            subpaths.push(sp);
        }
    };
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                flush(&mut current, &mut subpaths);
                last = tp(p);
                current = Some(Subpath::new(last));
            }
            PathSegment::LineTo(p) => {
                let p = tp(p);
                current.get_or_insert_with(|| Subpath::new(last)).segments.push(Segment::Line(p));
                last = p;
            }
            PathSegment::QuadTo(q, p) => {
                let q = tp(q);
                let p = tp(p);
                let p0 = last;
                let c1 = Point::new(p0.x + 2.0 / 3.0 * (q.x - p0.x), p0.y + 2.0 / 3.0 * (q.y - p0.y));
                let c2 = Point::new(p.x + 2.0 / 3.0 * (q.x - p.x), p.y + 2.0 / 3.0 * (q.y - p.y));
                current.get_or_insert_with(|| Subpath::new(p0)).segments.push(Segment::Curve(c1, c2, p));
                last = p;
            }
            PathSegment::CubicTo(a, b, p) => {
                let (a, b, p) = (tp(a), tp(b), tp(p));
                current.get_or_insert_with(|| Subpath::new(last)).segments.push(Segment::Curve(a, b, p));
                last = p;
            }
            PathSegment::Close => {
                if let Some(mut sp) = current.take() {
                    sp.closed = true;
                    last = sp.start;
                    if !sp.segments.is_empty() {
                        subpaths.push(sp);
                    }
                }
            }
        }
    }
    flush(&mut current, &mut subpaths);
    subpaths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt_eq(a: Point, x: f64, y: f64) -> bool {
        (a.x - x).abs() < 1e-4 && (a.y - y).abs() < 1e-4
    }

    #[test]
    fn quad_is_elevated_exactly() {
        let mut pb = usvg::tiny_skia_path::PathBuilder::new();
        pb.move_to(0.0, 0.0);
        pb.quad_to(30.0, 60.0, 90.0, 0.0);
        let path = pb.finish().unwrap();
        let sp = convert_path_data(&path, &Matrix::IDENTITY);
        assert_eq!(sp.len(), 1);
        match &sp[0].segments[0] {
            Segment::Curve(a, b, c) => {
                assert!(pt_eq(*a, 20.0, 40.0), "{a:?}");
                assert!(pt_eq(*b, 50.0, 40.0), "{b:?}");
                assert!(pt_eq(*c, 90.0, 0.0));
            }
            s => panic!("expected curve, got {s:?}"),
        }
    }

    #[test]
    fn close_marks_subpath_and_resets_point() {
        let mut pb = usvg::tiny_skia_path::PathBuilder::new();
        pb.move_to(0.0, 0.0);
        pb.line_to(10.0, 0.0);
        pb.line_to(10.0, 10.0);
        pb.close();
        pb.move_to(20.0, 20.0);
        pb.line_to(30.0, 20.0);
        let path = pb.finish().unwrap();
        let sp = convert_path_data(&path, &Matrix::scale(2.0, 2.0));
        assert_eq!(sp.len(), 2);
        assert!(sp[0].closed);
        assert!(!sp[1].closed);
        assert!(pt_eq(sp[1].start, 40.0, 40.0));
    }

    #[test]
    fn transform_conversion_order() {
        // usvg Transform::from_row(sx, ky, kx, sy, tx, ty)
        let t = usvg::Transform::from_row(1.0, 2.0, 3.0, 4.0, 5.0, 6.0);
        let m = to_matrix(t);
        assert_eq!(m, Matrix::new(1.0, 2.0, 3.0, 4.0, 5.0, 6.0));
        let p = m.apply(Point::new(1.0, 1.0));
        let mut q = usvg::tiny_skia_path::Point::from_xy(1.0, 1.0);
        t.map_point(&mut q);
        assert!(pt_eq(p, q.x as f64, q.y as f64));
    }

    #[test]
    fn counts_text_elements() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><defs><text id="d">x</text></defs>
            <text>Hi<tspan>there</tspan></text><text>  </text><g><text>b</text></g></svg>"#;
        assert_eq!(count_text_elements(svg), 2);
        assert_eq!(count_text_elements("<not xml"), 0);
    }

    #[test]
    fn gradient_average() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <linearGradient id="g"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient>
            <rect width="10" height="10" fill="url(#g)"/></svg>"##;
        let doc = parse_svg(svg).unwrap();
        let Element::Path(p) = &doc.elements[0] else { panic!() };
        assert_eq!(p.fill, Some(Color::Rgb(0.5, 0.0, 0.5)));
        assert_eq!(doc.warnings.iter().filter(|w| w.contains("gradient")).count(), 1);
    }
}
