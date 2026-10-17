//! Render an SVG to PNG at a given square size (used to build app icons).
//! Usage: cargo run --example render_svg -- input.svg output.png SIZE
use resvg::{tiny_skia, usvg};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output, size] = args.as_slice() else {
        return Err("usage: render_svg INPUT.svg OUTPUT.png SIZE".into());
    };
    let size: u32 = size.parse()?;
    let tree = usvg::Tree::from_data(&std::fs::read(input)?, &usvg::Options::default())?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("bad size")?;
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.save_png(output)?;
    Ok(())
}
