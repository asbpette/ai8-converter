//! PDF (and PDF-compatible .ai) input: interprets page content streams into the IR.

use crate::ir::Document;
use anyhow::Result;

/// Number of pages in the PDF.
pub fn page_count(_data: &[u8]) -> Result<usize> {
    todo!("implemented by the PDF parser task")
}

/// Parse page `page_index` (0-based) into the IR, in PDF user space
/// (points, y up). `Document.page` is the page's CropBox (or MediaBox).
pub fn parse_pdf(_data: &[u8], _page_index: usize) -> Result<Document> {
    todo!("implemented by the PDF parser task")
}
