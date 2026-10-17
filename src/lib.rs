//! Convert EPS / PS / PDF / AI / SVG vector files to Adobe Illustrator 8 (.ai).

pub mod ai8;
pub mod convert;
pub mod ghostscript;
pub mod ir;
pub mod patterns;
pub mod pdf;
pub mod svg;

pub use ai8::{ColorMode, WriteOptions, write_ai8};
pub use convert::{ConvertOptions, PatternMode, ConvertReport, InputKind, convert_file, detect_kind};
