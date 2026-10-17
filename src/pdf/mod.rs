//! PDF (and PDF-compatible .ai) input: interprets page content streams into the IR.
//!
//! Layout:
//! - [`lexer`]: tolerant content-stream tokenizer (inline images skipped).
//! - [`function`]: PDF function types 0, 2, 3 and 4 (tint transforms).
//! - [`color`]: colour spaces -> IR colours (spot colours preserved).
//! - [`interp`]: the content-stream interpreter (paths, gstate, clipping, forms).

mod color;
mod function;
mod interp;
mod lexer;

use crate::ir::{Document, Matrix, Rect};
use anyhow::{Context, Result, anyhow, bail};
use lopdf::{Dictionary, Document as PdfDoc, Object};

/// Maximum depth followed when looking up inherited page attributes.
const MAX_PARENT_DEPTH: usize = 64;

/// Follow references; on a dangling reference return `Null`.
pub(crate) fn deref<'a>(doc: &'a PdfDoc, obj: &'a Object) -> &'a Object {
    static NULL: Object = Object::Null;
    doc.dereference(obj).map(|(_, o)| o).unwrap_or(&NULL)
}

/// Numeric value of a (direct) object.
pub(crate) fn obj_num(obj: &Object) -> Option<f64> {
    match obj {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) if r.is_finite() => Some(*r as f64),
        _ => None,
    }
}

/// Array of numbers (elements may be references).
pub(crate) fn obj_nums(doc: &PdfDoc, obj: &Object) -> Option<Vec<f64>> {
    match deref(doc, obj) {
        Object::Array(a) => a.iter().map(|o| obj_num(deref(doc, o))).collect(),
        _ => None,
    }
}

/// The dictionary of a dictionary or stream object (after dereferencing).
pub(crate) fn dict_of<'a>(doc: &'a PdfDoc, obj: &'a Object) -> Option<&'a Dictionary> {
    match deref(doc, obj) {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

pub(crate) fn name_str(n: &[u8]) -> String {
    String::from_utf8_lossy(n).into_owned()
}

fn load(data: &[u8]) -> Result<PdfDoc> {
    // lopdf is not guaranteed panic-free on hostile input; never let a
    // malformed file take the application down.
    let res = std::panic::catch_unwind(|| PdfDoc::load_mem(data));
    let mut doc = match res {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => bail!("could not read the PDF: {e}"),
        Err(_) => bail!("could not read the PDF: the file is malformed"),
    };
    if doc.is_encrypted() {
        // Try the empty user password (common for "protected" PDFs).
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| doc.decrypt(""))).is_ok_and(|r| r.is_ok());
        if !ok {
            bail!("the PDF is encrypted and cannot be opened without a password");
        }
    }
    Ok(doc)
}

/// Number of pages in the PDF.
pub fn page_count(data: &[u8]) -> Result<usize> {
    let doc = load(data)?;
    Ok(doc.get_pages().len())
}

/// Look up a page attribute, following the /Parent chain for inheritable ones.
fn inherited<'a>(doc: &'a PdfDoc, page: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    let mut node = page;
    for _ in 0..MAX_PARENT_DEPTH {
        if let Ok(v) = node.get(key) {
            return Some(deref(doc, v));
        }
        node = node.get(b"Parent").ok().and_then(|p| dict_of(doc, p))?;
    }
    None
}

fn rect_of(doc: &PdfDoc, obj: &Object) -> Option<Rect> {
    let v = obj_nums(doc, obj)?;
    if v.len() < 4 {
        return None;
    }
    let r = Rect { llx: v[0].min(v[2]), lly: v[1].min(v[3]), urx: v[0].max(v[2]), ury: v[1].max(v[3]) };
    if r.width() <= 0.0 || r.height() <= 0.0 { None } else { Some(r) }
}

/// Base transform and resulting page rectangle for a /Rotate value, so the
/// page appears upright (as a viewer would display it).
fn rotation(page: Rect, rotate: i64) -> (Matrix, Rect) {
    let (x0, y0, x1, y1) = (page.llx, page.lly, page.urx, page.ury);
    let (w, h) = (page.width(), page.height());
    match rotate.rem_euclid(360) {
        // Clockwise by 90: (x, y) -> (y - y0, x1 - x).
        90 => (Matrix::new(0.0, -1.0, 1.0, 0.0, -y0, x1), Rect { llx: 0.0, lly: 0.0, urx: h, ury: w }),
        180 => (Matrix::new(-1.0, 0.0, 0.0, -1.0, x1, y1), Rect { llx: 0.0, lly: 0.0, urx: w, ury: h }),
        // Counter-clockwise by 90: (x, y) -> (y1 - y, x - x0).
        270 => (Matrix::new(0.0, 1.0, -1.0, 0.0, y1, -x0), Rect { llx: 0.0, lly: 0.0, urx: h, ury: w }),
        _ => (Matrix::IDENTITY, page),
    }
}

/// Concatenated, decoded content of a page.
fn page_content(doc: &PdfDoc, page: &Dictionary, warnings: &mut Vec<String>) -> Vec<u8> {
    let Ok(contents) = page.get(b"Contents") else { return Vec::new() };
    let streams: Vec<&Object> = match deref(doc, contents) {
        Object::Array(a) => a.iter().map(|o| deref(doc, o)).collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for s in streams {
        if let Object::Stream(st) = s {
            let remaining = interp::MAX_STREAM.saturating_sub(out.len());
            match st.get_plain_content_with_limit(remaining) {
                Ok(data) => out.extend_from_slice(&data),
                Err(e) => warnings.push(format!("a page content stream could not be decoded and was skipped: {e}")),
            }
            out.push(b'\n');
        }
    }
    out
}

/// Parse page `page_index` (0-based) into the IR, in PDF user space
/// (points, y up). `Document.page` is the page's CropBox (or MediaBox).
/// Pages with /Rotate are rotated upright, with the page rect at the origin.
pub fn parse_pdf(data: &[u8], page_index: usize) -> Result<Document> {
    let doc = load(data)?;
    let pages = doc.get_pages();
    let page_id = *pages
        .values()
        .nth(page_index)
        .ok_or_else(|| anyhow!("page {} does not exist (the document has {} pages)", page_index + 1, pages.len()))?;
    let page = doc.get_dictionary(page_id).context("reading the page dictionary")?;

    let mut warnings = Vec::new();
    let media = inherited(&doc, page, b"MediaBox").and_then(|o| rect_of(&doc, o)).unwrap_or_else(|| {
        warnings.push("the page has no valid MediaBox; assuming US Letter".to_string());
        Rect { llx: 0.0, lly: 0.0, urx: 612.0, ury: 792.0 }
    });
    let crop = inherited(&doc, page, b"CropBox")
        .and_then(|o| rect_of(&doc, o))
        .map(|c| Rect {
            llx: c.llx.max(media.llx),
            lly: c.lly.max(media.lly),
            urx: c.urx.min(media.urx),
            ury: c.ury.min(media.ury),
        })
        .filter(|r| r.width() > 0.0 && r.height() > 0.0)
        .unwrap_or(media);
    let rotate = inherited(&doc, page, b"Rotate").and_then(obj_num).unwrap_or(0.0);
    let rotate = ((rotate / 90.0).round() as i64) * 90;
    let (base, page_rect) = rotation(crop, rotate);
    let resources = inherited(&doc, page, b"Resources").and_then(|o| dict_of(&doc, o));

    let content = page_content(&doc, page, &mut warnings);
    let mut it = interp::Interp::new(&doc, base);
    for w in warnings {
        it.warn(w);
    }
    it.run(&content, resources);
    let (elements, warnings) = it.finish();
    let mut out = Document::new(page_rect);
    out.elements = elements;
    out.warnings = warnings;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Color, Element, LineCap, LineJoin, PathItem, Point, Segment};
    use lopdf::{Stream, dictionary};

    /// Build a one-page PDF from a content stream and resources.
    pub(crate) fn one_page(content: &str, resources: Dictionary) -> Vec<u8> {
        let mut doc = PdfDoc::with_version("1.5");
        let pages_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
                "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    fn parse(content: &str) -> Document {
        parse_pdf(&one_page(content, Dictionary::new()), 0).unwrap()
    }

    fn paths(els: &[Element]) -> Vec<&PathItem> {
        let mut v = Vec::new();
        for e in els {
            match e {
                Element::Path(p) => v.push(p),
                Element::Group(c) => v.extend(paths(c)),
                Element::ClipGroup { children, .. } => v.extend(paths(children)),
            }
        }
        v
    }

    #[test]
    fn inherited_mediabox_and_simple_fill() {
        let d = parse("0 0 1 rg 10 10 m 50 10 l 50 50 l h f");
        assert_eq!(d.page, Rect { llx: 0.0, lly: 0.0, urx: 200.0, ury: 100.0 });
        let p = paths(&d.elements);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].fill, Some(Color::Rgb(0.0, 0.0, 1.0)));
        assert!(p[0].subpaths[0].closed);
        assert_eq!(p[0].subpaths[0].segments.len(), 2);
    }

    #[test]
    fn curves_v_y() {
        let d = parse("0 0 m 1 1 2 2 3 3 c 4 4 5 5 v 6 6 7 7 y S");
        let sp = &paths(&d.elements)[0].subpaths[0];
        let p = |x: f64| Point::new(x, x);
        assert_eq!(sp.segments[0], Segment::Curve(p(1.0), p(2.0), p(3.0)));
        assert_eq!(sp.segments[1], Segment::Curve(p(3.0), p(4.0), p(5.0)));
        assert_eq!(sp.segments[2], Segment::Curve(p(6.0), p(7.0), p(7.0)));
    }

    #[test]
    fn line_after_close_starts_new_subpath() {
        let d = parse("10 10 m 20 10 l 20 20 l h 30 30 l S");
        let p = paths(&d.elements)[0];
        assert_eq!(p.subpaths.len(), 2);
        assert_eq!(p.subpaths[1].start, Point::new(10.0, 10.0));
        assert!(!p.subpaths[1].closed);
    }

    #[test]
    fn stroke_style_scaled() {
        let d = parse("2 0 0 2 0 0 cm 3 w 1 J 2 j 4 M [2 1] 1 d 0 0 m 10 0 l S");
        let s = paths(&d.elements)[0].stroke.clone().unwrap();
        assert_eq!(s.width, 6.0);
        assert_eq!(s.cap, LineCap::Round);
        assert_eq!(s.join, LineJoin::Bevel);
        assert_eq!(s.miter_limit, 4.0);
        assert_eq!(s.dash, vec![4.0, 2.0]);
        assert_eq!(s.dash_phase, 2.0);
    }

    #[test]
    fn clip_nests_until_q() {
        let d = parse("q 0 0 50 50 re W n 0 0 100 100 re f q 10 10 m 40 10 l 40 40 l W* n 0 0 100 100 re f Q 0 0 100 100 re f Q 0 0 10 10 re f");
        assert_eq!(d.elements.len(), 2);
        let Element::ClipGroup { children, even_odd, .. } = &d.elements[0] else { panic!("{:?}", d.elements) };
        assert!(!even_odd);
        assert_eq!(children.len(), 3);
        let Element::ClipGroup { even_odd, children: inner, .. } = &children[1] else { panic!() };
        assert!(even_odd);
        assert_eq!(inner.len(), 1);
        assert!(matches!(d.elements[1], Element::Path(_)));
    }

    #[test]
    fn redundant_rect_clip_dropped() {
        let d = parse("0 0 200 100 re W n 10 10 20 20 re f");
        assert!(matches!(d.elements[0], Element::Path(_)));
        // Unused clips vanish.
        let d = parse("q 0 0 5 5 re W n Q");
        assert!(d.elements.is_empty());
    }

    #[test]
    fn rotate_90() {
        let mut doc_bytes = one_page("0 0 10 10 re f", Dictionary::new());
        // Patch: rebuild with Rotate on the page.
        let mut doc = PdfDoc::load_mem(&doc_bytes).unwrap();
        let pid = *doc.get_pages().values().next().unwrap();
        doc.get_dictionary_mut(pid).unwrap().set("Rotate", 90);
        doc_bytes.clear();
        doc.save_to(&mut doc_bytes).unwrap();
        let d = parse_pdf(&doc_bytes, 0).unwrap();
        assert_eq!(d.page, Rect { llx: 0.0, lly: 0.0, urx: 100.0, ury: 200.0 });
        let b = d.content_bounds().unwrap();
        // Bottom-left square ends up at the top-left.
        assert_eq!((b.llx, b.lly, b.urx, b.ury), (0.0, 190.0, 10.0, 200.0));
    }

    #[test]
    fn rotation_matrices() {
        let r = Rect { llx: 10.0, lly: 20.0, urx: 110.0, ury: 70.0 };
        for rot in [0, 90, 180, 270, -90, 450] {
            let (m, pr) = rotation(r, rot);
            for (x, y) in [(10.0, 20.0), (110.0, 70.0), (10.0, 70.0)] {
                let p = m.apply(Point::new(x, y));
                assert!(p.x >= pr.llx - 1e-9 && p.x <= pr.urx + 1e-9 && p.y >= pr.lly - 1e-9 && p.y <= pr.ury + 1e-9, "rot {rot}");
            }
        }
        let (m, _) = rotation(r, 270);
        assert_eq!(m.apply(Point::new(10.0, 20.0)), Point::new(50.0, 0.0));
    }

    #[test]
    fn page_out_of_range_and_garbage() {
        let pdf = one_page("", Dictionary::new());
        assert_eq!(page_count(&pdf).unwrap(), 1);
        assert!(parse_pdf(&pdf, 1).is_err());
        assert!(parse_pdf(b"not a pdf", 0).is_err());
        assert!(page_count(b"%PDF-1.4\ngarbage").is_err());
    }

    #[test]
    fn unbalanced_q_and_junk() {
        let d = parse("Q Q q q 1 0 0 1 5 5 cm ] } >> (str) BT /F1 12 Tf (x) Tj ET 0 0 m 1 1 l S foo bar 1 2");
        assert_eq!(d.path_count(), 1);
        assert!(d.warnings.iter().any(|w| w.contains("text")));
    }
}
