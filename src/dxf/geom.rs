//! Geometry helpers: arcs, bulges, ellipses, NURBS → cubic Bézier, tight
//! bounds and OCS (arbitrary axis) matrices.

use crate::ir::{Element, Matrix, Point, Rect, Segment, Subpath};
use std::f64::consts::{FRAC_PI_2, PI, TAU};

pub type P3 = [f64; 3];

fn cross(a: P3, b: P3) -> P3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(a: P3) -> Option<P3> {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    (l > 1e-12 && l.is_finite()).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

/// Is `n` the default +Z extrusion?
pub fn is_default_normal(n: P3) -> bool {
    n[0].abs() < 1e-9 && n[1].abs() < 1e-9 && n[2] > 0.0
}

/// Matrix mapping OCS (x, y) at OCS elevation `z` to the WCS XY plane (top
/// view, WCS z dropped), using AutoCAD's arbitrary axis algorithm.
pub fn ocs_matrix(n: P3, z: f64) -> Matrix {
    let Some(n) = norm(n) else { return Matrix::IDENTITY };
    if is_default_normal(n) {
        return Matrix::IDENTITY;
    }
    let ax = if n[0].abs() < 1.0 / 64.0 && n[1].abs() < 1.0 / 64.0 {
        cross([0.0, 1.0, 0.0], n)
    } else {
        cross([0.0, 0.0, 1.0], n)
    };
    let Some(ax) = norm(ax) else { return Matrix::IDENTITY };
    let ay = cross(n, ax);
    Matrix::new(ax[0], ax[1], ay[0], ay[1], n[0] * z, n[1] * z)
}

/// Minor axis direction of an ELLIPSE: `normal × major`.
pub fn ellipse_minor(normal: P3, major: P3, ratio: f64) -> P3 {
    let n = norm(normal).unwrap_or([0.0, 0.0, 1.0]);
    let m = cross(n, major);
    [m[0] * ratio, m[1] * ratio, m[2] * ratio]
}

/// Builds subpaths, transforming local coordinates by a matrix.
pub struct PathBuilder {
    pub m: Matrix,
    pub subpaths: Vec<Subpath>,
    cur: Option<Point>,
}

impl PathBuilder {
    pub fn new(m: Matrix) -> Self {
        Self { m, subpaths: Vec::new(), cur: None }
    }
    pub fn move_to(&mut self, x: f64, y: f64) {
        let p = self.m.apply(Point::new(x, y));
        self.subpaths.push(Subpath::new(p));
        self.cur = Some(Point::new(x, y));
    }
    pub fn line_to(&mut self, x: f64, y: f64) {
        if self.subpaths.is_empty() {
            self.move_to(x, y);
            return;
        }
        let p = self.m.apply(Point::new(x, y));
        if let Some(sp) = self.subpaths.last_mut() {
            sp.segments.push(Segment::Line(p));
        }
        self.cur = Some(Point::new(x, y));
    }
    pub fn curve_to(&mut self, a: Point, b: Point, c: Point) {
        if self.subpaths.is_empty() {
            self.move_to(a.x, a.y);
        }
        let (a, b, c2) = (self.m.apply(a), self.m.apply(b), self.m.apply(c));
        if let Some(sp) = self.subpaths.last_mut() {
            sp.segments.push(Segment::Curve(a, b, c2));
        }
        self.cur = Some(c);
    }
    /// Current point in local coordinates.
    pub fn current(&self) -> Option<Point> {
        self.cur
    }
    pub fn close(&mut self) {
        if let Some(sp) = self.subpaths.last_mut() {
            // Drop a final segment that just returns to the start.
            if let Some(Segment::Line(p)) = sp.segments.last()
                && near(*p, sp.start, 1e-9)
                && sp.segments.len() > 1
            {
                sp.segments.pop();
            }
            sp.closed = true;
        }
    }
    /// Circular arc (in local coordinates) from the current point; the arc
    /// starts at angle `a0` and sweeps `sweep` radians (positive = CCW).
    /// If there is no current point, a new subpath is started at the arc start.
    pub fn arc(&mut self, c: Point, r: f64, a0: f64, sweep: f64) {
        self.ellipse_arc(c, Point::new(r, 0.0), Point::new(0.0, r), a0, sweep);
    }
    /// Elliptical arc `c + u·cos t + v·sin t` for t from `t0` sweeping `sweep`.
    pub fn ellipse_arc(&mut self, c: Point, u: Point, v: Point, t0: f64, sweep: f64) {
        let at = |t: f64| Point::new(c.x + u.x * t.cos() + v.x * t.sin(), c.y + u.y * t.cos() + v.y * t.sin());
        let d = |t: f64| Point::new(-u.x * t.sin() + v.x * t.cos(), -u.y * t.sin() + v.y * t.cos());
        let start = at(t0);
        match self.cur {
            None => self.move_to(start.x, start.y),
            Some(p) if !near(p, start, 1e-9 * (1.0 + start.x.abs() + start.y.abs())) => self.line_to(start.x, start.y),
            _ => {}
        }
        if !sweep.is_finite() || sweep.abs() < 1e-12 {
            return;
        }
        let n = ((sweep.abs() / FRAC_PI_2) - 1e-9).ceil().max(1.0) as usize;
        let n = n.min(64);
        let dt = sweep / n as f64;
        let k = 4.0 / 3.0 * (dt / 4.0).tan();
        for i in 0..n {
            let ta = t0 + dt * i as f64;
            let tb = ta + dt;
            let (pa, pb) = (at(ta), at(tb));
            let (da, db) = (d(ta), d(tb));
            self.curve_to(
                Point::new(pa.x + k * da.x, pa.y + k * da.y),
                Point::new(pb.x - k * db.x, pb.y - k * db.y),
                pb,
            );
        }
    }
    /// Polyline segment with a DXF bulge from the current point to (x, y).
    pub fn bulge_to(&mut self, bulge: f64, x: f64, y: f64) {
        let Some(p0) = self.cur else {
            self.move_to(x, y);
            return;
        };
        let p1 = Point::new(x, y);
        if !bulge.is_finite() || bulge.abs() < 1e-10 || near(p0, p1, 1e-12) {
            self.line_to(x, y);
            return;
        }
        let (c, r, a0, sweep) = bulge_arc(p0, p1, bulge);
        self.arc(c, r, a0, sweep);
        // Snap exactly onto the vertex.
        if let Some(Some(Segment::Curve(_, _, e))) = self.subpaths.last_mut().map(|s| s.segments.last_mut()) {
            *e = self.m.apply(p1);
        }
        self.cur = Some(p1);
    }
}

/// Centre, radius, start angle and signed sweep of a bulged segment.
pub fn bulge_arc(p0: Point, p1: Point, bulge: f64) -> (Point, f64, f64, f64) {
    let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
    let mid = Point::new((p0.x + p1.x) / 2.0, (p0.y + p1.y) / 2.0);
    let f = (1.0 - bulge * bulge) / (4.0 * bulge);
    let c = Point::new(mid.x - dy * f, mid.y + dx * f);
    let r = ((p0.x - c.x).powi(2) + (p0.y - c.y).powi(2)).sqrt();
    let a0 = (p0.y - c.y).atan2(p0.x - c.x);
    let sweep = 4.0 * bulge.atan();
    (c, r, a0, sweep)
}

pub fn near(a: Point, b: Point, eps: f64) -> bool {
    (a.x - b.x).abs() <= eps && (a.y - b.y).abs() <= eps
}

/// Normalise a CCW sweep from `a0` to `a1` into (0, 2π].
pub fn ccw_sweep(a0: f64, a1: f64) -> f64 {
    let mut s = (a1 - a0) % TAU;
    if s <= 1e-12 {
        s += TAU;
    }
    s
}

// ---------------------------------------------------------------------------
// Curve fitting
// ---------------------------------------------------------------------------

/// Approximate the parametric curve `f` on `[t0, t1]` by cubic Béziers
/// (Hermite interpolation with numerical derivatives, subdivided until the
/// deviation is below `tol`). Appends to `out`.
pub fn fit_cubics(f: &dyn Fn(f64) -> Point, t0: f64, t1: f64, tol: f64, out: &mut Vec<[Point; 3]>) {
    fn deriv(f: &dyn Fn(f64) -> Point, t: f64, a: f64, b: f64) -> Point {
        let h = (b - a) * 1e-4;
        // One-sided at the interval ends so that evaluations stay inside.
        let (ta, tb) = if t - h < a { (t, t + h) } else if t + h > b { (t - h, t) } else { (t - h, t + h) };
        let (pa, pb) = (f(ta), f(tb));
        Point::new((pb.x - pa.x) / (tb - ta), (pb.y - pa.y) / (tb - ta))
    }
    fn rec(f: &dyn Fn(f64) -> Point, a: f64, b: f64, tol: f64, depth: u32, out: &mut Vec<[Point; 3]>, budget: &mut usize) {
        let (p0, p3) = (f(a), f(b));
        let (d0, d1) = (deriv(f, a, a, b), deriv(f, b, a, b));
        let s = (b - a) / 3.0;
        let c1 = Point::new(p0.x + d0.x * s, p0.y + d0.y * s);
        let c2 = Point::new(p3.x - d1.x * s, p3.y - d1.y * s);
        let mut err: f64 = 0.0;
        for k in 1..8 {
            let u = k as f64 / 8.0;
            let q = f(a + (b - a) * u);
            let b3 = bez(p0, c1, c2, p3, u);
            err = err.max(((q.x - b3.x).powi(2) + (q.y - b3.y).powi(2)).sqrt());
        }
        if (err <= tol || depth >= 12 || *budget == 0) && err.is_finite() {
            out.push([c1, c2, p3]);
            return;
        }
        if !err.is_finite() {
            out.push([p0, p3, p3]);
            return;
        }
        *budget = budget.saturating_sub(1);
        let m = (a + b) / 2.0;
        rec(f, a, m, tol, depth + 1, out, budget);
        rec(f, m, b, tol, depth + 1, out, budget);
    }
    let mut budget = 4096;
    rec(f, t0, t1, tol, 0, out, &mut budget);
}

pub fn bez(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    Point::new(a * p0.x + b * p1.x + c * p2.x + d * p3.x, a * p0.y + b * p1.y + c * p2.y + d * p3.y)
}

// ---------------------------------------------------------------------------
// NURBS
// ---------------------------------------------------------------------------

/// A B-spline curve in 2D (homogeneous weights).
#[derive(Debug, Clone)]
pub struct Nurbs {
    pub degree: usize,
    pub knots: Vec<f64>,
    pub ctrl: Vec<Point>,
    /// Empty = non-rational.
    pub weights: Vec<f64>,
}

/// Output of [`Nurbs::to_beziers`].
pub struct BezierOut {
    pub start: Point,
    /// Cubic segments (control 1, control 2, end).
    pub segs: Vec<[Point; 3]>,
    /// The result is an approximation (rational or degree > 3).
    pub approximated: bool,
}

impl Nurbs {
    pub fn is_valid(&self) -> bool {
        let n = self.ctrl.len();
        self.degree >= 1
            && n > self.degree
            && self.knots.len() == n + self.degree + 1
            && self.knots.windows(2).all(|w| w[1] >= w[0] - 1e-12)
            && self.knots.iter().all(|k| k.is_finite())
            && self.ctrl.iter().all(|p| p.x.is_finite() && p.y.is_finite())
            && (self.weights.is_empty() || (self.weights.len() == n && self.weights.iter().all(|w| w.is_finite() && *w > 0.0)))
            && self.knots[n] > self.knots[self.degree]
    }

    fn rational(&self) -> bool {
        !self.weights.is_empty() && self.weights.iter().any(|w| (w - self.weights[0]).abs() > 1e-12 * w.abs().max(1.0))
    }

    /// Homogeneous control points (wx, wy, w).
    fn homog(&self) -> Vec<[f64; 3]> {
        self.ctrl
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let w = if self.rational() { self.weights[i] } else { 1.0 };
                [p.x * w, p.y * w, w]
            })
            .collect()
    }

    /// De Boor evaluation at `t` (clamped to the domain).
    pub fn eval(&self, t: f64) -> Point {
        let p = self.degree;
        let n = self.ctrl.len();
        let u = &self.knots;
        let t = t.clamp(u[p], u[n]);
        // span k: u[k] <= t < u[k+1], p <= k <= n-1
        let mut k = p;
        while k < n - 1 && u[k + 1] <= t {
            k += 1;
        }
        let h = self.homog();
        let mut d: Vec<[f64; 3]> = (0..=p).map(|j| h[j + k - p]).collect();
        for r in 1..=p {
            for j in (r..=p).rev() {
                let i = j + k - p;
                let den = u[i + p + 1 - r] - u[i];
                let a = if den.abs() > 1e-300 { (t - u[i]) / den } else { 0.0 };
                let prev = d[j - 1];
                for (c, v) in d[j].iter_mut().enumerate() {
                    *v = (1.0 - a) * prev[c] + a * *v;
                }
            }
        }
        let w = if d[p][2].abs() > 1e-300 { d[p][2] } else { 1.0 };
        Point::new(d[p][0] / w, d[p][1] / w)
    }

    /// Convert to cubic Béziers: exact (knot insertion / Bézier decomposition
    /// plus degree elevation) for non-rational splines of degree ≤ 3, fitted
    /// within `tol` otherwise.
    pub fn to_beziers(&self, tol: f64) -> Option<BezierOut> {
        if !self.is_valid() {
            return None;
        }
        let p = self.degree;
        let n = self.ctrl.len();
        let (a, b) = (self.knots[p], self.knots[n]);
        if self.rational() || p > 3 {
            let mut segs = Vec::new();
            // Fit span by span so that kinks at knots are kept.
            let mut spans: Vec<f64> = self.knots[p..=n].to_vec();
            spans.dedup_by(|x, y| (*x - *y).abs() < 1e-12);
            for w in spans.windows(2) {
                fit_cubics(&|t| self.eval(t), w[0], w[1], tol, &mut segs);
            }
            return Some(BezierOut { start: self.eval(a), segs, approximated: true });
        }
        // Knot insertion until every distinct knot in [a, b] has multiplicity p.
        let mut u = self.knots.clone();
        let mut q = self.homog();
        let mut distinct: Vec<f64> = u.iter().copied().filter(|k| *k >= a && *k <= b).collect();
        distinct.dedup_by(|x, y| (*x - *y).abs() <= 1e-12);
        for &v in &distinct {
            let mult = u.iter().filter(|k| (**k - v).abs() <= 1e-12).count();
            for _ in mult..p {
                insert_knot(&mut u, &mut q, p, v)?;
            }
        }
        let mut segs = Vec::new();
        let m = q.len();
        let mut start = None;
        for k in p..m {
            if k + 1 >= u.len() || u[k + 1] - u[k] <= 1e-12 || u[k] < a - 1e-12 || u[k + 1] > b + 1e-12 {
                continue;
            }
            if k < p {
                return None;
            }
            let pts: Vec<Point> = q[k - p..=k].iter().map(|h| Point::new(h[0] / h[2], h[1] / h[2])).collect();
            let cubic = elevate_to_cubic(&pts);
            if start.is_none() {
                start = Some(cubic[0]);
            }
            segs.push([cubic[1], cubic[2], cubic[3]]);
        }
        Some(BezierOut { start: start?, segs, approximated: false })
    }
}

/// Boehm single knot insertion of `t`.
fn insert_knot(u: &mut Vec<f64>, q: &mut Vec<[f64; 3]>, p: usize, t: f64) -> Option<()> {
    let n = q.len();
    // last k with u[k] <= t, limited so that k+1 exists in the control range
    let k = u.iter().rposition(|x| *x <= t + 1e-12)?;
    if k < p {
        return None;
    }
    let mut nq = Vec::with_capacity(n + 1);
    for i in 0..=n {
        if i + p <= k {
            nq.push(q[i]);
        } else if i > k {
            nq.push(q[i - 1]);
        } else {
            let den = u.get(i + p).map_or(0.0, |v| v - u[i]);
            let a = if den.abs() > 1e-300 { ((t - u[i]) / den).clamp(0.0, 1.0) } else { 0.0 };
            if a <= 0.0 || i >= n {
                nq.push(q[i - 1]);
                continue;
            }
            let (pa, pb) = (q[i - 1], q[i]);
            nq.push([(1.0 - a) * pa[0] + a * pb[0], (1.0 - a) * pa[1] + a * pb[1], (1.0 - a) * pa[2] + a * pb[2]]);
        }
    }
    u.insert(k + 1, t);
    *q = nq;
    Some(())
}

/// Degree elevation of a Bézier of degree 1..=3 to a cubic.
fn elevate_to_cubic(p: &[Point]) -> [Point; 4] {
    let l = |a: Point, b: Point, t: f64| Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
    match p.len() {
        2 => [p[0], l(p[0], p[1], 1.0 / 3.0), l(p[0], p[1], 2.0 / 3.0), p[1]],
        3 => [p[0], l(p[0], p[1], 2.0 / 3.0), l(p[2], p[1], 2.0 / 3.0), p[2]],
        _ => [p[0], p[1], p[2], p[3]],
    }
}

/// Smooth curve through fit points (chord-length parametrised Catmull-Rom,
/// optional end tangent directions). Returns cubic segments.
pub fn fit_point_curve(pts: &[Point], t_start: Option<Point>, t_end: Option<Point>, closed: bool) -> Vec<[Point; 3]> {
    let n = pts.len();
    let mut out = Vec::new();
    if n < 2 {
        return out;
    }
    let dist = |a: Point, b: Point| ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
    let unit = |v: Point| {
        let l = (v.x * v.x + v.y * v.y).sqrt();
        if l > 1e-12 { Point::new(v.x / l, v.y / l) } else { Point::new(0.0, 0.0) }
    };
    // Tangent direction at each point.
    let tan: Vec<Point> = (0..n)
        .map(|i| {
            let prev = if i > 0 { Some(pts[i - 1]) } else if closed { Some(pts[n - 2.min(n - 1)]) } else { None };
            let next = if i + 1 < n { Some(pts[i + 1]) } else if closed { Some(pts[1.min(n - 1)]) } else { None };
            if i == 0 && let Some(t) = t_start {
                return unit(t);
            }
            if i == n - 1 && let Some(t) = t_end {
                return unit(t);
            }
            match (prev, next) {
                (Some(a), Some(b)) => {
                    let (da, db) = (dist(a, pts[i]).max(1e-12), dist(pts[i], b).max(1e-12));
                    // Weighted (chord length) central difference.
                    let va = Point::new((pts[i].x - a.x) / da, (pts[i].y - a.y) / da);
                    let vb = Point::new((b.x - pts[i].x) / db, (b.y - pts[i].y) / db);
                    unit(Point::new(va.x * db + vb.x * da, va.y * db + vb.y * da))
                }
                (None, Some(b)) => unit(Point::new(b.x - pts[i].x, b.y - pts[i].y)),
                (Some(a), None) => unit(Point::new(pts[i].x - a.x, pts[i].y - a.y)),
                (None, None) => Point::new(0.0, 0.0),
            }
        })
        .collect();
    for i in 0..n - 1 {
        let (a, b) = (pts[i], pts[i + 1]);
        let d = dist(a, b) / 3.0;
        out.push([
            Point::new(a.x + tan[i].x * d, a.y + tan[i].y * d),
            Point::new(b.x - tan[i + 1].x * d, b.y - tan[i + 1].y * d),
            b,
        ]);
    }
    out
}

// ---------------------------------------------------------------------------
// Tight bounds
// ---------------------------------------------------------------------------

fn extend(r: &mut Option<Rect>, p: Point) {
    let b = r.get_or_insert(Rect { llx: p.x, lly: p.y, urx: p.x, ury: p.y });
    b.llx = b.llx.min(p.x);
    b.lly = b.lly.min(p.y);
    b.urx = b.urx.max(p.x);
    b.ury = b.ury.max(p.y);
}

/// Parameters in (0,1) where a cubic's 1D derivative vanishes.
fn extrema_1d(a: f64, b: f64, c: f64, d: f64) -> Vec<f64> {
    // derivative coefficients: 3[(b-a)(1-t)^2 + 2(c-b)(1-t)t + (d-c)t^2]
    let (p, q, r) = (b - a, c - b, d - c);
    let qa = p - 2.0 * q + r;
    let qb = 2.0 * (q - p);
    let qc = p;
    let mut out = Vec::new();
    if qa.abs() < 1e-12 {
        if qb.abs() > 1e-12 {
            out.push(-qc / qb);
        }
    } else {
        let disc = qb * qb - 4.0 * qa * qc;
        if disc >= 0.0 {
            let s = disc.sqrt();
            out.push((-qb + s) / (2.0 * qa));
            out.push((-qb - s) / (2.0 * qa));
        }
    }
    out.retain(|t| *t > 0.0 && *t < 1.0);
    out
}

pub fn tight_subpath_bounds(sps: &[Subpath]) -> Option<Rect> {
    let mut r = None;
    for sp in sps {
        let mut cur = sp.start;
        extend(&mut r, cur);
        for s in &sp.segments {
            match s {
                Segment::Line(p) => {
                    extend(&mut r, *p);
                    cur = *p;
                }
                Segment::Curve(a, b, c) => {
                    extend(&mut r, *c);
                    let mut ts = extrema_1d(cur.x, a.x, b.x, c.x);
                    ts.extend(extrema_1d(cur.y, a.y, b.y, c.y));
                    for t in ts {
                        extend(&mut r, bez(cur, *a, *b, *c, t));
                    }
                    cur = *c;
                }
            }
        }
    }
    r
}

pub fn tight_bounds(els: &[Element]) -> Option<Rect> {
    let mut r: Option<Rect> = None;
    let mut add = |b: Option<Rect>| {
        if let Some(b) = b {
            extend(&mut r, Point::new(b.llx, b.lly));
            extend(&mut r, Point::new(b.urx, b.ury));
        }
    };
    for e in els {
        match e {
            Element::Path(p) => add(tight_subpath_bounds(&p.subpaths)),
            Element::Group(c) => add(tight_bounds(c)),
            Element::ClipGroup { clip, children, .. } => {
                if let (Some(c), Some(k)) = (tight_subpath_bounds(clip), tight_bounds(children)) {
                    let i = Rect { llx: c.llx.max(k.llx), lly: c.lly.max(k.lly), urx: c.urx.min(k.urx), ury: c.ury.min(k.ury) };
                    if i.llx <= i.urx && i.lly <= i.ury {
                        add(Some(i));
                    }
                }
            }
        }
    }
    r
}

/// Rotation matrix by `a` radians.
pub fn rotate(a: f64) -> Matrix {
    let (s, c) = a.sin_cos();
    Matrix::new(c, s, -s, c, 0.0, 0.0)
}

pub const DEG: f64 = PI / 180.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulge_semicircle() {
        let (c, r, a0, sw) = bulge_arc(Point::new(0.0, 0.0), Point::new(2.0, 0.0), 1.0);
        assert!(near(c, Point::new(1.0, 0.0), 1e-12));
        assert!((r - 1.0).abs() < 1e-12);
        assert!((a0 - PI).abs() < 1e-12);
        assert!((sw - PI).abs() < 1e-12);
    }

    #[test]
    fn ocs_mirror() {
        let m = ocs_matrix([0.0, 0.0, -1.0], 5.0);
        let p = m.apply(Point::new(3.0, 4.0));
        assert!(near(p, Point::new(-3.0, 4.0), 1e-12));
    }

    #[test]
    fn nurbs_bezier_identity() {
        // A clamped cubic with one interior knot.
        let s = Nurbs {
            degree: 3,
            knots: vec![0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0],
            ctrl: vec![
                Point::new(0.0, 0.0),
                Point::new(1.0, 2.0),
                Point::new(3.0, 3.0),
                Point::new(4.0, 0.0),
                Point::new(6.0, 1.0),
            ],
            weights: vec![],
        };
        let b = s.to_beziers(1e-6).unwrap();
        assert!(!b.approximated);
        assert_eq!(b.segs.len(), 2);
        // Compare with de Boor along the curve.
        let mut p0 = b.start;
        for (i, seg) in b.segs.iter().enumerate() {
            for k in 0..=10 {
                let u = k as f64 / 10.0;
                let t = (i as f64 + u) * 0.5;
                let q = bez(p0, seg[0], seg[1], seg[2], u);
                assert!(near(q, s.eval(t), 1e-9), "t={t} {q:?} {:?}", s.eval(t));
            }
            p0 = seg[2];
        }
    }

    #[test]
    fn unclamped_periodic_nurbs() {
        // Uniform (unclamped) quadratic B-spline.
        let s = Nurbs {
            degree: 2,
            knots: (0..8).map(|i| i as f64).collect(),
            ctrl: vec![
                Point::new(0.0, 0.0),
                Point::new(1.0, 1.0),
                Point::new(2.0, 0.0),
                Point::new(3.0, 1.0),
                Point::new(4.0, 0.0),
            ],
            weights: vec![],
        };
        let b = s.to_beziers(1e-6).unwrap();
        assert_eq!(b.segs.len(), 3);
        assert!(near(b.start, s.eval(2.0), 1e-12));
        let mut p0 = b.start;
        for (i, seg) in b.segs.iter().enumerate() {
            let q = bez(p0, seg[0], seg[1], seg[2], 0.3);
            assert!(near(q, s.eval(2.0 + i as f64 + 0.3), 1e-9));
            p0 = seg[2];
        }
    }

    #[test]
    fn rational_circle_quadrant() {
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let s = Nurbs {
            degree: 2,
            knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            ctrl: vec![Point::new(1.0, 0.0), Point::new(1.0, 1.0), Point::new(0.0, 1.0)],
            weights: vec![1.0, w, 1.0],
        };
        let b = s.to_beziers(1e-6).unwrap();
        assert!(b.approximated);
        let mut p0 = b.start;
        for seg in &b.segs {
            for k in 0..=8 {
                let q = bez(p0, seg[0], seg[1], seg[2], k as f64 / 8.0);
                assert!(((q.x * q.x + q.y * q.y).sqrt() - 1.0).abs() < 1e-5);
            }
            p0 = seg[2];
        }
    }

    #[test]
    fn tight_circle_bounds() {
        let mut pb = PathBuilder::new(Matrix::IDENTITY);
        pb.arc(Point::new(0.0, 0.0), 1.0, 0.3, TAU);
        let r = tight_subpath_bounds(&pb.subpaths).unwrap();
        assert!((r.llx + 1.0).abs() < 1e-3 && (r.urx - 1.0).abs() < 1e-3);
        assert!((r.lly + 1.0).abs() < 1e-3 && (r.ury - 1.0).abs() < 1e-3);
    }
}
