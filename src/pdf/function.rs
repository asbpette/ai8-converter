//! PDF functions (ISO 32000 §7.10): sampled (type 0), exponential (type 2),
//! stitching (type 3) and PostScript calculator (type 4).

use super::{deref, dict_of, obj_num, obj_nums};
use lopdf::{Document as PdfDoc, Object};

/// Upper bound on the number of samples accepted for a type 0 function.
const MAX_SAMPLES: usize = 1 << 24;
/// Instruction budget for one PostScript calculator evaluation.
const MAX_PS_STEPS: usize = 100_000;
const MAX_PS_STACK: usize = 1000;
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone)]
pub enum Function {
    Sampled {
        domain: Vec<f64>,
        range: Vec<f64>,
        size: Vec<usize>,
        encode: Vec<f64>,
        decode: Vec<f64>,
        /// Samples normalised to 0..=1, `n_out` values per grid point, first
        /// input dimension varying fastest.
        samples: Vec<f64>,
    },
    Exponential {
        domain: [f64; 2],
        c0: Vec<f64>,
        c1: Vec<f64>,
        n: f64,
        range: Option<Vec<f64>>,
    },
    Stitching {
        domain: [f64; 2],
        functions: Vec<Function>,
        bounds: Vec<f64>,
        encode: Vec<f64>,
        range: Option<Vec<f64>>,
    },
    PostScript {
        domain: Vec<f64>,
        range: Vec<f64>,
        prog: Vec<PsOp>,
    },
    /// An array of single-output functions (allowed e.g. for shadings).
    Array(Vec<Function>),
}

fn clip(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        lo
    } else if lo <= hi {
        v.clamp(lo, hi)
    } else {
        v.clamp(hi, lo)
    }
}

fn interp(x: f64, xmin: f64, xmax: f64, ymin: f64, ymax: f64) -> f64 {
    if (xmax - xmin).abs() < 1e-12 {
        ymin
    } else {
        ymin + (x - xmin) * (ymax - ymin) / (xmax - xmin)
    }
}

fn clip_range(out: &mut [f64], range: &[f64]) {
    for (i, v) in out.iter_mut().enumerate() {
        if let (Some(lo), Some(hi)) = (range.get(2 * i), range.get(2 * i + 1)) {
            *v = clip(*v, *lo, *hi);
        }
    }
}

/// Read `count` big-endian unsigned values of `bps` bits each.
fn read_samples(data: &[u8], bps: usize, count: usize) -> Option<Vec<f64>> {
    if !matches!(bps, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32) {
        return None;
    }
    let total_bits = count.checked_mul(bps)?;
    if data.len() * 8 < total_bits {
        return None;
    }
    let max = ((1u64 << bps) - 1) as f64;
    let mut out = Vec::with_capacity(count);
    let mut bit = 0usize;
    for _ in 0..count {
        let mut v: u64 = 0;
        let mut remaining = bps;
        while remaining > 0 {
            let byte = data[bit / 8] as u64;
            let off = bit % 8;
            let take = remaining.min(8 - off);
            let bits = (byte >> (8 - off - take)) & ((1 << take) - 1);
            v = (v << take) | bits;
            bit += take;
            remaining -= take;
        }
        out.push(v as f64 / max);
    }
    Some(out)
}

impl Function {
    /// Parse a function object (dictionary, stream, or array of functions).
    pub fn parse(doc: &PdfDoc, obj: &Object) -> Option<Function> {
        Self::parse_depth(doc, obj, 0)
    }

    fn parse_depth(doc: &PdfDoc, obj: &Object, depth: usize) -> Option<Function> {
        if depth > MAX_DEPTH {
            return None;
        }
        let obj = deref(doc, obj);
        if let Object::Array(items) = obj {
            let fs: Option<Vec<Function>> = items.iter().map(|f| Self::parse_depth(doc, f, depth + 1)).collect();
            let fs = fs?;
            return if fs.is_empty() { None } else { Some(Function::Array(fs)) };
        }
        let dict = dict_of(doc, obj)?;
        let get_nums = |k: &[u8]| dict.get(k).ok().and_then(|o| obj_nums(doc, o));
        let get_num = |k: &[u8]| dict.get(k).ok().and_then(|o| obj_num(deref(doc, o)));
        let ftype = get_num(b"FunctionType")? as i64;
        let domain = get_nums(b"Domain").unwrap_or_else(|| vec![0.0, 1.0]);
        if domain.len() < 2 {
            return None;
        }
        let range = get_nums(b"Range");
        match ftype {
            0 => {
                let Object::Stream(stream) = obj else { return None };
                let range = range?;
                let m = domain.len() / 2;
                let n_out = range.len() / 2;
                if m == 0 || m > MAX_DEPTH || n_out == 0 {
                    return None;
                }
                let size: Vec<usize> = get_nums(b"Size")?.iter().map(|v| v.max(1.0) as usize).collect();
                if size.len() < m {
                    return None;
                }
                let size = size[..m].to_vec();
                let bps = get_num(b"BitsPerSample")? as usize;
                let mut points: usize = 1;
                for s in &size {
                    points = points.checked_mul(*s)?;
                }
                let count = points.checked_mul(n_out)?;
                if count > MAX_SAMPLES {
                    return None;
                }
                let encode = get_nums(b"Encode")
                    .filter(|e| e.len() >= 2 * m)
                    .unwrap_or_else(|| size.iter().flat_map(|s| [0.0, (*s as f64) - 1.0]).collect());
                let decode = get_nums(b"Decode").filter(|d| d.len() >= 2 * n_out).unwrap_or_else(|| range.clone());
                let data = stream.get_plain_content_with_limit(MAX_SAMPLES * 4).ok()?;
                let samples = read_samples(&data, bps, count)?;
                Some(Function::Sampled { domain, range, size, encode, decode, samples })
            }
            2 => {
                let c0 = get_nums(b"C0").unwrap_or_else(|| vec![0.0]);
                let c1 = get_nums(b"C1").unwrap_or_else(|| vec![1.0]);
                if c0.len() != c1.len() || c0.is_empty() {
                    return None;
                }
                let n = get_num(b"N").unwrap_or(1.0);
                Some(Function::Exponential { domain: [domain[0], domain[1]], c0, c1, n, range })
            }
            3 => {
                let fobj = dict.get(b"Functions").ok()?;
                let Object::Array(items) = deref(doc, fobj) else { return None };
                let functions: Option<Vec<Function>> =
                    items.iter().map(|f| Self::parse_depth(doc, f, depth + 1)).collect();
                let functions = functions?;
                let k = functions.len();
                if k == 0 {
                    return None;
                }
                let bounds = get_nums(b"Bounds").unwrap_or_default();
                let encode = get_nums(b"Encode").unwrap_or_default();
                if bounds.len() < k - 1 || encode.len() < 2 * k {
                    return None;
                }
                Some(Function::Stitching { domain: [domain[0], domain[1]], functions, bounds, encode, range })
            }
            4 => {
                let Object::Stream(stream) = obj else { return None };
                let range = range?;
                let code = stream.get_plain_content_with_limit(1 << 20).ok()?;
                let prog = parse_ps(&code)?;
                Some(Function::PostScript { domain, range, prog })
            }
            _ => None,
        }
    }

    /// Evaluate the function. `None` if evaluation is impossible.
    pub fn eval(&self, input: &[f64]) -> Option<Vec<f64>> {
        match self {
            Function::Array(fs) => {
                let mut out = Vec::new();
                for f in fs {
                    out.extend(f.eval(input)?);
                }
                Some(out)
            }
            Function::Exponential { domain, c0, c1, n, range } => {
                let x = clip(*input.first()?, domain[0], domain[1]);
                let xn = x.powf(*n);
                let xn = if xn.is_finite() { xn } else { 0.0 };
                let mut out: Vec<f64> = c0.iter().zip(c1).map(|(a, b)| a + xn * (b - a)).collect();
                if let Some(r) = range {
                    clip_range(&mut out, r);
                }
                Some(out)
            }
            Function::Stitching { domain, functions, bounds, encode, range } => {
                let x = clip(*input.first()?, domain[0], domain[1]);
                let k = functions.len();
                let mut i = 0;
                while i < k - 1 && x >= bounds[i] {
                    i += 1;
                }
                let lo = if i == 0 { domain[0] } else { bounds[i - 1] };
                let hi = if i == k - 1 { domain[1] } else { bounds[i] };
                let e = interp(x, lo, hi, encode[2 * i], encode[2 * i + 1]);
                let mut out = functions[i].eval(&[e])?;
                if let Some(r) = range {
                    clip_range(&mut out, r);
                }
                Some(out)
            }
            Function::Sampled { domain, range, size, encode, decode, samples } => {
                let m = size.len();
                let n_out = range.len() / 2;
                if input.len() < m {
                    return None;
                }
                // Per-dimension base index and fraction.
                let mut base = vec![0usize; m];
                let mut frac = vec![0.0f64; m];
                for i in 0..m {
                    let x = clip(input[i], domain[2 * i], domain[2 * i + 1]);
                    let e = interp(x, domain[2 * i], domain[2 * i + 1], encode[2 * i], encode[2 * i + 1]);
                    let e = clip(e, 0.0, (size[i] - 1) as f64);
                    let mut b = e.floor() as usize;
                    if b >= size[i] - 1 {
                        b = size[i].saturating_sub(2);
                    }
                    base[i] = b;
                    frac[i] = if size[i] == 1 { 0.0 } else { (e - b as f64).clamp(0.0, 1.0) };
                }
                let mut out = vec![0.0; n_out];
                for corner in 0..(1usize << m) {
                    let mut w = 1.0;
                    let mut idx = 0usize;
                    let mut stride = 1usize;
                    for i in 0..m {
                        let hi = corner >> i & 1 == 1;
                        let pos = if hi { (base[i] + 1).min(size[i] - 1) } else { base[i] };
                        w *= if hi { frac[i] } else { 1.0 - frac[i] };
                        idx += pos * stride;
                        stride *= size[i];
                    }
                    if w == 0.0 {
                        continue;
                    }
                    for (j, o) in out.iter_mut().enumerate() {
                        *o += w * samples.get(idx * n_out + j).copied().unwrap_or(0.0);
                    }
                }
                for (j, o) in out.iter_mut().enumerate() {
                    *o = interp(*o, 0.0, 1.0, decode[2 * j], decode[2 * j + 1]);
                }
                clip_range(&mut out, range);
                Some(out)
            }
            Function::PostScript { domain, range, prog } => {
                let m = domain.len() / 2;
                if input.len() < m {
                    return None;
                }
                let mut stack: Vec<PsVal> =
                    (0..m).map(|i| PsVal::Num(clip(input[i], domain[2 * i], domain[2 * i + 1]))).collect();
                let mut steps = 0usize;
                run_ps(prog, &mut stack, &mut steps)?;
                let n_out = range.len() / 2;
                if stack.len() < n_out {
                    return None;
                }
                let mut out = Vec::with_capacity(n_out);
                for v in &stack[stack.len() - n_out..] {
                    out.push(v.num()?);
                }
                clip_range(&mut out, range);
                Some(out)
            }
        }
    }
}

// --- PostScript calculator -------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum PsOp {
    Num(f64),
    Bool(bool),
    Op(Box<str>),
    Block(Vec<PsOp>),
    If(Vec<PsOp>),
    IfElse(Vec<PsOp>, Vec<PsOp>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PsVal {
    Num(f64),
    Bool(bool),
}

impl PsVal {
    fn num(self) -> Option<f64> {
        match self {
            PsVal::Num(n) => Some(n),
            PsVal::Bool(_) => None,
        }
    }
}

/// Parse a calculator program `{ ... }` into ops.
pub fn parse_ps(code: &[u8]) -> Option<Vec<PsOp>> {
    let mut pos = 0;
    // Skip to the opening brace.
    while pos < code.len() && code[pos] != b'{' {
        pos += 1;
    }
    if pos >= code.len() {
        return None;
    }
    pos += 1;
    parse_block(code, &mut pos, 0)
}

fn parse_block(code: &[u8], pos: &mut usize, depth: usize) -> Option<Vec<PsOp>> {
    if depth > 32 {
        return None;
    }
    let mut ops: Vec<PsOp> = Vec::new();
    loop {
        while *pos < code.len() && (code[*pos].is_ascii_whitespace() || code[*pos] == 0) {
            *pos += 1;
        }
        if *pos >= code.len() {
            // Unterminated: accept what we have.
            return Some(ops);
        }
        match code[*pos] {
            b'{' => {
                *pos += 1;
                ops.push(PsOp::Block(parse_block(code, pos, depth + 1)?));
            }
            b'}' => {
                *pos += 1;
                return Some(ops);
            }
            b'%' => {
                while *pos < code.len() && code[*pos] != b'\n' && code[*pos] != b'\r' {
                    *pos += 1;
                }
            }
            _ => {
                let start = *pos;
                while *pos < code.len()
                    && !code[*pos].is_ascii_whitespace()
                    && !matches!(code[*pos], b'{' | b'}' | b'%')
                {
                    *pos += 1;
                }
                let tok = &code[start..*pos];
                if let Some(n) = super::lexer::parse_number(tok) {
                    ops.push(PsOp::Num(n));
                    continue;
                }
                let word = std::str::from_utf8(tok).ok()?;
                match word {
                    "true" => ops.push(PsOp::Bool(true)),
                    "false" => ops.push(PsOp::Bool(false)),
                    "if" => {
                        let Some(PsOp::Block(b)) = ops.pop() else { return None };
                        ops.push(PsOp::If(b));
                    }
                    "ifelse" => {
                        let Some(PsOp::Block(b2)) = ops.pop() else { return None };
                        let Some(PsOp::Block(b1)) = ops.pop() else { return None };
                        ops.push(PsOp::IfElse(b1, b2));
                    }
                    w => ops.push(PsOp::Op(w.into())),
                }
            }
        }
    }
}

fn run_ps(prog: &[PsOp], st: &mut Vec<PsVal>, steps: &mut usize) -> Option<()> {
    use PsVal::{Bool as B, Num as N};
    for op in prog {
        *steps += 1;
        if *steps > MAX_PS_STEPS || st.len() > MAX_PS_STACK {
            return None;
        }
        match op {
            PsOp::Num(n) => st.push(N(*n)),
            PsOp::Bool(b) => st.push(B(*b)),
            PsOp::Block(_) => return None,
            PsOp::If(b) => {
                if let B(c) = st.pop()? {
                    if c {
                        run_ps(b, st, steps)?;
                    }
                } else {
                    return None;
                }
            }
            PsOp::IfElse(b1, b2) => {
                if let B(c) = st.pop()? {
                    run_ps(if c { b1 } else { b2 }, st, steps)?;
                } else {
                    return None;
                }
            }
            PsOp::Op(name) => {
                let name: &str = name;
                macro_rules! num1 {
                    ($f:expr) => {{
                        let a = st.pop()?.num()?;
                        let f: fn(f64) -> f64 = $f;
                        st.push(N(f(a)));
                    }};
                }
                macro_rules! num2 {
                    ($f:expr) => {{
                        let b = st.pop()?.num()?;
                        let a = st.pop()?.num()?;
                        let f: fn(f64, f64) -> f64 = $f;
                        st.push(N(f(a, b)));
                    }};
                }
                macro_rules! cmp {
                    ($f:expr) => {{
                        let b = st.pop()?.num()?;
                        let a = st.pop()?.num()?;
                        let f: fn(f64, f64) -> bool = $f;
                        st.push(B(f(a, b)));
                    }};
                }
                match name {
                    "add" => num2!(|a, b| a + b),
                    "sub" => num2!(|a, b| a - b),
                    "mul" => num2!(|a, b| a * b),
                    "div" => num2!(|a, b| if b == 0.0 { 0.0 } else { a / b }),
                    "idiv" => num2!(|a, b| {
                        let (a, b) = (a.trunc() as i64, b.trunc() as i64);
                        if b == 0 { 0.0 } else { a.wrapping_div(b) as f64 }
                    }),
                    "mod" => num2!(|a, b| {
                        let (a, b) = (a.trunc() as i64, b.trunc() as i64);
                        if b == 0 { 0.0 } else { a.wrapping_rem(b) as f64 }
                    }),
                    "exp" => num2!(|a, b| {
                        let r = a.powf(b);
                        if r.is_finite() { r } else { 0.0 }
                    }),
                    "atan" => num2!(|a, b| {
                        let d = a.atan2(b).to_degrees();
                        if d < 0.0 { d + 360.0 } else { d }
                    }),
                    "bitshift" => num2!(|a, b| {
                        let (a, s) = (a.trunc() as i64, b.trunc() as i64);
                        if s >= 0 { a.checked_shl(s as u32).unwrap_or(0) as f64 } else { a.checked_shr((-s) as u32).unwrap_or(0) as f64 }
                    }),
                    "abs" => num1!(f64::abs),
                    "neg" => num1!(|a| -a),
                    "ceiling" => num1!(f64::ceil),
                    "floor" => num1!(f64::floor),
                    "round" => num1!(|a| (a + 0.5).floor()),
                    "truncate" | "cvi" => num1!(f64::trunc),
                    "cvr" => num1!(|a| a),
                    "sqrt" => num1!(|a| a.max(0.0).sqrt()),
                    "sin" => num1!(|a| a.to_radians().sin()),
                    "cos" => num1!(|a| a.to_radians().cos()),
                    "ln" => num1!(|a| if a > 0.0 { a.ln() } else { 0.0 }),
                    "log" => num1!(|a| if a > 0.0 { a.log10() } else { 0.0 }),
                    "eq" | "ne" => {
                        let b = st.pop()?;
                        let a = st.pop()?;
                        let eq = match (a, b) {
                            (N(x), N(y)) => (x - y).abs() < 1e-12,
                            (B(x), B(y)) => x == y,
                            _ => false,
                        };
                        st.push(B(if name == "eq" { eq } else { !eq }));
                    }
                    "gt" => cmp!(|a, b| a > b),
                    "ge" => cmp!(|a, b| a >= b),
                    "lt" => cmp!(|a, b| a < b),
                    "le" => cmp!(|a, b| a <= b),
                    "and" | "or" | "xor" => {
                        let b = st.pop()?;
                        let a = st.pop()?;
                        let r = match (a, b) {
                            (B(x), B(y)) => B(match name {
                                "and" => x && y,
                                "or" => x || y,
                                _ => x ^ y,
                            }),
                            (N(x), N(y)) => {
                                let (x, y) = (x as i64, y as i64);
                                N(match name {
                                    "and" => x & y,
                                    "or" => x | y,
                                    _ => x ^ y,
                                } as f64)
                            }
                            _ => return None,
                        };
                        st.push(r);
                    }
                    "not" => {
                        let a = st.pop()?;
                        st.push(match a {
                            B(x) => B(!x),
                            N(x) => N(!(x as i64) as f64),
                        });
                    }
                    "pop" => {
                        st.pop()?;
                    }
                    "dup" => {
                        let a = *st.last()?;
                        st.push(a);
                    }
                    "exch" => {
                        let n = st.len();
                        if n < 2 {
                            return None;
                        }
                        st.swap(n - 1, n - 2);
                    }
                    "copy" => {
                        let n = st.pop()?.num()? as usize;
                        if n > st.len() || st.len() + n > MAX_PS_STACK {
                            return None;
                        }
                        let start = st.len() - n;
                        for i in start..start + n {
                            st.push(st[i]);
                        }
                    }
                    "index" => {
                        let n = st.pop()?.num()?;
                        if n < 0.0 || n as usize >= st.len() {
                            return None;
                        }
                        let v = st[st.len() - 1 - n as usize];
                        st.push(v);
                    }
                    "roll" => {
                        let j = st.pop()?.num()? as i64;
                        let n = st.pop()?.num()?;
                        if n < 0.0 || n as usize > st.len() {
                            return None;
                        }
                        let n = n as usize;
                        if n > 0 {
                            let start = st.len() - n;
                            let s = &mut st[start..];
                            let j = j.rem_euclid(n as i64) as usize;
                            s.rotate_right(j);
                        }
                    }
                    _ => return None,
                }
            }
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary, Stream};

    fn eval_ps(code: &str, input: &[f64], n_out: usize) -> Option<Vec<f64>> {
        let prog = parse_ps(code.as_bytes())?;
        let f = Function::PostScript {
            domain: input.iter().flat_map(|_| [-1000.0, 1000.0]).collect(),
            range: (0..n_out).flat_map(|_| [-1000.0, 1000.0]).collect(),
            prog,
        };
        f.eval(input)
    }

    #[test]
    fn exponential() {
        let doc = PdfDoc::new();
        let mut d = Dictionary::new();
        d.set("FunctionType", 2);
        d.set("Domain", vec![0.into(), 1.into()]);
        d.set("C0", vec![0.into(), 0.into(), 0.into(), 0.into()]);
        d.set("C1", vec![0.into(), 1.into(), 0.5.into(), 0.into()]);
        d.set("N", 1);
        let f = Function::parse(&doc, &Object::Dictionary(d)).unwrap();
        assert_eq!(f.eval(&[1.0]).unwrap(), vec![0.0, 1.0, 0.5, 0.0]);
        assert_eq!(f.eval(&[0.5]).unwrap(), vec![0.0, 0.5, 0.25, 0.0]);
        // Input is clipped to the domain.
        assert_eq!(f.eval(&[7.0]).unwrap(), vec![0.0, 1.0, 0.5, 0.0]);
    }

    #[test]
    fn exponential_n2_defaults() {
        let doc = PdfDoc::new();
        let mut d = Dictionary::new();
        d.set("FunctionType", 2);
        d.set("Domain", vec![0.into(), 1.into()]);
        d.set("N", 2);
        let f = Function::parse(&doc, &Object::Dictionary(d)).unwrap();
        assert!((f.eval(&[0.5]).unwrap()[0] - 0.25).abs() < 1e-9);
    }

    #[test]
    fn sampled_1in_2out() {
        let doc = PdfDoc::new();
        let mut d = Dictionary::new();
        d.set("FunctionType", 0);
        d.set("Domain", vec![0.into(), 1.into()]);
        d.set("Range", vec![0.into(), 1.into(), 0.into(), 1.into()]);
        d.set("Size", vec![2.into()]);
        d.set("BitsPerSample", 8);
        let s = Stream::new(d, vec![0, 255, 255, 0]);
        let f = Function::parse(&doc, &Object::Stream(s)).unwrap();
        let v = f.eval(&[0.25]).unwrap();
        assert!((v[0] - 0.25).abs() < 1e-9 && (v[1] - 0.75).abs() < 1e-9, "{v:?}");
        assert_eq!(f.eval(&[1.0]).unwrap(), vec![1.0, 0.0]);
    }

    #[test]
    fn sampled_2in_bilinear_16bit() {
        let doc = PdfDoc::new();
        let mut d = Dictionary::new();
        d.set("FunctionType", 0);
        d.set("Domain", vec![0.into(), 1.into(), 0.into(), 1.into()]);
        d.set("Range", vec![0.into(), 1.into()]);
        d.set("Size", vec![2.into(), 2.into()]);
        d.set("BitsPerSample", 16);
        // (0,0)=0 (1,0)=1 (0,1)=1 (1,1)=1
        let s = Stream::new(d, vec![0, 0, 255, 255, 255, 255, 255, 255]);
        let f = Function::parse(&doc, &Object::Stream(s)).unwrap();
        assert!((f.eval(&[0.5, 0.5]).unwrap()[0] - 0.75).abs() < 1e-9);
        assert!((f.eval(&[0.5, 0.0]).unwrap()[0] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn sampled_rejects_short_data() {
        let doc = PdfDoc::new();
        let mut d = Dictionary::new();
        d.set("FunctionType", 0);
        d.set("Domain", vec![0.into(), 1.into()]);
        d.set("Range", vec![0.into(), 1.into()]);
        d.set("Size", vec![100.into()]);
        d.set("BitsPerSample", 8);
        let s = Stream::new(d, vec![1, 2]);
        assert!(Function::parse(&doc, &Object::Stream(s)).is_none());
    }

    #[test]
    fn bit_reader() {
        assert_eq!(read_samples(&[0b1011_0000], 1, 4).unwrap(), vec![1.0, 0.0, 1.0, 1.0]);
        let v = read_samples(&[0xFF, 0xF0, 0x00], 12, 2).unwrap();
        assert_eq!(v, vec![1.0, 0.0]);
        assert!(read_samples(&[0], 3, 1).is_none());
    }

    #[test]
    fn stitching() {
        let doc = PdfDoc::new();
        let mk = |c0: f64, c1: f64| {
            let mut d = Dictionary::new();
            d.set("FunctionType", 2);
            d.set("Domain", vec![0.into(), 1.into()]);
            d.set("C0", vec![c0.into()]);
            d.set("C1", vec![c1.into()]);
            d.set("N", 1);
            Object::Dictionary(d)
        };
        let mut d = Dictionary::new();
        d.set("FunctionType", 3);
        d.set("Domain", vec![0.into(), 1.into()]);
        d.set("Functions", vec![mk(0.0, 1.0), mk(1.0, 0.0)]);
        d.set("Bounds", vec![0.5.into()]);
        d.set("Encode", vec![0.into(), 1.into(), 0.into(), 1.into()]);
        let f = Function::parse(&doc, &Object::Dictionary(d)).unwrap();
        assert!((f.eval(&[0.25]).unwrap()[0] - 0.5).abs() < 1e-9);
        assert!((f.eval(&[0.5]).unwrap()[0] - 1.0).abs() < 1e-9);
        assert!((f.eval(&[0.75]).unwrap()[0] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn postscript_arith_and_stack() {
        assert_eq!(eval_ps("{ 2 mul 1 add }", &[3.0], 1).unwrap(), vec![7.0]);
        assert_eq!(eval_ps("{ dup 0 0 }", &[0.4], 4).unwrap(), vec![0.4, 0.4, 0.0, 0.0]);
        assert_eq!(eval_ps("{ exch sub }", &[1.0, 3.0], 1).unwrap(), vec![2.0]);
        assert_eq!(eval_ps("{ 3 1 roll }", &[1.0, 2.0, 3.0], 3).unwrap(), vec![3.0, 1.0, 2.0]);
        assert_eq!(eval_ps("{ 1 index }", &[5.0, 6.0], 3).unwrap(), vec![5.0, 6.0, 5.0]);
        assert_eq!(eval_ps("{ 2 copy }", &[5.0, 6.0], 4).unwrap(), vec![5.0, 6.0, 5.0, 6.0]);
        assert_eq!(eval_ps("{ pop 15 7 idiv }", &[0.0], 1).unwrap(), vec![2.0]);
        assert_eq!(eval_ps("{ 7 3 mod }", &[], 1).unwrap(), vec![1.0]);
        assert_eq!(eval_ps("{ 9 sqrt neg abs }", &[], 1).unwrap(), vec![3.0]);
        assert_eq!(eval_ps("{ 2.6 cvi 1 2 bitshift }", &[], 2).unwrap(), vec![2.0, 4.0]);
        assert_eq!(eval_ps("{ 1 0 div }", &[], 1).unwrap(), vec![0.0]);
    }

    #[test]
    fn postscript_conditionals() {
        let prog = "{ dup 0.5 gt { 1 } { 0 } ifelse exch pop }";
        assert_eq!(eval_ps(prog, &[0.7], 1).unwrap(), vec![1.0]);
        assert_eq!(eval_ps(prog, &[0.2], 1).unwrap(), vec![0.0]);
        assert_eq!(eval_ps("{ dup 0 eq { pop 42 } if }", &[0.0], 1).unwrap(), vec![42.0]);
        assert_eq!(eval_ps("{ true false or not { 1 } { 2 } ifelse }", &[], 1).unwrap(), vec![2.0]);
        assert_eq!(eval_ps("{ 1 2 lt 3 3 ge and 2 2 ne xor { 5 } if }", &[], 1).unwrap(), vec![5.0]);
    }

    #[test]
    fn postscript_errors() {
        assert!(eval_ps("{ pop pop }", &[1.0], 1).is_none());
        assert!(eval_ps("{ frobnicate }", &[1.0], 1).is_none());
        assert!(parse_ps(b"no brace").is_none());
        assert!(parse_ps(b"{ if }").is_none());
        // Stack bomb is bounded.
        assert!(eval_ps("{ 1 dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup dup 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy 50 copy }", &[], 1).is_none());
    }
}
