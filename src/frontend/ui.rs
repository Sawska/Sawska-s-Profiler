use colored::{Color, Colorize};
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

/// Default palette, handy for `main.rs`.
pub fn default_palette() -> Vec<Color> {
    vec![NEON_CYAN, NEON_MAGENTA, NEON_YELLOW, NEON_GREEN, NEON_PURPLE]
}

// ============================================================
//  ASCII ART — boot-screen logo
// ============================================================

/// Custom ASCII art banner, loaded at compile time from
/// `assets/banner.txt` in the project root. Drop any ASCII art you like
/// in there (any width/height) and it will be rendered automatically,
/// cycling through the neon palette line by line. Leave the file empty
/// to skip it.
const CUSTOM_BANNER: &str = include_str!("../../assets/banner.txt");


/// Big block-letter "SAWSKAS" wordmark (5x7 dot-matrix font, original
/// design). Each row is split into 6-char columns — one per letter —
/// so `print_title` can give every letter its own neon color.
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

const INPUT_MESSAGE: &str =
    "SELECT AN OPERATION:\n  [1] RUN DAEMON\n  [2] TERMINATE SESSION";

const RUNNING_MESSAGE: &str =
    "DAEMON ONLINE.\nMONITORING SYSTEM PROCESSES...";

const EXIT_MESSAGE: &str =
    "INITIATING SHUTDOWN...\nKILLING THE CONNECTION...";

const ERROR_MESSAGE: &str =
    "ERROR :: UNRECOGNIZED INPUT...\nABORTING SESSION...";

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
}

impl Ui {
    pub fn new(color_palette: Vec<Color>) -> Self {
        Ui {
            palette: color_palette,
            current_state: UiState::Start,
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
    /// palette word-by-word for that classic neon "glitch terminal" look.
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

    /// Prints the boot-screen ASCII art: an optional custom banner from
    /// `assets/banner.txt`, horns, cyber-skull, and a neon multicolor
    /// "SAWSKAS" wordmark.
    fn print_banner(&self) {

        self.print_custom_banner();

        self.print_title();
        println!("{}", format!("  {}", SUBTITLE).color(NEON_PURPLE).italic());
        println!();
    }

    /// Prints the big block-letter title, giving each letter (a fixed
    /// `TITLE_COLUMN_WIDTH`-char column) its own color from the palette
    /// so the wordmark reads as a neon gradient sign.
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
    /// Renders `assets/banner.txt` (if non-empty), cycling through the
    /// neon palette line by line. Lets you swap in your own ASCII art
    /// without touching any code — just edit the file and rebuild.
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

    /// A short animated "loading bar" for boot sequence flavor.
    fn loading_bar(&self, label: &str, color: Color) {
        print!("  {} ", format!("[{label}]").color(color).bold());
        io::stdout().flush().unwrap();
        print!("{}", "│".color(color));

        for _ in 0..20 {
            thread::sleep(Duration::from_millis(35));
            print!("{}", "█".color(color));
            io::stdout().flush().unwrap();
        }

        println!("{} {}", "│".color(color), "ONLINE".color(NEON_GREEN).bold());
    }

    /// A horizontal neon divider line.
    fn print_divider(&self, color: Color) {
        println!("{}", "─".repeat(58).color(color));
    }

    /// Reads a line from stdin and maps it to the next [`UiState`].
    fn take_input(&self) -> UiState {
        let mut input = String::new();

        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read line");

        match input.trim() {
            "1" => UiState::Run,
            "2" => UiState::Exit,
            _ => UiState::Error,
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
                self.type_sentence(INPUT_MESSAGE, 10);
                print!("\n{} ", "root@sawskas:~$".color(NEON_GREEN).bold());
                io::stdout().flush().unwrap();

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