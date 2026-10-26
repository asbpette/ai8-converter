//! Integration tests for the SVG parser (src/svg.rs).

mod common;

use ai8_converter::ai8::{WriteOptions, write_ai8};
use ai8_converter::ir::{
    Color, Document, Element, GradientKind, GradientPaint, LineCap, LineJoin, Matrix, PathItem, PatternPaint, Point,
    Segment, Subpath,
};
use std::sync::Arc;
use ai8_converter::svg::parse_svg;

const NS: &str = r##"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""##;

fn svg(attrs: &str, body: &str) -> Vec<u8> {
    format!(r##"<svg {NS} {attrs}>{body}</svg>"##).into_bytes()
}

fn parse(attrs: &str, body: &str) -> Document {
    parse_svg(&svg(attrs, body)).expect("SVG should parse")
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

fn assert_pt(p: Point, x: f64, y: f64) {
    assert!(close(p.x, x) && close(p.y, y), "expected ({x}, {y}), got ({}, {})", p.x, p.y);
}

/// All paths in document order (flattening groups and clip groups).
fn paths(doc: &Document) -> Vec<&PathItem> {
    fn walk<'a>(els: &'a [Element], out: &mut Vec<&'a PathItem>) {
        for e in els {
            match e {
                Element::Path(p) => out.push(p),
                Element::Group(c) => walk(c, out),
                Element::ClipGroup { children, .. } => walk(children, out),
            }
        }
    }
    let mut v = Vec::new();
    walk(&doc.elements, &mut v);
    v
}

fn only_path(doc: &Document) -> &PathItem {
    let p = paths(doc);
    assert_eq!(p.len(), 1, "expected exactly one path, got {:#?}", doc.elements);
    p[0]
}

/// End points (move-to + segment end points) of a subpath.
fn anchors(sp: &Subpath) -> Vec<Point> {
    std::iter::once(sp.start)
        .chain(sp.segments.iter().map(|s| match s {
            Segment::Line(p) => *p,
            Segment::Curve(_, _, p) => *p,
        }))
        .collect()
}

fn has_warning(doc: &Document, needle: &str) -> bool {
    doc.warnings.iter().any(|w| w.to_lowercase().contains(needle))
}

fn rgb(doc_color: &Option<Color>) -> (f64, f64, f64) {
    match doc_color {
        Some(Color::Rgb(r, g, b)) => (*r, *g, *b),
        other => panic!("expected RGB colour, got {other:?}"),
    }
}

// ---------------------------------------------------------------- geometry

#[test]
fn rect_px_to_pt_and_y_flip() {
    let doc = parse(r##"width="200" height="100""##, r##"<rect x="10" y="20" width="40" height="30" fill="red"/>"##);
    assert!(close(doc.page.llx, 0.0) && close(doc.page.lly, 0.0));
    assert!(close(doc.page.urx, 150.0) && close(doc.page.ury, 75.0), "{:?}", doc.page);
    let p = only_path(&doc);
    assert_eq!(p.subpaths.len(), 1);
    let sp = &p.subpaths[0];
    assert!(sp.closed);
    assert!(sp.segments.iter().all(|s| matches!(s, Segment::Line(_))));
    let pts = anchors(sp);
    assert_pt(pts[0], 7.5, 60.0);
    assert_pt(pts[1], 37.5, 60.0);
    assert_pt(pts[2], 37.5, 37.5);
    assert_pt(pts[3], 7.5, 37.5);
    assert_eq!(p.fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    assert!(p.stroke.is_none());
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

#[test]
fn physical_units_size() {
    let doc = parse(r##"width="1in" height="2in" viewBox="0 0 10 20""##, r##"<rect width="10" height="20"/>"##);
    assert!(close(doc.page.urx, 72.0) && close(doc.page.ury, 144.0), "{:?}", doc.page);
    let b = doc.content_bounds().unwrap();
    assert!(close(b.llx, 0.0) && close(b.lly, 0.0) && close(b.urx, 72.0) && close(b.ury, 144.0), "{b:?}");
}

#[test]
fn circle_uses_curves() {
    let doc = parse(r##"width="100" height="100""##, r##"<circle cx="50" cy="50" r="40" fill="blue"/>"##);
    let p = only_path(&doc);
    let sp = &p.subpaths[0];
    assert!(sp.closed);
    assert!(sp.segments.iter().filter(|s| matches!(s, Segment::Curve(..))).count() >= 4);
    let b = doc.content_bounds().unwrap();
    // 10..90 px => 7.5..67.5 pt both axes (control points of a circle stay on the bbox).
    assert!(close(b.llx, 7.5) && close(b.urx, 67.5) && close(b.lly, 7.5) && close(b.ury, 67.5), "{b:?}");
}

#[test]
fn quadratic_elevated_to_exact_cubic() {
    // Page height 100 px = 75 pt; y' = 75 - 0.75 y.
    let doc = parse(r##"width="100" height="100""##, r##"<path d="M0 0 Q30 60 90 0" fill="none" stroke="black"/>"##);
    let p = only_path(&doc);
    let sp = &p.subpaths[0];
    assert_pt(sp.start, 0.0, 75.0);
    assert_eq!(sp.segments.len(), 1);
    match &sp.segments[0] {
        Segment::Curve(c1, c2, end) => {
            // c1 = p0 + 2/3 (q - p0) = (20, 40) px; c2 = p + 2/3 (q - p) = (50, 40) px.
            assert_pt(*c1, 15.0, 45.0);
            assert_pt(*c2, 37.5, 45.0);
            assert_pt(*end, 67.5, 75.0);
        }
        s => panic!("expected cubic, got {s:?}"),
    }
}

#[test]
fn smooth_quadratic_and_cubic() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<path d="M0 0 Q10 10 20 0 T40 0 C50 10 60 10 70 0" fill="none" stroke="black"/>"##,
    );
    let sp = &only_path(&doc).subpaths[0];
    assert_eq!(sp.segments.len(), 3);
    // T reflects the control point to (30,-10): c1 = (20,0)+2/3((30,-10)-(20,0)) = (26.667,-6.667)
    match &sp.segments[1] {
        Segment::Curve(c1, _, end) => {
            assert_pt(*c1, 26.6667 * 0.75, 75.0 + 6.6667 * 0.75);
            assert_pt(*end, 30.0, 75.0);
        }
        s => panic!("{s:?}"),
    }
    match &sp.segments[2] {
        Segment::Curve(c1, c2, end) => {
            assert_pt(*c1, 37.5, 67.5);
            assert_pt(*c2, 45.0, 67.5);
            assert_pt(*end, 52.5, 75.0);
        }
        s => panic!("{s:?}"),
    }
}

#[test]
fn nested_transforms() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<g transform="translate(10,10)"><g transform="scale(2)"><rect width="5" height="5"/></g></g>
           <g transform="translate(50 50)"><path transform="rotate(90)" d="M10 0 L20 0" stroke="black"/></g>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 2);
    let b = anchors(&ps[0].subpaths[0]);
    assert_pt(b[0], 7.5, 67.5);
    assert_pt(b[2], 15.0, 60.0);
    let l = anchors(&ps[1].subpaths[0]);
    // rotate(90): (10,0) -> (0,10); + (50,50) => (50,60) px
    assert_pt(l[0], 37.5, 30.0);
    assert_pt(l[1], 37.5, 22.5);
}

#[test]
fn viewbox_scaling() {
    let doc = parse(r##"width="100" height="50" viewBox="0 0 10 5""##, r##"<rect x="1" y="1" width="2" height="2"/>"##);
    assert!(close(doc.page.urx, 75.0) && close(doc.page.ury, 37.5));
    let pts = anchors(&only_path(&doc).subpaths[0]);
    // viewBox scale 10: (1,1) -> (10,10) px -> (7.5, 37.5-7.5)
    assert_pt(pts[0], 7.5, 30.0);
    assert_pt(pts[2], 22.5, 15.0);
}

#[test]
fn viewbox_only_and_offset_origin() {
    let doc = parse(r##"viewBox="100 100 40 20""##, r##"<rect x="100" y="100" width="40" height="20"/>"##);
    assert!(close(doc.page.urx, 30.0) && close(doc.page.ury, 15.0));
    let b = doc.content_bounds().unwrap();
    assert!(close(b.llx, 0.0) && close(b.lly, 0.0) && close(b.urx, 30.0) && close(b.ury, 15.0), "{b:?}");
}

// ---------------------------------------------------------------- paint

#[test]
fn fill_rules() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<path d="M0 0H50V50H0Z M10 10H40V40H10Z" fill-rule="evenodd"/>
           <path d="M0 0H50V50H0Z M10 10H40V40H10Z"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 2);
    assert!(ps[0].even_odd);
    assert_eq!(ps[0].subpaths.len(), 2);
    assert!(!ps[1].even_odd);
    // Default fill is black.
    assert_eq!(ps[1].fill, Some(Color::Rgb(0.0, 0.0, 0.0)));
}

#[test]
fn named_and_hex_colours() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<rect width="1" height="1" fill="navy"/>
           <rect width="1" height="1" fill="#0f0"/>
           <rect width="1" height="1" fill="#336699"/>
           <rect width="1" height="1" style="fill: rgb(255, 128, 0)"/>
           <rect width="1" height="1" fill="none" stroke="cornflowerblue"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 5);
    assert_eq!(ps[0].fill, Some(Color::Rgb(0.0, 0.0, 128.0 / 255.0)));
    assert_eq!(ps[1].fill, Some(Color::Rgb(0.0, 1.0, 0.0)));
    assert_eq!(ps[2].fill, Some(Color::Rgb(0.2, 0.4, 0.6)));
    let (r, g, b) = rgb(&ps[3].fill);
    assert!(close(r, 1.0) && close(g, 128.0 / 255.0) && close(b, 0.0));
    assert!(ps[4].fill.is_none());
    assert_eq!(ps[4].stroke.as_ref().unwrap().color, Color::Rgb(100.0 / 255.0, 149.0 / 255.0, 237.0 / 255.0));
}

#[test]
fn strokes_dash_caps_joins() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<g transform="scale(2)"><path d="M0 0 L10 10 L20 0" fill="none" stroke="#ff0000"
              stroke-width="4" stroke-linecap="round" stroke-linejoin="bevel" stroke-miterlimit="7"
              stroke-dasharray="2 4" stroke-dashoffset="1"/></g>
           <path d="M0 0 L10 10" fill="none" stroke="black" stroke-linecap="square" stroke-linejoin="round"/>
           <path d="M0 0 L10 10" fill="none" stroke="black" stroke-linejoin="miter-clip"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 3);
    let s = ps[0].stroke.as_ref().unwrap();
    assert!(ps[0].fill.is_none());
    assert_eq!(s.color, Color::Rgb(1.0, 0.0, 0.0));
    assert!(close(s.width, 6.0), "width {}", s.width); // 4 * 2 * 0.75
    assert_eq!(s.cap, LineCap::Round);
    assert_eq!(s.join, LineJoin::Bevel);
    assert!(close(s.miter_limit, 7.0));
    assert_eq!(s.dash.len(), 2);
    assert!(close(s.dash[0], 3.0) && close(s.dash[1], 6.0), "{:?}", s.dash);
    assert!(close(s.dash_phase, 1.5));
    assert!(!ps[0].subpaths[0].closed);

    let s = ps[1].stroke.as_ref().unwrap();
    assert!(close(s.width, 0.75));
    assert_eq!(s.cap, LineCap::Square);
    assert_eq!(s.join, LineJoin::Round);
    assert!(close(s.miter_limit, 4.0)); // SVG default
    assert!(s.dash.is_empty());
    assert_eq!(ps[2].stroke.as_ref().unwrap().join, LineJoin::Miter);
}

#[test]
fn fill_and_stroke_in_one_item() {
    let doc = parse(r##"width="100" height="100""##, r##"<rect width="10" height="10" fill="red" stroke="blue"/>"##);
    let p = only_path(&doc);
    assert!(p.fill.is_some() && p.stroke.is_some());
}

#[test]
fn paint_order_stroke_first_splits() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<rect width="10" height="10" fill="red" stroke="blue" paint-order="stroke"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 2);
    assert!(ps[0].fill.is_none() && ps[0].stroke.is_some());
    assert!(ps[1].fill.is_some() && ps[1].stroke.is_none());
}

// ---------------------------------------------------------------- patterns

fn pattern(c: &Option<Color>) -> &PatternPaint {
    match c {
        Some(Color::Pattern(p)) => p,
        other => panic!("expected pattern paint, got {other:?}"),
    }
}

fn gradient(c: &Option<Color>) -> &GradientPaint {
    match c {
        Some(Color::Gradient(g)) => g,
        other => panic!("expected gradient paint, got {other:?}"),
    }
}

fn assert_matrix(m: &Matrix, e: [f64; 6]) {
    let got = [m.a, m.b, m.c, m.d, m.e, m.f];
    assert!(got.iter().zip(e).all(|(a, b)| close(*a, b)), "expected matrix {e:?}, got {got:?}");
}

/// Check that a pattern paint matrix maps the upright IR tile space onto the
/// page exactly like SVG would: `tile_to_user` maps SVG tile space (px, y down,
/// origin at the tile's top-left corner) to the root user space of an SVG
/// whose height is `page_h_px`. `tile_h_px` is the tile height in px.
fn assert_pattern_maps(m: &Matrix, tile_h_px: f64, page_h_px: f64, tile_to_user: impl Fn(f64, f64) -> (f64, f64)) {
    for (tx, ty) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (3.0, 7.0), (-2.5, 4.0)] {
        let ir = Point::new(0.75 * tx, 0.75 * (tile_h_px - ty));
        let (ux, uy) = tile_to_user(tx, ty);
        assert_pt(m.apply(ir), 0.75 * ux, 0.75 * (page_h_px - uy));
    }
}

const DEG45: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[test]
fn pattern_tile_steps_and_identity_matrix() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><pattern id="dots" width="10" height="20" patternUnits="userSpaceOnUse">
             <rect width="5" height="5" fill="#ff0000"/><circle cx="5" cy="15" r="2" fill="blue"/></pattern></defs>
           <rect width="100" height="100" fill="url(#dots)"/>"##,
    );
    let pp = pattern(&only_path(&doc).fill);
    let pat = &pp.pattern;
    assert_eq!(pat.name, "dots");
    assert!(close(pat.bbox.llx, 0.0) && close(pat.bbox.lly, 0.0), "{:?}", pat.bbox);
    assert!(close(pat.bbox.urx, 7.5) && close(pat.bbox.ury, 15.0), "{:?}", pat.bbox);
    assert!(close(pat.x_step, 7.5) && close(pat.y_step, 15.0));
    // Tile art: upright (y up) in points.
    assert_eq!(pat.elements.len(), 2, "{:#?}", pat.elements);
    let Element::Path(sq) = &pat.elements[0] else { panic!("{:?}", pat.elements[0]) };
    assert_eq!(sq.fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    let a = anchors(&sq.subpaths[0]);
    assert_pt(a[0], 0.0, 15.0);
    assert_pt(a[1], 3.75, 15.0);
    assert_pt(a[2], 3.75, 11.25);
    assert_pt(a[3], 0.0, 11.25);
    let Element::Path(dot) = &pat.elements[1] else { panic!() };
    assert_eq!(dot.fill, Some(Color::Rgb(0.0, 0.0, 1.0)));
    // Circle centre (5, 15) px -> (3.75, 3.75) pt; radius 2 px -> 1.5 pt.
    let pts: Vec<Point> = dot.subpaths[0].points().collect();
    let minx = pts.iter().map(|p| p.x).fold(f64::MAX, f64::min);
    let maxy = pts.iter().map(|p| p.y).fold(f64::MIN, f64::max);
    assert!(close(minx, 2.25) && close(maxy, 5.25), "{pts:?}");
    // No patternTransform, no offset: tile top-left (0, 0) px sits at the page
    // top-left, i.e. the tile's lower-left corner at y = 75 - 15 = 60 pt.
    assert_matrix(&pp.matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 60.0]);
    assert!(!has_warning(&doc, "pattern"), "{:?}", doc.warnings);
}

#[test]
fn pattern_xy_offset() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="p" x="4" y="8" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="5" height="5"/></pattern>
           <rect width="100" height="100" fill="url(#p)"/>"##,
    );
    let pp = pattern(&only_path(&doc).fill);
    assert_matrix(&pp.matrix, [1.0, 0.0, 0.0, 1.0, 3.0, 75.0 - 0.75 * 18.0]);
    assert_pattern_maps(&pp.matrix, 10.0, 100.0, |x, y| (x + 4.0, y + 8.0));
}

#[test]
fn pattern_transform_rotate_and_scale() {
    let doc = parse(
        r##"width="200" height="100""##,
        r##"<pattern id="r" width="10" height="10" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><rect width="5" height="5"/></pattern>
           <pattern id="s" x="1" width="10" height="10" patternUnits="userSpaceOnUse" patternTransform="translate(3 4) scale(2 3)"><rect width="5" height="5"/></pattern>
           <rect width="100" height="100" fill="url(#r)"/><rect x="100" width="100" height="100" fill="url(#s)"/>"##,
    );
    let ps = paths(&doc);
    let r = pattern(&ps[0].fill);
    assert_eq!(r.pattern.name, "r");
    assert_pattern_maps(&r.matrix, 10.0, 100.0, |x, y| (DEG45 * (x - y), DEG45 * (x + y)));
    // Rotation is preserved as a proper rotation in y-up space (det > 0).
    assert!(r.matrix.a * r.matrix.d - r.matrix.b * r.matrix.c > 0.0);
    let s = pattern(&ps[1].fill);
    assert_pattern_maps(&s.matrix, 10.0, 100.0, |x, y| (3.0 + 2.0 * (x + 1.0), 4.0 + 3.0 * y));
    assert_matrix(&s.matrix, [2.0, 0.0, 0.0, 3.0, 0.75 * 5.0, 75.0 - 0.75 * 34.0]);
}

#[test]
fn pattern_in_transformed_group() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="p" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="5" height="5"/></pattern>
           <g transform="translate(10 20) scale(2)"><rect width="20" height="20" fill="url(#p)"/></g>"##,
    );
    let pp = pattern(&only_path(&doc).fill);
    assert_pattern_maps(&pp.matrix, 10.0, 100.0, |x, y| (10.0 + 2.0 * x, 20.0 + 2.0 * y));
    // Tile art itself is not affected by the element's transform.
    let Element::Path(t) = &pp.pattern.elements[0] else { panic!() };
    assert_pt(anchors(&t.subpaths[0])[1], 3.75, 7.5);
}

#[test]
fn pattern_view_box_scales_tile_content() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="p" width="10" height="10" viewBox="0 0 100 100" patternUnits="userSpaceOnUse"><rect width="50" height="50"/></pattern>
           <rect width="100" height="100" fill="url(#p)"/>"##,
    );
    let pp = pattern(&only_path(&doc).fill);
    assert!(close(pp.pattern.x_step, 7.5) && close(pp.pattern.y_step, 7.5));
    let Element::Path(t) = &pp.pattern.elements[0] else { panic!() };
    let a = anchors(&t.subpaths[0]);
    // 50 viewBox units = 5 px = 3.75 pt, from the tile's top edge.
    assert_pt(a[0], 0.0, 7.5);
    assert_pt(a[2], 3.75, 3.75);
    assert_matrix(&pp.matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 67.5]);
}

#[test]
fn pattern_object_bounding_box_units() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="p" width="0.25" height="0.5" patternContentUnits="objectBoundingBox"><rect width="0.1" height="0.1"/></pattern>
           <rect x="20" y="40" width="40" height="20" fill="url(#p)"/>"##,
    );
    let pp = pattern(&only_path(&doc).fill);
    // Tile: 0.25 * 40 = 10 px wide, 0.5 * 20 = 10 px high.
    assert!(close(pp.pattern.x_step, 7.5) && close(pp.pattern.y_step, 7.5), "{:?}", pp.pattern);
    // Content: 0.1 * bbox = 4 x 2 px.
    let Element::Path(t) = &pp.pattern.elements[0] else { panic!() };
    let a = anchors(&t.subpaths[0]);
    assert_pt(a[0], 0.0, 7.5);
    assert_pt(a[2], 3.0, 6.0);
    // Tile origin at the bbox origin (20, 40).
    assert_pattern_maps(&pp.matrix, 10.0, 100.0, |x, y| (x + 20.0, y + 40.0));
}

#[test]
fn pattern_on_stroke() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="p" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="2" height="2" fill="green"/></pattern>
           <path d="M10 10 L90 90" fill="none" stroke="url(#p)" stroke-width="8"/>"##,
    );
    let p = only_path(&doc);
    assert!(p.fill.is_none());
    let s = p.stroke.as_ref().unwrap();
    assert!(close(s.width, 6.0));
    let pp = pattern(&Some(s.color.clone())).clone();
    assert_eq!(pp.pattern.name, "p");
    assert_matrix(&pp.matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 72.0]);
}

#[test]
fn nested_pattern_inside_tile() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="inner" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="1" height="1" fill="red"/></pattern>
           <pattern id="outer" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="8" height="8" fill="url(#inner)"/></pattern>
           <rect width="100" height="100" fill="url(#outer)"/>"##,
    );
    let outer = pattern(&only_path(&doc).fill);
    assert_eq!(outer.pattern.name, "outer");
    let Element::Path(t) = &outer.pattern.elements[0] else { panic!() };
    let inner = pattern(&t.fill);
    assert_eq!(inner.pattern.name, "inner");
    assert!(close(inner.pattern.x_step, 1.5));
    // Inner IR space -> outer IR space: inner tile top (y = 1.5 pt) meets the
    // outer tile top (y = 7.5 pt).
    assert_matrix(&inner.matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 6.0]);
}

#[test]
fn self_referencing_pattern_does_not_hang() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="p" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="5" height="5" fill="url(#p)"/><rect x="5" width="5" height="5"/></pattern>
           <rect width="100" height="100" fill="url(#p)"/>"##,
    );
    assert!(!paths(&doc).is_empty());
}

#[test]
fn pattern_caching_and_names() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="a" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="5" height="5"/></pattern>
           <pattern id="b" width="10" height="10" patternUnits="userSpaceOnUse"><circle cx="5" cy="5" r="3"/></pattern>
           <pattern id="bb" width="0.5" height="0.5"><circle cx="5" cy="5" r="2"/></pattern>
           <rect width="10" height="10" fill="url(#a)"/>
           <rect x="20" width="10" height="10" fill="url(#a)" stroke="url(#b)"/>
           <rect x="40" width="20" height="20" fill="url(#bb)"/>
           <rect x="60" width="40" height="40" fill="url(#bb)"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 4);
    let a1 = pattern(&ps[0].fill);
    let a2 = pattern(&ps[1].fill);
    assert!(Arc::ptr_eq(&a1.pattern, &a2.pattern), "same <pattern> must share one IR pattern");
    // userSpaceOnUse: same tile origin for both uses.
    assert_eq!(a1.matrix, a2.matrix);
    let b = pattern(&Some(ps[1].stroke.as_ref().unwrap().color.clone())).clone();
    assert_eq!((a1.pattern.name.as_str(), b.pattern.name.as_str()), ("a", "b"));
    // objectBoundingBox pattern on differently sized shapes: different tiles,
    // distinct names.
    let c1 = pattern(&ps[2].fill);
    let c2 = pattern(&ps[3].fill);
    assert!(close(c1.pattern.x_step, 7.5) && close(c2.pattern.x_step, 15.0));
    let names: std::collections::HashSet<&str> =
        [&a1.pattern.name, &b.pattern.name, &c1.pattern.name, &c2.pattern.name].map(|s| s.as_str()).into();
    assert_eq!(names.len(), 4, "{names:?}");
}

#[test]
fn identical_pattern_tiles_are_shared() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="p" width="0.5" height="0.5"><rect width="2" height="2"/></pattern>
           <rect width="20" height="20" fill="url(#p)"/><rect x="50" width="20" height="20" fill="url(#p)"/>"##,
    );
    let ps = paths(&doc);
    assert!(Arc::ptr_eq(&pattern(&ps[0].fill).pattern, &pattern(&ps[1].fill).pattern));
}

#[test]
fn empty_pattern_paints_nothing() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<pattern id="p" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="5" height="5" fill="none"/></pattern>
           <rect width="10" height="10" fill="url(#p)" stroke="black"/>"##,
    );
    let p = only_path(&doc);
    assert!(p.fill.is_none() && p.stroke.is_some(), "{p:?}");
}

// ---------------------------------------------------------------- gradients

fn assert_stop(g: &GradientPaint, i: usize, offset: f64, r: f64, gr: f64, b: f64) {
    let s = &g.gradient.stops[i];
    assert!(close(s.offset, offset), "stop {i}: {s:?}");
    match s.color {
        Color::Rgb(x, y, z) => assert!(close(x, r) && close(y, gr) && close(z, b), "stop {i}: {s:?}"),
        ref c => panic!("stop {i}: expected RGB, got {c:?}"),
    }
}

#[test]
fn linear_gradient_user_space_geometry_and_stops() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<linearGradient id="g" x1="10" y1="20" x2="90" y2="30" gradientUnits="userSpaceOnUse">
             <stop offset="0" stop-color="#ffffff"/><stop offset="0.25" stop-color="#ff0000"/><stop offset="1" stop-color="#000000"/></linearGradient>
           <rect width="100" height="100" fill="url(#g)"/>"##,
    );
    let g = gradient(&only_path(&doc).fill);
    assert_eq!(g.gradient.kind, GradientKind::Linear);
    assert_pt(g.start, 10.0, 20.0);
    assert_pt(g.end, 90.0, 30.0);
    assert_matrix(&g.matrix, [0.75, 0.0, 0.0, -0.75, 0.0, 75.0]);
    assert_eq!(g.gradient.stops.len(), 3);
    assert_stop(g, 0, 0.0, 1.0, 1.0, 1.0);
    assert_stop(g, 1, 0.25, 1.0, 0.0, 0.0);
    assert_stop(g, 2, 1.0, 0.0, 0.0, 0.0);
    // Page-space end points.
    assert_pt(g.matrix.apply(g.start), 7.5, 60.0);
    assert_pt(g.matrix.apply(g.end), 67.5, 52.5);
    assert!(!has_warning(&doc, "gradient"), "{:?}", doc.warnings);
}

#[test]
fn linear_gradient_object_bounding_box_and_transform() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>
           <linearGradient id="t" href="#g" x2="0" y2="1" gradientTransform="rotate(90 0.5 0.5)"/>
           <rect x="10" y="10" width="80" height="40" fill="url(#g)"/>
           <g transform="translate(0 50)"><rect x="10" y="0" width="80" height="40" fill="url(#t)"/></g>"##,
    );
    let ps = paths(&doc);
    let g = gradient(&ps[0].fill);
    // Default objectBoundingBox: (0,0)->(1,0) in bbox space, folded into the matrix.
    assert_pt(g.start, 0.0, 0.0);
    assert_pt(g.end, 1.0, 0.0);
    assert_pt(g.matrix.apply(g.start), 7.5, 67.5);
    assert_pt(g.matrix.apply(g.end), 67.5, 67.5);
    // href inherits stops; rotate(90) about the bbox centre turns the vertical
    // axis (0.5,0)->(0.5,1)... here (0,0)->(0,1) into (1,0)->(0,0) in bbox units.
    let t = gradient(&ps[1].fill);
    assert_eq!(t.gradient.stops.len(), 2);
    assert_stop(t, 1, 1.0, 0.0, 0.0, 1.0);
    let ux = |x: f64, y: f64| (0.75 * (10.0 + 80.0 * x), 75.0 - 0.75 * (50.0 + 40.0 * y));
    let (sx, sy) = ux(1.0, 0.0);
    let (ex, ey) = ux(0.0, 0.0);
    assert_pt(t.matrix.apply(t.start), sx, sy);
    assert_pt(t.matrix.apply(t.end), ex, ey);
}

#[test]
fn radial_gradient_geometry() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<radialGradient id="r" cx="50" cy="40" r="30" fx="45" fy="35" gradientUnits="userSpaceOnUse" gradientTransform="scale(2 1)">
             <stop offset="0.1" stop-color="lime"/><stop offset="0.9" stop-color="#000080"/></radialGradient>
           <radialGradient id="plain" cx="20" cy="20" r="10" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="white"/><stop offset="1" stop-color="black"/></radialGradient>
           <rect width="100" height="100" fill="url(#r)"/><circle cx="20" cy="20" r="10" fill="url(#plain)"/>"##,
    );
    let ps = paths(&doc);
    let g = gradient(&ps[0].fill);
    assert_eq!(g.gradient.kind, GradientKind::Radial);
    assert_pt(g.end, 50.0, 40.0);
    assert_pt(g.start, 45.0, 35.0);
    assert!(close(g.radius, 30.0));
    assert_matrix(&g.matrix, [1.5, 0.0, 0.0, -0.75, 0.0, 75.0]);
    assert_stop(g, 0, 0.1, 0.0, 1.0, 0.0);
    assert_stop(g, 1, 0.9, 0.0, 0.0, 128.0 / 255.0);
    let p = gradient(&ps[1].fill);
    assert_pt(p.start, 20.0, 20.0);
    assert_pt(p.end, 20.0, 20.0);
    assert!(close(p.radius, 10.0));
}

#[test]
fn radial_gradient_object_bounding_box() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<radialGradient id="r"><stop offset="0" stop-color="white"/><stop offset="1" stop-color="black"/></radialGradient>
           <rect x="20" y="20" width="40" height="20" fill="url(#r)"/>"##,
    );
    let g = gradient(&only_path(&doc).fill);
    assert_pt(g.end, 0.5, 0.5);
    assert!(close(g.radius, 0.5));
    // Centre at the bbox centre (40, 30) px.
    assert_pt(g.matrix.apply(g.end), 30.0, 75.0 - 22.5);
    assert_matrix(&g.matrix, [30.0, 0.0, 0.0, -15.0, 15.0, 60.0]);
}

#[test]
fn gradient_on_stroke_is_shared_and_warnings() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<linearGradient id="g" spreadMethod="reflect"><stop offset="0" stop-color="red" stop-opacity="0.5"/><stop offset="1" stop-color="blue"/></linearGradient>
           <rect width="10" height="10" fill="url(#g)" stroke="url(#g)"/>
           <rect x="20" width="10" height="10" fill="url(#g)"/>"##,
    );
    let ps = paths(&doc);
    let f = gradient(&ps[0].fill);
    let s = gradient(&Some(ps[0].stroke.as_ref().unwrap().color.clone())).clone();
    assert!(Arc::ptr_eq(&f.gradient, &s.gradient));
    assert!(Arc::ptr_eq(&f.gradient, &gradient(&ps[1].fill).gradient));
    assert_stop(f, 0, 0.0, 1.0, 0.0, 0.0);
    assert_eq!(doc.warnings.iter().filter(|w| w.contains("stop-opacity")).count(), 1, "{:?}", doc.warnings);
    assert_eq!(doc.warnings.iter().filter(|w| w.contains("spreadMethod")).count(), 1, "{:?}", doc.warnings);
}

#[test]
fn single_stop_gradient_is_solid() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<linearGradient id="g"><stop offset="0.3" stop-color="#00ff00"/></linearGradient>
           <rect width="10" height="10" fill="url(#g)"/>"##,
    );
    assert_eq!(only_path(&doc).fill, Some(Color::Rgb(0.0, 1.0, 0.0)));
}

#[test]
fn gradient_inside_pattern_tile() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<linearGradient id="g" x1="0" y1="0" x2="10" y2="0" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>
           <pattern id="p" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="10" height="5" fill="url(#g)"/></pattern>
           <rect width="100" height="100" fill="url(#p)"/>"##,
    );
    let pp = pattern(&only_path(&doc).fill);
    let Element::Path(t) = &pp.pattern.elements[0] else { panic!() };
    let g = gradient(&t.fill);
    // Gradient matrix maps into the (upright, pt) pattern space.
    assert_matrix(&g.matrix, [0.75, 0.0, 0.0, -0.75, 0.0, 7.5]);
}

#[test]
fn opacity_ignored_with_single_warning() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<rect width="10" height="10" fill="red" fill-opacity="0.5"/>
           <rect width="10" height="10" fill="none" stroke="red" stroke-opacity="0.5"/>
           <g opacity="0.3"><rect width="10" height="10"/><rect x="20" width="10" height="10"/></g>"##,
    );
    assert_eq!(paths(&doc).len(), 4);
    assert_eq!(doc.warnings.iter().filter(|w| w.contains("opacity")).count(), 1, "{:?}", doc.warnings);
}

#[test]
fn invisible_and_unpainted_paths_dropped() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<rect width="10" height="10" fill="none"/>
           <rect width="10" height="10" visibility="hidden"/>
           <rect width="10" height="10" display="none"/>
           <rect width="10" height="10" fill="red" fill-opacity="0"/>
           <rect width="10" height="10" fill="green"/>"##,
    );
    assert_eq!(only_path(&doc).fill, Some(Color::Rgb(0.0, 128.0 / 255.0, 0.0)));
}

// ---------------------------------------------------------------- structure

#[test]
fn groups_inline_single_and_skip_empty() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<g id="two"><rect width="1" height="1"/><rect x="5" width="1" height="1"/></g>
           <g id="one"><rect width="1" height="1"/></g>
           <g id="empty"></g>
           <g id="emptyish"><rect width="1" height="1" fill="none"/></g>"##,
    );
    assert_eq!(doc.elements.len(), 2, "{:#?}", doc.elements);
    match &doc.elements[0] {
        Element::Group(c) => assert_eq!(c.len(), 2),
        e => panic!("expected group, got {e:?}"),
    }
    assert!(matches!(doc.elements[1], Element::Path(_)));
}

#[test]
fn clip_path_becomes_clip_group() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><clipPath id="c"><rect x="10" y="10" width="20" height="20"/></clipPath></defs>
           <g clip-path="url(#c)"><circle cx="20" cy="20" r="20" fill="red"/><rect width="5" height="5"/></g>"##,
    );
    assert_eq!(doc.elements.len(), 1);
    match &doc.elements[0] {
        Element::ClipGroup { clip, even_odd, children } => {
            assert!(!even_odd);
            assert_eq!(clip.len(), 1);
            let pts = anchors(&clip[0]);
            assert_pt(pts[0], 7.5, 67.5);
            assert_pt(pts[2], 22.5, 52.5);
            assert_eq!(children.len(), 2);
        }
        e => panic!("expected clip group, got {e:?}"),
    }
}

#[test]
fn clip_path_on_element_with_transform_and_rule() {
    // clip-path on a transformed shape: clip lives in the shape's user space.
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><clipPath id="c"><path d="M0 0H10V10H0Z M2 2H8V8H2Z" clip-rule="evenodd"/>
           </clipPath></defs>
           <rect transform="translate(20 20)" width="10" height="10" clip-path="url(#c)"/>"##,
    );
    match &doc.elements[0] {
        Element::ClipGroup { clip, even_odd, children } => {
            assert!(*even_odd);
            assert_eq!(clip.len(), 2);
            assert_pt(clip[0].start, 15.0, 60.0);
            assert_eq!(children.len(), 1);
        }
        e => panic!("expected clip group, got {e:?}"),
    }
}

#[test]
fn clip_path_object_bounding_box_and_nested() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs>
             <clipPath id="outer"><rect width="50" height="50"/></clipPath>
             <clipPath id="c" clipPathUnits="objectBoundingBox" clip-path="url(#outer)"><rect width="0.5" height="0.5"/></clipPath>
           </defs>
           <rect x="20" y="20" width="40" height="40" clip-path="url(#c)"/>"##,
    );
    // Nested clip-path is expressed as nested clip groups (intersection).
    match &doc.elements[0] {
        Element::ClipGroup { clip: outer, children, .. } => {
            assert_pt(anchors(&outer[0])[2], 37.5, 37.5);
            match &children[0] {
                Element::ClipGroup { clip, .. } => {
                    // bbox units: x 20 + 0.5*40 = 40 px
                    let pts = anchors(&clip[0]);
                    assert_pt(pts[0], 15.0, 60.0);
                    assert_pt(pts[2], 30.0, 45.0);
                }
                e => panic!("expected inner clip group, got {e:?}"),
            }
        }
        e => panic!("expected clip group, got {e:?}"),
    }
}

#[test]
fn mask_and_filter_ignored_with_warning() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><mask id="m"><rect width="50" height="50" fill="white"/></mask>
           <filter id="f"><feGaussianBlur stdDeviation="2"/></filter></defs>
           <rect width="10" height="10" mask="url(#m)"/>
           <rect width="10" height="10" filter="url(#f)"/>"##,
    );
    assert_eq!(paths(&doc).len(), 2);
    assert!(has_warning(&doc, "mask"), "{:?}", doc.warnings);
    assert!(has_warning(&doc, "filter"), "{:?}", doc.warnings);
}

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

#[test]
fn images_skipped_with_count_warning() {
    let body = format!(
        r##"<image width="10" height="10" xlink:href="data:image/png;base64,{PNG_1X1}"/>
           <image x="20" width="10" height="10" href="data:image/png;base64,{PNG_1X1}"/>
           <rect width="5" height="5"/>"##
    );
    let doc = parse(r##"width="100" height="100""##, &body);
    assert_eq!(paths(&doc).len(), 1);
    let w: Vec<_> = doc.warnings.iter().filter(|w| w.contains("image")).collect();
    assert_eq!(w.len(), 1, "{:?}", doc.warnings);
    assert!(w[0].starts_with('2'), "{}", w[0]);
}

#[test]
fn use_elements_are_resolved() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><rect id="r" width="10" height="10"/></defs>
           <use href="#r" x="10"/><use xlink:href="#r" x="30"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 2);
    assert_pt(ps[1].subpaths[0].start, 22.5, 75.0);
}

// ---------------------------------------------------------------- inputs

#[test]
fn svgz_is_supported() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    let raw = svg(r##"width="100" height="100""##, r##"<rect width="10" height="10" fill="red"/>"##);
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&raw).unwrap();
    let gz = enc.finish().unwrap();
    assert_eq!(&gz[..2], &[0x1f, 0x8b]);
    let doc = parse_svg(&gz).unwrap();
    assert_eq!(only_path(&doc).fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    assert_eq!(parse_svg(&raw).unwrap(), doc);
}

#[test]
fn bom_and_xml_prolog_accepted() {
    let mut data = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n".as_bytes().to_vec();
    data.extend(svg(r##"width="10" height="10""##, r##"<rect width="5" height="5"/>"##));
    let doc = parse_svg(&data).unwrap();
    assert_eq!(doc.path_count(), 1);
}

#[test]
fn text_converted_to_paths() {
    let doc = parse(
        r##"width="200" height="100""##,
        r##"<text x="10" y="50" font-family="Arial, Helvetica, sans-serif" font-size="30" fill="purple">Hello</text>"##,
    );
    if doc.path_count() == 0 {
        // No usable system fonts (e.g. a minimal CI container): must warn, not fail.
        assert!(has_warning(&doc, "font"), "{:?}", doc.warnings);
        eprintln!("no system fonts available; text test ran in tolerant mode");
        return;
    }
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    let ps = paths(&doc);
    assert!(ps.iter().all(|p| p.fill == Some(Color::Rgb(128.0 / 255.0, 0.0, 128.0 / 255.0))));
    assert!(ps.iter().flat_map(|p| &p.subpaths).count() >= 5, "expected several glyph outlines");
    let b = doc.content_bounds().unwrap();
    // Baseline at y=50 px -> 37.5 pt; text sits above the baseline in page space.
    assert!(b.llx >= 7.0 && b.lly >= 36.0 && b.ury <= 60.0 && b.urx < 150.0, "{b:?}");
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        assert!(doc.path_count() > 0);
    }
}

#[test]
fn empty_text_does_not_warn() {
    let doc = parse(r##"width="10" height="10""##, r##"<text x="1" y="1">   </text><rect width="1" height="1"/>"##);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

#[test]
fn malformed_input_returns_err() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("whitespace", b"  \n ".to_vec()),
        ("truncated", b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\"".to_vec()),
        ("not xml", b"this is not an svg".to_vec()),
        ("not svg root", b"<html><body/></html>".to_vec()),
        ("binary", vec![0xff, 0xfe, 0x00, 0x01, 0x80, 0x90]),
        ("bad gzip", vec![0x1f, 0x8b, 0x08, 0x00, 0xde, 0xad, 0xbe, 0xef]),
        ("zero size", svg(r##"width="0" height="10""##, "<rect width=\"1\" height=\"1\"/>")),
        ("unbalanced tags", b"<svg xmlns=\"http://www.w3.org/2000/svg\"><g></svg>".to_vec()),
    ];
    for (name, data) in cases {
        let r = parse_svg(&data);
        let err = r.expect_err(&format!("{name}: expected an error"));
        assert!(!err.to_string().is_empty(), "{name}");
    }
}

#[test]
fn garbage_attributes_do_not_panic() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<path d="M 10 10 L Z Q 1e400 5 nan"/><rect width="-5" height="abc"/>
           <circle r="-1"/><path d="M0 0 L10 10" stroke-width="-3" stroke="red" stroke-dasharray="-1 2"/>
           <g transform="matrix(0 0 0 0 0 0)"><rect width="10" height="10"/></g>
           <rect width="10" height="10" fill="url(#missing)"/>"##,
    );
    for p in paths(&doc) {
        for sp in &p.subpaths {
            assert!(sp.points().all(|pt| pt.x.is_finite() && pt.y.is_finite()));
        }
    }
}

// ---------------------------------------------------------------- end to end

#[test]
fn end_to_end_svg_to_ai8() {
    let doc = parse(
        r##"width="200" height="200""##,
        r##"<defs><clipPath id="c"><circle cx="100" cy="100" r="80"/></clipPath></defs>
           <g id="grp"><rect x="10" y="10" width="50" height="50" fill="#ff0000"/>
              <path d="M10 100 Q50 150 90 100" fill="none" stroke="#0000ff" stroke-width="2" stroke-dasharray="4 2"/></g>
           <g clip-path="url(#c)"><rect width="200" height="200" fill="lime"/></g>
           <path d="M0 0H50V50H0Z M10 10H40V40H10Z" fill-rule="evenodd" transform="translate(120 120)"/>"##,
    );
    let ai = write_ai8(&doc, &WriteOptions::default());
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.paths, 5, "{stats:?}"); // 4 painted paths + the clip path
    assert_eq!(stats.groups, 1, "{stats:?}");
    assert_eq!(stats.clip_groups, 1, "{stats:?}");
    assert_eq!(stats.compound_paths, 1, "{stats:?}"); // the even-odd ring
    assert!(ai.contains("%%BoundingBox:"));
    assert!(ai.contains("1 0 0 Xa"), "red RGB fill");
    assert!(ai.contains("0 0 1 XA"), "blue RGB stroke");
    assert!(ai.contains("[3 1.5]0 d"), "scaled dash");
    assert!(ai.contains(" C\n"), "curves");
    assert!(ai.contains("h W"), "clip");
    assert!(ai.contains("1 XR"), "even-odd");
    assert!(ai.lines().any(|l| l == "S"), "open stroked path");
    assert!(ai.lines().any(|l| l == "f"), "closed filled path");
    assert!(ai.lines().any(|l| l == "u") && ai.lines().any(|l| l == "U"));
    // The red rect: 10..60 px -> 7.5..45 pt, y flipped: 150 - 7.5 = 142.5.
    assert!(ai.contains("7.5 142.5 m"), "{ai}");
}

#[test]
fn end_to_end_patterns_and_gradients_all_modes() {
    use ai8_converter::convert::{ConvertOptions, PatternMode, convert_file};
    let svg = svg(
        r##"width="200" height="200""##,
        r##"<linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>
           <radialGradient id="r"><stop offset="0" stop-color="white"/><stop offset="1" stop-color="green"/></radialGradient>
           <pattern id="inner" width="3" height="3" patternUnits="userSpaceOnUse"><circle cx="1.5" cy="1.5" r="1" fill="black"/></pattern>
           <pattern id="dots" width="20" height="20" patternUnits="userSpaceOnUse" patternTransform="rotate(30)">
             <rect width="10" height="10" fill="url(#g)"/><rect x="10" y="10" width="10" height="10" fill="url(#inner)"/></pattern>
           <rect width="100" height="100" fill="url(#dots)" stroke="url(#r)" stroke-width="4"/>
           <circle cx="150" cy="150" r="40" fill="url(#r)"/>
           <path d="M110 20 H190" stroke="url(#g)" stroke-width="10"/>"##,
    );
    let dir = std::env::temp_dir().join(format!("ai8-svg-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("paints.svg");
    std::fs::write(&input, &svg).unwrap();
    for mode in [PatternMode::Native, PatternMode::Expand] {
        let out_dir = dir.join(format!("{mode:?}"));
        let opts = ConvertOptions { pattern_mode: mode, out_dir: Some(out_dir.clone()), ..Default::default() };
        let report = convert_file(&input, &opts).unwrap_or_else(|e| panic!("{mode:?}: {e:#}"));
        assert_eq!(report.outputs.len(), 1);
        let ai = std::fs::read_to_string(&report.outputs[0]).unwrap();
        let stats = common::validate_ai8(&ai);
        assert!(stats.paths >= 3, "{mode:?}: {stats:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

