//! End-to-end tests of the app state machine driven entirely through public
//! key/tick events — the same path the real terminal loop uses. The demo
//! provider gives us real (temp-file-backed) wipes with throttling removed via
//! tiny disk sizes, so a full wipe completes in milliseconds.

use std::time::Duration;

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

use scour::app::{App, Screen};
use scour::theme::Glyphs;
use scour::ui;

use scour_core::device::{Bus, Disk, DiskProvider, LockReason, MediaKind};
use scour_core::CoreError;

/// A deterministic provider with two tiny disks (one wipeable, one locked),
/// no throttle, so wipes finish almost instantly.
struct FastProvider {
    dir: tempfile::TempDir,
}

impl FastProvider {
    fn new() -> Self {
        FastProvider {
            dir: tempfile::tempdir().unwrap(),
        }
    }
}

impl DiskProvider for FastProvider {
    fn refresh(&mut self) -> Result<Vec<Disk>, CoreError> {
        let mut disks = Vec::new();
        for (name, lock) in [("sda", None), ("sdb", Some(LockReason::Mounted))] {
            let path = self.dir.path().join(format!("{name}.img"));
            let f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&path)
                .unwrap();
            f.set_len(64 * 1024).unwrap();
            disks.push(Disk {
                path,
                name: name.to_string(),
                model: format!("Fast {name}"),
                serial: format!("S-{name}"),
                size_bytes: 64 * 1024,
                bus: Bus::Demo,
                kind: MediaKind::Ssd,
                removable: false,
                lock,
                throttle_bps: None, // no pacing: finish ASAP
            });
        }
        Ok(disks)
    }
    fn is_simulation(&self) -> bool {
        true
    }
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn special(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn new_app() -> App {
    App::new(Box::new(FastProvider::new()), false)
}

/// Render once to a TestBackend; panics if any widget overflows or layout
/// math is invalid. This is our UI smoke test on every screen.
fn render(app: &App) {
    let glyphs = Glyphs::plain();
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| ui::draw(f, app, &glyphs)).unwrap();
}

#[test]
fn locked_disk_cannot_be_selected() {
    let mut app = new_app();
    // Cursor starts at sda; move to sdb (locked) and try to select.
    app.on_key(special(KeyCode::Down));
    app.on_key(key(' '));
    assert!(
        app.selected.is_empty(),
        "locked disk must not be selectable"
    );
    render(&app);
}

#[test]
fn cannot_arm_with_nothing_selected() {
    let mut app = new_app();
    app.on_key(key('s'));
    assert_eq!(
        app.screen,
        Screen::Disks,
        "must not advance without a selection"
    );
}

#[test]
fn wrong_phrase_blocks_start() {
    let mut app = new_app();
    app.on_key(key(' ')); // select sda
    app.on_key(key('s')); // arm
    assert_eq!(app.screen, Screen::Confirm);
    for c in "WRONG".chars() {
        app.on_key(key(c));
    }
    app.on_key(special(KeyCode::Enter));
    // Still on confirm, no countdown started.
    assert_eq!(app.screen, Screen::Confirm);
    render(&app);
}

#[test]
fn full_wipe_run_to_summary() {
    let mut app = new_app();
    app.on_key(key(' ')); // select sda
                          // Pick the fastest method: Quick Zero Fill (id "zero"), single pass.
    while app.current_scheme().id != "zero" {
        app.on_key(special(KeyCode::Right));
    }
    app.on_key(key('s')); // arm
    assert_eq!(app.screen, Screen::Confirm);
    for c in "ERASE 1 DISK".chars() {
        app.on_key(key(c));
    }
    app.on_key(special(KeyCode::Enter));

    // Burn through the 5s countdown.
    app.on_tick(Duration::from_millis(6000));
    assert_eq!(
        app.screen,
        Screen::Wiping,
        "countdown should have launched the job"
    );

    // Pump ticks until the worker finishes (bounded so a hang fails the test).
    let mut guard = 0;
    while app.screen != Screen::Summary {
        app.on_tick(Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(5));
        guard += 1;
        assert!(guard < 2000, "wipe did not complete in time");
    }

    let report = app.session_report.as_ref().unwrap();
    assert_eq!(report.jobs.len(), 1);
    assert!(
        report.all_succeeded(),
        "the wipe should have verified clean"
    );
    render(&app);
}

#[test]
fn countdown_is_abortable() {
    let mut app = new_app();
    app.on_key(key(' '));
    app.on_key(key('s'));
    for c in "ERASE 1 DISK".chars() {
        app.on_key(key(c));
    }
    app.on_key(special(KeyCode::Enter));
    app.on_tick(Duration::from_millis(1000)); // 4s left
                                              // Any key aborts during countdown.
    app.on_key(special(KeyCode::Esc));
    assert_eq!(app.screen, Screen::Disks);
    assert!(app.jobs.is_empty(), "abort must not have started a job");
}

#[test]
fn every_screen_renders_without_panic() {
    let mut app = new_app();
    render(&app); // Disks
    app.show_help = true;
    render(&app);
    app.show_help = false;

    app.on_key(key(' '));
    app.on_key(key('s'));
    render(&app); // Confirm (typing)
    for c in "ERASE 1 DISK".chars() {
        app.on_key(key(c));
    }
    app.on_key(special(KeyCode::Enter));
    render(&app); // Confirm (countdown)
    app.on_tick(Duration::from_millis(6000));
    render(&app); // Wiping
}

#[test]
fn tiny_terminal_does_not_panic() {
    // Pathological small sizes are a classic TUI crash. We must survive them.
    let app = new_app();
    let glyphs = Glyphs::fancy();
    for (w, h) in [(1, 1), (4, 2), (10, 3), (20, 5), (80, 24)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ui::draw(f, &app, &glyphs)).unwrap();
    }
}

#[test]
fn scheme_navigation_wraps() {
    let mut app = new_app();
    let total = app.schemes.len();
    let start = app.scheme_idx;
    for _ in 0..total {
        app.on_key(special(KeyCode::Right));
    }
    assert_eq!(app.scheme_idx, start, "a full cycle returns to the start");
    app.on_key(special(KeyCode::Left));
    assert_eq!(app.scheme_idx, (start + total - 1) % total);
}

#[test]
fn ctrl_c_requests_quit_not_kill() {
    let mut app = new_app();
    app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    // No running jobs, not pid1 → immediate clean exit request.
    assert!(app.exit.is_some());
}
