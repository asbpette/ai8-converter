//! DXF input: structure, geometry accuracy, styles, hatches, blocks, binary
//! DXF and end-to-end AI8 output.

mod common;

use ai8_converter::ai8::{WriteOptions, write_ai8};
use ai8_converter::dxf::{looks_like_dxf, parse_dxf};
use ai8_converter::ir::*;

const MM: f64 = 72.0 / 25.4;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Group pairs from a compact "code=value code=value" description.
fn pairs(spec: &str) -> String {
    let mut s = String::new();
    for tok in spec.split_whitespace() {
        let (c, v) = tok.split_once('=').unwrap_or_else(|| panic!("bad token {tok}"));
        s.push_str(&format!("{c:>3}\n{v}\n"));
    }
    s
}

/// A complete ASCII DXF from header variables, table records, blocks and
/// entities (each a list of `(TYPE, "code=value …")`).
struct Dxf {
    header: String,
    layers: Vec<String>,
    ltypes: Vec<String>,
    blocks: String,
    entities: String,
}

impl Dxf {
    fn new() -> Self {
        Dxf { header: String::new(), layers: Vec::new(), ltypes: Vec::new(), blocks: String::new(), entities: String::new() }
    }
    fn units(mut self, u: i32) -> Self {
        self.header.push_str(&pairs(&format!("9=$INSUNITS 70={u}")));
        self
    }
    fn var(mut self, spec: &str) -> Self {
        self.header.push_str(&pairs(spec));
        self
    }
    fn layer(mut self, spec: &str) -> Self {
        self.layers.push(format!("  0\nLAYER\n{}", pairs(spec)));
        self
    }
    fn ltype(mut self, spec: &str) -> Self {
        self.ltypes.push(format!("  0\nLTYPE\n{}", pairs(spec)));
        self
    }
    fn block(mut self, name: &str, base: (f64, f64), ents: &[(&str, &str)]) -> Self {
        self.blocks.push_str(&format!(
            "  0\nBLOCK\n{}",
            pairs(&format!("8=0 2={name} 70=0 10={} 20={} 30=0", base.0, base.1))
        ));
        for (t, spec) in ents {
            self.blocks.push_str(&format!("  0\n{t}\n{}", pairs(spec)));
        }
        self.blocks.push_str("  0\nENDBLK\n");
        self
    }
    fn ent(mut self, t: &str, spec: &str) -> Self {
        self.entities.push_str(&format!("  0\n{t}\n{}", pairs(spec)));
        self
    }
    fn raw_ent(mut self, t: &str, body: &str) -> Self {
        self.entities.push_str(&format!("  0\n{t}\n{body}"));
        self
    }
    fn build(&self) -> Vec<u8> {
        let mut s = String::from("  0\nSECTION\n  2\nHEADER\n");
        s.push_str(&self.header);
        s.push_str("  0\nENDSEC\n  0\nSECTION\n  2\nTABLES\n");
        s.push_str("  0\nTABLE\n  2\nLTYPE\n");
        for l in &self.ltypes {
            s.push_str(l);
        }
        s.push_str("  0\nENDTAB\n  0\nTABLE\n  2\nLAYER\n");
        for l in &self.layers {
            s.push_str(l);
        }
        s.push_str("  0\nENDTAB\n  0\nENDSEC\n  0\nSECTION\n  2\nBLOCKS\n");
        s.push_str(&self.blocks);
        s.push_str("  0\nENDSEC\n  0\nSECTION\n  2\nENTITIES\n");
        s.push_str(&self.entities);
        s.push_str("  0\nENDSEC\n  0\nEOF\n");
        s.into_bytes()
    }
    fn parse(&self) -> Document {
        parse_dxf(&self.build()).expect("parse")
    }
}

fn all_paths(els: &[Element]) -> Vec<&PathItem> {
    let mut v = Vec::new();
    for e in els {
        match e {
            Element::Path(p) => v.push(p),
            Element::Group(c) => v.extend(all_paths(c)),
            Element::ClipGroup { children, .. } => v.extend(all_paths(children)),
        }
    }
    v
}

fn paths(doc: &Document) -> Vec<&PathItem> {
    all_paths(&doc.elements)
}

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

fn assert_pt(p: Point, x: f64, y: f64, eps: f64) {
    assert!(close(p.x, x, eps) && close(p.y, y, eps), "expected ({x}, {y}), got ({}, {})", p.x, p.y);
}

fn bez(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    Point::new(a * p0.x + b * p1.x + c * p2.x + d * p3.x, a * p0.y + b * p1.y + c * p2.y + d * p3.y)
}

/// Sample points along all segments of a subpath (including curve interiors).
fn samples(sp: &Subpath) -> Vec<Point> {
    let mut out = vec![sp.start];
    let mut cur = sp.start;
    for s in &sp.segments {
        match s {
            Segment::Line(p) => {
                out.push(*p);
                cur = *p;
            }
            Segment::Curve(a, b, c) => {
                for k in 1..=16 {
                    out.push(bez(cur, *a, *b, *c, k as f64 / 16.0));
                }
                cur = *c;
            }
        }
    }
    out
}

fn end_point(sp: &Subpath) -> Point {
    match sp.segments.last() {
        Some(Segment::Line(p)) | Some(Segment::Curve(_, _, p)) => *p,
        None => sp.start,
    }
}

fn rgb_of(c: &Color) -> (f64, f64, f64) {
    match c {
        Color::Rgb(r, g, b) => (*r, *g, *b),
        other => panic!("expected RGB, got {other:?}"),
    }
}

fn stroke_rgb(p: &PathItem) -> (f64, f64, f64) {
    rgb_of(&p.stroke.as_ref().expect("stroke").color)
}

fn line(spec_extra: &str, x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    format!("8=0 {spec_extra} 10={x0} 20={y0} 30=0 11={x1} 21={y1} 31=0")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/dxf/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture")
}

fn ai8_roundtrip(doc: &Document) -> common::Ai8Stats {
    let ai = write_ai8(doc, &WriteOptions::default());
    common::validate_ai8(&ai)
}

// ---------------------------------------------------------------------------
// Detection, structure, errors
// ---------------------------------------------------------------------------

#[test]
fn detection() {
    assert!(looks_like_dxf(b"  0\nSECTION\n  2\nHEADER\n"));
    assert!(looks_like_dxf(b"0\r\nSECTION\r\n"));
    assert!(looks_like_dxf(b"\xEF\xBB\xBF  0\nSECTION\n"));
    assert!(looks_like_dxf(b"AutoCAD Binary DXF\r\n\x1a\0\0\0SECTION\0"));
    assert!(!looks_like_dxf(b"%!PS-Adobe-3.0"));
    assert!(!looks_like_dxf(b"<svg>"));
    assert!(looks_like_dxf(&fixture("r12_basic.dxf")));
    assert!(looks_like_dxf(&fixture("r2000_full.dxf")));
}

#[test]
fn malformed_inputs_are_errors_not_panics() {
    for bad in [
        &b""[..],
        b"   \n",
        b"garbage",
        b"  0\nSECTION\n  2\nENTITIES\n  0\nENDSEC\n  0\nEOF\n", // no geometry
        b"  0\nSECTION\n  2\nENTITIES\n 10\n",                  // truncated
        b"  0\nSECTION\n  2\nENTITIES\nxx\nLINE\n",              // bad group code
        b"AutoCAD Binary DXF\r\n\x1a\0\0\0SECTION\0\x02\x00ENTI", // truncated binary
    ] {
        let r = parse_dxf(bad);
        assert!(r.is_err(), "{:?} should fail", String::from_utf8_lossy(bad));
        assert!(!r.unwrap_err().to_string().is_empty());
    }
    // Every prefix and a set of byte corruptions of real files: never panic.
    for name in ["r12_basic.dxf", "r2000_full.dxf"] {
        let data = fixture(name);
        for cut in (0..data.len()).step_by(97) {
            let _ = parse_dxf(&data[..cut]);
        }
        for k in (0..data.len()).step_by(211) {
            let mut d = data.clone();
            d[k] = b"-9e\n0x."[k % 7];
            let _ = parse_dxf(&d);
        }
    }
}

#[test]
fn empty_drawing_with_only_text_explains() {
    let err = Dxf::new().units(4).ent("TEXT", "8=0 10=0 20=0 30=0 40=2.5 1=hi").build();
    let e = parse_dxf(&err).unwrap_err().to_string();
    assert!(e.contains("no drawable geometry") && e.contains("text"), "{e}");
}

// ---------------------------------------------------------------------------
// Units and page
// ---------------------------------------------------------------------------

#[test]
fn units_and_page() {
    for (u, f) in [(1, 72.0), (4, MM), (5, 10.0 * MM), (6, 1000.0 * MM), (2, 864.0)] {
        let doc = Dxf::new().units(u).ent("LINE", &line("", 10.0, 5.0, 110.0, 25.0)).parse();
        if u == 6 || u == 2 {
            // 100 m / 100 ft wide: larger than an Illustrator artboard.
            assert!(doc.warnings.iter().any(|w| w.contains("maximum artboard")), "{:?}", doc.warnings);
        } else {
            assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        }
        assert!(close(doc.page.llx, 0.0, 1e-9) && close(doc.page.lly, 0.0, 1e-9));
        assert!(close(doc.page.width(), 100.0 * f, 1e-6 * f), "units {u}: {:?}", doc.page);
        assert!(close(doc.page.height(), 20.0 * f, 1e-6 * f));
        let p = paths(&doc)[0];
        assert_pt(p.subpaths[0].start, 0.0, 0.0, 1e-9);
    }
    // Unitless → mm with a warning.
    let doc = Dxf::new().ent("LINE", &line("", 0.0, 0.0, 100.0, 0.0)).parse();
    assert!(close(doc.page.width(), 100.0 * MM, 1e-9));
    assert!(doc.warnings.iter().any(|w| w.contains("no units") && w.contains("mm")), "{:?}", doc.warnings);
}

#[test]
fn page_uses_tight_curve_bounds_not_stale_extents() {
    let doc = Dxf::new()
        .units(4)
        .var("9=$EXTMIN 10=-1000 20=-1000 30=0 9=$EXTMAX 10=1000 20=1000 30=0")
        .ent("CIRCLE", "8=0 10=50 20=50 30=0 40=10")
        .parse();
    assert!(close(doc.page.width(), 20.0 * MM, 1e-3), "{:?}", doc.page);
    assert!(close(doc.page.height(), 20.0 * MM, 1e-3));
}

// ---------------------------------------------------------------------------
// Geometry accuracy
// ---------------------------------------------------------------------------

#[test]
fn bulge_arcs_lie_on_their_circle() {
    // (0,0) -bulge 1-> (10,0): CCW semicircle below the chord, centre (5,0).
    // (10,0) -bulge −tan(22.5°)-> (15,5): clockwise quarter arc, centre (15,0).
    let doc = Dxf::new()
        .units(1)
        .ent("LWPOLYLINE", "8=0 90=3 70=0 10=0 20=0 42=1 10=10 20=0 42=-0.41421356237309503 10=15 20=5")
        .parse();
    let p = paths(&doc)[0];
    let sp = &p.subpaths[0];
    // Undo the page shift: the lowest point is the semicircle bottom (y = -5).
    let (dx, dy) = (0.0, 5.0 * 72.0);
    let to_units = |q: Point| Point::new((q.x - dx) / 72.0, (q.y - dy) / 72.0);
    let pts: Vec<Point> = samples(sp).into_iter().map(to_units).collect();
    assert_pt(pts[0], 0.0, 0.0, 1e-9);
    assert_pt(to_units(end_point(sp)), 15.0, 5.0, 1e-9);
    // Semicircle samples on circle (5,0) r 5; arc 2 samples on its circle.
    let mut on1 = 0;
    let mut on2 = 0;
    for q in &pts {
        let d1 = ((q.x - 5.0).powi(2) + q.y.powi(2)).sqrt();
        let d2a = ((q.x - 15.0).powi(2) + q.y.powi(2)).sqrt();
        let d2b = ((q.x - 10.0).powi(2) + (q.y - 5.0).powi(2)).sqrt();
        if q.x <= 10.0 + 1e-9 {
            assert!(close(d1, 5.0, 5e-3), "{q:?} off semicircle ({d1})");
            on1 += 1;
        } else {
            assert!(close(d2a, 5.0, 5e-3) || close(d2b, 5.0, 5e-3), "{q:?} off quarter arc");
            on2 += 1;
        }
    }
    assert!(on1 > 10 && on2 > 5);
    // Semicircle midpoint (5,-5) is an exact segment end (≤ 90° splits).
    assert!(pts.iter().any(|q| close(q.x, 5.0, 1e-9) && close(q.y, -5.0, 1e-9)));
    // The quarter arc bulges below the chord (clockwise) → centre (10,5).
    // Negative bulge = clockwise: centre (15,0) on the right of travel, so the
    // arc bulges to the left of the chord y = x − 10.
    let bulge = pts.iter().filter(|q| q.x > 10.0 + 1e-6).map(|q| q.y - (q.x - 10.0)).fold(f64::NEG_INFINITY, f64::max);
    assert!(bulge > 1.0, "quarter arc should bulge left of the chord ({bulge})");
    assert!(pts.iter().filter(|q| q.x > 10.0 + 1e-6).all(|q| close(((q.x - 15.0).powi(2) + q.y.powi(2)).sqrt(), 5.0, 5e-3)));
}

#[test]
fn closed_polyline_bulge_on_closing_segment() {
    // Square 0..10 whose closing edge (0,10)->(0,0) is a semicircle (bulge 1 = CCW).
    let doc = Dxf::new()
        .units(1)
        .ent("LWPOLYLINE", "8=0 90=4 70=1 10=0 20=0 10=10 20=0 10=10 20=10 10=0 20=10 42=1")
        .parse();
    let sp = &paths(&doc)[0].subpaths[0];
    assert!(sp.closed);
    // CCW from (0,10) to (0,0) around (0,5) passes x = -5: page shifted by +5.
    assert!(close(doc.page.width(), 15.0 * 72.0, 1e-6), "{:?}", doc.page);
    assert!(matches!(sp.segments.last(), Some(Segment::Curve(..))));
}

#[test]
fn circle_arc_ellipse_accuracy() {
    let doc = Dxf::new()
        .units(1)
        .ent("CIRCLE", "8=0 10=0 20=0 30=0 40=10")
        .ent("ARC", "8=0 10=0 20=0 30=0 40=5 50=350 51=10")
        // Rotated ellipse: major axis along +Y (length 4), ratio 0.5, half (0..π).
        .ent("ELLIPSE", "8=0 10=0 20=0 30=0 11=0 21=4 31=0 210=0 220=0 230=1 40=0.5 41=0 42=3.141592653589793")
        .parse();
    let ps = paths(&doc);
    let (dx, dy) = (10.0, 10.0);
    let u = |q: Point| Point::new(q.x / 72.0 - dx, q.y / 72.0 - dy);
    // Circle: closed, 4 curves, radius 10 everywhere within the Bézier error.
    let c = &ps[0].subpaths[0];
    assert!(c.closed);
    assert_eq!(c.segments.len(), 4);
    for q in samples(c).into_iter().map(u) {
        assert!(close((q.x * q.x + q.y * q.y).sqrt(), 10.0, 10.0 * 3e-4));
    }
    // Arc crossing 0°: from 350° to 10° CCW (20° sweep).
    let a = &ps[1].subpaths[0];
    assert!(!a.closed);
    assert_pt(u(a.start), 5.0 * (350f64).to_radians().cos(), 5.0 * (350f64).to_radians().sin(), 1e-9);
    assert_pt(u(end_point(a)), 5.0 * (10f64).to_radians().cos(), 5.0 * (10f64).to_radians().sin(), 1e-9);
    assert!(samples(a).into_iter().map(u).all(|q| q.x > 4.9));
    // Ellipse: t = 0 → centre + major = (0,4); t = π/2 → centre + minor
    // (normal × major = (-4, 0) · 0.5 = (-2, 0)); t = π → (0,-4).
    let e = &ps[2].subpaths[0];
    assert_pt(u(e.start), 0.0, 4.0, 1e-9);
    assert_pt(u(end_point(e)), 0.0, -4.0, 1e-9);
    assert!(samples(e).into_iter().map(u).any(|q| close(q.x, -2.0, 1e-9) && close(q.y, 0.0, 1e-9)));
    for q in samples(e).into_iter().map(u) {
        // (x/2)² + (y/4)² = 1
        assert!(close((q.x / 2.0).powi(2) + (q.y / 4.0).powi(2), 1.0, 2e-3), "{q:?}");
    }
}

#[test]
fn extrusion_minus_z_mirrors_x() {
    let doc = Dxf::new()
        .units(1)
        .ent("LINE", &line("", 0.0, 0.0, 1.0, 0.0))
        .ent("CIRCLE", "8=0 10=10 20=3 30=0 40=1 210=0 220=0 230=-1")
        .ent("ARC", "8=0 10=10 20=3 30=0 40=2 50=0 51=90 210=0 220=0 230=-1")
        .ent("LWPOLYLINE", "8=0 90=2 70=0 10=5 20=0 10=6 20=0 210=0 220=0 230=-1")
        .parse();
    let ps = paths(&doc);
    // Line starts at world (0,0); the page origin is at x = -12 (arc reaches -12).
    let origin = ps[0].subpaths[0].start;
    let u = |q: Point| Point::new((q.x - origin.x) / 72.0, (q.y - origin.y) / 72.0);
    let circle: Vec<Point> = samples(&ps[1].subpaths[0]).into_iter().map(u).collect();
    let cx = circle.iter().map(|q| q.x).sum::<f64>() / circle.len() as f64;
    assert!(close(cx, -10.0, 0.05), "circle centre x {cx}");
    // Mirrored arc: OCS 0°→90° becomes WCS 180°→90° (start (-12,3), end (-10,5)).
    let a = &ps[2].subpaths[0];
    assert_pt(u(a.start), -12.0, 3.0, 1e-9);
    assert_pt(u(end_point(a)), -10.0, 5.0, 1e-9);
    let l = &ps[3].subpaths[0];
    assert_pt(u(l.start), -5.0, 0.0, 1e-9);
    assert_pt(u(end_point(l)), -6.0, 0.0, 1e-9);
}

#[test]
fn spline_exact_bezier_endpoints_and_tangents() {
    // Single cubic span: Bézier control points == spline control points.
    let doc = Dxf::new()
        .units(1)
        .ent(
            "SPLINE",
            "8=0 70=8 71=3 72=8 73=4 74=0 40=0 40=0 40=0 40=0 40=1 40=1 40=1 40=1 \
             10=0 20=0 30=0 10=10 20=20 30=0 10=30 20=20 30=0 10=40 20=0 30=0",
        )
        // Two spans with an interior knot.
        .ent(
            "SPLINE",
            "8=0 70=8 71=3 72=9 73=5 74=0 40=0 40=0 40=0 40=0 40=0.5 40=1 40=1 40=1 40=1 \
             10=0 20=0 30=0 10=10 20=30 30=0 10=30 20=30 30=0 10=40 20=0 30=0 10=60 20=10 30=0",
        )
        .parse();
    let ps = paths(&doc);
    let origin = ps[0].subpaths[0].start; // world (0,0)
    let u = |q: Point| Point::new((q.x - origin.x) / 72.0, (q.y - origin.y) / 72.0);
    let s1 = &ps[0].subpaths[0];
    assert_eq!(s1.segments.len(), 1);
    match &s1.segments[0] {
        Segment::Curve(a, b, c) => {
            assert_pt(u(*a), 10.0, 20.0, 1e-9);
            assert_pt(u(*b), 30.0, 20.0, 1e-9);
            assert_pt(u(*c), 40.0, 0.0, 1e-9);
        }
        s => panic!("{s:?}"),
    }
    let s2 = &ps[1].subpaths[0];
    assert_eq!(s2.segments.len(), 2);
    assert_pt(u(s2.start), 0.0, 0.0, 1e-9);
    assert_pt(u(end_point(s2)), 60.0, 10.0, 1e-9);
    // Clamped: first Bézier tangent along P1 − P0, last along P4 − P3.
    let (Segment::Curve(c1, _, _), Segment::Curve(_, c2, _)) = (&s2.segments[0], &s2.segments[1]) else { panic!() };
    let (t0, t1) = (u(*c1), u(*c2));
    assert!(close(t0.x * 30.0 - t0.y * 10.0, 0.0, 1e-6), "start tangent {t0:?}");
    assert!(close((t1.x - 60.0) * 10.0 - (t1.y - 10.0) * 20.0, 0.0, 1e-6), "end tangent {t1:?}");
    // C2 continuity at the joint: knot 0.5 point from de Boor.
    // P(0.5) for this knot vector = 0.25·P1 + 0.5·P2 + 0.25·P3 = (27.5, 22.5).
    match &s2.segments[0] {
        Segment::Curve(_, _, j) => assert_pt(u(*j), 27.5, 22.5, 1e-9),
        s => panic!("{s:?}"),
    }
}

#[test]
fn rational_and_fit_point_splines_are_approximated() {
    let w = std::f64::consts::FRAC_1_SQRT_2;
    let doc = Dxf::new()
        .units(1)
        .ent(
            "SPLINE",
            &format!(
                "8=0 70=12 71=2 72=6 73=3 74=0 40=0 40=0 40=0 40=1 40=1 40=1 \
                 10=10 20=0 30=0 10=10 20=10 30=0 10=0 20=10 30=0 41=1 41={w} 41=1"
            ),
        )
        .ent("SPLINE", "8=0 70=8 71=3 72=0 73=0 74=3 11=20 21=0 31=0 11=25 21=5 31=0 11=30 21=0 31=0")
        .ent("LINE", &line("", 0.0, 0.0, 0.0, 0.0))
        .parse();
    let ps = paths(&doc);
    let origin = ps[2].subpaths[0].start;
    let u = |q: Point| Point::new((q.x - origin.x) / 72.0, (q.y - origin.y) / 72.0);
    // Rational quadratic quarter circle of radius 10.
    for q in samples(&ps[0].subpaths[0]).into_iter().map(u) {
        assert!(close((q.x * q.x + q.y * q.y).sqrt(), 10.0, 1e-3), "{q:?}");
    }
    let fit = &ps[1].subpaths[0];
    assert_pt(u(fit.start), 20.0, 0.0, 1e-9);
    assert_pt(u(end_point(fit)), 30.0, 0.0, 1e-9);
    assert!(samples(fit).into_iter().map(u).any(|q| close(q.x, 25.0, 1e-9) && close(q.y, 5.0, 1e-9)));
    assert!(doc.warnings.iter().any(|w| w.contains("splines")));
}

#[test]
fn solid_vertex_order_and_3dface() {
    let doc = Dxf::new()
        .units(1)
        .ent("SOLID", "8=0 62=1 10=0 20=0 30=0 11=10 21=0 31=0 12=0 22=10 32=0 13=10 23=10 33=0")
        .ent("3DFACE", "8=0 10=20 20=0 30=0 11=30 21=0 31=0 12=30 22=10 32=0 13=20 23=10 33=0 70=0")
        .ent("3DFACE", "8=0 10=40 20=0 30=0 11=50 21=0 31=0 12=50 22=10 32=0 13=40 23=10 33=0 70=5")
        .parse();
    let ps = paths(&doc);
    let s = &ps[0];
    assert!(s.stroke.is_none());
    assert_eq!(rgb_of(s.fill.as_ref().unwrap()), (1.0, 0.0, 0.0));
    // 1-2-4-3 → (0,0) (10,0) (10,10) (0,10): a proper (non-bow-tie) square.
    let pts: Vec<Point> = s.subpaths[0].points().collect();
    assert_eq!(pts.len(), 4);
    assert_pt(pts[2], 720.0, 720.0, 1e-9);
    assert_pt(pts[3], 0.0, 720.0, 1e-9);
    assert!(ps[1].subpaths[0].closed);
    // Edges 1 and 3 invisible (70 = 5): two open edges remain.
    assert_eq!(ps[2].subpaths.len(), 2);
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

#[test]
fn colours_aci_truecolor_bylayer_byblock() {
    let doc = Dxf::new()
        .units(4)
        .layer("2=GREEN 70=0 62=3 6=CONTINUOUS")
        .layer("2=TC 70=0 62=1 420=1193046 6=CONTINUOUS")
        .block("B", (0.0, 0.0), &[("LINE", &line("62=0", 0.0, 0.0, 1.0, 0.0)), ("LINE", &line("", 0.0, 1.0, 1.0, 1.0))])
        .ent("LINE", &line("62=1", 0.0, 0.0, 1.0, 0.0))
        .ent("LINE", "8=GREEN 10=0 20=1 30=0 11=1 21=1 31=0")
        .ent("LINE", &line("62=7", 0.0, 2.0, 1.0, 2.0))
        .ent("LINE", &line("62=1 420=16744448", 0.0, 3.0, 1.0, 3.0))
        .ent("LINE", "8=TC 10=0 20=4 30=0 11=1 21=4 31=0")
        .ent("INSERT", "8=GREEN 62=5 2=B 10=0 20=10 30=0")
        .parse();
    // Groups in layer-table order: GREEN, TC, then "0" (not in the table).
    assert_eq!(doc.elements.len(), 3);
    let groups: Vec<Vec<&PathItem>> =
        doc.elements.iter().map(|e| all_paths(std::slice::from_ref(e))).collect();
    let green = &groups[0];
    assert_eq!(stroke_rgb(green[0]), (0.0, 1.0, 0.0));
    // INSERT on GREEN with colour 5: BYBLOCK child → blue, BYLAYER child on
    // layer 0 → inherits the insert's layer → green.
    assert_eq!(stroke_rgb(green[1]), (0.0, 0.0, 1.0));
    assert_eq!(stroke_rgb(green[2]), (0.0, 1.0, 0.0));
    let tc = &groups[1];
    let (r, g, b) = stroke_rgb(tc[0]);
    assert!(close(r, 0x12 as f64 / 255.0, 1e-9) && close(g, 0x34 as f64 / 255.0, 1e-9) && close(b, 0x56 as f64 / 255.0, 1e-9));
    let zero = &groups[2];
    assert_eq!(stroke_rgb(zero[0]), (1.0, 0.0, 0.0));
    assert_eq!(stroke_rgb(zero[1]), (0.0, 0.0, 0.0), "colour 7 prints black");
    let (r, g, b) = stroke_rgb(zero[2]);
    assert!(close(r, 1.0, 1e-9) && close(g, 128.0 / 255.0, 1e-9) && close(b, 0.0, 1e-9), "true colour wins");
}

#[test]
fn lineweights() {
    let doc = Dxf::new()
        .units(4)
        .var("9=$LWDEFAULT 70=35")
        .layer("2=THICK 70=0 62=7 6=CONTINUOUS 370=100")
        .ent("LINE", &line("370=50", 0.0, 0.0, 1.0, 0.0))
        .ent("LINE", "8=THICK 10=0 20=1 30=0 11=1 21=1 31=0")
        .ent("LINE", &line("", 0.0, 2.0, 1.0, 2.0))
        .ent("LINE", &line("370=-3", 0.0, 3.0, 1.0, 3.0))
        .parse();
    let w: Vec<f64> = paths(&doc).iter().map(|p| p.stroke.as_ref().unwrap().width).collect();
    // THICK group first (layer order), then layer 0.
    assert!(close(w[0], 1.0 * MM, 1e-9), "{w:?}");
    assert!(close(w[1], 0.5 * MM, 1e-9));
    assert!(close(w[2], 0.35 * MM, 1e-9), "unknown layer 0 → $LWDEFAULT");
    assert!(close(w[3], 0.35 * MM, 1e-9));
}

#[test]
fn linetypes_scaled() {
    let doc = Dxf::new()
        .units(4)
        .var("9=$LTSCALE 40=2")
        .ltype("2=DASHED 70=0 3=x 72=65 73=2 40=0.75 49=0.5 74=0 49=-0.25 74=0")
        .ltype("2=DOTGAP 70=0 3=x 72=65 73=2 40=0.5 49=-0.25 74=0 49=0 74=0 ")
        .layer("2=D 70=0 62=7 6=DASHED")
        .ent("LINE", "8=D 10=0 20=0 30=0 11=10 21=0 31=0")
        .ent("LINE", "8=D 48=3 10=0 20=1 30=0 11=10 21=1 31=0")
        .ent("LINE", "8=D 6=CONTINUOUS 10=0 20=2 30=0 11=10 21=2 31=0")
        .ent("LINE", "8=D 6=DOTGAP 10=0 20=3 30=0 11=10 21=3 31=0")
        .parse();
    let s: Vec<&Stroke> = paths(&doc).iter().map(|p| p.stroke.as_ref().unwrap()).collect();
    let approx = |a: &[f64], b: &[f64]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| close(*x, *y, 1e-9));
    assert!(approx(&s[0].dash, &[1.0 * MM, 0.5 * MM]), "{:?}", s[0].dash);
    assert!(approx(&s[1].dash, &[3.0 * MM, 1.5 * MM]), "{:?}", s[1].dash);
    assert!(s[2].dash.is_empty());
    // Gap-first dotted pattern: dot then gap, phase into the gap.
    assert!(approx(&s[3].dash, &[0.0, 0.5 * MM]), "{:?}", s[3].dash);
    assert!(close(s[3].dash_phase, 0.0, 1e-9) || close(s[3].dash_phase, 0.0, 1e-9));
    assert_eq!(s[3].cap, LineCap::Round);
}

#[test]
fn polyline_width_becomes_stroke_width() {
    let doc = Dxf::new()
        .units(4)
        .ent("LWPOLYLINE", "8=0 90=2 70=0 43=2 10=0 20=0 10=10 20=0")
        .ent("LWPOLYLINE", "8=0 90=2 70=0 10=0 20=5 40=1 41=3 10=10 20=5")
        .parse();
    let ps = paths(&doc);
    let s0 = ps[0].stroke.as_ref().unwrap();
    assert!(close(s0.width, 2.0 * MM, 1e-9));
    assert_eq!(s0.cap, LineCap::Butt);
    assert!(doc.warnings.iter().any(|w| w.contains("varying width")));
}

// ---------------------------------------------------------------------------
// Layers, spaces, warnings
// ---------------------------------------------------------------------------

#[test]
fn layers_off_frozen_and_groups() {
    let doc = parse_dxf(&fixture("r12_basic.dxf")).unwrap();
    // Layers 0 and WALLS have geometry; OFFLAYER (off) and FROZEN are skipped.
    assert_eq!(doc.elements.len(), 2);
    let b = doc.content_bounds().unwrap();
    assert!(b.urx - b.llx < 200.0 * MM, "off/frozen far-away lines must be skipped: {b:?}");
    assert!(doc.warnings.iter().any(|w| w.contains("1 text object")), "{:?}", doc.warnings);
    assert!(doc.warnings.iter().any(|w| w.contains("no units")));
    // WALLS: red, dashed (layer linetype), CIRCLE r 20 mm.
    let walls = all_paths(std::slice::from_ref(&doc.elements[1]));
    assert_eq!(walls.len(), 1);
    assert_eq!(stroke_rgb(walls[0]), (1.0, 0.0, 0.0));
    assert_eq!(walls[0].stroke.as_ref().unwrap().dash.len(), 2);
    ai8_roundtrip(&doc);
}

#[test]
fn insert_on_off_layer_keeps_contents_on_other_layers() {
    let doc = Dxf::new()
        .units(4)
        .layer("2=OFF 70=0 62=-1 6=CONTINUOUS")
        .layer("2=VIS 70=0 62=1 6=CONTINUOUS")
        .layer("2=FRZ 70=1 62=1 6=CONTINUOUS")
        .block("B", (0.0, 0.0), &[("LINE", &line("", 0.0, 0.0, 1.0, 0.0)), ("LINE", "8=VIS 10=0 20=1 30=0 11=1 21=1 31=0")])
        .ent("INSERT", "8=OFF 2=B 10=0 20=0 30=0")
        .ent("INSERT", "8=FRZ 2=B 10=0 20=0 30=0")
        .ent("LINE", &line("", 0.0, 5.0, 1.0, 5.0))
        .parse();
    // Only the VIS line from the first insert plus the plain line.
    assert_eq!(paths(&doc).len(), 2);
}

#[test]
fn paperspace_fallback() {
    let doc = Dxf::new().units(4).ent("LINE", &line("67=1", 0.0, 0.0, 5.0, 0.0)).parse();
    assert_eq!(paths(&doc).len(), 1);
    assert!(doc.warnings.iter().any(|w| w.contains("paper space")));
    let doc = Dxf::new()
        .units(4)
        .ent("LINE", &line("67=1", 0.0, 0.0, 500.0, 0.0))
        .ent("LINE", &line("", 0.0, 0.0, 5.0, 0.0))
        .parse();
    assert_eq!(paths(&doc).len(), 1);
    assert!(close(doc.page.width(), 5.0 * MM, 1e-9));
}

#[test]
fn unsupported_entities_warn() {
    let doc = Dxf::new()
        .units(4)
        .ent("LINE", &line("", 0.0, 0.0, 5.0, 0.0))
        .ent("MTEXT", "8=0 10=0 20=0 30=0 1=x")
        .ent("TEXT", "8=0 10=0 20=0 30=0 1=x")
        .ent("LEADER", "8=0 10=0 20=0 30=0")
        .ent("IMAGE", "8=0 10=0 20=0 30=0")
        .ent("XLINE", "8=0 10=0 20=0 30=0")
        .ent("FROBNICATOR", "8=0")
        .ent("VIEWPORT", "8=0 10=0 20=0 30=0")
        .parse();
    let w = doc.warnings.join("\n");
    assert!(w.contains("2 text object"), "{w}");
    assert!(w.contains("leader"));
    assert!(w.contains("raster image"));
    assert!(w.contains("XLINE"));
    assert!(w.contains("FROBNICATOR"));
    assert!(!w.contains("VIEWPORT"));
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

#[test]
fn insert_rotation_scale_base_and_array() {
    let doc = Dxf::new()
        .units(1)
        .block("B", (1.0, 0.0), &[("LINE", &line("", 1.0, 0.0, 11.0, 0.0))])
        .ent("LINE", &line("", 0.0, 0.0, 0.0, 0.0))
        // Rotated 90°, scaled 2: block line (base-relative 0..10 along x) → (100,100)..(100,120).
        .ent("INSERT", "8=0 2=B 10=100 20=100 30=0 41=2 42=2 43=1 50=90")
        // 2 columns × 3 rows, spacing 30 × 40, rotated 90°: columns go +y, rows go −x.
        .ent("INSERT", "8=0 2=B 10=200 20=0 30=0 50=90 70=2 71=3 44=30 45=40")
        // Mirrored (negative x scale).
        .ent("INSERT", "8=0 2=B 10=-50 20=0 30=0 41=-1")
        .parse();
    let ps = paths(&doc);
    let origin = ps[0].subpaths[0].start;
    let u = |q: Point| Point::new((q.x - origin.x) / 72.0, (q.y - origin.y) / 72.0);
    assert_pt(u(ps[1].subpaths[0].start), 100.0, 100.0, 1e-9);
    assert_pt(u(end_point(&ps[1].subpaths[0])), 100.0, 120.0, 1e-9);
    let arr: Vec<Point> = ps[2..8].iter().map(|p| u(p.subpaths[0].start)).collect();
    for (row, col) in [(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1)] {
        let (x, y) = (200.0 - 40.0 * row as f64, 30.0 * col as f64);
        assert!(arr.iter().any(|p| close(p.x, x, 1e-9) && close(p.y, y, 1e-9)), "missing ({x},{y}) in {arr:?}");
    }
    assert_pt(u(end_point(&ps[8].subpaths[0])), -60.0, 0.0, 1e-9);
    // One group per INSERT.
    let groups = match &doc.elements[0] {
        Element::Group(c) => c.iter().filter(|e| matches!(e, Element::Group(_))).count(),
        _ => 0,
    };
    assert_eq!(groups, 3);
}

#[test]
fn nested_blocks_and_recursion_guard() {
    let doc = Dxf::new()
        .units(1)
        .block("INNER", (0.0, 0.0), &[("CIRCLE", "8=0 10=0 20=0 30=0 40=1")])
        .block("OUTER", (0.0, 0.0), &[("INSERT", "8=0 2=INNER 10=10 20=0 30=0 41=2 42=2")])
        .block("LOOP_A", (0.0, 0.0), &[("INSERT", "8=0 2=LOOP_B 10=0 20=0 30=0"), ("LINE", &line("", 0.0, 0.0, 1.0, 0.0))])
        .block("LOOP_B", (0.0, 0.0), &[("INSERT", "8=0 2=LOOP_A 10=0 20=0 30=0")])
        .ent("INSERT", "8=0 2=OUTER 10=100 20=0 30=0 50=90")
        .ent("INSERT", "8=0 2=LOOP_A 10=0 20=0 30=0")
        .ent("INSERT", "8=0 2=MISSING 10=0 20=0 30=0")
        .parse();
    let ps = paths(&doc);
    // Circle: OUTER rotated 90° at (100,0): INNER at (10,0)→(100,10), radius 2.
    let c = &ps[0].subpaths[0];
    let b = c.points().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
    assert!(close((b.1 - b.0) / 72.0, 4.0, 1e-6));
    assert!(doc.warnings.iter().any(|w| w.contains("inserts itself")));
    assert!(doc.warnings.iter().any(|w| w.contains("not defined")));
    // LOOP_A's own line is drawn once (twice nested before the cycle is cut).
    assert!(ps.len() >= 2);
}

#[test]
fn dimension_renders_anonymous_block() {
    let doc = Dxf::new()
        .units(4)
        .block("*D1", (0.0, 0.0), &[("LINE", &line("", 0.0, 0.0, 10.0, 0.0)), ("SOLID", "8=0 10=0 20=0 30=0 11=1 21=0.2 31=0 12=1 22=-0.2 32=0"), ("MTEXT", "8=0 10=5 20=1 30=0 1=10")])
        .ent("DIMENSION", "8=0 2=*D1 10=0 20=0 30=0 70=32")
        .parse();
    assert_eq!(paths(&doc).len(), 2);
    assert!(doc.warnings.iter().any(|w| w.contains("1 text")));
}

// ---------------------------------------------------------------------------
// Hatches
// ---------------------------------------------------------------------------

fn find_fill(doc: &Document, pred: impl Fn(&Color) -> bool) -> Vec<&PathItem> {
    paths(doc).into_iter().filter(|p| p.fill.as_ref().is_some_and(&pred)).collect()
}

#[test]
fn fixture_r2000_everything() {
    let doc = parse_dxf(&fixture("r2000_full.dxf")).unwrap();
    // Layers with output: 0, Outlines, Hatches, Blocks, Dashed, TrueColor.
    assert_eq!(doc.elements.len(), 6, "{:?}", doc.warnings);
    let w = doc.warnings.join("\n");
    assert!(w.contains("1 text object"), "{w}");
    assert!(w.contains("leader"));
    assert!(w.contains("expanded"), "irregular hatch must be expanded: {w}");
    assert!(!w.contains("no units"));
    // mm drawing: hidden (y=1000) / frozen (y=-1000) / paper space lines skipped.
    assert!(doc.page.height() < 400.0 * MM && doc.page.width() < 500.0 * MM, "{:?}", doc.page);

    // Solid hatch with a hole: two loops, even-odd, red.
    let solid = find_fill(&doc, |c| matches!(c, Color::Rgb(r, g, b) if *r == 1.0 && *g == 0.0 && *b == 0.0));
    assert_eq!(solid.len(), 1);
    assert_eq!(solid[0].subpaths.len(), 2);
    assert!(solid[0].even_odd);

    // Patterns: ANSI31, ANSI37 and BRICK are native.
    let pats = find_fill(&doc, |c| matches!(c, Color::Pattern(_)));
    assert_eq!(pats.len(), 3);
    let get = |name: &str| {
        pats.iter()
            .find_map(|p| match p.fill.as_ref() {
                Some(Color::Pattern(pp)) if pp.pattern.name == name => Some(pp.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("pattern {name}"))
    };
    let ansi31 = get("ANSI31");
    let page_area = |pp: &PatternPaint| pp.pattern.x_step * pp.pattern.y_step * (pp.matrix.a * pp.matrix.d - pp.matrix.b * pp.matrix.c).abs();
    let sp = 3.175 * MM;
    assert!(close(page_area(&ansi31), sp * sp, 1e-6 * sp * sp), "ANSI31 cell area {}", page_area(&ansi31));
    // The tile's lines run at 45° on the page.
    let line_dirs = |pp: &PatternPaint| -> Vec<f64> {
        all_paths(&pp.pattern.elements)
            .iter()
            .flat_map(|p| p.subpaths.iter())
            .map(|s| {
                let (a, b) = (pp.matrix.apply(s.start), pp.matrix.apply(end_point(s)));
                let ang = (b.y - a.y).atan2(b.x - a.x).to_degrees().rem_euclid(180.0);
                (ang * 1e6).round() / 1e6
            })
            .collect()
    };
    assert!(line_dirs(&ansi31).iter().all(|a| close(*a, 45.0, 1e-6)));
    let ansi37 = get("ANSI37");
    let dirs = line_dirs(&ansi37);
    assert!(dirs.iter().any(|a| close(*a, 45.0, 1e-6)) && dirs.iter().any(|a| close(*a, 135.0, 1e-6)));
    assert!(dirs.iter().all(|a| close(*a, 45.0, 1e-6) || close(*a, 135.0, 1e-6)));
    // Tile stroke maps to the hatch lineweight (layer Hatches: 0.18 mm).
    let s = all_paths(&ansi37.pattern.elements)[0].stroke.as_ref().unwrap().width * ansi37.matrix.mean_scale();
    assert!(close(s, 0.18 * MM, 1e-9), "{s}");
    let brick = get("BRICK");
    assert!(close(page_area(&brick), 2.0 * 6.35 * 6.35 * MM * MM, 1e-6), "BRICK cell {}", page_area(&brick));
    // Slot boundary: the clockwise arc edge reaches x = 140 (left), the CCW one x = 210.
    let slot = pats.iter().find(|p| matches!(p.fill.as_ref(), Some(Color::Pattern(pp)) if pp.pattern.name == "BRICK")).unwrap();
    let xs: Vec<f64> = slot.subpaths.iter().flat_map(samples).map(|p| p.x).collect();
    let (xmin, xmax) = xs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| (a.min(*x), b.max(*x)));
    assert!(close((xmax - xmin) / MM, 70.0, 0.05), "slot width {}", (xmax - xmin) / MM);

    // Expanded irregular hatch: clip group with dashed lines.
    let mut clip_groups = 0;
    fn walk(els: &[Element], n: &mut usize) {
        for e in els {
            match e {
                Element::ClipGroup { children, even_odd, .. } => {
                    assert!(*even_odd);
                    assert!(!all_paths(children).is_empty());
                    *n += 1;
                }
                Element::Group(c) => walk(c, n),
                _ => {}
            }
        }
    }
    walk(&doc.elements, &mut clip_groups);
    assert_eq!(clip_groups, 1);

    // Gradients: linear blue → yellow and spherical (radial).
    let grads: Vec<&GradientPaint> = paths(&doc)
        .into_iter()
        .filter_map(|p| match p.fill.as_ref() {
            Some(Color::Gradient(g)) => Some(g),
            _ => None,
        })
        .collect();
    assert_eq!(grads.len(), 2);
    let lin = grads.iter().find(|g| g.gradient.kind == GradientKind::Linear).unwrap();
    assert_eq!(rgb_of(&lin.gradient.stops[0].color), (0.0, 0.0, 1.0));
    assert_eq!(rgb_of(&lin.gradient.stops[1].color), (1.0, 1.0, 0.0));
    let (a, b) = (lin.matrix.apply(lin.start), lin.matrix.apply(lin.end));
    assert!(close(b.x - a.x, 60.0 * MM, 1e-6) && close(b.y - a.y, 0.0, 1e-9));
    let rad = grads.iter().find(|g| g.gradient.kind == GradientKind::Radial).unwrap();
    assert_eq!(rgb_of(&rad.gradient.stops[0].color), (1.0, 1.0, 1.0), "centre = second colour (true colour white)");
}

#[test]
fn hatch_edge_types_and_islands() {
    // Outer circle (full arc edge) with an inner ellipse edge island and a
    // spline-edged loop.
    let body = pairs(
        "8=0 62=3 100=AcDbHatch 10=0 20=0 30=0 210=0 220=0 230=1 2=SOLID 70=1 71=0 91=3 \
         92=1 93=1 72=2 10=0 20=0 40=10 50=0 51=360 73=1 97=0 \
         92=0 93=1 72=3 10=0 20=0 11=4 21=0 40=0.5 50=0 51=360 73=1 97=0 \
         92=0 93=2 72=4 94=3 73=0 74=0 95=8 96=4 40=0 40=0 40=0 40=0 40=1 40=1 40=1 40=1 \
         10=20 20=0 10=20 20=5 10=30 20=5 10=30 20=0 72=1 10=30 20=0 11=20 21=0 97=0 \
         75=0 76=1 98=0",
    );
    let doc = Dxf::new().units(1).raw_ent("HATCH", &body).parse();
    let p = paths(&doc)[0];
    assert_eq!(p.subpaths.len(), 3);
    assert!(p.even_odd);
    let origin = Point::new(-10.0, -10.0);
    let u = |q: Point| Point::new(q.x / 72.0 + origin.x, q.y / 72.0 + origin.y);
    for q in samples(&p.subpaths[0]).into_iter().map(u) {
        assert!(close((q.x * q.x + q.y * q.y).sqrt(), 10.0, 3e-3));
    }
    for q in samples(&p.subpaths[1]).into_iter().map(u) {
        assert!(close((q.x / 4.0).powi(2) + (q.y / 2.0).powi(2), 1.0, 2e-3), "{q:?}");
    }
    let s = &p.subpaths[2];
    assert_pt(u(s.start), 20.0, 0.0, 1e-9);
    assert!(s.closed);
}

#[test]
fn hatch_with_polyline_bulge_boundary_and_pattern_scale() {
    // Circle-ish polyline boundary (two bulge-1 arcs) with ANSI31 at scale 2.
    let s = 2.0 * 3.175 / 2f64.sqrt();
    let body = pairs(&format!(
        "8=0 100=AcDbHatch 10=0 20=0 30=0 210=0 220=0 230=1 2=ANSI31 70=0 71=0 91=1 \
         92=3 72=1 73=1 93=2 10=0 20=0 42=1 10=10 20=0 42=1 97=0 \
         75=0 76=1 52=0 41=2 77=0 78=1 53=45 43=0 44=0 45=-{s} 46={s} 79=0 98=0"
    ));
    let doc = Dxf::new().units(4).raw_ent("HATCH", &body).parse();
    let p = paths(&doc)[0];
    let Some(Color::Pattern(pp)) = &p.fill else { panic!("{:?}", p.fill) };
    let area = pp.pattern.x_step * pp.pattern.y_step * (pp.matrix.a * pp.matrix.d - pp.matrix.b * pp.matrix.c).abs();
    let sp = 6.35 * MM;
    assert!(close(area, sp * sp, 1e-6 * sp * sp));
    // Boundary = circle of radius 5 mm.
    let c = Point::new(5.0 * MM, 5.0 * MM);
    for q in samples(&p.subpaths[0]) {
        assert!(close(((q.x - c.x).powi(2) + (q.y - c.y).powi(2)).sqrt(), 5.0 * MM, 5e-3 * MM));
    }
}

#[test]
fn hatch_patterns_are_shared_between_identical_hatches() {
    let s = 3.175 / 2f64.sqrt();
    let h = |x: f64| {
        pairs(&format!(
            "8=0 100=AcDbHatch 10=0 20=0 30=0 210=0 220=0 230=1 2=ANSI31 70=0 71=0 91=1 \
             92=3 72=0 73=1 93=4 10={x} 20=0 10={} 20=0 10={} 20=10 10={x} 20=10 97=0 \
             75=0 76=1 52=0 41=1 77=0 78=1 53=45 43=0 44=0 45=-{s} 46={s} 79=0 98=0",
            x + 10.0,
            x + 10.0
        ))
    };
    let doc = Dxf::new().units(4).raw_ent("HATCH", &h(0.0)).raw_ent("HATCH", &h(20.0)).parse();
    let pats: Vec<&PatternPaint> = paths(&doc)
        .into_iter()
        .filter_map(|p| match &p.fill {
            Some(Color::Pattern(pp)) => Some(pp),
            _ => None,
        })
        .collect();
    assert_eq!(pats.len(), 2);
    assert!(std::sync::Arc::ptr_eq(&pats[0].pattern, &pats[1].pattern));
    let stats = ai8_roundtrip(&doc);
    assert_eq!(stats.pattern_defs.len(), 1);
    // Same pattern and matrix: the writer may set the paint only once.
    assert!(stats.pattern_uses >= 1);
}

// ---------------------------------------------------------------------------
// Binary DXF (written by the independent `dxf` crate)
// ---------------------------------------------------------------------------

fn crate_drawing(version: dxf::enums::AcadVersion) -> dxf::Drawing {
    use dxf::entities::*;
    let mut d = dxf::Drawing::new();
    d.header.version = version;
    d.header.default_drawing_units = dxf::enums::Units::Millimeters;
    let mut l = Entity::new(EntityType::Line(Line::new(dxf::Point::new(0.0, 0.0, 0.0), dxf::Point::new(100.0, 50.0, 0.0))));
    l.common.color = dxf::Color::from_index(1);
    d.add_entity(l);
    d.add_entity(Entity::new(EntityType::Circle(Circle::new(dxf::Point::new(50.0, 25.0, 0.0), 10.0))));
    d.add_entity(Entity::new(EntityType::Arc(Arc::new(dxf::Point::new(0.0, 0.0, 0.0), 20.0, 0.0, 90.0))));
    d
}

#[test]
fn binary_dxf_matches_ascii() {
    for version in [dxf::enums::AcadVersion::R12, dxf::enums::AcadVersion::R2000, dxf::enums::AcadVersion::R2013] {
        let d = crate_drawing(version);
        let mut bin = Vec::new();
        d.save_binary(&mut bin).expect("save binary");
        let mut ascii = Vec::new();
        d.save(&mut ascii).expect("save ascii");
        assert!(bin.starts_with(b"AutoCAD Binary DXF\r\n\x1a\0"));
        assert!(looks_like_dxf(&bin) && looks_like_dxf(&ascii));
        let a = parse_dxf(&ascii).expect("ascii");
        let b = parse_dxf(&bin).unwrap_or_else(|e| panic!("binary {version:?}: {e:#}"));
        assert_eq!(paths(&a).len(), 3, "{version:?}");
        assert_eq!(a.elements, b.elements, "{version:?}");
        assert_eq!(a.page, b.page);
        assert_eq!(stroke_rgb(paths(&b)[0]), (1.0, 0.0, 0.0));
        ai8_roundtrip(&b);
    }
}

// ---------------------------------------------------------------------------
// End to end
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_ai8() {
    let doc = parse_dxf(&fixture("r2000_full.dxf")).unwrap();
    let stats = ai8_roundtrip(&doc);
    assert!(stats.paths >= 20, "{stats:?}");
    assert_eq!(stats.pattern_defs.len(), 3, "{stats:?}");
    assert_eq!(stats.gradient_defs.len(), 2, "{stats:?}");
    assert!(stats.groups >= 6);
    assert!(stats.clip_groups >= 1);
    let doc = parse_dxf(&fixture("r12_basic.dxf")).unwrap();
    ai8_roundtrip(&doc);
}
