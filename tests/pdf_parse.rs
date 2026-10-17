//! Integration tests for the PDF content-stream interpreter (`pdf::parse_pdf`).

mod common;

use ai8_converter::ir::{Color, Document, Element, LineCap, LineJoin, PathItem, Point, Rect, Segment};
use ai8_converter::pdf::{page_count, parse_pdf};
use ai8_converter::{WriteOptions, write_ai8};
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};

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

#[test]
fn pattern_fill_approximated() {
    let res = dictionary! {
        "Pattern" => dictionary! {
            "P0" => dictionary! {
                "PatternType" => 2,
                "Shading" => dictionary! {
                    "ShadingType" => 2,
                    "ColorSpace" => "DeviceRGB",
                    "Coords" => rect_arr(0.0, 0.0, 1.0, 0.0),
                    "Function" => type2(&[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0]),
                },
            },
            "P1" => dictionary! { "PatternType" => 1, "PaintType" => 1 },
            "P2" => dictionary! { "PatternType" => 1, "PaintType" => 2 },
        },
        "ColorSpace" => dictionary! { "UP" => vec![name("Pattern"), name("DeviceCMYK")] },
    };
    let d = parse_res(
        "/Pattern cs /P0 scn 0 0 m 1 1 l f /P1 scn 0 0 m 1 1 l f /UP cs 0 1 0 0 /P2 scn 0 0 m 1 1 l f /Pattern cs 0 0 m 1 1 l f",
        res,
    );
    let p = paths(&d.elements);
    assert_eq!(p.len(), 3, "a pattern space without a pattern paints nothing");
    let Some(Color::Rgb(r, g, b)) = p[0].fill else { panic!() };
    assert!(approx(r, 0.5) && approx(g, 0.0) && approx(b, 0.5));
    assert_eq!(p[1].fill, Some(Color::Gray(0.5)));
    assert_eq!(p[2].fill, Some(Color::Cmyk(0.0, 1.0, 0.0, 0.0)));
    assert_eq!(d.warnings.iter().filter(|w| w.contains("pattern")).count(), 1);
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
