//! Content-stream interpreter producing IR elements.
//!
//! Clipping model: the output is built in a stack of open "frames". The root
//! frame collects top-level elements. Setting a clip pushes a `Clip` frame
//! tagged with the current graphics-state level (the depth of the q/Q stack);
//! everything painted afterwards lands inside it. When `Q` restores to a
//! lower level, every frame above that level is closed and turned into an
//! `Element::ClipGroup` inside its parent. Form XObjects push a `Group` frame
//! the same way (with an implicit q/Q around them).

use super::color::{ColorSpace, parse_colorspace};
use super::function::Function;
use super::lexer::{Lexer, Operand, Tok};
use super::{deref, dict_of, name_str, obj_num, obj_nums};
use crate::ir::{Color, Element, LineCap, LineJoin, Matrix, PathItem, Point, Rect, Segment, Stroke, Subpath};
use lopdf::{Dictionary, Document as PdfDoc, Object, ObjectId};
use std::collections::HashMap;
use std::rc::Rc;

const MAX_FORM_DEPTH: usize = 32;
const MAX_OPERANDS: usize = 4096;
const MAX_GSTATE_DEPTH: usize = 4096;
/// Decompressed size limit for one content stream.
pub const MAX_STREAM: usize = 512 << 20;

#[derive(Clone)]
struct Paint {
    cs: Rc<ColorSpace>,
    color: Option<Color>,
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    fill: Paint,
    stroke: Paint,
    line_width: f64,
    cap: LineCap,
    join: LineJoin,
    miter: f64,
    dash: Vec<f64>,
    dash_phase: f64,
    fill_op: bool,
    stroke_op: bool,
}

enum FrameKind {
    Root,
    Clip { clip: Vec<Subpath>, even_odd: bool },
    Group,
}

struct Frame {
    kind: FrameKind,
    level: usize,
    elements: Vec<Element>,
}

pub struct Interp<'a> {
    doc: &'a PdfDoc,
    gs: GState,
    gstack: Vec<GState>,
    /// `Q` never pops below this level (protects form XObject boundaries).
    q_floor: usize,
    frames: Vec<Frame>,
    path: Vec<Subpath>,
    pending_clip: Option<bool>,
    warnings: Vec<String>,
    images: usize,
    form_stack: Vec<ObjectId>,
    form_depth: usize,
    cs_cache: HashMap<usize, Rc<ColorSpace>>,
    gray: Rc<ColorSpace>,
    rgb: Rc<ColorSpace>,
    cmyk: Rc<ColorSpace>,
}

fn nums(args: &[Operand]) -> Vec<f64> {
    args.iter().filter_map(Operand::as_num).collect()
}

/// The last `n` numeric operands, or `None` if there are fewer.
fn last_n(args: &[Operand], n: usize) -> Option<Vec<f64>> {
    let v = nums(args);
    if v.len() < n { None } else { Some(v[v.len() - n..].to_vec()) }
}

fn cap_of(v: f64) -> LineCap {
    match v as i64 {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    }
}

fn join_of(v: f64) -> LineJoin {
    match v as i64 {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    }
}

/// If `clip` is a single axis-aligned rectangle, return it.
fn axis_rect(clip: &[Subpath]) -> Option<Rect> {
    let [sp] = clip else { return None };
    let mut pts = vec![sp.start];
    for s in &sp.segments {
        match s {
            Segment::Line(p) => pts.push(*p),
            Segment::Curve(..) => return None,
        }
    }
    if pts.len() == 5 && (pts[4].x - pts[0].x).abs() < 1e-6 && (pts[4].y - pts[0].y).abs() < 1e-6 {
        pts.pop();
    }
    if pts.len() != 4 {
        return None;
    }
    for i in 0..4 {
        let (a, b) = (pts[i], pts[(i + 1) % 4]);
        if (a.x - b.x).abs() > 1e-6 && (a.y - b.y).abs() > 1e-6 {
            return None;
        }
    }
    let r = Rect {
        llx: pts.iter().map(|p| p.x).fold(f64::INFINITY, f64::min),
        lly: pts.iter().map(|p| p.y).fold(f64::INFINITY, f64::min),
        urx: pts.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max),
        ury: pts.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max),
    };
    Some(r)
}

/// Conservative painted bounds of elements (strokes expanded).
fn painted_bounds(els: &[Element], r: &mut Option<Rect>) {
    for e in els {
        match e {
            Element::Path(p) => {
                let pad = p.stroke.as_ref().map_or(0.0, |s| {
                    let f = if s.join == LineJoin::Miter { s.miter_limit.max(1.5) } else { 1.5 };
                    s.width / 2.0 * f
                });
                for sp in &p.subpaths {
                    for pt in sp.points() {
                        let b = r.get_or_insert(Rect { llx: pt.x, lly: pt.y, urx: pt.x, ury: pt.y });
                        b.llx = b.llx.min(pt.x - pad);
                        b.lly = b.lly.min(pt.y - pad);
                        b.urx = b.urx.max(pt.x + pad);
                        b.ury = b.ury.max(pt.y + pad);
                    }
                }
            }
            Element::Group(c) => painted_bounds(c, r),
            // A nested clip group never paints outside its own clip.
            Element::ClipGroup { clip, children, .. } => {
                let mut inner = None;
                painted_bounds(children, &mut inner);
                if let Some(rect) = axis_rect(clip)
                    && let Some(i) = inner.as_mut()
                {
                    i.llx = i.llx.max(rect.llx);
                    i.lly = i.lly.max(rect.lly);
                    i.urx = i.urx.min(rect.urx);
                    i.ury = i.ury.min(rect.ury);
                }
                if let Some(i) = inner {
                    let b = r.get_or_insert(i);
                    b.llx = b.llx.min(i.llx);
                    b.lly = b.lly.min(i.lly);
                    b.urx = b.urx.max(i.urx);
                    b.ury = b.ury.max(i.ury);
                }
            }
        }
    }
}

impl<'a> Interp<'a> {
    pub fn new(doc: &'a PdfDoc, base: Matrix) -> Self {
        let gray = Rc::new(ColorSpace::Gray);
        let gs = GState {
            ctm: base,
            fill: Paint { cs: gray.clone(), color: Some(Color::Gray(0.0)) },
            stroke: Paint { cs: gray.clone(), color: Some(Color::Gray(0.0)) },
            line_width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 10.0,
            dash: Vec::new(),
            dash_phase: 0.0,
            fill_op: false,
            stroke_op: false,
        };
        Self {
            doc,
            gs,
            gstack: Vec::new(),
            q_floor: 0,
            frames: vec![Frame { kind: FrameKind::Root, level: 0, elements: Vec::new() }],
            path: Vec::new(),
            pending_clip: None,
            warnings: Vec::new(),
            images: 0,
            form_stack: Vec::new(),
            form_depth: 0,
            cs_cache: HashMap::new(),
            gray,
            rgb: Rc::new(ColorSpace::Rgb),
            cmyk: Rc::new(ColorSpace::Cmyk),
        }
    }

    pub fn warn(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }

    /// Close all frames and return the elements and warnings.
    pub fn finish(mut self) -> (Vec<Element>, Vec<String>) {
        while self.frames.len() > 1 {
            self.close_top();
        }
        if self.images > 0 {
            let n = self.images;
            self.warn(format!(
                "{n} raster image{} {} skipped (only vector artwork is converted)",
                if n == 1 { "" } else { "s" },
                if n == 1 { "was" } else { "were" }
            ));
        }
        let root = self.frames.pop().map(|f| f.elements).unwrap_or_default();
        (root, self.warnings)
    }

    // --- element tree ------------------------------------------------------

    fn push_element(&mut self, e: Element) {
        if let Some(f) = self.frames.last_mut() {
            f.elements.push(e);
        }
    }

    fn close_top(&mut self) {
        if self.frames.len() <= 1 {
            return;
        }
        let Some(f) = self.frames.pop() else { return };
        let mut els = f.elements;
        match f.kind {
            FrameKind::Root => {}
            FrameKind::Group => match els.len() {
                0 => {}
                1 => {
                    if let Some(e) = els.pop() {
                        self.push_element(e);
                    }
                }
                _ => self.push_element(Element::Group(els)),
            },
            FrameKind::Clip { clip, even_odd } => {
                if els.is_empty() {
                    return;
                }
                // Drop rectangular clips that clip nothing.
                if let Some(rect) = axis_rect(&clip) {
                    let mut b = None;
                    painted_bounds(&els, &mut b);
                    if let Some(b) = b {
                        let eps = 0.01;
                        if b.llx >= rect.llx - eps
                            && b.lly >= rect.lly - eps
                            && b.urx <= rect.urx + eps
                            && b.ury <= rect.ury + eps
                        {
                            for e in els {
                                self.push_element(e);
                            }
                            return;
                        }
                    }
                }
                self.push_element(Element::ClipGroup { clip, even_odd, children: els });
            }
        }
    }

    fn close_frames_above(&mut self, level: usize) {
        while self.frames.len() > 1 && self.frames.last().is_some_and(|f| f.level > level) {
            self.close_top();
        }
    }

    fn push_clip(&mut self, clip: Vec<Subpath>, even_odd: bool) {
        let level = self.gstack.len();
        self.frames.push(Frame { kind: FrameKind::Clip { clip, even_odd }, level, elements: Vec::new() });
    }

    // --- running -----------------------------------------------------------

    pub fn run(&mut self, data: &[u8], res: Option<&'a Dictionary>) {
        let mut lx = Lexer::new(data);
        let mut args: Vec<Operand> = Vec::new();
        loop {
            match lx.next_tok() {
                Tok::End => break,
                Tok::Operand(o) => {
                    if args.len() < MAX_OPERANDS {
                        args.push(o);
                    }
                }
                Tok::Op(op) => {
                    self.op(op, &args, res);
                    args.clear();
                }
                Tok::InlineImage => {
                    self.images += 1;
                    args.clear();
                }
            }
        }
    }

    fn pt(&self, x: f64, y: f64) -> Point {
        self.gs.ctm.apply(Point::new(x, y))
    }

    fn op(&mut self, op: &[u8], args: &[Operand], res: Option<&'a Dictionary>) {
        match op {
            // --- graphics state
            b"q" => {
                if self.gstack.len() < MAX_GSTATE_DEPTH {
                    self.gstack.push(self.gs.clone());
                }
            }
            b"Q" => {
                if self.gstack.len() > self.q_floor {
                    if let Some(g) = self.gstack.pop() {
                        self.gs = g;
                    }
                    let level = self.gstack.len();
                    self.close_frames_above(level);
                }
            }
            b"cm" => {
                if let Some(v) = last_n(args, 6) {
                    let m = Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]);
                    self.gs.ctm = m.then(&self.gs.ctm);
                }
            }
            b"w" => {
                if let Some(v) = last_n(args, 1) {
                    self.gs.line_width = v[0].abs();
                }
            }
            b"J" => {
                if let Some(v) = last_n(args, 1) {
                    self.gs.cap = cap_of(v[0]);
                }
            }
            b"j" => {
                if let Some(v) = last_n(args, 1) {
                    self.gs.join = join_of(v[0]);
                }
            }
            b"M" => {
                if let Some(v) = last_n(args, 1) {
                    self.gs.miter = v[0].max(1.0);
                }
            }
            b"d" => {
                if let Some(Operand::Array(a)) = args.iter().find(|a| matches!(a, Operand::Array(_))) {
                    self.gs.dash = a.iter().filter_map(Operand::as_num).map(f64::abs).collect();
                    self.gs.dash_phase = args.iter().rev().find_map(Operand::as_num).unwrap_or(0.0);
                }
            }
            b"gs" => {
                if let Some(name) = args.last().and_then(Operand::as_name) {
                    self.ext_gstate(name, res);
                }
            }
            // --- path construction
            b"m" => {
                if let Some(v) = last_n(args, 2) {
                    let p = self.pt(v[0], v[1]);
                    self.move_to(p);
                }
            }
            b"l" => {
                if let Some(v) = last_n(args, 2) {
                    let p = self.pt(v[0], v[1]);
                    self.line_to(p);
                }
            }
            b"c" => {
                if let Some(v) = last_n(args, 6) {
                    let (a, b, c) = (self.pt(v[0], v[1]), self.pt(v[2], v[3]), self.pt(v[4], v[5]));
                    self.curve_to(Some(a), b, c);
                }
            }
            b"v" => {
                if let Some(v) = last_n(args, 4) {
                    let (b, c) = (self.pt(v[0], v[1]), self.pt(v[2], v[3]));
                    self.curve_to(None, b, c);
                }
            }
            b"y" => {
                if let Some(v) = last_n(args, 4) {
                    let (a, c) = (self.pt(v[0], v[1]), self.pt(v[2], v[3]));
                    self.curve_to(Some(a), c, c);
                }
            }
            b"h" => self.close_path(),
            b"re" => {
                if let Some(v) = last_n(args, 4) {
                    let (x, y, w, h) = (v[0], v[1], v[2], v[3]);
                    let p0 = self.pt(x, y);
                    let mut sp = Subpath::new(p0);
                    sp.segments.push(Segment::Line(self.pt(x + w, y)));
                    sp.segments.push(Segment::Line(self.pt(x + w, y + h)));
                    sp.segments.push(Segment::Line(self.pt(x, y + h)));
                    sp.closed = true;
                    self.drop_trailing_empty();
                    self.path.push(sp);
                }
            }
            // --- painting
            b"S" => self.paint(false, true, false, false),
            b"s" => self.paint(false, true, false, true),
            b"f" | b"F" => self.paint(true, false, false, false),
            b"f*" => self.paint(true, false, true, false),
            b"B" => self.paint(true, true, false, false),
            b"B*" => self.paint(true, true, true, false),
            b"b" => self.paint(true, true, false, true),
            b"b*" => self.paint(true, true, true, true),
            b"n" => self.paint(false, false, false, false),
            b"W" => self.pending_clip = Some(false),
            b"W*" => self.pending_clip = Some(true),
            // --- colour
            b"g" | b"G" | b"rg" | b"RG" | b"k" | b"K" => {
                let stroke = op[0].is_ascii_uppercase();
                let (cs, n) = match op[op.len() - 1].to_ascii_lowercase() {
                    b'g' if op.len() == 1 => (self.gray.clone(), 1),
                    b'g' => (self.rgb.clone(), 3),
                    _ => (self.cmyk.clone(), 4),
                };
                if let Some(v) = last_n(args, n) {
                    let color = cs.to_color(&v);
                    let p = if stroke { &mut self.gs.stroke } else { &mut self.gs.fill };
                    *p = Paint { cs, color };
                }
            }
            b"cs" | b"CS" => {
                if let Some(name) = args.last().and_then(Operand::as_name) {
                    let cs = self.resolve_cs(name, res);
                    let color = cs.to_color(&cs.initial());
                    let p = if op == b"CS" { &mut self.gs.stroke } else { &mut self.gs.fill };
                    *p = Paint { cs, color };
                }
            }
            b"sc" | b"scn" | b"SC" | b"SCN" => self.set_color(op[0] == b'S', args, res),
            // --- XObjects, images, shadings
            b"Do" => {
                if let Some(name) = args.last().and_then(Operand::as_name) {
                    self.do_xobject(name, res);
                }
            }
            b"sh" => self.warn("smooth shadings (gradients) were skipped"),
            // --- text
            b"Tj" | b"TJ" | b"'" | b"\"" => self.warn(
                "text was not converted (it is not outlined); route the file through Ghostscript to convert text to outlines",
            ),
            // Everything else (text state, marked content, BX/EX, d0/d1, ri, i...) is ignored.
            _ => {}
        }
    }

    // --- paths ---------------------------------------------------------------

    /// Remove a trailing subpath that consists of a lone moveto.
    fn drop_trailing_empty(&mut self) {
        if self.path.last().is_some_and(|s| s.segments.is_empty() && !s.closed) {
            self.path.pop();
        }
    }

    fn move_to(&mut self, p: Point) {
        self.drop_trailing_empty();
        self.path.push(Subpath::new(p));
    }

    /// The subpath that new segments are appended to, starting a new one
    /// after a closepath. `None` if there is no current point.
    fn open_subpath(&mut self) -> Option<&mut Subpath> {
        let last = self.path.last()?;
        if last.closed {
            let start = last.start;
            self.path.push(Subpath::new(start));
        }
        self.path.last_mut()
    }

    fn current_point(&self) -> Option<Point> {
        let sp = self.path.last()?;
        if sp.closed {
            return Some(sp.start);
        }
        Some(match sp.segments.last() {
            Some(Segment::Line(p)) | Some(Segment::Curve(_, _, p)) => *p,
            None => sp.start,
        })
    }

    fn line_to(&mut self, p: Point) {
        match self.open_subpath() {
            Some(sp) => sp.segments.push(Segment::Line(p)),
            None => self.move_to(p),
        }
    }

    fn curve_to(&mut self, c1: Option<Point>, c2: Point, end: Point) {
        let Some(cur) = self.current_point() else {
            self.move_to(end);
            return;
        };
        let c1 = c1.unwrap_or(cur);
        if let Some(sp) = self.open_subpath() {
            sp.segments.push(Segment::Curve(c1, c2, end));
        }
    }

    fn close_path(&mut self) {
        if let Some(sp) = self.path.last_mut()
            && !sp.segments.is_empty()
        {
            sp.closed = true;
        }
    }

    fn paint(&mut self, fill: bool, stroke: bool, even_odd: bool, close: bool) {
        if close {
            self.close_path();
        }
        let mut subs: Vec<Subpath> = std::mem::take(&mut self.path).into_iter().filter(|s| !s.segments.is_empty()).collect();
        let clip = self.pending_clip.take();
        if subs.is_empty() {
            return;
        }
        let fill_c = if fill { self.gs.fill.color.clone() } else { None };
        let stroke_s = if stroke {
            self.gs.stroke.color.clone().map(|color| {
                let scale = self.gs.ctm.mean_scale();
                let dash: Vec<f64> = if self.gs.dash.iter().all(|d| *d <= 0.0) {
                    Vec::new()
                } else {
                    self.gs.dash.iter().map(|d| d * scale).collect()
                };
                Stroke {
                    color,
                    width: self.gs.line_width * scale,
                    cap: self.gs.cap,
                    join: self.gs.join,
                    miter_limit: self.gs.miter,
                    dash_phase: if dash.is_empty() { 0.0 } else { self.gs.dash_phase * scale },
                    dash,
                }
            })
        } else {
            None
        };
        if fill_c.is_some() || stroke_s.is_some() {
            let geometry = if clip.is_some() { subs.clone() } else { std::mem::take(&mut subs) };
            let mut item = PathItem::new(geometry);
            item.even_odd = even_odd;
            item.fill_overprint = fill_c.is_some() && self.gs.fill_op;
            item.stroke_overprint = stroke_s.is_some() && self.gs.stroke_op;
            item.fill = fill_c;
            item.stroke = stroke_s;
            self.push_element(Element::Path(item));
        }
        if let Some(eo) = clip {
            let clip_subs: Vec<Subpath> = subs
                .into_iter()
                .map(|mut s| {
                    s.closed = true;
                    s
                })
                .collect();
            self.push_clip(clip_subs, eo);
        }
    }

    // --- resources -------------------------------------------------------------

    fn lookup(&self, res: Option<&'a Dictionary>, category: &[u8], name: &[u8]) -> Option<&'a Object> {
        let res = res?;
        let cat = deref(self.doc, res.get(category).ok()?);
        let dict = dict_of(self.doc, cat)?;
        dict.get(name).ok().map(|o| deref(self.doc, o))
    }

    fn resolve_cs(&mut self, name: &[u8], res: Option<&'a Dictionary>) -> Rc<ColorSpace> {
        match name {
            b"DeviceGray" | b"G" => return self.gray.clone(),
            b"DeviceRGB" | b"RGB" => return self.rgb.clone(),
            b"DeviceCMYK" | b"CMYK" => return self.cmyk.clone(),
            b"Pattern" => return Rc::new(ColorSpace::Pattern { base: None }),
            _ => {}
        }
        let Some(obj) = self.lookup(res, b"ColorSpace", name) else {
            self.warn(format!("colour space /{} not found; using gray", name_str(name)));
            return self.gray.clone();
        };
        self.parse_cs_cached(obj, name)
    }

    fn parse_cs_cached(&mut self, obj: &'a Object, label: &[u8]) -> Rc<ColorSpace> {
        let key = obj as *const Object as usize;
        if let Some(cs) = self.cs_cache.get(&key) {
            return cs.clone();
        }
        let mut w = Vec::new();
        let cs = match parse_colorspace(self.doc, obj, &mut w) {
            Ok(cs) => Rc::new(cs),
            Err(e) => {
                w.push(format!("colour space /{}: {e}; using gray", name_str(label)));
                self.gray.clone()
            }
        };
        for m in w {
            self.warn(m);
        }
        self.cs_cache.insert(key, cs.clone());
        cs
    }

    fn set_color(&mut self, stroke: bool, args: &[Operand], res: Option<&'a Dictionary>) {
        let cs = if stroke { self.gs.stroke.cs.clone() } else { self.gs.fill.cs.clone() };
        let color = if let ColorSpace::Pattern { base } = &*cs {
            self.warn("pattern fills approximated with a flat colour");
            let comps = nums(args);
            let pat = args.last().and_then(Operand::as_name).and_then(|n| self.lookup(res, b"Pattern", n));
            Some(self.pattern_color(pat, base.as_deref(), &comps).unwrap_or(Color::Gray(0.5)))
        } else {
            let n = cs.n_comps();
            let mut v = nums(args);
            if v.is_empty() {
                return;
            }
            if v.len() > n {
                v.drain(..v.len() - n);
            }
            v.resize(n, 0.0);
            cs.to_color(&v)
        };
        let p = if stroke { &mut self.gs.stroke } else { &mut self.gs.fill };
        p.color = color;
    }

    /// Approximate a pattern by a flat colour.
    fn pattern_color(&mut self, pat: Option<&'a Object>, base: Option<&ColorSpace>, comps: &[f64]) -> Option<Color> {
        let dict = dict_of(self.doc, pat?)?;
        let ptype = dict.get(b"PatternType").ok().and_then(|o| obj_num(deref(self.doc, o)))? as i64;
        match ptype {
            1 => {
                let paint_type = dict.get(b"PaintType").ok().and_then(|o| obj_num(deref(self.doc, o))).unwrap_or(1.0);
                if paint_type as i64 == 2 {
                    let base = base?;
                    let n = base.n_comps();
                    if comps.len() < n {
                        return None;
                    }
                    base.to_color(&comps[comps.len() - n..])
                } else {
                    None
                }
            }
            2 => {
                let sh = dict_of(self.doc, deref(self.doc, dict.get(b"Shading").ok()?))?;
                self.shading_mid_color(sh)
            }
            _ => None,
        }
    }

    /// Colour at the middle of a shading's parameter domain.
    fn shading_mid_color(&mut self, sh: &'a Dictionary) -> Option<Color> {
        let cs_obj = deref(self.doc, sh.get(b"ColorSpace").ok()?);
        let cs = self.parse_cs_cached(cs_obj, b"shading");
        let f = Function::parse(self.doc, sh.get(b"Function").ok()?)?;
        let stype = sh.get(b"ShadingType").ok().and_then(|o| obj_num(deref(self.doc, o))).unwrap_or(2.0) as i64;
        let domain = sh.get(b"Domain").ok().and_then(|o| obj_nums(self.doc, o));
        let input = if stype == 1 {
            let d = domain.filter(|d| d.len() >= 4).unwrap_or_else(|| vec![0.0, 1.0, 0.0, 1.0]);
            vec![(d[0] + d[1]) / 2.0, (d[2] + d[3]) / 2.0]
        } else {
            let d = domain.filter(|d| d.len() >= 2).unwrap_or_else(|| vec![0.0, 1.0]);
            vec![(d[0] + d[1]) / 2.0]
        };
        let out = f.eval(&input)?;
        if out.len() < cs.n_comps() {
            return None;
        }
        cs.to_color(&out)
    }

    fn ext_gstate(&mut self, name: &[u8], res: Option<&'a Dictionary>) {
        let Some(d) = self.lookup(res, b"ExtGState", name).and_then(|o| dict_of(self.doc, o)) else {
            return;
        };
        let doc = self.doc;
        let num = |k: &[u8]| d.get(k).ok().and_then(|o| obj_num(deref(doc, o)));
        let boolean = |k: &[u8]| d.get(k).ok().and_then(|o| deref(doc, o).as_bool().ok());
        if let Some(v) = num(b"LW") {
            self.gs.line_width = v.abs();
        }
        if let Some(v) = num(b"LC") {
            self.gs.cap = cap_of(v);
        }
        if let Some(v) = num(b"LJ") {
            self.gs.join = join_of(v);
        }
        if let Some(v) = num(b"ML") {
            self.gs.miter = v.max(1.0);
        }
        if let Ok(Object::Array(dash)) = d.get(b"D").map(|o| deref(doc, o))
            && let Some(arr) = dash.first().and_then(|o| obj_nums(doc, o))
        {
            self.gs.dash = arr.into_iter().map(f64::abs).collect();
            self.gs.dash_phase = dash.get(1).and_then(|o| obj_num(deref(doc, o))).unwrap_or(0.0);
        }
        let op_stroke = boolean(b"OP");
        let op_fill = boolean(b"op");
        if let Some(v) = op_stroke {
            self.gs.stroke_op = v;
            if op_fill.is_none() {
                self.gs.fill_op = v;
            }
        }
        if let Some(v) = op_fill {
            self.gs.fill_op = v;
        }
        let transparent = num(b"CA").is_some_and(|v| v < 1.0)
            || num(b"ca").is_some_and(|v| v < 1.0)
            || d.get(b"SMask").ok().map(|o| deref(doc, o)).is_some_and(|o| !matches!(o, Object::Name(n) if n == b"None"));
        if transparent {
            self.warn("transparency (opacity / soft masks) is not supported in Illustrator 8 and was ignored");
        }
    }

    // --- XObjects ------------------------------------------------------------

    fn do_xobject(&mut self, name: &[u8], res: Option<&'a Dictionary>) {
        let Some(raw) = res.and_then(|r| r.get(b"XObject").ok()).map(|o| deref(self.doc, o)).and_then(|o| dict_of(self.doc, o)).and_then(|d| d.get(name).ok())
        else {
            return;
        };
        let id = raw.as_reference().ok();
        let Object::Stream(stream) = deref(self.doc, raw) else { return };
        let subtype = stream.dict.get(b"Subtype").ok().and_then(|o| deref(self.doc, o).as_name().ok());
        match subtype {
            Some(b"Image") => self.images += 1,
            Some(b"Form") => self.run_form(id, stream, res),
            _ => {}
        }
    }

    fn run_form(&mut self, id: Option<ObjectId>, stream: &'a lopdf::Stream, parent_res: Option<&'a Dictionary>) {
        if let Some(id) = id
            && self.form_stack.contains(&id)
        {
            self.warn("a recursive form XObject was skipped");
            return;
        }
        if self.form_depth >= MAX_FORM_DEPTH {
            self.warn("form XObjects nested too deeply were skipped");
            return;
        }
        let content = match stream.get_plain_content_with_limit(MAX_STREAM) {
            Ok(c) => c,
            Err(e) => {
                self.warn(format!("could not decode a form XObject: {e}"));
                return;
            }
        };
        let d = &stream.dict;
        let matrix = d
            .get(b"Matrix")
            .ok()
            .and_then(|o| obj_nums(self.doc, o))
            .filter(|v| v.len() >= 6)
            .map(|v| Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]))
            .unwrap_or(Matrix::IDENTITY);
        let bbox = d.get(b"BBox").ok().and_then(|o| obj_nums(self.doc, o)).filter(|v| v.len() >= 4);
        let res = d.get(b"Resources").ok().and_then(|o| dict_of(self.doc, deref(self.doc, o))).or(parent_res);

        // Implicit q.
        let saved_len = self.gstack.len();
        let saved_floor = self.q_floor;
        let saved_path = std::mem::take(&mut self.path);
        let saved_clip = self.pending_clip.take();
        self.gstack.push(self.gs.clone());
        self.q_floor = self.gstack.len();
        self.gs.ctm = matrix.then(&self.gs.ctm);
        let level = self.gstack.len();
        self.frames.push(Frame { kind: FrameKind::Group, level, elements: Vec::new() });
        if let Some(b) = bbox {
            let (x0, y0, x1, y1) = (b[0].min(b[2]), b[1].min(b[3]), b[0].max(b[2]), b[1].max(b[3]));
            let mut sp = Subpath::new(self.pt(x0, y0));
            sp.segments.push(Segment::Line(self.pt(x1, y0)));
            sp.segments.push(Segment::Line(self.pt(x1, y1)));
            sp.segments.push(Segment::Line(self.pt(x0, y1)));
            sp.closed = true;
            self.push_clip(vec![sp], false);
        }
        if let Some(id) = id {
            self.form_stack.push(id);
        }
        self.form_depth += 1;

        self.run(&content, res);

        self.form_depth -= 1;
        if id.is_some() {
            self.form_stack.pop();
        }
        // Implicit Q.
        if self.gstack.len() > saved_len {
            self.gs = self.gstack[saved_len].clone();
            self.gstack.truncate(saved_len);
        }
        self.close_frames_above(saved_len);
        self.q_floor = saved_floor;
        self.path = saved_path;
        self.pending_clip = saved_clip;
    }
}
