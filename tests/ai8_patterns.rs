//! Native pattern / gradient output and pattern expansion.

mod common;

use ai8_converter::ai8::{ColorMode, WriteOptions, write_ai8};
use ai8_converter::ir::*;
use ai8_converter::patterns::{expand_patterns, flatten_gradients};
use std::sync::Arc;

fn rect(x: f64, y: f64, w: f64, h: f64) -> Subpath {
    let mut sp = Subpath::new(Point::new(x, y));
    sp.segments = vec![
        Segment::Line(Point::new(x + w, y)),
        Segment::Line(Point::new(x + w, y + h)),
        Segment::Line(Point::new(x, y + h)),
    ];
    sp.closed = true;
    sp
}

fn page(llx: f64, lly: f64) -> Document {
    Document::new(Rect { llx, lly, urx: llx + 400.0, ury: lly + 300.0 })
}

fn pattern(name: &str, bbox: Rect, step: (f64, f64), fill: Color) -> Arc<Pattern> {
    let mut p = PathItem::new(vec![rect(bbox.llx, bbox.lly, bbox.width() / 2.0, bbox.height() / 2.0)]);
    p.fill = Some(fill);
    Arc::new(Pattern { name: name.into(), bbox, x_step: step.0, y_step: step.1, elements: vec![Element::Path(p)] })
}

fn unit_pattern(name: &str) -> Arc<Pattern> {
    pattern(name, Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 }, (10.0, 10.0), Color::Cmyk(0.0, 1.0, 0.0, 0.0))
}

fn pfill(pattern: &Arc<Pattern>, matrix: Matrix) -> Color {
    Color::Pattern(PatternPaint { pattern: pattern.clone(), matrix })
}

fn filled_path(sp: Subpath, fill: Color) -> Element {
    let mut p = PathItem::new(vec![sp]);
    p.fill = Some(fill);
    Element::Path(p)
}

fn write(doc: &Document) -> String {
    write_ai8(doc, &WriteOptions::default())
}

fn lines_ending<'a>(ai: &'a str, op: &str) -> Vec<&'a str> {
    let suffix = format!(" {op}");
    ai.lines().filter(|l| l.ends_with(&suffix)).collect()
}

/// The 6 numbers of a `[a b c d tx ty] p` operator.
fn p_matrix(line: &str) -> Vec<f64> {
    let inner = &line[line.find('[').unwrap() + 1..line.find(']').unwrap()];
    inner.split_whitespace().map(|t| t.parse().unwrap()).collect()
}

fn art(ai: &str) -> &str {
    &ai[ai.find("%AI5_BeginLayer").unwrap()..]
}

fn setup(ai: &str) -> &str {
    &ai[ai.find("%%BeginSetup").unwrap()..ai.find("%%EndSetup").unwrap()]
}

fn assert_close(a: &[f64], b: &[f64]) {
    assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < 1e-3, "{a:?} vs {b:?}");
    }
}

// ---------------------------------------------------------------------------
// Native patterns
// ---------------------------------------------------------------------------

#[test]
fn pattern_defined_once_for_many_uses() {
    let pat = unit_pattern("Dots");
    // An equal copy behind a different Arc shares the definition too.
    let copy = Arc::new((*pat).clone());
    let mut doc = page(0.0, 0.0);
    for i in 0..5 {
        doc.elements.push(filled_path(rect(i as f64 * 50.0, 0.0, 40.0, 40.0), pfill(&pat, Matrix::IDENTITY)));
    }
    doc.elements.push(filled_path(rect(0.0, 100.0, 40.0, 40.0), pfill(&copy, Matrix::IDENTITY)));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.pattern_defs, vec!["Dots".to_string()]);
    assert_eq!(ai.matches("%AI3_BeginPattern:").count(), 1);
    // Same paint state is not repeated for consecutive paths.
    assert_eq!(stats.pattern_uses, 1);
    assert_eq!(stats.paths, 6);
    assert_eq!(stats.tile_paths, 1);
    assert!(setup(&ai).contains("%AI3_BeginPattern: (Dots)\n(Dots) 0 0 10 10 [\n%AI3_Tile\n"));
    assert!(setup(&ai).contains("] E\n%AI3_EndPattern\n"));
    // Patterns live in the first non-printing block, as Illustrator 8 writes them.
    assert!(setup(&ai).contains("%AI5_Begin_NonPrinting\nNp\n%AI3_BeginPattern:"));
}

#[test]
fn different_patterns_with_same_name_get_unique_names() {
    let a = unit_pattern("P1");
    let b = pattern("P1", Rect { llx: 0.0, lly: 0.0, urx: 20.0, ury: 20.0 }, (20.0, 20.0), Color::Gray(0.2));
    let c = pattern("", Rect { llx: 0.0, lly: 0.0, urx: 5.0, ury: 5.0 }, (5.0, 5.0), Color::Gray(0.7));
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 40.0, 40.0), pfill(&a, Matrix::IDENTITY)));
    doc.elements.push(filled_path(rect(50.0, 0.0, 40.0, 40.0), pfill(&b, Matrix::IDENTITY)));
    doc.elements.push(filled_path(rect(100.0, 0.0, 40.0, 40.0), pfill(&c, Matrix::IDENTITY)));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.pattern_defs, vec!["P1", "P1 2", "Pattern"]);
    let uses = lines_ending(art(&ai), "p");
    assert!(uses[0].starts_with("(P1) ") && uses[1].starts_with("(P1 2) ") && uses[2].starts_with("(Pattern) "));
}

#[test]
fn p_operand_values() {
    let pat = unit_pattern("T");
    let shifted = pattern("S", Rect { llx: 5.0, lly: 7.0, urx: 15.0, ury: 17.0 }, (10.0, 10.0), Color::Gray(0.0));
    let r90 = Matrix::new(0.0, 1.0, -1.0, 0.0, 30.0, 40.0);
    type Case = (Arc<Pattern>, Matrix, (f64, f64), [f64; 6]);
    let cases: Vec<Case> = vec![
        (pat.clone(), Matrix::IDENTITY, (0.0, 0.0), [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
        (pat.clone(), Matrix::translate(10.0, 20.0), (0.0, 0.0), [1.0, 0.0, 0.0, 1.0, 10.0, 20.0]),
        (pat.clone(), Matrix::scale(2.0, 3.0), (0.0, 0.0), [2.0, 0.0, 0.0, 3.0, 0.0, 0.0]),
        (pat.clone(), r90, (0.0, 0.0), [0.0, 1.0, -1.0, 0.0, 30.0, 40.0]),
        // Page offset is subtracted like for every coordinate.
        (pat.clone(), Matrix::translate(10.0, 20.0), (100.0, 50.0), [1.0, 0.0, 0.0, 1.0, -90.0, -30.0]),
        // Tile normalised to start at 0,0: the bbox origin moves into the matrix.
        (shifted.clone(), Matrix::IDENTITY, (0.0, 0.0), [1.0, 0.0, 0.0, 1.0, 5.0, 7.0]),
        (shifted.clone(), Matrix::scale(2.0, 2.0), (0.0, 0.0), [2.0, 0.0, 0.0, 2.0, 10.0, 14.0]),
        // r90 applied to (5,7) = (-7+30, 5+40), then page offset (-100,-50).
        (shifted, r90, (100.0, 50.0), [0.0, 1.0, -1.0, 0.0, 23.0 - 100.0, 45.0 - 50.0]),
    ];
    for (pat, m, (px, py), want) in cases {
        let mut doc = page(px, py);
        doc.elements.push(filled_path(rect(px, py, 50.0, 50.0), pfill(&pat, m)));
        let ai = write(&doc);
        common::validate_ai8(&ai);
        let uses = lines_ending(art(&ai), "p");
        assert_eq!(uses.len(), 1);
        assert!(uses[0].contains(" 0 0 1 1 0 0 0 0 0 ["), "{}", uses[0]);
        assert_close(&p_matrix(uses[0]), &want);
    }
}

#[test]
fn pattern_tile_art_is_normalised() {
    let shifted = pattern("S", Rect { llx: 5.0, lly: 7.0, urx: 15.0, ury: 17.0 }, (10.0, 10.0), Color::Gray(0.0));
    let mut doc = page(100.0, 50.0);
    doc.elements.push(filled_path(rect(100.0, 50.0, 50.0, 50.0), pfill(&shifted, Matrix::IDENTITY)));
    let ai = write(&doc);
    common::validate_ai8(&ai);
    let s = setup(&ai);
    assert!(s.contains("(S) 0 0 10 10 [\n"));
    // Art drawn at (5,7)-(10,12) in pattern space → (0,0)-(5,5) in the tile;
    // the page offset does not apply inside tiles.
    assert!(s.contains("0 0 m\n5 0 L\n5 5 L\n0 5 L\nf\n"), "{s}");
    assert!(s.contains("%AI6_BeginPatternLayer\n800 Ar\n"));
}

#[test]
fn pattern_stroke_uses_upper_case_p() {
    let pat = unit_pattern("Stripe");
    let mut p = PathItem::new(vec![rect(0.0, 0.0, 40.0, 40.0)]);
    p.stroke = Some(Stroke::solid(pfill(&pat, Matrix::IDENTITY), 4.0));
    let mut doc = page(0.0, 0.0);
    doc.elements.push(Element::Path(p));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.pattern_uses, 1);
    assert_eq!(lines_ending(art(&ai), "P").len(), 1);
    assert!(art(&ai).contains("(Stripe) 0 0 1 1 0 0 0 0 0 [1 0 0 1 0 0] P\n"));
}

#[test]
fn step_larger_than_bbox_defines_step_sized_tile() {
    let gap = pattern("Gap", Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 }, (15.0, 20.0), Color::Gray(0.0));
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 50.0, 50.0), pfill(&gap, Matrix::IDENTITY)));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert!(setup(&ai).contains("(Gap) 0 0 15 20 ["));
    assert_eq!(stats.tile_paths, 1);
}

#[test]
fn overlapping_steps_wrap_copies_into_the_tile() {
    // A 10x10 square repeated every 6: copies from the neighbouring cells
    // reach into the 6x6 tile.
    let mut sq = PathItem::new(vec![rect(0.0, 0.0, 10.0, 10.0)]);
    sq.fill = Some(Color::Gray(0.0));
    let pat = Arc::new(Pattern {
        name: "Overlap".into(),
        bbox: Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 },
        x_step: 6.0,
        y_step: 6.0,
        elements: vec![Element::Path(sq)],
    });
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 50.0, 50.0), pfill(&pat, Matrix::IDENTITY)));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert!(setup(&ai).contains("(Overlap) 0 0 6 6 ["));
    assert_eq!(stats.tile_paths, 4);
    assert!(setup(&ai).contains("-6 -6 m\n"));
}

#[test]
fn nested_paints_in_tiles_are_flattened() {
    let inner = unit_pattern("Inner");
    let grad = Arc::new(Gradient {
        kind: GradientKind::Linear,
        stops: vec![
            GradientStop { offset: 0.0, color: Color::Gray(0.0) },
            GradientStop { offset: 1.0, color: Color::Gray(1.0) },
        ],
    });
    let gp = Color::Gradient(GradientPaint {
        gradient: grad,
        start: Point::new(0.0, 0.0),
        end: Point::new(10.0, 0.0),
        radius: 0.0,
        matrix: Matrix::IDENTITY,
    });
    let outer = Arc::new(Pattern {
        name: "Outer".into(),
        bbox: Rect { llx: 0.0, lly: 0.0, urx: 20.0, ury: 20.0 },
        x_step: 20.0,
        y_step: 20.0,
        elements: vec![
            filled_path(rect(0.0, 0.0, 10.0, 10.0), pfill(&inner, Matrix::IDENTITY)),
            filled_path(rect(10.0, 10.0, 10.0, 10.0), gp),
        ],
    });
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 100.0, 100.0), pfill(&outer, Matrix::IDENTITY)));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.pattern_defs, vec!["Outer"]);
    assert!(stats.gradient_defs.is_empty());
    assert!(!setup(&ai).contains("Bb"));
    // Inner pattern → its representative colour; gradient → its middle colour.
    assert!(setup(&ai).contains("0 1 0 0 k\n"));
    assert!(setup(&ai).contains("0.5 g\n"));
}

#[test]
fn colour_mode_applies_inside_tiles() {
    let mut a = PathItem::new(vec![rect(0.0, 0.0, 5.0, 5.0)]);
    a.fill = Some(Color::Rgb(1.0, 0.0, 0.0));
    let mut b = PathItem::new(vec![rect(5.0, 5.0, 5.0, 5.0)]);
    b.stroke = Some(Stroke::solid(Color::Cmyk(0.0, 0.0, 0.0, 1.0), 1.0));
    let pat = Arc::new(Pattern {
        name: "Mixed".into(),
        bbox: Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 },
        x_step: 10.0,
        y_step: 10.0,
        elements: vec![Element::Path(a), Element::Path(b)],
    });
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 50.0, 50.0), pfill(&pat, Matrix::IDENTITY)));

    let ai = write_ai8(&doc, &WriteOptions { color_mode: ColorMode::Cmyk, ..Default::default() });
    common::validate_ai8(&ai);
    let s = setup(&ai);
    assert!(s.contains("0 1 1 0 k\n") && !s.contains("Xa") && !s.contains("XA"), "{s}");

    let ai = write_ai8(&doc, &WriteOptions { color_mode: ColorMode::Rgb, ..Default::default() });
    common::validate_ai8(&ai);
    let s = setup(&ai);
    assert!(s.contains("1 0 0 Xa\n") && s.contains("0 0 0 XA\n"), "{s}");
}

#[test]
fn spot_colours_in_tiles_are_declared_and_escaped() {
    let pat = pattern(
        "Spotty",
        Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 },
        (10.0, 10.0),
        Color::Spot { name: "Pantone (Special) \\ 1".into(), cmyk: [0.1, 0.2, 0.3, 0.4], tint: 1.0 },
    );
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 50.0, 50.0), pfill(&pat, Matrix::IDENTITY)));
    let ai = write(&doc);
    common::validate_ai8(&ai);
    assert!(ai.contains("%%DocumentCustomColors: (Pantone \\(Special\\) \\\\ 1)"));
    // Inside the tile string: octal escapes, backslashes doubled.
    assert!(setup(&ai).contains("(Pantone \\\\050Special\\\\051 \\\\134 1) 0 x\n"), "{}", setup(&ai));
}

#[test]
fn huge_tiles_are_split_into_layers() {
    let mut els = Vec::new();
    for i in 0..3000 {
        let mut p = PathItem::new(vec![rect((i % 50) as f64, (i / 50) as f64, 0.5, 0.5)]);
        p.fill = Some(if i % 2 == 0 { Color::Gray(0.0) } else { Color::Cmyk(0.1, 0.2, 0.3, 0.4) });
        els.push(Element::Path(p));
    }
    let pat = Arc::new(Pattern {
        name: "Big".into(),
        bbox: Rect { llx: 0.0, lly: 0.0, urx: 60.0, ury: 60.0 },
        x_step: 60.0,
        y_step: 60.0,
        elements: els,
    });
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 100.0, 100.0), pfill(&pat, Matrix::IDENTITY)));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.tile_paths, 3000);
    let layers = setup(&ai).matches("%AI6_BeginPatternLayer").count();
    assert!(layers >= 2, "{layers}");
}

// ---------------------------------------------------------------------------
// Gradients
// ---------------------------------------------------------------------------

fn two_stop(kind: GradientKind, a: Color, b: Color) -> Arc<Gradient> {
    Arc::new(Gradient { kind, stops: vec![GradientStop { offset: 0.0, color: a }, GradientStop { offset: 1.0, color: b }] })
}

fn gfill(g: &Arc<Gradient>, start: Point, end: Point, radius: f64, matrix: Matrix) -> Color {
    Color::Gradient(GradientPaint { gradient: g.clone(), start, end, radius, matrix })
}

/// Parse "1 (name) x y angle length a b c d tx ty Bg".
fn bg(line: &str) -> (String, Vec<f64>) {
    let a = line.find('(').unwrap();
    let b = line.rfind(')').unwrap();
    assert_eq!(&line[..a], "1 ");
    let nums = line[b + 1..].trim().strip_suffix("Bg").unwrap().split_whitespace().map(|t| t.parse().unwrap()).collect();
    (line[a + 1..b].to_string(), nums)
}

fn nums_before(line: &str, op: &str) -> Vec<f64> {
    line.strip_suffix(op).unwrap().split_whitespace().map(|t| t.parse().unwrap()).collect()
}

#[test]
fn linear_gradient_definition_and_instance() {
    let g = two_stop(GradientKind::Linear, Color::Cmyk(1.0, 0.0, 0.0, 0.0), Color::Gray(1.0));
    let mut doc = page(100.0, 50.0);
    doc.elements.push(filled_path(
        rect(110.0, 60.0, 100.0, 50.0),
        gfill(&g, Point::new(110.0, 60.0), Point::new(210.0, 60.0), 0.0, Matrix::IDENTITY),
    ));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.gradient_defs, vec!["Linear Gradient 1"]);
    assert_eq!(stats.gradient_instances, 1);
    let s = setup(&ai);
    assert!(s.contains(
        "%AI5_Begin_NonPrinting\nNp\n1 Bn\n%AI5_BeginGradient: (Linear Gradient 1)\n(Linear Gradient 1) 0 2 Bd\n[\n\
         1 0 50 100 %_BS\n%_1 0 50 100 Bs\n1 0 0 0 1 50 0 %_BS\n%_1 0 0 0 1 50 0 Bs\nBD\n%AI5_EndGradient\n%AI5_End_NonPrinting--\n"
    ), "{s}");
    let a = art(&ai);
    // Path, then Bb, Bg, f, 0 BB.
    assert!(a.contains("10 10 m\n110 10 L\n110 60 L\n10 60 L\nBb\n1 (Linear Gradient 1) 10 10 0 100 1 0 0 1 0 0 Bg\nf\n0 BB\n"), "{a}");
    assert!(!a.contains(" k\n"), "no solid fill colour for a gradient fill");
}

#[test]
fn linear_gradient_through_skewed_matrix() {
    let g = two_stop(GradientKind::Linear, Color::Gray(0.0), Color::Gray(1.0));
    // Gradient from (0,0) to (10,10) in gradient space, scaled 2x horizontally.
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(
        rect(0.0, 0.0, 50.0, 50.0),
        gfill(&g, Point::new(0.0, 0.0), Point::new(10.0, 10.0), 0.0, Matrix::new(2.0, 0.0, 0.0, 1.0, 5.0, 3.0)),
    ));
    let ai = write(&doc);
    common::validate_ai8(&ai);
    let (_, n) = bg(lines_ending(art(&ai), "Bg")[0]);
    // Isoline t=1 in page space: (x-5)/2 + (y-3) = 20 → normal (0.5, 1).
    assert_close(&n[..4], &[5.0, 3.0, 63.4349, 17.8885]);
    assert_close(&n[4..], &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
}

#[test]
fn linear_gradient_rotated() {
    let g = two_stop(GradientKind::Linear, Color::Gray(0.0), Color::Gray(1.0));
    let r90 = Matrix::new(0.0, 1.0, -1.0, 0.0, 100.0, 0.0);
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(
        rect(0.0, 0.0, 100.0, 100.0),
        gfill(&g, Point::new(0.0, 0.0), Point::new(40.0, 0.0), 0.0, r90),
    ));
    let ai = write(&doc);
    common::validate_ai8(&ai);
    let (_, n) = bg(lines_ending(art(&ai), "Bg")[0]);
    assert_close(&n[..4], &[100.0, 0.0, 90.0, 40.0]);
}

#[test]
fn radial_gradient_instance() {
    let g = two_stop(GradientKind::Radial, Color::Gray(1.0), Color::Rgb(0.0, 0.0, 1.0));
    let mut doc = page(0.0, 0.0);
    // Centre (50,50) r 25 moved by (10,0); focal point 10 to the right.
    doc.elements.push(filled_path(
        rect(0.0, 0.0, 120.0, 100.0),
        gfill(&g, Point::new(60.0, 50.0), Point::new(50.0, 50.0), 25.0, Matrix::translate(10.0, 0.0)),
    ));
    // Plain radial, uniformly scaled by 2 → radius 50.
    doc.elements.push(filled_path(
        rect(200.0, 0.0, 100.0, 100.0),
        gfill(&g, Point::new(25.0, 25.0), Point::new(25.0, 25.0), 25.0, Matrix::new(2.0, 0.0, 0.0, 2.0, 200.0, 0.0)),
    ));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.gradient_defs, vec!["Radial Gradient 1"]);
    // Radial ramps are listed from the first stop; RGB stops carry CMYK + RGB.
    assert!(setup(&ai).contains("(Radial Gradient 1) 1 2 Bd\n[\n1 0 50 0 %_BS\n%_1 0 50 0 Bs\n1 1 0 0 0 0 1 2 50 100 %_BS\n"));
    let a = art(&ai);
    let bh = lines_ending(a, "Bh");
    assert_close(&nums_before(bh[0], "Bh"), &[10.0, 0.0, 0.0, 0.4]);
    assert_eq!(bh[1], "0 0 0 0 Bh");
    let bgs = lines_ending(a, "Bg");
    assert_close(&bg(bgs[0]).1, &[60.0, 50.0, 0.0, 25.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    assert_close(&bg(bgs[1]).1, &[250.0, 50.0, 0.0, 50.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    assert!(a.contains("Bb\n0 0 0 0 Bh\n1 (Radial Gradient 1) "));
    assert!(lines_ending(a, "Bm").is_empty());
}

#[test]
fn radial_gradient_non_uniform_uses_bg_matrix() {
    let g = two_stop(GradientKind::Radial, Color::Gray(1.0), Color::Gray(0.0));
    let m = Matrix::new(3.0, 0.0, 1.0, 1.0, 20.0, 30.0);
    let (w, h) = (400.0, 300.0);
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 200.0, 200.0), gfill(&g, Point::new(10.0, 10.0), Point::new(10.0, 10.0), 5.0, m)));
    let ai = write(&doc);
    common::validate_ai8(&ai);
    let a = art(&ai);
    let (_, n) = bg(lines_ending(a, "Bg")[0]);
    let centre = m.apply(Point::new(10.0, 10.0));
    assert_close(&n[..4], &[centre.x, centre.y, 0.0, 5.0]);
    // Spec formula (AI7 §3.5.4) with the canvas corner (xo, yo).
    let (xo, yo) = (w / 2.0 - 4320.0, h / 2.0 + 4320.0);
    let [ba, bb, bc, bd, tx, ty] = [n[4], n[5], n[6], n[7], n[8], n[9]];
    let map = |x: f64, y: f64| {
        (ba * (x - xo) - bc * (y - yo) + tx + xo, -bb * (x - xo) + bd * (y - yo) + ty + yo)
    };
    // A point on the untransformed circle (centre + (5,0)) must land on the
    // gradient-space circle point (15,10) mapped by the gradient matrix.
    let want = m.apply(Point::new(15.0, 10.0));
    let got = map(centre.x + 5.0, centre.y);
    assert!((got.0 - want.x).abs() < 1e-2 && (got.1 - want.y).abs() < 1e-2, "{got:?} vs {want:?}");
    let want = m.apply(Point::new(10.0, 15.0));
    let got = map(centre.x, centre.y + 5.0);
    assert!((got.0 - want.x).abs() < 1e-2 && (got.1 - want.y).abs() < 1e-2, "{got:?} vs {want:?}");
    assert_close(&nums_before(lines_ending(a, "Bm")[0], "Bm"), &[15.0, 0.0, -5.0, -5.0, centre.x, centre.y]);
}

#[test]
fn spot_gradient_stops_are_declared() {
    let spot = Color::Spot { name: "Gold".into(), cmyk: [0.05, 0.2, 0.95, 0.0], tint: 1.0 };
    let half = Color::Spot { name: "Gold".into(), cmyk: [0.05, 0.2, 0.95, 0.0], tint: 0.25 };
    let g = Arc::new(Gradient {
        kind: GradientKind::Linear,
        stops: vec![
            GradientStop { offset: 0.0, color: spot },
            GradientStop { offset: 0.375, color: half },
            GradientStop { offset: 1.0, color: Color::Cmyk(0.0, 1.0, 0.6, 0.0) },
        ],
    });
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 50.0, 50.0), gfill(&g, Point::new(0.0, 0.0), Point::new(50.0, 0.0), 0.0, Matrix::IDENTITY)));
    let ai = write(&doc);
    common::validate_ai8(&ai);
    assert!(ai.contains("%%DocumentCustomColors: (Gold)\n%%CMYKCustomColor: 0.05 0.2 0.95 0 (Gold)\n"));
    let s = setup(&ai);
    assert!(s.contains("(Linear Gradient 1) 0 3 Bd\n[\n0 1 0.6 0 1 50 100 %_BS\n"), "{s}");
    assert!(s.contains("0.05 0.2 0.95 0 (Gold) 0.75 3 50 37.5 %_BS\n%_0.05 0.2 0.95 0 (Gold) 0.75 3 50 37.5 Bs\n"), "{s}");
    assert!(s.contains("0.05 0.2 0.95 0 (Gold) 0 3 50 0 %_BS\n"));
}

#[test]
fn gradient_colour_modes() {
    let g = two_stop(GradientKind::Linear, Color::Rgb(1.0, 0.0, 0.0), Color::Gray(0.25));
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 50.0, 50.0), gfill(&g, Point::new(0.0, 0.0), Point::new(50.0, 0.0), 0.0, Matrix::IDENTITY)));
    let ai = write_ai8(&doc, &WriteOptions { color_mode: ColorMode::Cmyk, ..Default::default() });
    common::validate_ai8(&ai);
    let s = setup(&ai);
    assert!(s.contains("0 0 0 0.75 1 50 100 %_BS\n") && s.contains("0 1 1 0 1 50 0 %_BS\n"), "{s}");
    let ai = write_ai8(&doc, &WriteOptions { color_mode: ColorMode::Rgb, ..Default::default() });
    common::validate_ai8(&ai);
    let s = setup(&ai);
    assert!(s.contains("0 0 0 0.75 0.25 0.25 0.25 2 50 100 %_BS\n"), "{s}");
}

#[test]
fn gradients_shared_and_strokes_flattened() {
    let g = two_stop(GradientKind::Linear, Color::Gray(0.0), Color::Gray(1.0));
    let same = two_stop(GradientKind::Linear, Color::Gray(0.0), Color::Gray(1.0));
    let mut doc = page(0.0, 0.0);
    let mut a = PathItem::new(vec![rect(0.0, 0.0, 50.0, 50.0), rect(10.0, 10.0, 10.0, 10.0)]);
    a.fill = Some(gfill(&g, Point::new(0.0, 0.0), Point::new(50.0, 0.0), 0.0, Matrix::IDENTITY));
    a.stroke = Some(Stroke::solid(gfill(&same, Point::new(0.0, 0.0), Point::new(1.0, 0.0), 0.0, Matrix::IDENTITY), 2.0));
    a.even_odd = true;
    doc.elements.push(Element::Path(a));
    let mut open = PathItem::new(vec![{
        let mut s = Subpath::new(Point::new(0.0, 100.0));
        s.segments.push(Segment::Line(Point::new(50.0, 150.0)));
        s.segments.push(Segment::Line(Point::new(100.0, 100.0)));
        s
    }]);
    open.fill = Some(gfill(&same, Point::new(0.0, 0.0), Point::new(0.0, 50.0), 0.0, Matrix::IDENTITY));
    open.stroke = Some(Stroke::solid(Color::Gray(0.0), 1.0));
    doc.elements.push(Element::Path(open));
    // Solid fill after a gradient must be re-emitted.
    doc.elements.push(filled_path(rect(200.0, 0.0, 10.0, 10.0), Color::Gray(0.5)));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.gradient_defs.len(), 1);
    // Compound path: one instance per subpath, stroke via "2 BB".
    assert_eq!(stats.gradient_instances, 3);
    let a = art(&ai);
    assert!(a.contains("0.5 G\n"), "gradient stroke flattened to its middle colour");
    assert_eq!(a.matches("f\n2 BB\n").count(), 2);
    assert!(a.contains("F\n1 BB\n"));
    assert!(a.contains("0.5 g\n"));
    assert!(a.contains("*u\n") && a.contains("1 XR\n"));
}

#[test]
fn degenerate_gradients_fall_back_to_solid() {
    let g = two_stop(GradientKind::Linear, Color::Gray(0.0), Color::Gray(1.0));
    let r = two_stop(GradientKind::Radial, Color::Gray(0.0), Color::Gray(1.0));
    let one = Arc::new(Gradient { kind: GradientKind::Linear, stops: vec![GradientStop { offset: 0.5, color: Color::Gray(0.3) }] });
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 5.0, 5.0), gfill(&g, Point::new(1.0, 1.0), Point::new(1.0, 1.0), 0.0, Matrix::IDENTITY)));
    doc.elements.push(filled_path(rect(0.0, 0.0, 5.0, 5.0), gfill(&r, Point::new(1.0, 1.0), Point::new(1.0, 1.0), 0.0, Matrix::IDENTITY)));
    doc.elements.push(filled_path(
        rect(0.0, 0.0, 5.0, 5.0),
        gfill(&g, Point::new(0.0, 0.0), Point::new(1.0, 0.0), 0.0, Matrix::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0)),
    ));
    doc.elements.push(filled_path(rect(0.0, 0.0, 5.0, 5.0), gfill(&one, Point::new(0.0, 0.0), Point::new(1.0, 0.0), 0.0, Matrix::IDENTITY)));
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.gradient_instances, 0);
    assert!(stats.gradient_defs.is_empty());
    assert!(!ai.contains(" Bn\n"));
    assert!(art(&ai).contains("0.3 g\n"));
}

// ---------------------------------------------------------------------------
// Expand mode
// ---------------------------------------------------------------------------

#[test]
fn expand_mode_writes_plain_tiles() {
    let mut sq = PathItem::new(vec![rect(0.0, 0.0, 10.0, 10.0)]);
    sq.fill = Some(Color::Spot { name: "Cut".into(), cmyk: [0.0, 1.0, 0.0, 0.0], tint: 1.0 });
    let pat = Arc::new(Pattern {
        name: "Ten".into(),
        bbox: Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 },
        x_step: 10.0,
        y_step: 10.0,
        elements: vec![Element::Path(sq)],
    });
    let mut doc = page(0.0, 0.0);
    let mut p = PathItem::new(vec![rect(0.0, 0.0, 95.0, 95.0)]);
    p.fill = Some(pfill(&pat, Matrix::IDENTITY));
    p.stroke = Some(Stroke::solid(pfill(&pat, Matrix::IDENTITY), 1.0));
    doc.elements.push(Element::Path(p));
    expand_patterns(&mut doc);
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert!(stats.pattern_defs.is_empty());
    assert_eq!(stats.pattern_uses, 0);
    // The fill, and the pattern stroke expanded inside its outline.
    assert_eq!(stats.clip_groups, 2);
    // 100 fill tiles + the tiles covering the stroke outline's bounds
    // (-0.5..95.5: an 11 × 11 lattice); no flat-coloured stroke is left.
    assert_eq!(stats.paths, 100 + 121);
    assert!(!ai.lines().any(|l| l == "s" || l == "S"), "pattern stroke written as a flat stroke");
    assert!(ai.contains("%%DocumentCustomColors: (Cut)"));
    // Tile placement: lower-left corners on the 10 pt lattice.
    // Fill tiles (the first clip group).
    let a = art(&ai);
    let a = &a[..a.find("\nQ\n").unwrap()];
    for (i, j) in [(0, 0), (9, 0), (0, 9), (9, 9), (4, 7)] {
        assert!(a.contains(&format!("{} {} m\n", i * 10, j * 10)), "tile ({i},{j}) missing");
    }
    assert!(!a.contains("100 0 m\n") && !a.contains("-10 0 m\n"));
}

#[test]
fn expand_mode_rotated_pattern_page_offset() {
    let pat = unit_pattern("Rot");
    let mut doc = page(100.0, 50.0);
    let h = std::f64::consts::FRAC_1_SQRT_2;
    let m = Matrix::new(h, h, -h, h, 100.0, 50.0);
    doc.elements.push(filled_path(rect(100.0, 50.0, 60.0, 60.0), pfill(&pat, m)));
    expand_patterns(&mut doc);
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert!(stats.paths > 36 && stats.paths < 15 * 15, "{}", stats.paths);
    assert!(ai.contains("q\n0 XR\n0 0 m\n60 0 L\n60 60 L\n0 60 L\nh W\nn\n"));
}

#[test]
fn expand_mode_tile_cap_warns() {
    let tiny = pattern("Tiny", Rect { llx: 0.0, lly: 0.0, urx: 1.0, ury: 1.0 }, (1.0, 1.0), Color::Gray(0.0));
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 400.0, 300.0), pfill(&tiny, Matrix::IDENTITY)));
    expand_patterns(&mut doc);
    assert_eq!(doc.warnings.len(), 1, "{:?}", doc.warnings);
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.paths, 1);
    assert!(art(&ai).contains("0 g\n"));
}

#[test]
fn flatten_gradients_removes_instances() {
    let g = two_stop(GradientKind::Radial, Color::Gray(0.0), Color::Gray(1.0));
    let mut doc = page(0.0, 0.0);
    doc.elements.push(filled_path(rect(0.0, 0.0, 50.0, 50.0), gfill(&g, Point::new(25.0, 25.0), Point::new(25.0, 25.0), 25.0, Matrix::IDENTITY)));
    flatten_gradients(&mut doc);
    let ai = write(&doc);
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.gradient_instances, 0);
    assert!(art(&ai).contains("0.5 g\n"));
}

