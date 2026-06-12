//! Application state machine. Pure logic — no terminal I/O — so every
//! transition is unit-testable.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use scour_core::algorithm::{all_schemes, Scheme, VerifyMode};
use scour_core::device::{Disk, DiskProvider};
use scour_core::engine::{spawn_wipe, JobHandle, WipeSpec};
use scour_core::report::SessionReport;
use scour_core::safety::SafetyGate;
use scour_core::sysinfo::{self, SystemInfo};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Disks,
    Confirm,
    Wiping,
    Summary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitAction {
    Quit,
    Reboot,
    PowerOff,
}

/// Rolling throughput window for one job.
pub struct SpeedTracker {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedTracker {
    const WINDOW: Duration = Duration::from_secs(5);

    pub fn new() -> Self {
        SpeedTracker {
            samples: VecDeque::new(),
        }
    }

    pub fn push(&mut self, now: Instant, work_done: u64) {
        self.samples.push_back((now, work_done));
        while let Some(&(t, _)) = self.samples.front() {
            if now.duration_since(t) > Self::WINDOW && self.samples.len() > 2 {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn bytes_per_sec(&self) -> f64 {
        let (Some(&(t0, b0)), Some(&(t1, b1))) = (self.samples.front(), self.samples.back()) else {
            return 0.0;
        };
        let dt = t1.duration_since(t0).as_secs_f64();
        if dt <= 0.0 {
            return 0.0;
        }
        (b1.saturating_sub(b0)) as f64 / dt
    }

    pub fn eta(&self, work_done: u64, work_total: u64) -> Option<Duration> {
        let bps = self.bytes_per_sec();
        if bps <= 1.0 {
            return None;
        }
        let remaining = work_total.saturating_sub(work_done) as f64;
        Some(Duration::from_secs_f64(remaining / bps))
    }
}

impl Default for SpeedTracker {
    fn default() -> Self {
        Self::new()
    }
}

pub struct App {
    provider: Box<dyn DiskProvider>,
    pub simulation: bool,
    /// Running as init on the live ISO: quit becomes reboot/power-off.
    pub pid1: bool,
    pub sys: SystemInfo,

    pub disks: Vec<Disk>,
    pub cursor: usize,
    pub selected: HashSet<String>,

    pub schemes: Vec<Scheme>,
    pub scheme_idx: usize,
    pub verify: VerifyMode,
    pub rounds: u32,
    pub final_blank: bool,

    pub screen: Screen,
    pub gate: Option<SafetyGate>,
    pub jobs: Vec<JobHandle>,
    pub trackers: Vec<SpeedTracker>,
    pub session_report: Option<SessionReport>,
    pub saved_report_path: Option<String>,

    pub status: Option<(String, Instant)>,
    pub show_help: bool,
    pub confirm_quit: bool,
    pub confirm_cancel_job: Option<usize>,
    /// Set while waiting for cancelled jobs to wind down before exiting.
    pub quitting: bool,
    pending_exit: Option<ExitAction>,
    pub exit: Option<ExitAction>,
    pub spinner_frame: usize,
}

impl App {
    pub fn new(provider: Box<dyn DiskProvider>, pid1: bool) -> Self {
        let simulation = provider.is_simulation();
        let schemes = all_schemes();
        let scheme_idx = schemes.iter().position(|s| s.recommended).unwrap_or(0);
        let verify = schemes[scheme_idx].default_verify;
        let mut app = App {
            provider,
            simulation,
            pid1,
            sys: sysinfo::collect(),
            disks: Vec::new(),
            cursor: 0,
            selected: HashSet::new(),
            schemes,
            scheme_idx,
            verify,
            rounds: 1,
            final_blank: false,
            screen: Screen::Disks,
            gate: None,
            jobs: Vec::new(),
            trackers: Vec::new(),
            session_report: None,
            saved_report_path: None,
            status: None,
            show_help: false,
            confirm_quit: false,
            confirm_cancel_job: None,
            quitting: false,
            pending_exit: None,
            exit: None,
            spinner_frame: 0,
        };
        app.refresh_disks();
        app
    }

    pub fn refresh_disks(&mut self) {
        match self.provider.refresh() {
            Ok(disks) => {
                let present: HashSet<String> = disks.iter().map(|d| d.name.clone()).collect();
                self.selected.retain(|name| present.contains(name));
                self.disks = disks;
                if self.cursor >= self.disks.len() {
                    self.cursor = self.disks.len().saturating_sub(1);
                }
            }
            Err(e) => self.flash(format!("disk scan failed: {e}")),
        }
    }

    pub fn current_scheme(&self) -> &Scheme {
        &self.schemes[self.scheme_idx]
    }

    pub fn spec(&self) -> WipeSpec {
        WipeSpec {
            scheme: self.current_scheme().clone(),
            rounds: self.rounds,
            verify: self.verify,
            final_blank: self.final_blank,
        }
    }

    pub fn selected_disks(&self) -> Vec<&Disk> {
        self.disks
            .iter()
            .filter(|d| self.selected.contains(&d.name))
            .collect()
    }

    pub fn flash(&mut self, msg: String) {
        self.status = Some((msg, Instant::now()));
    }

    // -- input ---------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        // Ctrl+C is always a quit request, never a hard kill mid-wipe.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.request_quit();
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.confirm_quit {
            self.on_key_quit_modal(key.code);
            return;
        }
        if self.confirm_cancel_job.is_some() {
            self.on_key_cancel_modal(key.code);
            return;
        }
        match self.screen {
            Screen::Disks => self.on_key_disks(key.code),
            Screen::Confirm => self.on_key_confirm(key.code),
            Screen::Wiping => self.on_key_wiping(key.code),
            Screen::Summary => self.on_key_summary(key.code),
        }
    }

    fn on_key_disks(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.disks.is_empty() {
                    self.cursor = (self.cursor + 1).min(self.disks.len() - 1);
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle_current(),
            KeyCode::Left | KeyCode::Char('[') => {
                self.scheme_idx = (self.scheme_idx + self.schemes.len() - 1) % self.schemes.len();
                self.verify = self.current_scheme().default_verify;
            }
            KeyCode::Right | KeyCode::Char(']') => {
                self.scheme_idx = (self.scheme_idx + 1) % self.schemes.len();
                self.verify = self.current_scheme().default_verify;
            }
            KeyCode::Char('v') => self.verify = self.verify.cycle(),
            KeyCode::Char('+') | KeyCode::Char('=') => self.rounds = (self.rounds + 1).min(9),
            KeyCode::Char('-') => self.rounds = self.rounds.saturating_sub(1).max(1),
            KeyCode::Char('b') => self.final_blank = !self.final_blank,
            KeyCode::Char('r') => {
                self.refresh_disks();
                self.flash("disk list rescanned".to_string());
            }
            KeyCode::Char('s') => self.begin_confirmation(),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('q') | KeyCode::Esc => self.request_quit(),
            _ => {}
        }
    }

    fn toggle_current(&mut self) {
        let Some(disk) = self.disks.get(self.cursor) else {
            return;
        };
        if let Some(lock) = disk.lock {
            self.flash(format!(
                "{} is locked ({}) — it cannot be wiped in this session",
                disk.name,
                lock.label()
            ));
            return;
        }
        let name = disk.name.clone();
        if !self.selected.remove(&name) {
            self.selected.insert(name);
        }
    }

    fn begin_confirmation(&mut self) {
        let count = self.selected.len();
        match SafetyGate::new(count) {
            Ok(gate) => {
                self.gate = Some(gate);
                self.screen = Screen::Confirm;
            }
            Err(_) => self.flash("select at least one disk first (space toggles)".to_string()),
        }
    }

    fn on_key_confirm(&mut self, code: KeyCode) {
        let Some(gate) = self.gate.as_mut() else {
            self.screen = Screen::Disks;
            return;
        };
        // During the countdown ANY key aborts.
        if gate.countdown_remaining_ms().is_some() {
            gate.abort();
            self.gate = None;
            self.screen = Screen::Disks;
            self.flash("arming aborted".to_string());
            return;
        }
        match code {
            KeyCode::Esc => {
                gate.abort();
                self.gate = None;
                self.screen = Screen::Disks;
            }
            KeyCode::Backspace => gate.backspace(),
            KeyCode::Enter => {
                if gate.confirm().is_err() {
                    self.flash("phrase does not match — type it exactly".to_string());
                }
            }
            KeyCode::Char(c) => gate.input_char(c),
            _ => {}
        }
    }

    fn on_key_wiping(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.jobs.is_empty() {
                    self.cursor = (self.cursor + 1).min(self.jobs.len() - 1);
                }
            }
            KeyCode::Char('c') => {
                if let Some(job) = self.jobs.get(self.cursor) {
                    if !job.progress.snapshot().phase.is_terminal() {
                        self.confirm_cancel_job = Some(self.cursor);
                    }
                }
            }
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('q') | KeyCode::Esc => self.request_quit(),
            _ => {}
        }
    }

    fn on_key_summary(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('w') => self.save_report(),
            KeyCode::Char('n') => {
                self.jobs.clear();
                self.trackers.clear();
                self.selected.clear();
                self.session_report = None;
                self.saved_report_path = None;
                self.cursor = 0;
                self.refresh_disks();
                self.screen = Screen::Disks;
            }
            KeyCode::Char('q') | KeyCode::Esc => self.request_quit(),
            KeyCode::Char('p') if self.pid1 => self.exit = Some(ExitAction::PowerOff),
            KeyCode::Char('r') if self.pid1 => self.exit = Some(ExitAction::Reboot),
            _ => {}
        }
    }

    fn on_key_quit_modal(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') if !self.pid1 => self.execute_quit(ExitAction::Quit),
            KeyCode::Char('r') if self.pid1 => self.execute_quit(ExitAction::Reboot),
            KeyCode::Char('p') if self.pid1 => self.execute_quit(ExitAction::PowerOff),
            KeyCode::Esc | KeyCode::Char('n') => self.confirm_quit = false,
            _ => {}
        }
    }

    fn on_key_cancel_modal(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') => {
                if let Some(idx) = self.confirm_cancel_job.take() {
                    if let Some(job) = self.jobs.get(idx) {
                        job.request_cancel();
                        let name = job.disk.name.clone();
                        self.flash(format!("cancelling wipe of {name}"));
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => self.confirm_cancel_job = None,
            _ => {}
        }
    }

    fn request_quit(&mut self) {
        let busy = self.jobs_running();
        if busy || self.pid1 {
            self.confirm_quit = true;
        } else {
            self.exit = Some(ExitAction::Quit);
        }
    }

    fn execute_quit(&mut self, action: ExitAction) {
        self.confirm_quit = false;
        if self.jobs_running() {
            for job in &self.jobs {
                job.request_cancel();
            }
            self.quitting = true;
            // exit is set by on_tick once every worker has wound down.
            self.pending_exit = Some(action);
        } else {
            self.exit = Some(action);
        }
    }

    pub fn jobs_running(&self) -> bool {
        self.jobs.iter().any(|j| !j.is_finished())
    }

    // -- tick ----------------------------------------------------------------

    pub fn on_tick(&mut self, elapsed: Duration) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);

        // Expire transient status messages.
        if let Some((_, at)) = &self.status {
            if at.elapsed() > Duration::from_secs(4) {
                self.status = None;
            }
        }

        // Drive the arming countdown.
        if self.screen == Screen::Confirm {
            if let Some(gate) = self.gate.as_mut() {
                if let Some(token) = gate.tick(elapsed.as_millis() as u64) {
                    self.gate = None;
                    self.start_jobs(token);
                }
            }
        }

        // Sample throughput + detect completion.
        if self.screen == Screen::Wiping {
            let now = Instant::now();
            for (job, tracker) in self.jobs.iter().zip(self.trackers.iter_mut()) {
                let snap = job.progress.snapshot();
                if !snap.phase.is_terminal() {
                    tracker.push(now, snap.work_done);
                }
            }
            let all_done =
                !self.jobs.is_empty() && self.jobs.iter_mut().all(|j| j.report().is_some());
            if all_done && !self.quitting {
                let jobs: Vec<_> = self
                    .jobs
                    .iter_mut()
                    .filter_map(|j| j.report().cloned())
                    .collect();
                self.session_report =
                    Some(SessionReport::new(self.sys.clone(), self.simulation, jobs));
                self.cursor = 0;
                self.screen = Screen::Summary;
            }
        }

        // Finish a deferred quit once workers have wound down.
        if self.quitting && !self.jobs_running() {
            if let Some(action) = self.pending_exit.take() {
                self.exit = Some(action);
            } else {
                self.exit = Some(ExitAction::Quit);
            }
        }
    }

    fn start_jobs(&mut self, token: scour_core::safety::ArmToken) {
        let spec = self.spec();
        let targets: Vec<Disk> = self
            .disks
            .iter()
            .filter(|d| self.selected.contains(&d.name))
            .cloned()
            .collect();
        self.jobs.clear();
        self.trackers.clear();
        for disk in &targets {
            match spawn_wipe(disk, &spec, &token) {
                Ok(handle) => {
                    self.jobs.push(handle);
                    self.trackers.push(SpeedTracker::new());
                }
                Err(e) => self.flash(format!("failed to start {}: {e}", disk.name)),
            }
        }
        if self.jobs.is_empty() {
            self.screen = Screen::Disks;
            self.flash("no jobs could be started".to_string());
        } else {
            self.cursor = 0;
            self.screen = Screen::Wiping;
        }
    }

    fn save_report(&mut self) {
        let Some(report) = &self.session_report else {
            return;
        };
        // Live appliance: /tmp is a tmpfs the operator can copy from (or the
        // report can be photographed/transcribed). Hosted runs: cwd.
        let dir = if self.pid1 {
            std::path::PathBuf::from("/tmp")
        } else {
            std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir())
        };
        match report.save_json(&dir) {
            Ok(path) => {
                let shown = path.display().to_string();
                self.saved_report_path = Some(shown.clone());
                self.flash(format!("report saved to {shown}"));
            }
            Err(e) => self.flash(format!("could not save report: {e}")),
        }
    }
}
