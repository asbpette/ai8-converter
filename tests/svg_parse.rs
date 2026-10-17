//! Integration tests for the SVG parser (src/svg.rs).

mod common;

use ai8_converter::ai8::{WriteOptions, write_ai8};
use ai8_converter::ir::{Color, Document, Element, LineCap, LineJoin, PathItem, Point, Segment, Subpath};
use ai8_converter::svg::parse_svg;

const NS: &str = r##"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""##;

fn svg(attrs: &str, body: &str) -> Vec<u8> {
    format!(r##"<svg {NS} {attrs}>{body}</svg>"##).into_bytes()
}

fn parse(attrs: &str, body: &str) -> Document {
    parse_svg(&svg(attrs, body)).expect("SVG should parse")
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

fn assert_pt(p: Point, x: f64, y: f64) {
    assert!(close(p.x, x) && close(p.y, y), "expected ({x}, {y}), got ({}, {})", p.x, p.y);
}

/// All paths in document order (flattening groups and clip groups).
fn paths(doc: &Document) -> Vec<&PathItem> {
    fn walk<'a>(els: &'a [Element], out: &mut Vec<&'a PathItem>) {
        for e in els {
            match e {
                Element::Path(p) => out.push(p),
                Element::Group(c) => walk(c, out),
                Element::ClipGroup { children, .. } => walk(children, out),
            }
        }
    }
    let mut v = Vec::new();
    walk(&doc.elements, &mut v);
    v
}

fn only_path(doc: &Document) -> &PathItem {
    let p = paths(doc);
    assert_eq!(p.len(), 1, "expected exactly one path, got {:#?}", doc.elements);
    p[0]
}

/// End points (move-to + segment end points) of a subpath.
fn anchors(sp: &Subpath) -> Vec<Point> {
    std::iter::once(sp.start)
        .chain(sp.segments.iter().map(|s| match s {
            Segment::Line(p) => *p,
            Segment::Curve(_, _, p) => *p,
        }))
        .collect()
}

fn has_warning(doc: &Document, needle: &str) -> bool {
    doc.warnings.iter().any(|w| w.to_lowercase().contains(needle))
}

fn rgb(doc_color: &Option<Color>) -> (f64, f64, f64) {
    match doc_color {
        Some(Color::Rgb(r, g, b)) => (*r, *g, *b),
        other => panic!("expected RGB colour, got {other:?}"),
    }
}

// ---------------------------------------------------------------- geometry

#[test]
fn rect_px_to_pt_and_y_flip() {
    let doc = parse(r##"width="200" height="100""##, r##"<rect x="10" y="20" width="40" height="30" fill="red"/>"##);
    assert!(close(doc.page.llx, 0.0) && close(doc.page.lly, 0.0));
    assert!(close(doc.page.urx, 150.0) && close(doc.page.ury, 75.0), "{:?}", doc.page);
    let p = only_path(&doc);
    assert_eq!(p.subpaths.len(), 1);
    let sp = &p.subpaths[0];
    assert!(sp.closed);
    assert!(sp.segments.iter().all(|s| matches!(s, Segment::Line(_))));
    let pts = anchors(sp);
    assert_pt(pts[0], 7.5, 60.0);
    assert_pt(pts[1], 37.5, 60.0);
    assert_pt(pts[2], 37.5, 37.5);
    assert_pt(pts[3], 7.5, 37.5);
    assert_eq!(p.fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    assert!(p.stroke.is_none());
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

#[test]
fn physical_units_size() {
    let doc = parse(r##"width="1in" height="2in" viewBox="0 0 10 20""##, r##"<rect width="10" height="20"/>"##);
    assert!(close(doc.page.urx, 72.0) && close(doc.page.ury, 144.0), "{:?}", doc.page);
    let b = doc.content_bounds().unwrap();
    assert!(close(b.llx, 0.0) && close(b.lly, 0.0) && close(b.urx, 72.0) && close(b.ury, 144.0), "{b:?}");
}

#[test]
fn circle_uses_curves() {
    let doc = parse(r##"width="100" height="100""##, r##"<circle cx="50" cy="50" r="40" fill="blue"/>"##);
    let p = only_path(&doc);
    let sp = &p.subpaths[0];
    assert!(sp.closed);
    assert!(sp.segments.iter().filter(|s| matches!(s, Segment::Curve(..))).count() >= 4);
    let b = doc.content_bounds().unwrap();
    // 10..90 px => 7.5..67.5 pt both axes (control points of a circle stay on the bbox).
    assert!(close(b.llx, 7.5) && close(b.urx, 67.5) && close(b.lly, 7.5) && close(b.ury, 67.5), "{b:?}");
}

#[test]
fn quadratic_elevated_to_exact_cubic() {
    // Page height 100 px = 75 pt; y' = 75 - 0.75 y.
    let doc = parse(r##"width="100" height="100""##, r##"<path d="M0 0 Q30 60 90 0" fill="none" stroke="black"/>"##);
    let p = only_path(&doc);
    let sp = &p.subpaths[0];
    assert_pt(sp.start, 0.0, 75.0);
    assert_eq!(sp.segments.len(), 1);
    match &sp.segments[0] {
        Segment::Curve(c1, c2, end) => {
            // c1 = p0 + 2/3 (q - p0) = (20, 40) px; c2 = p + 2/3 (q - p) = (50, 40) px.
            assert_pt(*c1, 15.0, 45.0);
            assert_pt(*c2, 37.5, 45.0);
            assert_pt(*end, 67.5, 75.0);
        }
        s => panic!("expected cubic, got {s:?}"),
    }
}

#[test]
fn smooth_quadratic_and_cubic() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<path d="M0 0 Q10 10 20 0 T40 0 C50 10 60 10 70 0" fill="none" stroke="black"/>"##,
    );
    let sp = &only_path(&doc).subpaths[0];
    assert_eq!(sp.segments.len(), 3);
    // T reflects the control point to (30,-10): c1 = (20,0)+2/3((30,-10)-(20,0)) = (26.667,-6.667)
    match &sp.segments[1] {
        Segment::Curve(c1, _, end) => {
            assert_pt(*c1, 26.6667 * 0.75, 75.0 + 6.6667 * 0.75);
            assert_pt(*end, 30.0, 75.0);
        }
        s => panic!("{s:?}"),
    }
    match &sp.segments[2] {
        Segment::Curve(c1, c2, end) => {
            assert_pt(*c1, 37.5, 67.5);
            assert_pt(*c2, 45.0, 67.5);
            assert_pt(*end, 52.5, 75.0);
        }
        s => panic!("{s:?}"),
    }
}

#[test]
fn nested_transforms() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<g transform="translate(10,10)"><g transform="scale(2)"><rect width="5" height="5"/></g></g>
           <g transform="translate(50 50)"><path transform="rotate(90)" d="M10 0 L20 0" stroke="black"/></g>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 2);
    let b = anchors(&ps[0].subpaths[0]);
    assert_pt(b[0], 7.5, 67.5);
    assert_pt(b[2], 15.0, 60.0);
    let l = anchors(&ps[1].subpaths[0]);
    // rotate(90): (10,0) -> (0,10); + (50,50) => (50,60) px
    assert_pt(l[0], 37.5, 30.0);
    assert_pt(l[1], 37.5, 22.5);
}

#[test]
fn viewbox_scaling() {
    let doc = parse(r##"width="100" height="50" viewBox="0 0 10 5""##, r##"<rect x="1" y="1" width="2" height="2"/>"##);
    assert!(close(doc.page.urx, 75.0) && close(doc.page.ury, 37.5));
    let pts = anchors(&only_path(&doc).subpaths[0]);
    // viewBox scale 10: (1,1) -> (10,10) px -> (7.5, 37.5-7.5)
    assert_pt(pts[0], 7.5, 30.0);
    assert_pt(pts[2], 22.5, 15.0);
}

#[test]
fn viewbox_only_and_offset_origin() {
    let doc = parse(r##"viewBox="100 100 40 20""##, r##"<rect x="100" y="100" width="40" height="20"/>"##);
    assert!(close(doc.page.urx, 30.0) && close(doc.page.ury, 15.0));
    let b = doc.content_bounds().unwrap();
    assert!(close(b.llx, 0.0) && close(b.lly, 0.0) && close(b.urx, 30.0) && close(b.ury, 15.0), "{b:?}");
}

// ---------------------------------------------------------------- paint

#[test]
fn fill_rules() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<path d="M0 0H50V50H0Z M10 10H40V40H10Z" fill-rule="evenodd"/>
           <path d="M0 0H50V50H0Z M10 10H40V40H10Z"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 2);
    assert!(ps[0].even_odd);
    assert_eq!(ps[0].subpaths.len(), 2);
    assert!(!ps[1].even_odd);
    // Default fill is black.
    assert_eq!(ps[1].fill, Some(Color::Rgb(0.0, 0.0, 0.0)));
}

#[test]
fn named_and_hex_colours() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<rect width="1" height="1" fill="navy"/>
           <rect width="1" height="1" fill="#0f0"/>
           <rect width="1" height="1" fill="#336699"/>
           <rect width="1" height="1" style="fill: rgb(255, 128, 0)"/>
           <rect width="1" height="1" fill="none" stroke="cornflowerblue"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 5);
    assert_eq!(ps[0].fill, Some(Color::Rgb(0.0, 0.0, 128.0 / 255.0)));
    assert_eq!(ps[1].fill, Some(Color::Rgb(0.0, 1.0, 0.0)));
    assert_eq!(ps[2].fill, Some(Color::Rgb(0.2, 0.4, 0.6)));
    let (r, g, b) = rgb(&ps[3].fill);
    assert!(close(r, 1.0) && close(g, 128.0 / 255.0) && close(b, 0.0));
    assert!(ps[4].fill.is_none());
    assert_eq!(ps[4].stroke.as_ref().unwrap().color, Color::Rgb(100.0 / 255.0, 149.0 / 255.0, 237.0 / 255.0));
}

#[test]
fn strokes_dash_caps_joins() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<g transform="scale(2)"><path d="M0 0 L10 10 L20 0" fill="none" stroke="#ff0000"
              stroke-width="4" stroke-linecap="round" stroke-linejoin="bevel" stroke-miterlimit="7"
              stroke-dasharray="2 4" stroke-dashoffset="1"/></g>
           <path d="M0 0 L10 10" fill="none" stroke="black" stroke-linecap="square" stroke-linejoin="round"/>
           <path d="M0 0 L10 10" fill="none" stroke="black" stroke-linejoin="miter-clip"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 3);
    let s = ps[0].stroke.as_ref().unwrap();
    assert!(ps[0].fill.is_none());
    assert_eq!(s.color, Color::Rgb(1.0, 0.0, 0.0));
    assert!(close(s.width, 6.0), "width {}", s.width); // 4 * 2 * 0.75
    assert_eq!(s.cap, LineCap::Round);
    assert_eq!(s.join, LineJoin::Bevel);
    assert!(close(s.miter_limit, 7.0));
    assert_eq!(s.dash.len(), 2);
    assert!(close(s.dash[0], 3.0) && close(s.dash[1], 6.0), "{:?}", s.dash);
    assert!(close(s.dash_phase, 1.5));
    assert!(!ps[0].subpaths[0].closed);

    let s = ps[1].stroke.as_ref().unwrap();
    assert!(close(s.width, 0.75));
    assert_eq!(s.cap, LineCap::Square);
    assert_eq!(s.join, LineJoin::Round);
    assert!(close(s.miter_limit, 4.0)); // SVG default
    assert!(s.dash.is_empty());
    assert_eq!(ps[2].stroke.as_ref().unwrap().join, LineJoin::Miter);
}

#[test]
fn fill_and_stroke_in_one_item() {
    let doc = parse(r##"width="100" height="100""##, r##"<rect width="10" height="10" fill="red" stroke="blue"/>"##);
    let p = only_path(&doc);
    assert!(p.fill.is_some() && p.stroke.is_some());
}

#[test]
fn paint_order_stroke_first_splits() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<rect width="10" height="10" fill="red" stroke="blue" paint-order="stroke"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 2);
    assert!(ps[0].fill.is_none() && ps[0].stroke.is_some());
    assert!(ps[1].fill.is_some() && ps[1].stroke.is_none());
}

#[test]
fn gradient_approximation_warning() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><linearGradient id="g"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#000000"/></linearGradient>
           <radialGradient id="r"><stop offset="0" stop-color="red"/><stop offset="0.5" stop-color="lime"/><stop offset="1" stop-color="blue"/></radialGradient></defs>
           <rect width="10" height="10" fill="url(#g)"/>
           <rect width="10" height="10" fill="url(#r)" stroke="url(#g)"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 2);
    let (r, g, b) = rgb(&ps[0].fill);
    assert!(close(r, 0.5) && close(g, 0.5) && close(b, 0.5));
    let (r, g, b) = rgb(&ps[1].fill);
    assert!(close(r, 1.0 / 3.0) && close(g, 1.0 / 3.0) && close(b, 1.0 / 3.0));
    assert!(ps[1].stroke.is_some());
    assert_eq!(doc.warnings.iter().filter(|w| w.contains("gradient")).count(), 1, "{:?}", doc.warnings);
}

#[test]
fn pattern_becomes_gray() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><pattern id="p" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="2" height="2"/></pattern></defs>
           <rect width="10" height="10" fill="url(#p)"/>"##,
    );
    assert_eq!(only_path(&doc).fill, Some(Color::Rgb(0.5, 0.5, 0.5)));
    assert!(has_warning(&doc, "pattern"));
}

#[test]
fn opacity_ignored_with_single_warning() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<rect width="10" height="10" fill="red" fill-opacity="0.5"/>
           <rect width="10" height="10" fill="none" stroke="red" stroke-opacity="0.5"/>
           <g opacity="0.3"><rect width="10" height="10"/><rect x="20" width="10" height="10"/></g>"##,
    );
    assert_eq!(paths(&doc).len(), 4);
    assert_eq!(doc.warnings.iter().filter(|w| w.contains("opacity")).count(), 1, "{:?}", doc.warnings);
}

#[test]
fn invisible_and_unpainted_paths_dropped() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<rect width="10" height="10" fill="none"/>
           <rect width="10" height="10" visibility="hidden"/>
           <rect width="10" height="10" display="none"/>
           <rect width="10" height="10" fill="red" fill-opacity="0"/>
           <rect width="10" height="10" fill="green"/>"##,
    );
    assert_eq!(only_path(&doc).fill, Some(Color::Rgb(0.0, 128.0 / 255.0, 0.0)));
}

// ---------------------------------------------------------------- structure

#[test]
fn groups_inline_single_and_skip_empty() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<g id="two"><rect width="1" height="1"/><rect x="5" width="1" height="1"/></g>
           <g id="one"><rect width="1" height="1"/></g>
           <g id="empty"></g>
           <g id="emptyish"><rect width="1" height="1" fill="none"/></g>"##,
    );
    assert_eq!(doc.elements.len(), 2, "{:#?}", doc.elements);
    match &doc.elements[0] {
        Element::Group(c) => assert_eq!(c.len(), 2),
        e => panic!("expected group, got {e:?}"),
    }
    assert!(matches!(doc.elements[1], Element::Path(_)));
}

#[test]
fn clip_path_becomes_clip_group() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><clipPath id="c"><rect x="10" y="10" width="20" height="20"/></clipPath></defs>
           <g clip-path="url(#c)"><circle cx="20" cy="20" r="20" fill="red"/><rect width="5" height="5"/></g>"##,
    );
    assert_eq!(doc.elements.len(), 1);
    match &doc.elements[0] {
        Element::ClipGroup { clip, even_odd, children } => {
            assert!(!even_odd);
            assert_eq!(clip.len(), 1);
            let pts = anchors(&clip[0]);
            assert_pt(pts[0], 7.5, 67.5);
            assert_pt(pts[2], 22.5, 52.5);
            assert_eq!(children.len(), 2);
        }
        e => panic!("expected clip group, got {e:?}"),
    }
}

#[test]
fn clip_path_on_element_with_transform_and_rule() {
    // clip-path on a transformed shape: clip lives in the shape's user space.
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><clipPath id="c"><path d="M0 0H10V10H0Z M2 2H8V8H2Z" clip-rule="evenodd"/>
           </clipPath></defs>
           <rect transform="translate(20 20)" width="10" height="10" clip-path="url(#c)"/>"##,
    );
    match &doc.elements[0] {
        Element::ClipGroup { clip, even_odd, children } => {
            assert!(*even_odd);
            assert_eq!(clip.len(), 2);
            assert_pt(clip[0].start, 15.0, 60.0);
            assert_eq!(children.len(), 1);
        }
        e => panic!("expected clip group, got {e:?}"),
    }
}

#[test]
fn clip_path_object_bounding_box_and_nested() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs>
             <clipPath id="outer"><rect width="50" height="50"/></clipPath>
             <clipPath id="c" clipPathUnits="objectBoundingBox" clip-path="url(#outer)"><rect width="0.5" height="0.5"/></clipPath>
           </defs>
           <rect x="20" y="20" width="40" height="40" clip-path="url(#c)"/>"##,
    );
    // Nested clip-path is expressed as nested clip groups (intersection).
    match &doc.elements[0] {
        Element::ClipGroup { clip: outer, children, .. } => {
            assert_pt(anchors(&outer[0])[2], 37.5, 37.5);
            match &children[0] {
                Element::ClipGroup { clip, .. } => {
                    // bbox units: x 20 + 0.5*40 = 40 px
                    let pts = anchors(&clip[0]);
                    assert_pt(pts[0], 15.0, 60.0);
                    assert_pt(pts[2], 30.0, 45.0);
                }
                e => panic!("expected inner clip group, got {e:?}"),
            }
        }
        e => panic!("expected clip group, got {e:?}"),
    }
}

#[test]
fn mask_and_filter_ignored_with_warning() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><mask id="m"><rect width="50" height="50" fill="white"/></mask>
           <filter id="f"><feGaussianBlur stdDeviation="2"/></filter></defs>
           <rect width="10" height="10" mask="url(#m)"/>
           <rect width="10" height="10" filter="url(#f)"/>"##,
    );
    assert_eq!(paths(&doc).len(), 2);
    assert!(has_warning(&doc, "mask"), "{:?}", doc.warnings);
    assert!(has_warning(&doc, "filter"), "{:?}", doc.warnings);
}

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

#[test]
fn images_skipped_with_count_warning() {
    let body = format!(
        r##"<image width="10" height="10" xlink:href="data:image/png;base64,{PNG_1X1}"/>
           <image x="20" width="10" height="10" href="data:image/png;base64,{PNG_1X1}"/>
           <rect width="5" height="5"/>"##
    );
    let doc = parse(r##"width="100" height="100""##, &body);
    assert_eq!(paths(&doc).len(), 1);
    let w: Vec<_> = doc.warnings.iter().filter(|w| w.contains("image")).collect();
    assert_eq!(w.len(), 1, "{:?}", doc.warnings);
    assert!(w[0].starts_with('2'), "{}", w[0]);
}

#[test]
fn use_elements_are_resolved() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<defs><rect id="r" width="10" height="10"/></defs>
           <use href="#r" x="10"/><use xlink:href="#r" x="30"/>"##,
    );
    let ps = paths(&doc);
    assert_eq!(ps.len(), 2);
    assert_pt(ps[1].subpaths[0].start, 22.5, 75.0);
}

// ---------------------------------------------------------------- inputs

#[test]
fn svgz_is_supported() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    let raw = svg(r##"width="100" height="100""##, r##"<rect width="10" height="10" fill="red"/>"##);
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&raw).unwrap();
    let gz = enc.finish().unwrap();
    assert_eq!(&gz[..2], &[0x1f, 0x8b]);
    let doc = parse_svg(&gz).unwrap();
    assert_eq!(only_path(&doc).fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    assert_eq!(parse_svg(&raw).unwrap(), doc);
}

#[test]
fn bom_and_xml_prolog_accepted() {
    let mut data = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n".as_bytes().to_vec();
    data.extend(svg(r##"width="10" height="10""##, r##"<rect width="5" height="5"/>"##));
    let doc = parse_svg(&data).unwrap();
    assert_eq!(doc.path_count(), 1);
}

#[test]
fn text_converted_to_paths() {
    let doc = parse(
        r##"width="200" height="100""##,
        r##"<text x="10" y="50" font-family="Arial, Helvetica, sans-serif" font-size="30" fill="purple">Hello</text>"##,
    );
    if doc.path_count() == 0 {
        // No usable system fonts (e.g. a minimal CI container): must warn, not fail.
        assert!(has_warning(&doc, "font"), "{:?}", doc.warnings);
        eprintln!("no system fonts available; text test ran in tolerant mode");
        return;
    }
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    let ps = paths(&doc);
    assert!(ps.iter().all(|p| p.fill == Some(Color::Rgb(128.0 / 255.0, 0.0, 128.0 / 255.0))));
    assert!(ps.iter().flat_map(|p| &p.subpaths).count() >= 5, "expected several glyph outlines");
    let b = doc.content_bounds().unwrap();
    // Baseline at y=50 px -> 37.5 pt; text sits above the baseline in page space.
    assert!(b.llx >= 7.0 && b.lly >= 36.0 && b.ury <= 60.0 && b.urx < 150.0, "{b:?}");
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        assert!(doc.path_count() > 0);
    }
}

#[test]
fn empty_text_does_not_warn() {
    let doc = parse(r##"width="10" height="10""##, r##"<text x="1" y="1">   </text><rect width="1" height="1"/>"##);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

#[test]
fn malformed_input_returns_err() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("whitespace", b"  \n ".to_vec()),
        ("truncated", b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\"".to_vec()),
        ("not xml", b"this is not an svg".to_vec()),
        ("not svg root", b"<html><body/></html>".to_vec()),
        ("binary", vec![0xff, 0xfe, 0x00, 0x01, 0x80, 0x90]),
        ("bad gzip", vec![0x1f, 0x8b, 0x08, 0x00, 0xde, 0xad, 0xbe, 0xef]),
        ("zero size", svg(r##"width="0" height="10""##, "<rect width=\"1\" height=\"1\"/>")),
        ("unbalanced tags", b"<svg xmlns=\"http://www.w3.org/2000/svg\"><g></svg>".to_vec()),
    ];
    for (name, data) in cases {
        let r = parse_svg(&data);
        let err = r.expect_err(&format!("{name}: expected an error"));
        assert!(!err.to_string().is_empty(), "{name}");
    }
}

#[test]
fn garbage_attributes_do_not_panic() {
    let doc = parse(
        r##"width="100" height="100""##,
        r##"<path d="M 10 10 L Z Q 1e400 5 nan"/><rect width="-5" height="abc"/>
           <circle r="-1"/><path d="M0 0 L10 10" stroke-width="-3" stroke="red" stroke-dasharray="-1 2"/>
           <g transform="matrix(0 0 0 0 0 0)"><rect width="10" height="10"/></g>
           <rect width="10" height="10" fill="url(#missing)"/>"##,
    );
    for p in paths(&doc) {
        for sp in &p.subpaths {
            assert!(sp.points().all(|pt| pt.x.is_finite() && pt.y.is_finite()));
        }
    }
}

// ---------------------------------------------------------------- end to end

#[test]
fn end_to_end_svg_to_ai8() {
    let doc = parse(
        r##"width="200" height="200""##,
        r##"<defs><clipPath id="c"><circle cx="100" cy="100" r="80"/></clipPath></defs>
           <g id="grp"><rect x="10" y="10" width="50" height="50" fill="#ff0000"/>
              <path d="M10 100 Q50 150 90 100" fill="none" stroke="#0000ff" stroke-width="2" stroke-dasharray="4 2"/></g>
           <g clip-path="url(#c)"><rect width="200" height="200" fill="lime"/></g>
           <path d="M0 0H50V50H0Z M10 10H40V40H10Z" fill-rule="evenodd" transform="translate(120 120)"/>"##,
    );
    let ai = write_ai8(&doc, &WriteOptions::default());
    let stats = common::validate_ai8(&ai);
    assert_eq!(stats.paths, 5, "{stats:?}"); // 4 painted paths + the clip path
    assert_eq!(stats.groups, 1, "{stats:?}");
    assert_eq!(stats.clip_groups, 1, "{stats:?}");
    assert_eq!(stats.compound_paths, 1, "{stats:?}"); // the even-odd ring
    assert!(ai.contains("%%BoundingBox:"));
    assert!(ai.contains("1 0 0 Xa"), "red RGB fill");
    assert!(ai.contains("0 0 1 XA"), "blue RGB stroke");
    assert!(ai.contains("[3 1.5]0 d"), "scaled dash");
    assert!(ai.contains(" C\n"), "curves");
    assert!(ai.contains("h W"), "clip");
    assert!(ai.contains("1 XR"), "even-odd");
    assert!(ai.lines().any(|l| l == "S"), "open stroked path");
    assert!(ai.lines().any(|l| l == "f"), "closed filled path");
    assert!(ai.lines().any(|l| l == "u") && ai.lines().any(|l| l == "U"));
    // The red rect: 10..60 px -> 7.5..45 pt, y flipped: 150 - 7.5 = 142.5.
    assert!(ai.contains("7.5 142.5 m"), "{ai}");
}
