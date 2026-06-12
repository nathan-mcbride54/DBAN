//! Rendering. Pure functions of `&App` → frame; all mutable logic lives in
//! `app`. Split out so it can be exercised against ratatui's `TestBackend`.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Wrap};
use ratatui::Frame;

use scour_core::device::MediaKind;
use scour_core::engine::Phase;

use crate::app::{App, Screen};
use crate::theme::{self, Glyphs};

const APP_TITLE: &str = "SCOUR";
const TAGLINE: &str = "secure disk sanitization";

pub fn draw(f: &mut Frame, app: &App, glyphs: &Glyphs) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(5),    // body
            Constraint::Length(1), // status
            Constraint::Length(1), // key hints
        ])
        .split(area);

    draw_header(f, app, chunks[0]);
    match app.screen {
        Screen::Disks => draw_disks(f, app, glyphs, chunks[1]),
        Screen::Confirm => draw_confirm(f, app, chunks[1]),
        Screen::Wiping => draw_wiping(f, app, glyphs, chunks[1]),
        Screen::Summary => draw_summary(f, app, chunks[1]),
    }
    draw_status(f, app, chunks[2]);
    draw_hints(f, app, chunks[3]);

    if app.show_help {
        draw_help(f, app, area);
    }
    if app.confirm_quit {
        draw_quit_modal(f, app, area);
    }
    if app.confirm_cancel_job.is_some() {
        draw_cancel_modal(f, app, area);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(theme::muted());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mode = if app.simulation { "  [SIMULATION]" } else { "" };
    let left = Line::from(vec![
        Span::styled(APP_TITLE, theme::title()),
        Span::styled(format!(" {TAGLINE}"), theme::muted()),
        Span::styled(mode, theme::warn()),
    ]);
    let right = Line::from(vec![Span::styled(
        format!(
            "{}c · {} · {}",
            app.sys.cpu_cores,
            app.sys.mem_human(),
            app.sys.kernel
        ),
        theme::muted(),
    )])
    .alignment(Alignment::Right);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(inner);
    f.render_widget(Paragraph::new(left), cols[0]);
    f.render_widget(Paragraph::new(right), cols[1]);
}

fn draw_disks(f: &mut Frame, app: &App, glyphs: &Glyphs, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(6)])
        .split(area);

    // -- disk table --
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(glyphs.border)
        .border_style(theme::muted())
        .title(Span::styled(" Disks ", theme::title()));
    let inner = block.inner(cols[0]);
    f.render_widget(block, cols[0]);

    if app.disks.is_empty() {
        f.render_widget(
            Paragraph::new("No eligible disks detected. Press r to rescan.")
                .style(theme::muted())
                .alignment(Alignment::Center),
            inner,
        );
    } else {
        let mut lines = Vec::with_capacity(app.disks.len() + 1);
        lines.push(Line::from(Span::styled(
            format!(
                "    {:<11} {:<28} {:>9} {:<6} {:<5} {}",
                "DEVICE", "MODEL", "SIZE", "BUS", "TYPE", "STATE"
            ),
            theme::muted(),
        )));
        for (i, disk) in app.disks.iter().enumerate() {
            let is_cursor = i == app.cursor;
            let is_sel = app.selected.contains(&disk.name);
            let mark = if disk.is_locked() {
                Span::styled(glyphs.locked, theme::danger())
            } else if is_sel {
                Span::styled(glyphs.sel_on, theme::danger())
            } else {
                Span::styled(glyphs.sel_off, theme::muted())
            };
            let pointer = if is_cursor {
                Span::styled(
                    format!("{} ", glyphs.pointer),
                    Style::new().fg(theme::ACCENT),
                )
            } else {
                Span::raw("  ")
            };
            let state = match disk.lock {
                Some(reason) => Span::styled(reason.label().to_string(), theme::danger()),
                None if is_sel => Span::styled("WILL ERASE".to_string(), theme::danger()),
                None => Span::styled("ready".to_string(), theme::muted()),
            };
            let model: String = disk.model.chars().take(28).collect();
            let body = Span::styled(
                format!(
                    "{:<11} {:<28} {:>9} {:<6} {:<5} ",
                    disk.name,
                    model,
                    disk.size_human(),
                    disk.bus.label(),
                    disk.kind.label(),
                ),
                if is_cursor {
                    Style::new().fg(theme::BRIGHT).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(theme::TEXT)
                },
            );
            lines.push(Line::from(vec![pointer, mark, Span::raw(" "), body, state]));
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    draw_scheme_panel(f, app, glyphs, cols[1]);
}

fn draw_scheme_panel(f: &mut Frame, app: &App, _glyphs: &Glyphs, area: Rect) {
    let scheme = app.current_scheme();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(_glyphs.border)
        .border_style(theme::muted())
        .title(Span::styled(" Method ", theme::title()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rec = if scheme.recommended {
        Span::styled("  ★ recommended", theme::ok())
    } else {
        Span::raw("")
    };
    let spec = app.spec();
    let total_passes = spec.pass_list().len();

    let mut lines = vec![
        Line::from(vec![
            Span::styled("◀ ", theme::muted()),
            Span::styled(
                scheme.name,
                Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ▶   ", theme::muted()),
            Span::styled(format!("{} pass(es)", scheme.pass_count()), theme::muted()),
            rec,
        ]),
        Line::from(Span::styled(scheme.description, theme::muted())).alignment(Alignment::Left),
        Line::from(vec![
            Span::styled("verify ", theme::muted()),
            Span::styled(
                format!("[{}]", app.verify.label()),
                Style::new().fg(theme::ACCENT),
            ),
            Span::styled("   rounds ", theme::muted()),
            Span::styled(format!("[{}]", app.rounds), Style::new().fg(theme::ACCENT)),
            Span::styled("   final blank ", theme::muted()),
            Span::styled(
                format!("[{}]", if app.final_blank { "on" } else { "off" }),
                Style::new().fg(theme::ACCENT),
            ),
            Span::styled(
                format!("   → {total_passes} total write pass(es)"),
                theme::muted(),
            ),
        ]),
    ];

    // SSD advisory when any selected disk is flash.
    if app
        .selected_disks()
        .iter()
        .any(|d| d.kind == MediaKind::Ssd)
    {
        lines.push(Line::from(Span::styled(
            "⚠ SSD selected: overwrites cannot reach over-provisioned cells. \
             For flash, firmware Secure Erase / crypto-erase is the NIST-purge method.",
            theme::warn(),
        )));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::danger())
        .title(Span::styled(" ⚠  POINT OF NO RETURN ", theme::danger()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(gate) = &app.gate else { return };
    let mut lines = vec![
        Line::from(Span::styled(
            "The following disks will be PERMANENTLY and IRRECOVERABLY erased:",
            theme::warn(),
        )),
        Line::from(""),
    ];
    for d in app.selected_disks() {
        lines.push(Line::from(vec![
            Span::styled("    ✗ ", theme::danger()),
            Span::styled(
                format!("{:<11} {:<26} {:>9}", d.name, d.model, d.size_human()),
                Style::new().fg(theme::BRIGHT),
            ),
        ]));
    }
    lines.push(Line::from(""));
    let spec = app.spec();
    lines.push(Line::from(Span::styled(
        format!(
            "Method: {} · {} write pass(es) · verify {}",
            spec.scheme.name,
            spec.pass_list().len(),
            spec.verify.label()
        ),
        theme::muted(),
    )));
    lines.push(Line::from(""));

    if let Some(remaining) = gate.countdown_remaining_ms() {
        let secs = (remaining as f64 / 1000.0).ceil() as u64;
        lines.push(Line::from(vec![
            Span::styled("Starting in ", theme::warn()),
            Span::styled(
                format!("{secs}"),
                Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
            ),
            Span::styled("s — press ANY key to ABORT.", theme::warn()),
        ]));
    } else {
        let typed = gate.typed();
        let matched = gate.phrase_matches();
        lines.push(Line::from(vec![
            Span::styled("Type ", theme::muted()),
            Span::styled(
                format!("\"{}\"", gate.phrase()),
                Style::new().fg(theme::WARN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" then Enter to confirm, Esc to cancel.", theme::muted()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  > ", theme::muted()),
            Span::styled(
                typed.to_string(),
                if matched {
                    theme::ok()
                } else {
                    Style::new().fg(theme::BRIGHT)
                },
            ),
            Span::styled(if matched { "  ✓" } else { "" }, theme::ok()),
        ]));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_wiping(f: &mut Frame, app: &App, glyphs: &Glyphs, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(glyphs.border)
        .border_style(theme::muted())
        .title(Span::styled(" Erasing ", theme::title()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.jobs.is_empty() {
        return;
    }

    // Each job: a label line + a gauge. 2 rows; +1 spacer.
    let row_h = 3u16;
    let constraints: Vec<Constraint> = (0..app.jobs.len())
        .map(|_| Constraint::Length(row_h))
        .collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let spinner = glyphs.spinner[app.spinner_frame % glyphs.spinner.len()];

    for (i, job) in app.jobs.iter().enumerate() {
        if i >= rows.len() {
            break;
        }
        let snap = job.progress.snapshot();
        let tracker = &app.trackers[i];
        let is_cursor = i == app.cursor;

        let (phase_span, glyph) = match snap.phase {
            Phase::Done => (Span::styled("done", theme::ok()), "✓".to_string()),
            Phase::Failed => (
                Span::styled(snap.phase.label(), theme::danger()),
                "✗".to_string(),
            ),
            Phase::Cancelled => (Span::styled("cancelled", theme::warn()), "•".to_string()),
            _ => (
                Span::styled(snap.phase.label(), Style::new().fg(theme::ACCENT)),
                spinner.to_string(),
            ),
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

        let pointer = if is_cursor { glyphs.pointer } else { " " };
        let header = Line::from(vec![
            Span::styled(
                format!("{pointer} {glyph} "),
                Style::new().fg(theme::ACCENT),
            ),
            Span::styled(
                format!("{:<11} ", job.disk.name),
                Style::new().fg(theme::BRIGHT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("pass {}/{} ", snap.pass_index, snap.pass_count),
                theme::muted(),
            ),
            Span::styled(
                format!("{:<14} ", snap.pass_label),
                Style::new().fg(theme::ACCENT),
            ),
            phase_span,
            Span::styled(format!("   {speed_str}   ETA {eta_str}"), theme::muted()),
        ]);

        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(rows[i]);
        f.render_widget(Paragraph::new(header), sub[0]);

        let gauge_color = match snap.phase {
            Phase::Done => theme::OK,
            Phase::Failed => theme::DANGER,
            Phase::Cancelled => theme::WARN,
            _ => theme::ACCENT,
        };
        let label = match &snap.error {
            Some(err) => err.clone(),
            None => format!("{:.1}%", snap.ratio() * 100.0),
        };
        let gauge = Gauge::default()
            .gauge_style(Style::new().fg(gauge_color))
            .ratio(snap.ratio())
            .label(label);
        f.render_widget(gauge, sub[1]);
    }
}

fn draw_summary(f: &mut Frame, app: &App, area: Rect) {
    let Some(report) = &app.session_report else {
        return;
    };
    let all_ok = report.all_succeeded();
    let border = if all_ok { theme::ok() } else { theme::danger() };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Span::styled(
            if all_ok {
                " ✓ Complete "
            } else {
                " Completed with issues "
            },
            border,
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = Vec::new();
    for job in &report.jobs {
        let style = match job.status {
            scour_core::engine::JobStatus::Success => theme::ok(),
            scour_core::engine::JobStatus::Cancelled => theme::warn(),
            _ => theme::danger(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<11} ", job.disk_name),
                Style::new().fg(theme::BRIGHT),
            ),
            Span::styled(
                format!(
                    "{:<26} ",
                    job.disk_model.chars().take(26).collect::<String>()
                ),
                theme::muted(),
            ),
            Span::styled(format!("{:>10} ", job.status.label()), style),
            Span::styled(
                format!(
                    "{} · {}/{} passes · {} · {:.0} MB/s",
                    job.scheme_name,
                    job.passes_completed,
                    job.pass_count,
                    fmt_secs(job.duration_secs),
                    job.avg_write_mib_s * 1.048576, // MiB/s → MB/s
                ),
                theme::muted(),
            ),
        ]));
        if let Some(err) = &job.error {
            lines.push(Line::from(Span::styled(
                format!("      └ {err}"),
                theme::danger(),
            )));
        }
    }
    lines.push(Line::from(""));
    match &app.saved_report_path {
        Some(path) => lines.push(Line::from(Span::styled(
            format!("Report written: {path}"),
            theme::ok(),
        ))),
        None => lines.push(Line::from(Span::styled(
            "Press w to write a signed JSON erasure report.",
            theme::muted(),
        ))),
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    if let Some((msg, _)) = &app.status {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {msg}"), theme::warn()))),
            area,
        );
    }
}

fn draw_hints(f: &mut Frame, app: &App, area: Rect) {
    let pairs: Vec<(&str, &str)> = match app.screen {
        Screen::Disks => vec![
            ("↑↓", "move"),
            ("space", "select"),
            ("◀▶", "method"),
            ("v", "verify"),
            ("+/-", "rounds"),
            ("b", "blank"),
            ("r", "rescan"),
            ("s", "START"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Screen::Confirm => vec![("type phrase", ""), ("enter", "confirm"), ("esc", "cancel")],
        Screen::Wiping => vec![
            ("↑↓", "select"),
            ("c", "cancel job"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Screen::Summary => {
            let mut v = vec![("w", "write report"), ("n", "new session")];
            if app.pid1 {
                v.push(("r", "reboot"));
                v.push(("p", "power off"));
            } else {
                v.push(("q", "quit"));
            }
            v
        }
    };
    let mut spans = Vec::new();
    for (k, label) in pairs {
        spans.push(Span::styled(format!(" {k} "), theme::key_hint()));
        if !label.is_empty() {
            spans.push(Span::styled(format!(" {label}  "), theme::muted()));
        } else {
            spans.push(Span::raw("  "));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// -- modals ------------------------------------------------------------------

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

fn draw_help(f: &mut Frame, _app: &App, area: Rect) {
    let rect = centered(area, 64, 18);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::ACCENT))
        .title(Span::styled(" Help ", theme::title()));
    let text = vec![
        Line::from(Span::styled(
            "Scour — secure disk sanitization",
            theme::title(),
        )),
        Line::from(""),
        Line::from("Disks screen:"),
        Line::from("  ↑/↓ or j/k   move cursor"),
        Line::from("  space/enter  toggle disk for erasure"),
        Line::from("  ◀/▶ or [/]   change wipe method"),
        Line::from("  v            cycle verify mode (off/last/all)"),
        Line::from("  +/-          repeat the method N rounds"),
        Line::from("  b            toggle a final zero-blanking pass"),
        Line::from("  s            arm and start (requires typed phrase)"),
        Line::from(""),
        Line::from("Locked disks (mounted/swap/boot/in-use) can never be selected."),
        Line::from("Random passes are verified by regenerating their seed stream."),
        Line::from(""),
        Line::from(Span::styled("Press any key to close.", theme::muted())),
    ];
    f.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: true }),
        rect,
    );
}

fn draw_quit_modal(f: &mut Frame, app: &App, area: Rect) {
    let rect = centered(area, 56, 8);
    f.render_widget(Clear, rect);
    let busy = app.jobs_running();
    let mut lines = vec![Line::from("")];
    if busy {
        lines.push(Line::from(Span::styled(
            "Wipes are still running. Quitting cancels them.",
            theme::warn(),
        )));
    }
    lines.push(Line::from(""));
    if app.pid1 {
        lines.push(Line::from("  r  reboot      p  power off      n/esc  stay"));
    } else {
        lines.push(Line::from("  y  quit        n/esc  stay"));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::danger())
        .title(Span::styled(" Quit? ", theme::danger()));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Center),
        rect,
    );
}

fn draw_cancel_modal(f: &mut Frame, app: &App, area: Rect) {
    let rect = centered(area, 56, 8);
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
            theme::warn(),
        )),
        Line::from(Span::styled(
            "The disk will be left partially overwritten.",
            theme::muted(),
        )),
        Line::from(""),
        Line::from("  y  cancel job      n/esc  keep going"),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::warn())
        .title(Span::styled(" Cancel job ", theme::warn()));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Center),
        rect,
    );
}

// -- helpers -----------------------------------------------------------------

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
