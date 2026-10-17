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
    /// Tiling pattern fill/stroke (PDF PatternType 1, SVG <pattern>).
    Pattern(PatternPaint),
    /// Linear or radial gradient (PDF shading types 2/3, SVG gradients).
    Gradient(GradientPaint),
}

impl Color {
    /// Flat process/spot approximation of any paint (patterns: average-ish
    /// colour of the tile art; gradients: middle stop). Never returns a
    /// Pattern or Gradient.
    pub fn flatten(&self) -> Color {
        match self {
            Color::Pattern(p) => p.pattern.representative_color().unwrap_or(Color::Gray(0.5)),
            Color::Gradient(g) => g.gradient.color_at(0.5).flatten(),
            c => c.clone(),
        }
    }

    pub fn is_solid(&self) -> bool {
        !matches!(self, Color::Pattern(_) | Color::Gradient(_))
    }
}

/// A tiling pattern definition, shared between all uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    /// Unique (per document) name, used for the AI pattern swatch.
    pub name: String,
    /// Tile cell in pattern space (PDF /BBox). Art outside it is clipped.
    pub bbox: Rect,
    /// Repeat distances in pattern space (PDF /XStep /YStep). Usually equal to
    /// the bbox size, but may differ (gaps or overlaps between tiles).
    pub x_step: f64,
    pub y_step: f64,
    /// Tile artwork in pattern space with colours fully resolved (for PDF
    /// uncoloured patterns the parser bakes the colour of use into a separate
    /// Pattern instance). May itself contain pattern/gradient paints.
    pub elements: Vec<Element>,
}

impl Pattern {
    /// Colour of the first solid-painted path in the tile, if any.
    pub fn representative_color(&self) -> Option<Color> {
        fn find(els: &[Element]) -> Option<Color> {
            for e in els {
                let c = match e {
                    Element::Path(p) => p
                        .fill
                        .as_ref()
                        .or(p.stroke.as_ref().map(|s| &s.color))
                        .map(|c| c.flatten()),
                    Element::Group(c) => find(c),
                    Element::ClipGroup { children, .. } => find(children),
                };
                if c.is_some() {
                    return c;
                }
            }
            None
        }
        find(&self.elements)
    }
}

/// A use of a pattern: `matrix` maps pattern space to page space (PDF: the
/// pattern /Matrix concatenated with the base CTM of the page/form).
#[derive(Debug, Clone, PartialEq)]
pub struct PatternPaint {
    pub pattern: std::sync::Arc<Pattern>,
    pub matrix: Matrix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientKind {
    /// Axis from `start` to `end`.
    Linear,
    /// Circle centred at `end` with radius `radius`; `start` is the focal
    /// (highlight) point, equal to `end` for a plain radial gradient.
    Radial,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GradientStop {
    /// Position along the gradient, 0..=1, non-decreasing.
    pub offset: f64,
    /// Solid colour (Gray/Rgb/Cmyk/Spot only).
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub kind: GradientKind,
    /// At least two stops, sorted by offset.
    pub stops: Vec<GradientStop>,
}

impl Gradient {
    /// Interpolated colour at `t` (0..=1). Spot/mixed spaces fall back to the
    /// nearest stop.
    pub fn color_at(&self, t: f64) -> Color {
        let Some(first) = self.stops.first() else { return Color::Gray(0.5) };
        let t = t.clamp(0.0, 1.0);
        let mut prev = first;
        for s in &self.stops {
            if s.offset >= t {
                let span = s.offset - prev.offset;
                let u = if span > 1e-12 { (t - prev.offset) / span } else { 1.0 };
                return lerp_color(&prev.color, &s.color, u);
            }
            prev = s;
        }
        prev.color.clone()
    }
}

fn lerp_color(a: &Color, b: &Color, u: f64) -> Color {
    let l = |x: f64, y: f64| x + (y - x) * u;
    match (a, b) {
        (Color::Gray(x), Color::Gray(y)) => Color::Gray(l(*x, *y)),
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => Color::Rgb(l(*r1, *r2), l(*g1, *g2), l(*b1, *b2)),
        (Color::Cmyk(c1, m1, y1, k1), Color::Cmyk(c2, m2, y2, k2)) => {
            Color::Cmyk(l(*c1, *c2), l(*m1, *m2), l(*y1, *y2), l(*k1, *k2))
        }
        (Color::Spot { name: n1, cmyk, tint: t1 }, Color::Spot { name: n2, tint: t2, .. }) if n1 == n2 => {
            Color::Spot { name: n1.clone(), cmyk: *cmyk, tint: l(*t1, *t2) }
        }
        _ => if u < 0.5 { a.clone() } else { b.clone() },
    }
}

/// A use of a gradient. `start`, `end` and `radius` are in gradient space;
/// `matrix` maps gradient space to page space.
#[derive(Debug, Clone, PartialEq)]
pub struct GradientPaint {
    pub gradient: std::sync::Arc<Gradient>,
    pub start: Point,
    pub end: Point,
    pub radius: f64,
    pub matrix: Matrix,
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

// Paths dominate documents, so boxing them would only add indirection.
#[allow(clippy::large_enum_variant)]
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
        bounds_of(&self.elements)
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

fn subpath_bounds(sp: &[Subpath]) -> Option<Rect> {
    let mut r: Option<Rect> = None;
    for p in sp.iter().flat_map(|s| s.points()) {
        let b = r.get_or_insert(Rect { llx: p.x, lly: p.y, urx: p.x, ury: p.y });
        b.llx = b.llx.min(p.x);
        b.lly = b.lly.min(p.y);
        b.urx = b.urx.max(p.x);
        b.ury = b.ury.max(p.y);
    }
    r
}

fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Rect {
            llx: a.llx.min(b.llx),
            lly: a.lly.min(b.lly),
            urx: a.urx.max(b.urx),
            ury: a.ury.max(b.ury),
        }),
        (a, b) => a.or(b),
    }
}

/// Bounds of visible geometry: clip groups contribute the intersection of the
/// clip path and their children (control points included, stroke width ignored).
pub fn bounds_of(els: &[Element]) -> Option<Rect> {
    els.iter().fold(None, |acc, e| {
        let b = match e {
            Element::Path(p) => subpath_bounds(&p.subpaths),
            Element::Group(c) => bounds_of(c),
            Element::ClipGroup { clip, children, .. } => match (subpath_bounds(clip), bounds_of(children)) {
                (Some(c), Some(k)) => {
                    let r = Rect {
                        llx: c.llx.max(k.llx),
                        lly: c.lly.max(k.lly),
                        urx: c.urx.min(k.urx),
                        ury: c.ury.min(k.ury),
                    };
                    (r.llx <= r.urx && r.lly <= r.ury).then_some(r)
                }
                _ => None,
            },
        };
        union(acc, b)
    })
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
