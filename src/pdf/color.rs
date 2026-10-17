//! PDF colour spaces and their mapping to IR colours.

use super::function::Function;
use super::{deref, dict_of, name_str, obj_num, obj_nums};
use crate::ai8::rgb_to_cmyk;
use crate::ir::Color;
use lopdf::{Document as PdfDoc, Object};

const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone)]
pub enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Lab {
        white: [f64; 3],
        /// a*/b* ranges: amin amax bmin bmax.
        range: [f64; 4],
    },
    Indexed {
        base: Box<ColorSpace>,
        hival: usize,
        lookup: Vec<u8>,
    },
    Separation {
        name: String,
        /// CMYK approximation of 100% tint.
        full: [f64; 4],
    },
    DeviceN {
        names: Vec<String>,
        alt: Box<ColorSpace>,
        func: Option<Function>,
        /// CMYK at full tint for the single-colorant case.
        full: [f64; 4],
    },
    Pattern {
        /// Underlying space for uncoloured (PaintType 2) patterns.
        base: Option<Box<ColorSpace>>,
    },
}

/// Process colorant names that map directly onto CMYK channels.
fn process_index(name: &str) -> Option<usize> {
    match name {
        "Cyan" => Some(0),
        "Magenta" => Some(1),
        "Yellow" => Some(2),
        "Black" => Some(3),
        _ => None,
    }
}

/// Approximate CMYK of a process (or spot) colour.
pub fn color_to_cmyk(c: &Color) -> [f64; 4] {
    match c {
        Color::Gray(g) => [0.0, 0.0, 0.0, 1.0 - g.clamp(0.0, 1.0)],
        Color::Rgb(r, g, b) => rgb_to_cmyk(r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)),
        Color::Cmyk(c, m, y, k) => [*c, *m, *y, *k],
        Color::Spot { cmyk, tint, .. } => cmyk.map(|v| v * tint),
    }
}

/// CIE L*a*b* (relative to `white`) to sRGB, approximately.
pub fn lab_to_rgb(l: f64, a: f64, b: f64, white: [f64; 3]) -> (f64, f64, f64) {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let finv = |t: f64| {
        let d = 6.0 / 29.0;
        if t > d { t * t * t } else { 3.0 * d * d * (t - 4.0 / 29.0) }
    };
    let (x, y, z) = (white[0] * finv(fx), white[1] * finv(fy), white[2] * finv(fz));
    // XYZ (D50) -> linear sRGB (Bradford adapted).
    let r = 3.1338561 * x - 1.6168667 * y - 0.4906146 * z;
    let g = -0.9787684 * x + 1.9161415 * y + 0.0334540 * z;
    let bl = 0.0719453 * x - 0.2289914 * y + 1.4052427 * z;
    let gamma = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    };
    (gamma(r), gamma(g), gamma(bl))
}

impl ColorSpace {
    pub fn n_comps(&self) -> usize {
        match self {
            ColorSpace::Gray | ColorSpace::Indexed { .. } | ColorSpace::Separation { .. } => 1,
            ColorSpace::Rgb | ColorSpace::Lab { .. } => 3,
            ColorSpace::Cmyk => 4,
            ColorSpace::DeviceN { names, .. } => names.len().max(1),
            ColorSpace::Pattern { .. } => 0,
        }
    }

    /// Initial colour components after selecting this space with `cs`/`CS`.
    pub fn initial(&self) -> Vec<f64> {
        match self {
            ColorSpace::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Lab { range, .. } => vec![0.0, 0.0f64.clamp(range[0], range[1]), 0.0f64.clamp(range[2], range[3])],
            ColorSpace::Separation { .. } | ColorSpace::DeviceN { .. } => vec![1.0; self.n_comps()],
            _ => vec![0.0; self.n_comps()],
        }
    }

    /// Map components to an IR colour. `None` means "paint nothing"
    /// (Separation /None, or a pattern space without a pattern).
    pub fn to_color(&self, comps: &[f64]) -> Option<Color> {
        let c = |i: usize| comps.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        match self {
            ColorSpace::Gray => Some(Color::Gray(c(0))),
            ColorSpace::Rgb => Some(Color::Rgb(c(0), c(1), c(2))),
            ColorSpace::Cmyk => Some(Color::Cmyk(c(0), c(1), c(2), c(3))),
            ColorSpace::Lab { white, range } => {
                let l = comps.first().copied().unwrap_or(0.0).clamp(0.0, 100.0);
                let a = comps.get(1).copied().unwrap_or(0.0).clamp(range[0], range[1]);
                let b = comps.get(2).copied().unwrap_or(0.0).clamp(range[2], range[3]);
                let (r, g, b) = lab_to_rgb(l, a, b, *white);
                Some(Color::Rgb(r, g, b))
            }
            ColorSpace::Indexed { base, hival, lookup } => {
                let idx = comps.first().copied().unwrap_or(0.0).round().clamp(0.0, *hival as f64) as usize;
                let n = base.n_comps();
                let ranges = base.component_ranges();
                let vals: Vec<f64> = (0..n)
                    .map(|j| {
                        let byte = lookup.get(idx * n + j).copied().unwrap_or(0) as f64 / 255.0;
                        let (lo, hi) = ranges[j];
                        lo + byte * (hi - lo)
                    })
                    .collect();
                base.to_color(&vals)
            }
            ColorSpace::Separation { name, full, .. } => single_colorant(name, *full, c(0)),
            ColorSpace::DeviceN { names, alt, func, full } => {
                if names.iter().all(|n| n == "None") {
                    return None;
                }
                if names.len() == 1 {
                    return single_colorant(&names[0], *full, c(0));
                }
                // All process colorants: map straight to CMYK.
                if names.iter().all(|n| n == "None" || process_index(n).is_some()) {
                    let mut out = [0.0; 4];
                    for (i, n) in names.iter().enumerate() {
                        if let Some(p) = process_index(n) {
                            out[p] = c(i);
                        }
                    }
                    return Some(Color::Cmyk(out[0], out[1], out[2], out[3]));
                }
                let input: Vec<f64> = (0..names.len()).map(c).collect();
                match func.as_ref().and_then(|f| f.eval(&input)) {
                    Some(out) => alt.to_color(&out),
                    // Treat as ink coverage when the tint transform failed.
                    None => Some(Color::Gray(1.0 - input.iter().copied().fold(0.0, f64::max))),
                }
            }
            ColorSpace::Pattern { .. } => None,
        }
    }

    /// Value ranges of each component (used for Indexed lookups).
    fn component_ranges(&self) -> Vec<(f64, f64)> {
        match self {
            ColorSpace::Lab { range, .. } => vec![(0.0, 100.0), (range[0], range[1]), (range[2], range[3])],
            _ => vec![(0.0, 1.0); self.n_comps()],
        }
    }
}

fn single_colorant(name: &str, full: [f64; 4], t: f64) -> Option<Color> {
    match name {
        "None" => None,
        "All" => Some(Color::Cmyk(t, t, t, t)),
        n => {
            if let Some(p) = process_index(n) {
                let mut v = [0.0; 4];
                v[p] = t;
                return Some(Color::Cmyk(v[0], v[1], v[2], v[3]));
            }
            Some(Color::Spot { name: n.to_string(), cmyk: full, tint: t })
        }
    }
}

/// Evaluate the tint transform at full tint of the given colorant and
/// convert to CMYK. Returns `None` if evaluation is impossible.
fn full_tint_cmyk(alt: &ColorSpace, func: Option<&Function>, n_in: usize, which: usize) -> Option<[f64; 4]> {
    let mut input = vec![0.0; n_in];
    input[which] = 1.0;
    let out = func?.eval(&input)?;
    if out.len() < alt.n_comps() {
        return None;
    }
    alt.to_color(&out).map(|c| color_to_cmyk(&c))
}

/// Resolve a colour space object (name or array; may be a reference).
/// Approximations are reported through `warn`.
pub fn parse_colorspace(doc: &PdfDoc, obj: &Object, warn: &mut Vec<String>) -> Result<ColorSpace, String> {
    parse_depth(doc, obj, warn, 0)
}

fn by_name(name: &[u8]) -> Option<ColorSpace> {
    Some(match name {
        b"DeviceGray" | b"G" | b"CalGray" => ColorSpace::Gray,
        b"DeviceRGB" | b"RGB" | b"CalRGB" => ColorSpace::Rgb,
        b"DeviceCMYK" | b"CMYK" => ColorSpace::Cmyk,
        b"Pattern" => ColorSpace::Pattern { base: None },
        _ => return None,
    })
}

fn parse_depth(doc: &PdfDoc, obj: &Object, warn: &mut Vec<String>, depth: usize) -> Result<ColorSpace, String> {
    if depth > MAX_DEPTH {
        return Err("colour space nesting too deep".into());
    }
    let obj = deref(doc, obj);
    let arr = match obj {
        Object::Name(n) => {
            return by_name(n).ok_or_else(|| format!("unknown colour space /{}", String::from_utf8_lossy(n)));
        }
        Object::Array(a) => a,
        other => return Err(format!("invalid colour space object ({})", other.enum_variant())),
    };
    let family = arr.first().map(|o| deref(doc, o)).and_then(|o| o.as_name().ok()).ok_or("colour space array without family name")?;
    let arg = |i: usize| arr.get(i).map(|o| deref(doc, o));
    match family {
        b"DeviceGray" | b"G" | b"CalGray" => Ok(ColorSpace::Gray),
        b"DeviceRGB" | b"RGB" | b"CalRGB" => Ok(ColorSpace::Rgb),
        b"DeviceCMYK" | b"CMYK" => Ok(ColorSpace::Cmyk),
        b"Lab" => {
            let d = arg(1).and_then(|o| dict_of(doc, o));
            let white = d
                .and_then(|d| d.get(b"WhitePoint").ok())
                .and_then(|o| obj_nums(doc, o))
                .filter(|v| v.len() >= 3)
                .map(|v| [v[0], v[1], v[2]])
                .unwrap_or([0.9642, 1.0, 0.8249]);
            let range = d
                .and_then(|d| d.get(b"Range").ok())
                .and_then(|o| obj_nums(doc, o))
                .filter(|v| v.len() >= 4)
                .map(|v| [v[0], v[1], v[2], v[3]])
                .unwrap_or([-100.0, 100.0, -100.0, 100.0]);
            Ok(ColorSpace::Lab { white, range })
        }
        b"ICCBased" => {
            let d = arg(1).and_then(|o| dict_of(doc, o)).ok_or("ICCBased without stream")?;
            let n = d.get(b"N").ok().and_then(|o| obj_num(deref(doc, o))).map(|v| v as usize);
            if let Ok(alt) = d.get(b"Alternate")
                && let Ok(cs) = parse_depth(doc, alt, warn, depth + 1)
                && (n.is_none() || n == Some(cs.n_comps()))
            {
                return Ok(cs);
            }
            match n {
                Some(1) => Ok(ColorSpace::Gray),
                Some(3) => Ok(ColorSpace::Rgb),
                Some(4) => Ok(ColorSpace::Cmyk),
                other => Err(format!("ICCBased colour space with unsupported /N {other:?}")),
            }
        }
        b"Indexed" | b"I" => {
            let base = parse_depth(doc, arr.get(1).ok_or("Indexed without base")?, warn, depth + 1)?;
            if matches!(base, ColorSpace::Pattern { .. } | ColorSpace::Indexed { .. }) {
                return Err("invalid Indexed base colour space".into());
            }
            let hival = arg(2).and_then(obj_num).unwrap_or(0.0).clamp(0.0, 255.0) as usize;
            let lookup = match arg(3) {
                Some(Object::String(s, _)) => s.clone(),
                Some(Object::Stream(s)) => s.get_plain_content_with_limit(1 << 20).unwrap_or_default(),
                _ => Vec::new(),
            };
            Ok(ColorSpace::Indexed { base: Box::new(base), hival, lookup })
        }
        b"Separation" => {
            let name = arg(1).and_then(|o| o.as_name().ok()).map(name_str).ok_or("Separation without colorant name")?;
            let alt = parse_depth(doc, arr.get(2).ok_or("Separation without alternate")?, warn, depth + 1)
                .unwrap_or_else(|e| {
                    warn.push(format!("spot colour \"{name}\": {e}; using CMYK alternate"));
                    ColorSpace::Cmyk
                });
            let func = arr.get(3).and_then(|f| Function::parse(doc, f));
            let full = if name == "None" || name == "All" {
                [1.0; 4]
            } else {
                full_tint_cmyk(&alt, func.as_ref(), 1, 0).unwrap_or_else(|| {
                    warn.push(format!(
                        "could not evaluate the tint transform of spot colour \"{name}\"; its alternate was approximated as black"
                    ));
                    [0.0, 0.0, 0.0, 1.0]
                })
            };
            Ok(ColorSpace::Separation { name, full })
        }
        b"DeviceN" => {
            let names: Vec<String> = match arg(1) {
                Some(Object::Array(a)) => {
                    a.iter().filter_map(|o| deref(doc, o).as_name().ok()).map(name_str).collect()
                }
                _ => return Err("DeviceN without colorant names".into()),
            };
            if names.is_empty() {
                return Err("DeviceN without colorant names".into());
            }
            let alt = parse_depth(doc, arr.get(2).ok_or("DeviceN without alternate")?, warn, depth + 1)
                .unwrap_or(ColorSpace::Cmyk);
            let func = arr.get(3).and_then(|f| Function::parse(doc, f));
            let mut full = [0.0, 0.0, 0.0, 1.0];
            if names.len() == 1 && names[0] != "None" && names[0] != "All" {
                match full_tint_cmyk(&alt, func.as_ref(), 1, 0) {
                    Some(f) => full = f,
                    None => warn.push(format!(
                        "could not evaluate the tint transform of spot colour \"{}\"; its alternate was approximated as black",
                        names[0]
                    )),
                }
            }
            Ok(ColorSpace::DeviceN { names, alt: Box::new(alt), func, full })
        }
        b"Pattern" => {
            let base = match arr.get(1) {
                Some(b) => Some(Box::new(parse_depth(doc, b, warn, depth + 1)?)),
                None => None,
            };
            Ok(ColorSpace::Pattern { base })
        }
        other => Err(format!("unsupported colour space /{}", String::from_utf8_lossy(other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary, Stream};

    fn type2(c1: Vec<f64>) -> Object {
        let mut d = Dictionary::new();
        d.set("FunctionType", 2);
        d.set("Domain", vec![0.into(), 1.into()]);
        d.set("C0", c1.iter().map(|_| Object::Real(0.0)).collect::<Vec<_>>());
        d.set("C1", c1.iter().map(|v| Object::Real(*v as f32)).collect::<Vec<_>>());
        d.set("N", 1);
        Object::Dictionary(d)
    }

    fn sep(name: &str, alt: &str, c1: Vec<f64>) -> Object {
        Object::Array(vec![
            Object::Name(b"Separation".to_vec()),
            Object::Name(name.as_bytes().to_vec()),
            Object::Name(alt.as_bytes().to_vec()),
            type2(c1),
        ])
    }

    #[test]
    fn separation_spot() {
        let doc = PdfDoc::new();
        let mut w = Vec::new();
        let cs = parse_colorspace(&doc, &sep("CutContour", "DeviceCMYK", vec![0.0, 1.0, 0.0, 0.0]), &mut w).unwrap();
        assert!(w.is_empty());
        assert_eq!(
            cs.to_color(&[0.5]),
            Some(Color::Spot { name: "CutContour".into(), cmyk: [0.0, 1.0, 0.0, 0.0], tint: 0.5 })
        );
        assert_eq!(cs.initial(), vec![1.0]);
    }

    #[test]
    fn separation_rgb_and_gray_alternates() {
        let doc = PdfDoc::new();
        let mut w = Vec::new();
        let cs = parse_colorspace(&doc, &sep("Red", "DeviceRGB", vec![1.0, 0.0, 0.0]), &mut w).unwrap();
        let Some(Color::Spot { cmyk, .. }) = cs.to_color(&[1.0]) else { panic!() };
        assert_eq!(cmyk, [0.0, 1.0, 1.0, 0.0]);
        // Gray alternate: tint 1 -> gray 0 (black).
        let mut d = Dictionary::new();
        d.set("FunctionType", 2);
        d.set("Domain", vec![0.into(), 1.into()]);
        d.set("C0", vec![1.into()]);
        d.set("C1", vec![0.into()]);
        d.set("N", 1);
        let o = Object::Array(vec![
            Object::Name(b"Separation".to_vec()),
            Object::Name(b"Varnish".to_vec()),
            Object::Name(b"DeviceGray".to_vec()),
            Object::Dictionary(d),
        ]);
        let cs = parse_colorspace(&doc, &o, &mut w).unwrap();
        let Some(Color::Spot { cmyk, .. }) = cs.to_color(&[1.0]) else { panic!() };
        assert_eq!(cmyk, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn separation_special_names() {
        let doc = PdfDoc::new();
        let mut w = Vec::new();
        let all = parse_colorspace(&doc, &sep("All", "DeviceCMYK", vec![1.0, 1.0, 1.0, 1.0]), &mut w).unwrap();
        assert_eq!(all.to_color(&[1.0]), Some(Color::Cmyk(1.0, 1.0, 1.0, 1.0)));
        let none = parse_colorspace(&doc, &sep("None", "DeviceCMYK", vec![0.0, 0.0, 0.0, 0.0]), &mut w).unwrap();
        assert_eq!(none.to_color(&[1.0]), None);
        let black = parse_colorspace(&doc, &sep("Black", "DeviceCMYK", vec![0.0, 0.0, 0.0, 1.0]), &mut w).unwrap();
        assert_eq!(black.to_color(&[0.3]), Some(Color::Cmyk(0.0, 0.0, 0.0, 0.3)));
    }

    #[test]
    fn separation_bad_function_falls_back() {
        let doc = PdfDoc::new();
        let mut w = Vec::new();
        let o = Object::Array(vec![
            Object::Name(b"Separation".to_vec()),
            Object::Name(b"Gold".to_vec()),
            Object::Name(b"DeviceCMYK".to_vec()),
            Object::Integer(5),
        ]);
        let cs = parse_colorspace(&doc, &o, &mut w).unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(cs.to_color(&[1.0]), Some(Color::Spot { name: "Gold".into(), cmyk: [0.0, 0.0, 0.0, 1.0], tint: 1.0 }));
    }

    #[test]
    fn devicen() {
        let doc = PdfDoc::new();
        let mut w = Vec::new();
        let names = |v: &[&str]| Object::Array(v.iter().map(|n| Object::Name(n.as_bytes().to_vec())).collect());
        // Single colorant -> spot.
        let o = Object::Array(vec![
            Object::Name(b"DeviceN".to_vec()),
            names(&["Spot1"]),
            Object::Name(b"DeviceCMYK".to_vec()),
            type2(vec![0.2, 0.0, 0.8, 0.0]),
        ]);
        let cs = parse_colorspace(&doc, &o, &mut w).unwrap();
        assert_eq!(cs.to_color(&[0.4]), Some(Color::Spot { name: "Spot1".into(), cmyk: [0.2f32 as f64, 0.0, 0.8f32 as f64, 0.0], tint: 0.4 }));
        // Process colorants -> CMYK directly.
        let o = Object::Array(vec![
            Object::Name(b"DeviceN".to_vec()),
            names(&["Magenta", "Black"]),
            Object::Name(b"DeviceCMYK".to_vec()),
            Object::Null,
        ]);
        let cs = parse_colorspace(&doc, &o, &mut w).unwrap();
        assert_eq!(cs.to_color(&[0.5, 0.25]), Some(Color::Cmyk(0.0, 0.5, 0.0, 0.25)));
        // Mixed -> via PostScript tint transform.
        let mut d = Dictionary::new();
        d.set("FunctionType", 4);
        d.set("Domain", vec![0.into(), 1.into(), 0.into(), 1.into()]);
        d.set("Range", vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()]);
        let f = Stream::new(d, b"{ 0 0 4 2 roll }".to_vec());
        let mut doc = PdfDoc::new();
        let fid = doc.add_object(f);
        let o = Object::Array(vec![
            Object::Name(b"DeviceN".to_vec()),
            names(&["Cyan", "Gold"]),
            Object::Name(b"DeviceCMYK".to_vec()),
            Object::Reference(fid),
        ]);
        let cs = parse_colorspace(&doc, &o, &mut w).unwrap();
        assert_eq!(cs.to_color(&[0.5, 0.25]), Some(Color::Cmyk(0.0, 0.0, 0.5, 0.25)));
    }

    #[test]
    fn icc_lab_indexed() {
        let mut doc = PdfDoc::new();
        let mut w = Vec::new();
        let mut d = Dictionary::new();
        d.set("N", 4);
        let icc = doc.add_object(Stream::new(d, vec![]));
        let o = Object::Array(vec![Object::Name(b"ICCBased".to_vec()), Object::Reference(icc)]);
        assert!(matches!(parse_colorspace(&doc, &o, &mut w).unwrap(), ColorSpace::Cmyk));
        let mut d = Dictionary::new();
        d.set("N", 3);
        d.set("Alternate", Object::Name(b"DeviceRGB".to_vec()));
        let icc = doc.add_object(Stream::new(d, vec![]));
        let o = Object::Array(vec![Object::Name(b"ICCBased".to_vec()), Object::Reference(icc)]);
        assert!(matches!(parse_colorspace(&doc, &o, &mut w).unwrap(), ColorSpace::Rgb));

        let lab = Object::Array(vec![Object::Name(b"Lab".to_vec()), Object::Dictionary(Dictionary::new())]);
        let cs = parse_colorspace(&doc, &lab, &mut w).unwrap();
        let Some(Color::Rgb(r, g, b)) = cs.to_color(&[100.0, 0.0, 0.0]) else { panic!() };
        assert!(r > 0.97 && g > 0.97 && b > 0.97, "{r} {g} {b}");
        let Some(Color::Rgb(r, g, b)) = cs.to_color(&[50.0, 80.0, 60.0]) else { panic!() };
        assert!(r > g && r > b);

        let idx = Object::Array(vec![
            Object::Name(b"Indexed".to_vec()),
            Object::Name(b"DeviceRGB".to_vec()),
            Object::Integer(1),
            Object::string_literal(vec![255u8, 0, 0, 0, 0, 255]),
        ]);
        let cs = parse_colorspace(&doc, &idx, &mut w).unwrap();
        assert_eq!(cs.to_color(&[1.0]), Some(Color::Rgb(0.0, 0.0, 1.0)));
        assert_eq!(cs.to_color(&[9.0]), Some(Color::Rgb(0.0, 0.0, 1.0)));
        assert!(parse_colorspace(&doc, &Object::Name(b"Bogus".to_vec()), &mut w).is_err());
        assert!(parse_colorspace(&doc, &Object::Integer(3), &mut w).is_err());
    }
}
