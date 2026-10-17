//! Shared test helper: a strict structural validator for generated AI8 files.
#![allow(dead_code)]

/// Summary of the art section of a validated AI8 file.
#[derive(Debug, Default)]
pub struct Ai8Stats {
    pub paths: usize,
    pub groups: usize,
    pub clip_groups: usize,
    pub compound_paths: usize,
}

const PAINT_OPS: &[&str] = &["N", "n", "F", "f", "S", "s", "B", "b"];

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

    let art = &lines[order[8] + 1..order[9]];
    let mut stats = Ai8Stats::default();
    let mut stack: Vec<&'static str> = Vec::new(); // "u", "q", "*u"
    let mut in_path = false;
    let mut clip_pending = false;
    for (i, raw) in art.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        // Replace a (string) operand by a placeholder token before splitting.
        let flat;
        let line_for_toks = match (line.find('('), line.rfind(')')) {
            (Some(a), Some(b)) if a < b => {
                flat = format!("{}STR{}", &line[..a], &line[b + 1..]);
                flat.as_str()
            }
            _ => line,
        };
        let toks: Vec<&str> = line_for_toks.split_whitespace().collect();
        let op = *toks.last().unwrap();
        let operands = &toks[..toks.len() - 1];
        let ctx = || format!("art line {}: {line:?}", i + 1);
        let nums = |n: usize| {
            assert_eq!(operands.len(), n, "{}: expected {n} operands", ctx());
            for t in operands {
                let v: f64 = t.parse().unwrap_or_else(|_| panic!("{}: bad number {t}", ctx()));
                assert!(v.is_finite(), "{}", ctx());
            }
        };
        match op {
            "Lb" => nums(13),
            "Ln" | "LB" | "A" => {}
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
            op if PAINT_OPS.contains(&op) => {
                assert!(operands.is_empty(), "{}", ctx());
                assert!(in_path, "{}: paint without path", ctx());
                in_path = false;
                if op == "n" && line.trim() == "n" && clip_pending {
                    // the clip path(s) of a clip group
                } else {
                    clip_pending = false;
                    stats.paths += 1;
                }
            }
            "g" | "G" => nums(1),
            "k" | "K" => nums(4),
            "Xa" | "XA" => nums(3),
            "x" | "X" => {
                assert_eq!(operands.len(), 6, "{}", ctx());
                assert!(line.contains('(') && line.contains(')'), "{}: spot name", ctx());
            }
            "O" | "R" | "XR" | "D" => nums(1),
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
    assert!(!in_path, "unterminated path at end of layer");
    assert!(stack.is_empty(), "unclosed groups at end of layer: {stack:?}");
    stats
}
