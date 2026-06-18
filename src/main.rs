mod cli;
mod frontend;
mod interrupt;
mod report;

use frontend::ui::Ui;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match cli::parse(&args) {
        Ok(cli::Mode::Interactive) => {
            let mut app = Ui::new(frontend::ui::default_palette());
            loop {
                app.update();
            }
        }
        Ok(cli::Mode::Help) => {
            print!("{}", cli::help());
        }
        Ok(cli::Mode::Run(cfg)) => {
            std::process::exit(cli::run(cfg));
        }
        Err(e) => {
            eprintln!("error: {e}\n");
            print!("{}", cli::help());
            std::process::exit(2);
        }
    }
}
