use colored::{Color, Colorize};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    terminal,
};
use std::io::{self, Write};
use std::process;
use std::thread;
use std::time::Duration;

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

const MENU_ITEMS: [MenuItem; 2] = [
    MenuItem { label: "RUN DAEMON",         target: UiState::Run  },
    MenuItem { label: "TERMINATE SESSION",  target: UiState::Exit },
];

// ============================================================
//  UI STATE MACHINE
// ============================================================
#[derive(Debug, PartialEq, Clone, Copy)]
enum UiState {
    Start,
    Input,
    Run,
    Exit,
    Error,
}

pub struct Ui {
    palette: Vec<Color>,
    current_state: UiState,
    /// Currently highlighted menu row (0-based).
    selected_index: usize,
}

impl Ui {
    pub fn new(color_palette: Vec<Color>) -> Self {
        Ui {
            palette: color_palette,
            current_state: UiState::Start,
            selected_index: 0,
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