//! Locating and running Ghostscript, used to interpret EPS/PostScript
//! (and optionally to outline text in PDFs) by converting them to PDF.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Find a Ghostscript executable (bundled next to the app, on PATH, or in the
/// standard install locations on Windows/macOS/Linux).
pub fn find() -> Option<PathBuf> {
    todo!("implemented by the app task")
}

/// Run Ghostscript's pdfwrite device on `input` and return the PDF bytes.
/// `outline_text` converts all text to vector outlines (-dNoOutputFonts);
/// `eps_crop` crops the page to the EPS %%BoundingBox.
pub fn to_pdf(_gs: &Path, _input: &Path, _outline_text: bool, _eps_crop: bool) -> Result<Vec<u8>> {
    todo!("implemented by the app task")
}
