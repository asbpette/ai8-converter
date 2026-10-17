//! AI8 Converter desktop GUI.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use ai8_converter::{ColorMode, ConvertOptions, ConvertReport, OutputFormat, PatternMode, convert_file_to, ghostscript, preview};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

/// File extensions accepted by the converter (lower case).
const SUPPORTED_EXTENSIONS: &[&str] = &["eps", "epsf", "epsi", "ps", "pdf", "ai", "svg", "svgz", "dxf"];
/// Extensions offered in the "Add files…" dialog filter.
const DIALOG_EXTENSIONS: &[&str] = &["eps", "ps", "pdf", "ai", "svg", "svgz", "dxf"];
/// Resolution of the preview images (2 pixels per point: sharp on high-DPI
/// screens, and zoomable).
const PREVIEW_DPI: u32 = 144;

// ---------------------------------------------------------------------------
// Testable state model

#[derive(Debug, Clone, PartialEq)]
enum Status {
    Pending,
    Converting,
    Done { outputs: Vec<PathBuf>, warnings: Vec<String> },
    Failed(String),
}

/// State of the "Save as SVG" export of one file.
#[derive(Debug, Clone, PartialEq, Default)]
enum SvgExport {
    #[default]
    None,
    Saving,
    Saved(Vec<PathBuf>),
    Failed(String),
}

#[derive(Debug, Clone)]
struct QueueItem {
    id: u64,
    path: PathBuf,
    status: Status,
    svg: SvgExport,
}

/// Result of adding paths to the queue.
#[derive(Debug, Default, PartialEq)]
struct AddOutcome {
    added: usize,
    duplicates: usize,
    unsupported: Vec<PathBuf>,
}

#[derive(Debug, Default)]
struct Queue {
    items: Vec<QueueItem>,
    next_id: u64,
}

fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Key used to detect duplicate entries (same file reached via different spellings).
fn dedupe_key(path: &Path) -> PathBuf {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    // Windows and macOS file systems are case-insensitive by default.
    if cfg!(any(windows, target_os = "macos")) {
        PathBuf::from(p.to_string_lossy().to_lowercase())
    } else {
        p
    }
}

/// Expand directories (one level) into the supported files they contain.
fn expand_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&p)
                .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|f| f.is_file() && is_supported(f)).collect())
                .unwrap_or_default();
            files.sort();
            out.extend(files);
        } else {
            out.push(p);
        }
    }
    out
}

impl Queue {
    fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> AddOutcome {
        let mut outcome = AddOutcome::default();
        let mut keys: Vec<PathBuf> = self.items.iter().map(|i| dedupe_key(&i.path)).collect();
        for p in expand_paths(paths) {
            if !is_supported(&p) {
                outcome.unsupported.push(p);
                continue;
            }
            let key = dedupe_key(&p);
            if keys.contains(&key) {
                outcome.duplicates += 1;
                continue;
            }
            keys.push(key);
            self.items.push(QueueItem { id: self.next_id, path: p, status: Status::Pending, svg: SvgExport::None });
            self.next_id += 1;
            outcome.added += 1;
        }
        outcome
    }

    fn clear(&mut self) {
        self.items.clear();
    }

    fn remove(&mut self, id: u64) {
        self.items.retain(|i| i.id != id);
    }

    /// Items that the next "Convert" should process (pending or failed).
    fn jobs(&self) -> Vec<(u64, PathBuf)> {
        self.items
            .iter()
            .filter(|i| matches!(i.status, Status::Pending | Status::Failed(_)))
            .map(|i| (i.id, i.path.clone()))
            .collect()
    }

    fn set_status(&mut self, id: u64, status: Status) {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            i.status = status;
        }
    }

    fn apply(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::Started(id) => self.set_status(id, Status::Converting),
            WorkerMsg::Finished(id, Ok(r)) => {
                self.set_status(id, Status::Done { outputs: r.outputs, warnings: r.warnings })
            }
            WorkerMsg::Finished(id, Err(e)) => self.set_status(id, Status::Failed(e)),
            WorkerMsg::AllDone => {}
        }
    }

    fn counts(&self) -> (usize, usize, usize) {
        let done = self.items.iter().filter(|i| matches!(i.status, Status::Done { .. })).count();
        let failed = self.items.iter().filter(|i| matches!(i.status, Status::Failed(_))).count();
        (self.items.len(), done, failed)
    }
}

#[derive(Debug)]
enum WorkerMsg {
    Started(u64),
    Finished(u64, Result<ConvertReport, String>),
    AllDone,
}

#[derive(Debug, Clone, PartialEq)]
enum OutputLocation {
    SameAsSource,
    Folder(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
enum GsStatus {
    Found { path: PathBuf, version: Option<String> },
    NotFound,
    Invalid { path: PathBuf, error: String },
}

impl GsStatus {
    fn detect() -> Self {
        match ghostscript::find() {
            Some(path) => Self::check(path),
            None => Self::NotFound,
        }
    }

    fn check(path: PathBuf) -> Self {
        match ghostscript::version(&path) {
            Ok(v) => Self::Found { path, version: Some(v) },
            Err(e) => Self::Invalid { path, error: format!("{e:#}") },
        }
    }

    fn path(&self) -> Option<&Path> {
        match self {
            Self::Found { path, .. } => Some(path),
            _ => None,
        }
    }
}

fn color_mode_label(m: ColorMode) -> &'static str {
    match m {
        ColorMode::Preserve => "Preserve",
        ColorMode::Cmyk => "CMYK",
        ColorMode::Rgb => "RGB",
    }
}

fn pattern_mode_label(m: PatternMode) -> &'static str {
    match m {
        PatternMode::Native => "Native Illustrator patterns",
        PatternMode::Expand => "Expand to paths (for cutters)",
    }
}

fn build_options(
    location: &OutputLocation,
    color: ColorMode,
    patterns: PatternMode,
    outline: bool,
    gs: &GsStatus,
) -> ConvertOptions {
    ConvertOptions {
        color_mode: color,
        pattern_mode: patterns,
        outline_text_in_pdf: outline,
        out_dir: match location {
            OutputLocation::SameAsSource => None,
            OutputLocation::Folder(p) => Some(p.clone()),
        },
        ghostscript: gs.path().map(Path::to_path_buf),
    }
}

/// Folder to show for a finished item.
fn output_folder(outputs: &[PathBuf], source: &Path) -> Option<PathBuf> {
    outputs
        .first()
        .and_then(|o| o.parent())
        .or_else(|| source.parent())
        .map(Path::to_path_buf)
}

/// Open a file or folder with the system's default application.
fn open_path(dir: &Path) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(not(any(windows, target_os = "macos")))]
    let cmd = "xdg-open";
    if let Err(e) = std::process::Command::new(cmd).arg(dir).spawn() {
        eprintln!("could not open {}: {e}", dir.display());
    }
}

/// Convert one file; a panic in a parser must not take the whole app down.
fn convert_guarded(path: &Path, opts: &ConvertOptions, format: OutputFormat) -> Result<ConvertReport, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| convert_file_to(path, opts, format)))
        .map_err(|p| {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown error".into());
            format!("internal error: {msg}")
        })
        .and_then(|r| r.map_err(|e| format!("{e:#}")))
}

fn spawn_worker(jobs: Vec<(u64, PathBuf)>, opts: ConvertOptions, tx: Sender<WorkerMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        for (id, path) in jobs {
            let _ = tx.send(WorkerMsg::Started(id));
            ctx.request_repaint();
            let result = convert_guarded(&path, &opts, OutputFormat::Ai8);
            let _ = tx.send(WorkerMsg::Finished(id, result));
            ctx.request_repaint();
        }
        let _ = tx.send(WorkerMsg::AllDone);
        ctx.request_repaint();
    });
}

// ---------------------------------------------------------------------------
// Preview (testable part)

/// What the preview window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewView {
    Converted,
    Original,
    SideBySide,
}

/// Rendered images for one preview: the AI8 output and the source page.
#[derive(Debug)]
struct PreviewImages {
    converted: Result<preview::Raster, String>,
    original: Result<preview::Raster, String>,
}

/// Render an output file (through our AI8 procset) and the matching page of
/// its source file. Runs on a worker thread.
fn render_preview(gs: Option<&Path>, ai: &Path, source: &Path, page: usize, dpi: u32) -> PreviewImages {
    let err = |e: anyhow::Error| format!("{e:#}");
    let converted = match gs {
        Some(gs) => preview::render_ai8_png(gs, ai, dpi).and_then(|b| preview::decode_png(&b)).map_err(err),
        None => Err("The preview needs Ghostscript (see the Ghostscript line above).".to_string()),
    };
    let original = preview::render_input_png(gs, source, page, dpi).and_then(|b| preview::decode_png(&b)).map_err(err);
    PreviewImages { converted, original }
}

/// Where "Open as PDF" writes the PDF rendering of `ai`:
/// `<temp>/ai8-converter-preview/<name>-preview.pdf`.
fn preview_pdf_path(temp_root: &Path, ai: &Path) -> PathBuf {
    let stem = ai.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "output".into());
    temp_root.join("ai8-converter-preview").join(format!("{stem}-preview.pdf"))
}

/// Write the PDF rendering of `ai` (for viewers such as macOS Preview, which
/// cannot open PostScript-based .ai files) and return its path.
fn write_preview_pdf(gs: Option<&Path>, ai: &Path, temp_root: &Path) -> Result<PathBuf, String> {
    let gs = gs.ok_or_else(|| "Opening as PDF needs Ghostscript.".to_string())?;
    let pdf = preview_pdf_path(temp_root, ai);
    if let Some(dir) = pdf.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    preview::ai8_to_pdf(gs, ai, &pdf).map_err(|e| format!("{e:#}"))?;
    Ok(pdf)
}

/// Scale that fits an image of `img` points into `avail` (never above 1).
fn fit_scale(img: egui::Vec2, avail: egui::Vec2) -> f32 {
    if img.x <= 0.0 || img.y <= 0.0 {
        return 1.0;
    }
    (avail.x / img.x).min(avail.y / img.y).clamp(0.05, 1.0)
}

// ---------------------------------------------------------------------------
// GUI

/// The preview window of one output file.
struct PreviewWindow {
    title: String,
    ai: PathBuf,
    open: bool,
    view: PreviewView,
    /// None = fit to the window.
    zoom: Option<f32>,
    rx: Option<Receiver<PreviewImages>>,
    images: Option<PreviewImages>,
    textures: [Option<egui::TextureHandle>; 2],
}

impl PreviewWindow {
    fn start(ctx: &egui::Context, gs: Option<PathBuf>, ai: PathBuf, source: PathBuf, page: usize) -> Self {
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let (ai2, source2) = (ai.clone(), source.clone());
        std::thread::spawn(move || {
            let images = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                render_preview(gs.as_deref(), &ai2, &source2, page, PREVIEW_DPI)
            }))
            .unwrap_or_else(|_| PreviewImages {
                converted: Err("internal error while rendering".into()),
                original: Err("internal error while rendering".into()),
            });
            let _ = tx.send(images);
            ctx2.request_repaint();
        });
        let name = ai.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self {
            title: format!("Preview: {name}"),
            ai,
            open: true,
            view: PreviewView::Converted,
            zoom: None,
            rx: Some(rx),
            images: None,
            textures: [None, None],
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        if let Ok(images) = rx.try_recv() {
            let tex = |r: &Result<preview::Raster, String>, name: &str| {
                r.as_ref().ok().map(|r| {
                    let img = egui::ColorImage::from_rgba_unmultiplied([r.width as usize, r.height as usize], &r.rgba);
                    ctx.load_texture(name, img, egui::TextureOptions::LINEAR)
                })
            };
            self.textures = [tex(&images.converted, "preview-converted"), tex(&images.original, "preview-original")];
            self.images = Some(images);
            self.rx = None;
        }
    }

    /// One image with its caption; `scale` maps image pixels to points.
    fn image_ui(ui: &mut egui::Ui, caption: &str, tex: &Option<egui::TextureHandle>, err: Option<&String>, scale: f32) {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(caption).strong());
            match (tex, err) {
                (Some(t), _) => {
                    let size = t.size_vec2() * scale;
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, egui::Color32::WHITE);
                    ui.painter().image(
                        t.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                (None, Some(e)) => {
                    ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(200, 50, 50)));
                }
                (None, None) => {}
            }
        });
    }

    fn ui(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
        self.poll(ctx);
        let mut open = self.open;
        egui::Window::new(self.title.clone())
            .id(egui::Id::unique("preview_window"))
            .open(&mut open)
            .default_size([640.0, 520.0])
            .resizable(true)
            .vscroll(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.view, PreviewView::Converted, "Converted (.ai)");
                    ui.selectable_value(&mut self.view, PreviewView::Original, "Original");
                    ui.selectable_value(&mut self.view, PreviewView::SideBySide, "Side by side");
                    ui.separator();
                    if ui.selectable_label(self.zoom.is_none(), "Fit").clicked() {
                        self.zoom = None;
                    }
                    for (label, z) in [("100%", 1.0), ("200%", 2.0), ("400%", 4.0)] {
                        if ui.selectable_label(self.zoom == Some(z), label).clicked() {
                            self.zoom = Some(z);
                        }
                    }
                    ui.separator();
                    if ui.button("Open as PDF").on_hover_text("Open a PDF rendering in the system PDF viewer").clicked() {
                        actions.push(UiAction::OpenPdf(self.ai.clone()));
                    }
                });
                ui.separator();
                let Some(images) = &self.images else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Rendering…");
                    });
                    return;
                };
                let px_per_pt = PREVIEW_DPI as f32 / 72.0;
                let shown: Vec<usize> = match self.view {
                    PreviewView::Converted => vec![0],
                    PreviewView::Original => vec![1],
                    PreviewView::SideBySide => vec![1, 0],
                };
                // Size in points of what is shown, for "Fit".
                let mut total = egui::Vec2::ZERO;
                for &i in &shown {
                    if let Some(t) = &self.textures[i] {
                        let s = t.size_vec2() / px_per_pt;
                        total.x += s.x + 12.0;
                        total.y = total.y.max(s.y + 24.0);
                    }
                }
                let zoom = self.zoom.unwrap_or_else(|| fit_scale(total, ui.available_size()));
                let scale = zoom / px_per_pt;
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        for &i in &shown {
                            let (caption, err) = if i == 0 {
                                ("Converted (Illustrator 8)", images.converted.as_ref().err())
                            } else {
                                ("Original", images.original.as_ref().err())
                            };
                            Self::image_ui(ui, caption, &self.textures[i], err, scale);
                            ui.add_space(12.0);
                        }
                    });
                });
            });
        self.open = open;
    }
}

/// Actions requested by the UI, handled after drawing.
enum UiAction {
    Preview { ai: PathBuf, source: PathBuf, page: usize },
    OpenPdf(PathBuf),
    SaveSvg(u64),
}

struct App {
    queue: Queue,
    location: OutputLocation,
    color: ColorMode,
    patterns: PatternMode,
    outline_text: bool,
    gs: GsStatus,
    worker: Option<Receiver<WorkerMsg>>,
    notice: Option<String>,
    preview: Option<PreviewWindow>,
    pdf_jobs: Vec<Receiver<Result<PathBuf, String>>>,
    svg_jobs: Vec<(u64, Receiver<Result<ConvertReport, String>>)>,
}

impl App {
    fn new() -> Self {
        Self {
            queue: Queue::default(),
            location: OutputLocation::SameAsSource,
            color: ColorMode::Preserve,
            patterns: PatternMode::Native,
            outline_text: true,
            gs: GsStatus::detect(),
            worker: None,
            notice: None,
            preview: None,
            pdf_jobs: Vec::new(),
            svg_jobs: Vec::new(),
        }
    }

    fn busy(&self) -> bool {
        self.worker.is_some()
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        let o = self.queue.add(paths);
        let mut parts = Vec::new();
        if o.duplicates > 0 {
            parts.push(format!("{} already in the list", o.duplicates));
        }
        if !o.unsupported.is_empty() {
            let names: Vec<String> = o
                .unsupported
                .iter()
                .take(3)
                .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                .collect();
            let more = if o.unsupported.len() > 3 { ", …" } else { "" };
            parts.push(format!("skipped unsupported: {}{more}", names.join(", ")));
        }
        self.notice = if parts.is_empty() { None } else { Some(parts.join("; ")) };
    }

    fn start(&mut self, ctx: &egui::Context) {
        let jobs = self.queue.jobs();
        if jobs.is_empty() {
            return;
        }
        for (id, _) in &jobs {
            self.queue.set_status(*id, Status::Pending);
        }
        let (tx, rx) = channel();
        let opts = build_options(&self.location, self.color, self.patterns, self.outline_text, &self.gs);
        spawn_worker(jobs, opts, tx, ctx.clone());
        self.worker = Some(rx);
        self.notice = None;
    }

    fn poll_worker(&mut self) {
        let Some(rx) = &self.worker else { return };
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(WorkerMsg::AllDone) => finished = true,
                Ok(msg) => self.queue.apply(msg),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.worker = None;
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Add files…").clicked()
                    && let Some(files) = rfd::FileDialog::new()
                        .set_title("Add files to convert")
                        .add_filter("Vector files", DIALOG_EXTENSIONS)
                        .add_filter("All files", &["*"])
                        .pick_files()
                {
                    self.add_paths(files);
                }
                if ui.add_enabled(!self.queue.items.is_empty(), egui::Button::new("Clear")).clicked() {
                    self.queue.clear();
                    self.notice = None;
                }
                ui.label(egui::RichText::new("or drop files onto this window").weak());
            });
            ui.add_space(4.0);
            egui::Grid::new("settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Output:");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.location, OutputLocation::SameAsSource, "Same folder as source");
                    let chosen = matches!(self.location, OutputLocation::Folder(_));
                    let label = match &self.location {
                        OutputLocation::Folder(p) => p.display().to_string(),
                        OutputLocation::SameAsSource => "Choose folder…".into(),
                    };
                    if ui.radio(chosen, label).clicked()
                        && let Some(dir) = rfd::FileDialog::new().set_title("Output folder").pick_folder()
                    {
                        self.location = OutputLocation::Folder(dir);
                    }
                });
                ui.end_row();

                ui.label("Colours:");
                egui::ComboBox::from_id_salt("color_mode").selected_text(color_mode_label(self.color)).show_ui(
                    ui,
                    |ui| {
                        for m in [ColorMode::Preserve, ColorMode::Cmyk, ColorMode::Rgb] {
                            ui.selectable_value(&mut self.color, m, color_mode_label(m));
                        }
                    },
                );
                ui.end_row();

                ui.label("Patterns:");
                ui.horizontal(|ui| {
                    for m in [PatternMode::Native, PatternMode::Expand] {
                        ui.radio_value(&mut self.patterns, m, pattern_mode_label(m)).on_hover_text(match m {
                            PatternMode::Native => "Pattern fills stay editable pattern swatches in Illustrator",
                            PatternMode::Expand => {
                                "Replace pattern fills by plain clipped tile paths, for cutters, plotters and RIPs \
                                 that do not understand pattern fills"
                            }
                        });
                    }
                });
                ui.end_row();

                ui.label("Text:");
                ui.checkbox(&mut self.outline_text, "Outline text in PDF/AI files (needs Ghostscript)");
                ui.end_row();

                ui.label("Ghostscript:");
                self.gs_ui(ui);
                ui.end_row();
            });
        });
    }

    fn gs_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            match &self.gs {
                GsStatus::Found { path, version } => {
                    let v = version.as_deref().map(|v| format!(" ({v})")).unwrap_or_default();
                    ui.label(egui::RichText::new("✔ found").color(egui::Color32::from_rgb(40, 160, 70)));
                    ui.label(egui::RichText::new(format!("{}{v}", path.display())).small());
                }
                GsStatus::NotFound => {
                    ui.label(egui::RichText::new("not found").color(egui::Color32::from_rgb(210, 120, 0)));
                    let hint = if cfg!(windows) {
                        "EPS/PS needs Ghostscript: install from ghostscript.com"
                    } else if cfg!(target_os = "macos") {
                        "EPS/PS needs Ghostscript: brew install ghostscript"
                    } else {
                        "EPS/PS needs Ghostscript: install the ghostscript package"
                    };
                    ui.label(egui::RichText::new(hint).small());
                }
                GsStatus::Invalid { path, error } => {
                    ui.label(egui::RichText::new("not usable").color(egui::Color32::from_rgb(200, 50, 50)))
                        .on_hover_text(error.as_str());
                    ui.label(egui::RichText::new(path.display().to_string()).small());
                }
            }
            if ui.small_button("Browse…").clicked() {
                let mut dlg = rfd::FileDialog::new().set_title("Locate the Ghostscript executable");
                if cfg!(windows) {
                    dlg = dlg.add_filter("Ghostscript", &["exe"]);
                }
                if let Some(p) = dlg.pick_file() {
                    self.gs = GsStatus::check(p);
                }
            }
            if ui.small_button("Re-detect").clicked() {
                self.gs = GsStatus::detect();
            }
        });
    }

    fn queue_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
        if self.queue.items.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Drop EPS, PS, PDF, AI, SVG or DXF files here\nor use “Add files…”")
                        .size(16.0)
                        .weak(),
                );
            });
            return;
        }
        let busy = self.busy();
        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for item in &self.queue.items {
                ui.horizontal(|ui| {
                    let name = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let (text, color) = match &item.status {
                        Status::Pending => ("Pending", ui.visuals().weak_text_color()),
                        Status::Converting => ("Converting…", egui::Color32::from_rgb(60, 130, 220)),
                        Status::Done { .. } => ("Done", egui::Color32::from_rgb(40, 160, 70)),
                        Status::Failed(_) => ("Failed", egui::Color32::from_rgb(200, 50, 50)),
                    };
                    if matches!(item.status, Status::Converting) {
                        ui.spinner();
                    }
                    ui.label(egui::RichText::new(text).color(color).strong());
                    ui.label(name).on_hover_text(item.path.display().to_string());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !busy && ui.small_button("✖").on_hover_text("Remove from list").clicked() {
                            remove = Some(item.id);
                        }
                        let can_save_svg = !matches!(item.status, Status::Converting) && item.svg != SvgExport::Saving;
                        if ui
                            .add_enabled(can_save_svg, egui::Button::new("Save as SVG").small())
                            .on_hover_text(
                                "Save an SVG with the same cleaned-up paths as the .ai file \
                                 (an SVG file is saved as a new copy, name_fixed.svg)",
                            )
                            .clicked()
                        {
                            actions.push(UiAction::SaveSvg(item.id));
                        }
                        if let Status::Done { outputs, .. } = &item.status
                            && ui.small_button("Open output folder").clicked()
                            && let Some(dir) = output_folder(outputs, &item.path)
                        {
                            open_path(&dir);
                        }
                    });
                });
                match &item.svg {
                    SvgExport::None => {}
                    SvgExport::Saving => {
                        ui.indent(("svg", item.id), |ui| {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(egui::RichText::new("Saving SVG…").small());
                            });
                        });
                    }
                    SvgExport::Saved(paths) => {
                        ui.indent(("svg", item.id), |ui| {
                            for p in paths {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(format!("→ {} (SVG)", p.display())).small());
                                    if ui.small_button("Open").on_hover_text("Open the SVG in the default app").clicked() {
                                        open_path(p);
                                    }
                                });
                            }
                        });
                    }
                    SvgExport::Failed(e) => {
                        ui.indent(("svg", item.id), |ui| {
                            ui.label(
                                egui::RichText::new(format!("SVG export failed: {e}"))
                                    .small()
                                    .color(egui::Color32::from_rgb(200, 50, 50)),
                            );
                        });
                    }
                }
                match &item.status {
                    Status::Done { outputs, warnings } => {
                        ui.indent(("out", item.id), |ui| {
                            for (page, o) in outputs.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(format!("→ {}", o.display())).small());
                                    if ui.small_button("Preview").on_hover_text("Show the converted file and the original").clicked() {
                                        actions.push(UiAction::Preview { ai: o.clone(), source: item.path.clone(), page });
                                    }
                                    if ui
                                        .small_button("Open as PDF")
                                        .on_hover_text("Open a PDF rendering of the .ai file in the system PDF viewer")
                                        .clicked()
                                    {
                                        actions.push(UiAction::OpenPdf(o.clone()));
                                    }
                                });
                            }
                            if !warnings.is_empty() {
                                egui::CollapsingHeader::new(
                                    egui::RichText::new(format!("{} warning(s)", warnings.len()))
                                        .small()
                                        .color(egui::Color32::from_rgb(210, 120, 0)),
                                )
                                .id_salt(("warn", item.id))
                                .default_open(false)
                                .show(ui, |ui| {
                                    for w in warnings {
                                        ui.label(egui::RichText::new(w).small());
                                    }
                                });
                            }
                        });
                    }
                    Status::Failed(e) => {
                        ui.indent(("err", item.id), |ui| {
                            ui.label(egui::RichText::new(e).small().color(egui::Color32::from_rgb(200, 50, 50)));
                        });
                    }
                    _ => {}
                }
                ui.separator();
            }
        });
        if let Some(id) = remove {
            self.queue.remove(id);
        }
    }

    fn actions_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let jobs = self.queue.jobs().len();
            let label = if self.busy() { "Converting…".to_string() } else { format!("Convert ({jobs})") };
            let btn = egui::Button::new(egui::RichText::new(label).strong()).min_size(egui::vec2(140.0, 28.0));
            if ui.add_enabled(!self.busy() && jobs > 0, btn).clicked() {
                self.start(ui.ctx());
            }
            let (total, done, failed) = self.queue.counts();
            if total > 0 {
                ui.label(format!("{total} file(s) · {done} done · {failed} failed"));
            }
            if let Some(n) = &self.notice {
                ui.label(egui::RichText::new(n).small().weak());
            }
        });
    }

    fn handle_actions(&mut self, ctx: &egui::Context, actions: Vec<UiAction>) {
        let gs = self.gs.path().map(Path::to_path_buf);
        for a in actions {
            match a {
                UiAction::Preview { ai, source, page } => {
                    self.preview = Some(PreviewWindow::start(ctx, gs.clone(), ai, source, page));
                }
                UiAction::SaveSvg(id) => {
                    let Some(item) = self.queue.items.iter_mut().find(|i| i.id == id) else { continue };
                    item.svg = SvgExport::Saving;
                    let (tx, rx) = channel();
                    let path = item.path.clone();
                    let opts = build_options(&self.location, self.color, self.patterns, self.outline_text, &self.gs);
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(convert_guarded(&path, &opts, OutputFormat::Svg));
                        ctx.request_repaint();
                    });
                    self.svg_jobs.push((id, rx));
                }
                UiAction::OpenPdf(ai) => {
                    let (tx, rx) = channel();
                    let (gs, ctx) = (gs.clone(), ctx.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(write_preview_pdf(gs.as_deref(), &ai, &std::env::temp_dir()));
                        ctx.request_repaint();
                    });
                    self.pdf_jobs.push(rx);
                    self.notice = Some("Writing PDF preview…".into());
                }
            }
        }
    }

    fn poll_svg_jobs(&mut self) {
        let queue = &mut self.queue;
        self.svg_jobs.retain(|(id, rx)| {
            let state = match rx.try_recv() {
                Ok(Ok(report)) => SvgExport::Saved(report.outputs),
                Ok(Err(e)) => SvgExport::Failed(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => SvgExport::Failed("internal error".into()),
            };
            // The item may have been removed from the list meanwhile.
            if let Some(item) = queue.items.iter_mut().find(|i| i.id == *id) {
                item.svg = state;
            }
            false
        });
    }

    fn poll_pdf_jobs(&mut self) {
        let mut done = Vec::new();
        for (i, rx) in self.pdf_jobs.iter().enumerate() {
            match rx.try_recv() {
                Ok(Ok(pdf)) => {
                    open_path(&pdf);
                    self.notice = Some(format!("Opened {}", pdf.display()));
                    done.push(i);
                }
                Ok(Err(e)) => {
                    self.notice = Some(format!("PDF preview failed: {e}"));
                    done.push(i);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => done.push(i),
            }
        }
        for i in done.into_iter().rev() {
            self.pdf_jobs.remove(i);
        }
    }

    fn handle_drops(&mut self, ui: &mut egui::Ui) {
        let (dropped, hovering) = ui.ctx().input(|i| {
            (
                i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>(),
                !i.raw.hovered_files.is_empty(),
            )
        });
        if !dropped.is_empty() && !self.busy() {
            self.add_paths(dropped);
        }
        if hovering {
            let ctx = ui.ctx();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop_overlay")));
            let rect = ctx.content_rect();
            painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(160));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                if self.busy() { "Busy — wait for the current conversion" } else { "Drop to add files" },
                egui::FontId::proportional(22.0),
                egui::Color32::WHITE,
            );
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_worker();
        self.poll_pdf_jobs();
        self.poll_svg_jobs();
        let mut actions = Vec::new();
        egui::Panel::top("settings_panel").show(ui, |ui| {
            ui.add_space(6.0);
            self.settings_ui(ui);
            ui.add_space(6.0);
        });
        egui::Panel::bottom("actions_panel").show(ui, |ui| {
            ui.add_space(6.0);
            self.actions_ui(ui);
            ui.add_space(6.0);
        });
        egui::CentralPanel::default().show(ui, |ui| self.queue_ui(ui, &mut actions));
        if let Some(p) = &mut self.preview {
            p.ui(ui.ctx(), &mut actions);
            if !p.open {
                self.preview = None;
            }
        }
        self.handle_actions(&ui.ctx().clone(), actions);
        self.handle_drops(ui);
    }
}

/// Window/taskbar icon, decoded from the PNG embedded at build time.
fn app_icon() -> Option<egui::IconData> {
    let r = preview::decode_png(include_bytes!("../assets/icon-256.png")).ok()?;
    Some(egui::IconData { rgba: r.rgba, width: r.width, height: r.height })
}

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default();
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_title("AI8 Converter")
            .with_inner_size([720.0, 520.0])
            .with_min_inner_size([720.0, 520.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("AI8 Converter", options, Box::new(|_cc| Ok(Box::new(App::new()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ai8-gui-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// "Save as SVG" next to an SVG input writes a new name_fixed.svg copy
    /// and leaves the input untouched; a second save replaces that copy.
    #[test]
    fn save_svg_makes_a_new_copy() {
        let d = tmp("svg");
        let input = d.join("logo.svg");
        let src = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/svg/clip_stroke.svg")).unwrap();
        std::fs::write(&input, &src).unwrap();
        let opts = build_options(&OutputLocation::SameAsSource, ColorMode::Preserve, PatternMode::Native, true, &GsStatus::NotFound);
        for _ in 0..2 {
            let report = convert_guarded(&input, &opts, OutputFormat::Svg).unwrap();
            assert_eq!(report.outputs, vec![d.join("logo_fixed.svg")]);
        }
        assert_eq!(std::fs::read(&input).unwrap(), src);
        let svg = std::fs::read_to_string(d.join("logo_fixed.svg")).unwrap();
        assert!(svg.starts_with("<?xml") && svg.contains("<path d=\"M"), "{svg}");
        assert!(convert_guarded(&d.join("missing.svg"), &opts, OutputFormat::Svg).is_err());
    }

    #[test]
    fn extension_filter() {
        for ok in ["a.eps", "b.EPS", "c.ps", "d.pdf", "e.Ai", "f.svg", "g.svgz", "h.epsf"] {
            assert!(is_supported(Path::new(ok)), "{ok}");
        }
        for bad in ["a.png", "b", "c.txt", "eps"] {
            assert!(!is_supported(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn add_dedupes_and_filters() {
        let mut q = Queue::default();
        let o = q.add(vec!["/x/a.eps".into(), "/x/b.pdf".into(), "/x/a.eps".into(), "/x/c.png".into()]);
        assert_eq!(o.added, 2);
        assert_eq!(o.duplicates, 1);
        assert_eq!(o.unsupported, vec![PathBuf::from("/x/c.png")]);
        let o = q.add(vec!["/x/b.pdf".into()]);
        assert_eq!(o.added, 0);
        assert_eq!(o.duplicates, 1);
        assert_eq!(q.items.len(), 2);
        assert!(q.items.iter().all(|i| i.status == Status::Pending));
        // ids are unique
        assert_ne!(q.items[0].id, q.items[1].id);
    }

    #[test]
    fn dedupe_resolves_real_paths() {
        let d = tmp("dedupe");
        std::fs::write(d.join("x.svg"), "<svg/>").unwrap();
        let mut q = Queue::default();
        q.add(vec![d.join("x.svg")]);
        let o = q.add(vec![d.join(".").join("x.svg")]);
        assert_eq!(o.duplicates, 1);
    }

    #[test]
    fn directories_expand_to_supported_files() {
        let d = tmp("dir");
        for f in ["b.eps", "a.svg", "note.txt"] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        let mut q = Queue::default();
        let o = q.add(vec![d.clone()]);
        assert_eq!(o.added, 2);
        let names: Vec<_> = q.items.iter().map(|i| i.path.file_name().unwrap().to_owned()).collect();
        assert_eq!(names, vec!["a.svg", "b.eps"]);
    }

    #[test]
    fn status_transitions_and_jobs() {
        let mut q = Queue::default();
        q.add(vec!["/x/a.eps".into(), "/x/b.eps".into(), "/x/c.eps".into()]);
        let ids: Vec<u64> = q.items.iter().map(|i| i.id).collect();
        assert_eq!(q.jobs().len(), 3);

        q.apply(WorkerMsg::Started(ids[0]));
        assert_eq!(q.items[0].status, Status::Converting);
        let report = ConvertReport { outputs: vec!["/x/a.ai".into()], warnings: vec!["w".into()], paths_written: 3 };
        q.apply(WorkerMsg::Finished(ids[0], Ok(report)));
        assert_eq!(q.items[0].status, Status::Done { outputs: vec!["/x/a.ai".into()], warnings: vec!["w".into()] });
        q.apply(WorkerMsg::Finished(ids[1], Err("boom".into())));
        assert_eq!(q.items[1].status, Status::Failed("boom".into()));
        q.apply(WorkerMsg::AllDone);

        // Done items are not re-run; failed and pending are.
        let jobs: Vec<u64> = q.jobs().into_iter().map(|(id, _)| id).collect();
        assert_eq!(jobs, vec![ids[1], ids[2]]);
        assert_eq!(q.counts(), (3, 1, 1));

        // Unknown id is ignored.
        q.set_status(9999, Status::Converting);
        q.remove(ids[0]);
        assert_eq!(q.items.len(), 2);
        q.clear();
        assert!(q.items.is_empty());
    }

    #[test]
    fn options_mapping() {
        let gs = GsStatus::Found { path: "/usr/bin/gs".into(), version: None };
        let o = build_options(&OutputLocation::Folder("/out".into()), ColorMode::Rgb, PatternMode::Expand, false, &gs);
        assert_eq!(o.out_dir, Some(PathBuf::from("/out")));
        assert_eq!(o.color_mode, ColorMode::Rgb);
        assert_eq!(o.pattern_mode, PatternMode::Expand);
        assert!(!o.outline_text_in_pdf);
        assert_eq!(o.ghostscript, Some(PathBuf::from("/usr/bin/gs")));
        let o = build_options(&OutputLocation::SameAsSource, ColorMode::Cmyk, PatternMode::Native, true, &GsStatus::NotFound);
        assert_eq!(o.pattern_mode, PatternMode::Native);
        assert_eq!(o.out_dir, None);
        assert_eq!(o.ghostscript, None);
    }

    #[test]
    fn output_folder_prefers_output_parent() {
        assert_eq!(output_folder(&["/o/a.ai".into()], Path::new("/s/a.eps")), Some(PathBuf::from("/o")));
        assert_eq!(output_folder(&[], Path::new("/s/a.eps")), Some(PathBuf::from("/s")));
    }

    #[test]
    fn dxf_is_supported() {
        assert!(is_supported(Path::new("plan.DXF")));
        assert!(DIALOG_EXTENSIONS.contains(&"dxf"));
    }

    #[test]
    fn preview_pdf_goes_to_temp_subfolder() {
        let p = preview_pdf_path(Path::new("/tmp"), Path::new("/out/logo_p2.ai"));
        assert_eq!(p, PathBuf::from("/tmp/ai8-converter-preview/logo_p2-preview.pdf"));
    }

    #[test]
    fn fit_scale_never_enlarges() {
        assert_eq!(fit_scale(egui::vec2(100.0, 50.0), egui::vec2(400.0, 400.0)), 1.0);
        assert_eq!(fit_scale(egui::vec2(800.0, 100.0), egui::vec2(400.0, 400.0)), 0.5);
        assert_eq!(fit_scale(egui::Vec2::ZERO, egui::vec2(400.0, 400.0)), 1.0);
    }

    #[test]
    fn preview_without_ghostscript_reports_errors() {
        let d = tmp("preview-nogs");
        let svg = d.join("a.svg");
        std::fs::write(&svg, r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="10" height="10"/></svg>"#).unwrap();
        let r = render_preview(None, &d.join("a.ai"), &svg, 0, 72);
        assert!(r.converted.unwrap_err().contains("Ghostscript"));
        // SVG originals are rendered without Ghostscript.
        let orig = r.original.unwrap();
        assert_eq!((orig.width, orig.height), (15, 8));
        assert!(write_preview_pdf(None, &d.join("a.ai"), &d).unwrap_err().contains("Ghostscript"));
    }

    #[test]
    fn preview_renders_output_and_original() {
        let Some(gs) = ghostscript::find() else { return };
        let d = tmp("preview-gs");
        let svg = d.join("b.svg");
        std::fs::write(&svg, r#"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="48"><rect width="48" height="48" fill="red"/></svg>"#).unwrap();
        let opts = ConvertOptions { out_dir: Some(d.clone()), ghostscript: Some(gs.clone()), ..Default::default() };
        let report = ai8_converter::convert_file(&svg, &opts).unwrap();
        let r = render_preview(Some(&gs), &report.outputs[0], &svg, 0, 72);
        let (c, o) = (r.converted.unwrap(), r.original.unwrap());
        assert_eq!((c.width, c.height), (72, 36));
        assert_eq!((o.width, o.height), (72, 36));
        assert_eq!(&c.pixel(10, 10)[..3], &[255, 0, 0]);
        assert_eq!(&c.pixel(60, 10)[..3], &[255, 255, 255]);
        let pdf = write_preview_pdf(Some(&gs), &report.outputs[0], &d).unwrap();
        assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
    }

    #[test]
    fn worker_reports_progress() {
        let d = tmp("worker");
        let bad = d.join("bad.svg");
        std::fs::write(&bad, "not svg at all").unwrap();
        let (tx, rx) = channel();
        spawn_worker(vec![(7, bad), (8, d.join("missing.eps"))], ConvertOptions::default(), tx, egui::Context::default());
        let msgs: Vec<WorkerMsg> = rx.iter().collect();
        assert!(matches!(msgs.first(), Some(WorkerMsg::Started(7))));
        assert!(matches!(msgs.last(), Some(WorkerMsg::AllDone)));
        let mut q = Queue::default();
        q.items.push(QueueItem { id: 8, path: "missing.eps".into(), status: Status::Pending, svg: SvgExport::None });
        for m in msgs {
            q.apply(m);
        }
        assert!(matches!(q.items[0].status, Status::Failed(_)));
    }
}
