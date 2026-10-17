//! Render an SVG to PNG or 24-bit BMP (used to build app icons and the
//! installer artwork). SIZE is the longest side in pixels; the other side
//! follows the SVG's aspect ratio. System fonts are loaded for <text>.
//! Usage: cargo run --example render_svg -- input.svg output.(png|bmp) SIZE
use resvg::{tiny_skia, usvg};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output, size] = args.as_slice() else {
        return Err("usage: render_svg INPUT.svg OUTPUT.(png|bmp) SIZE".into());
    };
    let size: f32 = size.parse()?;
    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_data(&std::fs::read(input)?, &opt)?;
    let (w, h) = (tree.size().width(), tree.size().height());
    let scale = size / w.max(h);
    let (pw, ph) = ((w * scale).round() as u32, (h * scale).round() as u32);
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or("bad size")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    if output.to_ascii_lowercase().ends_with(".bmp") {
        std::fs::write(output, bmp24(&pixmap))?;
    } else {
        pixmap.save_png(output)?;
    }
    Ok(())
}

/// Uncompressed bottom-up 24-bit BMP (what NSIS expects), composited on white.
fn bmp24(p: &tiny_skia::Pixmap) -> Vec<u8> {
    let (w, h) = (p.width() as usize, p.height() as usize);
    let row = (w * 3).div_ceil(4) * 4;
    let mut out = Vec::with_capacity(54 + row * h);
    let file_size = (54 + row * h) as u32;
    out.extend(b"BM");
    out.extend(file_size.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend((w as i32).to_le_bytes());
    out.extend((h as i32).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(24u16.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(((row * h) as u32).to_le_bytes());
    out.extend(2835u32.to_le_bytes());
    out.extend(2835u32.to_le_bytes());
    out.extend([0u8; 8]);
    for y in (0..h).rev() {
        let start = out.len();
        for x in 0..w {
            // Pixmap data is premultiplied RGBA; composite onto white.
            let c = p.pixel(x as u32, y as u32).unwrap();
            let a = c.alpha() as u32;
            let over = |v: u8| (v as u32 + (255 - a)) as u8;
            out.extend([over(c.blue()), over(c.green()), over(c.red())]);
        }
        out.resize(start + row, 0);
    }
    out
}
