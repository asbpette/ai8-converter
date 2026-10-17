//! HATCH entities: boundary parsing, pattern line families (native tiling
//! pattern or expanded lines) and gradient fills.

use super::geom::{DEG, Nurbs, PathBuilder, ccw_sweep, fit_point_curve, near};
use super::model::Record;
use super::reader::Pair;
use crate::ir::{Element, Matrix, PathItem, Pattern, Point, Rect, Segment, Stroke, Subpath};
use std::f64::consts::TAU;

/// One line family of a hatch pattern (OCS, scale and angle already applied).
#[derive(Debug, Clone)]
pub struct Family {
    pub angle: f64,
    pub base: Point,
    pub offset: Point,
    /// Dash lengths: > 0 dash, < 0 gap, 0 dot. Empty = continuous.
    pub dashes: Vec<f64>,
}

impl Family {
    fn dir(&self) -> Point {
        Point::new(self.angle.cos(), self.angle.sin())
    }
    fn normal(&self) -> Point {
        Point::new(-self.angle.sin(), self.angle.cos())
    }
    fn period(&self) -> f64 {
        self.dashes.iter().map(|d| d.abs()).sum()
    }
    fn continuous(&self) -> bool {
        self.dashes.is_empty() || self.period() <= 1e-12 || self.dashes.iter().all(|d| *d > 0.0)
    }
    /// Signed perpendicular spacing between successive lines.
    fn spacing(&self) -> f64 {
        dot(self.offset, self.normal())
    }
}

#[derive(Debug, Clone, Default)]
pub struct GradientDef {
    pub name: String,
    /// Colours as (ACI, true colour) – true colour wins when present.
    pub colors: Vec<(Option<i64>, Option<i64>)>,
    pub angle: f64,
    pub shift: f64,
    pub single_color: bool,
    pub tint: f64,
}

#[derive(Debug, Clone)]
pub enum Piece {
    Line(Point, Point),
    /// c + u·cos t + v·sin t, t from t0 sweeping `sweep`.
    Arc { c: Point, u: Point, v: Point, t0: f64, sweep: f64 },
    Cubics { start: Point, segs: Vec<[Point; 3]> },
}

impl Piece {
    fn eval_arc(c: Point, u: Point, v: Point, t: f64) -> Point {
        Point::new(c.x + u.x * t.cos() + v.x * t.sin(), c.y + u.y * t.cos() + v.y * t.sin())
    }
    fn start(&self) -> Point {
        match self {
            Piece::Line(a, _) => *a,
            Piece::Arc { c, u, v, t0, .. } => Self::eval_arc(*c, *u, *v, *t0),
            Piece::Cubics { start, .. } => *start,
        }
    }
    fn end(&self) -> Point {
        match self {
            Piece::Line(_, b) => *b,
            Piece::Arc { c, u, v, t0, sweep } => Self::eval_arc(*c, *u, *v, t0 + sweep),
            Piece::Cubics { start, segs } => segs.last().map(|s| s[2]).unwrap_or(*start),
        }
    }
    fn reversed(&self) -> Piece {
        match self {
            Piece::Line(a, b) => Piece::Line(*b, *a),
            Piece::Arc { c, u, v, t0, sweep } => Piece::Arc { c: *c, u: *u, v: *v, t0: t0 + sweep, sweep: -sweep },
            Piece::Cubics { start, segs } => {
                let mut pts = vec![*start];
                for s in segs {
                    pts.extend_from_slice(s);
                }
                pts.reverse();
                let segs = pts[1..].chunks(3).filter(|c| c.len() == 3).map(|c| [c[0], c[1], c[2]]).collect();
                Piece::Cubics { start: pts[0], segs }
            }
        }
    }
    fn emit(&self, pb: &mut PathBuilder) {
        match self {
            Piece::Line(a, b) => {
                if pb.current().is_none_or(|p| !near(p, *a, 1e-9)) {
                    pb.line_to(a.x, a.y);
                }
                pb.line_to(b.x, b.y);
            }
            Piece::Arc { c, u, v, t0, sweep } => pb.ellipse_arc(*c, *u, *v, *t0, *sweep),
            Piece::Cubics { start, segs } => {
                if pb.current().is_none_or(|p| !near(p, *start, 1e-9)) {
                    pb.line_to(start.x, start.y);
                }
                for s in segs {
                    pb.curve_to(s[0], s[1], s[2]);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Hatch {
    pub elevation: f64,
    pub normal: [f64; 3],
    pub name: String,
    pub solid: bool,
    /// Each loop is a chain of pieces (OCS).
    pub loops: Vec<Vec<Piece>>,
    pub families: Vec<Family>,
    pub gradient: Option<GradientDef>,
    /// Some boundary data could not be read.
    pub damaged: bool,
    pub approximated_splines: bool,
}

struct Cur<'a> {
    p: &'a [Pair],
    i: usize,
}

impl<'a> Cur<'a> {
    fn peek(&self) -> Option<&'a Pair> {
        self.p.get(self.i)
    }
    fn peek_code(&self) -> Option<i32> {
        self.peek().map(|p| p.code)
    }
    fn take(&mut self, code: i32) -> Option<&'a Pair> {
        let p = self.peek().filter(|p| p.code == code)?;
        self.i += 1;
        Some(p)
    }
    fn f(&mut self, code: i32) -> Option<f64> {
        self.take(code).and_then(Pair::f)
    }
    fn i(&mut self, code: i32) -> Option<i64> {
        self.take(code).and_then(Pair::i)
    }
    /// Consume pairs whose codes are in `set` (in any order), returning them.
    fn take_set(&mut self, set: &[i32]) -> Vec<&'a Pair> {
        let mut out = Vec::new();
        while let Some(p) = self.peek().filter(|p| set.contains(&p.code)) {
            out.push(p);
            self.i += 1;
        }
        out
    }
}

fn val(ps: &[&Pair], code: i32) -> Option<f64> {
    ps.iter().find(|p| p.code == code).and_then(|p| p.f())
}

/// Parse a HATCH record. `tol` is the curve approximation tolerance (OCS units).
pub fn parse(e: &Record, tol: f64) -> Hatch {
    let mut h = Hatch {
        elevation: e.f(30).unwrap_or(0.0),
        normal: e.normal(),
        name: e.s(2).unwrap_or_default(),
        solid: e.i_or(70, 0) & 1 == 1,
        ..Default::default()
    };
    // Boundary data starts at the first 91 after the AcDbHatch marker.
    let start = e.pairs.iter().position(|p| p.is(100, "AcDbHatch")).unwrap_or(0);
    let Some(i91) = e.pairs[start..].iter().position(|p| p.code == 91).map(|k| k + start) else {
        h.damaged = true;
        return h;
    };
    let mut c = Cur { p: &e.pairs, i: i91 };
    let npaths = c.i(91).unwrap_or(0).clamp(0, 100_000);
    for _ in 0..npaths {
        let Some(flags) = c.i(92) else {
            h.damaged = true;
            break;
        };
        let mut pieces = Vec::new();
        if flags & 2 != 0 {
            // Polyline boundary.
            let has_bulge = c.i(72).unwrap_or(0) != 0;
            let closed = c.i(73).unwrap_or(1) != 0;
            let _n = c.i(93);
            let mut verts: Vec<(Point, f64)> = Vec::new();
            while c.peek_code() == Some(10) {
                let x = c.f(10).unwrap_or(0.0);
                let y = c.f(20).unwrap_or(0.0);
                let b = if has_bulge || c.peek_code() == Some(42) { c.f(42).unwrap_or(0.0) } else { 0.0 };
                verts.push((Point::new(x, y), b));
            }
            let n = verts.len();
            let segs = if closed { n } else { n.saturating_sub(1) };
            for k in 0..segs {
                let (a, b) = (verts[k], verts[(k + 1) % n]);
                pieces.push(bulge_piece(a.0, b.0, a.1));
            }
            if !closed && n >= 2 {
                // Hatch loops are always closed areas.
                pieces.push(Piece::Line(verts[n - 1].0, verts[0].0));
            }
        } else {
            let nedges = c.i(93).unwrap_or(0).clamp(0, 1_000_000);
            for _ in 0..nedges {
                let Some(t) = c.i(72) else {
                    h.damaged = true;
                    break;
                };
                match t {
                    1 => {
                        let ps = c.take_set(&[10, 20, 11, 21]);
                        let a = Point::new(val(&ps, 10).unwrap_or(0.0), val(&ps, 20).unwrap_or(0.0));
                        let b = Point::new(val(&ps, 11).unwrap_or(0.0), val(&ps, 21).unwrap_or(0.0));
                        pieces.push(Piece::Line(a, b));
                    }
                    2 | 3 => {
                        let ps = c.take_set(&[10, 20, 11, 21, 40, 50, 51, 73]);
                        let ctr = Point::new(val(&ps, 10).unwrap_or(0.0), val(&ps, 20).unwrap_or(0.0));
                        let (u, v) = if t == 2 {
                            let r = val(&ps, 40).unwrap_or(0.0);
                            (Point::new(r, 0.0), Point::new(0.0, r))
                        } else {
                            let m = Point::new(val(&ps, 11).unwrap_or(0.0), val(&ps, 21).unwrap_or(0.0));
                            let ratio = val(&ps, 40).unwrap_or(1.0);
                            (m, Point::new(-m.y * ratio, m.x * ratio))
                        };
                        let a0 = val(&ps, 50).unwrap_or(0.0) * DEG;
                        let a1 = val(&ps, 51).unwrap_or(360.0) * DEG;
                        let ccw = val(&ps, 73).unwrap_or(1.0) != 0.0;
                        let full = (a1 - a0).abs() >= TAU - 1e-9;
                        let sweep = if full { TAU } else { ccw_sweep(a0, a1) };
                        // Clockwise edges store angles measured clockwise.
                        let (t0, sweep) = if ccw { (a0, sweep) } else { (-a0, -sweep) };
                        pieces.push(Piece::Arc { c: ctr, u, v, t0, sweep });
                    }
                    4 => match spline_edge(&mut c, tol, &mut h.approximated_splines) {
                        Some(p) => pieces.push(p),
                        None => h.damaged = true,
                    },
                    _ => {
                        h.damaged = true;
                        break;
                    }
                }
            }
        }
        // Source boundary objects.
        if let Some(n) = c.i(97) {
            for _ in 0..n.clamp(0, 1_000_000) {
                if c.take(330).is_none() {
                    break;
                }
            }
        }
        if !pieces.is_empty() {
            h.loops.push(chain(pieces));
        }
    }
    // Pattern definition lines and gradient data follow the boundaries.
    let rest = &e.pairs[c.i..];
    if !h.solid {
        let mut cur: Option<Family> = None;
        let mut nums = (0.0, 0.0, 0.0, 0.0);
        let mut in_lines = false;
        for p in rest {
            match p.code {
                78 => in_lines = true,
                53 if in_lines => {
                    if let Some(f) = cur.take() {
                        h.families.push(f);
                    }
                    cur = Some(Family {
                        angle: p.f().unwrap_or(0.0) * DEG,
                        base: Point::new(0.0, 0.0),
                        offset: Point::new(0.0, 0.0),
                        dashes: Vec::new(),
                    });
                    nums = (0.0, 0.0, 0.0, 0.0);
                }
                43..=46 if in_lines => {
                    let v = p.f().unwrap_or(0.0);
                    match p.code {
                        43 => nums.0 = v,
                        44 => nums.1 = v,
                        45 => nums.2 = v,
                        _ => nums.3 = v,
                    }
                    if let Some(f) = cur.as_mut() {
                        f.base = Point::new(nums.0, nums.1);
                        f.offset = Point::new(nums.2, nums.3);
                    }
                }
                79 if in_lines => {}
                49 if in_lines => {
                    if let Some(f) = cur.as_mut() {
                        f.dashes.push(p.f().unwrap_or(0.0));
                    }
                }
                _ if in_lines => {
                    in_lines = false;
                    if let Some(f) = cur.take() {
                        h.families.push(f);
                    }
                }
                _ => {}
            }
        }
        if let Some(f) = cur.take() {
            h.families.push(f);
        }
    }
    // Gradient.
    let gflag = rest.iter().find(|p| p.code == 450).and_then(Pair::i).unwrap_or(0);
    if gflag != 0 {
        let mut g = GradientDef { name: "LINEAR".into(), ..Default::default() };
        let mut colors_started = false;
        for p in rest {
            match p.code {
                452 => g.single_color = p.i().unwrap_or(0) != 0,
                453 => colors_started = true,
                460 => g.angle = p.f().unwrap_or(0.0),
                461 => g.shift = p.f().unwrap_or(0.0),
                462 => g.tint = p.f().unwrap_or(0.0),
                470 => g.name = p.s().trim().to_ascii_uppercase(),
                63 if colors_started => g.colors.push((p.i(), None)),
                421 if colors_started => match g.colors.last_mut() {
                    Some(last) if last.1.is_none() => last.1 = p.i(),
                    _ => g.colors.push((None, p.i())),
                },
                _ => {}
            }
        }
        h.gradient = Some(g);
    }
    h
}

fn bulge_piece(a: Point, b: Point, bulge: f64) -> Piece {
    if bulge.abs() < 1e-10 || !bulge.is_finite() || near(a, b, 1e-12) {
        return Piece::Line(a, b);
    }
    let (c, r, a0, sweep) = super::geom::bulge_arc(a, b, bulge);
    Piece::Arc { c, u: Point::new(r, 0.0), v: Point::new(0.0, r), t0: a0, sweep }
}

fn spline_edge(c: &mut Cur, tol: f64, approx: &mut bool) -> Option<Piece> {
    let degree = c.i(94)?.clamp(1, 30) as usize;
    let rational = c.i(73).unwrap_or(0) != 0;
    let _periodic = c.i(74);
    let nk = c.i(95).unwrap_or(0).clamp(0, 1_000_000) as usize;
    let nc = c.i(96).unwrap_or(0).clamp(0, 1_000_000) as usize;
    let mut knots = Vec::with_capacity(nk);
    while c.peek_code() == Some(40) {
        knots.push(c.f(40).unwrap_or(0.0));
    }
    let mut ctrl = Vec::with_capacity(nc);
    let mut weights = Vec::new();
    while c.peek_code() == Some(10) {
        let x = c.f(10).unwrap_or(0.0);
        let y = c.f(20).unwrap_or(0.0);
        ctrl.push(Point::new(x, y));
        if c.peek_code() == Some(42) {
            weights.push(c.f(42).unwrap_or(1.0));
        }
    }
    let mut fit = Vec::new();
    if let Some(p) = c.peek().filter(|p| p.code == 97) {
        let n = p.i().unwrap_or(0);
        let next = c.p.get(c.i + 1).map(|p| p.code);
        if (n > 0 && next == Some(11)) || n == 0 {
            c.i += 1;
            while c.peek_code() == Some(11) {
                let x = c.f(11).unwrap_or(0.0);
                let y = c.f(21).unwrap_or(0.0);
                fit.push(Point::new(x, y));
            }
        }
    }
    let ts = c.take_set(&[12, 22]);
    let te = c.take_set(&[13, 23]);
    let tan = |ps: &[&Pair], a: i32, b: i32| -> Option<Point> {
        let p = Point::new(val(ps, a)?, val(ps, b).unwrap_or(0.0));
        (p.x.abs() + p.y.abs() > 1e-12).then_some(p)
    };
    let weights = if rational && weights.len() == ctrl.len() { weights } else { Vec::new() };
    let nurbs = Nurbs { degree, knots, ctrl: ctrl.clone(), weights };
    if let Some(b) = nurbs.to_beziers(tol) {
        *approx |= b.approximated;
        return Some(Piece::Cubics { start: b.start, segs: b.segs });
    }
    let pts = if fit.len() >= 2 { fit } else { ctrl };
    if pts.len() < 2 {
        return None;
    }
    *approx = true;
    let segs = fit_point_curve(&pts, tan(&ts, 12, 22), tan(&te, 13, 23), false);
    Some(Piece::Cubics { start: pts[0], segs })
}

/// Order the pieces into a connected chain (reversing pieces whose end meets
/// the previous end).
fn chain(pieces: Vec<Piece>) -> Vec<Piece> {
    let scale = pieces
        .iter()
        .flat_map(|p| [p.start(), p.end()])
        .fold(0.0f64, |m, p| m.max(p.x.abs()).max(p.y.abs()))
        .max(1.0);
    let eps = scale * 1e-6;
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for p in pieces {
        if let Some(prev) = out.last() {
            let e = prev.end();
            if !near(p.start(), e, eps) && near(p.end(), e, eps) {
                out.push(p.reversed());
                continue;
            }
            // The first piece itself may be reversed.
            if out.len() == 1 && !near(p.start(), e, eps) && !near(p.end(), e, eps) {
                let s0 = out[0].start();
                if near(p.start(), s0, eps) || near(p.end(), s0, eps) {
                    let first = out[0].reversed();
                    out[0] = first;
                    let e = out[0].end();
                    out.push(if near(p.start(), e, eps) { p } else { p.reversed() });
                    continue;
                }
            }
        }
        out.push(p);
    }
    out
}

/// Boundary loops as subpaths in OCS.
pub fn boundary(h: &Hatch) -> Vec<Subpath> {
    let mut pb = PathBuilder::new(Matrix::IDENTITY);
    for l in &h.loops {
        let Some(first) = l.first() else { continue };
        let s = first.start();
        pb.move_to(s.x, s.y);
        for p in l {
            p.emit(&mut pb);
        }
        pb.close();
    }
    pb.subpaths.retain(|s| !s.segments.is_empty());
    pb.subpaths
}

pub fn dot(a: Point, b: Point) -> f64 {
    a.x * b.x + a.y * b.y
}

fn det(a: Point, b: Point) -> f64 {
    a.x * b.y - a.y * b.x
}

fn len(a: Point) -> f64 {
    dot(a, a).sqrt()
}

fn sub(a: Point, b: Point) -> Point {
    Point::new(a.x - b.x, a.y - b.y)
}

fn add(a: Point, b: Point) -> Point {
    Point::new(a.x + b.x, a.y + b.y)
}

fn mul(a: Point, s: f64) -> Point {
    Point::new(a.x * s, a.y * s)
}

/// Rational approximation p/q of `x` with q ≤ `max_q`.
fn rationalize(x: f64, max_q: i64) -> Option<(i64, i64)> {
    for q in 1..=max_q {
        let p = (x * q as f64).round();
        if (x * q as f64 - p).abs() <= 1e-6 * (1.0 + x.abs() * q as f64) {
            return Some((p as i64, q));
        }
    }
    None
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

/// A translation lattice under which every family is invariant: basis
/// vectors (v1, v2) in OCS with det(v1, v2) > 0. `None` if the families do
/// not share a lattice (or it would be unreasonably large).
pub fn common_lattice(fams: &[Family]) -> Option<(Point, Point)> {
    // Each family contributes linear functionals g with t·g ∈ ℤ.
    let mut gs: Vec<Point> = Vec::new();
    for f in fams {
        let h = f.spacing();
        if !h.is_finite() || h.abs() < 1e-9 {
            return None;
        }
        if f.continuous() {
            gs.push(mul(f.normal(), 1.0 / h));
        } else {
            let a = mul(f.dir(), f.period());
            let b = f.offset;
            let dt = det(a, b);
            if dt.abs() < 1e-12 {
                return None;
            }
            gs.push(Point::new(b.y / dt, -b.x / dt));
            gs.push(Point::new(-a.y / dt, a.x / dt));
        }
    }
    let ga = *gs.first()?;
    let gb = gs
        .iter()
        .copied()
        .filter(|g| det(ga, *g).abs() > 1e-6 * len(ga) * len(*g))
        .max_by(|a, b| (det(ga, *a).abs() / len(*a)).total_cmp(&(det(ga, *b).abs() / len(*b))));
    let gb = match gb {
        Some(g) => g,
        None => {
            // All families parallel and continuous: choose a period along the
            // lines equal to the line spacing.
            let f = &fams[0];
            mul(f.dir(), 1.0 / f.spacing().abs())
        }
    };
    let dab = det(ga, gb);
    // Dual basis: e_a·g_a = 1, e_a·g_b = 0, …
    let ea = Point::new(gb.y / dab, -gb.x / dab);
    let eb = Point::new(-ga.y / dab, ga.x / dab);
    // Express every g in (ga, gb) with rational coefficients.
    let mut coeffs: Vec<((i64, i64), (i64, i64))> = Vec::new();
    let mut dmax = 1i64;
    for g in &gs {
        let al = dot(*g, ea);
        let be = dot(*g, eb);
        let ra = rationalize(al, 12)?;
        let rb = rationalize(be, 12)?;
        dmax = dmax / gcd(dmax, ra.1) * ra.1;
        dmax = dmax / gcd(dmax, rb.1) * rb.1;
        if dmax > 48 {
            return None;
        }
        coeffs.push((ra, rb));
    }
    let valid = |i: i64, j: i64| {
        coeffs.iter().all(|((pa, qa), (pb, qb))| {
            // i·pa/qa + j·pb/qb ∈ ℤ
            let num = i * pa * qb + j * pb * qa;
            num % (qa * qb) == 0
        })
    };
    let d = dmax;
    let mut cands: Vec<Point> = Vec::new();
    for i in -d..=d {
        for j in -d..=d {
            if (i, j) != (0, 0) && valid(i, j) {
                cands.push(add(mul(ea, i as f64), mul(eb, j as f64)));
            }
        }
    }
    let v1 = *cands.iter().min_by(|a, b| len(**a).total_cmp(&len(**b)))?;
    let v2 = *cands
        .iter()
        .filter(|v| det(v1, **v).abs() > 1e-9 * len(v1) * len(**v))
        .min_by(|a, b| det(v1, **a).abs().total_cmp(&det(v1, **b).abs()).then(len(**a).total_cmp(&len(**b))))?;
    // Lagrange–Gauss reduction (least shear).
    let (mut v1, mut v2) = (v1, v2);
    for _ in 0..64 {
        if len(v2) < len(v1) {
            std::mem::swap(&mut v1, &mut v2);
        }
        let mu = (dot(v1, v2) / dot(v1, v1)).round();
        if mu == 0.0 {
            break;
        }
        v2 = sub(v2, mul(v1, mu));
    }
    // Prefer v1 along the first family's lines.
    let d0 = fams[0].dir();
    if (dot(v2, d0) / len(v2)).abs() > (dot(v1, d0) / len(v1)).abs() + 1e-9 {
        std::mem::swap(&mut v1, &mut v2);
    }
    if dot(v1, d0) < -1e-9 {
        v1 = mul(v1, -1.0);
    }
    if det(v1, v2) < 0.0 {
        v2 = mul(v2, -1.0);
    }
    Some((v1, v2))
}

/// Dash intervals of one line within `[s0, s1]` (parameter = OCS distance
/// from the line origin). Dots become intervals of length `dot_len`.
fn dash_intervals(f: &Family, s0: f64, s1: f64, dot_len: f64, budget: &mut usize, out: &mut Vec<(f64, f64)>) {
    if s1 <= s0 {
        return;
    }
    if f.continuous() {
        if *budget > 0 {
            *budget -= 1;
            out.push((s0, s1));
        }
        return;
    }
    let period = f.period();
    let k0 = (s0 / period).floor() as i64;
    let k1 = (s1 / period).ceil() as i64;
    for k in k0..=k1 {
        let mut pos = k as f64 * period;
        for &d in &f.dashes {
            let (a, b) = if d > 0.0 {
                (pos, pos + d)
            } else if d == 0.0 {
                (pos - dot_len / 2.0, pos + dot_len / 2.0)
            } else {
                pos += d.abs();
                continue;
            };
            if d > 0.0 {
                pos += d;
            }
            let (a, b) = (a.max(s0), b.min(s1));
            if b > a || (d == 0.0 && b >= a) {
                if *budget == 0 {
                    return;
                }
                *budget -= 1;
                out.push((a, b));
            }
        }
    }
}

/// Liang–Barsky: parameter range of `o + s·d` inside `r`.
fn clip_line(o: Point, d: Point, r: &Rect) -> Option<(f64, f64)> {
    let mut t0 = f64::NEG_INFINITY;
    let mut t1 = f64::INFINITY;
    for (p, q0, lo, hi) in [(d.x, o.x, r.llx, r.urx), (d.y, o.y, r.lly, r.ury)] {
        if p.abs() < 1e-15 {
            if q0 < lo || q0 > hi {
                return None;
            }
        } else {
            let (a, b) = ((lo - q0) / p, (hi - q0) / p);
            let (a, b) = if a < b { (a, b) } else { (b, a) };
            t0 = t0.max(a);
            t1 = t1.min(b);
        }
    }
    (t1 > t0).then_some((t0, t1))
}

/// Stroked hatch line segments of all families clipped to `region`, where
/// `to_region` maps OCS into the region's space (pattern space or OCS) and
/// `out_m` maps OCS into the output space of the returned subpaths.
/// Returns `None` if the segment budget ran out.
pub fn lines_in_region(
    fams: &[Family],
    region: &Rect,
    to_region: &Matrix,
    out_m: &Matrix,
    dot_len: f64,
    budget: usize,
) -> (Vec<Subpath>, bool) {
    let mut out = Vec::new();
    let mut budget_left = budget;
    let corners = [
        Point::new(region.llx, region.lly),
        Point::new(region.urx, region.lly),
        Point::new(region.urx, region.ury),
        Point::new(region.llx, region.ury),
    ];
    for f in fams {
        let d = f.dir();
        let base = to_region.apply(f.base);
        let lin = |v: Point| Point::new(to_region.a * v.x + to_region.c * v.y, to_region.b * v.x + to_region.d * v.y);
        let dr = lin(d);
        let or = lin(f.offset);
        let nr = Point::new(-dr.y, dr.x);
        let step = dot(or, nr);
        if step.abs() < 1e-12 || len(dr) < 1e-15 {
            continue;
        }
        let b0 = dot(base, nr);
        let (mut cmin, mut cmax) = (f64::INFINITY, f64::NEG_INFINITY);
        for c in &corners {
            let v = dot(*c, nr);
            cmin = cmin.min(v);
            cmax = cmax.max(v);
        }
        let (ka, kb) = ((cmin - b0) / step, (cmax - b0) / step);
        let (k0, k1) = (ka.min(kb).ceil() as i64, ka.max(kb).floor() as i64);
        if k1 < k0 {
            continue;
        }
        if (k1 - k0) as usize > budget_left {
            return (out, true);
        }
        let mut iv = Vec::new();
        for k in k0..=k1 {
            let o = add(base, mul(or, k as f64));
            let Some((s0, s1)) = clip_line(o, dr, region) else { continue };
            iv.clear();
            dash_intervals(f, s0, s1, dot_len, &mut budget_left, &mut iv);
            let origin_ocs = add(f.base, mul(f.offset, k as f64));
            for &(a, b) in &iv {
                let pa = out_m.apply(add(origin_ocs, mul(d, a)));
                let pb = out_m.apply(add(origin_ocs, mul(d, b)));
                let mut sp = Subpath::new(pa);
                sp.segments.push(Segment::Line(pb));
                out.push(sp);
            }
            if budget_left == 0 {
                return (out, true);
            }
        }
    }
    (out, false)
}

/// Build a native tiling pattern for the families. Returns the pattern
/// (name and elements filled in) and the pattern → OCS matrix.
pub fn build_tile(
    name: &str,
    fams: &[Family],
    stroke: &Stroke,
    ocs_to_page_scale: f64,
) -> Option<(Pattern, Matrix)> {
    let (v1, v2) = common_lattice(fams)?;
    let w = len(v1);
    let hgt = det(v1, v2) / w;
    if !(w > 1e-9 && hgt > 1e-9) {
        return None;
    }
    // Pattern space → OCS: origin at the first base point minus half a cell
    // (keeps lines off the tile edges where possible).
    let origin = sub(fams[0].base, mul(add(v1, v2), 0.5));
    let u = mul(v1, 1.0 / w);
    let v = mul(v2, 1.0 / hgt);
    let m = Matrix::new(u.x, u.y, v.x, v.y, origin.x, origin.y);
    let inv = crate::patterns::invert(&m)?;
    // Pattern space has the scale of OCS (|u| = 1), shear aside.
    let page_scale = ocs_to_page_scale * m.mean_scale();
    if page_scale <= 0.0 || !page_scale.is_finite() {
        return None;
    }
    let width_pat = stroke.width / page_scale;
    let margin = width_pat.max(w.max(hgt) * 1e-3) * 2.0;
    let cell = Rect { llx: 0.0, lly: 0.0, urx: w, ury: hgt };
    let region = Rect { llx: -margin, lly: -margin, urx: w + margin, ury: hgt + margin };
    let dot_len = width_pat.max(w.min(hgt) * 1e-3);
    let (subpaths, overflow) = lines_in_region(fams, &region, &inv, &inv, dot_len, 4000);
    if overflow || subpaths.is_empty() {
        return None;
    }
    let mut item = PathItem::new(subpaths);
    let mut s = stroke.clone();
    s.width = width_pat;
    s.dash.clear();
    s.dash_phase = 0.0;
    item.stroke = Some(s);
    let pattern = Pattern { name: name.to_string(), bbox: cell, x_step: w, y_step: hgt, elements: vec![Element::Path(item)] };
    Some((pattern, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fam(angle_deg: f64, base: (f64, f64), off: (f64, f64), dashes: &[f64]) -> Family {
        Family {
            angle: angle_deg * DEG,
            base: Point::new(base.0, base.1),
            offset: Point::new(off.0, off.1),
            dashes: dashes.to_vec(),
        }
    }

    #[test]
    fn ansi31_lattice() {
        // 45° lines spaced 3.175 apart.
        let s = 3.175 / 2f64.sqrt();
        let f = fam(45.0, (0.0, 0.0), (-s, s), &[]);
        let (v1, v2) = common_lattice(&[f]).unwrap();
        assert!((det(v1, v2) - 3.175 * 3.175).abs() < 1e-6);
        assert!(dot(v1, v2).abs() < 1e-9);
    }

    #[test]
    fn crosshatch_lattice() {
        let f1 = fam(45.0, (0.0, 0.0), (-1.0, 1.0), &[]);
        let f2 = fam(135.0, (0.0, 0.0), (-1.0, -1.0), &[]);
        let (v1, v2) = common_lattice(&[f1.clone(), f2.clone()]).unwrap();
        for v in [v1, v2] {
            for f in [&f1, &f2] {
                let k = dot(v, f.normal()) / f.spacing();
                assert!((k - k.round()).abs() < 1e-9);
            }
        }
        assert!((det(v1, v2) - 2.0).abs() < 1e-9, "{}", det(v1, v2));
    }

    #[test]
    fn dashed_with_shift_lattice() {
        // Brick-like: horizontal continuous + vertical dashed family.
        let f1 = fam(0.0, (0.0, 0.0), (0.0, 1.0), &[]);
        let f2 = fam(90.0, (0.0, 0.0), (-1.0, 1.0), &[1.0, -1.0]);
        let (v1, v2) = common_lattice(&[f1, f2]).unwrap();
        assert!((det(v1, v2) - 2.0).abs() < 1e-9, "{v1:?} {v2:?}");
    }

    #[test]
    fn incommensurable() {
        let f1 = fam(0.0, (0.0, 0.0), (0.0, 1.0), &[]);
        let f2 = fam(90.0, (0.0, 0.0), (-std::f64::consts::PI, 0.0), &[]);
        // Two perpendicular families always tile (1 × π cell)...
        assert!(common_lattice(&[f1.clone(), f2.clone()]).is_some());
        // ...but a third 45° family with spacing 1 does not fit that lattice.
        let f3 = fam(45.0, (0.0, 0.0), (-0.5f64.sqrt(), 0.5f64.sqrt()), &[]);
        assert!(common_lattice(&[f1, f2, f3]).is_none());
    }
}
