//! Non-interactive command-line mode — makes the profiler scriptable and able
//! to emit JSON / CSV / HTML reports without the terminal UI.

use crate::interrupt::hardware_interrupt::{self, Filter};
use crate::report::{Report, SortKey};
use std::path::PathBuf;
use std::time::Duration;

const SAMPLE_INTERVAL: Duration = Duration::from_micros(500);

/// What the parsed arguments tell us to do.
pub enum Mode {
    /// No path given — launch the interactive terminal UI.
    Interactive,
    /// `--help` / `-h`.
    Help,
    /// Run headless against `Config`.
    Run(Config),
}

pub struct Config {
    pub path: String,
    pub bulk: bool,
    pub sweep: bool,
    pub threads: usize,
    pub filter: Filter,
    pub sort: SortKey,
    pub json: Option<PathBuf>,
    pub csv: Option<PathBuf>,
    pub html: Option<PathBuf>,
}

pub fn help() -> String {
    format!(
        "SAWSKAS PROFILER — hardware-interrupt file measurement\n\
\n\
USAGE:\n\
    {bin} [OPTIONS] [PATH]\n\
\n\
    With no PATH, launches the interactive terminal UI.\n\
    With a PATH (file or directory), runs headless and prints a report.\n\
\n\
OPTIONS:\n\
    --bulk                Parallel bulk-throughput mode (no per-file sampling)\n\
    --sweep               Block-size sweep on a single file (4 KiB → 16 MiB)\n\
    --threads N           Worker threads for --bulk (default: CPU count)\n\
    --ext LIST            Only files with these extensions, e.g. rs,toml\n\
    --all                 Include hidden (dot) files (default: skipped)\n\
    --sort KEY            throughput | size | name  (default: throughput)\n\
    --json [FILE]         Write a JSON report (default: profiler-report.json)\n\
    --csv  [FILE]         Write a CSV report  (default: profiler-report.csv)\n\
    --html [FILE]         Write an HTML report with graphs (default: profiler-report.html)\n\
    -h, --help            Show this help\n\
\n\
EXAMPLES:\n\
    {bin} src --ext rs --html report.html\n\
    {bin} . --bulk --threads 8 --json\n",
        bin = "Sawskas-profiler"
    )
}

/// Parses `args` (already stripped of argv[0]).
pub fn parse(args: &[String]) -> Result<Mode, String> {
    let mut path: Option<String> = None;
    let mut bulk = false;
    let mut sweep = false;
    let mut threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut skip_hidden = true;
    let mut extensions: Vec<String> = Vec::new();
    let mut sort = SortKey::Throughput;
    let mut json: Option<PathBuf> = None;
    let mut csv: Option<PathBuf> = None;
    let mut html: Option<PathBuf> = None;

    // Pulls an optional value for --json/--csv/--html: consumes the next arg
    // only if it isn't another flag; otherwise returns the given default name.
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-h" | "--help" => return Ok(Mode::Help),
            "--bulk" => bulk = true,
            "--sweep" => sweep = true,
            "--all" => skip_hidden = false,
            "--threads" => {
                let v = args.get(i + 1).ok_or("--threads requires a number")?;
                threads = v
                    .parse::<usize>()
                    .map_err(|_| format!("invalid --threads value: {v}"))?
                    .clamp(1, 256);
                i += 1;
            }
            "--ext" => {
                let v = args.get(i + 1).ok_or("--ext requires a list")?;
                extensions = parse_extensions(v);
                i += 1;
            }
            "--sort" => {
                let v = args.get(i + 1).ok_or("--sort requires a key")?;
                sort = SortKey::parse(v);
                i += 1;
            }
            "--json" | "--csv" | "--html" => {
                let default = match a.as_str() {
                    "--json" => "profiler-report.json",
                    "--csv" => "profiler-report.csv",
                    _ => "profiler-report.html",
                };
                let next_is_value = args
                    .get(i + 1)
                    .map(|n| !n.starts_with('-'))
                    .unwrap_or(false);
                let file = if next_is_value {
                    i += 1;
                    PathBuf::from(&args[i])
                } else {
                    PathBuf::from(default)
                };
                match a.as_str() {
                    "--json" => json = Some(file),
                    "--csv" => csv = Some(file),
                    _ => html = Some(file),
                }
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            _ => {
                if path.is_some() {
                    return Err(format!("unexpected extra argument: {a}"));
                }
                path = Some(a.clone());
            }
        }
        i += 1;
    }

    match path {
        None => {
            // Output flags without a path don't make sense headless.
            if json.is_some() || csv.is_some() || html.is_some() || bulk || sweep {
                Err("a PATH is required for headless mode".to_string())
            } else {
                Ok(Mode::Interactive)
            }
        }
        Some(path) => Ok(Mode::Run(Config {
            path,
            bulk,
            sweep,
            threads,
            filter: Filter {
                skip_hidden,
                extensions,
            },
            sort,
            json,
            csv,
            html,
        })),
    }
}

fn parse_extensions(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .map(|t| {
            t.trim_start_matches('*')
                .trim_start_matches('.')
                .to_ascii_lowercase()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// Runs a headless measurement and writes any requested reports. Returns a
/// process exit code.
pub fn run(cfg: Config) -> i32 {
    let files = match hardware_interrupt::collect_files_filtered(&cfg.path, &cfg.filter) {
        Ok(f) if !f.is_empty() => f,
        Ok(_) => {
            eprintln!("error: no files matched under {}", cfg.path);
            return 1;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let report = if cfg.sweep {
        if files.len() != 1 {
            eprintln!("error: --sweep requires a single file (got {})", files.len());
            return 1;
        }
        let path = files[0].to_str().unwrap_or(&cfg.path);
        match hardware_interrupt::sweep_file(path, &hardware_interrupt::DEFAULT_SWEEP_SIZES) {
            Ok(s) => {
                println!("block-size sweep · {} · {}", path, human_bytes(s.size_bytes));
                for p in &s.points {
                    println!(
                        "  {:>9}  {:>9.1} MiB/s  avg {} ns",
                        human_bytes(p.chunk_size as u64),
                        p.throughput_mib_s(),
                        p.avg_chunk_nanos,
                    );
                }
                if let Some(b) = s.best() {
                    println!("  best: {} block", human_bytes(b.chunk_size as u64));
                }
                Report::Sweep { sweep: s }
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else if cfg.bulk {
        eprintln!(
            "bulk reading {} files across {} threads…",
            files.len(),
            cfg.threads
        );
        let m = hardware_interrupt::measure_bulk(&files, cfg.threads);
        println!(
            "bulk: {} files · {} · {:.2} MiB/s · {} threads · {} failed",
            m.file_count,
            human_bytes(m.bytes_read),
            m.throughput_mib_s(),
            m.threads,
            m.errors,
        );
        Report::Bulk {
            root: cfg.path.clone(),
            bulk: m,
        }
    } else {
        let mut measured = Vec::with_capacity(files.len());
        for f in &files {
            let Some(p) = f.to_str() else { continue };
            match hardware_interrupt::measure_file(p, SAMPLE_INTERVAL) {
                Ok(m) => measured.push(m),
                Err(e) => eprintln!("skip {p}: {e}"),
            }
        }
        cfg.sort.sort(&mut measured);

        let total: u64 = measured.iter().map(|m| m.bytes_read).sum();
        println!("profiled {} files · {}", measured.len(), human_bytes(total));
        for m in &measured {
            println!(
                "  {:>10}  {:>9.1} MiB/s  {}",
                human_bytes(m.size_bytes),
                m.throughput_mib_s(),
                m.path,
            );
        }
        Report::Profile {
            root: cfg.path.clone(),
            files: measured,
        }
    };

    let mut exit = 0;
    let outputs: [(&Option<PathBuf>, fn(&Report) -> String); 3] = [
        (&cfg.json, Report::to_json),
        (&cfg.csv, Report::to_csv),
        (&cfg.html, Report::to_html),
    ];
    for (opt, render) in outputs {
        if let Some(path) = opt {
            match Report::write(path, &render(&report)) {
                Ok(()) => eprintln!("wrote {}", path.display()),
                Err(e) => {
                    eprintln!("error: failed to write {}: {e}", path.display());
                    exit = 1;
                }
            }
        }
    }
    exit
}

fn human_bytes(n: u64) -> String {
    const U: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}
