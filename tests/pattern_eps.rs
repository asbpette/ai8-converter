//! End-to-end checks of PostScript patterns and shadings: EPS -> Ghostscript
//! (pdfwrite) -> PDF interpreter -> IR. Asserts on the IR (pattern tiles,
//! steps, matrices, gradient stops), not on the AI8 text. Tests skip (pass
//! with a message) when Ghostscript is not installed.

use ai8_converter::ConvertOptions;
use ai8_converter::convert::load_documents;
use ai8_converter::ghostscript;
use ai8_converter::ir::{Color, Document, Element, GradientKind, GradientPaint, Matrix, PathItem, PatternPaint, Point};
use std::path::Path;
use std::sync::Arc;

fn load(test: &str, fixture: &str) -> Option<Document> {
    if ghostscript::find().is_none() {
        eprintln!("SKIPPED {test}: Ghostscript not found (set AI8_GHOSTSCRIPT or install gs)");
        return None;
    }
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eps").join(fixture);
    let mut docs = load_documents(&p, &ConvertOptions::default()).unwrap();
    assert_eq!(docs.len(), 1);
    Some(docs.remove(0))
}

fn paths(els: &[Element]) -> Vec<&PathItem> {
    let mut v = Vec::new();
    for e in els {
        match e {
            Element::Path(p) => v.push(p),
            Element::Group(c) => v.extend(paths(c)),
            Element::ClipGroup { children, .. } => v.extend(paths(children)),
        }
    }
    v
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

fn matrix_approx(m: &Matrix, v: [f64; 6]) -> bool {
    [m.a, m.b, m.c, m.d, m.e, m.f].iter().zip(v).all(|(a, b)| approx(*a, b))
}

fn pattern(c: Option<&Color>) -> &PatternPaint {
    match c {
        Some(Color::Pattern(p)) => p,
        other => panic!("expected a pattern paint, got {other:?}"),
    }
}

fn gradient(c: Option<&Color>) -> &GradientPaint {
    match c {
        Some(Color::Gradient(g)) => g,
        other => panic!("expected a gradient paint, got {other:?}"),
    }
}

fn no_pattern_warnings(d: &Document) {
    assert!(
        !d.warnings.iter().any(|w| w.contains("pattern") || w.contains("gradient") || w.contains("shading")),
        "{:?}",
        d.warnings
    );
}

#[test]
fn coloured_tiling_pattern() {
    let Some(d) = load("coloured_tiling_pattern", "pattern_colored.eps") else { return };
    no_pattern_warnings(&d);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 1, "{:?}", d.elements);
    let pp = pattern(p[0].fill.as_ref());
    let pat = &pp.pattern;
    assert_eq!((pat.bbox.llx, pat.bbox.lly, pat.bbox.urx, pat.bbox.ury), (0.0, 0.0, 20.0, 20.0));
    assert_eq!((pat.x_step, pat.y_step), (20.0, 20.0));
    // Ghostscript writes [0.99998 0 0 0.99998 0 0]; the noise is snapped away.
    assert_eq!(pp.matrix, Matrix::IDENTITY);
    // Tile art in pattern space: red 10x10 square and a blue dot (r = 3) at
    // the centre; Ghostscript's redundant cell clip is dropped.
    let tile = paths(&pat.elements);
    assert_eq!(tile.len(), 2, "{:?}", pat.elements);
    assert!(matches!(pat.elements[0], Element::Path(_)));
    assert_eq!(tile[0].fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    let b = ai8_converter::ir::bounds_of(&pat.elements[..1]).unwrap();
    assert_eq!((b.llx, b.lly, b.urx, b.ury), (0.0, 0.0, 10.0, 10.0));
    assert_eq!(tile[1].fill, Some(Color::Rgb(0.0, 0.0, 1.0)));
    let b = ai8_converter::ir::bounds_of(&pat.elements[1..]).unwrap();
    assert!(approx(b.llx, 7.0) && approx(b.urx, 13.0) && approx(b.lly, 7.0) && approx(b.ury, 13.0), "{b:?}");
    assert_eq!(pat.representative_color(), Some(Color::Rgb(1.0, 0.0, 0.0)));
}

#[test]
fn uncoloured_pattern_one_definition_per_colour() {
    let Some(d) = load("uncoloured_pattern_one_definition_per_colour", "pattern_uncolored.eps") else { return };
    no_pattern_warnings(&d);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 3);
    let (a, b, c) = (pattern(p[0].fill.as_ref()), pattern(p[1].fill.as_ref()), pattern(p[2].fill.as_ref()));
    // Same colour -> the same shared definition; a different colour -> its own.
    assert!(Arc::ptr_eq(&a.pattern, &c.pattern));
    assert!(!Arc::ptr_eq(&a.pattern, &b.pattern));
    assert_ne!(a.pattern.name, b.pattern.name);
    for (pp, colour) in [(a, Color::Cmyk(0.0, 1.0, 0.0, 0.0)), (b, Color::Cmyk(1.0, 0.0, 0.0, 0.0))] {
        assert_eq!((pp.pattern.x_step, pp.pattern.y_step), (10.0, 10.0));
        assert_eq!(pp.matrix, Matrix::IDENTITY);
        let tile = paths(&pp.pattern.elements);
        assert_eq!(tile.len(), 1);
        assert_eq!(tile[0].fill, Some(colour));
        let pts: Vec<Point> = tile[0].subpaths[0].points().collect();
        assert_eq!(pts, vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0), Point::new(10.0, 5.0), Point::new(5.0, 0.0)]);
    }
}

#[test]
fn pattern_matrix_from_makepattern_ctm_and_stroke_use() {
    let Some(d) = load("pattern_matrix_from_makepattern_ctm_and_stroke_use", "pattern_matrix.eps") else { return };
    no_pattern_warnings(&d);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 2);
    let fill = pattern(p[0].fill.as_ref());
    // translate(50,50) x rotate(30) x scale(2): [2cos30 2sin30 -2sin30 2cos30 50 50].
    let (c, s) = (2.0 * 30f64.to_radians().cos(), 2.0 * 30f64.to_radians().sin());
    assert!(matrix_approx(&fill.matrix, [c, s, -s, c, 50.0, 50.0]), "{:?}", fill.matrix);
    // XStep/YStep differ from the 8x8 BBox (gaps between tiles).
    assert_eq!((fill.pattern.x_step, fill.pattern.y_step), (10.0, 12.0));
    assert_eq!((fill.pattern.bbox.urx, fill.pattern.bbox.ury), (8.0, 8.0));
    assert_eq!(paths(&fill.pattern.elements)[0].fill, Some(Color::Rgb(0.0, 0.5, 0.0)));
    // The stroke uses the very same definition and matrix.
    assert!(p[1].fill.is_none());
    let stroke = p[1].stroke.as_ref().unwrap();
    assert!(approx(stroke.width, 12.0));
    let sp = pattern(Some(&stroke.color));
    assert!(Arc::ptr_eq(&sp.pattern, &fill.pattern));
    assert_eq!(sp.matrix, fill.matrix);
}

#[test]
fn nested_patterns() {
    let Some(d) = load("nested_patterns", "pattern_nested.eps") else { return };
    no_pattern_warnings(&d);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 1);
    let outer = pattern(p[0].fill.as_ref());
    assert_eq!((outer.pattern.x_step, outer.pattern.y_step), (30.0, 30.0));
    let tile = paths(&outer.pattern.elements);
    assert_eq!(tile.len(), 2);
    let inner = pattern(tile[0].fill.as_ref());
    // The inner pattern lives in the outer pattern's space.
    assert_eq!(inner.matrix, Matrix::IDENTITY);
    assert_eq!((inner.pattern.x_step, inner.pattern.bbox.urx), (4.0, 4.0));
    assert_eq!(paths(&inner.pattern.elements)[0].fill, Some(Color::Cmyk(0.0, 0.0, 0.0, 1.0)));
    assert_eq!(tile[1].fill, Some(Color::Cmyk(1.0, 0.0, 0.0, 0.0)));
    assert_ne!(inner.pattern.name, outer.pattern.name);
}

#[test]
fn shfill_and_shading_patterns_become_gradients() {
    let Some(d) = load("shfill_and_shading_patterns_become_gradients", "pattern_shading.eps") else { return };
    no_pattern_warnings(&d);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 3, "{:?}", d.elements);

    // 1) shfill inside a rectangular clip: Ghostscript emits the clip plus a
    //    shading-pattern fill of the same rectangle -> one gradient path.
    assert!(matches!(d.elements[0], Element::Path(_)), "redundant clip should be dropped");
    let g = gradient(p[0].fill.as_ref());
    assert_eq!(g.gradient.kind, GradientKind::Linear);
    assert_eq!((g.start, g.end), (Point::new(10.0, 0.0), Point::new(140.0, 0.0)));
    assert_eq!(g.matrix, Matrix::IDENTITY);
    let stops: Vec<(f64, Color)> = g.gradient.stops.iter().map(|s| (s.offset, s.color.clone())).collect();
    assert_eq!(stops, vec![(0.0, Color::Rgb(1.0, 0.0, 0.0)), (1.0, Color::Rgb(0.0, 0.0, 1.0))]);
    let b = ai8_converter::ir::bounds_of(&d.elements[..1]).unwrap();
    assert_eq!((b.llx, b.lly, b.urx, b.ury), (10.0, 10.0, 140.0, 90.0));

    // 2) Radial CMYK shading pattern with a stitching function: the bound
    //    becomes a stop.
    let g = gradient(p[1].fill.as_ref());
    assert_eq!(g.gradient.kind, GradientKind::Radial);
    assert_eq!((g.start, g.end, g.radius), (Point::new(225.0, 50.0), Point::new(225.0, 50.0), 45.0));
    let stops: Vec<(f64, Color)> = g.gradient.stops.iter().map(|s| (s.offset, s.color.clone())).collect();
    assert_eq!(
        stops,
        vec![
            (0.0, Color::Cmyk(0.0, 0.0, 1.0, 0.0)),
            (0.5, Color::Cmyk(0.0, 1.0, 0.0, 0.0)),
            (1.0, Color::Cmyk(1.0, 0.0, 0.0, 0.0)),
        ]
    );

    // 3) Axial shading in a Separation space: spot-colour stops.
    let g = gradient(p[2].fill.as_ref());
    assert_eq!(g.gradient.kind, GradientKind::Linear);
    assert_eq!((g.start, g.end), (Point::new(10.0, 110.0), Point::new(10.0, 190.0)));
    assert_eq!(g.gradient.stops.len(), 2);
    for (s, tint) in g.gradient.stops.iter().zip([0.0, 1.0]) {
        let Color::Spot { name, cmyk, tint: t } = &s.color else { panic!("{:?}", s.color) };
        assert_eq!(name, "Gold");
        assert!(approx(*t, tint));
        assert!(approx(cmyk[0], 0.2) && approx(cmyk[1], 0.4) && approx(cmyk[2], 0.9) && approx(cmyk[3], 0.0), "{cmyk:?}");
    }
}
