//! Pattern / gradient helpers shared by the AI8 writer and the "expand" mode.
//!
//! * [`expand_patterns`] replaces every pattern-filled path with a clip group
//!   (clip = the path) containing transformed copies of the tile art, for
//!   cutters / RIPs that do not understand Illustrator pattern swatches.
//! * [`flatten_gradients`] replaces gradient paints by flat colours.
//! * Geometry helpers (affine transforms of IR elements, lattice maths) used by
//!   both the expander and the native pattern writer in [`crate::ai8`].

use crate::ir::{
    Color, Document, Element, LineCap, LineJoin, Matrix, PathItem, Pattern, PatternPaint, Point, Rect, Segment, Stroke,
    Subpath, painted_bounds,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Most tiles generated for one pattern-filled path before falling back to a
/// flat colour.
pub const MAX_TILES_PER_PATH: usize = 20_000;
/// Most tiles generated for the whole document (nested patterns multiply).
pub const MAX_TILES_PER_DOCUMENT: usize = 400_000;
/// Largest stroke outline (segments) used to expand a pattern stroke; more
/// complex strokes (e.g. very long dashed lines) get a flat colour.
pub const MAX_OUTLINE_SEGMENTS: usize = 5_000;
/// Most tiles generated when expanding the patterns nested inside one
/// native pattern swatch (its art is stored once, as 64 KB strings).
pub const MAX_NESTED_TILES_PER_SWATCH: usize = 2_000;
/// Deepest nesting of patterns inside pattern tiles that is expanded.
pub const MAX_PATTERN_DEPTH: u32 = 4;

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// Inverse of an affine matrix, `None` if it is (nearly) singular.
pub fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m.a * m.d - m.b * m.c;
    let scale = m.a.abs().max(m.b.abs()).max(m.c.abs()).max(m.d.abs());
    if !det.is_finite() || scale == 0.0 || det.abs() <= 1e-12 * scale * scale {
        return None;
    }
    let (a, b, c, d) = (m.d / det, -m.b / det, -m.c / det, m.a / det);
    Some(Matrix::new(a, b, c, d, -(m.e * a + m.f * c), -(m.e * b + m.f * d)))
}

pub fn is_finite_matrix(m: &Matrix) -> bool {
    [m.a, m.b, m.c, m.d, m.e, m.f].iter().all(|v| v.is_finite())
}

/// Tight bounds of subpath control points.
pub fn subpaths_bounds(sps: &[Subpath]) -> Option<Rect> {
    let mut r: Option<Rect> = None;
    for p in sps.iter().flat_map(|s| s.points()) {
        let b = r.get_or_insert(Rect { llx: p.x, lly: p.y, urx: p.x, ury: p.y });
        b.llx = b.llx.min(p.x);
        b.lly = b.lly.min(p.y);
        b.urx = b.urx.max(p.x);
        b.ury = b.ury.max(p.y);
    }
    r
}

/// Intersection of two rectangles with positive area.
pub fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let r = Rect { llx: a.llx.max(b.llx), lly: a.lly.max(b.lly), urx: a.urx.min(b.urx), ury: a.ury.min(b.ury) };
    (r.urx - r.llx > 1e-9 && r.ury - r.lly > 1e-9).then_some(r)
}

/// Does `outer` contain `inner` (with a small tolerance)?
pub fn contains(outer: &Rect, inner: &Rect) -> bool {
    let eps = 1e-6 * (1.0 + outer.width().abs().max(outer.height().abs()));
    inner.llx >= outer.llx - eps && inner.lly >= outer.lly - eps && inner.urx <= outer.urx + eps && inner.ury <= outer.ury + eps
}

fn translate_rect(r: &Rect, dx: f64, dy: f64) -> Rect {
    Rect { llx: r.llx + dx, lly: r.lly + dy, urx: r.urx + dx, ury: r.ury + dy }
}

fn corners(r: &Rect) -> [Point; 4] {
    [Point::new(r.llx, r.lly), Point::new(r.urx, r.lly), Point::new(r.urx, r.ury), Point::new(r.llx, r.ury)]
}

/// Bounds of a rectangle after an affine transform.
pub fn transform_rect_bounds(r: &Rect, m: &Matrix) -> Rect {
    let pts = corners(r).map(|p| m.apply(p));
    let mut out = Rect { llx: pts[0].x, lly: pts[0].y, urx: pts[0].x, ury: pts[0].y };
    for p in &pts[1..] {
        out.llx = out.llx.min(p.x);
        out.lly = out.lly.min(p.y);
        out.urx = out.urx.max(p.x);
        out.ury = out.ury.max(p.y);
    }
    out
}

/// A closed subpath along `r` transformed by `m` (a parallelogram).
pub fn rect_subpath(r: &Rect, m: &Matrix) -> Subpath {
    let [a, b, c, d] = corners(r).map(|p| m.apply(p));
    let mut sp = Subpath::new(a);
    sp.segments = vec![Segment::Line(b), Segment::Line(c), Segment::Line(d)];
    sp.closed = true;
    sp
}

/// Does the convex quadrilateral `quad` overlap the axis-aligned `r` with a
/// positive area? (Separating axis theorem.)
pub fn quad_overlaps_rect(quad: &[Point; 4], r: &Rect) -> bool {
    let rc = corners(r);
    let mut axes = vec![Point::new(1.0, 0.0), Point::new(0.0, 1.0)];
    for i in 0..4 {
        let (p, q) = (quad[i], quad[(i + 1) % 4]);
        let n = Point::new(-(q.y - p.y), q.x - p.x);
        if n.x.abs() + n.y.abs() > 1e-12 {
            axes.push(n);
        }
    }
    for ax in axes {
        let len = ax.x.hypot(ax.y);
        let proj = |p: &Point| (p.x * ax.x + p.y * ax.y) / len;
        let (qmin, qmax) = quad.iter().map(proj).fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let (rmin, rmax) = rc.iter().map(proj).fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        if qmax.min(rmax) - qmin.max(rmin) <= 1e-9 {
            return false;
        }
    }
    true
}

pub fn transform_subpaths(sps: &[Subpath], m: &Matrix) -> Vec<Subpath> {
    sps.iter()
        .map(|s| Subpath {
            start: m.apply(s.start),
            segments: s
                .segments
                .iter()
                .map(|seg| match seg {
                    Segment::Line(p) => Segment::Line(m.apply(*p)),
                    Segment::Curve(a, b, c) => Segment::Curve(m.apply(*a), m.apply(*b), m.apply(*c)),
                })
                .collect(),
            closed: s.closed,
        })
        .collect()
}

/// A paint moved together with the geometry it is applied to.
pub fn transform_color(c: &Color, m: &Matrix) -> Color {
    match c {
        Color::Pattern(p) => Color::Pattern(PatternPaint { pattern: p.pattern.clone(), matrix: p.matrix.then(m) }),
        Color::Gradient(g) => {
            let mut g = g.clone();
            g.matrix = g.matrix.then(m);
            Color::Gradient(g)
        }
        c => c.clone(),
    }
}

/// Deep copy of `els` with all geometry (and paint spaces) transformed by `m`.
pub fn transform_elements(els: &[Element], m: &Matrix) -> Vec<Element> {
    let scale = m.mean_scale();
    els.iter()
        .map(|e| match e {
            Element::Path(p) => {
                let mut q = PathItem::new(transform_subpaths(&p.subpaths, m));
                q.fill = p.fill.as_ref().map(|c| transform_color(c, m));
                q.stroke = p.stroke.as_ref().map(|s| {
                    let mut s = s.clone();
                    s.color = transform_color(&s.color, m);
                    s.width *= scale;
                    s.dash.iter_mut().for_each(|d| *d *= scale);
                    s.dash_phase *= scale;
                    s
                });
                q.even_odd = p.even_odd;
                q.fill_overprint = p.fill_overprint;
                q.stroke_overprint = p.stroke_overprint;
                Element::Path(q)
            }
            Element::Group(c) => Element::Group(transform_elements(c, m)),
            Element::ClipGroup { clip, even_odd, children } => Element::ClipGroup {
                clip: transform_subpaths(clip, m),
                even_odd: *even_odd,
                children: transform_elements(children, m),
            },
        })
        .collect()
}

/// Effective repeat distances of a pattern: |XStep| / |YStep|, falling back
/// to the bbox size when a step is zero. `None` for degenerate patterns.
pub fn tile_steps(p: &Pattern) -> Option<(f64, f64)> {
    let pick = |step: f64, size: f64| {
        let s = if step.is_finite() && step.abs() > 1e-9 { step.abs() } else { size.abs() };
        (s.is_finite() && s > 1e-9).then_some(s)
    };
    Some((pick(p.x_step, p.bbox.width())?, pick(p.y_step, p.bbox.height())?))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * (1.0 + a.abs().max(b.abs()))
}

/// Most copies made when re-tiling one pattern cell for the native writer.
const MAX_CELL_COPIES: i64 = 1_000;

/// Tile art for one Illustrator pattern cell `[llx, llx+xs] × [lly, lly+ys]`
/// (pattern space).
///
/// Illustrator tiles the `E` bounding box edge to edge and clips the art at it,
/// so when the PDF-style step equals the bbox size the art is used as is. When
/// they differ, the cell is the step-sized rectangle and the copies of the art
/// from neighbouring lattice positions that reach into it are added (the
/// classic "copy the edge art to the opposite side" construction); if the art
/// itself extends past its own bbox each copy is clipped to its bbox.
pub fn tile_cell_art(p: &Pattern, xs: f64, ys: f64) -> Vec<Element> {
    let bb = p.bbox;
    let Some(art) = painted_bounds(&p.elements) else { return Vec::new() };
    if close(xs, bb.width()) && close(ys, bb.height()) {
        return p.elements.clone();
    }
    let Some(vis) = intersect(&art, &bb) else { return Vec::new() };
    let exceeds = !contains(&bb, &art);
    let cell = Rect { llx: bb.llx, lly: bb.lly, urx: bb.llx + xs, ury: bb.lly + ys };
    let i0 = ((cell.llx - vis.urx) / xs).floor() as i64;
    let i1 = ((cell.urx - vis.llx) / xs).ceil() as i64;
    let j0 = ((cell.lly - vis.ury) / ys).floor() as i64;
    let j1 = ((cell.ury - vis.lly) / ys).ceil() as i64;
    if (i1 - i0 + 1).saturating_mul(j1 - j0 + 1) > MAX_CELL_COPIES {
        return p.elements.clone();
    }
    let mut out = Vec::new();
    for j in j0..=j1 {
        for i in i0..=i1 {
            let (tx, ty) = (i as f64 * xs, j as f64 * ys);
            if intersect(&translate_rect(&vis, tx, ty), &cell).is_none() {
                continue;
            }
            let m = Matrix::translate(tx, ty);
            let els = if i == 0 && j == 0 { p.elements.clone() } else { transform_elements(&p.elements, &m) };
            if exceeds {
                out.push(Element::ClipGroup { clip: vec![rect_subpath(&bb, &m)], even_odd: false, children: els });
            } else {
                out.extend(els);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Expand mode
// ---------------------------------------------------------------------------

struct Ctx {
    warnings: Vec<String>,
    total_tiles: usize,
    /// Expand pattern strokes inside their outline (expand mode); otherwise
    /// they are flattened (inside native pattern tiles, whose layer strings
    /// are limited to 64 KB and cannot hold large outlines).
    outline_strokes: bool,
    /// Tile budget for the whole expansion.
    max_tiles: usize,
}

impl Ctx {
    fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }
}

/// Replace every pattern-filled path by a clip group (clip = the path's
/// subpaths and fill rule) holding transformed copies of the tile art covering
/// the path, followed by the path's stroke as a separate path. Pattern strokes
/// become flat colours. Gradients are left alone.
pub fn expand_patterns(doc: &mut Document) {
    let mut ctx = Ctx { warnings: Vec::new(), total_tiles: 0, outline_strokes: true, max_tiles: MAX_TILES_PER_DOCUMENT };
    let els = std::mem::take(&mut doc.elements);
    doc.elements = expand_list(els, 0, &mut ctx);
    for w in ctx.warnings {
        if !doc.warnings.contains(&w) {
            doc.warnings.push(w);
        }
    }
}

/// Expand the pattern fills inside pattern tile art (patterns nested in
/// patterns) into clipped copies of their tiles, as [`expand_patterns`] does
/// for a document; pattern strokes are flattened. Used by the native writer,
/// which keeps the outer pattern as a swatch. Returns the art and any
/// warnings.
pub fn expand_nested(els: Vec<Element>) -> (Vec<Element>, Vec<String>) {
    let mut ctx = Ctx { warnings: Vec::new(), total_tiles: 0, outline_strokes: false, max_tiles: MAX_NESTED_TILES_PER_SWATCH };
    let out = expand_list(els, 1, &mut ctx);
    (out, ctx.warnings)
}

fn expand_list(els: Vec<Element>, depth: u32, ctx: &mut Ctx) -> Vec<Element> {
    let mut out = Vec::with_capacity(els.len());
    for e in els {
        expand_into(e, depth, ctx, &mut out);
    }
    out
}

fn expand_into(e: Element, depth: u32, ctx: &mut Ctx, out: &mut Vec<Element>) {
    match e {
        Element::Group(c) => out.push(Element::Group(expand_list(c, depth, ctx))),
        Element::ClipGroup { clip, even_odd, children } => {
            out.push(Element::ClipGroup { clip, even_odd, children: expand_list(children, depth, ctx) })
        }
        Element::Path(mut p) => {
            // A pattern stroke is expanded as a fill of the stroke's outline,
            // painted after the path's fill.
            let mut stroke_fill = None;
            if let Some(s) = p.stroke.as_ref()
                && let Color::Pattern(pp) = &s.color
            {
                let outline = if ctx.outline_strokes { stroke_outline(&p.subpaths, s) } else { None };
                let too_complex = outline
                    .as_ref()
                    .is_some_and(|o| o.iter().map(|sp| sp.segments.len()).sum::<usize>() > MAX_OUTLINE_SEGMENTS);
                if too_complex {
                    ctx.warn(format!(
                        "a pattern stroke was too complex to expand (more than {MAX_OUTLINE_SEGMENTS} outline segments); a flat colour was used instead"
                    ));
                }
                match outline.filter(|_| !too_complex) {
                    Some(outline) => {
                        let mut sp = PathItem::new(outline);
                        sp.fill = Some(Color::Pattern(pp.clone()));
                        sp.fill_overprint = p.stroke_overprint;
                        stroke_fill = Some(sp);
                        p.stroke = None;
                    }
                    None => {
                        if let Some(s) = p.stroke.as_mut() {
                            s.color = s.color.flatten();
                        }
                    }
                }
            }
            match p.fill.clone() {
                Some(Color::Pattern(pp)) => match expand_fill(&p, &pp, depth, ctx) {
                    Some(tiles) => {
                        if !tiles.is_empty() {
                            out.push(Element::ClipGroup { clip: p.subpaths.clone(), even_odd: p.even_odd, children: tiles });
                        }
                        if p.stroke.is_some() {
                            p.fill = None;
                            out.push(Element::Path(p));
                        }
                    }
                    None => {
                        p.fill = Some(Color::Pattern(pp).flatten());
                        out.push(Element::Path(p));
                    }
                },
                _ => {
                    if p.fill.is_some() || p.stroke.is_some() || stroke_fill.is_none() {
                        out.push(Element::Path(p));
                    }
                }
            }
            if let Some(sp) = stroke_fill {
                expand_into(Element::Path(sp), depth, ctx, out);
            }
        }
    }
}

/// The outline of a stroke (dashes, caps and joins applied) as closed
/// subpaths to be filled with the non-zero rule. `None` for hairlines or
/// when the stroker fails.
pub fn stroke_outline(subpaths: &[Subpath], s: &Stroke) -> Option<Vec<Subpath>> {
    use usvg::tiny_skia_path as tsp;
    if !(s.width.is_finite() && s.width > 0.0) {
        return None;
    }
    let mut pb = tsp::PathBuilder::new();
    let f = |v: f64| v as f32;
    for sp in subpaths {
        pb.move_to(f(sp.start.x), f(sp.start.y));
        for seg in &sp.segments {
            match seg {
                Segment::Line(p) => pb.line_to(f(p.x), f(p.y)),
                Segment::Curve(a, b, c) => pb.cubic_to(f(a.x), f(a.y), f(b.x), f(b.y), f(c.x), f(c.y)),
            }
        }
        if sp.closed {
            pb.close();
        }
    }
    let mut path = pb.finish()?;
    // Curve flattening tolerance: a quarter of a point is invisible at any
    // sensible size; scale it with the stroke so thin strokes stay smooth.
    let res = (4.0 / s.width.min(4.0)).max(1.0) as f32 * 4.0;
    if !s.dash.is_empty() && s.dash.iter().any(|d| *d > 0.0) {
        let dash = tsp::StrokeDash::new(s.dash.iter().map(|d| f(*d)).collect(), f(s.dash_phase))?;
        path = path.dash(&dash, res)?;
    }
    let stroke = tsp::Stroke {
        width: f(s.width),
        miter_limit: f(s.miter_limit.max(1.0)),
        line_cap: match s.cap {
            LineCap::Butt => tsp::LineCap::Butt,
            LineCap::Round => tsp::LineCap::Round,
            LineCap::Square => tsp::LineCap::Square,
        },
        line_join: match s.join {
            LineJoin::Miter => tsp::LineJoin::Miter,
            LineJoin::Round => tsp::LineJoin::Round,
            LineJoin::Bevel => tsp::LineJoin::Bevel,
        },
        dash: None,
    };
    let outline = path.stroke(&stroke, res)?;
    let sps = crate::svg::convert_path_data(&outline, &Matrix::IDENTITY);
    (!sps.is_empty()).then_some(sps)
}

/// Tiles for one pattern-filled path; `None` = fall back to a flat colour.
fn expand_fill(p: &PathItem, pp: &PatternPaint, depth: u32, ctx: &mut Ctx) -> Option<Vec<Element>> {
    if depth >= MAX_PATTERN_DEPTH {
        ctx.warn(format!("patterns nested more than {MAX_PATTERN_DEPTH} deep were replaced by a flat colour"));
        return None;
    }
    let pat = &pp.pattern;
    let Some((xs, ys)) = tile_steps(pat) else {
        ctx.warn(format!("pattern \"{}\" has an empty tile; replaced by a flat colour", pat.name));
        return None;
    };
    if !is_finite_matrix(&pp.matrix) {
        return None;
    }
    let Some(inv) = invert(&pp.matrix) else {
        // Collapsed pattern space: nothing sensible to tile.
        return None;
    };
    let Some(bounds) = subpaths_bounds(&p.subpaths) else { return Some(Vec::new()) };
    let Some(art) = painted_bounds(&pat.elements) else { return Some(Vec::new()) };
    let Some(vis) = intersect(&art, &pat.bbox) else { return Some(Vec::new()) };
    let needs_clip = !contains(&pat.bbox, &art);

    // Candidate lattice indices from the path bounds mapped into pattern space.
    let pb = transform_rect_bounds(&bounds, &inv);
    let i0 = ((pb.llx - vis.urx) / xs).floor();
    let i1 = ((pb.urx - vis.llx) / xs).ceil();
    let j0 = ((pb.lly - vis.ury) / ys).floor();
    let j1 = ((pb.ury - vis.lly) / ys).ceil();
    let candidates = (i1 - i0 + 1.0) * (j1 - j0 + 1.0);
    let too_many = |ctx: &mut Ctx| {
        ctx.warn(format!(
            "pattern \"{}\" would need more than {MAX_TILES_PER_PATH} tiles to fill a path; a flat colour was used instead",
            pat.name
        ));
        None
    };
    if !candidates.is_finite() || candidates > (MAX_TILES_PER_PATH as f64) * 8.0 {
        return too_many(ctx);
    }
    let (i0, i1, j0, j1) = (i0 as i64, i1 as i64, j0 as i64, j1 as i64);

    let mut cells = Vec::new();
    for j in j0..=j1 {
        for i in i0..=i1 {
            let (tx, ty) = (i as f64 * xs, j as f64 * ys);
            let m = Matrix::translate(tx, ty).then(&pp.matrix);
            let quad = corners(&translate_rect(&vis, tx, ty)).map(|c| pp.matrix.apply(c));
            if quad_overlaps_rect(&quad, &bounds) {
                cells.push(m);
                if cells.len() > MAX_TILES_PER_PATH {
                    return too_many(ctx);
                }
            }
        }
    }
    if ctx.total_tiles + cells.len() > ctx.max_tiles {
        ctx.warn(format!(
            "pattern expansion exceeded {} tiles; remaining pattern fills use flat colours",
            ctx.max_tiles
        ));
        return None;
    }
    ctx.total_tiles += cells.len();

    let mut tiles = Vec::with_capacity(cells.len());
    for m in cells {
        let art = expand_list(transform_elements(&pat.elements, &m), depth + 1, ctx);
        if art.is_empty() {
            continue;
        }
        if needs_clip {
            tiles.push(Element::ClipGroup { clip: vec![rect_subpath(&pat.bbox, &m)], even_odd: false, children: art });
        } else if art.len() == 1 {
            tiles.extend(art);
        } else {
            tiles.push(Element::Group(art));
        }
    }
    Some(tiles)
}

// ---------------------------------------------------------------------------
// Gradient flattening
// ---------------------------------------------------------------------------

/// Replace every gradient paint (also inside pattern tiles) by its flat
/// approximation. Pattern definitions stay shared (one rewritten copy per
/// original definition).
pub fn flatten_gradients(doc: &mut Document) {
    let mut memo: HashMap<*const Pattern, Arc<Pattern>> = HashMap::new();
    flatten_list(&mut doc.elements, &mut memo, 0);
}

fn flatten_list(els: &mut [Element], memo: &mut HashMap<*const Pattern, Arc<Pattern>>, depth: u32) {
    for e in els {
        match e {
            Element::Path(p) => {
                if let Some(f) = p.fill.as_mut() {
                    flatten_paint(f, memo, depth);
                }
                if let Some(s) = p.stroke.as_mut() {
                    flatten_paint(&mut s.color, memo, depth);
                }
            }
            Element::Group(c) => flatten_list(c, memo, depth),
            Element::ClipGroup { children, .. } => flatten_list(children, memo, depth),
        }
    }
}

fn flatten_paint(c: &mut Color, memo: &mut HashMap<*const Pattern, Arc<Pattern>>, depth: u32) {
    match c {
        Color::Gradient(_) => *c = c.flatten(),
        Color::Pattern(pp) if depth < MAX_PATTERN_DEPTH => {
            let key = Arc::as_ptr(&pp.pattern);
            if let Some(done) = memo.get(&key) {
                pp.pattern = done.clone();
                return;
            }
            let mut pat = (*pp.pattern).clone();
            flatten_list(&mut pat.elements, memo, depth + 1);
            let new = Arc::new(pat);
            memo.insert(key, new.clone());
            pp.pattern = new;
        }
        Color::Pattern(_) => *c = c.flatten(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{GradientKind, GradientPaint, GradientStop, Gradient, Stroke};

    fn square(x: f64, y: f64, s: f64) -> Subpath {
        rect_subpath(&Rect { llx: x, lly: y, urx: x + s, ury: y + s }, &Matrix::IDENTITY)
    }

    fn tile_pattern(size: f64, step: f64) -> Arc<Pattern> {
        let mut dot = PathItem::new(vec![square(0.0, 0.0, size)]);
        dot.fill = Some(Color::Cmyk(1.0, 0.0, 0.0, 0.0));
        Arc::new(Pattern {
            name: "Tile".into(),
            bbox: Rect { llx: 0.0, lly: 0.0, urx: size, ury: size },
            x_step: step,
            y_step: step,
            elements: vec![Element::Path(dot)],
        })
    }

    fn filled(sp: Subpath, pattern: Arc<Pattern>, m: Matrix) -> Document {
        let mut p = PathItem::new(vec![sp]);
        p.fill = Some(Color::Pattern(PatternPaint { pattern, matrix: m }));
        let mut d = Document::new(Rect { llx: 0.0, lly: 0.0, urx: 500.0, ury: 500.0 });
        d.elements.push(Element::Path(p));
        d
    }

    fn tiles(d: &Document) -> &Vec<Element> {
        match &d.elements[0] {
            Element::ClipGroup { children, .. } => children,
            e => panic!("expected clip group, got {e:?}"),
        }
    }

    #[test]
    fn invert_roundtrip() {
        let m = Matrix::new(2.0, 1.0, -0.5, 3.0, 10.0, -4.0);
        let i = invert(&m).unwrap();
        let p = Point::new(3.0, 7.0);
        let q = i.apply(m.apply(p));
        assert!((q.x - p.x).abs() < 1e-9 && (q.y - p.y).abs() < 1e-9);
        assert!(invert(&Matrix::new(1.0, 2.0, 2.0, 4.0, 0.0, 0.0)).is_none());
    }

    #[test]
    fn ten_by_ten_tiles_over_95_square() {
        let mut d = filled(square(0.0, 0.0, 95.0), tile_pattern(10.0, 10.0), Matrix::IDENTITY);
        expand_patterns(&mut d);
        let t = tiles(&d);
        assert_eq!(t.len(), 100);
        // Tile (9,9) is the last one, at 90..100.
        let Element::Path(last) = t.last().unwrap() else { panic!() };
        assert_eq!(last.subpaths[0].start, Point::new(90.0, 90.0));
        assert!(d.warnings.is_empty());
    }

    #[test]
    fn stroke_kept_after_clip_group() {
        let mut d = filled(square(0.0, 0.0, 20.0), tile_pattern(10.0, 10.0), Matrix::IDENTITY);
        if let Element::Path(p) = &mut d.elements[0] {
            p.stroke = Some(Stroke::solid(Color::Gray(0.0), 1.0));
        }
        expand_patterns(&mut d);
        assert_eq!(d.elements.len(), 2);
        let Element::Path(s) = &d.elements[1] else { panic!() };
        assert!(s.fill.is_none() && s.stroke.is_some());
    }

    #[test]
    fn rotated_pattern_covers_corners() {
        let m = Matrix::new(0.6, 0.8, -0.8, 0.6, 3.0, 7.0);
        let mut d = filled(square(0.0, 0.0, 100.0), tile_pattern(10.0, 10.0), m);
        expand_patterns(&mut d);
        let t = tiles(&d);
        // Every corner (and the centre) of the square lies in some tile.
        let inv_tiles: Vec<Rect> = t
            .iter()
            .map(|e| {
                let Element::Path(p) = e else { panic!() };
                let pts: Vec<Point> = p.subpaths[0].points().map(|q| invert(&m).unwrap().apply(q)).collect();
                Rect {
                    llx: pts.iter().map(|p| p.x).fold(f64::MAX, f64::min),
                    lly: pts.iter().map(|p| p.y).fold(f64::MAX, f64::min),
                    urx: pts.iter().map(|p| p.x).fold(f64::MIN, f64::max),
                    ury: pts.iter().map(|p| p.y).fold(f64::MIN, f64::max),
                }
            })
            .collect();
        for c in [Point::new(0.1, 0.1), Point::new(99.9, 0.1), Point::new(99.9, 99.9), Point::new(0.1, 99.9), Point::new(50.0, 50.0)] {
            let q = invert(&m).unwrap().apply(c);
            assert!(
                inv_tiles.iter().any(|r| q.x >= r.llx - 1e-9 && q.x <= r.urx + 1e-9 && q.y >= r.lly - 1e-9 && q.y <= r.ury + 1e-9),
                "corner {c:?} uncovered"
            );
        }
        // Fewer tiles than the axis-aligned bound of the rotated square would give.
        assert!(t.len() < 15 * 15, "{}", t.len());
    }

    #[test]
    fn tile_cap_falls_back_to_flat_colour() {
        let mut d = filled(square(0.0, 0.0, 2000.0), tile_pattern(1.0, 1.0), Matrix::IDENTITY);
        expand_patterns(&mut d);
        let Element::Path(p) = &d.elements[0] else { panic!() };
        assert_eq!(p.fill, Some(Color::Cmyk(1.0, 0.0, 0.0, 0.0)));
        assert_eq!(d.warnings.len(), 1);
        assert!(d.warnings[0].contains("20000"));
    }

    #[test]
    fn art_exceeding_bbox_is_clipped_per_tile() {
        let mut big = PathItem::new(vec![square(-5.0, -5.0, 20.0)]);
        big.fill = Some(Color::Gray(0.0));
        let pat = Arc::new(Pattern {
            name: "Over".into(),
            bbox: Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 },
            x_step: 10.0,
            y_step: 10.0,
            elements: vec![Element::Path(big)],
        });
        let mut d = filled(square(0.0, 0.0, 20.0), pat, Matrix::IDENTITY);
        expand_patterns(&mut d);
        let t = tiles(&d);
        assert_eq!(t.len(), 4);
        assert!(t.iter().all(|e| matches!(e, Element::ClipGroup { .. })));
    }

    #[test]
    fn nested_patterns_expand_recursively() {
        let inner = tile_pattern(2.0, 2.0);
        let mut cell = PathItem::new(vec![square(0.0, 0.0, 10.0)]);
        cell.fill = Some(Color::Pattern(PatternPaint { pattern: inner, matrix: Matrix::IDENTITY }));
        let outer = Arc::new(Pattern {
            name: "Outer".into(),
            bbox: Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 },
            x_step: 10.0,
            y_step: 10.0,
            elements: vec![Element::Path(cell)],
        });
        let mut d = filled(square(0.0, 0.0, 20.0), outer, Matrix::IDENTITY);
        expand_patterns(&mut d);
        assert_eq!(d.path_count(), 4 * 25);
        fn no_patterns(els: &[Element]) -> bool {
            els.iter().all(|e| match e {
                Element::Path(p) => p.fill.as_ref().is_none_or(|c| c.is_solid()),
                Element::Group(c) => no_patterns(c),
                Element::ClipGroup { children, .. } => no_patterns(children),
            })
        }
        assert!(no_patterns(&d.elements));
    }

    #[test]
    fn gap_steps_retile_cell() {
        // 5x5 art in a 5x5 bbox repeated every 10: the cell is 10x10 with
        // only the original copy.
        let p = tile_pattern(5.0, 10.0);
        assert_eq!(tile_cell_art(&p, 10.0, 10.0).len(), 1);
        // Overlapping: 10x10 art repeated every 6 → copies wrap into the cell.
        let p = tile_pattern(10.0, 6.0);
        let cell = tile_cell_art(&p, 6.0, 6.0);
        assert_eq!(cell.len(), 4);
    }

    #[test]
    fn flatten_gradients_everywhere() {
        let g = Arc::new(Gradient {
            kind: GradientKind::Linear,
            stops: vec![
                GradientStop { offset: 0.0, color: Color::Gray(0.0) },
                GradientStop { offset: 1.0, color: Color::Gray(1.0) },
            ],
        });
        let gp = Color::Gradient(GradientPaint {
            gradient: g,
            start: Point::new(0.0, 0.0),
            end: Point::new(1.0, 0.0),
            radius: 0.0,
            matrix: Matrix::IDENTITY,
        });
        let mut inner = PathItem::new(vec![square(0.0, 0.0, 5.0)]);
        inner.fill = Some(gp.clone());
        let pat = Arc::new(Pattern {
            name: "G".into(),
            bbox: Rect { llx: 0.0, lly: 0.0, urx: 5.0, ury: 5.0 },
            x_step: 5.0,
            y_step: 5.0,
            elements: vec![Element::Path(inner)],
        });
        let pp = Color::Pattern(PatternPaint { pattern: pat, matrix: Matrix::IDENTITY });
        let mut a = PathItem::new(vec![square(0.0, 0.0, 5.0)]);
        a.fill = Some(gp);
        let mut b = PathItem::new(vec![square(0.0, 0.0, 5.0)]);
        b.fill = Some(pp.clone());
        let mut c = PathItem::new(vec![square(0.0, 0.0, 5.0)]);
        c.fill = Some(pp);
        let mut d = Document::new(Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 });
        d.elements = vec![Element::Path(a), Element::Path(b), Element::Path(c)];
        flatten_gradients(&mut d);
        let Element::Path(a) = &d.elements[0] else { panic!() };
        assert_eq!(a.fill, Some(Color::Gray(0.5)));
        let pats: Vec<Arc<Pattern>> = d.elements[1..]
            .iter()
            .map(|e| match e {
                Element::Path(PathItem { fill: Some(Color::Pattern(p)), .. }) => p.pattern.clone(),
                _ => panic!(),
            })
            .collect();
        assert!(Arc::ptr_eq(&pats[0], &pats[1]), "shared pattern stays shared");
        let Element::Path(t) = &pats[0].elements[0] else { panic!() };
        assert!(t.fill.as_ref().unwrap().is_solid());
    }
}
