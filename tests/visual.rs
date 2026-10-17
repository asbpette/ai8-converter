//! Visual regression tests: every fixture is converted to AI8 (native and
//! expanded patterns), the AI8 output is rendered with Ghostscript through
//! our AI8 procset (`assets/ai8-procset.ps`), the original is rendered with
//! Ghostscript (EPS/PDF) or resvg (SVG), and the two images are compared.
//!
//! Skipped when Ghostscript is not installed. On failure the original,
//! converted and diff images are written to `target/visual-diff/`. Set
//! `AI8_VISUAL_SAVE=1` to write them for every fixture.

use ai8_converter::preview::{self, Raster};
use ai8_converter::{ConvertOptions, PatternMode, convert_file, ghostscript};
use std::path::{Path, PathBuf};

/// Rendering resolution. 72 dpi puts the fixtures' pattern steps on whole
/// pixels, so Ghostscript's own tile rounding does not shift the phase.
const DPI: u32 = 72;

/// SVG fixtures are rendered at 96 dpi (one pixel per user unit): resvg
/// rasterises a pattern tile at a whole number of pixels and repeats that
/// bitmap, so tiles that are not a whole number of pixels drift and show
/// seams in the *reference* image. The SVG fixtures use tile sizes that are
/// whole numbers at this resolution.
const SVG_DPI: u32 = 96;

fn dpi(input: &Path) -> u32 {
    let default = if input.extension().is_some_and(|e| e == "svg") { SVG_DPI } else { DPI };
    std::env::var("AI8_VISUAL_DPI").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
/// Per-channel difference (0-255) below which two pixels count as equal.
const THRESHOLD: u8 = 48;
/// Neighbourhood (pixels) searched for a matching colour (anti-aliasing).
const RADIUS: u32 = 1;
/// Both images are rendered at this multiple of the resolution and shrunk
/// with a box filter, so that all edges are anti-aliased the same way
/// (Ghostscript does not anti-alias clip paths and pattern fills).
const SUPERSAMPLE: u32 = 3;
/// Default limit: differing pixels as a percentage of ink pixels.
const MAX_PERCENT: f64 = 0.5;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures(dir: &str, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root().join("tests/fixtures").join(dir))
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    v.retain(|p| p.extension().and_then(|e| e.to_str()) == Some(ext));
    v.sort();
    v
}

/// Known, documented approximations: (fixture file name, mode or None for
/// both, max percent of ink allowed to differ).
const ALLOWANCES: &[(&str, Option<PatternMode>, f64)] = &[
    // In PostScript the inner pattern is anchored in *page* space, so with an
    // inner step (4) that does not divide the outer one (30) the dots shift
    // from one outer tile to the next. PDF (Ghostscript's intermediate) and
    // AI8 tiles are periodic, so this cannot be represented; the PDF that
    // Ghostscript writes already looks like our output (2.3% differs).
    // pattern_nested_aligned.eps covers the representable case exactly.
    ("pattern_nested.eps", None, 3.0),
    // Text is outlined with the installed system fonts while resvg renders the
    // reference with the same fonts but different hinting/anti-aliasing; the
    // edge pixels differ more on Windows (0.65%) than on macOS (0.3%).
    ("text.svg", None, 1.5),
];

fn limit(name: &str, mode: PatternMode) -> f64 {
    ALLOWANCES
        .iter()
        .find(|(n, m, _)| *n == name && m.is_none_or(|m| m == mode))
        .map(|a| a.2)
        .unwrap_or(MAX_PERCENT)
}

fn save(dir: &Path, stem: &str, orig: &Raster, conv: &Raster, diff: &Raster) {
    let _ = std::fs::create_dir_all(dir);
    for (suffix, img) in [("original", orig), ("converted", conv), ("diff", diff)] {
        if let Ok(png) = preview::encode_png(img) {
            let _ = std::fs::write(dir.join(format!("{stem}-{suffix}.png")), png);
        }
    }
    let both = preview::side_by_side(orig, conv, 8);
    if let Ok(png) = preview::encode_png(&both) {
        let _ = std::fs::write(dir.join(format!("{stem}-side-by-side.png")), png);
    }
}

/// Convert, render and compare one fixture; returns a failure message.
fn check(gs: &Path, input: &Path, mode: PatternMode) -> Result<String, String> {
    let name = input.file_name().unwrap().to_string_lossy().into_owned();
    let mode_name = match mode {
        PatternMode::Native => "native",
        PatternMode::Expand => "expand",
    };
    // One folder per input kind and mode: fixtures of different kinds may
    // share a name (text.eps, text.svg) and the tests run in parallel.
    let ext = input.extension().unwrap().to_string_lossy().into_owned();
    let out_dir = root().join("target/visual-out").join(&ext).join(mode_name);
    let opts = ConvertOptions {
        pattern_mode: mode,
        out_dir: Some(out_dir),
        ghostscript: Some(gs.to_path_buf()),
        ..ConvertOptions::default()
    };
    let report = convert_file(input, &opts).map_err(|e| format!("{name} ({mode_name}): conversion failed: {e:#}"))?;
    let mut lines = Vec::new();
    let mut failures = Vec::new();
    for (page, ai) in report.outputs.iter().enumerate() {
        let label = format!("{name} p{} ({mode_name})", page + 1);
        let orig = preview::render_input_png(Some(gs), input, page, dpi(input) * SUPERSAMPLE)
            .and_then(|b| preview::decode_png(&b))
            .map(|r| preview::downsample(&r, SUPERSAMPLE))
            .map_err(|e| format!("{label}: rendering the original failed: {e:#}"))?;
        let conv = preview::render_ai8_png_strict(gs, ai, dpi(input) * SUPERSAMPLE)
            .and_then(|b| preview::decode_png(&b))
            .map(|r| preview::downsample(&r, SUPERSAMPLE))
            .map_err(|e| format!("{label}: rendering the AI8 output failed: {e:#}"))?;
        let stats = preview::compare(&orig, &conv, THRESHOLD, RADIUS);
        let size_ok = orig.width.abs_diff(conv.width) <= 2 && orig.height.abs_diff(conv.height) <= 2;
        let max = limit(&name, mode);
        let pct = stats.percent_of_ink();
        let ok = size_ok && pct <= max;
        let stem = format!("{}-{ext}-p{}-{mode_name}", input.file_stem().unwrap().to_string_lossy(), page + 1);
        if !ok || std::env::var_os("AI8_VISUAL_SAVE").is_some() {
            save(&root().join("target/visual-diff"), &stem, &orig, &conv, &stats.diff_image);
        }
        lines.push(format!(
            "{label}: {}x{} vs {}x{}, {:.3}% of ink differs ({} px of {} ink px), limit {max}%",
            orig.width, orig.height, conv.width, conv.height, pct, stats.differing, stats.ink
        ));
        if !ok {
            failures.push(format!(
                "{label}: {:.3}% of ink differs (limit {max}%), sizes {}x{} / {}x{}; images in target/visual-diff/{stem}-*.png",
                pct, orig.width, orig.height, conv.width, conv.height
            ));
        }
    }
    if failures.is_empty() { Ok(lines.join("\n")) } else { Err(format!("{}\n{}", lines.join("\n"), failures.join("\n"))) }
}

fn run(dir: &str, ext: &str, mode: PatternMode) {
    let Some(gs) = ghostscript::find() else {
        eprintln!("SKIPPED visual tests: Ghostscript not found (set AI8_GHOSTSCRIPT or install gs)");
        return;
    };
    let files = fixtures(dir, ext);
    assert!(!files.is_empty(), "no fixtures in tests/fixtures/{dir}");
    let mut errors = Vec::new();
    for f in &files {
        match check(&gs, f, mode) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                println!("{e}");
                errors.push(e);
            }
        }
    }
    assert!(errors.is_empty(), "visual differences:\n{}", errors.join("\n"));
}

#[test]
fn eps_native() {
    run("eps", "eps", PatternMode::Native);
}

#[test]
fn eps_expand() {
    run("eps", "eps", PatternMode::Expand);
}

#[test]
fn svg_native() {
    run("svg", "svg", PatternMode::Native);
}

#[test]
fn svg_expand() {
    run("svg", "svg", PatternMode::Expand);
}

#[test]
fn pdf_native() {
    run("pdf", "pdf", PatternMode::Native);
}

#[test]
fn pdf_expand() {
    run("pdf", "pdf", PatternMode::Expand);
}

/// The preview of an empty art section is a blank page of the art size.
#[test]
fn renders_blank_page() {
    let Some(gs) = ghostscript::find() else { return };
    let dir = root().join("target/visual-out/blank");
    std::fs::create_dir_all(&dir).unwrap();
    let ai_path = dir.join("blank.ai");
    let doc = ai8_converter::ir::Document::new(ai8_converter::ir::Rect { llx: 0.0, lly: 0.0, urx: 144.0, ury: 72.0 });
    std::fs::write(&ai_path, ai8_converter::write_ai8(&doc, &Default::default())).unwrap();
    let img = preview::decode_png(&preview::render_ai8_png_strict(&gs, &ai_path, 72).unwrap()).unwrap();
    assert_eq!((img.width, img.height), (144, 72));
    assert!(img.rgba.chunks(4).all(|p| p == [255, 255, 255, 255]));
    let pdf = dir.join("blank.pdf");
    preview::ai8_to_pdf(&gs, &ai_path, &pdf).unwrap();
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
}

/// DXF has no independent reference renderer here, so the native and the
/// expanded output are rendered and compared with each other (they are two
/// encodings of the same art), and each must render without PostScript
/// errors and show ink.
#[test]
fn dxf_native_matches_expand() {
    let Some(gs) = ghostscript::find() else {
        eprintln!("SKIPPED visual tests: Ghostscript not found");
        return;
    };
    let files = fixtures("dxf", "dxf");
    assert!(!files.is_empty());
    let mut errors = Vec::new();
    for input in &files {
        let name = input.file_name().unwrap().to_string_lossy().into_owned();
        let mut renders = Vec::new();
        for (mode, mode_name) in [(PatternMode::Native, "native"), (PatternMode::Expand, "expand")] {
            let opts = ConvertOptions {
                pattern_mode: mode,
                out_dir: Some(root().join("target/visual-out/dxf").join(mode_name)),
                ghostscript: Some(gs.clone()),
                ..ConvertOptions::default()
            };
            let report = convert_file(input, &opts).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let img = preview::render_ai8_png_strict(&gs, &report.outputs[0], DPI * SUPERSAMPLE)
                .and_then(|b| preview::decode_png(&b))
                .map(|r| preview::downsample(&r, SUPERSAMPLE))
                .unwrap_or_else(|e| panic!("{name} ({mode_name}): {e:#}"));
            assert!(img.rgba.chunks(4).any(|p| p[..3] != [255, 255, 255]), "{name} ({mode_name}) renders blank");
            renders.push(img);
        }
        let stats = preview::compare(&renders[0], &renders[1], THRESHOLD, RADIUS);
        let stem = format!("{}-dxf", input.file_stem().unwrap().to_string_lossy());
        let pct = stats.percent_of_ink();
        println!("{name}: native vs expand {:.3}% of ink differs ({} of {} px)", pct, stats.differing, stats.ink);
        if pct > MAX_PERCENT || std::env::var_os("AI8_VISUAL_SAVE").is_some() {
            save(&root().join("target/visual-diff"), &stem, &renders[0], &renders[1], &stats.diff_image);
        }
        if pct > MAX_PERCENT {
            errors.push(format!("{name}: native and expanded output differ in {pct:.3}% of ink"));
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
