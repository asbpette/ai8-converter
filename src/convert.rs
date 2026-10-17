//! High-level conversion pipeline: detect input type, parse to IR, write AI8.

use crate::ai8::{ColorMode, WriteOptions, write_ai8};
use crate::ir::Document;
use crate::{ghostscript, pdf, svg};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// PDF, or a PDF-compatible Illustrator 9+ .ai file.
    Pdf,
    /// EPS / PS / legacy PostScript-based .ai (needs Ghostscript).
    PostScript,
    /// SVG or SVGZ.
    Svg,
}

/// How pattern fills/strokes are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatternMode {
    /// Native Illustrator 8 pattern swatches (editable in Illustrator).
    #[default]
    Native,
    /// Expand patterns into plain clipped tile paths (for cutters/RIPs that
    /// do not understand pattern fills).
    Expand,
}

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub color_mode: ColorMode,
    pub pattern_mode: PatternMode,
    /// Convert text to outlines by routing PDFs through Ghostscript as well
    /// (EPS/PS always have their text outlined).
    pub outline_text_in_pdf: bool,
    /// Directory for output files; `None` = next to the input file.
    pub out_dir: Option<PathBuf>,
    /// Override the Ghostscript executable; `None` = auto-detect.
    pub ghostscript: Option<PathBuf>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self { color_mode: ColorMode::Preserve, pattern_mode: PatternMode::Native, outline_text_in_pdf: true, out_dir: None, ghostscript: None }
    }
}

#[derive(Debug, Clone)]
pub struct ConvertReport {
    pub outputs: Vec<PathBuf>,
    pub warnings: Vec<String>,
    pub paths_written: usize,
}

/// Detect the input type from the file contents, falling back to the extension.
pub fn detect_kind(data: &[u8], path: &Path) -> Option<InputKind> {
    let head = &data[..data.len().min(1024)];
    // DOS EPS binary header (C5 D0 D3 C6).
    if head.starts_with(&[0xC5, 0xD0, 0xD3, 0xC6]) {
        return Some(InputKind::PostScript);
    }
    // gzip: only SVGZ is expected.
    if head.starts_with(&[0x1F, 0x8B]) {
        return Some(InputKind::Svg);
    }
    if find(head, b"%PDF-").is_some() {
        return Some(InputKind::Pdf);
    }
    if head.starts_with(b"%!") {
        return Some(InputKind::PostScript);
    }
    // SVG may start with long comments / DOCTYPEs, so look further.
    let text = String::from_utf8_lossy(&data[..data.len().min(16 * 1024)]).to_ascii_lowercase();
    if text.contains("<svg") || (text.contains("<?xml") && text.contains("svg")) {
        return Some(InputKind::Svg);
    }
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "pdf" => Some(InputKind::Pdf),
        "eps" | "epsf" | "epsi" | "ps" | "ai" => Some(InputKind::PostScript),
        "svg" | "svgz" => Some(InputKind::Svg),
        _ => None,
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn gs_path(opts: &ConvertOptions) -> Option<PathBuf> {
    opts.ghostscript.clone().or_else(ghostscript::find)
}

/// Parse any supported input file into one IR document per page.
pub fn load_documents(input: &Path, opts: &ConvertOptions) -> Result<Vec<Document>> {
    let data = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let kind = detect_kind(&data, input).context("unrecognised file type (expected EPS, PS, PDF, AI or SVG)")?;
    match kind {
        InputKind::Svg => Ok(vec![svg::parse_svg(&data)?]),
        InputKind::PostScript => {
            let Some(gs) = gs_path(opts) else {
                bail!(
                    "Ghostscript is required to read EPS/PostScript files but was not found. \
                     Install it (macOS: `brew install ghostscript`; Windows: ghostscript.com) \
                     or place it next to this application."
                );
            };
            let pdf_bytes = ghostscript::to_pdf(&gs, input, true, true)?;
            parse_all_pages(&pdf_bytes)
        }
        InputKind::Pdf => {
            if opts.outline_text_in_pdf
                && let Some(gs) = gs_path(opts) {
                    let pdf_bytes = ghostscript::to_pdf(&gs, input, true, false)?;
                    return parse_all_pages(&pdf_bytes);
                }
            parse_all_pages(&data)
        }
    }
}

fn parse_all_pages(pdf_bytes: &[u8]) -> Result<Vec<Document>> {
    let n = pdf::page_count(pdf_bytes)?;
    if n == 0 {
        bail!("the document has no pages");
    }
    (0..n).map(|i| pdf::parse_pdf(pdf_bytes, i)).collect()
}

/// Output path for page `index` of `total`: `name.ai`, or `name_p2.ai` etc.
/// If the output would overwrite the input (an .ai input), `_ai8` is appended.
pub fn output_path(input: &Path, out_dir: Option<&Path>, index: usize, total: usize) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let dir = out_dir.map(Path::to_path_buf).unwrap_or_else(|| input.parent().unwrap_or(Path::new(".")).to_path_buf());
    let mut name = if total > 1 { format!("{stem}_p{}", index + 1) } else { stem.to_string() };
    let mut out = dir.join(format!("{name}.ai"));
    if same_file(&out, input) {
        name.push_str("_ai8");
        out = dir.join(format!("{name}.ai"));
    }
    out
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Convert one input file, writing one .ai file per page.
pub fn convert_file(input: &Path, opts: &ConvertOptions) -> Result<ConvertReport> {
    let docs = load_documents(input, opts)?;
    let total = docs.len();
    let mut report = ConvertReport { outputs: Vec::new(), warnings: Vec::new(), paths_written: 0 };
    if let Some(dir) = &opts.out_dir {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    for (i, doc) in docs.iter().enumerate() {
        let out = output_path(input, opts.out_dir.as_deref(), i, total);
        let title = out.file_name().and_then(|s| s.to_str()).unwrap_or("Untitled.ai").to_string();
        let wopts = WriteOptions { title, creator_for: String::new(), color_mode: opts.color_mode };
        let ai = write_ai8(doc, &wopts);
        // AI files are 8-bit; our writer only emits ASCII.
        std::fs::write(&out, ai.as_bytes()).with_context(|| format!("writing {}", out.display()))?;
        report.paths_written += doc.path_count();
        for w in &doc.warnings {
            let w = if total > 1 { format!("page {}: {w}", i + 1) } else { w.clone() };
            if !report.warnings.contains(&w) {
                report.warnings.push(w);
            }
        }
        report.outputs.push(out);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_kinds() {
        let p = Path::new("x.bin");
        assert_eq!(detect_kind(b"%PDF-1.4\n", p), Some(InputKind::Pdf));
        assert_eq!(detect_kind(b"%!PS-Adobe-3.0 EPSF-3.0\n", p), Some(InputKind::PostScript));
        assert_eq!(detect_kind(&[0xC5, 0xD0, 0xD3, 0xC6, 0, 0], p), Some(InputKind::PostScript));
        assert_eq!(detect_kind(b"<?xml version=\"1.0\"?>\n<svg>", p), Some(InputKind::Svg));
        assert_eq!(detect_kind(b"<svg xmlns=\"http://www.w3.org/2000/svg\">", p), Some(InputKind::Svg));
        assert_eq!(detect_kind(b"garbage", Path::new("a.EPS")), Some(InputKind::PostScript));
        assert_eq!(detect_kind(b"garbage", p), None);
        let mut long = b"<?xml version=\"1.0\"?>\n<!--".to_vec();
        long.extend(std::iter::repeat_n(b'x', 3000));
        long.extend(b"-->\n<svg></svg>");
        assert_eq!(detect_kind(&long, p), Some(InputKind::Svg));
        // Illustrator 9+ files are PDF-compatible: PDF header after a short prefix.
        assert_eq!(detect_kind(b"\n%PDF-1.5\n", Path::new("a.ai")), Some(InputKind::Pdf));
    }

    #[test]
    fn output_naming() {
        let input = Path::new("/tmp/dir/logo.eps");
        assert_eq!(output_path(input, None, 0, 1), PathBuf::from("/tmp/dir/logo.ai"));
        assert_eq!(output_path(input, None, 1, 3), PathBuf::from("/tmp/dir/logo_p2.ai"));
        assert_eq!(output_path(input, Some(Path::new("/out")), 0, 1), PathBuf::from("/out/logo.ai"));
        // Same path as input -> suffixed.
        let ai = Path::new("/tmp/dir/logo.ai");
        assert_eq!(output_path(ai, None, 0, 1), PathBuf::from("/tmp/dir/logo_ai8.ai"));
    }
}
