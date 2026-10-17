//! DXF (AutoCAD Drawing Exchange Format) input, ASCII and binary, R12–R2018.
//!
//! The file is read with our own group-code reader ([`reader`]) – the `dxf`
//! crate does not parse HATCH entities, which matter most here.
//!
//! * **Units:** `$INSUNITS` → points; unitless drawings are taken as
//!   millimetres (with a warning). The page is the tight bounds of the
//!   converted geometry, moved so the art starts at (0, 0).
//! * **Spaces:** modelspace entities are converted; the active paperspace
//!   layout is used only if modelspace has nothing drawable.
//! * **Layers:** off / frozen layers are skipped; each layer becomes one
//!   group (layer table order). Block contents stay grouped per INSERT.
//! * **Colours:** ACI (full 256-entry table, 7 = black for print), true
//!   colour (420), BYLAYER / BYBLOCK → [`Color::Rgb`].
//! * **Strokes:** lineweight (370, BYLAYER/BYBLOCK/default → `$LWDEFAULT`),
//!   linetypes (LTYPE dash patterns × `$LTSCALE` × entity scale × insert
//!   scale), constant polyline widths.
//! * **Geometry:** LINE, (LW)POLYLINE incl. bulges, polyface and polygon
//!   meshes, CIRCLE, ARC, ELLIPSE, SPLINE (exact Bézier decomposition for
//!   non-rational degree ≤ 3, fitted otherwise), SOLID/TRACE, 3DFACE, HATCH,
//!   INSERT (scale, rotation, arrays, nesting), DIMENSION and ACAD_TABLE
//!   (their anonymous blocks). OCS / extrusion directions are honoured
//!   (top view projection).
//! * **HATCH:** boundaries (polyline with bulges; line/arc/ellipse/spline
//!   edges) become even-odd compound paths. Solid fills are flat colours,
//!   gradient fills become linear / radial [`Color::Gradient`]s, and pattern
//!   hatches become native [`Color::Pattern`] tiles when the line families
//!   share a translation lattice (ANSI31, ANSI37, crosshatches, bricks, …),
//!   otherwise clipped, dashed lines.
//! * Text (TEXT, MTEXT, attributes, dimension text), leaders, images and 3D
//!   solids are skipped with a warning.

mod color;
mod geom;
mod hatch;
mod model;
mod reader;

pub use reader::looks_like_dxf;

use crate::ir::{
    Color, Document, Element, Gradient, GradientKind, GradientPaint, GradientStop, LineCap, LineJoin, Matrix, PathItem,
    Pattern, PatternPaint, Point, Rect, Stroke, Subpath,
};
use anyhow::{Result, anyhow, bail};
use geom::{DEG, Nurbs, P3, PathBuilder, ccw_sweep, ellipse_minor, ocs_matrix, rotate};
use model::{Drawing, Record};
use std::collections::{BTreeMap, HashMap};
use std::f64::consts::TAU;
use std::sync::Arc;

/// Deepest INSERT nesting followed.
const MAX_BLOCK_DEPTH: usize = 32;
/// Most paths generated for one drawing.
const MAX_PATHS: usize = 2_000_000;
/// Most expanded hatch line segments per HATCH entity.
const MAX_HATCH_LINES: usize = 20_000;
/// Illustrator's largest artboard side (227 inches).
const MAX_PAGE_PT: f64 = 16_383.0;

/// Parse a DXF file into the IR.
pub fn parse_dxf(data: &[u8]) -> Result<Document> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse_inner(data))) {
        Ok(r) => r,
        Err(_) => Err(anyhow!("internal error while reading the DXF file (parser panicked)")),
    }
}

fn parse_inner(data: &[u8]) -> Result<Document> {
    if data.iter().all(|b| b.is_ascii_whitespace()) {
        bail!("the DXF file is empty");
    }
    if !looks_like_dxf(data) {
        bail!("not a DXF file (expected \"0 / SECTION\" or the binary DXF sentinel at the start)");
    }
    let pairs = reader::read_pairs(data)?;
    let drawing = model::parse_structure(&pairs)?;
    drop(pairs);
    Conv::new(&drawing).run()
}

#[derive(Debug, Clone)]
struct Layer {
    color: Color,
    off: bool,
    frozen: bool,
    ltype: String,
    /// Lineweight in 1/100 mm (or -3 = default).
    lw: i64,
}

/// Resolved BYBLOCK properties of the enclosing INSERT.
#[derive(Debug, Clone)]
struct ByBlock {
    color: Color,
    lw_pt: f64,
    /// Dash pattern in drawing units (already × $LTSCALE × entity scale).
    dash: Vec<f64>,
}

#[derive(Debug, Clone)]
struct Ctx {
    /// Drawing units of the current block → points (before the final shift).
    m: Matrix,
    /// Layer of the enclosing INSERT (entities on layer 0 inherit it).
    layer: Option<String>,
    byblock: ByBlock,
    stack: Vec<String>,
}

struct Style {
    color: Color,
    width: f64,
    dash: Vec<f64>,
}

struct Conv<'a> {
    d: &'a Drawing,
    layers: HashMap<String, Layer>,
    layer_order: Vec<String>,
    ltypes: HashMap<String, Vec<f64>>,
    ltscale: f64,
    lw_default: i64,
    cap: LineCap,
    join: LineJoin,
    unit: f64,
    warnings: Vec<String>,
    skipped: BTreeMap<&'static str, usize>,
    unknown: BTreeMap<String, usize>,
    paths: usize,
    patterns: Vec<Arc<Pattern>>,
    flags: Flags,
}

#[derive(Default)]
struct Flags {
    varying_width: bool,
    spline_approx: bool,
    hatch_expanded: bool,
    hatch_truncated: bool,
    hatch_damaged: bool,
    cycle: bool,
    depth: bool,
    budget: bool,
    missing_block: bool,
    xref: bool,
    nonfinite: bool,
    array_capped: bool,
}

fn rgb(c: [u8; 3]) -> Color {
    Color::Rgb(c[0] as f64 / 255.0, c[1] as f64 / 255.0, c[2] as f64 / 255.0)
}

fn true_color(v: i64) -> Color {
    rgb([((v >> 16) & 255) as u8, ((v >> 8) & 255) as u8, (v & 255) as u8])
}

fn lw_to_pt(lw: i64) -> f64 {
    lw.max(0) as f64 / 100.0 * 72.0 / 25.4
}

fn unit_factor(code: i64) -> Option<(f64, &'static str)> {
    let mm = 72.0 / 25.4;
    Some(match code {
        1 => (72.0, "inches"),
        2 => (864.0, "feet"),
        3 => (72.0 * 63_360.0, "miles"),
        4 => (mm, "millimetres"),
        5 => (mm * 10.0, "centimetres"),
        6 => (mm * 1000.0, "metres"),
        7 => (mm * 1.0e6, "kilometres"),
        8 => (72.0e-6, "microinches"),
        9 => (72.0e-3, "mils"),
        10 => (2592.0, "yards"),
        11 => (mm * 1.0e-7, "ångströms"),
        12 => (mm * 1.0e-6, "nanometres"),
        13 => (mm * 1.0e-3, "microns"),
        14 => (mm * 100.0, "decimetres"),
        15 => (mm * 1.0e4, "decametres"),
        16 => (mm * 1.0e5, "hectometres"),
        17 => (mm * 1.0e12, "gigametres"),
        18 => (mm * 1.495_978_707e14, "astronomical units"),
        19 => (mm * 9.460_730_472_580_8e18, "light years"),
        20 => (mm * 3.085_677_581_49e19, "parsecs"),
        21 => (mm * 1000.0 * 1200.0 / 3937.0, "US survey feet"),
        _ => return None,
    })
}

impl<'a> Conv<'a> {
    fn new(d: &'a Drawing) -> Self {
        let mut layers = HashMap::new();
        let mut layer_order = Vec::new();
        for r in d.table("LAYER") {
            let Some(name) = r.s(2) else { continue };
            let key = name.to_ascii_uppercase();
            let aci = r.i_or(62, 7);
            let color = match r.i(420) {
                Some(tc) => true_color(tc),
                None => rgb(color::aci_rgb(aci.abs().clamp(1, 255))),
            };
            let flags = r.i_or(70, 0);
            if !layers.contains_key(&key) {
                layer_order.push(key.clone());
            }
            layers.insert(
                key,
                Layer {
                    color,
                    off: aci < 0,
                    frozen: flags & 1 != 0,
                    ltype: r.s(6).unwrap_or_else(|| "CONTINUOUS".into()),
                    lw: r.i_or(370, -3),
                },
            );
        }
        let mut ltypes = HashMap::new();
        for r in d.table("LTYPE") {
            let Some(name) = r.s(2) else { continue };
            let dashes: Vec<f64> = r.pairs.iter().filter(|p| p.code == 49).filter_map(|p| p.f()).collect();
            ltypes.insert(name.to_ascii_uppercase(), dashes);
        }
        let cap = match d.header_i("$ENDCAPS") {
            Some(0) => LineCap::Butt,
            Some(2) | Some(3) => LineCap::Square,
            _ => LineCap::Round,
        };
        let join = match d.header_i("$JOINSTYLE") {
            Some(0) | Some(2) => LineJoin::Miter,
            Some(3) => LineJoin::Bevel,
            _ => LineJoin::Round,
        };
        let mut warnings = Vec::new();
        let insunits = d.header_i("$INSUNITS").unwrap_or(0);
        let unit = match unit_factor(insunits) {
            Some((f, _)) => f,
            None => {
                warnings.push(if insunits == 0 {
                    "the drawing has no units ($INSUNITS unitless or missing); 1 drawing unit was taken as 1 mm".to_string()
                } else {
                    format!("unknown drawing units ($INSUNITS = {insunits}); 1 drawing unit was taken as 1 mm")
                });
                72.0 / 25.4
            }
        };
        let lw_default = d.header_i("$LWDEFAULT").filter(|v| (0..=211).contains(v)).unwrap_or(25);
        let ltscale = d.header_f("$LTSCALE").filter(|v| *v > 0.0).unwrap_or(1.0);
        Self {
            d,
            layers,
            layer_order,
            ltypes,
            ltscale,
            lw_default,
            cap,
            join,
            unit,
            warnings,
            skipped: BTreeMap::new(),
            unknown: BTreeMap::new(),
            paths: 0,
            patterns: Vec::new(),
            flags: Flags::default(),
        }
    }

    fn root_ctx(&self) -> Ctx {
        Ctx {
            m: Matrix::scale(self.unit, self.unit),
            layer: None,
            byblock: ByBlock { color: Color::Rgb(0.0, 0.0, 0.0), lw_pt: lw_to_pt(self.lw_default), dash: Vec::new() },
            stack: Vec::new(),
        }
    }

    fn run(mut self) -> Result<Document> {
        let d = self.d;
        let ctx = self.root_ctx();
        let mut groups: HashMap<String, Vec<Element>> = HashMap::new();
        let mut extra_order: Vec<String> = Vec::new();
        let model: Vec<&Record> = d.entities.iter().filter(|e| e.i_or(67, 0) != 1).collect();
        let paper: Vec<&Record> = d.entities.iter().filter(|e| e.i_or(67, 0) == 1).collect();
        self.convert_list(&model, &ctx, &mut groups, &mut extra_order);
        if groups.is_empty() && !paper.is_empty() {
            // Model space empty: fall back to the paper space layout.
            let before = self.skipped.clone();
            self.convert_list(&paper, &ctx, &mut groups, &mut extra_order);
            if !groups.is_empty() {
                self.warnings.push("model space is empty; the paper space layout was converted instead".into());
            } else {
                self.skipped = before;
            }
        }
        let mut elements = Vec::new();
        for name in self.layer_order.iter().chain(extra_order.iter()) {
            if let Some(els) = groups.remove(name)
                && !els.is_empty()
            {
                elements.push(Element::Group(els));
            }
        }
        self.finish_warnings();
        let Some(b) = geom::tight_bounds(&elements) else {
            let mut msg = "the DXF file contains no drawable geometry".to_string();
            if !self.warnings.is_empty() {
                msg.push_str(&format!(" ({})", self.warnings.join("; ")));
            }
            bail!(msg);
        };
        let shift = Matrix::translate(-b.llx, -b.lly);
        let elements = crate::patterns::transform_elements(&elements, &shift);
        let (w, h) = (b.urx - b.llx, b.ury - b.lly);
        if w > MAX_PAGE_PT || h > MAX_PAGE_PT {
            self.warnings.push(format!(
                "the drawing is {:.0} × {:.0} mm, larger than Illustrator's maximum artboard (5779 mm); check the drawing units",
                w * 25.4 / 72.0,
                h * 25.4 / 72.0
            ));
        }
        let mut doc = Document::new(Rect { llx: 0.0, lly: 0.0, urx: w, ury: h });
        doc.elements = elements;
        doc.warnings = self.warnings;
        Ok(doc)
    }

    fn convert_list(
        &mut self,
        ents: &[&Record],
        ctx: &Ctx,
        groups: &mut HashMap<String, Vec<Element>>,
        extra_order: &mut Vec<String>,
    ) {
        for e in ents {
            let mut out = Vec::new();
            self.entity(e, ctx, &mut out);
            if out.is_empty() {
                continue;
            }
            let layer = e.layer().to_ascii_uppercase();
            if !self.layers.contains_key(&layer) && !extra_order.contains(&layer) {
                extra_order.push(layer.clone());
            }
            groups.entry(layer).or_default().extend(out);
        }
    }

    fn finish_warnings(&mut self) {
        let f = &self.flags;
        let mut w = Vec::new();
        if let Some(n) = self.skipped.get("text") {
            w.push(format!(
                "{n} text object(s) (TEXT, MTEXT, attributes, dimension text) were skipped: DXF text needs CAD fonts; \
                 explode the text to geometry in the CAD program first"
            ));
        }
        for (k, label) in [
            ("leader", "leader(s) (LEADER/MULTILEADER)"),
            ("image", "raster image(s) / wipeout(s)"),
            ("infinite", "construction line(s) (XLINE/RAY) of infinite length"),
            ("solid3d", "3D solid / region / surface object(s)"),
            ("mline", "multiline(s) (MLINE)"),
            ("shape", "shape(s) (SHAPE)"),
            ("proxy", "proxy / custom object(s)"),
        ] {
            if let Some(n) = self.skipped.get(k) {
                w.push(format!("{n} {label} were skipped (not supported)"));
            }
        }
        if !self.unknown.is_empty() {
            let list: Vec<String> = self.unknown.iter().map(|(k, n)| format!("{k} ×{n}")).collect();
            w.push(format!("unsupported entities were skipped: {}", list.join(", ")));
        }
        if f.varying_width {
            w.push("polylines with varying width were drawn with their average width".into());
        }
        if f.spline_approx {
            w.push("some splines (rational, high degree or fit-point only) were approximated with Bézier curves".into());
        }
        if f.hatch_expanded {
            w.push("some hatch patterns do not form a regular tile and were expanded to clipped lines".into());
        }
        if f.hatch_truncated {
            w.push(format!(
                "a hatch pattern was too dense to expand completely (limit {MAX_HATCH_LINES} lines per hatch)"
            ));
        }
        if f.hatch_damaged {
            w.push("some hatch boundary data could not be read; those hatches may be incomplete".into());
        }
        if f.cycle {
            w.push("a block that (indirectly) inserts itself was skipped".into());
        }
        if f.depth {
            w.push(format!("blocks nested deeper than {MAX_BLOCK_DEPTH} levels were skipped"));
        }
        if f.budget {
            w.push(format!("the drawing has more than {MAX_PATHS} paths; the rest was skipped"));
        }
        if f.missing_block {
            w.push("some INSERTs reference blocks that are not defined in the file".into());
        }
        if f.xref {
            w.push("external references (xrefs) are not included in the DXF and were skipped".into());
        }
        if f.nonfinite {
            w.push("some entities with invalid (non-finite) coordinates were skipped".into());
        }
        if f.array_capped {
            w.push("a very large block array (MINSERT) was truncated".into());
        }
        self.warnings.extend(w);
    }

    fn skip(&mut self, what: &'static str) {
        *self.skipped.entry(what).or_default() += 1;
    }

    // ---------------------------------------------------------------------
    // Properties
    // ---------------------------------------------------------------------

    fn layer_info(&self, name: &str) -> Option<&Layer> {
        self.layers.get(&name.to_ascii_uppercase())
    }

    fn effective_layer(&self, e: &Record, ctx: &Ctx) -> String {
        let l = e.layer();
        match &ctx.layer {
            Some(parent) if l == "0" => parent.clone(),
            _ => l,
        }
    }

    fn visible(&self, e: &Record, ctx: &Ctx) -> bool {
        if e.i_or(60, 0) == 1 {
            return false;
        }
        let own = e.layer();
        if self.layer_info(&own).is_some_and(|l| l.frozen) {
            return false;
        }
        let eff = self.effective_layer(e, ctx);
        // An INSERT on a layer that is off still shows its contents on other
        // layers (entities on layer 0 inherit the off layer and are hidden).
        let insert = e.kind == "INSERT";
        !self.layer_info(&eff).is_some_and(|l| (l.off && !insert) || l.frozen)
    }

    fn color(&self, e: &Record, ctx: &Ctx) -> Color {
        if let Some(tc) = e.i(420) {
            return true_color(tc);
        }
        match e.i_or(62, 256) {
            0 => ctx.byblock.color.clone(),
            256 => self.layer_color(&self.effective_layer(e, ctx)),
            c if (1..=255).contains(&c.abs()) => rgb(color::aci_rgb(c.abs())),
            _ => self.layer_color(&self.effective_layer(e, ctx)),
        }
    }

    fn layer_color(&self, layer: &str) -> Color {
        self.layer_info(layer).map(|l| l.color.clone()).unwrap_or(Color::Rgb(0.0, 0.0, 0.0))
    }

    fn lineweight_pt(&self, e: &Record, ctx: &Ctx) -> f64 {
        match e.i_or(370, -1) {
            -2 => ctx.byblock.lw_pt,
            -3 => lw_to_pt(self.lw_default),
            v if v >= 0 => lw_to_pt(v),
            _ => {
                let lw = self.layer_info(&self.effective_layer(e, ctx)).map(|l| l.lw).unwrap_or(-3);
                lw_to_pt(if lw < 0 { self.lw_default } else { lw })
            }
        }
    }

    /// Dash pattern in drawing units (× $LTSCALE × entity linetype scale).
    fn dash_units(&self, e: &Record, ctx: &Ctx) -> Vec<f64> {
        let name = e.s(6).unwrap_or_else(|| "BYLAYER".into()).to_ascii_uppercase();
        let lt = match name.as_str() {
            "BYBLOCK" => return ctx.byblock.dash.clone(),
            "BYLAYER" => self
                .layer_info(&self.effective_layer(e, ctx))
                .map(|l| l.ltype.to_ascii_uppercase())
                .unwrap_or_else(|| "CONTINUOUS".into()),
            _ => name,
        };
        let Some(pat) = self.ltypes.get(&lt) else { return Vec::new() };
        let s = self.ltscale * e.f(48).filter(|v| *v > 0.0).unwrap_or(1.0);
        pat.iter().map(|v| v * s).collect()
    }

    fn style(&self, e: &Record, ctx: &Ctx) -> Style {
        Style { color: self.color(e, ctx), width: self.lineweight_pt(e, ctx), dash: self.dash_units(e, ctx) }
    }

    fn stroke(&self, st: &Style, scale: f64) -> Stroke {
        let mut s = Stroke::solid(st.color.clone(), st.width);
        s.cap = self.cap;
        s.join = self.join;
        if let Some((dash, phase, dots)) = ir_dash(&st.dash, scale) {
            s.dash = dash;
            s.dash_phase = phase;
            if dots && s.cap == LineCap::Butt {
                s.cap = LineCap::Round;
            }
        }
        s
    }

    // ---------------------------------------------------------------------
    // Output helpers
    // ---------------------------------------------------------------------

    fn finite(&mut self, sps: &[Subpath]) -> bool {
        let ok = sps.iter().flat_map(|s| s.points()).all(|p| p.x.is_finite() && p.y.is_finite());
        if !ok {
            self.flags.nonfinite = true;
        }
        ok
    }

    fn budget_ok(&mut self) -> bool {
        if self.paths >= MAX_PATHS {
            self.flags.budget = true;
            return false;
        }
        true
    }

    fn push_stroked(&mut self, sps: Vec<Subpath>, st: &Style, ctx: &Ctx, out: &mut Vec<Element>) {
        if sps.is_empty() || !self.finite(&sps) || !self.budget_ok() {
            return;
        }
        let mut p = PathItem::new(sps);
        p.stroke = Some(self.stroke(st, ctx.m.mean_scale()));
        self.paths += 1;
        out.push(Element::Path(p));
    }

    fn push_filled(&mut self, sps: Vec<Subpath>, fill: Color, even_odd: bool, out: &mut Vec<Element>) {
        if sps.is_empty() || !self.finite(&sps) || !self.budget_ok() {
            return;
        }
        let mut p = PathItem::new(sps);
        p.fill = Some(fill);
        p.even_odd = even_odd;
        self.paths += 1;
        out.push(Element::Path(p));
    }

    // ---------------------------------------------------------------------
    // Entities
    // ---------------------------------------------------------------------

    fn entity(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        if !self.visible(e, ctx) || !self.budget_ok() {
            return;
        }
        match e.kind.as_str() {
            "LINE" => self.line(e, ctx, out),
            "POINT" | "VIEWPORT" | "ATTDEF" | "SEQEND" | "ENDBLK" | "BLOCK" => {}
            "CIRCLE" | "ARC" => self.circle(e, ctx, out),
            "ELLIPSE" => self.ellipse(e, ctx, out),
            "LWPOLYLINE" => self.lwpolyline(e, ctx, out),
            "POLYLINE" => self.polyline(e, ctx, out),
            "SPLINE" => self.spline(e, ctx, out),
            "SOLID" | "TRACE" => self.solid(e, ctx, out),
            "3DFACE" => self.face3d(e, ctx, out),
            "HATCH" => self.hatch(e, ctx, out),
            "INSERT" => {
                for a in &e.children {
                    if a.i_or(70, 0) & 1 == 0 && a.i_or(60, 0) != 1 {
                        self.skip("text");
                    }
                }
                self.insert(e, ctx, out)
            }
            "DIMENSION" | "ACAD_TABLE" | "ARC_DIMENSION" | "LARGE_RADIAL_DIMENSION" => self.anon_block(e, ctx, out),
            "TEXT" | "MTEXT" | "ATTRIB" | "RTEXT" | "ARCALIGNEDTEXT" | "TOLERANCE" => self.skip("text"),
            "LEADER" | "MULTILEADER" | "MLEADER" => self.skip("leader"),
            "IMAGE" | "WIPEOUT" | "OLE2FRAME" | "OLEFRAME" | "PDFUNDERLAY" | "DWFUNDERLAY" | "DGNUNDERLAY" => {
                self.skip("image")
            }
            "XLINE" | "RAY" => self.skip("infinite"),
            "3DSOLID" | "REGION" | "BODY" | "SURFACE" | "PLANESURFACE" | "EXTRUDEDSURFACE" | "LOFTEDSURFACE"
            | "REVOLVEDSURFACE" | "SWEPTSURFACE" | "MESH" | "NURBSURFACE" => self.skip("solid3d"),
            "MLINE" => self.skip("mline"),
            "SHAPE" => self.skip("shape"),
            "ACAD_PROXY_ENTITY" => self.skip("proxy"),
            other => *self.unknown.entry(other.to_string()).or_default() += 1,
        }
    }

    fn line(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let a = e.pt3_or0(10);
        let b = e.pt3_or0(11);
        let mut pb = PathBuilder::new(ctx.m);
        pb.move_to(a[0], a[1]);
        pb.line_to(b[0], b[1]);
        let st = self.style(e, ctx);
        self.push_stroked(pb.subpaths, &st, ctx, out);
    }

    fn circle(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let c = e.pt3_or0(10);
        let r = e.f_or(40, 0.0).abs();
        if r <= 0.0 {
            return;
        }
        let m = ocs_matrix(e.normal(), c[2]).then(&ctx.m);
        let mut pb = PathBuilder::new(m);
        let center = Point::new(c[0], c[1]);
        if e.kind == "CIRCLE" {
            pb.arc(center, r, 0.0, TAU);
            pb.close();
        } else {
            let a0 = e.f_or(50, 0.0) * DEG;
            let a1 = e.f_or(51, 360.0) * DEG;
            pb.arc(center, r, a0, ccw_sweep(a0, a1));
        }
        let st = self.style(e, ctx);
        self.push_stroked(pb.subpaths, &st, ctx, out);
    }

    fn ellipse(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let c = e.pt3_or0(10);
        let maj = e.pt3_or0(11);
        let ratio = e.f_or(40, 1.0);
        let minor = ellipse_minor(e.normal(), maj, ratio);
        let t0 = e.f_or(41, 0.0);
        let t1 = e.f_or(42, TAU);
        let full = (t1 - t0).abs() >= TAU - 1e-9 || ((t1 - t0) % TAU).abs() < 1e-12;
        let sweep = if full { TAU } else { ccw_sweep(t0, t1) };
        let mut pb = PathBuilder::new(ctx.m);
        pb.ellipse_arc(Point::new(c[0], c[1]), Point::new(maj[0], maj[1]), Point::new(minor[0], minor[1]), t0, sweep);
        if full {
            pb.close();
        }
        let st = self.style(e, ctx);
        self.push_stroked(pb.subpaths, &st, ctx, out);
    }

    /// Stroke for a (possibly wide) polyline.
    fn poly_style(&mut self, e: &Record, ctx: &Ctx, widths: &[(f64, f64)], const_w: Option<f64>, m: &Matrix) -> (Style, bool) {
        let mut st = self.style(e, ctx);
        let w = match const_w.filter(|w| *w > 0.0) {
            Some(w) => w,
            None => {
                let nz: Vec<f64> = widths.iter().flat_map(|(a, b)| [*a, *b]).collect();
                if nz.iter().all(|w| *w <= 0.0) {
                    0.0
                } else {
                    let first = nz[0];
                    if nz.iter().any(|w| (w - first).abs() > 1e-9) {
                        self.flags.varying_width = true;
                    }
                    nz.iter().sum::<f64>() / nz.len() as f64
                }
            }
        };
        if w > 0.0 {
            st.width = w * m.mean_scale();
            return (st, true);
        }
        (st, false)
    }

    fn push_poly(&mut self, sps: Vec<Subpath>, st: Style, wide: bool, ctx: &Ctx, out: &mut Vec<Element>) {
        let n = out.len();
        self.push_stroked(sps, &st, ctx, out);
        if wide && let Some(Element::Path(p)) = out.get_mut(n) && let Some(s) = p.stroke.as_mut() {
            s.cap = LineCap::Butt;
            s.join = LineJoin::Miter;
        }
    }

    fn lwpolyline(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        // (x, y, start width, end width, bulge)
        let mut v: Vec<[f64; 5]> = Vec::new();
        for p in &e.pairs {
            match p.code {
                10 => v.push([p.f().unwrap_or(0.0), 0.0, 0.0, 0.0, 0.0]),
                20 => {
                    if let Some(l) = v.last_mut() {
                        l[1] = p.f().unwrap_or(0.0)
                    }
                }
                40..=42 => {
                    if let Some(l) = v.last_mut() {
                        l[(p.code - 38) as usize] = p.f().unwrap_or(0.0)
                    }
                }
                _ => {}
            }
        }
        let closed = e.i_or(70, 0) & 1 != 0;
        let m = ocs_matrix(e.normal(), e.f_or(38, 0.0)).then(&ctx.m);
        let verts: Vec<(Point, f64)> = v.iter().map(|a| (Point::new(a[0], a[1]), a[4])).collect();
        let widths: Vec<(f64, f64)> = v.iter().map(|a| (a[2], a[3])).collect();
        let Some(sps) = poly_path(&verts, closed, m) else { return };
        let (st, wide) = self.poly_style(e, ctx, &widths, e.f(43), &m);
        self.push_poly(sps, st, wide, ctx, out);
    }

    fn polyline(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let flags = e.i_or(70, 0);
        let closed = flags & 1 != 0;
        if flags & 64 != 0 {
            return self.polyface(e, ctx, out);
        }
        if flags & 16 != 0 {
            return self.mesh(e, ctx, out);
        }
        let is3d = flags & 8 != 0;
        let verts: Vec<&Record> = e.children.iter().filter(|v| v.i_or(70, 0) & 16 == 0).collect();
        let m = if is3d { ctx.m } else { ocs_matrix(e.normal(), e.pt3_or0(10)[2]).then(&ctx.m) };
        let pts: Vec<(Point, f64)> = verts
            .iter()
            .map(|v| {
                let p = v.pt3_or0(10);
                (Point::new(p[0], p[1]), if is3d { 0.0 } else { v.f_or(42, 0.0) })
            })
            .collect();
        let dw = (e.f_or(40, 0.0), e.f_or(41, 0.0));
        let widths: Vec<(f64, f64)> = verts.iter().map(|v| (v.f(40).unwrap_or(dw.0), v.f(41).unwrap_or(dw.1))).collect();
        let Some(sps) = poly_path(&pts, closed, m) else { return };
        let (st, wide) = if is3d { (self.style(e, ctx), false) } else { self.poly_style(e, ctx, &widths, None, &m) };
        self.push_poly(sps, st, wide, ctx, out);
    }

    fn polyface(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let mut coords: Vec<Point> = Vec::new();
        let mut faces: Vec<Vec<i64>> = Vec::new();
        for v in &e.children {
            let f = v.i_or(70, 0);
            if f & 128 != 0 && f & 64 != 0 {
                let p = v.pt3_or0(10);
                coords.push(Point::new(p[0], p[1]));
            } else if f & 128 != 0 {
                faces.push([71, 72, 73, 74].iter().filter_map(|c| v.i(*c)).filter(|i| *i != 0).collect());
            }
        }
        let mut pb = PathBuilder::new(ctx.m);
        for f in &faces {
            let n = f.len();
            // Visible edges only (negative index = invisible edge from it).
            for k in 0..n {
                let (a, b) = (f[k], f[(k + 1) % n]);
                if a < 0 || n < 2 {
                    continue;
                }
                let (Some(pa), Some(pbp)) =
                    (coords.get((a.unsigned_abs() as usize).wrapping_sub(1)), coords.get((b.unsigned_abs() as usize).wrapping_sub(1)))
                else {
                    continue;
                };
                pb.move_to(pa.x, pa.y);
                pb.line_to(pbp.x, pbp.y);
            }
        }
        let st = self.style(e, ctx);
        self.push_stroked(pb.subpaths, &st, ctx, out);
    }

    fn mesh(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let flags = e.i_or(70, 0);
        let (mc, nc) = (e.i_or(71, 0).max(0) as usize, e.i_or(72, 0).max(0) as usize);
        let pts: Vec<Point> = e.children.iter().map(|v| v.pt3_or0(10)).map(|p| Point::new(p[0], p[1])).collect();
        if mc == 0 || nc == 0 || pts.len() < mc * nc {
            return;
        }
        let mut pb = PathBuilder::new(ctx.m);
        for i in 0..mc {
            pb.move_to(pts[i * nc].x, pts[i * nc].y);
            for j in 1..nc {
                pb.line_to(pts[i * nc + j].x, pts[i * nc + j].y);
            }
            if flags & 32 != 0 {
                pb.close();
            }
        }
        for j in 0..nc {
            pb.move_to(pts[j].x, pts[j].y);
            for i in 1..mc {
                pb.line_to(pts[i * nc + j].x, pts[i * nc + j].y);
            }
            if flags & 1 != 0 {
                pb.close();
            }
        }
        let st = self.style(e, ctx);
        self.push_stroked(pb.subpaths, &st, ctx, out);
    }

    fn spline(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let flags = e.i_or(70, 0);
        let degree = e.i_or(71, 3).clamp(1, 30) as usize;
        let knots: Vec<f64> = e.pairs.iter().filter(|p| p.code == 40).filter_map(|p| p.f()).collect();
        let weights: Vec<f64> = e.pairs.iter().filter(|p| p.code == 41).filter_map(|p| p.f()).collect();
        let pts = |code: i32| -> Vec<Point> {
            let mut v: Vec<Point> = Vec::new();
            for p in &e.pairs {
                if p.code == code {
                    v.push(Point::new(p.f().unwrap_or(0.0), 0.0));
                } else if p.code == code + 10
                    && let Some(l) = v.last_mut()
                {
                    l.y = p.f().unwrap_or(0.0);
                }
            }
            v
        };
        let ctrl = pts(10);
        let fit = pts(11);
        let closed = flags & 1 != 0;
        // Tolerance: 1e-4 of the control polygon size.
        let size = ctrl
            .iter()
            .chain(fit.iter())
            .fold(0.0f64, |m, p| m.max(p.x.abs()).max(p.y.abs()))
            .max(1e-9);
        let tol = size * 1e-5;
        let nurbs = Nurbs { degree, knots, ctrl: ctrl.clone(), weights: if flags & 4 != 0 { weights } else { Vec::new() } };
        let mut pb = PathBuilder::new(ctx.m);
        if let Some(b) = nurbs.to_beziers(tol) {
            self.flags.spline_approx |= b.approximated;
            pb.move_to(b.start.x, b.start.y);
            for s in &b.segs {
                pb.curve_to(s[0], s[1], s[2]);
            }
        } else {
            let pts = if fit.len() >= 2 { fit } else { ctrl };
            if pts.len() < 2 {
                return;
            }
            let tan = |c: i32| {
                let p = e.pt3(c)?;
                (p[0].abs() + p[1].abs() > 1e-12).then(|| Point::new(p[0], p[1]))
            };
            self.flags.spline_approx = true;
            pb.move_to(pts[0].x, pts[0].y);
            if degree == 1 {
                for p in &pts[1..] {
                    pb.line_to(p.x, p.y);
                }
            } else {
                for s in geom::fit_point_curve(&pts, tan(12), tan(13), closed) {
                    pb.curve_to(s[0], s[1], s[2]);
                }
            }
        }
        if closed {
            pb.close();
        }
        let st = self.style(e, ctx);
        self.push_stroked(pb.subpaths, &st, ctx, out);
    }

    fn solid(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let p: Vec<P3> = [10, 11, 12, 13].iter().map(|c| e.pt3_or0(*c)).collect();
        let m = ocs_matrix(e.normal(), p[0][2]).then(&ctx.m);
        let mut pb = PathBuilder::new(m);
        // Vertex order 1-2-4-3.
        pb.move_to(p[0][0], p[0][1]);
        pb.line_to(p[1][0], p[1][1]);
        let p4 = if e.pt3(13).is_some() { p[3] } else { p[2] };
        pb.line_to(p4[0], p4[1]);
        if (p4[0] - p[2][0]).abs() > 1e-12 || (p4[1] - p[2][1]).abs() > 1e-12 {
            pb.line_to(p[2][0], p[2][1]);
        }
        pb.close();
        let color = self.color(e, ctx);
        self.push_filled(pb.subpaths, color, false, out);
    }

    fn face3d(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let p: Vec<P3> = [10, 11, 12, 13].iter().map(|c| e.pt3(*c).unwrap_or_else(|| e.pt3_or0(12))).collect();
        let inv = e.i_or(70, 0);
        let mut pb = PathBuilder::new(ctx.m);
        if inv & 15 == 0 {
            pb.move_to(p[0][0], p[0][1]);
            for q in &p[1..] {
                pb.line_to(q[0], q[1]);
            }
            pb.close();
        } else {
            for k in 0..4 {
                if inv & (1 << k) == 0 {
                    let (a, b) = (p[k], p[(k + 1) % 4]);
                    pb.move_to(a[0], a[1]);
                    pb.line_to(b[0], b[1]);
                }
            }
        }
        let st = self.style(e, ctx);
        self.push_stroked(pb.subpaths, &st, ctx, out);
    }

    // ---------------------------------------------------------------------
    // Blocks
    // ---------------------------------------------------------------------

    fn insert(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let name = e.s(2).unwrap_or_default();
        let ins = e.pt3_or0(10);
        let sx = e.f(41).filter(|v| *v != 0.0).unwrap_or(1.0);
        let sy = e.f(42).filter(|v| *v != 0.0).unwrap_or(1.0);
        let rot = e.f_or(50, 0.0) * DEG;
        let cols = e.i_or(70, 1).max(1);
        let rows = e.i_or(71, 1).max(1);
        let (cs, rs) = (e.f_or(44, 0.0), e.f_or(45, 0.0));
        let (mut cols, mut rows) = (cols as usize, rows as usize);
        if cols.saturating_mul(rows) > 100_000 {
            self.flags.array_capped = true;
            cols = cols.min(316);
            rows = rows.min(316);
        }
        let ocs = ocs_matrix(e.normal(), ins[2]);
        let r = rotate(rot);
        let mut group = Vec::new();
        for row in 0..rows {
            for col in 0..cols {
                let off = r.apply(Point::new(col as f64 * cs, row as f64 * rs));
                let local = Matrix::scale(sx, sy)
                    .then(&r)
                    .then(&Matrix::translate(ins[0] + off.x, ins[1] + off.y))
                    .then(&ocs)
                    .then(&ctx.m);
                self.block_into(&name, e, local, ctx, &mut group);
                if self.flags.budget {
                    break;
                }
            }
        }
        if !group.is_empty() {
            out.push(Element::Group(group));
        }
    }

    fn anon_block(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let Some(name) = e.s(2) else { return };
        let m = if e.kind == "ACAD_TABLE" {
            let p = e.pt3_or0(10);
            Matrix::translate(p[0], p[1]).then(&ocs_matrix(e.normal(), p[2])).then(&ctx.m)
        } else {
            ctx.m
        };
        let mut group = Vec::new();
        self.block_into(&name, e, m, ctx, &mut group);
        if !group.is_empty() {
            out.push(Element::Group(group));
        }
    }

    /// Convert block `name` with `m` = block coordinates (after the base
    /// point shift) → page. `e` is the referencing entity.
    fn block_into(&mut self, name: &str, e: &Record, m: Matrix, ctx: &Ctx, out: &mut Vec<Element>) {
        let d = self.d;
        let Some(block) = d.blocks.get(&name.to_ascii_uppercase()) else {
            self.flags.missing_block = true;
            return;
        };
        if block.flags & 4 != 0 {
            self.flags.xref = true;
            return;
        }
        if ctx.stack.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            self.flags.cycle = true;
            return;
        }
        if ctx.stack.len() >= MAX_BLOCK_DEPTH {
            self.flags.depth = true;
            return;
        }
        let base = block.base;
        let st = self.style(e, ctx);
        let child = Ctx {
            m: Matrix::translate(-base[0], -base[1]).then(&m),
            layer: Some(self.effective_layer(e, ctx)),
            byblock: ByBlock { color: st.color, lw_pt: st.width, dash: st.dash },
            stack: {
                let mut s = ctx.stack.clone();
                s.push(name.to_string());
                s
            },
        };
        for be in &block.entities {
            self.entity(be, &child, out);
            if self.flags.budget {
                return;
            }
        }
    }

    // ---------------------------------------------------------------------
    // Hatches
    // ---------------------------------------------------------------------

    fn hatch(&mut self, e: &Record, ctx: &Ctx, out: &mut Vec<Element>) {
        let size = e
            .pairs
            .iter()
            .filter(|p| matches!(p.code, 10 | 20))
            .filter_map(|p| p.f())
            .fold(0.0f64, |m, v| m.max(v.abs()))
            .max(1e-9);
        let h = hatch::parse(e, size * 1e-5);
        self.flags.hatch_damaged |= h.damaged;
        self.flags.spline_approx |= h.approximated_splines;
        let local = hatch::boundary(&h);
        if local.is_empty() {
            return;
        }
        let f = ocs_matrix(h.normal, h.elevation).then(&ctx.m);
        let page = crate::patterns::transform_subpaths(&local, &f);
        if !self.finite(&page) {
            return;
        }
        let color = self.color(e, ctx);
        if let Some(g) = &h.gradient {
            let paint = gradient_paint(g, &local, &f, &color);
            return self.push_filled(page, paint, true, out);
        }
        if h.solid || h.families.is_empty() {
            return self.push_filled(page, color, true, out);
        }
        let st = self.style(e, ctx);
        let mut stroke = self.stroke(&Style { color: color.clone(), width: st.width, dash: Vec::new() }, 1.0);
        stroke.cap = LineCap::Butt;
        // Native pattern.
        let name = if h.name.trim().is_empty() { "Hatch".to_string() } else { h.name.trim().to_string() };
        if let Some((pat, pm)) = hatch::build_tile(&name, &h.families, &stroke, f.mean_scale()) {
            let pattern = self.intern_pattern(pat);
            let paint = Color::Pattern(PatternPaint { pattern, matrix: pm.then(&f) });
            return self.push_filled(page, paint, true, out);
        }
        // Expanded lines clipped to the boundary.
        self.flags.hatch_expanded = true;
        let Some(bb) = geom::tight_subpath_bounds(&local) else { return };
        let dot_len = stroke.width / f.mean_scale().max(1e-12);
        let (lines, overflow) =
            hatch::lines_in_region(&h.families, &bb, &Matrix::IDENTITY, &f, dot_len.max(1e-9), MAX_HATCH_LINES);
        self.flags.hatch_truncated |= overflow;
        if lines.is_empty() || !self.finite(&lines) {
            return;
        }
        let mut p = PathItem::new(lines);
        p.stroke = Some(stroke);
        self.paths += 1;
        out.push(Element::ClipGroup { clip: page, even_odd: true, children: vec![Element::Path(p)] });
    }

    /// Share structurally identical patterns; give each a unique name.
    fn intern_pattern(&mut self, mut p: Pattern) -> Arc<Pattern> {
        if let Some(q) =
            self.patterns.iter().find(|q| q.bbox == p.bbox && q.x_step == p.x_step && q.y_step == p.y_step && q.elements == p.elements)
        {
            return q.clone();
        }
        let base = p.name.clone();
        let mut n = 1;
        while self.patterns.iter().any(|q| q.name == p.name) {
            n += 1;
            p.name = format!("{base} {n}");
        }
        let a = Arc::new(p);
        self.patterns.push(a.clone());
        a
    }
}

/// Polyline vertices (with bulges) → subpaths. `None` for < 2 vertices.
fn poly_path(verts: &[(Point, f64)], closed: bool, m: Matrix) -> Option<Vec<Subpath>> {
    if verts.len() < 2 {
        return None;
    }
    let mut pb = PathBuilder::new(m);
    pb.move_to(verts[0].0.x, verts[0].0.y);
    for k in 1..verts.len() {
        pb.bulge_to(verts[k - 1].1, verts[k].0.x, verts[k].0.y);
    }
    if closed {
        let (last, first) = (verts[verts.len() - 1], verts[0]);
        if !geom::near(last.0, first.0, 1e-12) || last.1.abs() > 1e-10 {
            pb.bulge_to(last.1, first.0.x, first.0.y);
        }
        pb.close();
    }
    Some(pb.subpaths)
}

/// DXF linetype elements (drawing units) → IR dash array in points, phase and
/// whether it contains dots. `None` = continuous.
fn ir_dash(pat: &[f64], scale: f64) -> Option<(Vec<f64>, f64, bool)> {
    if pat.is_empty() || pat.iter().all(|v| *v >= 0.0) || !scale.is_finite() {
        return None;
    }
    // Merge runs of the same kind: (length, on).
    let mut runs: Vec<(f64, bool)> = Vec::new();
    let mut dots = false;
    for &v in pat {
        let on = v >= 0.0;
        dots |= v == 0.0;
        let l = v.abs() * scale;
        match runs.last_mut() {
            Some((len, o)) if *o == on => *len += l,
            _ => runs.push((l, on)),
        }
    }
    let mut phase = 0.0;
    if runs.len() > 1 && runs[0].1 == runs[runs.len() - 1].1 {
        let (l, _) = runs.pop().unwrap_or((0.0, true));
        runs[0].0 += l;
        phase += l;
    }
    if !runs[0].1 {
        // Starts with a gap: rotate it to the end.
        let g = runs.remove(0);
        phase += -g.0;
        runs.push(g);
    }
    let total: f64 = runs.iter().map(|r| r.0).sum();
    if runs.len() < 2 || total <= 1e-9 {
        return None;
    }
    let phase = phase.rem_euclid(total);
    Some((runs.into_iter().map(|r| r.0).collect(), phase, dots))
}

fn lerp(a: &Color, b: &Color, t: f64) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            Color::Rgb(r1 + (r2 - r1) * t, g1 + (g2 - g1) * t, b1 + (b2 - b1) * t)
        }
        _ => a.clone(),
    }
}

fn gradient_paint(g: &hatch::GradientDef, local: &[Subpath], f: &Matrix, entity_color: &Color) -> Color {
    let conv = |c: &(Option<i64>, Option<i64>)| match c {
        (_, Some(tc)) => true_color(*tc),
        (Some(aci), None) => rgb(color::aci_rgb((*aci).clamp(1, 255))),
        _ => entity_color.clone(),
    };
    let c1 = g.colors.first().map(conv).unwrap_or_else(|| entity_color.clone());
    let c2 = if g.single_color || g.colors.len() < 2 {
        let t = g.tint.clamp(0.0, 1.0);
        if t >= 0.5 {
            lerp(&c1, &Color::Rgb(1.0, 1.0, 1.0), (t - 0.5) * 2.0)
        } else {
            lerp(&Color::Rgb(0.0, 0.0, 0.0), &c1, t * 2.0)
        }
    } else {
        conv(&g.colors[1])
    };
    let bb = geom::tight_subpath_bounds(local).unwrap_or(Rect { llx: 0.0, lly: 0.0, urx: 1.0, ury: 1.0 });
    let ctr = Point::new((bb.llx + bb.urx) / 2.0, (bb.lly + bb.ury) / 2.0);
    let (dir, nrm) = ((g.angle.cos(), g.angle.sin()), (-g.angle.sin(), g.angle.cos()));
    let corners = [(bb.llx, bb.lly), (bb.urx, bb.lly), (bb.urx, bb.ury), (bb.llx, bb.ury)];
    let ext = |d: (f64, f64)| {
        corners
            .iter()
            .map(|c| ((c.0 - ctr.x) * d.0 + (c.1 - ctr.y) * d.1).abs())
            .fold(0.0f64, f64::max)
            .max(1e-9)
    };
    let half = ext(dir);
    let stop = |o: f64, c: &Color| GradientStop { offset: o, color: c.clone() };
    let along = |s: f64| Point::new(ctr.x + dir.0 * s, ctr.y + dir.1 * s);
    let radius = ((bb.urx - bb.llx).powi(2) + (bb.ury - bb.lly).powi(2)).sqrt() / 2.0;
    let (kind, stops, start, end, r) = match g.name.as_str() {
        "CYLINDER" => (GradientKind::Linear, vec![stop(0.0, &c1), stop(0.5, &c2), stop(1.0, &c1)], along(-half), along(half), 0.0),
        "INVCYLINDER" => (GradientKind::Linear, vec![stop(0.0, &c2), stop(0.5, &c1), stop(1.0, &c2)], along(-half), along(half), 0.0),
        "SPHERICAL" => (GradientKind::Radial, vec![stop(0.0, &c2), stop(1.0, &c1)], ctr, ctr, radius),
        "INVSPHERICAL" => (GradientKind::Radial, vec![stop(0.0, &c1), stop(1.0, &c2)], ctr, ctr, radius),
        "HEMISPHERICAL" | "INVHEMISPHERICAL" => {
            let h2 = ext(nrm);
            let c = Point::new(ctr.x - nrm.0 * h2, ctr.y - nrm.1 * h2);
            let stops = if g.name == "HEMISPHERICAL" {
                vec![stop(0.0, &c2), stop(1.0, &c1)]
            } else {
                vec![stop(0.0, &c1), stop(1.0, &c2)]
            };
            (GradientKind::Radial, stops, c, c, radius * 2.0)
        }
        "CURVED" => (GradientKind::Linear, vec![stop(0.0, &c1), stop(0.35, &lerp(&c1, &c2, 0.75)), stop(1.0, &c2)], along(-half), along(half), 0.0),
        "INVCURVED" => (GradientKind::Linear, vec![stop(0.0, &c2), stop(0.65, &lerp(&c1, &c2, 0.25)), stop(1.0, &c1)], along(-half), along(half), 0.0),
        _ => (GradientKind::Linear, vec![stop(0.0, &c1), stop(1.0, &c2)], along(-half), along(half), 0.0),
    };
    Color::Gradient(GradientPaint { gradient: Arc::new(Gradient { kind, stops }), start, end, radius: r, matrix: *f })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_conversion() {
        // DASHED: 0.5 dash, -0.25 gap.
        assert_eq!(ir_dash(&[0.5, -0.25], 2.0), Some((vec![1.0, 0.5], 0.0, false)));
        // Starts with a gap.
        let (d, ph, _) = ir_dash(&[-0.25, 0.5], 1.0).unwrap();
        assert_eq!(d, vec![0.5, 0.25]);
        assert!((ph - 0.5).abs() < 1e-12);
        // Dash-dot with dots.
        let (d, _, dots) = ir_dash(&[0.5, -0.25, 0.0, -0.25], 1.0).unwrap();
        assert_eq!(d, vec![0.5, 0.25, 0.0, 0.25]);
        assert!(dots);
        assert_eq!(ir_dash(&[], 1.0), None);
        assert_eq!(ir_dash(&[1.0], 1.0), None);
    }
}
