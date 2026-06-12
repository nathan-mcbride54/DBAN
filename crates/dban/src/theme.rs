//! Colors, glyphs, and styles.
//!
//! Two hard constraints shape this module:
//!
//! * **The live appliance renders on the bare Linux framebuffer console**
//!   (`TERM=linux`). That console supports only the 16 ANSI colors and a
//!   CP437-ish glyph set: no truecolor, no rounded corners, no exotic Unicode.
//! * **Terminal fonts disagree about glyph width.** Characters with an
//!   *ambiguous* or *wide* East Asian Width (★ ● ▶ ✕ → · ⚠ …) are counted as
//!   one column by `unicode-width` (what ratatui uses for layout) but drawn as
//!   two columns by many fonts. The cumulative error detaches box borders —
//!   the classic TUI "border drift". We therefore restrict ourselves to glyphs
//!   that are width-1 in every font: ASCII, box-drawing, and the block-element
//!   range (░▒▓█▌▏…), which terminals special-case.
//!
//! [`Theme::detect`] picks a truecolor + block-glyph theme for desktop
//! terminals and a 16-color + ASCII theme for the kernel console, so the UI is
//! both pretty where it can be and never garbled where it can't.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

/// A complete color palette. Desktop themes use 24-bit RGB; the console theme
/// uses named ANSI colors that survive a 16-color terminal.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Primary accent (links, highlights, method name).
    pub accent: Color,
    /// Secondary accent (firmware badge).
    pub accent_alt: Color,
    /// Success / safe state.
    pub ok: Color,
    /// Warning / caution.
    pub warn: Color,
    /// Danger / destructive.
    pub danger: Color,
    /// Brighter danger, used for badges.
    pub danger_bright: Color,
    /// Primary body text.
    pub text: Color,
    /// Brightest text (headings, emphasized values).
    pub bright: Color,
    /// De-emphasized text.
    pub muted: Color,
    /// Faintest text and borders.
    pub faint: Color,
    /// Background tint for the row under the cursor.
    pub sel_bg: Color,
    /// Background for the danger/confirm surfaces.
    pub danger_bg: Color,
}

impl Palette {
    /// Truecolor palette (Tailwind-ish slate/sky/amber) for desktop terminals.
    pub const fn truecolor() -> Self {
        Palette {
            accent: Color::Rgb(56, 189, 248),       // sky-400
            accent_alt: Color::Rgb(167, 139, 250),  // violet-400
            ok: Color::Rgb(74, 222, 128),           // green-400
            warn: Color::Rgb(251, 191, 36),         // amber-400
            danger: Color::Rgb(248, 113, 113),      // red-400
            danger_bright: Color::Rgb(239, 68, 68), // red-500
            text: Color::Rgb(226, 232, 240),        // slate-200
            bright: Color::Rgb(248, 250, 252),      // slate-50
            muted: Color::Rgb(148, 163, 184),       // slate-400
            faint: Color::Rgb(71, 85, 105),         // slate-600
            sel_bg: Color::Rgb(30, 41, 59),         // slate-800
            danger_bg: Color::Rgb(60, 20, 24),      // deep maroon
        }
    }

    /// 16-color palette for the bare Linux console.
    pub const fn ansi() -> Self {
        Palette {
            accent: Color::Cyan,
            accent_alt: Color::Magenta,
            ok: Color::Green,
            warn: Color::Yellow,
            danger: Color::Red,
            danger_bright: Color::LightRed,
            text: Color::Gray,
            bright: Color::White,
            muted: Color::DarkGray,
            faint: Color::DarkGray,
            sel_bg: Color::Blue,
            danger_bg: Color::Red,
        }
    }
}

/// Width-1-safe glyphs. Every member is either ASCII, a box-drawing character,
/// or a block element — all of which render at exactly one column everywhere.
#[derive(Clone, Copy, Debug)]
pub struct Glyphs {
    /// Block border style (rounded on desktop, plain on the console).
    pub border: BorderType,
    /// Cursor row indicator (a left bar). Trailing space included.
    pub cursor: &'static str,
    /// Selection marker shown for a disk that will be erased.
    pub sel_on: &'static str,
    /// Marker for a disk that cannot be selected.
    pub locked: &'static str,
    /// Empty selection slot.
    pub sel_off: &'static str,
    /// Horizontal rule fill.
    pub rule: &'static str,
    /// Spinner frames for active work.
    pub spinner: &'static [&'static str],
    /// Whether the gauge may use sub-cell block resolution (▏▎▍…).
    pub fine_gauge: bool,
}

impl Glyphs {
    /// Block-element set for desktop terminals.
    pub const fn fancy() -> Self {
        Glyphs {
            border: BorderType::Rounded,
            cursor: "▌ ",
            sel_on: "█",
            locked: "▒",
            sel_off: " ",
            rule: "─",
            spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            fine_gauge: true,
        }
    }

    /// CP437-safe set for the Linux framebuffer console.
    pub const fn plain() -> Self {
        Glyphs {
            border: BorderType::Plain,
            cursor: "> ",
            sel_on: "#",
            locked: "x",
            sel_off: " ",
            rule: "-",
            spinner: &["|", "/", "-", "\\"],
            fine_gauge: false,
        }
    }
}

/// Palette + glyphs + style helpers, threaded through every render function.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// The color palette.
    pub p: Palette,
    /// The glyph set.
    pub g: Glyphs,
}

impl Theme {
    /// Truecolor + block glyphs.
    pub const fn fancy() -> Self {
        Theme {
            p: Palette::truecolor(),
            g: Glyphs::fancy(),
        }
    }

    /// 16-color + ASCII glyphs.
    pub const fn plain() -> Self {
        Theme {
            p: Palette::ansi(),
            g: Glyphs::plain(),
        }
    }

    /// Pick a theme from the environment. The kernel console (`TERM=linux`)
    /// and dumb terminals get the safe theme; everything else gets the pretty
    /// one. `NO_COLOR` and an explicit `DBAN_PLAIN` also force safe mode.
    pub fn detect() -> Self {
        if std::env::var_os("DBAN_PLAIN").is_some() || std::env::var_os("NO_COLOR").is_some() {
            return Self::plain();
        }
        match std::env::var("TERM").as_deref() {
            Ok("linux") | Ok("dumb") | Ok("") => Self::plain(),
            Err(_) => Self::plain(),
            _ => Self::fancy(),
        }
    }

    // -- style helpers -------------------------------------------------------

    /// Bold accent — panel titles and the app name.
    pub fn title(&self) -> Style {
        Style::new().fg(self.p.accent).add_modifier(Modifier::BOLD)
    }
    /// Bold bright — headings and emphasized values.
    pub fn heading(&self) -> Style {
        Style::new().fg(self.p.bright).add_modifier(Modifier::BOLD)
    }
    /// Primary body text.
    pub fn text(&self) -> Style {
        Style::new().fg(self.p.text)
    }
    /// De-emphasized text.
    pub fn muted(&self) -> Style {
        Style::new().fg(self.p.muted)
    }
    /// Faintest text.
    pub fn faint(&self) -> Style {
        Style::new().fg(self.p.faint)
    }
    /// Accent-colored (non-bold) text.
    pub fn accent(&self) -> Style {
        Style::new().fg(self.p.accent)
    }
    /// Success-colored text.
    pub fn ok(&self) -> Style {
        Style::new().fg(self.p.ok)
    }
    /// Bold warning-colored text.
    pub fn warn(&self) -> Style {
        Style::new().fg(self.p.warn).add_modifier(Modifier::BOLD)
    }
    /// Bold danger-colored text.
    pub fn danger(&self) -> Style {
        Style::new().fg(self.p.danger).add_modifier(Modifier::BOLD)
    }
    /// Border / rule color.
    pub fn border(&self) -> Style {
        Style::new().fg(self.p.faint)
    }
    /// Reverse-video badge, e.g. a green "RECOMMENDED" or amber "SSD" chip.
    pub fn badge(&self, bg: Color) -> Style {
        Style::new()
            .fg(Color::Black)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
    }
    /// Key-cap style used in the hint bar.
    pub fn keycap(&self) -> Style {
        Style::new()
            .fg(Color::Black)
            .bg(self.p.accent)
            .add_modifier(Modifier::BOLD)
    }
    /// Background highlight for the cursor row.
    pub fn cursor_row(&self) -> Style {
        Style::new().bg(self.p.sel_bg).add_modifier(Modifier::BOLD)
    }
}
