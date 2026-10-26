//! Integration tests for the PDF content-stream interpreter (`pdf::parse_pdf`).

mod common;

use ai8_converter::ir::{
    Color, Document, Element, GradientKind, GradientPaint, LineCap, LineJoin, Matrix, PathItem, PatternPaint, Point, Rect,
    Segment,
};
use ai8_converter::pdf::{page_count, parse_pdf};
use ai8_converter::{WriteOptions, write_ai8};
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use std::sync::Arc;

// --- PDF construction helpers ------------------------------------------------

struct PdfBuilder {
    doc: lopdf::Document,
    pages_id: ObjectId,
    kids: Vec<Object>,
}

impl PdfBuilder {
    fn new() -> Self {
        let mut doc = lopdf::Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        Self { doc, pages_id, kids: Vec::new() }
    }

    fn add<T: Into<Object>>(&mut self, obj: T) -> ObjectId {
        self.doc.add_object(obj)
    }

    /// Add a stream object (optionally Flate-compressed).
    fn stream(&mut self, dict: Dictionary, content: &str, compress: bool) -> ObjectId {
        let mut s = Stream::new(dict, content.as_bytes().to_vec());
        if compress {
            s.compress().unwrap();
        }
        self.add(s)
    }

    /// Add a page. `extra` entries are merged into the page dictionary.
    fn page(&mut self, content: &str, resources: Dictionary, extra: Dictionary) -> ObjectId {
        let content_id = self.stream(Dictionary::new(), content, false);
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => self.pages_id,
            "Contents" => content_id,
            "Resources" => resources,
        };
        for (k, v) in extra.iter() {
            page.set(k.clone(), v.clone());
        }
        let id = self.add(page);
        self.kids.push(id.into());
        id
    }

    /// Serialise. `pages_extra` entries go on the root Pages node (inherited).
    fn finish(mut self, pages_extra: Dictionary) -> Vec<u8> {
        let mut pages = dictionary! {
            "Type" => "Pages",
            "Count" => self.kids.len() as i64,
            "Kids" => self.kids.clone(),
        };
        for (k, v) in pages_extra.iter() {
            pages.set(k.clone(), v.clone());
        }
        self.doc.objects.insert(self.pages_id, Object::Dictionary(pages));
        let catalog = self.add(dictionary! { "Type" => "Catalog", "Pages" => self.pages_id });
        self.doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        self.doc.save_to(&mut out).unwrap();
        out
    }
}

fn rect_arr(a: f64, b: f64, c: f64, d: f64) -> Object {
    Object::Array(vec![a.into(), b.into(), c.into(), d.into()])
}

/// Wrap a content stream and resources into a one-page PDF (MediaBox 0 0 200 100,
/// inherited from the Pages node).
fn one_page(content: &str, resources: Dictionary) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.page(content, resources, Dictionary::new());
    b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 200.0, 100.0) })
}

fn parse(content: &str) -> Document {
    parse_pdf(&one_page(content, Dictionary::new()), 0).unwrap()
}

fn parse_res(content: &str, res: Dictionary) -> Document {
    parse_pdf(&one_page(content, res), 0).unwrap()
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

fn only_path(d: &Document) -> &PathItem {
    let p = paths(&d.elements);
    assert_eq!(p.len(), 1, "expected one path, got {:?}", d.elements);
    p[0]
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

fn pt_approx(p: Point, x: f64, y: f64) -> bool {
    approx(p.x, x) && approx(p.y, y)
}

fn type2(c0: &[f64], c1: &[f64]) -> Object {
    Object::Dictionary(dictionary! {
        "FunctionType" => 2,
        "Domain" => vec![0.into(), 1.into()],
        "C0" => c0.iter().map(|v| Object::Real(*v as f32)).collect::<Vec<_>>(),
        "C1" => c1.iter().map(|v| Object::Real(*v as f32)).collect::<Vec<_>>(),
        "N" => 1,
    })
}

fn name(n: &str) -> Object {
    Object::Name(n.as_bytes().to_vec())
}

fn cs_res(entries: Vec<(&str, Object)>) -> Dictionary {
    let mut cs = Dictionary::new();
    for (k, v) in entries {
        cs.set(k, v);
    }
    dictionary! { "ColorSpace" => cs }
}

fn ai(doc: &Document) -> String {
    let out = write_ai8(doc, &WriteOptions::default());
    common::validate_ai8(&out);
    out
}

// --- path construction & painting -------------------------------------------

#[test]
fn path_ops_m_l_c_v_y_h() {
    let d = parse("10 10 m 20 10 l 30 30 40 40 50 10 c 60 60 70 70 v 80 80 90 90 y h S");
    let p = only_path(&d);
    let sp = &p.subpaths[0];
    assert_eq!(sp.start, Point::new(10.0, 10.0));
    assert_eq!(sp.segments[0], Segment::Line(Point::new(20.0, 10.0)));
    assert_eq!(
        sp.segments[1],
        Segment::Curve(Point::new(30.0, 30.0), Point::new(40.0, 40.0), Point::new(50.0, 10.0))
    );
    // v: first control point = current point.
    assert_eq!(
        sp.segments[2],
        Segment::Curve(Point::new(50.0, 10.0), Point::new(60.0, 60.0), Point::new(70.0, 70.0))
    );
    // y: second control point = end point.
    assert_eq!(
        sp.segments[3],
        Segment::Curve(Point::new(80.0, 80.0), Point::new(90.0, 90.0), Point::new(90.0, 90.0))
    );
    assert!(sp.closed);
    assert!(p.fill.is_none());
    assert_eq!(p.stroke.as_ref().unwrap().color, Color::Gray(0.0));
}

#[test]
fn rect_op() {
    let d = parse("10 20 30 40 re f");
    let sp = &only_path(&d).subpaths[0];
    assert!(sp.closed);
    assert_eq!(sp.start, Point::new(10.0, 20.0));
    assert_eq!(
        sp.segments,
        vec![
            Segment::Line(Point::new(40.0, 20.0)),
            Segment::Line(Point::new(40.0, 60.0)),
            Segment::Line(Point::new(10.0, 60.0)),
        ]
    );
}

#[test]
fn painting_operators() {
    // (op, fill, stroke, even_odd, closed)
    let cases = [
        ("S", false, true, false, false),
        ("s", false, true, false, true),
        ("f", true, false, false, false),
        ("F", true, false, false, false),
        ("f*", true, false, true, false),
        ("B", true, true, false, false),
        ("B*", true, true, true, false),
        ("b", true, true, false, true),
        ("b*", true, true, true, true),
    ];
    for (op, fill, stroke, eo, closed) in cases {
        let d = parse(&format!("0 0 m 10 0 l 10 10 l {op}"));
        let p = only_path(&d);
        assert_eq!(p.fill.is_some(), fill, "{op}");
        assert_eq!(p.stroke.is_some(), stroke, "{op}");
        assert_eq!(p.even_odd, eo, "{op}");
        assert_eq!(p.subpaths[0].closed, closed, "{op}");
    }
    assert!(parse("0 0 m 10 0 l 10 10 l n").elements.is_empty());
}

#[test]
fn empty_paths_skipped() {
    assert!(parse("10 10 m f 5 5 m S").elements.is_empty());
    assert!(parse("f S B").elements.is_empty());
    // Lone movetos inside a path are dropped, real subpaths kept.
    let d = parse("0 0 m 5 5 m 10 0 l 20 20 m f");
    let p = only_path(&d);
    assert_eq!(p.subpaths.len(), 1);
    assert_eq!(p.subpaths[0].start, Point::new(5.0, 5.0));
}

#[test]
fn compound_path_multiple_subpaths() {
    let d = parse("0 0 100 100 re 25 25 50 50 re f*");
    let p = only_path(&d);
    assert_eq!(p.subpaths.len(), 2);
    assert!(p.even_odd);
}

// --- transforms ---------------------------------------------------------------

#[test]
fn nested_cm_and_q_restore() {
    let d = parse(
        "q 1 0 0 1 100 0 cm q 2 0 0 2 0 0 cm 0 0 m 10 10 l S Q 0 0 m 10 10 l S Q 0 0 m 10 10 l S",
    );
    let p = paths(&d.elements);
    assert_eq!(p.len(), 3);
    // Inner: scale then translate -> (100,0) .. (120,20), width scaled by 2.
    assert_eq!(p[0].subpaths[0].start, Point::new(100.0, 0.0));
    assert_eq!(p[0].subpaths[0].segments[0], Segment::Line(Point::new(120.0, 20.0)));
    assert_eq!(p[0].stroke.as_ref().unwrap().width, 2.0);
    assert_eq!(p[1].subpaths[0].segments[0], Segment::Line(Point::new(110.0, 10.0)));
    assert_eq!(p[1].stroke.as_ref().unwrap().width, 1.0);
    assert_eq!(p[2].subpaths[0].segments[0], Segment::Line(Point::new(10.0, 10.0)));
}

#[test]
fn rotation_cm() {
    // 90 degree rotation: (x, y) -> (-y, x)
    let d = parse("0 1 -1 0 50 0 cm 10 0 m 10 20 l S");
    let sp = &only_path(&d).subpaths[0];
    assert!(pt_approx(sp.start, 50.0, 10.0));
    let Segment::Line(p) = sp.segments[0] else { panic!() };
    assert!(pt_approx(p, 30.0, 10.0));
}

#[test]
fn q_restores_colour_and_width() {
    let d = parse("q 1 0 0 rg 5 w 0 0 m 1 1 l B Q 0 0 m 1 1 l B");
    let p = paths(&d.elements);
    assert_eq!(p[0].fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    assert_eq!(p[0].stroke.as_ref().unwrap().width, 5.0);
    assert_eq!(p[1].fill, Some(Color::Gray(0.0)));
    assert_eq!(p[1].stroke.as_ref().unwrap().width, 1.0);
}

// --- line styles ------------------------------------------------------------------

#[test]
fn line_styles_scaled_by_ctm() {
    let d = parse("3 0 0 3 0 0 cm 2 w 2 J 1 j 7 M [3 1 2] 0.5 d 0 0 m 10 0 l S");
    let s = only_path(&d).stroke.clone().unwrap();
    assert!(approx(s.width, 6.0));
    assert_eq!(s.cap, LineCap::Square);
    assert_eq!(s.join, LineJoin::Round);
    assert_eq!(s.miter_limit, 7.0);
    assert_eq!(s.dash, vec![9.0, 3.0, 6.0]);
    assert!(approx(s.dash_phase, 1.5));
    // Non-uniform scale: geometric mean.
    let d = parse("4 0 0 1 0 0 cm 1 w 0 0 m 10 0 l S");
    assert!(approx(only_path(&d).stroke.as_ref().unwrap().width, 2.0));
    // Empty dash = solid.
    let d = parse("[] 0 d 0 0 m 10 0 l S");
    assert!(only_path(&d).stroke.as_ref().unwrap().dash.is_empty());
}

#[test]
fn ext_gstate_parameters() {
    let res = dictionary! {
        "ExtGState" => dictionary! {
            "GS1" => dictionary! {
                "LW" => 4,
                "LC" => 1,
                "LJ" => 2,
                "ML" => 3,
                "D" => vec![Object::Array(vec![2.into(), 2.into()]), 1.into()],
                "OP" => true,
            },
            "GS2" => dictionary! { "op" => false, "ca" => 0.5 },
        }
    };
    let d = parse_res("2 0 0 2 0 0 cm /GS1 gs 0 0 m 10 0 l B /GS2 gs 0 0 m 10 0 l B", res);
    let p = paths(&d.elements);
    let s = p[0].stroke.as_ref().unwrap();
    assert_eq!(s.width, 8.0);
    assert_eq!(s.cap, LineCap::Round);
    assert_eq!(s.join, LineJoin::Bevel);
    assert_eq!(s.miter_limit, 3.0);
    assert_eq!(s.dash, vec![4.0, 4.0]);
    assert_eq!(s.dash_phase, 2.0);
    // OP without op sets both.
    assert!(p[0].stroke_overprint && p[0].fill_overprint);
    assert!(p[1].stroke_overprint && !p[1].fill_overprint);
    assert_eq!(d.warnings.iter().filter(|w| w.contains("transparency")).count(), 1);
}

#[test]
fn transparency_warned_once() {
    let res = dictionary! {
        "ExtGState" => dictionary! { "A" => dictionary! { "CA" => 0.3 }, "B" => dictionary! { "ca" => 0.2 } }
    };
    let d = parse_res("/A gs /B gs /A gs", res);
    assert_eq!(d.warnings.len(), 1, "{:?}", d.warnings);
    let res = dictionary! { "ExtGState" => dictionary! { "A" => dictionary! { "CA" => 1, "ca" => 1.0 } } };
    assert!(parse_res("/A gs", res).warnings.is_empty());
}

// --- colours ------------------------------------------------------------------

#[test]
fn device_colour_operators() {
    let d = parse(
        "0.5 g 0.25 G 0 0 m 1 1 l B 1 0 0 rg 0 1 0 RG 0 0 m 1 1 l B 0.1 0.2 0.3 0.4 k 0 0 0 1 K 0 0 m 1 1 l B",
    );
    let p = paths(&d.elements);
    assert_eq!(p[0].fill, Some(Color::Gray(0.5)));
    assert_eq!(p[0].stroke.as_ref().unwrap().color, Color::Gray(0.25));
    assert_eq!(p[1].fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    assert_eq!(p[1].stroke.as_ref().unwrap().color, Color::Rgb(0.0, 1.0, 0.0));
    assert_eq!(p[2].fill, Some(Color::Cmyk(0.1, 0.2, 0.3, 0.4)));
    assert_eq!(p[2].stroke.as_ref().unwrap().color, Color::Cmyk(0.0, 0.0, 0.0, 1.0));
}

#[test]
fn cs_sc_device_spaces() {
    let d = parse(
        "/DeviceCMYK cs 0 0 m 1 1 l f /DeviceRGB cs 0.2 0.4 0.6 sc 0 0 m 1 1 l f /DeviceGray CS 0.7 SCN 0 0 m 1 1 l S",
    );
    let p = paths(&d.elements);
    // Initial CMYK colour is black.
    assert_eq!(p[0].fill, Some(Color::Cmyk(0.0, 0.0, 0.0, 1.0)));
    assert_eq!(p[1].fill, Some(Color::Rgb(0.2, 0.4, 0.6)));
    assert_eq!(p[2].stroke.as_ref().unwrap().color, Color::Gray(0.7));
}

#[test]
fn cal_and_icc_spaces() {
    let mut b = PdfBuilder::new();
    let icc4 = b.stream(dictionary! { "N" => 4 }, "", false);
    let icc1 = b.stream(dictionary! { "N" => 1, "Alternate" => "DeviceGray" }, "", false);
    let res = cs_res(vec![
        ("CS0", Object::Array(vec![name("ICCBased"), icc4.into()])),
        ("CS1", Object::Array(vec![name("ICCBased"), icc1.into()])),
        ("CS2", Object::Array(vec![name("CalRGB"), Object::Dictionary(dictionary! { "WhitePoint" => rect_arr(0.95, 1.0, 1.09, 0.0) })])),
        ("CS3", Object::Array(vec![name("CalGray"), Object::Dictionary(Dictionary::new())])),
    ]);
    b.page(
        "/CS0 cs 0.1 0.2 0.3 0.4 scn 0 0 m 1 1 l f /CS1 cs 0.3 scn 0 0 m 1 1 l f /CS2 cs 1 0 1 scn 0 0 m 1 1 l f /CS3 cs 0.9 sc 0 0 m 1 1 l f",
        res,
        dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 10.0, 10.0) },
    );
    let d = parse_pdf(&b.finish(Dictionary::new()), 0).unwrap();
    let p = paths(&d.elements);
    assert_eq!(p[0].fill, Some(Color::Cmyk(0.1, 0.2, 0.3, 0.4)));
    assert_eq!(p[1].fill, Some(Color::Gray(0.3)));
    assert_eq!(p[2].fill, Some(Color::Rgb(1.0, 0.0, 1.0)));
    assert_eq!(p[3].fill, Some(Color::Gray(0.9)));
}

#[test]
fn separation_cutcontour_type2() {
    let sep = Object::Array(vec![
        name("Separation"),
        name("CutContour"),
        name("DeviceCMYK"),
        type2(&[0.0, 0.0, 0.0, 0.0], &[0.0, 1.0, 0.0, 0.0]),
    ]);
    let d = parse_res("/CS0 CS 1 SCN 0.5 w 0 0 m 50 50 l S /CS0 cs 0.4 scn 0 0 m 1 1 l f", cs_res(vec![("CS0", sep)]));
    let p = paths(&d.elements);
    assert_eq!(
        p[0].stroke.as_ref().unwrap().color,
        Color::Spot { name: "CutContour".into(), cmyk: [0.0, 1.0, 0.0, 0.0], tint: 1.0 }
    );
    let Some(Color::Spot { name, tint, .. }) = &p[1].fill else { panic!() };
    assert_eq!(name, "CutContour");
    assert!(approx(*tint, 0.4));
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
}

#[test]
fn separation_with_referenced_sampled_function() {
    let mut b = PdfBuilder::new();
    let f = b.stream(
        dictionary! {
            "FunctionType" => 0,
            "Domain" => vec![0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
            "Size" => vec![2.into()],
            "BitsPerSample" => 8,
        },
        "",
        false,
    );
    // Replace with binary samples: tint 0 -> white, tint 1 -> RGB (1, 0.5, 0)
    if let Ok(Object::Stream(s)) = b.doc.get_object_mut(f) {
        s.set_content(vec![255, 255, 255, 255, 128, 0]);
    }
    let sep_id = b.add(Object::Array(vec![name("Separation"), name("Orange Spot"), name("DeviceRGB"), f.into()]));
    b.page("/S cs 0.5 scn 0 0 m 1 1 l f", cs_res(vec![("S", sep_id.into())]), Dictionary::new());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 10.0, 10.0) }), 0).unwrap();
    let Some(Color::Spot { name, cmyk, tint }) = &only_path(&d).fill else { panic!("{:?}", d.elements) };
    assert_eq!(name, "Orange Spot");
    assert!(approx(*tint, 0.5));
    // RGB (1, 0.502, 0) -> CMYK (0, ~0.5, 1, 0)
    assert!(approx(cmyk[0], 0.0) && (cmyk[1] - 0.498).abs() < 0.01 && approx(cmyk[2], 1.0) && approx(cmyk[3], 0.0), "{cmyk:?}");
}

#[test]
fn separation_with_postscript_function() {
    let mut b = PdfBuilder::new();
    let f = b.stream(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        "{ dup 0.8 mul exch 0 exch 0 }",
        true,
    );
    let sep = Object::Array(vec![name("Separation"), name("Teal"), name("DeviceCMYK"), f.into()]);
    b.page("/S cs 1 scn 0 0 m 1 1 l f", cs_res(vec![("S", sep)]), Dictionary::new());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 10.0, 10.0) }), 0).unwrap();
    let Some(Color::Spot { cmyk, .. }) = &only_path(&d).fill else { panic!() };
    assert!(approx(cmyk[0], 0.8) && approx(cmyk[1], 0.0) && approx(cmyk[2], 1.0) && approx(cmyk[3], 0.0), "{cmyk:?}");
}

#[test]
fn separation_all_and_none() {
    let all = Object::Array(vec![name("Separation"), name("All"), name("DeviceCMYK"), type2(&[0.0; 4], &[1.0; 4])]);
    let none = Object::Array(vec![name("Separation"), name("None"), name("DeviceCMYK"), type2(&[0.0; 4], &[0.0; 4])]);
    let d = parse_res(
        "/A cs 1 scn 0 0 m 1 1 l f /N cs 1 scn 0 0 m 1 1 l f /N CS 0 0 m 1 1 l S",
        cs_res(vec![("A", all), ("N", none)]),
    );
    assert_eq!(d.path_count(), 1);
    assert_eq!(only_path(&d).fill, Some(Color::Cmyk(1.0, 1.0, 1.0, 1.0)));
}

#[test]
fn devicen_single_and_multi() {
    let single = Object::Array(vec![
        name("DeviceN"),
        Object::Array(vec![name("Gold")]),
        name("DeviceCMYK"),
        type2(&[0.0; 4], &[0.0, 0.2, 1.0, 0.1]),
    ]);
    let process = Object::Array(vec![name("DeviceN"), Object::Array(vec![name("Cyan"), name("Yellow")]), name("DeviceCMYK"), type2(&[0.0; 4], &[1.0; 4])]);
    let mut b = PdfBuilder::new();
    let f = b.stream(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        "{ add 1 exch sub dup dup }",
        false,
    );
    let mixed = Object::Array(vec![name("DeviceN"), Object::Array(vec![name("SpotA"), name("SpotB")]), name("DeviceRGB"), f.into()]);
    b.page(
        "/D1 cs 0.5 scn 0 0 m 1 1 l f /D2 cs 0.3 0.6 scn 0 0 m 1 1 l f /D3 cs 0.25 0.25 scn 0 0 m 1 1 l f",
        cs_res(vec![("D1", single), ("D2", process), ("D3", mixed)]),
        Dictionary::new(),
    );
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 10.0, 10.0) }), 0).unwrap();
    let p = paths(&d.elements);
    let Some(Color::Spot { name, cmyk, tint }) = &p[0].fill else { panic!() };
    assert_eq!(name, "Gold");
    assert!(approx(cmyk[1], 0.2) && approx(cmyk[2], 1.0) && approx(cmyk[3], 0.1));
    assert!(approx(*tint, 0.5));
    assert_eq!(p[1].fill, Some(Color::Cmyk(0.3, 0.0, 0.6, 0.0)));
    let Some(Color::Rgb(r, g, bl)) = p[2].fill else { panic!("{:?}", p[2].fill) };
    assert!(approx(r, 0.5) && approx(g, 0.5) && approx(bl, 0.5));
}

#[test]
fn lab_and_indexed() {
    let lab = Object::Array(vec![name("Lab"), Object::Dictionary(dictionary! { "WhitePoint" => vec![Object::Real(0.9642), 1.into(), Object::Real(0.8249)] })]);
    let idx = Object::Array(vec![name("Indexed"), name("DeviceCMYK"), 1.into(), Object::string_literal(vec![0u8, 0, 0, 255, 255, 0, 0, 0])]);
    let d = parse_res("/L cs 0 0 0 sc 0 0 m 1 1 l f /I cs 1 sc 0 0 m 1 1 l f", cs_res(vec![("L", lab), ("I", idx)]));
    let p = paths(&d.elements);
    let Some(Color::Rgb(r, g, b)) = p[0].fill else { panic!() };
    assert!(r < 0.01 && g < 0.01 && b < 0.01);
    assert_eq!(p[1].fill, Some(Color::Cmyk(1.0, 0.0, 0.0, 0.0)));
}

// --- tiling patterns ----------------------------------------------------------

fn nums_arr(v: &[f64]) -> Object {
    Object::Array(v.iter().map(|x| Object::Real(*x as f32)).collect())
}

/// Tiling pattern stream dictionary.
fn tiling(paint_type: i64, bbox: [f64; 4], step: (f64, f64), matrix: Option<[f64; 6]>, res: Option<Dictionary>) -> Dictionary {
    let mut d = dictionary! {
        "Type" => "Pattern",
        "PatternType" => 1,
        "PaintType" => paint_type,
        "TilingType" => 1,
        "BBox" => nums_arr(&bbox),
        "XStep" => Object::Real(step.0 as f32),
        "YStep" => Object::Real(step.1 as f32),
    };
    if let Some(m) = matrix {
        d.set("Matrix", nums_arr(&m));
    }
    if let Some(r) = res {
        d.set("Resources", r);
    }
    d
}

fn pattern_res(entries: Vec<(&str, ObjectId)>) -> Dictionary {
    let mut p = Dictionary::new();
    for (k, id) in entries {
        p.set(k, id);
    }
    dictionary! { "Pattern" => p }
}

fn pat(c: Option<&Color>) -> &PatternPaint {
    match c {
        Some(Color::Pattern(p)) => p,
        other => panic!("expected a pattern paint, got {other:?}"),
    }
}

fn grad(c: Option<&Color>) -> &GradientPaint {
    match c {
        Some(Color::Gradient(g)) => g,
        other => panic!("expected a gradient paint, got {other:?}"),
    }
}

fn m_approx(m: &Matrix, v: [f64; 6]) -> bool {
    [m.a, m.b, m.c, m.d, m.e, m.f].iter().zip(v).all(|(a, b)| approx(*a, b))
}

fn stops(g: &GradientPaint) -> Vec<(f64, Color)> {
    g.gradient.stops.iter().map(|s| (s.offset, s.color.clone())).collect()
}

fn page_pdf(b: PdfBuilder) -> Document {
    parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 200.0, 100.0) }), 0).unwrap()
}

#[test]
fn coloured_tiling_pattern_in_pattern_space() {
    let mut b = PdfBuilder::new();
    // The cell has its own resources (a spot colour space).
    let cell_res = cs_res(vec![("Sp", Object::Array(vec![name("Separation"), name("Gold"), name("DeviceCMYK"), type2(&[0.0; 4], &[0.0, 0.2, 0.9, 0.0])]))]);
    let p0 = b.stream(
        tiling(1, [0.0, 0.0, 10.0, 10.0], (10.0, 10.0), Some([1.0, 0.0, 0.0, 1.0, 3.0, 4.0]), Some(cell_res)),
        "1 0 0 rg 0 0 5 5 re f /Sp cs 0.5 scn 2 0 0 2 0 0 cm 2.5 2.5 2.5 2.5 re f",
        true,
    );
    // The CTM at paint time (cm 2) scales the path but not the pattern.
    b.page("2 0 0 2 0 0 cm /Pattern cs /P0 scn 0 0 50 40 re f", pattern_res(vec![("P0", p0)]), Dictionary::new());
    let d = page_pdf(b);
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    let p = only_path(&d);
    assert!(pt_approx(p.subpaths[0].segments[1].clone().end(), 100.0, 80.0));
    let pp = pat(p.fill.as_ref());
    assert_eq!(pp.matrix, Matrix::new(1.0, 0.0, 0.0, 1.0, 3.0, 4.0));
    let t = &pp.pattern;
    assert_eq!(t.name, "Pattern 1");
    assert_eq!((t.bbox, t.x_step, t.y_step), (Rect { llx: 0.0, lly: 0.0, urx: 10.0, ury: 10.0 }, 10.0, 10.0));
    let tile = paths(&t.elements);
    assert_eq!(tile.len(), 2);
    assert_eq!(tile[0].fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    assert_eq!(tile[0].subpaths[0].start, Point::new(0.0, 0.0));
    // Cell CTM starts at identity; the cell's own cm applies.
    assert_eq!(tile[1].subpaths[0].start, Point::new(5.0, 5.0));
    assert!(matches!(&tile[1].fill, Some(Color::Spot { name, tint, .. }) if name == "Gold" && *tint == 0.5));
}

trait SegEnd {
    fn end(self) -> Point;
}
impl SegEnd for Segment {
    fn end(self) -> Point {
        match self {
            Segment::Line(p) | Segment::Curve(_, _, p) => p,
        }
    }
}

#[test]
fn uncoloured_pattern_colour_from_scn_and_cached_per_colour() {
    let mut b = PdfBuilder::new();
    let p0 = b.stream(
        tiling(2, [0.0, 0.0, 4.0, 4.0], (4.0, 4.0), None, None),
        // Colour operators are ignored in an uncoloured cell.
        "1 0 0 rg 0 0 2 2 re f 0 0 m 4 4 l S",
        false,
    );
    let mut res = pattern_res(vec![("P0", p0)]);
    res.set(
        "ColorSpace",
        dictionary! {
            "UP" => vec![name("Pattern"), name("DeviceCMYK")],
            "US" => Object::Array(vec![name("Pattern"), Object::Array(vec![name("Separation"), name("Varnish"), name("DeviceCMYK"), type2(&[0.0; 4], &[0.1, 0.0, 0.0, 0.0])])]),
        },
    );
    b.page(
        "/UP cs 0 1 0 0 /P0 scn 0 0 10 10 re f 1 0 0 0 /P0 scn 20 0 10 10 re f 0 1 0 0 /P0 scn 40 0 10 10 re f \
         /US cs 0.4 /P0 scn 60 0 10 10 re f",
        res,
        Dictionary::new(),
    );
    let d = page_pdf(b);
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 4);
    let pp: Vec<&PatternPaint> = p.iter().map(|x| pat(x.fill.as_ref())).collect();
    assert!(Arc::ptr_eq(&pp[0].pattern, &pp[2].pattern));
    assert!(!Arc::ptr_eq(&pp[0].pattern, &pp[1].pattern));
    let names: std::collections::HashSet<&str> = pp.iter().map(|x| x.pattern.name.as_str()).collect();
    assert_eq!(names.len(), 3, "{names:?}");
    let expect = [
        Color::Cmyk(0.0, 1.0, 0.0, 0.0),
        Color::Cmyk(1.0, 0.0, 0.0, 0.0),
        Color::Cmyk(0.0, 1.0, 0.0, 0.0),
        Color::Spot { name: "Varnish".into(), cmyk: [0.1f32 as f64, 0.0, 0.0, 0.0], tint: 0.4 },
    ];
    for (x, c) in pp.iter().zip(expect) {
        let tile = paths(&x.pattern.elements);
        assert_eq!(tile.len(), 2);
        assert_eq!(tile[0].fill.as_ref(), Some(&c));
        assert_eq!(tile[1].stroke.as_ref().map(|s| &s.color), Some(&c));
    }
}

#[test]
fn pattern_matrix_with_rotate_and_xstep_differs_from_bbox() {
    let mut b = PdfBuilder::new();
    let p0 = b.stream(tiling(1, [0.0, 0.0, 6.0, 6.0], (10.0, 8.0), Some([2.0, 0.0, 0.0, 2.0, 5.0, 0.0]), None), "0 0 6 6 re f", false);
    b.page("/Pattern cs /P0 scn 0 0 10 10 re f", pattern_res(vec![("P0", p0)]), dictionary! { "Rotate" => 90 });
    let d = page_pdf(b);
    let p = only_path(&d);
    let pp = pat(p.fill.as_ref());
    assert_eq!((pp.pattern.x_step, pp.pattern.y_step), (10.0, 8.0));
    assert_eq!(pp.pattern.bbox.width(), 6.0);
    // Rotate 90 on a 200x100 page: (x, y) -> (y, 200 - x). The pattern
    // matrix lands in the same space as the paths.
    assert!(m_approx(&pp.matrix, [0.0, -2.0, 2.0, 0.0, 0.0, 195.0]), "{:?}", pp.matrix);
    let o = pp.matrix.apply(Point::new(0.0, 0.0));
    assert!(pt_approx(o, 0.0, 195.0));
    // The page-space path corner (0,0) maps to (0,200) like the geometry.
    assert_eq!(p.subpaths[0].start, Point::new(0.0, 200.0));
}

#[test]
fn pattern_in_form_maps_to_form_space_at_invocation() {
    let mut b = PdfBuilder::new();
    let p0 = b.stream(tiling(1, [0.0, 0.0, 5.0, 5.0], (5.0, 5.0), Some([1.0, 0.0, 0.0, 1.0, 1.0, 0.0]), None), "0 0 2 2 re f", false);
    let form = b.stream(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => rect_arr(0.0, 0.0, 50.0, 50.0),
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 10.into(), 20.into()],
            "Resources" => pattern_res(vec![("P0", p0)]),
        },
        // A cm inside the form does not move the pattern.
        "/Pattern cs /P0 scn 3 0 0 3 0 0 cm 0 0 10 10 re f",
        false,
    );
    let mut res = pattern_res(vec![("P0", p0)]);
    res.set("XObject", dictionary! { "Fm0" => form });
    b.page("/Pattern cs /P0 scn 0 0 5 5 re f q 2 0 0 2 0 0 cm /Fm0 Do Q", res, Dictionary::new());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 500.0, 500.0) }), 0).unwrap();
    let p = paths(&d.elements);
    assert_eq!(p.len(), 2);
    let (page, in_form) = (pat(p[0].fill.as_ref()), pat(p[1].fill.as_ref()));
    assert_eq!(page.matrix, Matrix::new(1.0, 0.0, 0.0, 1.0, 1.0, 0.0));
    // Pattern matrix x form matrix (translate 10,20) x CTM at Do (scale 2).
    assert_eq!(in_form.matrix, Matrix::new(2.0, 0.0, 0.0, 2.0, 22.0, 40.0));
    // One shared definition for both uses.
    assert!(Arc::ptr_eq(&page.pattern, &in_form.pattern));
}

#[test]
fn pattern_on_stroke_and_fill_shares_definition() {
    let mut b = PdfBuilder::new();
    let p0 = b.stream(tiling(1, [0.0, 0.0, 4.0, 4.0], (4.0, 4.0), None, None), "0 0 1 rg 0 0 2 4 re f", false);
    b.page(
        "/Pattern cs /Pattern CS /P0 scn /P0 SCN 3 w 0 0 m 50 50 l S 0 0 20 20 re B /P0 scn 30 30 5 5 re f",
        pattern_res(vec![("P0", p0)]),
        Dictionary::new(),
    );
    let d = page_pdf(b);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 3);
    assert!(p[0].fill.is_none());
    let s = p[0].stroke.as_ref().unwrap();
    assert_eq!(s.width, 3.0);
    let sp = pat(Some(&s.color));
    let fp = pat(p[1].fill.as_ref());
    assert!(Arc::ptr_eq(&sp.pattern, &fp.pattern));
    assert!(Arc::ptr_eq(&sp.pattern, &pat(p[1].stroke.as_ref().map(|s| &s.color)).pattern));
    assert!(Arc::ptr_eq(&sp.pattern, &pat(p[2].fill.as_ref()).pattern));
    assert_eq!(paths(&sp.pattern.elements)[0].fill, Some(Color::Rgb(0.0, 0.0, 1.0)));
}

#[test]
fn nested_patterns_and_recursion_guard() {
    let mut b = PdfBuilder::new();
    let inner = b.stream(tiling(1, [0.0, 0.0, 2.0, 2.0], (2.0, 2.0), Some([0.5, 0.0, 0.0, 0.5, 0.0, 0.0]), None), "0 g 0 0 1 1 re f", false);
    let outer = b.stream(
        tiling(1, [0.0, 0.0, 10.0, 10.0], (10.0, 10.0), Some([3.0, 0.0, 0.0, 3.0, 0.0, 0.0]), Some(pattern_res(vec![("In", inner)]))),
        "/Pattern cs /In scn 0 0 5 5 re f",
        false,
    );
    // A pattern that paints with itself.
    let selfref_id = b.doc.new_object_id();
    let selfref = Stream::new(
        tiling(1, [0.0, 0.0, 10.0, 10.0], (10.0, 10.0), None, Some(pattern_res(vec![("Me", selfref_id)]))),
        b"/Pattern cs /Me scn 0 0 5 5 re f".to_vec(),
    );
    b.doc.objects.insert(selfref_id, Object::Stream(selfref));
    b.page(
        "/Pattern cs /Out scn 0 0 50 50 re f /Me scn 60 0 10 10 re f",
        pattern_res(vec![("Out", outer), ("Me", selfref_id)]),
        Dictionary::new(),
    );
    let d = page_pdf(b);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 2);
    let o = pat(p[0].fill.as_ref());
    assert_eq!(o.matrix, Matrix::scale(3.0, 3.0));
    let i = pat(paths(&o.pattern.elements)[0].fill.as_ref());
    // Inner pattern matrix is relative to the outer pattern space.
    assert_eq!(i.matrix, Matrix::scale(0.5, 0.5));
    assert_eq!(paths(&i.pattern.elements)[0].fill, Some(Color::Gray(0.0)));
    assert_ne!(i.pattern.name, o.pattern.name);
    // The self-referencing pattern terminates: its cell gets a flat colour.
    let me = pat(p[1].fill.as_ref());
    assert!(paths(&me.pattern.elements)[0].fill.as_ref().is_some_and(Color::is_solid));
    assert!(d.warnings.iter().any(|w| w.contains("uses itself")), "{:?}", d.warnings);
}

#[test]
fn malformed_patterns_fall_back_with_warnings() {
    let mut b = PdfBuilder::new();
    let no_bbox = b.stream(dictionary! { "PatternType" => 1, "PaintType" => 1, "XStep" => 5, "YStep" => 5 }, "0 0 1 1 re f", false);
    let zero_step = b.stream(tiling(1, [0.0, 0.0, 5.0, 5.0], (0.0, 5.0), None, None), "0 0 1 1 re f", false);
    let not_stream = b.add(tiling(1, [0.0, 0.0, 5.0, 5.0], (5.0, 5.0), None, None));
    let no_type = b.add(dictionary! { "Shading" => 5 });
    let bad_shading = b.add(dictionary! { "PatternType" => 2, "Shading" => dictionary! { "ShadingType" => 2 } });
    let no_shading = b.add(dictionary! { "PatternType" => 2 });
    let uncoloured = b.stream(tiling(2, [0.0, 0.0, 5.0, 5.0], (5.0, 5.0), None, None), "0 0 1 1 re f", false);
    let flat_matrix = b.stream(tiling(1, [0.0, 0.0, 5.0, 5.0], (5.0, 5.0), Some([1.0, 2.0, 2.0, 4.0, 0.0, 0.0]), None), "0 0 1 1 re f", false);
    let mut res = pattern_res(vec![
        ("A", no_bbox),
        ("B", zero_step),
        ("C", not_stream),
        ("D", no_type),
        ("E", bad_shading),
        ("F", no_shading),
        ("U", uncoloured),
        ("Z", flat_matrix),
    ]);
    res.get_mut(b"Pattern").unwrap().as_dict_mut().unwrap().set("G", 42);
    b.page(
        "/Pattern cs /A scn 0 0 1 1 re f /B scn 0 0 1 1 re f /C scn 0 0 1 1 re f /D scn 0 0 1 1 re f \
         /E scn 0 0 1 1 re f /F scn 0 0 1 1 re f /G scn 0 0 1 1 re f /Missing scn 0 0 1 1 re f \
         1 0 0 scn 0 0 1 1 re f 0.3 /U scn 0 0 1 1 re f /U scn 0 0 1 1 re f /Z scn 0 0 1 1 re f",
        res,
        Dictionary::new(),
    );
    let d = page_pdf(b);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 12);
    for x in &p {
        assert!(x.fill.as_ref().is_some_and(Color::is_solid), "{:?}", x.fill);
    }
    assert!(d.warnings.iter().any(|w| w.contains("/A") && w.contains("BBox")), "{:?}", d.warnings);
    assert!(d.warnings.iter().any(|w| w.contains("/B") && w.contains("XStep")), "{:?}", d.warnings);
    assert!(d.warnings.iter().any(|w| w.contains("/U") && w.contains("underlying")), "{:?}", d.warnings);
    assert!(d.warnings.iter().any(|w| w.contains("/Missing")), "{:?}", d.warnings);
    assert!(d.warnings.iter().any(|w| w.contains("/Z") && w.contains("degenerate")), "{:?}", d.warnings);
    assert!(d.warnings.len() >= 9, "{:?}", d.warnings);
    assert!(!d.warnings.iter().any(|w| w.contains("approximated with a flat colour") && w.contains("ShadingType 2")));
}

#[test]
fn pattern_space_without_pattern_paints_nothing() {
    let d = parse("/Pattern cs 0 0 m 1 1 l f");
    assert_eq!(d.path_count(), 0);
}

#[test]
fn gs_matrix_noise_snapped_but_real_scales_kept() {
    let mut b = PdfBuilder::new();
    let noisy = b.stream(tiling(1, [0.0, 0.0, 20.0, 20.0], (20.0, 20.0), Some([0.99998, 0.0, 0.0, 0.99998, 0.0, 0.0]), None), "0 0 1 1 re f", false);
    let real = b.stream(tiling(1, [0.0, 0.0, 20.0, 20.0], (20.0, 20.0), Some([0.99, 0.0, 0.0, 0.99, 0.0, 0.0]), None), "0 0 1 1 re f", false);
    b.page("/Pattern cs /N scn 0 0 5 5 re f /R scn 0 0 5 5 re f", pattern_res(vec![("N", noisy), ("R", real)]), Dictionary::new());
    let d = page_pdf(b);
    let p = paths(&d.elements);
    assert_eq!(pat(p[0].fill.as_ref()).matrix, Matrix::IDENTITY);
    assert!(approx(pat(p[1].fill.as_ref()).matrix.a, 0.99));
}

// --- shadings -------------------------------------------------------------------

fn axial(cs: Object, coords: [f64; 4], func: Object) -> Dictionary {
    dictionary! { "ShadingType" => 2, "ColorSpace" => cs, "Coords" => nums_arr(&coords), "Function" => func }
}

fn radial(coords: [f64; 6], func: Object) -> Dictionary {
    dictionary! { "ShadingType" => 3, "ColorSpace" => "DeviceRGB", "Coords" => nums_arr(&coords), "Function" => func }
}

fn shading_pattern_doc(sh: Dictionary, matrix: Option<[f64; 6]>, content: &str) -> Document {
    let mut b = PdfBuilder::new();
    let mut pd = dictionary! { "PatternType" => 2, "Shading" => sh };
    if let Some(m) = matrix {
        pd.set("Matrix", nums_arr(&m));
    }
    let id = b.add(pd);
    b.page(content, pattern_res(vec![("Sh", id)]), Dictionary::new());
    page_pdf(b)
}

#[test]
fn axial_shading_pattern() {
    let d = shading_pattern_doc(
        axial(name("DeviceRGB"), [0.0, 0.0, 100.0, 0.0], type2(&[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0])),
        Some([1.0, 0.0, 0.0, 1.0, 10.0, 5.0]),
        "2 0 0 2 0 0 cm /Pattern cs /Sh scn 0 0 50 20 re f",
    );
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    let g = grad(only_path(&d).fill.as_ref());
    assert_eq!(g.gradient.kind, GradientKind::Linear);
    assert_eq!((g.start, g.end), (Point::new(0.0, 0.0), Point::new(100.0, 0.0)));
    // Pattern matrix x base (the page cm does not apply).
    assert_eq!(g.matrix, Matrix::translate(10.0, 5.0));
    assert_eq!(stops(g), vec![(0.0, Color::Rgb(1.0, 0.0, 0.0)), (1.0, Color::Rgb(0.0, 0.0, 1.0))]);
}

#[test]
fn shading_stops_from_stitching_bounds_and_domain() {
    let func = Object::Dictionary(dictionary! {
        "FunctionType" => 3,
        "Domain" => vec![0.into(), 1.into()],
        "Bounds" => vec![Object::Real(0.25), Object::Real(0.75)],
        "Encode" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        "Functions" => vec![type2(&[0.0], &[1.0]), type2(&[1.0], &[1.0]), type2(&[0.0], &[0.5])],
    });
    let mut sh = axial(name("DeviceGray"), [0.0, 0.0, 10.0, 0.0], func);
    // Extend flags do not change the result.
    sh.set("Extend", vec![true.into(), false.into()]);
    let d = shading_pattern_doc(sh, None, "/Pattern cs /Sh scn 0 0 5 5 re f");
    let g = grad(only_path(&d).fill.as_ref());
    let s = stops(g);
    // 0..0.25 ramps 0->1, flat 1 to 0.75, then jumps to 0 and ramps to 0.5.
    let offsets: Vec<f64> = s.iter().map(|x| x.0).collect();
    assert_eq!(offsets, vec![0.0, 0.25, 0.75, 0.75, 1.0], "{s:?}");
    let grays: Vec<f64> = s.iter().map(|x| if let Color::Gray(v) = x.1 { v } else { panic!() }).collect();
    assert!(approx(grays[0], 0.0) && approx(grays[1], 1.0) && approx(grays[2], 1.0) && approx(grays[3], 0.0) && approx(grays[4], 0.5), "{grays:?}");

    // A non-default /Domain maps onto 0..1.
    let mut sh = axial(name("DeviceGray"), [0.0, 0.0, 10.0, 0.0], Object::Dictionary(dictionary! {
        "FunctionType" => 2, "Domain" => vec![0.into(), 2.into()], "C0" => vec![0.into()], "C1" => vec![1.into()], "N" => 1,
    }));
    sh.set("Domain", vec![1.into(), 2.into()]);
    let d = shading_pattern_doc(sh, None, "/Pattern cs /Sh scn 0 0 5 5 re f");
    let g = grad(only_path(&d).fill.as_ref());
    // Exponential with N=1 over 0..2 evaluated on 1..2: x^1 clipped to Domain
    // [0 2] gives 1..2 -> C0 + x (C1 - C0) clamped by the gray space.
    assert_eq!(g.gradient.stops.len(), 2);
    assert_eq!(g.gradient.stops[0].offset, 0.0);
    assert_eq!(g.gradient.stops[1].offset, 1.0);
}

#[test]
fn nonlinear_shading_function_sampled() {
    let func = Object::Dictionary(dictionary! {
        "FunctionType" => 2, "Domain" => vec![0.into(), 1.into()], "C0" => vec![0.into()], "C1" => vec![1.into()], "N" => 2,
    });
    let d = shading_pattern_doc(axial(name("DeviceGray"), [0.0, 0.0, 10.0, 0.0], func), None, "/Pattern cs /Sh scn 0 0 5 5 re f");
    let g = grad(only_path(&d).fill.as_ref());
    let n = g.gradient.stops.len();
    assert!(n > 4 && n <= 33, "{n}");
    for i in 0..=20 {
        let t = i as f64 / 20.0;
        let Color::Gray(v) = g.gradient.color_at(t) else { panic!() };
        assert!((v - t * t).abs() < 0.01, "t={t} v={v}");
    }
}

#[test]
fn radial_shading_mappings() {
    let f = || type2(&[1.0, 1.0, 1.0], &[0.0, 0.0, 0.0]);
    // Point start circle inside the end circle: exact focal gradient.
    let d = shading_pattern_doc(radial([40.0, 50.0, 0.0, 50.0, 50.0, 20.0], f()), None, "/Pattern cs /Sh scn 0 0 100 100 re f");
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    let g = grad(only_path(&d).fill.as_ref());
    assert_eq!(g.gradient.kind, GradientKind::Radial);
    assert_eq!((g.start, g.end, g.radius), (Point::new(40.0, 50.0), Point::new(50.0, 50.0), 20.0));
    assert_eq!(stops(g), vec![(0.0, Color::Rgb(1.0, 1.0, 1.0)), (1.0, Color::Rgb(0.0, 0.0, 0.0))]);

    // Concentric with r0 > 0: offsets become radius fractions (exact).
    let d = shading_pattern_doc(radial([50.0, 50.0, 10.0, 50.0, 50.0, 40.0], f()), None, "/Pattern cs /Sh scn 0 0 100 100 re f");
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    let g = grad(only_path(&d).fill.as_ref());
    assert_eq!((g.start, g.end, g.radius), (Point::new(50.0, 50.0), Point::new(50.0, 50.0), 40.0));
    let s = stops(g);
    assert!(approx(s[0].0, 0.25) && approx(s[1].0, 1.0), "{s:?}");

    // Shrinking circles (r0 > r1): the start circle is the gradient circle
    // and the stops are reversed.
    let d = shading_pattern_doc(radial([50.0, 50.0, 40.0, 50.0, 50.0, 0.0], f()), None, "/Pattern cs /Sh scn 0 0 100 100 re f");
    let g = grad(only_path(&d).fill.as_ref());
    assert_eq!(g.radius, 40.0);
    assert_eq!(stops(g), vec![(0.0, Color::Rgb(0.0, 0.0, 0.0)), (1.0, Color::Rgb(1.0, 1.0, 1.0))]);

    // Non-nested circles: approximated with a warning, focal kept inside.
    let d = shading_pattern_doc(radial([0.0, 0.0, 5.0, 50.0, 0.0, 20.0], f()), None, "/Pattern cs /Sh scn 0 0 100 100 re f");
    assert!(d.warnings.iter().any(|w| w.contains("radial") && w.contains("approximated")), "{:?}", d.warnings);
    let g = grad(only_path(&d).fill.as_ref());
    assert!((g.start.x - g.end.x).hypot(g.start.y - g.end.y) < g.radius);
}

#[test]
fn separation_shading_gives_spot_stops() {
    let sep = Object::Array(vec![name("Separation"), name("PANTONE 300 C"), name("DeviceCMYK"), type2(&[0.0; 4], &[1.0, 0.4, 0.0, 0.0])]);
    let d = shading_pattern_doc(axial(sep, [0.0, 0.0, 0.0, 10.0], type2(&[0.2], &[1.0])), None, "/Pattern cs /Sh scn 0 0 5 5 re f");
    let g = grad(only_path(&d).fill.as_ref());
    let s = stops(g);
    assert_eq!(s.len(), 2);
    for (st, tint) in s.iter().zip([0.2, 1.0]) {
        let Color::Spot { name, tint: t, cmyk } = &st.1 else { panic!("{:?}", st.1) };
        assert_eq!(name, "PANTONE 300 C");
        assert!(approx(*t, tint));
        assert!(approx(cmyk[1], 0.4));
    }
}

#[test]
fn sh_fills_clip_bbox_or_page() {
    let mut b = PdfBuilder::new();
    let sh = b.add(axial(name("DeviceGray"), [0.0, 0.0, 10.0, 0.0], type2(&[0.0], &[1.0])));
    let mut with_bbox = axial(name("DeviceGray"), [0.0, 0.0, 10.0, 0.0], type2(&[0.0], &[1.0]));
    with_bbox.set("BBox", rect_arr(0.0, 0.0, 10.0, 10.0));
    let shb = b.add(with_bbox);
    b.page(
        "q 10 10 50 50 re W n 2 0 0 2 0 0 cm /Sh sh Q q 1 0 0 1 100 0 cm /ShB sh Q /Sh sh",
        dictionary! { "Shading" => dictionary! { "Sh" => sh, "ShB" => shb } },
        Dictionary::new(),
    );
    let d = page_pdf(b);
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    // The clip is redundant once the clip area itself is painted.
    assert_eq!(d.elements.len(), 3, "{:?}", d.elements);
    let p = paths(&d.elements);
    // 1) the clip region, gradient in the CTM at `sh`.
    let b0 = ai8_converter::ir::bounds_of(&d.elements[..1]).unwrap();
    assert_eq!((b0.llx, b0.lly, b0.urx, b0.ury), (10.0, 10.0, 60.0, 60.0));
    assert_eq!(grad(p[0].fill.as_ref()).matrix, Matrix::scale(2.0, 2.0));
    // 2) the shading /BBox in user space.
    let b1 = ai8_converter::ir::bounds_of(&d.elements[1..2]).unwrap();
    assert_eq!((b1.llx, b1.lly, b1.urx, b1.ury), (100.0, 0.0, 110.0, 10.0));
    assert_eq!(grad(p[1].fill.as_ref()).matrix, Matrix::translate(100.0, 0.0));
    // 3) no clip: the whole page.
    let b2 = ai8_converter::ir::bounds_of(&d.elements[2..]).unwrap();
    assert_eq!((b2.llx, b2.lly, b2.urx, b2.ury), (0.0, 0.0, 200.0, 100.0));
    // One shared gradient definition per shading.
    assert!(Arc::ptr_eq(&grad(p[0].fill.as_ref()).gradient, &grad(p[2].fill.as_ref()).gradient));
}

#[test]
fn sh_with_non_rectangular_clip_keeps_clip_group() {
    let mut b = PdfBuilder::new();
    let sh = b.add(radial([50.0, 50.0, 0.0, 50.0, 50.0, 30.0], type2(&[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0])));
    b.page(
        "q 20 20 m 80 20 l 50 80 l h W n /Sh sh Q",
        dictionary! { "Shading" => dictionary! { "Sh" => sh } },
        Dictionary::new(),
    );
    let d = page_pdf(b);
    assert!(matches!(d.elements[0], Element::ClipGroup { .. }), "{:?}", d.elements);
    let p = only_path(&d);
    // The painted path is the triangle itself.
    assert_eq!(p.subpaths[0].start, Point::new(20.0, 20.0));
    assert_eq!(p.subpaths[0].segments.len(), 2);
    assert_eq!(grad(p.fill.as_ref()).gradient.kind, GradientKind::Radial);
}

#[test]
fn unsupported_shadings_fall_back_to_flat_colour() {
    let mut b = PdfBuilder::new();
    let mesh = b.stream(dictionary! { "ShadingType" => 4, "ColorSpace" => "DeviceRGB", "BitsPerCoordinate" => 8, "BitsPerComponent" => 8, "BitsPerFlag" => 8 }, "", false);
    let func_based = b.add(dictionary! {
        "ShadingType" => 1, "ColorSpace" => "DeviceGray",
        "Function" => Object::Dictionary(dictionary! { "FunctionType" => 2, "Domain" => vec![0.into(), 1.into()], "C0" => vec![0.into()], "C1" => vec![1.into()], "N" => 1 }),
    });
    let broken = b.add(dictionary! { "ShadingType" => 2, "ColorSpace" => "DeviceGray", "Coords" => rect_arr(0.0, 0.0, 0.0, 0.0), "Function" => type2(&[0.0], &[1.0]) });
    b.page(
        "/M sh /F sh /B sh /Nope sh",
        dictionary! { "Shading" => dictionary! { "M" => mesh, "F" => func_based, "B" => broken } },
        Dictionary::new(),
    );
    let d = page_pdf(b);
    let p = paths(&d.elements);
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].fill, Some(Color::Gray(0.5)));
    assert_eq!(p[1].fill, Some(Color::Gray(0.5)));
    assert_eq!(p[2].fill, Some(Color::Gray(0.5)));
    assert!(d.warnings.iter().any(|w| w.contains("ShadingType 4")), "{:?}", d.warnings);
    assert!(d.warnings.iter().any(|w| w.contains("ShadingType 1")), "{:?}", d.warnings);
    assert!(d.warnings.iter().any(|w| w.contains("zero length")), "{:?}", d.warnings);
    assert!(d.warnings.iter().any(|w| w.contains("/Nope")), "{:?}", d.warnings);
}

#[test]
fn patterns_and_gradients_survive_ai8_writer() {
    let mut b = PdfBuilder::new();
    let p0 = b.stream(tiling(1, [0.0, 0.0, 4.0, 4.0], (4.0, 4.0), None, None), "1 0 0 rg 0 0 2 2 re f", false);
    let sh = b.add(dictionary! { "PatternType" => 2, "Shading" => axial(name("DeviceRGB"), [0.0, 0.0, 10.0, 0.0], type2(&[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0])) });
    b.page("/Pattern cs /P0 scn 0 0 50 50 re f /Sh scn 60 0 50 50 re f", pattern_res(vec![("P0", p0), ("Sh", sh)]), Dictionary::new());
    let d = page_pdf(b);
    let out = ai(&d);
    assert!(out.contains("%%EOF"));
}

#[test]
fn unknown_colour_space_falls_back() {
    let d = parse("/Missing cs 0.5 sc 0 0 m 1 1 l f");
    assert_eq!(only_path(&d).fill, Some(Color::Gray(0.5)));
    assert!(!d.warnings.is_empty());
}

// --- clipping ------------------------------------------------------------------

#[test]
fn clip_applies_after_paint_and_restores() {
    let d = parse(
        "q 10 10 m 90 10 l 50 90 l h W n 0 0 200 100 re f 0 0 m 200 100 l S Q 0 0 5 5 re f",
    );
    assert_eq!(d.elements.len(), 2);
    let Element::ClipGroup { clip, even_odd, children } = &d.elements[0] else { panic!("{:?}", d.elements) };
    assert!(!even_odd);
    assert_eq!(clip.len(), 1);
    assert!(clip[0].closed);
    assert_eq!(children.len(), 2);
    assert!(matches!(d.elements[1], Element::Path(_)));
}

#[test]
fn clip_path_is_also_painted() {
    // "re W f": the rectangle is filled AND becomes the clip.
    let d = parse("q 0 0 m 50 0 l 25 50 l h W f 0 0 100 100 re f Q");
    let Element::Path(_) = &d.elements[0] else { panic!("{:?}", d.elements) };
    let Element::ClipGroup { children, .. } = &d.elements[1] else { panic!() };
    assert_eq!(children.len(), 1);
}

#[test]
fn nested_clips_and_even_odd() {
    let d = parse(
        "q 0 0 m 100 0 l 50 80 l h W n q 0 0 100 100 re 25 25 50 50 re W* n 0 0 200 100 re f Q 0 0 200 100 re f Q",
    );
    assert_eq!(d.elements.len(), 1);
    let Element::ClipGroup { even_odd: false, children, .. } = &d.elements[0] else { panic!() };
    assert_eq!(children.len(), 2);
    let Element::ClipGroup { even_odd: true, clip, children: inner } = &children[0] else { panic!("{children:?}") };
    assert_eq!(clip.len(), 2);
    assert_eq!(inner.len(), 1);
    assert!(matches!(children[1], Element::Path(_)));
}

#[test]
fn successive_clips_in_same_gstate_nest() {
    let d = parse("q 0 0 m 100 0 l 50 80 l h W n 0 0 m 80 0 l 40 60 l h W n 0 0 200 100 re f Q");
    let Element::ClipGroup { children, .. } = &d.elements[0] else { panic!() };
    let Element::ClipGroup { children: inner, .. } = &children[0] else { panic!() };
    assert_eq!(inner.len(), 1);
}

#[test]
fn clip_without_q_lasts_to_end() {
    let d = parse("0 0 m 100 0 l 50 80 l h W n 0 0 200 100 re f 0 0 10 10 re f");
    assert_eq!(d.elements.len(), 1);
    let Element::ClipGroup { children, .. } = &d.elements[0] else { panic!() };
    assert_eq!(children.len(), 2);
}

#[test]
fn redundant_page_clip_removed() {
    let d = parse("0 0 200 100 re W n 10 10 20 20 re f 1 w 50 50 m 60 60 l S");
    assert_eq!(d.elements.len(), 2);
    assert!(d.elements.iter().all(|e| matches!(e, Element::Path(_))));
    // A rect clip that does cut the content is kept.
    let d = parse("20 20 50 50 re W n 0 0 200 100 re f");
    assert!(matches!(d.elements[0], Element::ClipGroup { .. }));
}

// --- form XObjects ----------------------------------------------------------------

#[test]
fn form_xobject_matrix_resources_group() {
    let mut b = PdfBuilder::new();
    let form = b.stream(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => rect_arr(0.0, 0.0, 100.0, 100.0),
            "Matrix" => vec![2.into(), 0.into(), 0.into(), 2.into(), 10.into(), 10.into()],
            "Resources" => cs_res(vec![("Own", Object::Array(vec![name("Separation"), name("Spot1"), name("DeviceCMYK"), type2(&[0.0; 4], &[1.0, 0.0, 0.0, 0.0])]))]),
        },
        "/Own cs 1 scn 0 0 10 10 re f 0 g 20 20 10 10 re f",
        true,
    );
    let res = dictionary! { "XObject" => dictionary! { "Fm0" => form } };
    b.page("q 1 0 0 1 5 0 cm /Fm0 Do Q 0 0 m 1 1 l S", res, Dictionary::new());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 500.0, 500.0) }), 0).unwrap();
    assert_eq!(d.elements.len(), 2, "{:?}", d.elements);
    let Element::Group(children) = &d.elements[0] else { panic!("{:?}", d.elements) };
    assert_eq!(children.len(), 2);
    let Element::Path(p0) = &children[0] else { panic!() };
    // (0,0) -> form matrix (10,10) -> cm (15,10); size 10 -> 20.
    assert_eq!(p0.subpaths[0].start, Point::new(15.0, 10.0));
    assert_eq!(p0.subpaths[0].segments[1], Segment::Line(Point::new(35.0, 30.0)));
    assert!(matches!(&p0.fill, Some(Color::Spot { name, .. }) if name == "Spot1"));
    let Element::Path(p1) = &children[1] else { panic!() };
    assert_eq!(p1.fill, Some(Color::Gray(0.0)));
    // Colour state does not leak out of the form (implicit q/Q).
    let Element::Path(after) = &d.elements[1] else { panic!() };
    assert_eq!(after.stroke.as_ref().unwrap().color, Color::Gray(0.0));
}

#[test]
fn form_single_child_not_grouped_and_parent_resources_used() {
    let mut b = PdfBuilder::new();
    let form = b.stream(
        dictionary! { "Subtype" => "Form", "BBox" => rect_arr(0.0, 0.0, 50.0, 50.0) },
        "/C cs 1 sc 0 0 10 10 re f",
        false,
    );
    let mut res = cs_res(vec![("C", Object::Array(vec![name("Separation"), name("PSpot"), name("DeviceCMYK"), type2(&[0.0; 4], &[0.0, 0.0, 1.0, 0.0])]))]);
    res.set("XObject", dictionary! { "X" => form });
    b.page("/X Do", res, Dictionary::new());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 100.0, 100.0) }), 0).unwrap();
    assert_eq!(d.elements.len(), 1);
    let Element::Path(p) = &d.elements[0] else { panic!("{:?}", d.elements) };
    assert!(matches!(&p.fill, Some(Color::Spot { name, .. }) if name == "PSpot"));
}

#[test]
fn form_bbox_clips_overflowing_content() {
    let mut b = PdfBuilder::new();
    let form = b.stream(
        dictionary! { "Subtype" => "Form", "BBox" => rect_arr(0.0, 0.0, 10.0, 10.0) },
        "0 0 50 50 re f",
        false,
    );
    b.page("/X Do", dictionary! { "XObject" => dictionary! { "X" => form } }, Dictionary::new());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 100.0, 100.0) }), 0).unwrap();
    let Element::ClipGroup { clip, children, .. } = &d.elements[0] else { panic!("{:?}", d.elements) };
    assert_eq!(clip[0].segments[1], Segment::Line(Point::new(10.0, 10.0)));
    assert_eq!(children.len(), 1);
}

#[test]
fn form_unbalanced_q_and_clip_contained() {
    let mut b = PdfBuilder::new();
    let form = b.stream(
        dictionary! { "Subtype" => "Form", "BBox" => rect_arr(0.0, 0.0, 100.0, 100.0) },
        "Q Q 1 0 0 rg 5 5 50 50 re W n 0 0 100 100 re f q 2 0 0 2 0 0 cm",
        false,
    );
    b.page("/X Do 0 0 m 10 10 l S", dictionary! { "XObject" => dictionary! { "X" => form } }, Dictionary::new());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 100.0, 100.0) }), 0).unwrap();
    assert_eq!(d.elements.len(), 2, "{:?}", d.elements);
    assert!(matches!(d.elements[0], Element::ClipGroup { .. }));
    let Element::Path(p) = &d.elements[1] else { panic!() };
    assert_eq!(p.subpaths[0].segments[0], Segment::Line(Point::new(10.0, 10.0)));
    assert_eq!(p.stroke.as_ref().unwrap().color, Color::Gray(0.0));
}

#[test]
fn recursive_forms_do_not_loop() {
    let mut b = PdfBuilder::new();
    let form_id = b.doc.new_object_id();
    let res = dictionary! { "XObject" => dictionary! { "Self" => form_id } };
    let form = Stream::new(
        dictionary! { "Subtype" => "Form", "BBox" => rect_arr(0.0, 0.0, 100.0, 100.0), "Resources" => res.clone() },
        b"0 0 10 10 re f /Self Do".to_vec(),
    );
    b.doc.objects.insert(form_id, Object::Stream(form));
    b.page("/Self Do", res, Dictionary::new());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 100.0, 100.0) }), 0).unwrap();
    assert_eq!(d.path_count(), 1);
    assert!(d.warnings.iter().any(|w| w.contains("recursive")));
}

// --- skipped content ----------------------------------------------------------

#[test]
fn images_skipped_with_single_warning() {
    let mut b = PdfBuilder::new();
    let img = b.stream(
        dictionary! { "Subtype" => "Image", "Width" => 1, "Height" => 1, "BitsPerComponent" => 8, "ColorSpace" => "DeviceGray" },
        "\u{0}",
        false,
    );
    b.page(
        "q 10 0 0 10 0 0 cm /Im0 Do Q /Im0 Do BI /W 1 /H 1 /BPC 8 /CS /G ID \x7f EI 0 0 5 5 re f",
        dictionary! { "XObject" => dictionary! { "Im0" => img } },
        Dictionary::new(),
    );
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 100.0, 100.0) }), 0).unwrap();
    assert_eq!(d.path_count(), 1);
    let w: Vec<_> = d.warnings.iter().filter(|w| w.contains("raster image")).collect();
    assert_eq!(w.len(), 1);
    assert!(w[0].starts_with("3 raster images were skipped"), "{w:?}");
}

#[test]
fn shading_and_text_warn_once() {
    let d = parse("/Sh0 sh /Sh0 sh BT /F1 12 Tf 0 0 Td (Hello) Tj [(a) 10 (b)] TJ (x) ' 1 2 (y) \" ET 0 0 5 5 re f");
    assert_eq!(d.path_count(), 1);
    assert_eq!(d.warnings.len(), 2, "{:?}", d.warnings);
    assert!(d.warnings.iter().any(|w| w.contains("shading")));
    assert!(d.warnings.iter().any(|w| w.contains("text")));
}

#[test]
fn marked_content_and_compat_ignored() {
    let d = parse("/OC /MC0 BDC BX 1 2 3 weirdop EX /Artifact BMC 0 0 5 5 re f EMC EMC /Span << /MCID 0 >> BDC EMC 1 i /Perceptual ri");
    assert_eq!(d.path_count(), 1);
    assert!(d.warnings.is_empty());
}

// --- page geometry ----------------------------------------------------------------

fn rotated(rotate: i64) -> Document {
    let mut b = PdfBuilder::new();
    b.page("0 0 10 10 re f", Dictionary::new(), dictionary! { "Rotate" => rotate });
    parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 200.0, 100.0) }), 0).unwrap()
}

#[test]
fn rotate_pages_upright() {
    // Square at the bottom-left of a 200x100 page.
    let d = rotated(90);
    assert_eq!(d.page, Rect { llx: 0.0, lly: 0.0, urx: 100.0, ury: 200.0 });
    let b = d.content_bounds().unwrap();
    assert_eq!((b.llx, b.lly, b.urx, b.ury), (0.0, 190.0, 10.0, 200.0));

    let d = rotated(180);
    assert_eq!(d.page, Rect { llx: 0.0, lly: 0.0, urx: 200.0, ury: 100.0 });
    let b = d.content_bounds().unwrap();
    assert_eq!((b.llx, b.lly, b.urx, b.ury), (190.0, 90.0, 200.0, 100.0));

    let d = rotated(270);
    assert_eq!(d.page, Rect { llx: 0.0, lly: 0.0, urx: 100.0, ury: 200.0 });
    let b = d.content_bounds().unwrap();
    assert_eq!((b.llx, b.lly, b.urx, b.ury), (90.0, 0.0, 100.0, 10.0));

    let d = rotated(-90);
    assert_eq!(d.content_bounds().unwrap().llx, 90.0);
}

#[test]
fn rotate_inherited_and_line_width_unaffected() {
    let mut b = PdfBuilder::new();
    b.page("3 w 0 0 m 10 0 l S", Dictionary::new(), Dictionary::new());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 200.0, 100.0), "Rotate" => 90 }), 0).unwrap();
    assert_eq!(d.page.width(), 100.0);
    assert!(approx(only_path(&d).stroke.as_ref().unwrap().width, 3.0));
}

#[test]
fn cropbox_used_as_page() {
    let mut b = PdfBuilder::new();
    b.page(
        "0 0 5 5 re f",
        Dictionary::new(),
        dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 600.0, 800.0), "CropBox" => rect_arr(50.0, 60.0, 250.0, 160.0) },
    );
    let d = parse_pdf(&b.finish(Dictionary::new()), 0).unwrap();
    assert_eq!(d.page, Rect { llx: 50.0, lly: 60.0, urx: 250.0, ury: 160.0 });
    // Geometry stays in PDF user space.
    assert_eq!(only_path(&d).subpaths[0].start, Point::new(0.0, 0.0));

    // CropBox with rotation: page rect moves to origin, geometry follows.
    let mut b = PdfBuilder::new();
    b.page(
        "50 60 10 10 re f",
        Dictionary::new(),
        dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 600.0, 800.0), "CropBox" => rect_arr(50.0, 60.0, 250.0, 160.0), "Rotate" => 180 },
    );
    let d = parse_pdf(&b.finish(Dictionary::new()), 0).unwrap();
    assert_eq!(d.page, Rect { llx: 0.0, lly: 0.0, urx: 200.0, ury: 100.0 });
    let bb = d.content_bounds().unwrap();
    assert_eq!((bb.llx, bb.lly, bb.urx, bb.ury), (190.0, 90.0, 200.0, 100.0));
}

#[test]
fn inherited_mediabox_and_resources() {
    let mut b = PdfBuilder::new();
    let content = b.stream(Dictionary::new(), "/C cs 1 sc 0 0 5 5 re f", false);
    let mid_id = b.doc.new_object_id();
    let page_id = b.add(dictionary! { "Type" => "Page", "Parent" => mid_id, "Contents" => content });
    b.doc.objects.insert(
        mid_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Parent" => b.pages_id,
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "Resources" => cs_res(vec![("C", Object::Array(vec![name("Separation"), name("Inherited"), name("DeviceCMYK"), type2(&[0.0; 4], &[0.0, 0.0, 0.0, 1.0])]))]),
        }),
    );
    b.kids.push(mid_id.into());
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 321.0, 123.0) }), 0).unwrap();
    assert_eq!(d.page, Rect { llx: 0.0, lly: 0.0, urx: 321.0, ury: 123.0 });
    assert!(matches!(&only_path(&d).fill, Some(Color::Spot { name, .. }) if name == "Inherited"));
}

#[test]
fn multi_page_and_content_arrays() {
    let mut b = PdfBuilder::new();
    b.page("0 0 1 1 re f", Dictionary::new(), Dictionary::new());
    // Page 2: content split across two (one compressed) streams.
    let c1 = b.stream(Dictionary::new(), "0 0 m 10 0 l", true);
    let c2 = b.stream(Dictionary::new(), "10 10 l h 1 0 0 rg f", false);
    let p2 = b.add(dictionary! { "Type" => "Page", "Parent" => b.pages_id, "Contents" => vec![c1.into(), c2.into()] });
    b.kids.push(p2.into());
    b.page("", Dictionary::new(), Dictionary::new());
    let pdf = b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 100.0, 100.0) });
    assert_eq!(page_count(&pdf).unwrap(), 3);
    let d = parse_pdf(&pdf, 1).unwrap();
    let p = only_path(&d);
    assert_eq!(p.fill, Some(Color::Rgb(1.0, 0.0, 0.0)));
    assert_eq!(p.subpaths[0].segments.len(), 2);
    assert!(parse_pdf(&pdf, 2).unwrap().elements.is_empty());
    assert!(parse_pdf(&pdf, 3).is_err());
}

// --- robustness --------------------------------------------------------------------

#[test]
fn malformed_streams_do_not_panic() {
    let samples = [
        "",
        "q q q q",
        "Q Q Q Q W n W* f",
        "1 2 3 4 5 cm 0 0 m 1 l S",
        "/Name /Other ( unterminated",
        "<< /A [1 2 3 >> ] ] }} {{ 0 0 m 1 1 l S",
        "0 0 m 1e5 1 l S 1.2.3 4 l --5 6 l S",
        "/X Do /Y gs /Z cs 1 sc /P scn /Q sh",
        "BI /W 1 /H 1 ID",
        "[1 2 3] d [] d d 10 w w J j M",
        "1 0 0 1 0 0 cm 0 0 0 0 0 0 cm 0 0 m 10 10 l S 1 0 0 1 nan inf cm",
    ];
    for s in samples {
        let d = parse(s);
        // Output stays writable.
        ai(&d);
    }
    // Pseudo-random byte soup built from content-stream fragments.
    let frags: &[&str] = &[
        "q", "Q", "0", "1", "-3.5", "m", "l", "c", "v", "y", "h", "re", "f", "S", "B*", "W", "W*", "n", "cm", "rg",
        "/CS0", "cs", "scn", "gs", "Do", "[", "]", "(", ")", "<", ">", "<<", ">>", "BI", "ID", "EI", "%", "\n", "d",
    ];
    let mut seed: u64 = 0x1234_5678;
    for _ in 0..200 {
        let mut s = String::new();
        for _ in 0..120 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            s.push_str(frags[(seed >> 33) as usize % frags.len()]);
            s.push(' ');
        }
        let d = parse(&s);
        ai(&d);
    }
}

#[test]
fn malformed_files_return_errors() {
    assert!(parse_pdf(b"", 0).is_err());
    assert!(parse_pdf(b"%PDF-1.4\n%%EOF", 0).is_err());
    assert!(page_count(b"<svg/>").is_err());
    let pdf = one_page("0 0 10 10 re f", Dictionary::new());
    for cut in [10, pdf.len() / 2, pdf.len() - 30] {
        // Truncated files may or may not be recoverable, but must not panic.
        let _ = parse_pdf(&pdf[..cut], 0);
        let _ = page_count(&pdf[..cut]);
    }
}

// --- end to end ------------------------------------------------------------------

#[test]
fn end_to_end_ai8() {
    let mut b = PdfBuilder::new();
    let form = b.stream(
        dictionary! { "Subtype" => "Form", "BBox" => rect_arr(0.0, 0.0, 200.0, 200.0) },
        "1 0 0 rg 0 0 20 20 re f 0 0 1 rg 30 0 20 20 re f",
        false,
    );
    let sep = Object::Array(vec![name("Separation"), name("CutContour"), name("DeviceCMYK"), type2(&[0.0; 4], &[0.0, 1.0, 0.0, 0.0])]);
    let mut res = cs_res(vec![("CS0", sep)]);
    res.set("XObject", dictionary! { "Fm" => form });
    b.page(
        "q 10 10 100 50 re W n 0 0 300 300 re f Q \
         /CS0 CS 1 SCN 0.25 w 5 5 m 100 5 l 100 80 l h S \
         0 0 100 100 re 25 25 50 50 re f* \
         q 1 0 0 1 120 20 cm /Fm Do Q",
        res,
        Dictionary::new(),
    );
    let d = parse_pdf(&b.finish(dictionary! { "MediaBox" => rect_arr(0.0, 0.0, 300.0, 200.0) }), 0).unwrap();
    let out = ai(&d);
    let stats = common::validate_ai8(&out);
    assert_eq!(stats.clip_groups, 1);
    assert_eq!(stats.groups, 1);
    assert_eq!(stats.compound_paths, 1);
    assert!(out.contains("%%DocumentCustomColors: (CutContour)"));
    assert!(out.contains("%%CMYKCustomColor: 0 1 0 0 (CutContour)"));
    assert!(out.contains("0 1 0 0 (CutContour) 0 X"));
    assert!(out.contains("h W"));
    assert!(out.contains("*u") && out.contains("*U"));
    assert!(out.contains("1 XR"));
    assert!(out.contains("0.25 w"));
    assert!(out.lines().any(|l| l == "u") && out.lines().any(|l| l == "U"));
    assert!(out.contains("%AI5_ArtSize: 300 200"));
}
