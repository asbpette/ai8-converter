//! Structural tests for the AI8 writer using randomly generated IR documents.

mod common;

use ai8_converter::ai8::{ColorMode, WriteOptions, write_ai8};
use ai8_converter::ir::*;
use ai8_converter::patterns::expand_patterns;
use std::sync::Arc;

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

fn solid(r: &mut Rng) -> Color {
    match r.range(4) {
        0 => Color::Gray(r.f()),
        1 => Color::Rgb(r.f(), r.f(), r.f()),
        2 => Color::Cmyk(r.f(), r.f(), r.f(), r.f()),
        _ => Color::Spot { name: format!("Spot (#{})", r.range(3)), cmyk: [r.f(), r.f(), 0.0, 0.0], tint: r.f() },
    }
}

/// Random affine matrix: rotation, (non-uniform) scale, optional skew, translation.
fn matrix(r: &mut Rng) -> Matrix {
    let a = r.f() * std::f64::consts::TAU;
    let (sx, sy) = (0.5 + r.f() * 1.5, 0.5 + r.f() * 1.5);
    let sy = if r.range(3) == 0 { sx } else { sy };
    let k = if r.range(3) == 0 { r.f() - 0.5 } else { 0.0 };
    let m = Matrix::new(sx, 0.0, k * sx, sy, 0.0, 0.0);
    let rot = Matrix::new(a.cos(), a.sin(), -a.sin(), a.cos(), r.f() * 400.0 - 100.0, r.f() * 400.0 - 100.0);
    if r.range(4) == 0 { Matrix::translate(r.f() * 50.0, r.f() * 50.0) } else { m.then(&rot) }
}

/// Generator state: patterns are shared between paints (like real documents).
struct Gen {
    r: Rng,
    pool: Vec<Arc<Pattern>>,
}

impl Gen {
    fn color(&mut self, depth: u32) -> Color {
        match self.r.range(7) {
            5 if depth < 3 => self.pattern_paint(depth),
            6 => self.gradient_paint(),
            _ => solid(&mut self.r),
        }
    }

    fn pattern_paint(&mut self, depth: u32) -> Color {
        let reuse = !self.pool.is_empty() && self.r.range(2) == 0;
        let pattern = if reuse {
            let i = self.r.range(self.pool.len() as u64) as usize;
            self.pool[i].clone()
        } else {
            let (x, y) = (self.r.f() * 100.0 - 50.0, self.r.f() * 100.0 - 50.0);
            let (w, h) = (60.0 + self.r.f() * 120.0, 60.0 + self.r.f() * 120.0);
            let bbox = Rect { llx: x, lly: y, urx: x + w, ury: y + h };
            let step = |r: &mut Rng, s: f64| match r.range(4) {
                0 => s * (0.6 + r.f()),
                1 => 0.0,
                _ => s,
            };
            let (x_step, y_step) = (step(&mut self.r, w), step(&mut self.r, h));
            let n = 1 + self.r.range(3);
            let elements = (0..n)
                .map(|_| {
                    // Tile art around the bbox (sometimes exceeding it).
                    let e = self.element(depth + 2);
                    let m = Matrix::new(w / 700.0, 0.0, 0.0, h / 900.0, x, y);
                    ai8_converter::patterns::transform_elements(&[e], &m).remove(0)
                })
                .collect();
            let name = if self.r.range(3) == 0 { "Shared".to_string() } else { format!("P{}", self.pool.len()) };
            let p = Arc::new(Pattern { name, bbox, x_step, y_step, elements });
            self.pool.push(p.clone());
            p
        };
        Color::Pattern(PatternPaint { pattern, matrix: matrix(&mut self.r) })
    }

    fn gradient_paint(&mut self) -> Color {
        let n = 2 + self.r.range(3) as usize;
        let mut offs: Vec<f64> = (0..n).map(|_| self.r.f()).collect();
        offs.sort_by(f64::total_cmp);
        if self.r.range(2) == 0 {
            offs[0] = 0.0;
            offs[n - 1] = 1.0;
        }
        let stops = offs.into_iter().map(|offset| GradientStop { offset, color: solid(&mut self.r) }).collect();
        let kind = if self.r.range(2) == 0 { GradientKind::Linear } else { GradientKind::Radial };
        Color::Gradient(GradientPaint {
            gradient: Arc::new(Gradient { kind, stops }),
            start: self.r.pt(),
            end: if self.r.range(5) == 0 { Point::new(0.0, 0.0) } else { self.r.pt() },
            radius: self.r.f() * 200.0,
            matrix: matrix(&mut self.r),
        })
    }

    fn element(&mut self, depth: u32) -> Element {
        let r = &mut self.r;
        let kind = if depth > 3 { 0 } else { r.range(5) };
        match kind {
            3 => Element::Group((0..self.r.range(4)).map(|_| self.element(depth + 1)).collect()),
            4 => Element::ClipGroup {
                clip: (0..1 + r.range(2)).map(|_| subpath(r)).collect(),
                even_odd: r.range(2) == 0,
                children: (0..1 + self.r.range(3)).map(|_| self.element(depth + 1)).collect(),
            },
            _ => {
                let mut p = PathItem::new((0..1 + r.range(3)).map(|_| subpath(r)).collect());
                if self.r.range(3) != 0 {
                    p.fill = Some(self.color(depth));
                }
                if self.r.range(2) == 0 {
                    let c = self.color(depth);
                    let mut s = Stroke::solid(c, self.r.f() * 5.0);
                    if self.r.range(2) == 0 {
                        s.dash = vec![self.r.f() * 10.0, self.r.f() * 5.0];
                    }
                    p.stroke = Some(s);
                }
                p.even_odd = self.r.range(2) == 0;
                p.fill_overprint = self.r.range(4) == 0;
                Element::Path(p)
            }
        }
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
    let mut g = Gen { r: Rng(r.next()), pool: Vec::new() };
    let e = g.element(depth);
    r.0 = g.r.0;
    e
}

fn has_paint_server(els: &[Element]) -> bool {
    els.iter().any(|e| match e {
        Element::Path(p) => {
            p.fill.as_ref().is_some_and(|c| !c.is_solid()) || p.stroke.as_ref().is_some_and(|s| !s.color.is_solid())
        }
        Element::Group(c) => has_paint_server(c),
        Element::ClipGroup { children, .. } => has_paint_server(children),
    })
}

#[test]
fn random_documents_are_well_formed() {
    let mut g = Gen { r: Rng(0x9E37_79B9_7F4A_7C15), pool: Vec::new() };
    let mut with_patterns = 0;
    let (mut defs, mut instances) = (0, 0);
    for i in 0..300 {
        g.pool.clear();
        let mut doc = Document::new(Rect { llx: 0.0, lly: 0.0, urx: 595.0, ury: 842.0 });
        if i % 3 == 1 {
            doc.page = Rect { llx: -40.0, lly: 25.0, urx: 555.0, ury: 867.0 };
        }
        doc.elements = (0..g.r.range(8)).map(|_| g.element(0)).collect();
        if has_paint_server(&doc.elements) {
            with_patterns += 1;
        }
        let mut expanded = doc.clone();
        expand_patterns(&mut expanded);
        for mode in [ColorMode::Preserve, ColorMode::Cmyk, ColorMode::Rgb] {
            let opts = WriteOptions { title: format!("t{i}.ai"), creator_for: "Ø user".into(), color_mode: mode };
            for (native, d) in [(true, &doc), (false, &expanded)] {
                let ai = write_ai8(d, &opts);
                let stats = common::validate_ai8(&ai);
                if mode == ColorMode::Cmyk {
                    assert!(!ai.contains(" Xa\n") && !ai.contains(" XA\n"), "RGB left in CMYK mode");
                    assert!(!ai.contains(" 2 50 "), "RGB gradient stop left in CMYK mode");
                }
                assert!(stats.paths <= d.path_count() * 3 + 1);
                if native {
                    defs += stats.pattern_defs.len();
                    instances += stats.gradient_instances;
                } else {
                    assert!(stats.pattern_defs.is_empty() && stats.pattern_uses == 0, "patterns left after expansion");
                }
            }
        }
    }
    assert!(with_patterns > 100, "generator produced too few pattern/gradient documents: {with_patterns}");
    assert!(defs > 100 && instances > 100, "{defs} {instances}");
}

#[test]
fn every_spot_is_declared() {
    let mut r = Rng(42);
    for _ in 0..50 {
        let mut doc = Document::new(Rect { llx: 0.0, lly: 0.0, urx: 100.0, ury: 100.0 });
        doc.elements = (0..5).map(|_| element(&mut r, 0)).collect();
        let ai = write_ai8(&doc, &WriteOptions::default());
        let header = &ai[..ai.find("%%EndComments").unwrap()];
        // Spots inside pattern tiles are checked by the validator.
        common::validate_ai8(&ai);
        let art = &ai[ai.find("%AI5_BeginLayer").unwrap()..];
        for line in art.lines().filter(|l| l.ends_with(" x") || l.ends_with(" X")) {
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
