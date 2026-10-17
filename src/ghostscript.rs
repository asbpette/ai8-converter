//! Locating and running Ghostscript, used to interpret EPS/PostScript
//! (and optionally to outline text in PDFs) by converting them to PDF.

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Environment variable that overrides Ghostscript detection.
pub const ENV_VAR: &str = "AI8_GHOSTSCRIPT";

/// Default time limit for one Ghostscript run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Executable names searched for on `PATH`, in order of preference.
#[cfg(windows)]
const PATH_NAMES: &[&str] = &["gswin64c.exe", "gswin32c.exe", "gs.exe"];
#[cfg(not(windows))]
const PATH_NAMES: &[&str] = &["gs"];

/// Find a Ghostscript executable (bundled next to the app, on PATH, or in the
/// standard install locations on Windows/macOS/Linux).
///
/// Order: `$AI8_GHOSTSCRIPT`, a copy bundled next to the executable (or in the
/// macOS `.app` bundle's `Contents/Resources`), `PATH`, standard locations.
pub fn find() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(ENV_VAR).filter(|v| !v.is_empty()).map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    if let Some(p) = exe_dir.as_deref().and_then(|d| bundled_candidates(d).into_iter().find(|p| p.is_file())) {
        return Some(p);
    }
    if let Some(p) = find_on_path(std::env::var_os("PATH")) {
        return Some(p);
    }
    standard_locations().into_iter().find(|p| p.is_file())
}

/// Places where a bundled Ghostscript may live relative to the directory
/// containing the application executable.
pub fn bundled_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for name in PATH_NAMES {
        out.push(exe_dir.join("gs").join("bin").join(name));
        out.push(exe_dir.join("gs").join(name));
        out.push(exe_dir.join(name));
    }
    // macOS bundle: <App>.app/Contents/MacOS/<exe> -> Contents/Resources/gs/bin/gs
    if let Some(contents) = exe_dir.parent() {
        out.push(contents.join("Resources").join("gs").join("bin").join("gs"));
    }
    out
}

/// Search a `PATH`-style variable for a Ghostscript executable.
pub fn find_on_path(path_var: Option<OsString>) -> Option<PathBuf> {
    let path_var = path_var?;
    for name in PATH_NAMES {
        for dir in std::env::split_paths(&path_var) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Standard installation locations for the current platform.
pub fn standard_locations() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        let mut roots: Vec<PathBuf> = Vec::new();
        for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(v) = std::env::var_os(var) {
                let p = PathBuf::from(v);
                if !roots.contains(&p) {
                    roots.push(p);
                }
            }
        }
        for fallback in [r"C:\Program Files", r"C:\Program Files (x86)"] {
            let p = PathBuf::from(fallback);
            if !roots.contains(&p) {
                roots.push(p);
            }
        }
        for root in roots {
            let gs_root = root.join("gs");
            let Ok(entries) = std::fs::read_dir(&gs_root) else { continue };
            let mut versions: Vec<(Vec<u32>, PathBuf)> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    parse_gs_dir_version(&name).map(|v| (v, e.path()))
                })
                .collect();
            versions.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
            for (_, dir) in versions {
                for exe in ["gswin64c.exe", "gswin32c.exe"] {
                    out.push(dir.join("bin").join(exe));
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        for p in ["/opt/homebrew/bin/gs", "/usr/local/bin/gs", "/opt/local/bin/gs"] {
            out.push(PathBuf::from(p));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for p in ["/usr/bin/gs", "/usr/local/bin/gs", "/snap/bin/gs"] {
            out.push(PathBuf::from(p));
        }
    }
    out
}

/// Parse a Windows install directory name like `gs10.04.0` into `[10, 4, 0]`.
pub fn parse_gs_dir_version(name: &str) -> Option<Vec<u32>> {
    let rest = name.strip_prefix("gs")?;
    let parts: Option<Vec<u32>> = rest.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|v| !v.is_empty())
}

/// Run `gs --version` and return the trimmed output.
pub fn version(gs: &Path) -> Result<String> {
    let mut cmd = Command::new(gs);
    cmd.arg("--version");
    let out = run_with_timeout(cmd, Duration::from_secs(15))?;
    if !out.success {
        bail!("`{} --version` failed: {}", gs.display(), out.combined());
    }
    Ok(out.stdout.trim().to_string())
}

/// Run Ghostscript's pdfwrite device on `input` and return the PDF bytes.
/// `outline_text` converts all text to vector outlines (-dNoOutputFonts);
/// `eps_crop` crops the page to the EPS %%BoundingBox.
pub fn to_pdf(gs: &Path, input: &Path, outline_text: bool, eps_crop: bool) -> Result<Vec<u8>> {
    to_pdf_with_timeout(gs, input, outline_text, eps_crop, DEFAULT_TIMEOUT)
}

/// [`to_pdf`] with an explicit time limit; the Ghostscript process is killed
/// when it is exceeded.
pub fn to_pdf_with_timeout(
    gs: &Path,
    input: &Path,
    outline_text: bool,
    eps_crop: bool,
    timeout: Duration,
) -> Result<Vec<u8>> {
    if !input.is_file() {
        bail!("input file not found: {}", input.display());
    }
    // An absolute path can never be mistaken for a switch ("-x") or an
    // "@argfile" by Ghostscript's command-line parser.
    let input_abs = std::path::absolute(input).unwrap_or_else(|_| input.to_path_buf());
    let tmp = TempFile::new("pdf");

    let mut cmd = Command::new(gs);
    cmd.args(pdfwrite_args(outline_text, eps_crop));
    let mut out_arg = OsString::from("-sOutputFile=");
    out_arg.push(tmp.path.as_os_str());
    cmd.arg(out_arg);
    cmd.arg("-f");
    cmd.arg(&input_abs);

    let out = run_with_timeout(cmd, timeout)
        .with_context(|| format!("running Ghostscript ({})", gs.display()))?;
    if out.timed_out {
        bail!("Ghostscript timed out after {} s while converting {}", timeout.as_secs(), input.display());
    }
    if !out.success {
        bail!("Ghostscript failed to convert {}:\n{}", input.display(), out.combined());
    }
    let bytes = std::fs::read(&tmp.path)
        .with_context(|| format!("Ghostscript produced no output for {}: {}", input.display(), out.combined()))?;
    if bytes.is_empty() || !bytes.starts_with(b"%PDF") {
        bail!("Ghostscript produced an empty or invalid PDF for {}: {}", input.display(), out.combined());
    }
    Ok(bytes)
}

/// The fixed pdfwrite arguments (without output and input file).
pub fn pdfwrite_args(outline_text: bool, eps_crop: bool) -> Vec<&'static str> {
    let mut args = vec![
        "-q",
        "-dNOPAUSE",
        "-dBATCH",
        "-dSAFER",
        "-sDEVICE=pdfwrite",
        "-dCompatibilityLevel=1.4",
        "-sColorConversionStrategy=LeaveColorUnchanged",
        "-dAutoRotatePages=/None",
    ];
    if outline_text {
        args.push("-dNoOutputFonts");
    }
    if eps_crop {
        args.push("-dEPSCrop");
    }
    args
}

/// Unique temporary file, deleted on drop.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn new(ext: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let name = format!("ai8conv-{}-{}-{}.{}", std::process::id(), n, nanos, ext);
        Self { path: std::env::temp_dir().join(name) }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct ProcOutput {
    success: bool,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

impl ProcOutput {
    fn combined(&self) -> String {
        let mut s = String::new();
        for part in [self.stderr.trim(), self.stdout.trim()] {
            if !part.is_empty() {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(part);
            }
        }
        if s.is_empty() {
            s.push_str("(no output)");
        }
        // Keep error messages readable in the UI.
        const MAX: usize = 4000;
        if s.len() > MAX {
            let mut cut = MAX;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
            s.push_str("\n…");
        }
        s
    }
}

fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<ProcOutput> {
    hide_console(&mut cmd);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child: Child = cmd.spawn().context("could not start process")?;

    // Drain pipes on separate threads so a chatty child never blocks.
    let reader = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out_t = reader(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err_t = reader(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));

    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    Ok(ProcOutput { success: status.is_some_and(|s| s.success()), timed_out, stdout, stderr })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_install_dir_versions() {
        assert_eq!(parse_gs_dir_version("gs10.04.0"), Some(vec![10, 4, 0]));
        assert_eq!(parse_gs_dir_version("gs9.56.1"), Some(vec![9, 56, 1]));
        assert_eq!(parse_gs_dir_version("gs"), None);
        assert_eq!(parse_gs_dir_version("fonts"), None);
        let mut v = [parse_gs_dir_version("gs9.56.1").unwrap(), parse_gs_dir_version("gs10.02.0").unwrap()];
        v.sort_by(|a, b| b.cmp(a));
        assert_eq!(v[0], vec![10, 2, 0]);
    }

    #[test]
    fn args_include_options() {
        let a = pdfwrite_args(true, true);
        assert!(a.contains(&"-dNoOutputFonts") && a.contains(&"-dEPSCrop") && a.contains(&"-dSAFER"));
        let a = pdfwrite_args(false, false);
        assert!(!a.contains(&"-dNoOutputFonts") && !a.contains(&"-dEPSCrop"));
    }

    #[test]
    fn bundled_candidates_include_mac_resources() {
        let c = bundled_candidates(Path::new("/Apps/X.app/Contents/MacOS"));
        assert!(c.contains(&PathBuf::from("/Apps/X.app/Contents/Resources/gs/bin/gs")));
    }

    #[test]
    fn temp_files_are_unique() {
        let a = TempFile::new("pdf");
        let b = TempFile::new("pdf");
        assert_ne!(a.path, b.path);
    }

    #[test]
    fn path_search_finds_nothing_in_empty_dir() {
        let dir = std::env::temp_dir().join(format!("ai8-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(find_on_path(Some(dir.clone().into_os_string())), None);
        assert_eq!(find_on_path(None), None);
        let _ = std::fs::remove_dir(&dir);
    }
}
