use colored::{Color, Colorize};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    terminal,
};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::Duration;

use crate::interrupt::hardware_interrupt::{
    self, BulkMeasurement, FileMeasurement, Filter, Progress, SweepResult,
};
use crate::report::{Report, SortKey};

/// Sampling cadence for the hardware timer interrupt while profiling.
const SAMPLE_INTERVAL: Duration = Duration::from_micros(500);

/// Running totals across a batch (directory / multi-file) scan.
#[derive(Default)]
struct Aggregate {
    files: u64,
    bytes: u64,
    wall_nanos: u64,
    chunks: u64,
    irqs: u64,
    samples: u64,
    errors: u64,
    counter_hz: u64,
}

impl Aggregate {
    fn add(&mut self, m: &FileMeasurement) {
        self.files += 1;
        self.bytes += m.bytes_read;
        self.wall_nanos += m.wall_nanos;
        self.chunks += m.chunk_count;
        self.irqs += m.timer_interrupts;
        self.samples += m.samples.len() as u64;
        self.counter_hz = m.counter_hz;
    }

    fn throughput_mib_s(&self) -> f64 {
        let secs = self.wall_nanos as f64 / 1e9;
        if secs <= 0.0 {
            0.0
        } else {
            (self.bytes as f64 / (1024.0 * 1024.0)) / secs
        }
    }
}

/// Human-readable nanosecond duration (ns / µs / ms).
fn human_ns(n: u64) -> String {
    if n < 1_000 {
        format!("{n} ns")
    } else if n < 1_000_000 {
        format!("{} µs", n / 1_000)
    } else {
        format!("{} ms", n / 1_000_000)
    }
}

/// Human-readable byte count (B / KiB / MiB / GiB).
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

// ============================================================
//  CYBERPUNK NEON PALETTE
// ============================================================
pub const NEON_CYAN: Color = Color::TrueColor { r: 0, g: 247, b: 255 };
pub const NEON_MAGENTA: Color = Color::TrueColor { r: 255, g: 0, b: 199 };
pub const NEON_YELLOW: Color = Color::TrueColor { r: 252, g: 238, b: 9 };
pub const NEON_GREEN: Color = Color::TrueColor { r: 0, g: 255, b: 140 };
pub const NEON_RED: Color = Color::TrueColor { r: 255, g: 38, b: 70 };
pub const NEON_PURPLE: Color = Color::TrueColor { r: 162, g: 0, b: 255 };
/// Muted slate used for inactive UI chrome (empty progress, dim hints).
pub const DIM_SLATE: Color = Color::TrueColor { r: 88, g: 96, b: 124 };

/// Inner width (in cells) of the framed panels.
const PANEL_WIDTH: usize = 52;

/// Default palette, handy for `main.rs`.
pub fn default_palette() -> Vec<Color> {
    vec![NEON_CYAN, NEON_MAGENTA, NEON_YELLOW, NEON_GREEN, NEON_PURPLE]
}

// ============================================================
//  ASCII ART — boot-screen logo
// ============================================================

/// Custom ASCII art banner, loaded at compile time from
/// `assets/banner.txt` in the project root.
const CUSTOM_BANNER: &str = include_str!("../../assets/banner.txt");

/// Big block-letter "SAWSKAS" wordmark (5x7 dot-matrix font).
const TITLE_LINES: [&str; 7] = [
    r" ████  ███  █   █  ████ █   █  ███   ████ ",
    r"█     █   █ █   █ █     █  █  █   █ █     ",
    r"█     █   █ █   █ █     █ █   █   █ █     ",
    r" ███  █████ █ █ █  ███  ██    █████  ███  ",
    r"    █ █   █ █ █ █     █ █ █   █   █     █ ",
    r"    █ █   █ ██ ██     █ █  █  █   █     █ ",
    r"████  █   █ █   █ ████  █   █ █   █ ████  ",
];

const TITLE_COLUMN_WIDTH: usize = 6;
const SUBTITLE: &str = "// P R O F I L E R :: SYSTEM DAEMON INTERFACE //";

// ============================================================
//  TERMINAL TEXT
// ============================================================
const INITIAL_MESSAGE: &str =
    "INITIATING DAEMON...\nRUNNING STARTUP SCRIPTS...";

const GREETING_MESSAGE: &str = "WELCOME TO THE SAWSKAS PROFILER MAINFRAME";

const INPUT_HEADER: &str = "AWAITING OPERATOR INPUT...";

const RUNNING_MESSAGE: &str =
    "DAEMON ONLINE.\nMONITORING SYSTEM PROCESSES...";

const EXIT_MESSAGE: &str =
    "INITIATING SHUTDOWN...\nKILLING THE CONNECTION...";

const ERROR_MESSAGE: &str =
    "ERROR :: UNRECOGNIZED INPUT...\nABORTING SESSION...";

// ============================================================
//  MENU DEFINITION
// ============================================================

#[derive(Clone, Copy)]
struct MenuItem {
    label: &'static str,
    target: UiState,
}

const MENU_ITEMS: [MenuItem; 5] = [
    MenuItem { label: "RUN DAEMON",         target: UiState::Run    },
    MenuItem { label: "BULK THROUGHPUT",    target: UiState::Bulk   },
    MenuItem { label: "BLOCK-SIZE SWEEP",   target: UiState::Sweep  },
    MenuItem { label: "EXPORT REPORT",      target: UiState::Export },
    MenuItem { label: "TERMINATE SESSION",  target: UiState::Exit   },
];

// ============================================================
//  UI STATE MACHINE
// ============================================================
#[derive(Debug, PartialEq, Clone, Copy)]
enum UiState {
    Start,
    Input,
    Run,
    Bulk,
    Sweep,
    Export,
    Exit,
    Error,
}

pub struct Ui {
    palette: Vec<Color>,
    current_state: UiState,
    /// Currently highlighted menu row (0-based).
    selected_index: usize,
    /// The most recent scan, retained so it can be exported.
    last_report: Option<Report>,
}

impl Ui {
    pub fn new(color_palette: Vec<Color>) -> Self {
        Ui {
            palette: color_palette,
            current_state: UiState::Start,
            selected_index: 0,
            last_report: None,
        }
    }

    // --------------------------------------------------------
    //  Low level helpers
    // --------------------------------------------------------

    /// Clears the terminal and snaps the cursor back to (1, 1).
    fn clear_screen() {
        print!("\x1B[2J\x1B[1;1H");
        io::stdout().flush().unwrap();
    }

    /// Prints a single word/character chunk with a "hacker terminal"
    /// typing effect in the given color.
    fn typing_effect(chunk: &str, delay_ms: u64, color: Color) {
        for c in chunk.chars() {
            print!("{}", c.to_string().color(color));
            io::stdout().flush().unwrap();
            thread::sleep(Duration::from_millis(delay_ms));
        }
    }

    /// Types out a (possibly multi-line) string, cycling through the
    /// palette word-by-word.
    fn type_sentence(&self, text: &str, delay_ms: u64) {
        let mut colors = self.palette.iter().cycle();

        for line in text.split('\n') {
            for word in line.split(' ') {
                if word.is_empty() {
                    print!(" ");
                    continue;
                }
                let current_color = *colors.next().unwrap();
                Self::typing_effect(word, delay_ms, current_color);
                print!(" ");
                io::stdout().flush().unwrap();
            }
            println!();
        }
    }

    /// Prints the boot-screen banner.
    fn print_banner(&self) {
        self.print_custom_banner();
        self.print_title();
        // Center the subtitle under the wordmark (title is ~42 cells wide).
        let pad = (42usize.saturating_sub(SUBTITLE.chars().count())) / 2;
        println!(
            "{}{}",
            " ".repeat(pad + 2),
            SUBTITLE.color(NEON_PURPLE).italic()
        );
        println!();
    }

    /// Big block-letter title with per-letter neon colors.
    fn print_title(&self) {
        for line in TITLE_LINES.iter() {
            let chars: Vec<char> = line.chars().collect();
            for (i, chunk) in chars.chunks(TITLE_COLUMN_WIDTH).enumerate() {
                let color = self.palette[i % self.palette.len()];
                let segment: String = chunk.iter().collect();
                print!("{}", segment.color(color).bold());
            }
            println!();
        }
    }

    /// Renders `assets/banner.txt` (if non-empty).
    fn print_custom_banner(&self) {
        if CUSTOM_BANNER.trim().is_empty() {
            return;
        }
        let mut colors = self.palette.iter().cycle();
        for line in CUSTOM_BANNER.lines() {
            let color = *colors.next().unwrap();
            println!("{}", line.color(color));
        }
    }

    /// Animated loading bar with a live percentage readout. The bar fills
    /// in place (carriage-return redraw) and snaps to a green check on done.
    fn loading_bar(&self, label: &str, color: Color) {
        const SLOTS: usize = 30;

        for step in 0..=SLOTS {
            let pct = step * 100 / SLOTS;
            let filled = "█".repeat(step);
            let empty = "░".repeat(SLOTS - step);

            print!(
                "\r  {} {} {} {}",
                format!("{:<10}", label).color(color).bold(),
                format!("{}{}", filled.color(color), empty.color(DIM_SLATE)),
                "▏".color(DIM_SLATE),
                format!("{:>3}%", pct).color(NEON_GREEN).bold(),
            );
            io::stdout().flush().unwrap();
            thread::sleep(Duration::from_millis(22));
        }

        println!(
            "\r  {} {} {} {}   ",
            format!("{:<10}", label).color(color).bold(),
            format!("{}", "█".repeat(SLOTS).color(color)),
            "▏".color(DIM_SLATE),
            "✓ ONLINE".color(NEON_GREEN).bold(),
        );
    }

    /// Horizontal neon divider line with end caps, indented to match panels.
    fn print_divider(&self, color: Color) {
        println!(
            "  {}",
            format!("╾{}╼", "─".repeat(PANEL_WIDTH)).color(color)
        );
    }

    /// Pads `s` with trailing spaces so its visible width equals `width`.
    fn pad_to(s: &str, width: usize) -> String {
        let len = s.chars().count();
        if len >= width {
            s.to_string()
        } else {
            format!("{}{}", s, " ".repeat(width - len))
        }
    }

    // --------------------------------------------------------
    //  Interactive menu (arrow keys + Enter)
    // --------------------------------------------------------

    /// Number of lines `draw_menu_items` emits *before* the final prompt
    /// (which carries no trailing newline). Used to rewind for redraws.
    ///   top border (1) + N rows + bottom border (1)
    ///   + blank (1) + hint (1) + blank (1)  = N + 5
    const MENU_LINES_BEFORE_PROMPT: usize = MENU_ITEMS.len() + 5;

    /// Draws the framed menu panel in its current selected/unselected state.
    /// After this call the cursor sits at the end of the prompt line
    /// (no trailing newline).
    fn draw_menu_items(&self) {
        let border = NEON_MAGENTA;

        // ── Top border with embedded title ──────────────────────────────
        let title = "═ SELECT AN OPERATION ";
        let fill = PANEL_WIDTH.saturating_sub(title.chars().count());
        println!(
            "  {}",
            format!("╔{}{}╗", title, "═".repeat(fill)).color(border).bold()
        );

        // ── Menu rows ───────────────────────────────────────────────────
        for (i, item) in MENU_ITEMS.iter().enumerate() {
            let selected = i == self.selected_index;
            let arrow = if selected { "▸" } else { " " };
            let content = format!("  {} [{}] {}", arrow, i + 1, item.label);
            let padded = Self::pad_to(&content, PANEL_WIDTH);

            let body = if selected {
                padded.color(NEON_YELLOW).bold()
            } else {
                padded.color(NEON_CYAN)
            };

            println!(
                "  {}{}{}",
                "║".color(border).bold(),
                body,
                "║".color(border).bold()
            );
        }

        // ── Bottom border ───────────────────────────────────────────────
        println!(
            "  {}",
            format!("╚{}╝", "═".repeat(PANEL_WIDTH)).color(border).bold()
        );

        // ── Navigation hint ─────────────────────────────────────────────
        println!(
            "   {}  {}  {}",
            "↑/↓ navigate".color(DIM_SLATE),
            "↵ execute".color(NEON_GREEN),
            "q abort".color(NEON_RED),
        );

        // Blank line + prompt (no trailing newline so cursor stays on this line)
        print!(
            "\n  {}{} {} ",
            "root@sawskas".color(NEON_GREEN).bold(),
            ":~$".color(DIM_SLATE),
            "▮".color(NEON_GREEN),
        );
        io::stdout().flush().unwrap();
    }

    /// Moves the cursor back to the top of the menu and redraws it.
    fn redraw_menu_items(&self) {
        let lines_up = Self::MENU_LINES_BEFORE_PROMPT;
        // \x1B[{n}A  — cursor up n lines
        // \r         — move to column 0
        // \x1B[J     — erase from cursor to end of screen
        print!("\x1B[{}A\r\x1B[J", lines_up);
        io::stdout().flush().unwrap();
        self.draw_menu_items();
    }

    /// Blocks until the user presses Enter on a selection, using raw
    /// mode so arrow keys are captured without waiting for a newline.
    fn take_input(&mut self) -> UiState {
        // Initial draw
        self.draw_menu_items();

        // Enter raw mode + hide cursor for clean navigation UX
        terminal::enable_raw_mode().expect("Failed to enable raw mode");
        crossterm::execute!(io::stdout(), cursor::Hide).ok();

        let chosen = loop {
            // crossterm's event::read() blocks until a key event arrives
            if let Ok(Event::Key(KeyEvent { code, kind, .. })) = event::read() {
                // On Windows crossterm fires both Press and Release; ignore Release
                if kind == KeyEventKind::Release {
                    continue;
                }

                match code {
                    KeyCode::Up => {
                        if self.selected_index > 0 {
                            self.selected_index -= 1;
                            self.redraw_menu_items();
                        }
                    }
                    KeyCode::Down => {
                        if self.selected_index < MENU_ITEMS.len() - 1 {
                            self.selected_index += 1;
                            self.redraw_menu_items();
                        }
                    }
                    KeyCode::Enter => {
                        break MENU_ITEMS[self.selected_index].target;
                    }
                    // Allow Esc / q as a quick-exit shortcut
                    KeyCode::Esc | KeyCode::Char('q') => {
                        break UiState::Exit;
                    }
                    _ => {}
                }
            }
        };

        // Restore terminal
        crossterm::execute!(io::stdout(), cursor::Show).ok();
        terminal::disable_raw_mode().expect("Failed to disable raw mode");
        println!(); // newline after the prompt
        chosen
    }

    // --------------------------------------------------------
    //  Real file profiling (hardware-interrupt driven)
    // --------------------------------------------------------

    /// Prompts for a target path (file *or* directory), discovers the files to
    /// measure, and dispatches to a detailed single-file report or a batch scan
    /// with an aggregate summary.
    fn run_profiler(&mut self) {
        let default = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
            .unwrap_or_else(|| "Cargo.toml".to_string());

        let input = self.prompt_path(&default);

        // Filtering / sorting only make sense when scanning a directory.
        let is_dir = Path::new(&input).is_dir();
        let (filter, sort) = if is_dir {
            (self.prompt_filter(), self.prompt_sort())
        } else {
            (Filter::default(), SortKey::Throughput)
        };
        let uncached = self.prompt_uncached();
        println!();

        let files = match hardware_interrupt::collect_files_filtered(&input, &filter) {
            Ok(f) if !f.is_empty() => f,
            Ok(_) => {
                self.scan_error("no files matched under that path");
                return;
            }
            Err(e) => {
                self.scan_error(&e.to_string());
                return;
            }
        };

        let measured = if files.len() == 1 {
            let path = files[0].to_str().unwrap_or(&input);
            self.profile_single(path, uncached).into_iter().collect()
        } else {
            self.profile_batch(&input, &files, sort, uncached)
        };

        if !measured.is_empty() {
            self.last_report = Some(Report::Profile {
                root: input,
                files: measured,
            });
        }
    }

    /// Detailed profile of a single file: live bar + full report. The bar's
    /// refresh cadence is driven by the same hardware timer interrupt that
    /// produces the measurement samples.
    fn profile_single(&self, path: &str, uncached: bool) -> Option<FileMeasurement> {
        // Draw an initial 0% bar so even instant reads show a frame.
        self.render_scan_bar(0, 1, 0);

        match hardware_interrupt::measure_file_with(path, SAMPLE_INTERVAL, uncached, |p: Progress| {
            self.render_scan_bar(p.bytes_read, p.size_bytes, p.elapsed_nanos)
        }) {
            Ok(m) => {
                self.render_scan_bar(m.bytes_read, m.bytes_read.max(1), m.wall_nanos);
                println!("  {}", "✓ SCAN COMPLETE".color(NEON_GREEN).bold());
                println!();
                self.print_report(&m);
                Some(m)
            }
            Err(e) => {
                print!("\r\x1B[K"); // clear the partial bar line
                self.scan_error(&e.to_string());
                None
            }
        }
    }

    /// Batch profile of many files. Each file is measured sequentially (the
    /// hardware timer + handler are global, single-armed state) behind a live
    /// transient bar; afterwards the results are sorted and printed as a table
    /// followed by an aggregate report.
    fn profile_batch(
        &self,
        root: &str,
        files: &[PathBuf],
        sort: SortKey,
        uncached: bool,
    ) -> Vec<FileMeasurement> {
        println!(
            "  {} {}",
            "BATCH SCAN".color(NEON_MAGENTA).bold(),
            format!("{} files · {} · {}", files.len(), root, sort.label()).color(DIM_SLATE),
        );
        self.print_divider(NEON_PURPLE);

        let base = Path::new(root);
        let total = files.len();
        let mut agg = Aggregate::default();
        // (original index, display name, measurement) for successful reads.
        let mut rows: Vec<(usize, String, FileMeasurement)> = Vec::new();

        for (i, f) in files.iter().enumerate() {
            let idx = i + 1;
            let name = f
                .strip_prefix(base)
                .unwrap_or(f)
                .to_string_lossy()
                .into_owned();

            let Some(path) = f.to_str() else {
                self.print_batch_error(idx, total, &name, "non-UTF-8 path");
                agg.errors += 1;
                continue;
            };

            // Live, transient progress for the file currently being scanned.
            self.render_batch_bar(idx, total, &name, 0, 1, 0);
            match hardware_interrupt::measure_file_with(path, SAMPLE_INTERVAL, uncached, |p: Progress| {
                self.render_batch_bar(idx, total, &name, p.bytes_read, p.size_bytes, p.elapsed_nanos)
            }) {
                Ok(m) => {
                    agg.add(&m);
                    rows.push((idx, name, m));
                }
                Err(e) => {
                    self.print_batch_error(idx, total, &name, &e.to_string());
                    agg.errors += 1;
                }
            }
        }

        print!("\r\x1B[K"); // wipe the final transient bar line

        // Sort the results per the requested key.
        match sort {
            SortKey::Throughput => rows.sort_by(|a, b| {
                a.2.throughput_mib_s()
                    .partial_cmp(&b.2.throughput_mib_s())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortKey::Size => rows.sort_by(|a, b| b.2.size_bytes.cmp(&a.2.size_bytes)),
            SortKey::Name => rows.sort_by(|a, b| a.1.cmp(&b.1)),
        }

        const CAP: usize = 20;
        for (idx, name, m) in rows.iter().take(CAP) {
            self.print_batch_row(*idx, total, name, m);
        }
        if rows.len() > CAP {
            println!(
                "  {}",
                format!("… +{} more", rows.len() - CAP).color(DIM_SLATE)
            );
        }

        self.print_aggregate(&agg);
        rows.into_iter().map(|(_, _, m)| m).collect()
    }

    /// Prompts for an optional extension filter; hidden entries are always
    /// skipped in directory mode.
    fn prompt_filter(&self) -> Filter {
        println!(
            "  {} {}",
            "FILTER".color(NEON_CYAN).bold(),
            "[extensions e.g. rs,toml · blank = all]".color(DIM_SLATE),
        );
        print!("  {} ", "›".color(NEON_GREEN).bold());
        io::stdout().flush().unwrap();

        let mut line = String::new();
        io::stdin().lock().read_line(&mut line).ok();

        // Accept "rs", ".rs", "*.rs", "rs, toml" — split on commas/whitespace,
        // strip leading '*'/'.' decoration, lowercase, drop empties.
        let extensions: Vec<String> = line
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(|t| t.trim_start_matches('*').trim_start_matches('.').to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect();

        Filter {
            skip_hidden: true,
            extensions,
        }
    }

    /// Prompts for the batch sort order (defaults to throughput).
    fn prompt_sort(&self) -> SortKey {
        println!(
            "  {} {}",
            "SORT".color(NEON_CYAN).bold(),
            "[throughput · size · name · blank = throughput]".color(DIM_SLATE),
        );
        print!("  {} ", "›".color(NEON_GREEN).bold());
        io::stdout().flush().unwrap();

        let mut line = String::new();
        io::stdin().lock().read_line(&mut line).ok();
        match line.trim().to_ascii_lowercase().as_str() {
            "size" | "s" => SortKey::Size,
            "name" | "n" => SortKey::Name,
            _ => SortKey::Throughput,
        }
    }

    /// Neon-red one-line scan failure.
    fn scan_error(&self, msg: &str) {
        println!(
            "  {} {}",
            "✗ SCAN FAILED ::".color(NEON_RED).bold(),
            msg.color(NEON_RED),
        );
    }

    /// Reads a target path (file or directory) from the user (cooked-mode line
    /// input). Empty input falls back to `default`.
    fn prompt_path(&self, default: &str) -> String {
        println!(
            "  {} {}",
            "TARGET PATH".color(NEON_CYAN).bold(),
            format!("[file or dir · default: {default}]").color(DIM_SLATE),
        );
        print!("  {} ", "›".color(NEON_GREEN).bold());
        io::stdout().flush().unwrap();

        let mut line = String::new();
        io::stdin().lock().read_line(&mut line).ok();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            default.to_string()
        } else {
            trimmed.to_string()
        }
    }

    /// Renders a single-line neon scan bar (carriage-return redraw) showing
    /// completion percentage and live throughput.
    fn render_scan_bar(&self, bytes: u64, size: u64, elapsed_ns: u64) {
        const SLOTS: usize = 30;
        let frac = if size == 0 {
            1.0
        } else {
            (bytes as f64 / size as f64).clamp(0.0, 1.0)
        };
        let filled = (frac * SLOTS as f64).round() as usize;
        let mibps = if elapsed_ns == 0 {
            0.0
        } else {
            (bytes as f64 / (1024.0 * 1024.0)) / (elapsed_ns as f64 / 1e9)
        };

        print!(
            "\r  {} {}{} {} {}",
            "SCAN".color(NEON_GREEN).bold(),
            "█".repeat(filled).color(NEON_GREEN),
            "░".repeat(SLOTS - filled).color(DIM_SLATE),
            format!("{:>3}%", (frac * 100.0) as u32).color(NEON_YELLOW).bold(),
            format!("{:>9.1} MiB/s", mibps).color(NEON_CYAN),
        );
        io::stdout().flush().unwrap();
    }

    /// One labelled metric row in the report.
    fn report_row(&self, label: &str, value: String) {
        println!(
            "  {} {}",
            format!("{label:<13}").color(NEON_CYAN),
            value.color(NEON_YELLOW).bold(),
        );
    }

    /// Prints the real measurement as a neon-styled report.
    fn print_report(&self, m: &FileMeasurement) {
        self.print_divider(NEON_PURPLE);
        self.report_row("TARGET", m.path.clone());
        self.report_row("SIZE", format!("{} bytes", m.size_bytes));
        self.report_row(
            "READ MODE",
            if m.uncached {
                "uncached (page cache bypassed)".to_string()
            } else {
                "cached".to_string()
            },
        );
        self.report_row("WALL TIME", format!("{:.3} ms", m.wall_nanos as f64 / 1e6));
        self.report_row("THROUGHPUT", format!("{:.2} MiB/s", m.throughput_mib_s()));
        self.report_row("CHUNKS", format!("{}", m.chunk_count));
        self.report_row(
            "READ LATENCY",
            format!(
                "avg {} ns · min {} ns · max {} ns",
                m.avg_chunk_nanos(),
                m.min_chunk_nanos(),
                m.max_chunk_nanos(),
            ),
        );
        let pct = m.percentiles_ns(&[50.0, 95.0, 99.0]);
        self.report_row(
            "PERCENTILES",
            format!("p50 {} ns · p95 {} ns · p99 {} ns", pct[0], pct[1], pct[2]),
        );
        self.report_row(
            "READ-BUSY",
            format!("{:.1}% of wall time", m.read_busy_fraction() * 100.0),
        );
        self.report_row(
            "TIMER IRQS",
            format!("{} (hw counter @ {} Hz)", m.timer_interrupts, m.counter_hz),
        );
        self.report_row("SAMPLES", format!("{} captured", m.samples.len()));
        self.print_latency_histogram(m);
        self.print_divider(NEON_PURPLE);
    }

    /// Renders the per-chunk read-latency distribution as a neon bar chart.
    /// Empty buckets are skipped so the chart stays compact.
    fn print_latency_histogram(&self, m: &FileMeasurement) {
        let bounds = hardware_interrupt::LATENCY_BOUNDS_NS;
        let max = m.latency_buckets.iter().copied().max().unwrap_or(0);
        if max == 0 {
            return;
        }

        println!(
            "  {}",
            "READ-LATENCY DISTRIBUTION".color(NEON_CYAN).bold()
        );
        const WIDTH: usize = 28;
        for (i, &count) in m.latency_buckets.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let label = if i < bounds.len() {
                format!("≤ {}", human_ns(bounds[i]))
            } else {
                format!("> {}", human_ns(bounds[bounds.len() - 1]))
            };
            let bar = ((count as f64 / max as f64) * WIDTH as f64).round() as usize;
            let bar = bar.max(1); // a non-empty bucket always shows at least one cell
            println!(
                "  {} {}{} {}",
                format!("{label:>8}").color(DIM_SLATE),
                "█".repeat(bar).color(NEON_GREEN),
                "░".repeat(WIDTH - bar).color(DIM_SLATE),
                format!("{count:>5}").color(NEON_YELLOW).bold(),
            );
        }
    }

    /// Truncates `s` to `max` visible cells, keeping the tail (filenames are
    /// more informative than leading directories) with a leading ellipsis.
    fn truncate(s: &str, max: usize) -> String {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() <= max {
            return s.to_string();
        }
        let keep = max.saturating_sub(1);
        let tail: String = chars[chars.len() - keep..].iter().collect();
        format!("…{tail}")
    }

    /// Live per-file scan bar for batch mode (carriage-return redraw).
    fn render_batch_bar(&self, idx: usize, total: usize, name: &str, bytes: u64, size: u64, elapsed_ns: u64) {
        const SLOTS: usize = 14;
        let frac = if size == 0 {
            1.0
        } else {
            (bytes as f64 / size as f64).clamp(0.0, 1.0)
        };
        let filled = (frac * SLOTS as f64).round() as usize;
        let mibps = if elapsed_ns == 0 {
            0.0
        } else {
            (bytes as f64 / (1024.0 * 1024.0)) / (elapsed_ns as f64 / 1e9)
        };

        print!(
            "\r\x1B[K  {} {} {}{} {} {}",
            format!("[{idx:02}/{total:02}]").color(NEON_MAGENTA),
            Self::pad_to(&Self::truncate(name, 28), 28).color(NEON_CYAN),
            "█".repeat(filled).color(NEON_GREEN),
            "░".repeat(SLOTS - filled).color(DIM_SLATE),
            format!("{:>3}%", (frac * 100.0) as u32).color(NEON_YELLOW),
            format!("{:>9.1} MiB/s", mibps).color(NEON_CYAN),
        );
        io::stdout().flush().unwrap();
    }

    /// Settles the batch bar into a final one-line result for the file.
    fn print_batch_row(&self, idx: usize, total: usize, name: &str, m: &FileMeasurement) {
        println!(
            "\r\x1B[K  {} {} {} {} {}",
            format!("[{idx:02}/{total:02}]").color(NEON_MAGENTA),
            Self::pad_to(&Self::truncate(name, 28), 28).color(NEON_CYAN),
            format!("{:>10}", human_bytes(m.size_bytes)).color(NEON_YELLOW).bold(),
            format!("{:>11.1} MiB/s", m.throughput_mib_s()).color(NEON_GREEN),
            format!("{:>2} IRQ", m.timer_interrupts).color(DIM_SLATE),
        );
    }

    /// Settles the batch bar into a neon-red error row for the file.
    fn print_batch_error(&self, idx: usize, total: usize, name: &str, msg: &str) {
        println!(
            "\r\x1B[K  {} {} {}",
            format!("[{idx:02}/{total:02}]").color(NEON_RED),
            Self::pad_to(&Self::truncate(name, 28), 28).color(NEON_RED),
            format!("✗ {msg}").color(NEON_RED),
        );
    }

    /// Aggregate report across a whole batch scan.
    fn print_aggregate(&self, a: &Aggregate) {
        println!();
        self.print_divider(NEON_PURPLE);
        self.report_row(
            "FILES",
            format!("{} scanned · {} failed", a.files, a.errors),
        );
        self.report_row("TOTAL SIZE", human_bytes(a.bytes));
        self.report_row("READ TIME", format!("{:.3} ms", a.wall_nanos as f64 / 1e6));
        self.report_row("THROUGHPUT", format!("{:.2} MiB/s", a.throughput_mib_s()));
        self.report_row("CHUNKS", format!("{}", a.chunks));
        self.report_row(
            "TIMER IRQS",
            format!("{} (hw counter @ {} Hz)", a.irqs, a.counter_hz),
        );
        self.report_row("SAMPLES", format!("{} captured", a.samples));
        self.print_divider(NEON_PURPLE);
    }

    // --------------------------------------------------------
    //  Parallel bulk-throughput mode
    // --------------------------------------------------------

    /// Drives the parallel bulk-read mode: discover a tree, fan the reads out
    /// across N threads, and report aggregate throughput. No per-file interrupt
    /// sampling here — see `hardware_interrupt::measure_bulk`.
    fn run_bulk(&mut self) {
        let input = self.prompt_path(".");

        let filter = if Path::new(&input).is_dir() {
            self.prompt_filter()
        } else {
            Filter::default()
        };
        let threads = self.prompt_threads();
        let uncached = self.prompt_uncached();
        println!();

        let files = match hardware_interrupt::collect_files_filtered(&input, &filter) {
            Ok(f) if !f.is_empty() => f,
            Ok(_) => {
                self.scan_error("no files matched under that path");
                return;
            }
            Err(e) => {
                self.scan_error(&e.to_string());
                return;
            }
        };

        println!(
            "  {} {}",
            "BULK READ".color(NEON_CYAN).bold(),
            format!("{} files · {} threads · working…", files.len(), threads).color(DIM_SLATE),
        );
        io::stdout().flush().unwrap();

        let m = hardware_interrupt::measure_bulk(&files, threads, uncached);

        println!("  {}", "✓ BULK COMPLETE".color(NEON_GREEN).bold());
        println!();
        self.print_bulk_report(&m);

        self.last_report = Some(Report::Bulk {
            root: input,
            bulk: m,
        });
    }

    /// Prompts whether to bypass the page cache (measure storage, not RAM).
    fn prompt_uncached(&self) -> bool {
        println!(
            "  {} {}",
            "READ MODE".color(NEON_CYAN).bold(),
            "[cached / uncached · blank = cached]".color(DIM_SLATE),
        );
        print!("  {} ", "›".color(NEON_GREEN).bold());
        io::stdout().flush().unwrap();
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line).ok();
        matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "uncached" | "u" | "cold" | "nocache"
        )
    }

    /// Prompts for a worker-thread count (defaults to available parallelism).
    fn prompt_threads(&self) -> usize {
        let default = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        println!(
            "  {} {}",
            "THREADS".color(NEON_CYAN).bold(),
            format!("[1-256 · blank = {default}]").color(DIM_SLATE),
        );
        print!("  {} ", "›".color(NEON_GREEN).bold());
        io::stdout().flush().unwrap();

        let mut line = String::new();
        io::stdin().lock().read_line(&mut line).ok();
        let t = line.trim();
        if t.is_empty() {
            default
        } else {
            t.parse::<usize>().unwrap_or(default).clamp(1, 256)
        }
    }

    /// Neon-styled aggregate report for a bulk read.
    fn print_bulk_report(&self, m: &BulkMeasurement) {
        self.print_divider(NEON_PURPLE);
        self.report_row("FILES", format!("{} · {} failed", m.file_count, m.errors));
        self.report_row("THREADS", format!("{}", m.threads));
        self.report_row("READ MODE", if m.uncached { "uncached".into() } else { "cached".into() });
        self.report_row("TOTAL READ", human_bytes(m.bytes_read));
        self.report_row("WALL TIME", format!("{:.3} ms", m.wall_nanos as f64 / 1e6));
        self.report_row("THROUGHPUT", format!("{:.2} MiB/s", m.throughput_mib_s()));
        self.report_row(
            "HW COUNTER",
            format!("{} ticks @ {} Hz", m.counter_ticks, m.counter_hz),
        );
        self.print_divider(NEON_PURPLE);
    }

    // --------------------------------------------------------
    //  Block-size sweep
    // --------------------------------------------------------

    /// Sweeps a single file across block sizes and shows a throughput curve.
    fn run_sweep(&mut self) {
        let default = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
            .unwrap_or_else(|| "Cargo.toml".to_string());
        let input = self.prompt_path(&default);

        if !Path::new(&input).is_file() {
            println!();
            self.scan_error("block-size sweep needs a single file");
            return;
        }
        let uncached = self.prompt_uncached();
        println!();

        println!(
            "  {} {}",
            "SWEEPING".color(NEON_PURPLE).bold(),
            format!(
                "reading at 4 KiB → 16 MiB block sizes ({})…",
                if uncached { "uncached" } else { "cached" }
            )
            .color(DIM_SLATE),
        );
        io::stdout().flush().unwrap();

        match hardware_interrupt::sweep_file(&input, &hardware_interrupt::DEFAULT_SWEEP_SIZES, uncached) {
            Ok(s) => {
                println!("  {}", "✓ SWEEP COMPLETE".color(NEON_GREEN).bold());
                println!();
                self.print_sweep_report(&s);
                self.last_report = Some(Report::Sweep { sweep: s });
            }
            Err(e) => self.scan_error(&e.to_string()),
        }
    }

    /// Neon bar chart of throughput vs block size, marking the fastest.
    fn print_sweep_report(&self, s: &SweepResult) {
        self.print_divider(NEON_PURPLE);
        self.report_row("FILE", s.path.clone());
        self.report_row("SIZE", human_bytes(s.size_bytes));
        self.report_row("READ MODE", if s.uncached { "uncached".into() } else { "cached".into() });
        println!("  {}", "THROUGHPUT BY BLOCK SIZE".color(NEON_CYAN).bold());

        let max = s
            .points
            .iter()
            .map(|p| p.throughput_mib_s())
            .fold(0.0_f64, f64::max)
            .max(1e-9);
        let best = s.best().map(|p| p.chunk_size);

        const WIDTH: usize = 26;
        for p in &s.points {
            let frac = p.throughput_mib_s() / max;
            let bar = ((frac * WIDTH as f64).round() as usize).max(1);
            let is_best = best == Some(p.chunk_size);
            let bar_color = if is_best { NEON_GREEN } else { NEON_CYAN };
            println!(
                "  {} {}{} {} {}",
                format!("{:>9}", human_bytes(p.chunk_size as u64)).color(DIM_SLATE),
                "█".repeat(bar).color(bar_color),
                "░".repeat(WIDTH - bar).color(DIM_SLATE),
                format!("{:>8.0} MiB/s", p.throughput_mib_s())
                    .color(NEON_YELLOW)
                    .bold(),
                if is_best {
                    "◀ best".color(NEON_GREEN).bold()
                } else {
                    "".normal()
                },
            );
        }
        self.print_divider(NEON_PURPLE);
    }

    // --------------------------------------------------------
    //  Report export (JSON / CSV / HTML)
    // --------------------------------------------------------

    /// Writes the most recent scan to JSON, CSV, and HTML using a user-supplied
    /// base filename. Does nothing useful if no scan has run yet.
    fn export_report(&self) {
        let Some(report) = &self.last_report else {
            self.scan_error("no report yet — run a scan first");
            return;
        };

        println!(
            "  {} {}",
            "BASE NAME".color(NEON_CYAN).bold(),
            "[blank = profiler-report]".color(DIM_SLATE),
        );
        print!("  {} ", "›".color(NEON_GREEN).bold());
        io::stdout().flush().unwrap();
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line).ok();
        let base = {
            let t = line.trim();
            if t.is_empty() { "profiler-report" } else { t }
        };

        println!();
        let outputs: [(&str, fn(&Report) -> String); 3] = [
            ("json", Report::to_json),
            ("csv", Report::to_csv),
            ("html", Report::to_html),
        ];
        for (ext, render) in outputs {
            let path = std::path::PathBuf::from(format!("{base}.{ext}"));
            match Report::write(&path, &render(report)) {
                Ok(()) => println!(
                    "  {} {}",
                    "✓ WROTE".color(NEON_GREEN).bold(),
                    path.display().to_string().color(NEON_YELLOW),
                ),
                Err(e) => println!(
                    "  {} {}",
                    "✗ FAILED".color(NEON_RED).bold(),
                    format!("{}: {e}", path.display()).color(NEON_RED),
                ),
            }
        }
    }

    // --------------------------------------------------------
    //  Main state-machine tick
    // --------------------------------------------------------
    pub fn update(&mut self) {
        match self.current_state {
            UiState::Start => {
                Self::clear_screen();
                self.print_banner();

                self.print_divider(NEON_MAGENTA);
                self.type_sentence(INITIAL_MESSAGE, 8);
                println!();

                self.loading_bar("BOOTSTRAP", NEON_CYAN);
                self.loading_bar("NET LINK ", NEON_MAGENTA);
                self.loading_bar("SECURITY ", NEON_PURPLE);
                self.loading_bar("CORE     ", NEON_GREEN);

                println!();
                self.print_divider(NEON_MAGENTA);
                self.type_sentence(GREETING_MESSAGE, 18);
                self.print_divider(NEON_MAGENTA);

                self.current_state = UiState::Input;
            }
            UiState::Input => {
                println!();
                self.type_sentence(INPUT_HEADER, 10);
                println!();

                // Reset cursor to top of menu on every Input visit so the
                // highlight always starts at whichever row was last chosen.
                self.selected_index = 0;
                self.current_state = self.take_input();
            }
            UiState::Run => {
                println!();
                self.print_divider(NEON_GREEN);
                self.type_sentence(RUNNING_MESSAGE, 15);
                self.print_divider(NEON_GREEN);
                println!();

                self.run_profiler();

                self.current_state = UiState::Input;
            }
            UiState::Bulk => {
                println!();
                self.print_divider(NEON_CYAN);
                self.type_sentence("PARALLEL BULK-READ MODE...", 15);
                self.print_divider(NEON_CYAN);
                println!();

                self.run_bulk();

                self.current_state = UiState::Input;
            }
            UiState::Sweep => {
                println!();
                self.print_divider(NEON_PURPLE);
                self.type_sentence("BLOCK-SIZE SWEEP...", 15);
                self.print_divider(NEON_PURPLE);
                println!();

                self.run_sweep();

                self.current_state = UiState::Input;
            }
            UiState::Export => {
                println!();
                self.print_divider(NEON_YELLOW);
                self.type_sentence("EXPORT LAST REPORT...", 15);
                self.print_divider(NEON_YELLOW);
                println!();

                self.export_report();

                self.current_state = UiState::Input;
            }
            UiState::Exit => {
                println!();
                self.print_divider(NEON_RED);
                self.type_sentence(EXIT_MESSAGE, 30);
                self.print_divider(NEON_RED);
                process::exit(0);
            }
            UiState::Error => {
                println!();
                self.print_divider(NEON_RED);
                self.type_sentence(ERROR_MESSAGE, 30);
                self.print_divider(NEON_RED);
                process::exit(1);
            }
        }
    }
}