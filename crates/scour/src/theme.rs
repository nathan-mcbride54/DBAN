//! Colors and glyphs.
//!
//! Two constraints shape this module:
//! * The live appliance renders on the bare Linux framebuffer console, which
//!   only reliably supports the 16 ANSI colors and the CP437-ish glyph set —
//!   no rounded corners, no braille spinners, no emoji.
//! * Desktop terminals (where `--demo` runs) can take the prettier set.
//!
//! `Glyphs::detect()` picks per environment so the UI never renders tofu.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

// -- palette (16-color safe) -------------------------------------------------

pub const ACCENT: Color = Color::Cyan;
pub const ACCENT_ALT: Color = Color::Magenta;
pub const OK: Color = Color::Green;
pub const WARN: Color = Color::Yellow;
pub const DANGER: Color = Color::Red;
pub const MUTED: Color = Color::DarkGray;
pub const TEXT: Color = Color::Gray;
pub const BRIGHT: Color = Color::White;

pub fn title() -> Style {
    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn muted() -> Style {
    Style::new().fg(MUTED)
}

pub fn ok() -> Style {
    Style::new().fg(OK)
}

pub fn warn() -> Style {
    Style::new().fg(WARN).add_modifier(Modifier::BOLD)
}

pub fn danger() -> Style {
    Style::new().fg(DANGER).add_modifier(Modifier::BOLD)
}

pub fn key_hint() -> Style {
    Style::new().fg(Color::Black).bg(ACCENT)
}

pub fn selected_row() -> Style {
    Style::new()
        .fg(BRIGHT)
        .bg(Color::Blue)
        .add_modifier(Modifier::BOLD)
}

// -- glyphs ------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Glyphs {
    pub border: BorderType,
    pub sel_on: &'static str,
    pub sel_off: &'static str,
    pub locked: &'static str,
    pub pointer: &'static str,
    pub bullet: &'static str,
    pub spinner: &'static [&'static str],
}

impl Glyphs {
    /// Fancy set for desktop terminals.
    pub fn fancy() -> Self {
        Glyphs {
            border: BorderType::Rounded,
            sel_on: "[●]",
            sel_off: "[ ]",
            locked: " ✕ ",
            pointer: "▶",
            bullet: "•",
            spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
        }
    }

    /// CP437-safe set for the Linux framebuffer console.
    pub fn plain() -> Self {
        Glyphs {
            border: BorderType::Plain,
            sel_on: "[*]",
            sel_off: "[ ]",
            locked: " x ",
            pointer: ">",
            bullet: "·",
            spinner: &["|", "/", "-", "\\"],
        }
    }

    /// `TERM=linux` means the kernel console: degrade gracefully.
    pub fn detect() -> Self {
        match std::env::var("TERM") {
            Ok(term) if term == "linux" || term == "dumb" => Self::plain(),
            _ => Self::fancy(),
        }
    }
}
