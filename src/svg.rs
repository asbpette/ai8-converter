//! SVG input via usvg.

use crate::ir::Document;
use anyhow::Result;

/// Parse an SVG (or gzip-compressed SVGZ) document into the IR.
/// SVG user units (CSS px, 96/in) are converted to points and the y axis flipped.
pub fn parse_svg(_data: &[u8]) -> Result<Document> {
    todo!("implemented by the SVG parser task")
}
