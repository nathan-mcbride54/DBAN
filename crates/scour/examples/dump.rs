//! Render each screen to a TestBackend and print the buffer as text, so UI
//! glitches (border drift, overflow) are visible without a real terminal.
//! Run: cargo run -p scour --example dump

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use scour::app::App;
use scour::theme::Theme;
use scour::ui;

use scour_core::demo::DemoProvider;

fn dump(term: &Terminal<TestBackend>, label: &str) {
    let buf = term.backend().buffer();
    let area = *buf.area();
    println!("== {label} ({}x{}) ==", area.width, area.height);
    // Top ruler so column drift is obvious.
    print!("    ");
    for x in 0..area.width {
        print!("{}", (b'0' + (x % 10) as u8) as char);
    }
    println!();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        println!("{y:>3} {line}");
    }
    println!();
}

fn main() {
    let theme = Theme::fancy();
    let provider = DemoProvider::new().unwrap();
    let mut app = App::new(Box::new(provider), false);
    // Select the first unlocked disk so we see a WILL ERASE row + SSD warning.
    app.on_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char(' '),
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));

    for (w, h) in [(80u16, 24u16), (100, 30), (120, 30)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ui::draw(f, &app, &theme)).unwrap();
        dump(&term, &format!("Disks {w}x{h}"));
    }

    // Show a firmware method selected, with an NVMe disk chosen.
    if let Some(idx) = app.disks.iter().position(|d| d.name == "nvme0n1") {
        app.cursor = idx;
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char(' '),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
    }
    while !app.is_firmware() {
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Right,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
    }
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| ui::draw(f, &app, &theme)).unwrap();
    dump(&term, "Firmware method 100x30");
}
