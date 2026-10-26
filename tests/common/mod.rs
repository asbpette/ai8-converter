//! Shared test helper: a strict structural validator for generated AI8 files.
#![allow(dead_code)]

/// Summary of a validated AI8 file.
#[derive(Debug, Default)]
pub struct Ai8Stats {
    /// Painted paths in the art section.
    pub paths: usize,
    pub groups: usize,
    pub clip_groups: usize,
    pub compound_paths: usize,
    /// Names of the pattern definitions in the setup section, in order.
    pub pattern_defs: Vec<String>,
    /// Names of the gradient definitions in the setup section, in order.
    pub gradient_defs: Vec<String>,
    /// Painted paths inside pattern tiles.
    pub tile_paths: usize,
    /// `p` / `P` operators in the art.
    pub pattern_uses: usize,
    /// `Bb` … `BB` gradient instances in the art.
    pub gradient_instances: usize,
}

const PAINT_OPS: &[&str] = &["N", "n", "F", "f", "S", "s", "B", "b"];

/// Number of colorSpec operands for each gradient stop colorStyle (AI7 spec table 8).
const STOP_SPEC_LEN: [usize; 5] = [1, 4, 7, 6, 10];

/// Extract the first PostScript string literal of `line` (decoded loosely:
/// escapes kept) and the rest of the line with it replaced by `STR`.
fn split_string(line: &str) -> Option<(String, String)> {
    let a = line.find('(')?;
    let mut depth = 0usize;
    let mut esc = false;
    for (i, ch) in line[a..].char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match ch {
            '\\' => esc = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let end = a + i;
                    return Some((line[a + 1..end].to_string(), format!("{}STR{}", &line[..a], &line[end + 1..])));
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_nums(toks: &[&str], ctx: &str) -> Vec<f64> {
    toks.iter()
        .map(|t| {
            let v: f64 = t.parse().unwrap_or_else(|_| panic!("{ctx}: bad number {t:?}"));
            assert!(v.is_finite(), "{ctx}: non-finite number");
            v
        })
        .collect()
}

/// Are the unescaped parentheses of `s` balanced (a valid PostScript string body)?
fn ps_parens_balanced(s: &str) -> bool {
    let mut depth = 0i64;
    let mut esc = false;
    for ch in s.chars() {
        if esc {
            esc = false;
            continue;
        }
        match ch {
            '\\' => esc = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0 && !esc
}

struct ArtCtx<'a> {
    patterns: &'a [String],
    gradients: &'a [String],
    in_tile: bool,
    /// Only colour / overprint operators allowed (pattern `@` strings).
    color_only: bool,
}

/// Validate a sequence of art operators; panics on error.
fn validate_art(art: &[&str], cx: &ArtCtx, stats: &mut Ai8Stats, label: &str) {
    let mut stack: Vec<&'static str> = Vec::new(); // "u", "q", "*u"
    let mut in_path = false;
    let mut clip_pending = false;
    // Gradient instance: Some(painted?)
    let mut gradient: Option<bool> = None;
    for (i, raw) in art.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        let ctx = || format!("{label} line {}: {line:?}", i + 1);
        let (string, line_for_toks) = match split_string(line) {
            Some((s, rest)) => (Some(s), rest),
            None => (None, line.to_string()),
        };
        let toks: Vec<&str> = line_for_toks.split_whitespace().collect();
        let op = *toks.last().unwrap();
        let operands = &toks[..toks.len() - 1];
        let nums = |n: usize| {
            assert_eq!(operands.len(), n, "{}: expected {n} operands", ctx());
            parse_nums(operands, &ctx())
        };
        if cx.color_only {
            assert!(
                ["O", "R", "g", "G", "k", "K", "x", "X", "Xa", "XA"].contains(&op),
                "{}: not a colour operator",
                ctx()
            );
        }
        if gradient.is_some() {
            assert!(
                ["Bh", "Bg", "Bm", "f", "F", "BB"].contains(&op),
                "{}: unexpected operator inside gradient instance",
                ctx()
            );
        }
        match op {
            "Lb" => {
                nums(13);
            }
            "Ln" | "LB" | "A" => {}
            "Ar" => {
                nums(1);
            }
            "u" | "q" | "*u" => {
                assert!(!in_path, "{}: group opened inside path", ctx());
                if op == "*u" {
                    assert!(!stack.contains(&"*u"), "{}: nested compound path", ctx());
                    stats.compound_paths += 1;
                } else if op == "u" {
                    stats.groups += 1;
                } else {
                    stats.clip_groups += 1;
                    clip_pending = true;
                }
                stack.push(match op {
                    "u" => "u",
                    "q" => "q",
                    _ => "*u",
                });
            }
            "U" | "Q" | "*U" => {
                assert!(!in_path, "{}: group closed inside path", ctx());
                let open = match op {
                    "U" => "u",
                    "Q" => "q",
                    _ => "*u",
                };
                assert_eq!(stack.pop(), Some(open), "{}: unbalanced", ctx());
            }
            "m" => {
                nums(2);
                assert!(!in_path, "{}: m inside unterminated path", ctx());
                in_path = true;
            }
            "L" | "l" => {
                nums(2);
                assert!(in_path, "{}: lineto without m", ctx());
            }
            "C" | "c" => {
                nums(6);
                assert!(in_path, "{}: curveto without m", ctx());
            }
            "W" => {
                assert!(in_path, "{}: W outside path", ctx());
                assert!(clip_pending, "{}: W outside a fresh clip group", ctx());
            }
            "h" => assert!(in_path, "{}", ctx()),
            "Bb" => {
                assert!(!cx.in_tile, "{}: gradient instance inside a pattern tile", ctx());
                assert!(operands.is_empty(), "{}", ctx());
                assert!(in_path, "{}: Bb without a constructed path", ctx());
                gradient = Some(false);
            }
            "Bh" => {
                let n = nums(4);
                assert!((0.0..=1.0 + 1e-9).contains(&n[3]), "{}: hilight length must be a fraction", ctx());
            }
            "Bg" => {
                let name = string.clone().unwrap_or_else(|| panic!("{}: Bg without name", ctx()));
                assert!(cx.gradients.contains(&name), "{}: gradient {name:?} not defined", ctx());
                assert_eq!(operands.len(), 12, "{}: flag (name) x y angle length a b c d tx ty", ctx());
                assert_eq!(operands[1], "STR", "{}", ctx());
                let flag = parse_nums(&operands[..1], &ctx())[0];
                assert!([0.0, 1.0, 2.0].contains(&flag), "{}", ctx());
                let n = parse_nums(&operands[2..], &ctx());
                assert!(n[3] > 0.0, "{}: gradient length must be positive", ctx());
                assert!((n[4] * n[7] - n[5] * n[6]).abs() > 1e-9, "{}: singular Bg matrix", ctx());
                stats.gradient_instances += 1;
            }
            "Bm" => {
                nums(6);
            }
            "BB" => {
                let n = nums(1);
                assert!([0.0, 1.0, 2.0].contains(&n[0]), "{}", ctx());
                assert_eq!(gradient, Some(true), "{}: BB without a painted gradient instance", ctx());
                assert!(!in_path, "{}", ctx());
                gradient = None;
            }
            op if PAINT_OPS.contains(&op) => {
                assert!(operands.is_empty(), "{}", ctx());
                assert!(in_path, "{}: paint without path", ctx());
                if gradient.is_some() {
                    assert!(op == "f" || op == "F", "{}: gradient instance must fill", ctx());
                    gradient = Some(true);
                }
                in_path = false;
                if op == "n" && line == "n" && clip_pending {
                    // the clip path(s) of a clip group
                } else {
                    clip_pending = false;
                    stats.paths += 1;
                }
            }
            "g" | "G" => {
                let n = nums(1);
                assert!((0.0..=1.0).contains(&n[0]), "{}", ctx());
            }
            "k" | "K" => {
                let n = nums(4);
                assert!(n.iter().all(|v| (0.0..=1.0).contains(v)), "{}", ctx());
            }
            "Xa" | "XA" => {
                nums(3);
            }
            "x" | "X" => {
                assert_eq!(operands.len(), 6, "{}", ctx());
                assert!(string.is_some(), "{}: spot name", ctx());
            }
            "p" | "P" => {
                assert!(!cx.in_tile, "{}: pattern paint inside a pattern tile", ctx());
                let name = string.clone().unwrap_or_else(|| panic!("{}: pattern without name", ctx()));
                assert!(cx.patterns.contains(&name), "{}: pattern {name:?} not defined", ctx());
                // (name) px py sx sy angle rf r k ka [a b c d tx ty]
                let flat = line_for_toks.replace('[', " [ ").replace(']', " ] ");
                let t: Vec<&str> = flat.split_whitespace().collect();
                assert_eq!(t.len(), 1 + 9 + 1 + 6 + 1 + 1, "{}: pattern operands", ctx());
                assert_eq!((t[0], t[10], t[17]), ("STR", "[", "]"), "{}", ctx());
                parse_nums(&t[1..10], &ctx());
                let m = parse_nums(&t[11..17], &ctx());
                assert!((m[0] * m[3] - m[1] * m[2]).abs() > 1e-12, "{}: singular pattern matrix", ctx());
                stats.pattern_uses += 1;
            }
            "O" | "R" | "XR" | "D" => {
                nums(1);
            }
            "d" => assert!(line.contains('[') && line.contains(']'), "{}", ctx()),
            "J" | "j" | "w" | "M" => {
                // stroke style line: "c J j j w w M M [..]p d"
            }
            other => panic!("{}: unknown operator {other:?}", ctx()),
        }
        // A combined style line ends in "d"; validate it separately.
        if op == "d" {
            assert!(line.contains(" J ") && line.contains(" j ") && line.contains(" w ") && line.contains(" M "));
        }
    }
    assert!(!in_path, "{label}: unterminated path at end");
    assert!(gradient.is_none(), "{label}: unterminated gradient instance");
    assert!(stack.is_empty(), "{label}: unclosed groups at end: {stack:?}");
}

/// Split a run of operators written on shared lines ("0 O 0 R 0 g") into one
/// line per operator (strings kept intact).
fn split_operators(text: &str) -> Vec<String> {
    let mut toks: Vec<String> = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '(' {
            let mut t = String::new();
            let mut depth = 0;
            let mut esc = false;
            for ch in chars.by_ref() {
                t.push(ch);
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == '(' {
                    depth += 1;
                } else if ch == ')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            toks.push(t);
        } else {
            let mut t = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() || ch == '(' {
                    break;
                }
                t.push(ch);
                chars.next();
            }
            toks.push(t);
        }
    }
    let mut out = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for t in toks {
        let is_op = t.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
        cur.push(t);
        if is_op {
            out.push(cur.join(" "));
            cur.clear();
        }
    }
    assert!(cur.is_empty(), "dangling operands {cur:?}");
    out
}

/// Parse one `%AI3_BeginPattern` block starting at `lines[*i]`; returns the
/// name and the raw (still string-encoded) layer art for later validation.
/// A pattern layer: its colour-definition lines and its decoded art lines.
type TileLayer<'a> = (Vec<&'a str>, Vec<String>);

fn parse_pattern<'a>(lines: &[&'a str], i: &mut usize) -> (String, Vec<TileLayer<'a>>) {
    let begin = lines[*i];
    let ctx = |k: usize| format!("pattern definition line {k}: {:?}", lines.get(k));
    let (name, _) = split_string(begin).unwrap_or_else(|| panic!("{}", ctx(*i)));
    *i += 1;
    let (n2, rest) = split_string(lines[*i]).unwrap_or_else(|| panic!("{}", ctx(*i)));
    assert_eq!(name, n2, "{}: name mismatch", ctx(*i));
    let toks: Vec<&str> = rest.split_whitespace().collect();
    assert_eq!(toks.len(), 6, "{}: (name) llx lly urx ury [", ctx(*i));
    assert_eq!((toks[0], toks[5]), ("STR", "["), "{}", ctx(*i));
    let bb = parse_nums(&toks[1..5], &ctx(*i));
    assert!(bb[2] > bb[0] && bb[3] > bb[1], "{}: empty tile", ctx(*i));
    *i += 1;
    assert_eq!(lines[*i], "%AI3_Tile", "{}", ctx(*i));
    *i += 1;
    let mut layers = Vec::new();
    loop {
        let l = lines[*i];
        if l == "] E" {
            *i += 1;
            break;
        }
        // (colour definition) @
        assert!(l.starts_with('('), "{}: expected colour string", ctx(*i));
        let mut color = vec![&l[1..]];
        while !lines[*i].ends_with(") @") {
            *i += 1;
            color.push(lines[*i]);
        }
        let last = color.len() - 1;
        color[last] = color[last].strip_suffix(") @").unwrap();
        *i += 1;
        // (tile definition) &
        assert_eq!(lines[*i], "(", "{}: expected tile string", ctx(*i));
        *i += 1;
        assert_eq!(lines[*i], "%AI6_BeginPatternLayer", "{}", ctx(*i));
        *i += 1;
        let mut raw = Vec::new();
        while lines[*i] != "%AI6_EndPatternLayer" {
            raw.push(lines[*i]);
            *i += 1;
            assert!(*i < lines.len(), "unterminated pattern layer");
        }
        *i += 1;
        assert_eq!(lines[*i], ") &", "{}", ctx(*i));
        *i += 1;
        let joined = raw.join("\n");
        assert!(ps_parens_balanced(&joined), "pattern {name:?}: tile art is not a valid PostScript string body");
        assert!(joined.len() < 65_535, "pattern {name:?}: layer string exceeds 64K");
        let decoded: Vec<String> = raw.iter().map(|l| l.replace("\\\\", "\\")).collect();
        layers.push((color, decoded));
    }
    assert_eq!(lines[*i], "%AI3_EndPattern", "{}", ctx(*i));
    *i += 1;
    assert!(!layers.is_empty(), "pattern {name:?} has no layers");
    (name, layers)
}

/// Parse one `%AI5_BeginGradient` block starting at `lines[*i]`.
fn parse_gradient(lines: &[&str], i: &mut usize) -> String {
    let ctx = |k: usize| format!("gradient definition line {k}: {:?}", lines.get(k));
    let (name, _) = split_string(lines[*i]).unwrap_or_else(|| panic!("{}", ctx(*i)));
    *i += 1;
    let (n2, rest) = split_string(lines[*i]).unwrap_or_else(|| panic!("{}", ctx(*i)));
    assert_eq!(name, n2, "{}", ctx(*i));
    let toks: Vec<&str> = rest.split_whitespace().collect();
    assert_eq!(toks.len(), 4, "{}: (name) type nColors Bd", ctx(*i));
    assert_eq!((toks[0], toks[3]), ("STR", "Bd"), "{}", ctx(*i));
    let kind: u32 = toks[1].parse().unwrap();
    assert!(kind <= 1, "{}", ctx(*i));
    let n: usize = toks[2].parse().unwrap();
    assert!(n >= 2, "{}: at least two stops", ctx(*i));
    *i += 1;
    assert_eq!(lines[*i], "[", "{}", ctx(*i));
    *i += 1;
    let mut ramps = Vec::new();
    while lines[*i] != "BD" {
        let a = lines[*i];
        let b = lines[*i + 1];
        let ops_a = a.strip_suffix(" %_BS").unwrap_or_else(|| panic!("{}: expected %_BS", ctx(*i)));
        let ops_b = b
            .strip_prefix("%_")
            .and_then(|s| s.strip_suffix(" Bs"))
            .unwrap_or_else(|| panic!("{}: expected %_… Bs", ctx(*i + 1)));
        assert_eq!(ops_a, ops_b, "{}: %_BS and Bs differ", ctx(*i));
        let (string, flat) = match split_string(ops_a) {
            Some((s, f)) => (Some(s), f),
            None => (None, ops_a.to_string()),
        };
        let t: Vec<&str> = flat.split_whitespace().collect();
        assert!(t.len() >= 4, "{}", ctx(*i));
        let tail = parse_nums(&t[t.len() - 3..], &ctx(*i));
        let style = tail[0] as usize;
        assert!(style <= 4, "{}: colorStyle", ctx(*i));
        assert!((13.0..=87.0).contains(&tail[1]), "{}: midpoint", ctx(*i));
        assert!((0.0..=100.0).contains(&tail[2]), "{}: rampPoint", ctx(*i));
        let spec = &t[..t.len() - 3];
        assert_eq!(spec.len(), STOP_SPEC_LEN[style], "{}: colorSpec length for style {style}", ctx(*i));
        assert_eq!(string.is_some(), style >= 3, "{}: custom colour name", ctx(*i));
        let nums: Vec<&str> = spec.iter().copied().filter(|t| *t != "STR").collect();
        assert!(parse_nums(&nums, &ctx(*i)).iter().all(|v| (0.0..=1.0).contains(v)), "{}: colour range", ctx(*i));
        ramps.push(tail[2]);
        *i += 2;
    }
    assert_eq!(ramps.len(), n, "gradient {name:?}: nColors {n} but {} stops", ramps.len());
    let up = ramps.windows(2).all(|w| w[0] <= w[1]);
    let down = ramps.windows(2).all(|w| w[0] >= w[1]);
    assert!(up || down, "gradient {name:?}: ramp points not monotone: {ramps:?}");
    *i += 1;
    assert_eq!(lines[*i], "%AI5_EndGradient", "{}", ctx(*i));
    *i += 1;
    name
}

/// Validate an AI8 file's structure; panics with a descriptive message on error.
pub fn validate_ai8(ai: &str) -> Ai8Stats {
    assert!(ai.is_ascii(), "AI8 output must be 7-bit ASCII");
    let lines: Vec<&str> = ai.lines().collect();
    assert_eq!(lines.first().map(|l| l.trim_end()), Some("%!PS-Adobe-3.0"), "bad first line");
    let idx = |needle: &str| {
        lines
            .iter()
            .position(|l| l.starts_with(needle))
            .unwrap_or_else(|| panic!("missing {needle}"))
    };
    let order = [
        idx("%%Creator: Adobe Illustrator(R) 8.0"),
        idx("%%BoundingBox:"),
        idx("%AI5_FileFormat 4.0"),
        idx("%%EndComments"),
        idx("%%BeginProlog"),
        idx("%%EndProlog"),
        idx("%%BeginSetup"),
        idx("%%EndSetup"),
        idx("%AI5_BeginLayer"),
        idx("%AI5_EndLayer--"),
        idx("%%Trailer"),
        idx("%%EOF"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "header sections out of order: {order:?}");

    let bbox: Vec<f64> = lines[order[1]]["%%BoundingBox:".len()..]
        .split_whitespace()
        .map(|t| t.parse().expect("integer bbox"))
        .collect();
    assert_eq!(bbox.len(), 4);
    assert!(bbox[0] <= bbox[2] && bbox[1] <= bbox[3], "inverted bbox {bbox:?}");

    let header = &lines[..order[3]];

    // ---- setup section ----
    let setup = &lines[order[6] + 1..order[7]];
    let mut stats = Ai8Stats::default();
    let mut tiles = Vec::new();
    let mut bn: Option<usize> = None;
    let mut nonprinting = false;
    let mut i = 0;
    while i < setup.len() {
        let l = setup[i];
        if l.ends_with("/initialize get exec") {
            assert!(!nonprinting, "procset init inside non-printing block");
            i += 1;
        } else if l == "%AI5_Begin_NonPrinting" {
            assert!(!nonprinting, "nested non-printing block");
            nonprinting = true;
            i += 1;
            assert_eq!(setup.get(i), Some(&"Np"), "Np must follow %AI5_Begin_NonPrinting");
            i += 1;
        } else if l == "%AI5_End_NonPrinting--" {
            assert!(nonprinting, "unbalanced %AI5_End_NonPrinting--");
            nonprinting = false;
            i += 1;
        } else if l.starts_with("%AI3_BeginPattern: ") {
            assert!(nonprinting, "pattern definition outside non-printing block");
            assert!(bn.is_none(), "patterns must precede the gradient definitions");
            let (name, layers) = parse_pattern(setup, &mut i);
            assert!(!stats.pattern_defs.contains(&name), "pattern {name:?} defined twice");
            stats.pattern_defs.push(name.clone());
            tiles.push((name, layers));
        } else if let Some(n) = l.strip_suffix(" Bn") {
            assert!(nonprinting, "Bn outside non-printing block");
            assert!(bn.is_none(), "Bn given twice");
            bn = Some(n.parse().expect("integer Bn"));
            i += 1;
        } else if l.starts_with("%AI5_BeginGradient: ") {
            assert!(bn.is_some(), "gradient definition before Bn");
            let name = parse_gradient(setup, &mut i);
            assert!(!stats.gradient_defs.contains(&name), "gradient {name:?} defined twice");
            stats.gradient_defs.push(name);
        } else {
            panic!("unexpected setup line {l:?}");
        }
    }
    assert!(!nonprinting, "unterminated non-printing block");
    assert_eq!(bn.unwrap_or(0), stats.gradient_defs.len(), "Bn must equal the number of gradient definitions");

    for (name, layers) in &tiles {
        for (k, (color, art)) in layers.iter().enumerate() {
            let label = format!("pattern {name:?} layer {k}");
            let cx = ArtCtx { patterns: &[], gradients: &[], in_tile: true, color_only: true };
            let ops = split_operators(&color.join("\n"));
            let ops: Vec<&str> = ops.iter().map(String::as_str).collect();
            assert!(!ops.is_empty(), "{label}: empty colour definition");
            validate_art(&ops, &cx, &mut Ai8Stats::default(), &format!("{label} colour"));
            let art: Vec<&str> = art.iter().map(String::as_str).collect();
            let cx = ArtCtx { patterns: &[], gradients: &[], in_tile: true, color_only: false };
            let mut tile_stats = Ai8Stats::default();
            validate_art(&art, &cx, &mut tile_stats, &label);
            stats.tile_paths += tile_stats.paths;
            // Spot names inside tiles are octal-escaped; they must be declared
            // in the header like any other spot colour.
            for l in &art {
                if (l.ends_with(" x") || l.ends_with(" X"))
                    && let Some((spot, _)) = split_string(l)
                {
                    let enc = spot.replace("\\050", "\\(").replace("\\051", "\\)").replace("\\134", "\\\\");
                    let needle = format!("({enc})");
                    assert!(header.iter().any(|h| h.contains(&needle)), "{label}: spot {needle} not declared");
                }
            }
        }
    }

    // ---- art ----
    let art = &lines[order[8] + 1..order[9]];
    let cx = ArtCtx { patterns: &stats.pattern_defs.clone(), gradients: &stats.gradient_defs.clone(), in_tile: false, color_only: false };
    validate_art(art, &cx, &mut stats, "art");
    stats
}
