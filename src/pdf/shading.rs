//! Axial / radial PDF shadings (ShadingType 2 and 3) -> IR gradients.
//!
//! The shading function is sampled over the shading /Domain and reduced to
//! the smallest set of stops that reproduces it (piecewise-linear in the
//! colour components): exponential functions with N = 1 collapse to their two
//! end points, stitching functions keep their /Bounds as stops (including
//! colour jumps at a bound), and anything else is sampled densely and
//! simplified.

use super::color::ColorSpace;
use super::function::Function;
use super::{deref, obj_num, obj_nums};
use crate::ir::{Gradient, GradientKind, GradientStop, Point};
use lopdf::{Dictionary, Document as PdfDoc};
use std::sync::Arc;

/// Samples taken per (stitching) segment before simplification.
const SEG_SAMPLES: usize = 32;
/// Maximum deviation (in colour component units, 0..1) when dropping stops.
const TOLERANCE: f64 = 0.002;
const EPS: f64 = 1e-9;

/// A gradient together with its geometry in shading space.
#[derive(Debug, Clone)]
pub struct GradientGeom {
    pub gradient: Arc<Gradient>,
    pub start: Point,
    pub end: Point,
    pub radius: f64,
}

/// Why a shading could not be turned into a gradient.
#[derive(Debug, Clone)]
pub enum ShadingError {
    /// Function-based (1) or mesh (4..=7) shading, or an unknown type.
    Unsupported(i64),
    Malformed(String),
}

/// One sampled point: parameter position (0..=1 along the shading domain)
/// and the raw colour components.
type Sample = (f64, Vec<f64>);

fn shading_type(doc: &PdfDoc, sh: &Dictionary) -> Option<i64> {
    sh.get(b"ShadingType").ok().and_then(|o| obj_num(deref(doc, o))).map(|v| v as i64)
}

/// Convert an axial or radial shading. `warn` receives approximation notes.
pub fn gradient_from_shading(
    doc: &PdfDoc,
    sh: &Dictionary,
    cs: &ColorSpace,
    warn: &mut Vec<String>,
) -> Result<GradientGeom, ShadingError> {
    let malformed = |m: &str| ShadingError::Malformed(m.to_string());
    let stype = shading_type(doc, sh).ok_or_else(|| malformed("missing /ShadingType"))?;
    if stype != 2 && stype != 3 {
        return Err(ShadingError::Unsupported(stype));
    }
    if matches!(cs, ColorSpace::Pattern { .. }) {
        return Err(malformed("a shading cannot use the Pattern colour space"));
    }
    let coords = sh.get(b"Coords").ok().and_then(|o| obj_nums(doc, o)).ok_or_else(|| malformed("missing /Coords"))?;
    let need = if stype == 2 { 4 } else { 6 };
    if coords.len() < need {
        return Err(malformed("/Coords has too few values"));
    }
    let domain = sh.get(b"Domain").ok().and_then(|o| obj_nums(doc, o)).filter(|d| d.len() >= 2).unwrap_or(vec![0.0, 1.0]);
    let (t0, t1) = (domain[0], domain[1]);
    if (t1 - t0).abs() < EPS {
        return Err(malformed("empty /Domain"));
    }
    let func = sh
        .get(b"Function")
        .ok()
        .and_then(|f| Function::parse(doc, f))
        .ok_or_else(|| malformed("missing or unsupported /Function"))?;
    let n = cs.n_comps();
    let mut samples = sample_function(&func, t0, t1, n).ok_or_else(|| malformed("the shading function could not be evaluated"))?;

    let (kind, start, end, radius) = if stype == 2 {
        let (p0, p1) = (Point::new(coords[0], coords[1]), Point::new(coords[2], coords[3]));
        if (p0.x - p1.x).hypot(p0.y - p1.y) < EPS {
            return Err(malformed("the gradient axis has zero length"));
        }
        (GradientKind::Linear, p0, p1, 0.0)
    } else {
        let (c0, r0) = (Point::new(coords[0], coords[1]), coords[2]);
        let (c1, r1) = (Point::new(coords[3], coords[4]), coords[5]);
        if r0 < 0.0 || r1 < 0.0 {
            return Err(malformed("negative radius"));
        }
        if r0.max(r1) < EPS {
            return Err(malformed("both circles have zero radius"));
        }
        // The larger circle becomes the gradient circle, the smaller one's
        // centre the focal point; offsets are radius fractions.
        let reversed = r0 > r1;
        let (outer_c, outer_r, inner_c, inner_r) = if reversed { (c0, r0, c1, r1) } else { (c1, r1, c0, r0) };
        let d = (inner_c.x - outer_c.x).hypot(inner_c.y - outer_c.y);
        let concentric = d < 1e-6 * outer_r.max(1.0);
        let nested = d + inner_r <= outer_r * (1.0 + 1e-9);
        let exact = concentric || (inner_r < 1e-6 * outer_r && nested);
        if !exact {
            warn.push(
                "a radial gradient whose start circle is not a point (or not inside the end circle) was approximated"
                    .to_string(),
            );
        }
        // Keep the focal point strictly inside the circle.
        let focal = if d <= outer_r * 0.999 || concentric {
            inner_c
        } else {
            let k = outer_r * 0.999 / d;
            Point::new(outer_c.x + (inner_c.x - outer_c.x) * k, outer_c.y + (inner_c.y - outer_c.y) * k)
        };
        for s in samples.iter_mut() {
            s.0 = (r0 + s.0 * (r1 - r0)) / outer_r;
        }
        if reversed {
            samples.reverse();
        }
        (GradientKind::Radial, focal, outer_c, outer_r)
    };

    let mut stops = Vec::with_capacity(samples.len());
    for (offset, comps) in samples {
        let color = cs.to_color(&comps).ok_or_else(|| malformed("the shading colour space paints nothing"))?;
        stops.push(GradientStop { offset: offset.clamp(0.0, 1.0), color });
    }
    Ok(GradientGeom { gradient: Arc::new(Gradient { kind, stops }), start, end, radius })
}

/// Interior /Bounds of stitching functions (recursively through arrays).
fn bounds_of(f: &Function, out: &mut Vec<f64>) {
    match f {
        Function::Stitching { bounds, .. } => out.extend(bounds.iter().copied()),
        Function::Array(fs) => fs.iter().for_each(|g| bounds_of(g, out)),
        _ => {}
    }
}

fn eval_n(f: &Function, t: f64, n: usize) -> Option<Vec<f64>> {
    let mut v = f.eval(&[t])?;
    if v.len() < n || v.iter().take(n).any(|x| !x.is_finite()) {
        return None;
    }
    v.truncate(n);
    Some(v)
}

/// Sample `f` from `t0` to `t1` into simplified stops (offsets 0..=1).
fn sample_function(f: &Function, t0: f64, t1: f64, n: usize) -> Option<Vec<Sample>> {
    let (lo, hi) = (t0.min(t1), t0.max(t1));
    let mut cuts = Vec::new();
    bounds_of(f, &mut cuts);
    cuts.retain(|b| b.is_finite() && *b > lo + EPS && *b < hi - EPS);
    cuts.sort_by(|a, b| a.total_cmp(b));
    cuts.dedup_by(|a, b| (*a - *b).abs() < EPS);
    if t1 < t0 {
        cuts.reverse();
    }
    let mut edges = vec![t0];
    edges.extend(cuts);
    edges.push(t1);
    let span = t1 - t0;
    let off = |t: f64| (t - t0) / span;

    let mut out: Vec<Sample> = Vec::new();
    let segs = edges.len() - 1;
    for (k, w) in edges.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        let mut seg = Vec::with_capacity(SEG_SAMPLES + 1);
        for i in 0..=SEG_SAMPLES {
            let mut t = a + (b - a) * i as f64 / SEG_SAMPLES as f64;
            // Left limit at an interior bound (the right value starts the
            // next segment), so colour jumps are kept.
            if i == SEG_SAMPLES && k + 1 < segs {
                t = b - (b - a) * 1e-7;
            }
            seg.push((if i == SEG_SAMPLES { off(b) } else { off(t) }, eval_n(f, t, n)?));
        }
        let seg = simplify(&seg);
        if let (Some(last), Some(first)) = (out.last(), seg.first())
            && close(&last.1, &first.1)
        {
            out.pop();
        }
        out.extend(seg);
    }
    // A discontinuity-free result may still have collinear stops across
    // segment boundaries (e.g. a stitching function of identical slopes).
    Some(simplify(&out))
}

fn close(a: &[f64], b: &[f64]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= TOLERANCE)
}

/// Greedily drop samples that lie (within tolerance) on the straight line
/// between the kept neighbours. Two samples at the same offset (a jump) are
/// always kept.
fn simplify(pts: &[Sample]) -> Vec<Sample> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0].clone()];
    let mut anchor = 0;
    let mut j = 1;
    while j < pts.len() {
        // Try to extend the run anchor..=j+1.
        if j + 1 < pts.len() && fits(pts, anchor, j + 1) {
            j += 1;
            continue;
        }
        out.push(pts[j].clone());
        anchor = j;
        j += 1;
    }
    out
}

/// Do all samples strictly between `i` and `j` lie on the line from i to j?
fn fits(pts: &[Sample], i: usize, j: usize) -> bool {
    let (ti, tj) = (pts[i].0, pts[j].0);
    let span = tj - ti;
    if span.abs() < 1e-12 {
        return false;
    }
    pts[i + 1..j].iter().all(|(t, c)| {
        let u = (t - ti) / span;
        if !(-1e-12..=1.0 + 1e-12).contains(&u) {
            return false;
        }
        c.iter().enumerate().all(|(k, v)| {
            let expect = pts[i].1[k] + u * (pts[j].1[k] - pts[i].1[k]);
            (v - expect).abs() <= TOLERANCE
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exp(c0: Vec<f64>, c1: Vec<f64>, n: f64) -> Function {
        Function::Exponential { domain: [0.0, 1.0], c0, c1, n, range: None }
    }

    #[test]
    fn linear_function_two_stops() {
        let s = sample_function(&exp(vec![0.0], vec![1.0], 1.0), 0.0, 1.0, 1).unwrap();
        assert_eq!(s, vec![(0.0, vec![0.0]), (1.0, vec![1.0])]);
    }

    #[test]
    fn stitching_bounds_and_jumps() {
        let f = Function::Stitching {
            domain: [0.0, 1.0],
            functions: vec![exp(vec![0.0], vec![1.0], 1.0), exp(vec![0.0], vec![0.5], 1.0)],
            bounds: vec![0.25],
            encode: vec![0.0, 1.0, 0.0, 1.0],
            range: None,
        };
        let s = sample_function(&f, 0.0, 1.0, 1).unwrap();
        assert_eq!(s.len(), 4, "{s:?}");
        assert_eq!(s[1].0, 0.25);
        assert!((s[1].1[0] - 1.0).abs() < 1e-5);
        assert_eq!(s[2], (0.25, vec![0.0]));
        assert_eq!(s[3], (1.0, vec![0.5]));
    }

    #[test]
    fn continuous_stitching_keeps_bound_only_when_needed() {
        let f = Function::Stitching {
            domain: [0.0, 1.0],
            functions: vec![exp(vec![0.0], vec![0.5], 1.0), exp(vec![0.5], vec![1.0], 1.0)],
            bounds: vec![0.5],
            encode: vec![0.0, 1.0, 0.0, 1.0],
            range: None,
        };
        // Same slope on both sides: the bound is redundant.
        let s = sample_function(&f, 0.0, 1.0, 1).unwrap();
        assert_eq!(s.len(), 2, "{s:?}");
    }

    #[test]
    fn nonlinear_sampled_within_tolerance() {
        let f = exp(vec![0.0], vec![1.0], 2.0);
        let s = sample_function(&f, 0.0, 1.0, 1).unwrap();
        assert!(s.len() > 3 && s.len() < 33, "{}", s.len());
        for i in 0..=100 {
            let t = i as f64 / 100.0;
            let k = s.iter().position(|p| p.0 >= t).unwrap().max(1);
            let (a, b) = (&s[k - 1], &s[k]);
            let v = a.1[0] + (t - a.0) / (b.0 - a.0) * (b.1[0] - a.1[0]);
            assert!((v - t * t).abs() < 0.01, "t={t} v={v}");
        }
    }

    #[test]
    fn reversed_domain() {
        let s = sample_function(&exp(vec![0.0], vec![1.0], 1.0), 1.0, 0.0, 1).unwrap();
        assert_eq!(s, vec![(0.0, vec![1.0]), (1.0, vec![0.0])]);
    }
}
