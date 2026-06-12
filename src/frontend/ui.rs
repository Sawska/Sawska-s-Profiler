use std::io::{self,Write};
use std::thread;
use std::time::Duration;
use colored::{Color, Colorize};
use std::sync::LazyLock;
use std::process;
use std::io;

static INITIAL_MESSAGE: LazyLock<String> = LazyLock::new(|| {
    String::from("Initiating demon... \n Running scripts...")
})

static GREETING_MESSAGE: LazyLock<String> = LazyLock::new(|| {
    String::from("Welcome to the Sawskas's program profile!")
})

static INPUT_MESSAGE: LazyLock<String> = LazyLock::new(|| {
    String::from("Choose an option 1.Run...\n 2. Exit...")
})

static RUNNING_MESSAGE: LazyLock<String> = LazyLock::new(|| {
    String::from("Starting the demon...")
})

static EXIT_MESSAGE: LazyLock<String> = LazyLock::new(|| {
    String::from("Initiating exit... \n Killing the connection..")
})

static ERROR_MESSAGE: LazyLock<String> = LazyLock::new(|| {
    String::from("Error occured during running.... \n Exiting....")
})

#[derive(Debug, PartialEq)]
enum UiState {    
    Start,
    Input
    Run,
    Exit,
    Error,
}

pub struct Ui {
    palette: Vec<&Color>
    current_state: UiState,
}

impl Ui {
    pub fn new(color_palette: Vec&<Color>) -> Self {
        Ui {
            palette: color_palette,
            current_state: UiState::Start;
        }
    }
    fn typing_effect(word: &str, delay_ms: u64,color: &Color) {
        for c in word.chars() {
            print!("{}", c.to_string().color(*color));
            
            io::stdout().flush().unwrap();
            thread::sleep(Duration::from_millis(delay_ms));
        }
        println!();
    }
    fn type_sentence(text: &str, delay_ms: u64) {
        let mut colors = palette.iter().cycle();

        for word in text.split(' ') {
            let current_color = colors.next().unwrap();

            typing_effect(word,delay_ms,current_color);
        }
    }

    fn take_input() -> UiState {
        let mut input = String::new();

        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read line");

        let current_state: UiState = match input.trim() {
            "1" => UiState::Run,
            "2" => UiState::Exit
            _ => {
                UiState::Error
            }
        } 
    }

    pub fn update(&mut self) {
        match self.current_state {
            UiState::Start => {
                type_sentence(&*INITIAL_MESSAGE,100);
                type_sentence(&*GREETING_MESSAGE,50);
                
                self.current_state = UiState::Input;
            }
            UiState::Input => {
                type_sentence(INPUT_MESSAGE,50);
                self.current_state = take_input();
            }
            UiState::Run => {
                type_sentence(&*RUNNING_MESSAGE,50);
            }
            UiState::Exit => {
                type_sentence(&*EXIT_MESSAGE,50);
                process(1);
            }
            UiState::Error => {
                type_sentence(&*ERROR_MESSAGE,50);
                process(0);
            }
        }
    }
}