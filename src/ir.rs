//! Intermediate representation shared by all input parsers and the AI8 writer.
//!
//! All coordinates are in PostScript points (1/72 inch) with the origin at the
//! bottom-left of the page and the y axis pointing up, i.e. already transformed
//! into final page space. Parsers are responsible for applying their own CTMs.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Line(Point),
    /// Cubic Bézier: control 1, control 2, end point.
    Curve(Point, Point, Point),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Subpath {
    pub start: Point,
    pub segments: Vec<Segment>,
    pub closed: bool,
}

impl Subpath {
    pub fn new(start: Point) -> Self {
        Self { start, segments: Vec::new(), closed: false }
    }

    pub fn points(&self) -> impl Iterator<Item = Point> + '_ {
        std::iter::once(self.start).chain(self.segments.iter().flat_map(|s| match s {
            Segment::Line(p) => vec![*p],
            Segment::Curve(a, b, c) => vec![*a, *b, *c],
        }))
    }
}

/// Process / spot colours. All components are in 0.0..=1.0.
#[derive(Debug, Clone, PartialEq)]
pub enum Color {
    /// 0 = black, 1 = white (PostScript convention).
    Gray(f64),
    Rgb(f64, f64, f64),
    Cmyk(f64, f64, f64, f64),
    /// Named spot colour (e.g. "CutContour"). `cmyk` is the alternate
    /// process approximation of 100% ink; `tint` is 0 (no ink) ..= 1 (full ink).
    Spot { name: String, cmyk: [f64; 4], tint: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub color: Color,
    /// Width in points (already scaled by the CTM).
    pub width: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f64,
    /// Dash array in points (already scaled) and phase.
    pub dash: Vec<f64>,
    pub dash_phase: f64,
}

impl Stroke {
    pub fn solid(color: Color, width: f64) -> Self {
        Self {
            color,
            width,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_phase: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathItem {
    pub subpaths: Vec<Subpath>,
    pub fill: Option<Color>,
    pub stroke: Option<Stroke>,
    /// true = even-odd fill rule, false = non-zero winding.
    pub even_odd: bool,
    pub fill_overprint: bool,
    pub stroke_overprint: bool,
}

impl PathItem {
    pub fn new(subpaths: Vec<Subpath>) -> Self {
        Self {
            subpaths,
            fill: None,
            stroke: None,
            even_odd: false,
            fill_overprint: false,
            stroke_overprint: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    Path(PathItem),
    Group(Vec<Element>),
    /// Children are clipped by `clip` (a path with no paint).
    ClipGroup { clip: Vec<Subpath>, even_odd: bool, children: Vec<Element> },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub llx: f64,
    pub lly: f64,
    pub urx: f64,
    pub ury: f64,
}

impl Rect {
    pub fn width(&self) -> f64 {
        self.urx - self.llx
    }
    pub fn height(&self) -> f64 {
        self.ury - self.lly
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// Page / artboard size in points (origin bottom-left).
    pub page: Rect,
    pub elements: Vec<Element>,
    /// Non-fatal problems found while parsing (unsupported features etc.).
    pub warnings: Vec<String>,
}

impl Document {
    pub fn new(page: Rect) -> Self {
        Self { page, elements: Vec::new(), warnings: Vec::new() }
    }

    /// Tight bounding box of all geometry (control points included; ignores
    /// stroke width). `None` if there is no geometry.
    pub fn content_bounds(&self) -> Option<Rect> {
        fn walk(els: &[Element], r: &mut Option<Rect>) {
            let add = |sp: &[Subpath], r: &mut Option<Rect>| {
                for s in sp {
                    for p in s.points() {
                        let b = r.get_or_insert(Rect { llx: p.x, lly: p.y, urx: p.x, ury: p.y });
                        b.llx = b.llx.min(p.x);
                        b.lly = b.lly.min(p.y);
                        b.urx = b.urx.max(p.x);
                        b.ury = b.ury.max(p.y);
                    }
                }
            };
            for e in els {
                match e {
                    Element::Path(p) => add(&p.subpaths, r),
                    Element::Group(c) => walk(c, r),
                    Element::ClipGroup { children, .. } => walk(children, r),
                }
            }
        }
        let mut r = None;
        walk(&self.elements, &mut r);
        r
    }

    pub fn path_count(&self) -> usize {
        fn count(els: &[Element]) -> usize {
            els.iter()
                .map(|e| match e {
                    Element::Path(_) => 1,
                    Element::Group(c) => count(c),
                    Element::ClipGroup { children, .. } => count(children),
                })
                .sum()
        }
        count(&self.elements)
    }
}

/// 2D affine matrix in PDF/PostScript order [a b c d e f]:
/// x' = a*x + c*y + e, y' = b*x + d*y + f.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    pub fn translate(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// `self` applied first, then `then` (PDF `cm` semantics: new CTM = m × CTM
    /// is `m.then(ctm)`).
    pub fn then(&self, then: &Matrix) -> Matrix {
        Matrix {
            a: self.a * then.a + self.b * then.c,
            b: self.a * then.b + self.b * then.d,
            c: self.c * then.a + self.d * then.c,
            d: self.c * then.b + self.d * then.d,
            e: self.e * then.a + self.f * then.c + then.e,
            f: self.e * then.b + self.f * then.d + then.f,
        }
    }

    pub fn apply(&self, p: Point) -> Point {
        Point::new(self.a * p.x + self.c * p.y + self.e, self.b * p.x + self.d * p.y + self.f)
    }

    /// Uniform scale factor used for line widths and dash lengths.
    pub fn mean_scale(&self) -> f64 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }
}
