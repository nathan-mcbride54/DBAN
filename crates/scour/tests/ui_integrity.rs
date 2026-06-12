//! Rendering-integrity guards.
//!
//! These tests defend against the specific class of bug that produced the
//! detached/floating borders reported in the screenshot:
//!
//! * **Glyph width.** Every cell the UI emits must be exactly one display
//!   column wide. A glyph that `unicode-width` (ratatui's layout oracle) sizes
//!   at one column but a terminal font draws at two will desynchronise every
//!   following cell and detach the right border. We assert width ≤ 1 for every
//!   rendered cell, across all screens, sizes, and both themes.
//! * **Border integrity.** On the Disks screen the panel borders must form
//!   clean vertical lines — the left and right edge columns of each panel are
//!   box-drawing glyphs on every interior row, never overwritten by content.

use std::collections::HashSet;
use std::time::Duration;

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use unicode_width::UnicodeWidthStr;

use scour::app::{App, Screen};
use scour::theme::Theme;
use scour::ui;

use scour_core::demo::DemoProvider;

fn app() -> App {
    App::new(Box::new(DemoProvider::new().unwrap()), false)
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

/// Render `app` at `w`×`h` in `theme` and return the buffer for inspection.
fn render(app: &App, theme: &Theme, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::draw(f, app, theme)).unwrap();
    term.backend().buffer().clone()
}

/// Assert every non-empty cell symbol is at most one display column wide.
fn assert_all_width_one(buf: &ratatui::buffer::Buffer, ctx: &str) {
    let area = *buf.area();
    let mut offenders: HashSet<String> = HashSet::new();
    for y in 0..area.height {
        for x in 0..area.width {
            let sym = buf[(x, y)].symbol();
            if !sym.is_empty() && sym.width() > 1 {
                offenders.insert(sym.to_string());
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{ctx}: found wide glyphs that will drift borders: {offenders:?}"
    );
}

#[test]
fn no_wide_glyphs_on_any_screen() {
    for theme in [Theme::fancy(), Theme::plain()] {
        // Drive the app through every screen and snapshot each.
        let mut a = app();
        for (w, h) in [(80u16, 24u16), (100, 30), (120, 40), (132, 50)] {
            // Disks
            assert_all_width_one(&render(&a, &theme, w, h), "disks");
        }

        // Select a disk and open the confirm screen.
        a.on_key(key(' '));
        a.on_key(key('s'));
        assert_eq!(a.screen, Screen::Confirm);
        assert_all_width_one(&render(&a, &theme, 100, 30), "confirm-typing");
        for c in "ERASE 1 DISK".chars() {
            a.on_key(key(c));
        }
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_all_width_one(&render(&a, &theme, 100, 30), "confirm-countdown");

        // Into the wipe.
        a.on_tick(Duration::from_millis(6000));
        assert_eq!(a.screen, Screen::Wiping);
        assert_all_width_one(&render(&a, &theme, 100, 30), "wiping");

        // Modals.
        a.show_help = true;
        assert_all_width_one(&render(&a, &theme, 100, 30), "help");
        a.show_help = false;
        a.confirm_quit = true;
        assert_all_width_one(&render(&a, &theme, 100, 30), "quit-modal");
    }
}

#[test]
fn disk_panel_borders_are_intact() {
    // On the Disks screen the panel's left and right edges must be unbroken
    // vertical box-drawing glyphs — never clobbered by row content.
    let a = app();
    for theme in [Theme::fancy(), Theme::plain()] {
        let (w, h) = (100u16, 30u16);
        let buf = render(&a, &theme, w, h);

        // Both themes draw vertical edges with the same box-drawing glyph;
        // rounded vs. plain only changes the corners.
        let vbar = "│";
        let mut checked_rows = 0;
        for y in 0..h {
            let left = buf[(0, y)].symbol().to_string();
            let right = buf[(w - 1, y)].symbol().to_string();
            // Interior rows of a bordered panel have the vbar on both edges.
            if left == vbar {
                assert_eq!(
                    right, vbar,
                    "row {y}: left edge is a border but right edge is {right:?} — border drift"
                );
                checked_rows += 1;
            }
        }
        assert!(
            checked_rows >= 5,
            "expected several bordered interior rows, found {checked_rows}"
        );
    }
}

#[test]
fn cursor_and_selection_markers_render() {
    // The cursor highlight symbol and the selection block must actually appear
    // once a disk is selected, in the fancy theme.
    let mut a = app();
    a.on_key(key(' ')); // select first disk
    let theme = Theme::fancy();
    let buf = render(&a, &theme, 100, 30);
    let mut text = String::new();
    let area = *buf.area();
    for y in 0..area.height {
        for x in 0..area.width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(text.contains('█'), "selection block should be visible");
    assert!(text.contains('▌'), "cursor bar should be visible");
    assert!(
        text.contains("WILL ERASE"),
        "selected disk should be flagged"
    );
}
