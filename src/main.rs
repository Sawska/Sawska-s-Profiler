mod frontend;
mod interrupt;

use frontend::ui::Ui;

fn main() {
    let mut app = Ui::new(frontend::ui::default_palette());

    loop {
        app.update();
    }
}