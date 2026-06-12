//! Rendering. Pure functions of `&App` → frame; all mutable logic lives in
//! [`crate::app`]. Split out so it can be exercised against ratatui's
//! `TestBackend` (see `tests/app_flow.rs` and `examples/dump.rs`).
//!
//! Layout safety: tabular screens use ratatui's [`Table`]/[`Gauge`] widgets
//! rather than hand-formatted strings. Those widgets clip every cell to its
//! column and draw the [`Block`] border independently, so a glyph the terminal
//! happens to render wider than expected can never push content past a border.
//! Combined with the width-1-safe glyph set in [`crate::theme`], this prevents
//! the border-drift that plagues naive TUIs.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Gauge, Padding, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;

use scour_core::device::{Disk, MediaKind};
use scour_core::engine::{JobStatus, Phase};

use crate::app::{App, Screen};
use crate::theme::Theme;

const APP_TITLE: &str = "SCOUR";
const TAGLINE: &str = "secure disk sanitization";

/// Entry point: draw the whole frame for the current app state.
pub fn draw(f: &mut Frame, app: &App, t: &Theme) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header + rule
            Constraint::Min(5),    // body
            Constraint::Length(1), // status line
            Constraint::Length(1), // key hints
        ])
        .split(area);

    draw_header(f, app, t, chunks[0]);
    match app.screen {
        Screen::Disks => draw_disks(f, app, t, chunks[1]),
        Screen::Confirm => draw_confirm(f, app, t, chunks[1]),
        Screen::Wiping => draw_wiping(f, app, t, chunks[1]),
        Screen::Summary => draw_summary(f, app, t, chunks[1]),
    }
    draw_status(f, app, t, chunks[2]);
    draw_hints(f, app, t, chunks[3]);

    if app.show_help {
        draw_help(f, t, area);
    }
    if app.confirm_quit {
        draw_quit_modal(f, app, t, area);
    }
    if app.confirm_cancel_job.is_some() {
        draw_cancel_modal(f, app, t, area);
    }
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn draw_header(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(rows[0]);

    // The tagline is a luxury; drop it on narrow consoles so the SIMULATION
    // badge always has room.
    let roomy = area.width >= 96;
    let mut left = vec![Span::styled(APP_TITLE, t.title())];
    if roomy {
        left.push(Span::styled(format!("  {TAGLINE}"), t.muted()));
    }
    if app.simulation {
        left.push(Span::raw("  "));
        left.push(Span::styled(" SIMULATION ", t.badge(t.p.warn)));
    }
    f.render_widget(Paragraph::new(Line::from(left)), cols[0]);

    // System summary, right-aligned. Drop the kernel string when cramped.
    let sep = Span::styled("  │  ", t.faint());
    let mut right = vec![
        Span::styled(format!("{} cores", app.sys.cpu_cores), t.muted()),
        sep.clone(),
        Span::styled(app.sys.mem_human(), t.muted()),
    ];
    if roomy {
        right.push(sep);
        right.push(Span::styled(app.sys.kernel.clone(), t.muted()));
    }
    f.render_widget(
        Paragraph::new(Line::from(right).alignment(Alignment::Right)),
        cols[1],
    );

    // Full-width rule under the header.
    let rule = t.g.rule.repeat(area.width as usize);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(rule, t.border()))),
        rows[1],
    );
}

// ---------------------------------------------------------------------------
// Disks screen
// ---------------------------------------------------------------------------

fn draw_disks(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(9)])
        .split(area);

    draw_disk_table(f, app, t, rows[0]);
    draw_method_panel(f, app, t, rows[1]);
}

fn draw_disk_table(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = panel(t, " Disks ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.disks.is_empty() {
        f.render_widget(
            Paragraph::new("No eligible disks detected — press r to rescan.")
                .style(t.muted())
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("DEVICE"),
        Cell::from("MODEL"),
        Cell::from(Line::from("SIZE").alignment(Alignment::Right)),
        Cell::from("BUS"),
        Cell::from("TYPE"),
        Cell::from("STATE"),
    ])
    .style(t.muted());

    let body: Vec<Row> = app
        .disks
        .iter()
        .map(|disk| disk_row(app, t, disk))
        .collect();

    let widths = [
        Constraint::Length(1),  // selection marker
        Constraint::Length(10), // device
        Constraint::Min(16),    // model (flexes)
        Constraint::Length(9),  // size
        Constraint::Length(6),  // bus
        Constraint::Length(4),  // type
        Constraint::Length(11), // state
    ];

    let table = Table::new(body, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(t.cursor_row())
        .highlight_symbol(t.g.cursor);

    let mut state = TableState::default();
    state.select(Some(app.cursor));
    f.render_stateful_widget(table, inner, &mut state);
}

fn disk_row<'a>(app: &App, t: &Theme, disk: &'a Disk) -> Row<'a> {
    let is_sel = app.selected.contains(&disk.name);
    let marker = if disk.is_locked() {
        Span::styled(t.g.locked, t.faint())
    } else if is_sel {
        Span::styled(t.g.sel_on, t.danger())
    } else {
        Span::styled(t.g.sel_off, t.muted())
    };
    let state = match disk.lock {
        Some(reason) => Span::styled(reason.label(), t.warn()),
        None if is_sel => Span::styled("WILL ERASE", t.danger()),
        None => Span::styled("ready", t.muted()),
    };
    let name_style = if is_sel { t.heading() } else { t.text() };

    Row::new(vec![
        Cell::from(marker),
        Cell::from(Span::styled(disk.name.clone(), name_style)),
        Cell::from(Span::styled(disk.model.clone(), t.text())),
        Cell::from(
            Line::from(Span::styled(disk.size_human(), t.text())).alignment(Alignment::Right),
        ),
        Cell::from(Span::styled(disk.bus.label(), t.muted())),
        Cell::from(Span::styled(disk.kind.label(), t.muted())),
        Cell::from(state),
    ])
}

fn draw_method_panel(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = panel(t, " Method ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // name + badge
            Constraint::Length(3), // description
            Constraint::Length(1), // options
            Constraint::Min(1),    // advisory
        ])
        .split(inner);

    let scheme = app.current_scheme();
    let spec = app.spec();
    let total = spec.pass_list().len();

    // -- name line --
    let mut name = vec![
        Span::styled("‹ ", t.faint()),
        Span::styled(scheme.name, t.title()),
        Span::styled(" ›", t.faint()),
        Span::styled(
            format!("   {}/{}", app.scheme_idx + 1, app.schemes.len()),
            t.muted(),
        ),
        Span::styled(format!("   {} pass(es)", scheme.pass_count()), t.muted()),
    ];
    if scheme.recommended {
        name.push(Span::raw("   "));
        name.push(Span::styled(" RECOMMENDED ", t.badge(t.p.ok)));
    }
    f.render_widget(Paragraph::new(Line::from(name)), parts[0]);

    // -- description (clipped to two lines) --
    f.render_widget(
        Paragraph::new(Span::styled(scheme.description, t.muted())).wrap(Wrap { trim: true }),
        parts[1],
    );

    // -- options --
    let mut opt_spans = Vec::new();
    opt_spans.extend(chip(t, "verify", spec.verify.label()));
    opt_spans.push(Span::raw("  "));
    opt_spans.extend(chip(t, "rounds", &app.rounds.to_string()));
    opt_spans.push(Span::raw("  "));
    opt_spans.extend(chip(
        t,
        "final blank",
        if app.final_blank { "on" } else { "off" },
    ));
    opt_spans.push(Span::styled(
        format!("    {total} write pass(es)"),
        t.faint(),
    ));
    f.render_widget(Paragraph::new(Line::from(opt_spans)), parts[2]);

    // -- SSD advisory --
    if app
        .selected_disks()
        .iter()
        .any(|d| d.kind == MediaKind::Ssd)
    {
        let advisory = Line::from(vec![
            Span::styled(" SSD ", t.badge(t.p.warn)),
            Span::styled(
                " overwrites can't reach over-provisioned cells — use firmware \
                 Secure Erase for a true flash purge.",
                t.muted(),
            ),
        ]);
        f.render_widget(Paragraph::new(advisory).wrap(Wrap { trim: true }), parts[3]);
    }
}

/// A `label [value]` chip rendered as a small group of spans: dim label,
/// accent value in brackets.
fn chip<'a>(t: &Theme, label: &'a str, value: &str) -> Vec<Span<'a>> {
    vec![
        Span::styled(format!("{label} "), t.muted()),
        Span::styled(format!("[{value}]"), t.accent()),
    ]
}

// ---------------------------------------------------------------------------
// Confirm screen
// ---------------------------------------------------------------------------

fn draw_confirm(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(t.g.border)
        .border_style(t.danger())
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            " POINT OF NO RETURN ",
            t.badge(t.p.danger_bright),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(gate) = &app.gate else { return };

    let mut lines = vec![
        Line::from(Span::styled(
            "These disks will be PERMANENTLY and IRRECOVERABLY erased:",
            t.warn(),
        )),
        Line::from(""),
    ];
    for d in app.selected_disks() {
        lines.push(Line::from(vec![
            Span::styled(format!("{}  ", t.g.sel_on), t.danger()),
            Span::styled(format!("{:<10} ", d.name), t.heading()),
            Span::styled(format!("{}  ", clip(&d.model, 28)), t.text()),
            Span::styled(d.size_human(), t.muted()),
        ]));
    }
    lines.push(Line::from(""));
    let spec = app.spec();
    lines.push(Line::from(vec![
        Span::styled("Method  ", t.faint()),
        Span::styled(spec.scheme.name, t.accent()),
        Span::styled(
            format!(
                "   {} write pass(es)   verify {}",
                spec.pass_list().len(),
                spec.verify.label()
            ),
            t.muted(),
        ),
    ]));
    lines.push(Line::from(""));

    if let Some(remaining) = gate.countdown_remaining_ms() {
        let secs = (remaining as f64 / 1000.0).ceil() as u64;
        lines.push(Line::from(vec![
            Span::styled("Starting in ", t.warn()),
            Span::styled(format!("{secs}"), t.danger()),
            Span::styled("s — press any key to ABORT.", t.warn()),
        ]));
    } else {
        let matched = gate.phrase_matches();
        lines.push(Line::from(vec![
            Span::styled("Type ", t.muted()),
            Span::styled(format!("\"{}\"", gate.phrase()), t.warn()),
            Span::styled("  then Enter to confirm, Esc to cancel.", t.muted()),
        ]));
        let caret = if matched { "  OK" } else { "" };
        lines.push(Line::from(vec![
            Span::styled("  > ", t.faint()),
            Span::styled(
                gate.typed().to_string(),
                if matched { t.ok() } else { t.heading() },
            ),
            Span::styled(caret, t.ok()),
        ]));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

// ---------------------------------------------------------------------------
// Wiping screen
// ---------------------------------------------------------------------------

fn draw_wiping(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = panel(t, " Erasing ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.jobs.is_empty() {
        return;
    }

    let row_h: u16 = 3;
    let max_rows = (inner.height / row_h).max(1) as usize;
    let shown = app.jobs.len().min(max_rows);

    let constraints: Vec<Constraint> = (0..shown).map(|_| Constraint::Length(row_h)).collect();
    let slots = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let spinner = t.g.spinner[app.spinner_frame % t.g.spinner.len()];

    for (i, job) in app.jobs.iter().take(shown).enumerate() {
        let snap = job.progress.snapshot();
        let tracker = &app.trackers[i];
        let is_cursor = i == app.cursor;

        let (mark, mark_style) = match snap.phase {
            Phase::Done => ("done", t.ok()),
            Phase::Failed => ("FAIL", t.danger()),
            Phase::Cancelled => ("stop", t.warn()),
            _ => (spinner, t.accent()),
        };

        let speed = tracker.bytes_per_sec();
        let speed_str = if speed > 1.0 {
            format!("{:.0} MB/s", speed / 1_000_000.0)
        } else {
            "—".to_string()
        };
        let eta_str = match tracker.eta(snap.work_done, snap.work_total) {
            Some(d) if !snap.phase.is_terminal() => fmt_duration(d),
            _ => "—".to_string(),
        };

        let name_style = if is_cursor { t.title() } else { t.heading() };
        let header = Line::from(vec![
            Span::styled(format!("{mark:<5}"), mark_style),
            Span::styled(format!("{:<11}", job.disk.name), name_style),
            Span::styled(
                format!("pass {}/{}  ", snap.pass_index, snap.pass_count),
                t.muted(),
            ),
            Span::styled(format!("{:<14}", clip(&snap.pass_label, 14)), t.accent()),
            Span::styled(snap.phase.label(), t.muted()),
            Span::styled(format!("   {speed_str}   ETA {eta_str}"), t.faint()),
        ]);

        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(slots[i]);
        f.render_widget(Paragraph::new(header), sub[0]);

        let gauge_color = match snap.phase {
            Phase::Done => t.p.ok,
            Phase::Failed => t.p.danger,
            Phase::Cancelled => t.p.warn,
            _ => t.p.accent,
        };
        let label = match &snap.error {
            Some(err) => clip(err, sub[1].width.saturating_sub(2) as usize),
            None => format!("{:.1}%", snap.ratio() * 100.0),
        };
        let gauge = Gauge::default()
            .gauge_style(Style::new().fg(gauge_color).bg(t.p.sel_bg))
            .use_unicode(t.g.fine_gauge)
            .ratio(snap.ratio())
            .label(Span::styled(label, t.heading()));
        f.render_widget(gauge, sub[1]);
    }
}

// ---------------------------------------------------------------------------
// Summary screen
// ---------------------------------------------------------------------------

fn draw_summary(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let Some(report) = &app.session_report else {
        return;
    };
    let all_ok = report.all_succeeded();
    let border_style = if all_ok { t.ok() } else { t.danger() };
    let title = if all_ok {
        Span::styled(" COMPLETE ", t.badge(t.p.ok))
    } else {
        Span::styled(" COMPLETED WITH ISSUES ", t.badge(t.p.danger_bright))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(t.g.border)
        .border_style(border_style)
        .padding(Padding::horizontal(1))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(inner);

    let header = Row::new(vec![
        Cell::from("DEVICE"),
        Cell::from("MODEL"),
        Cell::from("RESULT"),
        Cell::from("METHOD"),
        Cell::from(Line::from("PASSES").alignment(Alignment::Right)),
        Cell::from(Line::from("TIME").alignment(Alignment::Right)),
        Cell::from(Line::from("RATE").alignment(Alignment::Right)),
    ])
    .style(t.muted());

    let mut body: Vec<Row> = Vec::new();
    for job in &report.jobs {
        let style = match job.status {
            JobStatus::Success => t.ok(),
            JobStatus::Cancelled => t.warn(),
            _ => t.danger(),
        };
        body.push(Row::new(vec![
            Cell::from(Span::styled(job.disk_name.clone(), t.heading())),
            Cell::from(Span::styled(clip(&job.disk_model, 24), t.muted())),
            Cell::from(Span::styled(job.status.label(), style)),
            Cell::from(Span::styled(job.scheme_name.clone(), t.text())),
            Cell::from(
                Line::from(format!("{}/{}", job.passes_completed, job.pass_count))
                    .alignment(Alignment::Right),
            ),
            Cell::from(Line::from(fmt_secs(job.duration_secs)).alignment(Alignment::Right)),
            Cell::from(
                Line::from(format!("{:.0} MB/s", job.avg_write_mib_s * 1.048576))
                    .alignment(Alignment::Right),
            ),
        ]));
        if let Some(err) = &job.error {
            body.push(Row::new(vec![
                Cell::from(""),
                Cell::from(Span::styled(format!("└ {err}"), t.danger())),
            ]));
        }
    }

    let widths = [
        Constraint::Length(10),
        Constraint::Min(16),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(9),
    ];
    f.render_widget(
        Table::new(body, widths).header(header).column_spacing(1),
        parts[0],
    );

    let footer = match &app.saved_report_path {
        Some(path) => Line::from(vec![
            Span::styled("Report written  ", t.ok()),
            Span::styled(path.clone(), t.muted()),
        ]),
        None => Line::from(Span::styled(
            "Press w to write a JSON erasure report.",
            t.muted(),
        )),
    };
    f.render_widget(Paragraph::new(footer), parts[1]);
}

// ---------------------------------------------------------------------------
// Status line + hint bar
// ---------------------------------------------------------------------------

fn draw_status(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    if let Some((msg, _)) = &app.status {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" • ", t.accent()),
                Span::styled(msg.clone(), t.warn()),
            ])),
            area,
        );
    }
}

fn draw_hints(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let pairs: &[(&str, &str)] = match app.screen {
        Screen::Disks => &[
            ("up/dn", "move"),
            ("spc", "select"),
            ("</>", "method"),
            ("v", "verify"),
            ("+/-", "rounds"),
            ("b", "blank"),
            ("r", "rescan"),
            ("s", "START"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Screen::Confirm => &[("type phrase", ""), ("enter", "confirm"), ("esc", "cancel")],
        Screen::Wiping => &[
            ("up/dn", "select"),
            ("c", "cancel"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Screen::Summary => {
            if app.pid1 {
                &[
                    ("w", "report"),
                    ("n", "new"),
                    ("r", "reboot"),
                    ("p", "power off"),
                ]
            } else {
                &[("w", "report"), ("n", "new"), ("q", "quit")]
            }
        }
    };

    let mut spans = Vec::new();
    for (k, label) in pairs {
        spans.push(Span::styled(format!(" {k} "), t.keycap()));
        if label.is_empty() {
            spans.push(Span::raw(" "));
        } else {
            spans.push(Span::styled(format!(" {label}   "), t.muted()));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

fn draw_help(f: &mut Frame, t: &Theme, area: Rect) {
    let rect = centered(area, 66, 19);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(t.g.border)
        .border_style(t.accent())
        .padding(Padding::horizontal(1))
        .title(Span::styled(" Help ", t.title()));
    let text = vec![
        Line::from(Span::styled("Scour — secure disk sanitization", t.title())),
        Line::from(""),
        Line::from(vec![Span::styled("Disks screen", t.heading())]),
        Line::from("  up/down or j/k   move the cursor"),
        Line::from("  space or enter   toggle a disk for erasure"),
        Line::from("  < / >  or [ / ]  change wipe method"),
        Line::from("  v                cycle verify mode (off / last / all)"),
        Line::from("  + / -            repeat the method for N rounds"),
        Line::from("  b                toggle a final zero-blanking pass"),
        Line::from("  s                arm and start (requires the typed phrase)"),
        Line::from(""),
        Line::from(Span::styled(
            "Locked disks (mounted / swap / boot / in-use) can never be selected.",
            t.muted(),
        )),
        Line::from(Span::styled(
            "Random passes are verified by regenerating their seed stream.",
            t.muted(),
        )),
        Line::from(""),
        Line::from(Span::styled("Press any key to close.", t.faint())),
    ];
    f.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: true }),
        rect,
    );
}

fn draw_quit_modal(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let rect = centered(area, 58, 8);
    f.render_widget(Clear, rect);
    let mut lines = vec![Line::from("")];
    if app.jobs_running() {
        lines.push(
            Line::from(Span::styled(
                "Wipes are still running — quitting cancels them.",
                t.warn(),
            ))
            .alignment(Alignment::Center),
        );
    }
    lines.push(Line::from(""));
    let keys = if app.pid1 {
        "r  reboot      p  power off      n / esc  stay"
    } else {
        "y  quit        n / esc  stay"
    };
    lines.push(Line::from(Span::styled(keys, t.text())).alignment(Alignment::Center));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(t.g.border)
        .border_style(t.danger())
        .title(Span::styled(" Quit? ", t.danger()));
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn draw_cancel_modal(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let rect = centered(area, 58, 9);
    f.render_widget(Clear, rect);
    let name = app
        .confirm_cancel_job
        .and_then(|i| app.jobs.get(i))
        .map(|j| j.disk.name.clone())
        .unwrap_or_default();
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("Cancel the wipe of {name}?"),
            t.warn(),
        ))
        .alignment(Alignment::Center),
        Line::from(Span::styled(
            "The disk will be left partially overwritten.",
            t.muted(),
        ))
        .alignment(Alignment::Center),
        Line::from(""),
        Line::from(Span::styled(
            "y  cancel job      n / esc  keep going",
            t.text(),
        ))
        .alignment(Alignment::Center),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(t.g.border)
        .border_style(t.warn())
        .title(Span::styled(" Cancel job ", t.warn()));
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A standard bordered panel with a styled title.
fn panel<'a>(t: &Theme, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(t.g.border)
        .border_style(t.border())
        .padding(Padding::horizontal(1))
        .title(Span::styled(title, t.title()))
}

/// Clip a string to `max` characters (char-based, so multibyte is safe),
/// appending a single ellipsis-dot when truncated.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('.');
        out
    }
}

/// Center a `w`×`h` rect within `area`, clamped to fit.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn fmt_duration(d: std::time::Duration) -> String {
    fmt_secs(d.as_secs_f64())
}

fn fmt_secs(secs: f64) -> String {
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}
