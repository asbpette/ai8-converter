//! Tests for the `ai8conv` command-line binary.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn ai8conv(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ai8conv")).args(args).output().expect("running ai8conv")
}

fn run(args: &[&str]) -> Output {
    let a: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    ai8conv(&a)
}

fn temp_dir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "ai8-cli-test-{}-{}-{}",
        tag,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="100" height="80" viewBox="0 0 100 80">
  <rect x="10" y="10" width="50" height="30" fill="#ff0000"/>
  <path d="M10 70 C 30 40, 70 40, 90 70" fill="none" stroke="#0000ff" stroke-width="2"/>
</svg>
"##;

#[test]
fn help_prints_usage() {
    for flag in ["--help", "-h"] {
        let o = run(&[flag]);
        assert!(o.status.success());
        let s = stdout(&o);
        assert!(s.contains("USAGE") && s.contains("--out") && s.contains("--color"), "{s}");
    }
}

#[test]
fn version_prints_version() {
    let o = run(&["--version"]);
    assert!(o.status.success());
    assert!(stdout(&o).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn no_args_is_usage_error() {
    let o = run(&[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("no input files"));
}

#[test]
fn unknown_option_is_usage_error() {
    let o = run(&["--frobnicate", "x.eps"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("unknown option"));
}

#[test]
fn bad_color_mode_is_usage_error() {
    let o = run(&["--color", "purple", "x.eps"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("invalid colour mode"));
}

#[test]
fn missing_option_value_is_usage_error() {
    let o = run(&["x.eps", "--out"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn bad_gs_path_is_usage_error() {
    let o = run(&["--gs", "/no/such/gs", "x.eps"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("Ghostscript"));
}

#[test]
fn missing_file_fails() {
    let dir = temp_dir("missing");
    let missing = dir.join("does-not-exist.svg");
    let o = ai8conv(&[missing.as_os_str()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("does-not-exist.svg"), "{}", stderr(&o));
}

#[test]
fn unrecognised_file_fails() {
    let dir = temp_dir("unknown");
    let f = dir.join("notes.txt");
    std::fs::write(&f, "hello").unwrap();
    let o = ai8conv(&[f.as_os_str()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("unrecognised"), "{}", stderr(&o));
}

fn check_ai(path: &Path) -> String {
    let ai = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let stats = common::validate_ai8(&ai);
    assert!(stats.paths >= 2, "{stats:?}");
    ai
}

// Requires src/svg.rs.
#[test]
fn converts_svg_to_out_dir() {
    let dir = temp_dir("svg");
    let input = dir.join("logo æ.svg");
    std::fs::write(&input, SVG).unwrap();
    let out = dir.join("out dir");
    let o = ai8conv(&["--out".as_ref(), out.as_os_str(), input.as_os_str()]);
    assert!(o.status.success(), "stderr: {}", stderr(&o));
    let expected = out.join("logo æ.ai");
    assert!(stdout(&o).contains("logo æ.ai"), "{}", stdout(&o));
    let ai = check_ai(&expected);
    assert!(ai.lines().any(|l| l.ends_with(" Xa")), "RGB fill expected in preserve mode");
}

// Requires src/svg.rs.
#[test]
fn converts_svg_cmyk_next_to_input() {
    let dir = temp_dir("svg-cmyk");
    let input = dir.join("shape.svg");
    std::fs::write(&input, SVG).unwrap();
    let o = ai8conv(&["--color".as_ref(), "cmyk".as_ref(), input.as_os_str()]);
    assert!(o.status.success(), "stderr: {}", stderr(&o));
    let ai = check_ai(&dir.join("shape.ai"));
    assert!(!ai.lines().any(|l| l.ends_with(" Xa") || l.ends_with(" XA")), "RGB left in CMYK mode");
    assert!(ai.lines().any(|l| l.ends_with(" k")));
}

// Requires src/svg.rs. One good + one bad file -> exit 1 but good file written.
#[test]
fn partial_failure_exits_nonzero() {
    let dir = temp_dir("partial");
    let good = dir.join("good.svg");
    std::fs::write(&good, SVG).unwrap();
    let bad = dir.join("bad.svg");
    let o = ai8conv(&[good.as_os_str(), bad.as_os_str()]);
    assert_eq!(o.status.code(), Some(1), "stderr: {}", stderr(&o));
    assert!(dir.join("good.ai").is_file(), "stderr: {}", stderr(&o));
    assert!(stderr(&o).contains("1 of 2"));
}
