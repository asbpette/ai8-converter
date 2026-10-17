//! AI8 Converter desktop GUI.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use ai8_converter::{ColorMode, ConvertOptions, ConvertReport, convert_file, ghostscript};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

/// File extensions accepted by the converter (lower case).
const SUPPORTED_EXTENSIONS: &[&str] = &["eps", "epsf", "epsi", "ps", "pdf", "ai", "svg", "svgz"];
/// Extensions offered in the "Add files…" dialog filter.
const DIALOG_EXTENSIONS: &[&str] = &["eps", "ps", "pdf", "ai", "svg", "svgz"];

// ---------------------------------------------------------------------------
// Testable state model

#[derive(Debug, Clone, PartialEq)]
enum Status {
    Pending,
    Converting,
    Done { outputs: Vec<PathBuf>, warnings: Vec<String> },
    Failed(String),
}

#[derive(Debug, Clone)]
struct QueueItem {
    id: u64,
    path: PathBuf,
    status: Status,
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
            self.items.push(QueueItem { id: self.next_id, path: p, status: Status::Pending });
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

fn build_options(location: &OutputLocation, color: ColorMode, outline: bool, gs: &GsStatus) -> ConvertOptions {
    ConvertOptions {
        color_mode: color,
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

fn open_folder(dir: &Path) {
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

fn spawn_worker(jobs: Vec<(u64, PathBuf)>, opts: ConvertOptions, tx: Sender<WorkerMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        for (id, path) in jobs {
            let _ = tx.send(WorkerMsg::Started(id));
            ctx.request_repaint();
            // A panic in a parser must not take the whole app down.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| convert_file(&path, &opts)))
                .map_err(|p| {
                    let msg = p
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "unknown error".into());
                    format!("internal error: {msg}")
                })
                .and_then(|r| r.map_err(|e| format!("{e:#}")));
            let _ = tx.send(WorkerMsg::Finished(id, result));
            ctx.request_repaint();
        }
        let _ = tx.send(WorkerMsg::AllDone);
        ctx.request_repaint();
    });
}

// ---------------------------------------------------------------------------
// GUI

struct App {
    queue: Queue,
    location: OutputLocation,
    color: ColorMode,
    outline_text: bool,
    gs: GsStatus,
    worker: Option<Receiver<WorkerMsg>>,
    notice: Option<String>,
}

impl App {
    fn new() -> Self {
        Self {
            queue: Queue::default(),
            location: OutputLocation::SameAsSource,
            color: ColorMode::Preserve,
            outline_text: true,
            gs: GsStatus::detect(),
            worker: None,
            notice: None,
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
        let opts = build_options(&self.location, self.color, self.outline_text, &self.gs);
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

    fn queue_ui(&mut self, ui: &mut egui::Ui) {
        if self.queue.items.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Drop EPS, PS, PDF, AI or SVG files here\nor use “Add files…”")
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
                        if let Status::Done { outputs, .. } = &item.status
                            && ui.small_button("Open output folder").clicked()
                            && let Some(dir) = output_folder(outputs, &item.path)
                        {
                            open_folder(&dir);
                        }
                    });
                });
                match &item.status {
                    Status::Done { outputs, warnings } => {
                        ui.indent(("out", item.id), |ui| {
                            for o in outputs {
                                ui.label(egui::RichText::new(format!("→ {}", o.display())).small());
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
        egui::CentralPanel::default().show(ui, |ui| self.queue_ui(ui));
        self.handle_drops(ui);
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
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
        let o = build_options(&OutputLocation::Folder("/out".into()), ColorMode::Rgb, false, &gs);
        assert_eq!(o.out_dir, Some(PathBuf::from("/out")));
        assert_eq!(o.color_mode, ColorMode::Rgb);
        assert!(!o.outline_text_in_pdf);
        assert_eq!(o.ghostscript, Some(PathBuf::from("/usr/bin/gs")));
        let o = build_options(&OutputLocation::SameAsSource, ColorMode::Cmyk, true, &GsStatus::NotFound);
        assert_eq!(o.out_dir, None);
        assert_eq!(o.ghostscript, None);
    }

    #[test]
    fn output_folder_prefers_output_parent() {
        assert_eq!(output_folder(&["/o/a.ai".into()], Path::new("/s/a.eps")), Some(PathBuf::from("/o")));
        assert_eq!(output_folder(&[], Path::new("/s/a.eps")), Some(PathBuf::from("/s")));
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
        q.items.push(QueueItem { id: 8, path: "missing.eps".into(), status: Status::Pending });
        for m in msgs {
            q.apply(m);
        }
        assert!(matches!(q.items[0].status, Status::Failed(_)));
    }
}
