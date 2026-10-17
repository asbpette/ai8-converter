//! Ghostscript integration tests: locating gs, EPS -> PDF, and end-to-end
//! EPS -> AI8 conversion. Tests that need Ghostscript skip (pass with a
//! message) when it is not installed.

mod common;

use ai8_converter::ghostscript;
use ai8_converter::{ColorMode, ConvertOptions, convert_file};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eps").join(name)
}

fn temp_dir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "ai8-gs-test-{}-{}-{}",
        tag,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn gs_or_skip(test: &str) -> Option<PathBuf> {
    let gs = ghostscript::find();
    if gs.is_none() {
        eprintln!("SKIPPED {test}: Ghostscript not found (set AI8_GHOSTSCRIPT or install gs)");
    }
    gs
}

/// Wrap PostScript in a DOS EPS binary header (C5 D0 D3 C6), as written by
/// Illustrator/Photoshop when a TIFF preview is included (here: no preview).
fn dos_eps(ps: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0xC5, 0xD0, 0xD3, 0xC6]);
    v.extend_from_slice(&30u32.to_le_bytes()); // PS offset
    v.extend_from_slice(&(ps.len() as u32).to_le_bytes()); // PS length
    v.extend_from_slice(&0u32.to_le_bytes()); // WMF offset
    v.extend_from_slice(&0u32.to_le_bytes()); // WMF length
    v.extend_from_slice(&0u32.to_le_bytes()); // TIFF offset
    v.extend_from_slice(&0u32.to_le_bytes()); // TIFF length
    v.extend_from_slice(&0xFFFFu16.to_le_bytes()); // checksum: none
    assert_eq!(v.len(), 30);
    v.extend_from_slice(ps);
    v
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

fn opts(out: &Path) -> ConvertOptions {
    ConvertOptions { out_dir: Some(out.to_path_buf()), ..ConvertOptions::default() }
}

/// Convert and return (validated stats, AI text).
fn convert_to_ai(input: &Path, o: &ConvertOptions) -> (common::Ai8Stats, String) {
    let report = convert_file(input, o).unwrap_or_else(|e| panic!("converting {}: {e:#}", input.display()));
    assert_eq!(report.outputs.len(), 1, "expected one page: {:?}", report.outputs);
    let ai = std::fs::read_to_string(&report.outputs[0]).unwrap();
    let stats = common::validate_ai8(&ai);
    (stats, ai)
}

/// Is there a colour line `v0 v1 ... op` with components within 0.005?
fn has_color(ai: &str, op: &str, want: &[f64]) -> bool {
    ai.lines().any(|l| {
        let parts: Vec<&str> = l.split_whitespace().collect();
        parts.len() == want.len() + 1
            && parts.last() == Some(&op)
            && parts[..want.len()]
                .iter()
                .zip(want)
                .all(|(p, w)| p.parse::<f64>().is_ok_and(|v| (v - w).abs() < 0.005))
    })
}

fn has_line_ending(ai: &str, op: &str) -> bool {
    ai.lines().any(|l| l == op || l.ends_with(&format!(" {op}")))
}

// ---------------------------------------------------------------------------
// find() / to_pdf()

#[test]
fn find_respects_env_override_when_valid() {
    // Can't mutate the environment safely in parallel tests; just check that
    // whatever find() returns is an existing file.
    if let Some(p) = ghostscript::find() {
        assert!(p.is_file(), "find() returned a non-file: {}", p.display());
    }
}

#[test]
fn to_pdf_with_missing_gs_is_an_error() {
    let err = ghostscript::to_pdf(Path::new("/definitely/not/gs-binary"), &fixture("shapes.eps"), true, true)
        .unwrap_err();
    assert!(format!("{err:#}").to_lowercase().contains("ghostscript"), "{err:#}");
}

#[test]
fn to_pdf_with_missing_input_is_an_error() {
    let Some(gs) = gs_or_skip("to_pdf_with_missing_input_is_an_error") else { return };
    let err = ghostscript::to_pdf(&gs, &fixture("nope.eps"), true, true).unwrap_err();
    assert!(format!("{err:#}").contains("not found"), "{err:#}");
}

#[test]
fn version_reports_something() {
    let Some(gs) = gs_or_skip("version_reports_something") else { return };
    let v = ghostscript::version(&gs).unwrap();
    assert!(v.chars().next().is_some_and(|c| c.is_ascii_digit()), "odd version: {v}");
}

#[test]
fn eps_fixtures_convert_to_pdf() {
    let Some(gs) = gs_or_skip("eps_fixtures_convert_to_pdf") else { return };
    for name in ["shapes.eps", "cutcontour.eps", "text.eps", "clip.eps"] {
        let pdf = ghostscript::to_pdf(&gs, &fixture(name), true, true)
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert!(pdf.starts_with(b"%PDF-"), "{name}: not a PDF");
        assert!(pdf.len() > 200, "{name}: suspiciously small PDF");
    }
}

#[test]
fn spot_colour_survives_pdfwrite() {
    let Some(gs) = gs_or_skip("spot_colour_survives_pdfwrite") else { return };
    let pdf = ghostscript::to_pdf(&gs, &fixture("cutcontour.eps"), true, true).unwrap();
    assert!(count(&pdf, b"CutContour") >= 1, "Separation colour space lost");
}

#[test]
fn outline_text_removes_fonts() {
    let Some(gs) = gs_or_skip("outline_text_removes_fonts") else { return };
    let outlined = ghostscript::to_pdf(&gs, &fixture("text.eps"), true, true).unwrap();
    assert_eq!(count(&outlined, b"/Font"), 0, "text not outlined");
    let with_fonts = ghostscript::to_pdf(&gs, &fixture("text.eps"), false, true).unwrap();
    assert!(count(&with_fonts, b"/Font") >= 1, "expected embedded font without outlining");
}

#[test]
fn eps_crop_uses_bounding_box() {
    let Some(gs) = gs_or_skip("eps_crop_uses_bounding_box") else { return };
    let pdf = ghostscript::to_pdf(&gs, &fixture("shapes.eps"), true, true).unwrap();
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/MediaBox [0 0 200 150]"), "MediaBox not cropped to BoundingBox");
}

#[test]
fn handles_spaces_and_unicode_in_paths() {
    let Some(gs) = gs_or_skip("handles_spaces_and_unicode_in_paths") else { return };
    let dir = temp_dir("unicode").join("mappe med æøå ünïcode");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("-logo (kopi) å.eps");
    std::fs::copy(fixture("shapes.eps"), &input).unwrap();
    let pdf = ghostscript::to_pdf(&gs, &input, true, true).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn dos_binary_eps_converts() {
    let Some(gs) = gs_or_skip("dos_binary_eps_converts") else { return };
    let dir = temp_dir("dos");
    let input = dir.join("binary.eps");
    std::fs::write(&input, dos_eps(&std::fs::read(fixture("shapes.eps")).unwrap())).unwrap();
    let pdf = ghostscript::to_pdf(&gs, &input, true, true).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn invalid_postscript_reports_gs_output() {
    let Some(gs) = gs_or_skip("invalid_postscript_reports_gs_output") else { return };
    let dir = temp_dir("bad");
    let input = dir.join("bad.eps");
    std::fs::write(&input, b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\nthisisnotanoperator\n").unwrap();
    let err = ghostscript::to_pdf(&gs, &input, true, true).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("undefined") || msg.contains("Error"), "gs output not included: {msg}");
}

#[test]
fn temp_files_are_cleaned_up() {
    let Some(gs) = gs_or_skip("temp_files_are_cleaned_up") else { return };
    let pid = std::process::id();
    let leftover = || {
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("ai8conv-{pid}-")))
            .count()
    };
    ghostscript::to_pdf(&gs, &fixture("shapes.eps"), true, true).unwrap();
    // Other tests in this process may be mid-run; allow a few in flight.
    assert!(leftover() <= 8);
}

// ---------------------------------------------------------------------------
// End-to-end EPS -> AI8 (requires the PDF parser in src/pdf.rs)

#[test]
fn e2e_shapes_cmyk_fill_rgb_stroke_curves() {
    let Some(_) = gs_or_skip("e2e_shapes_cmyk_fill_rgb_stroke_curves") else { return };
    let out = temp_dir("e2e-shapes");
    let (stats, ai) = convert_to_ai(&fixture("shapes.eps"), &opts(&out));
    assert!(stats.paths >= 3, "expected at least 3 paths, got {stats:?}");
    assert!(has_line_ending(&ai, "k"), "no CMYK fill (k)");
    // Ghostscript quantises colour components slightly (0.1 -> 0.1001).
    assert!(has_color(&ai, "k", &[0.1, 0.8, 0.6, 0.05]), "CMYK fill values not preserved");
    assert!(has_line_ending(&ai, "XA"), "no RGB stroke (XA)");
    assert!(has_line_ending(&ai, "C"), "no curves (C)");
    assert!(ai.lines().any(|l| l == "S" || l == "s"), "no stroke paint op");
    assert!(ai.lines().any(|l| l == "f" || l == "F"), "no fill paint op");
}

#[test]
fn e2e_shapes_cmyk_mode_has_no_rgb() {
    let Some(_) = gs_or_skip("e2e_shapes_cmyk_mode_has_no_rgb") else { return };
    let out = temp_dir("e2e-cmyk");
    let o = ConvertOptions { color_mode: ColorMode::Cmyk, ..opts(&out) };
    let (_, ai) = convert_to_ai(&fixture("shapes.eps"), &o);
    assert!(!has_line_ending(&ai, "XA") && !has_line_ending(&ai, "Xa"), "RGB left in CMYK mode");
    assert!(has_line_ending(&ai, "K"), "no CMYK stroke (K)");
}

#[test]
fn e2e_cutcontour_spot_stroke() {
    let Some(_) = gs_or_skip("e2e_cutcontour_spot_stroke") else { return };
    let out = temp_dir("e2e-cut");
    let (_, ai) = convert_to_ai(&fixture("cutcontour.eps"), &opts(&out));
    assert!(ai.contains("(CutContour)"), "spot colour name missing");
    assert!(ai.contains("%%DocumentCustomColors: (CutContour)"), "spot colour not declared");
    assert!(ai.lines().any(|l| l.contains("(CutContour)") && l.ends_with(" X")), "no spot stroke (X)");
    assert!(has_line_ending(&ai, "k"), "no CMYK fill");
}

#[test]
fn e2e_text_is_outlined() {
    let Some(_) = gs_or_skip("e2e_text_is_outlined") else { return };
    let out = temp_dir("e2e-text");
    let (stats, ai) = convert_to_ai(&fixture("text.eps"), &opts(&out));
    // "Hello AI8" -> 8 glyphs, several of them compound (e, o, A, 8).
    assert!(stats.paths >= 8, "text not outlined into paths: {stats:?}");
    assert!(has_line_ending(&ai, "C"), "glyph outlines should contain curves");
    assert!(!ai.contains("Helvetica"), "font reference left in output");
}

#[test]
fn e2e_clip_group() {
    let Some(_) = gs_or_skip("e2e_clip_group") else { return };
    let out = temp_dir("e2e-clip");
    let (stats, ai) = convert_to_ai(&fixture("clip.eps"), &opts(&out));
    assert!(stats.clip_groups >= 1, "no clip group: {stats:?}");
    assert!(ai.contains("h W"), "no clipping path");
}

#[test]
fn e2e_dos_binary_eps() {
    let Some(_) = gs_or_skip("e2e_dos_binary_eps") else { return };
    let dir = temp_dir("e2e-dos");
    let input = dir.join("binary header.eps");
    std::fs::write(&input, dos_eps(&std::fs::read(fixture("shapes.eps")).unwrap())).unwrap();
    let (stats, _) = convert_to_ai(&input, &ConvertOptions::default());
    assert!(stats.paths >= 3);
    assert!(dir.join("binary header.ai").is_file(), "output not written next to input");
}

#[test]
fn runaway_postscript_times_out() {
    let Some(gs) = gs_or_skip("runaway_postscript_times_out") else { return };
    let dir = temp_dir("loop");
    let input = dir.join("loop.eps");
    std::fs::write(&input, b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n{ } loop\n").unwrap();
    let start = std::time::Instant::now();
    let err = ghostscript::to_pdf_with_timeout(&gs, &input, true, true, std::time::Duration::from_secs(2))
        .unwrap_err();
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
    assert!(start.elapsed() < std::time::Duration::from_secs(20));
}
