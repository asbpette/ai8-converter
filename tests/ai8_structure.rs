//! Structural tests for the AI8 writer using randomly generated IR documents.

mod common;

use ai8_converter::ai8::{ColorMode, WriteOptions, write_ai8};
use ai8_converter::ir::*;

/// Small deterministic PRNG (xorshift) so tests need no extra crates.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f64 {
        (self.next() % 1_000_000) as f64 / 1_000_000.0
    }
    fn range(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pt(&mut self) -> Point {
        Point::new(self.f() * 600.0 - 50.0, self.f() * 800.0 - 50.0)
    }
}

fn color(r: &mut Rng) -> Color {
    match r.range(4) {
        0 => Color::Gray(r.f()),
        1 => Color::Rgb(r.f(), r.f(), r.f()),
        2 => Color::Cmyk(r.f(), r.f(), r.f(), r.f()),
        _ => Color::Spot { name: format!("Spot (#{})", r.range(3)), cmyk: [r.f(), r.f(), 0.0, 0.0], tint: r.f() },
    }
}

fn subpath(r: &mut Rng) -> Subpath {
    let mut sp = Subpath::new(r.pt());
    for _ in 0..1 + r.range(6) {
        sp.segments.push(if r.range(2) == 0 { Segment::Line(r.pt()) } else { Segment::Curve(r.pt(), r.pt(), r.pt()) });
    }
    sp.closed = r.range(2) == 0;
    sp
}

fn element(r: &mut Rng, depth: u32) -> Element {
    let kind = if depth > 3 { 0 } else { r.range(5) };
    match kind {
        3 => Element::Group((0..r.range(4)).map(|_| element(r, depth + 1)).collect()),
        4 => Element::ClipGroup {
            clip: (0..1 + r.range(2)).map(|_| subpath(r)).collect(),
            even_odd: r.range(2) == 0,
            children: (0..1 + r.range(3)).map(|_| element(r, depth + 1)).collect(),
        },
        _ => {
            let mut p = PathItem::new((0..1 + r.range(3)).map(|_| subpath(r)).collect());
            if r.range(3) != 0 {
                p.fill = Some(color(r));
            }
            if r.range(2) == 0 {
                let mut s = Stroke::solid(color(r), r.f() * 5.0);
                if r.range(2) == 0 {
                    s.dash = vec![r.f() * 10.0, r.f() * 5.0];
                }
                p.stroke = Some(s);
            }
            p.even_odd = r.range(2) == 0;
            p.fill_overprint = r.range(4) == 0;
            Element::Path(p)
        }
    }
}

#[test]
fn random_documents_are_well_formed() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    for i in 0..300 {
        let mut doc = Document::new(Rect { llx: 0.0, lly: 0.0, urx: 595.0, ury: 842.0 });
        doc.elements = (0..r.range(8)).map(|_| element(&mut r, 0)).collect();
        for mode in [ColorMode::Preserve, ColorMode::Cmyk, ColorMode::Rgb] {
            let opts = WriteOptions { title: format!("t{i}.ai"), creator_for: "Ø user".into(), color_mode: mode };
            let ai = write_ai8(&doc, &opts);
            let stats = common::validate_ai8(&ai);
            if mode == ColorMode::Cmyk {
                assert!(!ai.contains(" Xa\n") && !ai.contains(" XA\n"), "RGB left in CMYK mode");
            }
            assert!(stats.paths <= doc.path_count() * 3 + 1);
        }
    }
}

#[test]
fn every_spot_is_declared() {
    let mut r = Rng(42);
    for _ in 0..50 {
        let mut doc = Document::new(Rect { llx: 0.0, lly: 0.0, urx: 100.0, ury: 100.0 });
        doc.elements = (0..5).map(|_| element(&mut r, 0)).collect();
        let ai = write_ai8(&doc, &WriteOptions::default());
        let header = &ai[..ai.find("%%EndComments").unwrap()];
        for line in ai.lines().filter(|l| l.ends_with(" x") || l.ends_with(" X")) {
            let name = &line[line.find('(').unwrap()..=line.rfind(')').unwrap()];
            assert!(header.contains(name), "spot {name} used but not declared");
        }
    }
}

#[test]
fn bounding_box_encloses_all_coordinates() {
    let mut r = Rng(7);
    let mut doc = Document::new(Rect { llx: 0.0, lly: 0.0, urx: 500.0, ury: 500.0 });
    // Clip groups are excluded: their bounds are the visible (clipped) extent.
    doc.elements = (0..40).map(|_| element(&mut r, 0)).filter(|e| !has_clip(e)).collect();
    assert!(doc.elements.len() > 5);
    let ai = write_ai8(&doc, &WriteOptions::default());
    let bb: Vec<f64> = ai
        .lines()
        .find(|l| l.starts_with("%%HiResBoundingBox:"))
        .unwrap()
        .split_whitespace()
        .skip(1)
        .map(|t| t.parse().unwrap())
        .collect();
    let art = &ai[ai.find("%AI5_BeginLayer").unwrap()..];
    for line in art.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if matches!(toks.last(), Some(&"m") | Some(&"L") | Some(&"C")) {
            let n: Vec<f64> = toks[..toks.len() - 1].iter().map(|t| t.parse().unwrap()).collect();
            for xy in n.chunks(2) {
                assert!(xy[0] >= bb[0] - 1e-3 && xy[0] <= bb[2] + 1e-3, "x {} outside {bb:?}", xy[0]);
                assert!(xy[1] >= bb[1] - 1e-3 && xy[1] <= bb[3] + 1e-3, "y {} outside {bb:?}", xy[1]);
            }
        }
    }
}

fn has_clip(e: &Element) -> bool {
    match e {
        Element::ClipGroup { .. } => true,
        Element::Group(c) => c.iter().any(has_clip),
        Element::Path(_) => false,
    }
}

#[test]
fn clip_group_bounds_are_intersection() {
    let sq = |x: f64, s: f64| {
        let mut sp = Subpath::new(Point::new(x, x));
        sp.segments.push(Segment::Line(Point::new(x + s, x)));
        sp.segments.push(Segment::Line(Point::new(x + s, x + s)));
        sp.closed = true;
        sp
    };
    let mut p = PathItem::new(vec![sq(0.0, 100.0)]);
    p.fill = Some(Color::Gray(0.0));
    let mut doc = Document::new(Rect { llx: 0.0, lly: 0.0, urx: 200.0, ury: 200.0 });
    doc.elements = vec![Element::ClipGroup { clip: vec![sq(50.0, 100.0)], even_odd: false, children: vec![Element::Path(p)] }];
    let b = doc.content_bounds().unwrap();
    assert_eq!((b.llx, b.lly, b.urx, b.ury), (50.0, 50.0, 100.0, 100.0));
}
