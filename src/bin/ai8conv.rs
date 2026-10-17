//! Command-line converter: EPS / PS / PDF / AI / SVG -> Adobe Illustrator 8.

use ai8_converter::{ColorMode, ConvertOptions, PatternMode, convert_file};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
ai8conv - convert EPS / PS / PDF / AI / SVG files to Adobe Illustrator 8 (.ai)

USAGE:
    ai8conv [OPTIONS] FILES...

OPTIONS:
    -o, --out DIR            Write output files to DIR (default: next to each input)
    -c, --color MODE         Colour handling: preserve (default), cmyk or rgb
    -p, --patterns MODE      Pattern fills/strokes: native (default) writes
                             editable Illustrator pattern swatches; expand
                             replaces them by plain clipped tile paths (for
                             cutters, plotters and RIPs without pattern support)
        --no-outline-text    Keep PDF/AI input as-is instead of outlining text
                             with Ghostscript (EPS/PS text is always outlined)
        --gs PATH            Ghostscript executable to use (default: auto-detect,
                             or the AI8_GHOSTSCRIPT environment variable)
    -h, --help               Show this help
    -V, --version            Show the version

Multi-page inputs produce one file per page (name_p1.ai, name_p2.ai, ...).
Exit status: 0 = all converted, 1 = at least one file failed, 2 = usage error.
";

#[derive(Debug, PartialEq)]
enum Cli {
    Help,
    Version,
    Run { files: Vec<PathBuf>, opts: CliOpts },
}

#[derive(Debug, PartialEq)]
struct CliOpts {
    out: Option<PathBuf>,
    color: ColorMode,
    patterns: PatternMode,
    outline_text: bool,
    gs: Option<PathBuf>,
}

fn parse_color(s: &str) -> Result<ColorMode, String> {
    match s.to_ascii_lowercase().as_str() {
        "preserve" | "keep" => Ok(ColorMode::Preserve),
        "cmyk" => Ok(ColorMode::Cmyk),
        "rgb" => Ok(ColorMode::Rgb),
        _ => Err(format!("invalid colour mode '{s}' (expected preserve, cmyk or rgb)")),
    }
}

fn parse_patterns(s: &str) -> Result<PatternMode, String> {
    match s.to_ascii_lowercase().as_str() {
        "native" => Ok(PatternMode::Native),
        "expand" | "expanded" => Ok(PatternMode::Expand),
        _ => Err(format!("invalid pattern mode '{s}' (expected native or expand)")),
    }
}

fn parse_args<I: IntoIterator<Item = std::ffi::OsString>>(args: I) -> Result<Cli, String> {
    let mut files = Vec::new();
    let mut opts =
        CliOpts { out: None, color: ColorMode::Preserve, patterns: PatternMode::Native, outline_text: true, gs: None };
    let mut it = args.into_iter();
    let mut only_files = false;
    while let Some(arg) = it.next() {
        if only_files {
            files.push(PathBuf::from(arg));
            continue;
        }
        let s = arg.to_string_lossy().into_owned();
        // Support --opt=value.
        let (name, inline) = match s.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (s.clone(), None),
        };
        let mut value = |what: &str| -> Result<std::ffi::OsString, String> {
            if let Some(v) = &inline {
                return Ok(v.into());
            }
            it.next().ok_or_else(|| format!("{name} requires {what}"))
        };
        match name.as_str() {
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "-o" | "--out" => opts.out = Some(PathBuf::from(value("a directory")?)),
            "-c" | "--color" | "--colour" => opts.color = parse_color(&value("a mode")?.to_string_lossy())?,
            "-p" | "--patterns" => opts.patterns = parse_patterns(&value("a mode")?.to_string_lossy())?,
            "--no-outline-text" => opts.outline_text = false,
            "--gs" => opts.gs = Some(PathBuf::from(value("a path")?)),
            "--" => only_files = true,
            _ if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option '{s}'")),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if files.is_empty() {
        return Err("no input files given".into());
    }
    Ok(Cli::Run { files, opts })
}

fn main() -> ExitCode {
    let cli = match parse_args(std::env::args_os().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}\n");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let (files, o) = match cli {
        Cli::Help => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Cli::Version => {
            println!("ai8conv {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Cli::Run { files, opts } => (files, opts),
    };
    if let Some(gs) = &o.gs
        && !gs.is_file()
    {
        eprintln!("error: Ghostscript executable not found: {}", gs.display());
        return ExitCode::from(2);
    }
    let opts = ConvertOptions {
        color_mode: o.color,
        pattern_mode: o.patterns,
        outline_text_in_pdf: o.outline_text,
        out_dir: o.out,
        ghostscript: o.gs,
    };
    let mut failed = 0usize;
    for f in &files {
        match convert_file(f, &opts) {
            Ok(report) => {
                for out in &report.outputs {
                    println!("{} -> {}", f.display(), out.display());
                }
                for w in &report.warnings {
                    eprintln!("warning: {}: {w}", f.display());
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("error: {}: {e:#}", f.display());
            }
        }
    }
    if failed > 0 {
        if files.len() > 1 {
            eprintln!("{failed} of {} file(s) failed", files.len());
        }
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Cli, String> {
        parse_args(args.iter().map(std::ffi::OsString::from))
    }

    #[test]
    fn parses_options() {
        let Cli::Run { files, opts } =
            p(&["--out", "o", "--color=cmyk", "--no-outline-text", "--gs", "/x/gs", "a.eps", "b.pdf"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(files, vec![PathBuf::from("a.eps"), PathBuf::from("b.pdf")]);
        assert_eq!(opts.out, Some(PathBuf::from("o")));
        assert_eq!(opts.color, ColorMode::Cmyk);
        assert!(!opts.outline_text);
        assert_eq!(opts.gs, Some(PathBuf::from("/x/gs")));
        assert_eq!(opts.patterns, PatternMode::Native);
    }

    #[test]
    fn parses_pattern_mode() {
        for (args, want) in [
            (&["--patterns", "expand", "a"][..], PatternMode::Expand),
            (&["--patterns=native", "a"][..], PatternMode::Native),
            (&["-p", "EXPAND", "a"][..], PatternMode::Expand),
        ] {
            let Cli::Run { opts, .. } = p(args).unwrap() else { panic!() };
            assert_eq!(opts.patterns, want, "{args:?}");
        }
        assert!(p(&["--patterns", "tiles", "a"]).unwrap_err().contains("invalid pattern mode"));
        assert!(p(&["a", "--patterns"]).is_err());
    }

    #[test]
    fn errors_and_special_cases() {
        assert_eq!(p(&["--help"]), Ok(Cli::Help));
        assert_eq!(p(&["a", "-V"]), Ok(Cli::Version));
        assert!(p(&[]).is_err());
        assert!(p(&["--bogus", "a"]).is_err());
        assert!(p(&["--color", "purple", "a"]).is_err());
        assert!(p(&["a", "--out"]).is_err());
        let Cli::Run { files, .. } = p(&["--", "-weird.eps"]).unwrap() else { panic!() };
        assert_eq!(files, vec![PathBuf::from("-weird.eps")]);
        let Cli::Run { files, .. } = p(&["-"]).unwrap() else { panic!() };
        assert_eq!(files, vec![PathBuf::from("-")]);
    }
}
