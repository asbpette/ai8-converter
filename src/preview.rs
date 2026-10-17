//! Previewing Illustrator 8 output with Ghostscript.
//!
//! AI8 files are PostScript programs that rely on Illustrator's procsets,
//! which our writer (like Illustrator's own "include: none" saves) leaves
//! out, so neither Ghostscript nor macOS Preview can image them directly.
//! [`PROCSET`] is our own prolog defining every AI8 art operator (see
//! `assets/ai8-procset.ps` for the semantics); prepended to the file it lets
//! Ghostscript render the output to PNG or PDF.
//!
//! The module also renders the *input* files (EPS/PS/PDF with Ghostscript,
//! SVG with resvg) so the original and the converted artwork can be shown
//! side by side and compared pixel by pixel ([`compare`]).

use crate::convert::{InputKind, detect_kind};
use crate::ghostscript::{self, TempFile};
use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

/// The AI8 preview prolog (PostScript).
pub const PROCSET: &str = include_str!("../assets/ai8-procset.ps");

/// Time limit for one preview rendering.
pub const RENDER_TIMEOUT: Duration = Duration::from_secs(120);

/// Marker printed by the strict preview program when the art leaves operands
/// on the stack (a sign of an operator the writer emitted wrongly).
const STACK_MARKER: &str = "AI8P-STACK-NOT-EMPTY";

/// Artboard size of an AI file in points: `%AI5_ArtSize: w h`, falling back
/// to the upper-right corner of `%%BoundingBox`, then US Letter.
pub fn art_size(ai: &str) -> (f64, f64) {
    let header = ai.split("%%EndComments").next().unwrap_or(ai);
    let nums = |rest: &str| -> Vec<f64> { rest.split_whitespace().filter_map(|t| t.parse().ok()).collect() };
    for line in header.lines() {
        if let Some(rest) = line.strip_prefix("%AI5_ArtSize:") {
            let v = nums(rest);
            if v.len() >= 2 && v[0] > 0.0 && v[1] > 0.0 {
                return (v[0], v[1]);
            }
        }
    }
    for line in header.lines() {
        if let Some(rest) = line.strip_prefix("%%BoundingBox:") {
            let v = nums(rest);
            if v.len() == 4 && v[2] > 0.0 && v[3] > 0.0 {
                return (v[2], v[3]);
            }
        }
    }
    (612.0, 792.0)
}

/// The complete PostScript program that images `ai`: the procset, the page
/// set-up and the AI file itself. In `strict` mode an undefined operator is
/// an error and operands left on the stack at the end are reported (see
/// [`render_ai8_png_strict`]); otherwise unknown operators are skipped.
pub fn preview_program(ai: &str, strict: bool) -> String {
    let (w, h) = art_size(ai);
    let mut s = String::with_capacity(PROCSET.len() + ai.len() + 512);
    s.push_str(PROCSET);
    s.push_str(&format!(
        "\nAI8P_Dict begin /AI8P_ArtW {} def /AI8P_ArtH {} def\n",
        crate::ai8::num(w),
        crate::ai8::num(h)
    ));
    if !strict {
        s.push_str("ai8p_lenient\n");
    }
    s.push_str("ai8p_newstate begin\n");
    s.push_str(ai);
    if strict {
        s.push_str(&format!("\ncount 0 ne {{ ({STACK_MARKER}) print count == pstack }} if\n"));
    }
    s
}

fn page_args(w: f64, h: f64) -> Vec<OsString> {
    vec![
        format!("-dDEVICEWIDTHPOINTS={}", crate::ai8::num(w)).into(),
        format!("-dDEVICEHEIGHTPOINTS={}", crate::ai8::num(h)).into(),
        "-dFIXEDMEDIA".into(),
    ]
}

fn png_args(dpi: u32, out: &Path) -> Vec<OsString> {
    let mut out_arg = OsString::from("-sOutputFile=");
    out_arg.push(out.as_os_str());
    vec![
        "-q".into(),
        "-dSAFER".into(),
        "-dBATCH".into(),
        "-dNOPAUSE".into(),
        "-sDEVICE=png16m".into(),
        format!("-r{}", dpi.clamp(1, 2400)).into(),
        "-dTextAlphaBits=4".into(),
        "-dGraphicsAlphaBits=4".into(),
        out_arg,
    ]
}

/// Run a preview program through Ghostscript with the given device arguments.
fn run_program(gs: &Path, program: &str, w: f64, h: f64, mut args: Vec<OsString>) -> Result<String> {
    let src = TempFile::new("ps");
    std::fs::write(src.path(), program).context("writing the preview program")?;
    args.extend(page_args(w, h));
    args.push("-f".into());
    args.push(src.path().as_os_str().to_owned());
    ghostscript::run(gs, &args, RENDER_TIMEOUT)
}

fn read_ai(ai_path: &Path) -> Result<String> {
    let bytes = std::fs::read(ai_path).with_context(|| format!("reading {}", ai_path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn render_ai8(gs: &Path, ai_path: &Path, dpi: u32, strict: bool) -> Result<Vec<u8>> {
    let ai = read_ai(ai_path)?;
    let (w, h) = art_size(&ai);
    let out = TempFile::new("png");
    let log = run_program(gs, &preview_program(&ai, strict), w, h, png_args(dpi, out.path()))
        .with_context(|| format!("rendering {}", ai_path.display()))?;
    if strict && log.contains(STACK_MARKER) {
        bail!("operands left on the stack after rendering {}:\n{log}", ai_path.display());
    }
    read_png(out.path())
}

/// Render an AI8 file (first and only page) to a PNG at `dpi`. Unknown
/// operators are skipped.
pub fn render_ai8_png(gs: &Path, ai_path: &Path, dpi: u32) -> Result<Vec<u8>> {
    render_ai8(gs, ai_path, dpi, false)
}

/// Like [`render_ai8_png`], but any PostScript error (such as an undefined
/// operator) or operands left on the stack make it fail. Used by tests.
pub fn render_ai8_png_strict(gs: &Path, ai_path: &Path, dpi: u32) -> Result<Vec<u8>> {
    render_ai8(gs, ai_path, dpi, true)
}

/// Write a PDF rendering of an AI8 file to `out_pdf` (Ghostscript pdfwrite),
/// so it can be opened in any PDF viewer.
pub fn ai8_to_pdf(gs: &Path, ai_path: &Path, out_pdf: &Path) -> Result<()> {
    let ai = read_ai(ai_path)?;
    let (w, h) = art_size(&ai);
    let mut out_arg = OsString::from("-sOutputFile=");
    out_arg.push(out_pdf.as_os_str());
    let args: Vec<OsString> = vec![
        "-q".into(),
        "-dSAFER".into(),
        "-dBATCH".into(),
        "-dNOPAUSE".into(),
        "-sDEVICE=pdfwrite".into(),
        "-dAutoRotatePages=/None".into(),
        "-sColorConversionStrategy=LeaveColorUnchanged".into(),
        out_arg,
    ];
    run_program(gs, &preview_program(&ai, false), w, h, args)
        .with_context(|| format!("converting {} to PDF", ai_path.display()))?;
    let ok = std::fs::metadata(out_pdf).map(|m| m.len() > 0).unwrap_or(false);
    if !ok {
        bail!("Ghostscript wrote no PDF for {}", ai_path.display());
    }
    Ok(())
}

fn read_png(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).context("Ghostscript produced no image")?;
    if !bytes.starts_with(b"\x89PNG") {
        bail!("Ghostscript produced an invalid image");
    }
    Ok(bytes)
}

/// Render page `page` (0-based) of an input file the way it is meant to look:
/// EPS/PS/PDF with Ghostscript (EPS cropped to its bounding box, PDF to its
/// crop box), SVG/SVGZ with resvg. The result is a PNG at `dpi` on white.
/// DXF files have no reference renderer (an error is returned).
pub fn render_input_png(gs: Option<&Path>, input: &Path, page: usize, dpi: u32) -> Result<Vec<u8>> {
    let data = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let kind = detect_kind(&data, input).ok_or_else(|| anyhow!("unrecognised file type"))?;
    match kind {
        InputKind::Svg => return render_svg_png(&data, dpi),
        InputKind::Dxf => bail!("there is no reference renderer for DXF files; only the converted file can be previewed"),
        InputKind::Pdf | InputKind::PostScript => {}
    }
    let gs = gs.ok_or_else(|| anyhow!("Ghostscript is needed to preview {}", input.display()))?;
    let out = TempFile::new("png");
    let mut args = png_args(dpi, out.path());
    let page = (page + 1).to_string();
    args.push(format!("-dFirstPage={page}").into());
    args.push(format!("-dLastPage={page}").into());
    match kind {
        InputKind::Pdf => args.push("-dUseCropBox".into()),
        _ => args.push("-dEPSCrop".into()),
    }
    args.push("-f".into());
    args.push(std::path::absolute(input).unwrap_or_else(|_| input.to_path_buf()).into_os_string());
    ghostscript::run(gs, &args, RENDER_TIMEOUT).with_context(|| format!("rendering {}", input.display()))?;
    read_png(out.path())
}

/// Rasterise SVG data with resvg at `dpi` (SVG user units are 1/96 inch) on
/// a white background.
pub fn render_svg_png(data: &[u8], dpi: u32) -> Result<Vec<u8>> {
    let opt = usvg::Options { fontdb: crate::svg::system_fonts(), ..Default::default() };
    let tree = usvg::Tree::from_data(data, &opt).map_err(|e| anyhow!("failed to parse SVG: {e}"))?;
    let scale = dpi as f32 / 96.0;
    let size = tree.size();
    let w = (size.width() * scale).round().max(1.0) as u32;
    let h = (size.height() * scale).round().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow!("image too large"))?;
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| anyhow!("encoding PNG: {e}"))
}

// ---------------------------------------------------------------------------
// Raster images and comparison

/// An 8-bit RGBA image (straight alpha).
#[derive(Debug, Clone, PartialEq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Raster {
    pub fn new(width: u32, height: u32, fill: [u8; 4]) -> Self {
        Self { width, height, rgba: fill.repeat((width * height) as usize) }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, p: [u8; 4]) {
        let i = ((y * self.width + x) * 4) as usize;
        self.rgba[i..i + 4].copy_from_slice(&p);
    }

    /// The top-left `w` × `h` part of the image.
    pub fn crop(&self, w: u32, h: u32) -> Raster {
        let (w, h) = (w.min(self.width), h.min(self.height));
        let mut out = Raster { width: w, height: h, rgba: Vec::with_capacity((w * h * 4) as usize) };
        for y in 0..h {
            let start = (y * self.width * 4) as usize;
            out.rgba.extend_from_slice(&self.rgba[start..start + (w * 4) as usize]);
        }
        out
    }
}

/// Shrink an image by an integer `factor` with a box filter (averaging
/// `factor`² pixels, composited on white). Rendering at a multiple of the
/// target resolution and shrinking gives every edge the same anti-aliasing,
/// including edges Ghostscript does not anti-alias itself (clip paths,
/// pattern fills).
pub fn downsample(r: &Raster, factor: u32) -> Raster {
    let f = factor.max(1);
    let (w, h) = (r.width / f, r.height / f);
    let mut out = Raster::new(w, h, [255, 255, 255, 255]);
    let n = f * f;
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 3];
            for sy in 0..f {
                for sx in 0..f {
                    let p = flatten_white(r.pixel(x * f + sx, y * f + sy));
                    for i in 0..3 {
                        acc[i] += p[i] as u32;
                    }
                }
            }
            out.set_pixel(x, y, [(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8, 255]);
        }
    }
    out
}

/// Decode a PNG (any 8-bit colour type except indexed) into straight RGBA.
pub fn decode_png(bytes: &[u8]) -> Result<Raster> {
    let pm = resvg::tiny_skia::Pixmap::decode_png(bytes).map_err(|e| anyhow!("decoding PNG: {e}"))?;
    let rgba = pm
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Ok(Raster { width: pm.width(), height: pm.height(), rgba })
}

/// Encode a raster as PNG.
pub fn encode_png(r: &Raster) -> Result<Vec<u8>> {
    let size = resvg::tiny_skia::IntSize::from_wh(r.width, r.height).ok_or_else(|| anyhow!("empty image"))?;
    let mut data = Vec::with_capacity(r.rgba.len());
    for px in r.rgba.as_chunks::<4>().0 {
        // Premultiply for tiny-skia.
        let a = px[3] as u32;
        data.extend(px[..3].iter().map(|&c| ((c as u32 * a + 127) / 255) as u8));
        data.push(px[3]);
    }
    let pm = resvg::tiny_skia::Pixmap::from_vec(data, size).ok_or_else(|| anyhow!("invalid image"))?;
    pm.encode_png().map_err(|e| anyhow!("encoding PNG: {e}"))
}

/// Result of [`compare`].
#[derive(Debug, Clone)]
pub struct DiffStats {
    /// Compared area (the common top-left part of both images).
    pub width: u32,
    pub height: u32,
    /// Pixels whose colour has no close match within the tolerance radius in
    /// the other image (checked in both directions).
    pub differing: usize,
    /// Pixels that are not white in at least one image.
    pub ink: usize,
    /// Largest per-channel difference between co-located pixels.
    pub max_channel_diff: u8,
    /// Visualisation: differing pixels red, the rest a faded copy of `a`.
    pub diff_image: Raster,
}

impl DiffStats {
    /// Differing pixels as a percentage of all ink pixels (0 when blank).
    pub fn percent_of_ink(&self) -> f64 {
        if self.ink == 0 { 0.0 } else { 100.0 * self.differing as f64 / self.ink as f64 }
    }

    /// Differing pixels as a percentage of the compared area.
    pub fn percent_of_area(&self) -> f64 {
        let n = self.width as usize * self.height as usize;
        if n == 0 { 0.0 } else { 100.0 * self.differing as f64 / n as f64 }
    }
}

fn flatten_white(p: [u8; 4]) -> [i32; 3] {
    let a = p[3] as i32;
    let f = |c: u8| (c as i32 * a + 255 * (255 - a) + 127) / 255;
    [f(p[0]), f(p[1]), f(p[2])]
}

fn close(a: [i32; 3], b: [i32; 3], threshold: i32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= threshold)
}

/// Compare two renderings of the same page. A pixel counts as differing when
/// its colour is not within the per-channel range (min..max, widened by
/// `threshold`) of the pixels within `radius` of the same position in the
/// other image. This is checked both ways, so missing and extra ink are both
/// found, while anti-aliased edges (a mix of the neighbouring colours) and
/// sub-pixel placement are tolerated. Images are compared over their common
/// top-left area; transparent pixels are composited on white.
pub fn compare(a: &Raster, b: &Raster, threshold: u8, radius: u32) -> DiffStats {
    let (w, h) = (a.width.min(b.width), a.height.min(b.height));
    let flat = |r: &Raster| -> Vec<[i32; 3]> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| flatten_white(r.pixel(x, y))).collect()
    };
    let (fa, fb) = (flat(a), flat(b));
    let t = threshold as i32;
    let r = radius as i32;
    let idx = |x: i32, y: i32| (y * w as i32 + x) as usize;
    let matched = |src: &[[i32; 3]], other: &[[i32; 3]], x: i32, y: i32| {
        let c = src[idx(x, y)];
        let (mut lo, mut hi) = ([255i32; 3], [0i32; 3]);
        for ny in (y - r).max(0)..=(y + r).min(h as i32 - 1) {
            for nx in (x - r).max(0)..=(x + r).min(w as i32 - 1) {
                let o = other[idx(nx, ny)];
                for i in 0..3 {
                    lo[i] = lo[i].min(o[i]);
                    hi[i] = hi[i].max(o[i]);
                }
            }
        }
        (0..3).all(|i| c[i] >= lo[i] - t && c[i] <= hi[i] + t)
    };
    let mut diff_image = Raster::new(w, h, [255, 255, 255, 255]);
    let (mut differing, mut ink, mut max_diff) = (0usize, 0usize, 0i32);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let (pa, pb) = (fa[idx(x, y)], fb[idx(x, y)]);
            let white = |p: [i32; 3]| p.iter().all(|&c| c >= 250);
            if !white(pa) || !white(pb) {
                ink += 1;
            }
            max_diff = max_diff.max((0..3).map(|i| (pa[i] - pb[i]).abs()).max().unwrap_or(0));
            let bad = !(close(pa, pb, t) || (matched(&fa, &fb, x, y) && matched(&fb, &fa, x, y)));
            if bad {
                differing += 1;
                diff_image.set_pixel(x as u32, y as u32, [255, 0, 0, 255]);
            } else {
                let fade = |c: i32| (200 + c * 55 / 255) as u8;
                diff_image.set_pixel(x as u32, y as u32, [fade(pa[0]), fade(pa[1]), fade(pa[2]), 255]);
            }
        }
    }
    DiffStats { width: w, height: h, differing, ink, max_channel_diff: max_diff as u8, diff_image }
}

/// Place two images side by side (left, right) on white with a gap.
pub fn side_by_side(left: &Raster, right: &Raster, gap: u32) -> Raster {
    let w = left.width + gap + right.width;
    let h = left.height.max(right.height);
    let mut out = Raster::new(w, h, [255, 255, 255, 255]);
    for (img, ox) in [(left, 0), (right, left.width + gap)] {
        for y in 0..img.height {
            for x in 0..img.width {
                let p = flatten_white(img.pixel(x, y));
                out.set_pixel(ox + x, y, [p[0] as u8, p[1] as u8, p[2] as u8, 255]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_size_from_header() {
        assert_eq!(art_size("%!PS\n%AI5_ArtSize: 200 100\n%%EndComments\n"), (200.0, 100.0));
        assert_eq!(art_size("%!PS\n%%BoundingBox: 0 0 30 40\n%%EndComments\n"), (30.0, 40.0));
        assert_eq!(art_size("garbage"), (612.0, 792.0));
    }

    #[test]
    fn program_wraps_art() {
        let ai = "%!PS-Adobe-3.0\n%AI5_ArtSize: 10 20\n%%EndComments\n0 g\n";
        let strict = preview_program(ai, true);
        assert!(strict.starts_with("%!PS-Adobe-3.0"));
        assert!(strict.contains("/AI8P_ArtW 10 def /AI8P_ArtH 20 def"));
        assert!(!strict.contains("ai8p_lenient\n"));
        assert!(strict.contains(STACK_MARKER));
        let lenient = preview_program(ai, false);
        assert!(lenient.contains("ai8p_lenient\n") && !lenient.contains(STACK_MARKER));
        assert!(lenient.trim_end().ends_with("0 g"));
    }

    #[test]
    fn procset_defines_every_writer_operator() {
        for op in [
            "Lb", "Ln", "LB", "A", "Np", "u", "U", "*u", "*U", "q", "Q", "m", "L", "l", "C", "c", "v", "V", "y", "Y", "f",
            "F", "s", "S", "b", "B", "n", "N", "h", "H", "W", "g", "G", "k", "K", "Xa", "XA", "x", "X", "O", "R", "XR",
            "D", "J", "j", "w", "M", "d", "i", "p", "P", "E", "@", "&", "Bd", "BD", "Bn", "Bb", "BB", "Bg", "Bh", "Bm", "Ar",
        ] {
            assert!(PROCSET.contains(&format!("\n/{op} ")), "operator {op} not defined");
        }
    }

    #[test]
    fn dxf_input_has_no_reference_rendering() {
        let dir = std::env::temp_dir().join(format!("ai8-preview-dxf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.dxf");
        std::fs::write(&f, "0\nSECTION\n2\nENTITIES\n0\nENDSEC\n0\nEOF\n").unwrap();
        let e = render_input_png(None, &f, 0, 72).unwrap_err();
        assert!(format!("{e}").contains("DXF"), "{e}");
    }

    #[test]
    fn png_round_trip_and_crop() {
        let mut r = Raster::new(3, 2, [255, 255, 255, 255]);
        r.set_pixel(1, 1, [10, 20, 30, 255]);
        let back = decode_png(&encode_png(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        let c = r.crop(2, 2);
        assert_eq!((c.width, c.height), (2, 2));
        assert_eq!(c.pixel(1, 1), [10, 20, 30, 255]);
    }

    #[test]
    fn compare_tolerates_one_pixel_shift_but_not_missing_ink() {
        let mut a = Raster::new(20, 20, [255, 255, 255, 255]);
        let mut b = a.clone();
        let mut c = a.clone();
        for y in 5..15 {
            for x in 5..15 {
                a.set_pixel(x, y, [0, 0, 0, 255]);
                b.set_pixel(x + 1, y, [0, 0, 0, 255]);
                if x < 10 {
                    c.set_pixel(x, y, [0, 0, 0, 255]);
                }
            }
        }
        let same = compare(&a, &b, 16, 1);
        assert_eq!(same.differing, 0);
        assert!(same.ink > 0);
        let strict = compare(&a, &b, 16, 0);
        assert_eq!(strict.differing, 20);
        let half = compare(&a, &c, 16, 1);
        assert!(half.differing >= 40, "{}", half.differing);
        assert!(half.percent_of_ink() > 30.0);
    }

    #[test]
    fn downsample_averages_blocks() {
        let mut r = Raster::new(4, 2, [255, 255, 255, 255]);
        r.set_pixel(0, 0, [0, 0, 0, 255]);
        r.set_pixel(1, 1, [0, 0, 0, 255]);
        let d = downsample(&r, 2);
        assert_eq!((d.width, d.height), (2, 1));
        assert_eq!(d.pixel(0, 0), [127, 127, 127, 255]);
        assert_eq!(d.pixel(1, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn side_by_side_layout() {
        let a = Raster::new(2, 3, [0, 0, 0, 255]);
        let b = Raster::new(4, 1, [0, 0, 0, 0]);
        let s = side_by_side(&a, &b, 1);
        assert_eq!((s.width, s.height), (7, 3));
        assert_eq!(s.pixel(0, 0), [0, 0, 0, 255]);
        assert_eq!(s.pixel(2, 0), [255, 255, 255, 255]);
        assert_eq!(s.pixel(3, 0), [255, 255, 255, 255]);
    }
}
