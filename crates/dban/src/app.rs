//! Application state machine. Pure logic — no terminal I/O — so every
//! transition is unit-testable.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use dban_core::algorithm::{all_schemes, Scheme, VerifyMode};
use dban_core::device::{Disk, DiskProvider};
use dban_core::engine::{spawn_firmware, spawn_wipe, JobHandle, WipeSpec};
use dban_core::firmware::{self, FirmwareMethod, FirmwareSupport};
use dban_core::report::SessionReport;
use dban_core::safety::SafetyGate;
use dban_core::sysinfo::{self, SystemInfo};

/// The top-level screen currently shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Disk selection and method picker.
    Disks,
    /// The arming ceremony (phrase + countdown).
    Confirm,
    /// Live wipe progress.
    Wiping,
    /// Post-session results.
    Summary,
}

/// A choice in the method picker: a software overwrite scheme (by index into
/// [`App::schemes`]) or a firmware/drive-internal erase command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodChoice {
    /// A software overwrite scheme, by index into [`App::schemes`].
    Overwrite(usize),
    /// A firmware/drive-internal erase command.
    Firmware(FirmwareMethod),
}

impl MethodChoice {
    /// True when this choice is a firmware method.
    pub fn is_firmware(&self) -> bool {
        matches!(self, MethodChoice::Firmware(_))
    }
}

/// How the app should leave when it exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitAction {
    /// Return to the shell (hosted mode).
    Quit,
    /// Reboot the machine (appliance mode).
    Reboot,
    /// Power the machine off (appliance mode).
    PowerOff,
}

/// Rolling throughput window for one job.
pub struct SpeedTracker {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedTracker {
    const WINDOW: Duration = Duration::from_secs(5);

    /// Create an empty tracker.
    pub fn new() -> Self {
        SpeedTracker {
            samples: VecDeque::new(),
        }
    }

    /// Record a `(timestamp, work_done)` sample, trimming the rolling window.
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

    /// Current throughput estimate in bytes/sec over the rolling window.
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

    /// Estimated time remaining, or `None` when throughput is too low to judge.
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

/// The entire application state. Pure data + logic; rendering lives in
/// [`crate::ui`]. Most fields are public so tests and the renderer can read
/// them, but they are only mutated through the `on_key` / `on_tick` methods.
pub struct App {
    provider: Box<dyn DiskProvider>,
    /// True when running against simulated disks.
    pub simulation: bool,
    /// Running as init on the live ISO: quit becomes reboot/power-off.
    pub pid1: bool,
    /// Host hardware summary.
    pub sys: SystemInfo,

    /// The currently enumerated disks.
    pub disks: Vec<Disk>,
    /// Index of the highlighted row (disk list or job list).
    pub cursor: usize,
    /// Names of disks toggled on for erasure.
    pub selected: HashSet<String>,
    /// Firmware capability per disk name, refreshed with the disk list.
    pub supports: HashMap<String, FirmwareSupport>,

    /// All overwrite schemes (firmware methods are appended in `methods`).
    pub schemes: Vec<Scheme>,
    /// Overwrite schemes followed by every firmware method — the picker order.
    pub methods: Vec<MethodChoice>,
    /// Index of the selected method in `methods`.
    pub method_idx: usize,
    /// Verify mode (overwrite methods only).
    pub verify: VerifyMode,
    /// Number of scheme rounds (overwrite methods only).
    pub rounds: u32,
    /// Whether to append a final zero-blanking pass (overwrite only).
    pub final_blank: bool,

    /// The active screen.
    pub screen: Screen,
    /// The arming gate while on the confirm screen.
    pub gate: Option<SafetyGate>,
    /// Running/finished job handles, one per target disk.
    pub jobs: Vec<JobHandle>,
    /// Throughput trackers paired with `jobs`.
    pub trackers: Vec<SpeedTracker>,
    /// The assembled session report once all jobs finish.
    pub session_report: Option<SessionReport>,
    /// Path of the written report, if saved.
    pub saved_report_path: Option<String>,
    /// Fingerprint of the Ed25519 key that signed the saved report.
    pub saved_report_fingerprint: Option<String>,

    /// Transient status message and when it was set.
    pub status: Option<(String, Instant)>,
    /// Whether the help overlay is open.
    pub show_help: bool,
    /// Whether the quit-confirmation modal is open.
    pub confirm_quit: bool,
    /// Index of the job pending a cancel confirmation, if any.
    pub confirm_cancel_job: Option<usize>,
    /// Set while waiting for cancelled jobs to wind down before exiting.
    pub quitting: bool,
    pending_exit: Option<ExitAction>,
    /// Set to request the event loop exit with the given action.
    pub exit: Option<ExitAction>,
    /// Animation frame counter for spinners/pulses.
    pub spinner_frame: usize,
}

impl App {
    /// Build the app from a disk provider, probing disks immediately. `pid1`
    /// selects appliance mode (quit becomes reboot/power-off).
    pub fn new(provider: Box<dyn DiskProvider>, pid1: bool) -> Self {
        let simulation = provider.is_simulation();
        let schemes = all_schemes();
        // Build the picker list: overwrite schemes first, then firmware methods.
        let mut methods: Vec<MethodChoice> =
            (0..schemes.len()).map(MethodChoice::Overwrite).collect();
        for fw in [
            FirmwareMethod::AtaSecureErase { enhanced: false },
            FirmwareMethod::AtaSecureErase { enhanced: true },
            FirmwareMethod::NvmeFormat { crypto: false },
            FirmwareMethod::NvmeFormat { crypto: true },
            FirmwareMethod::NvmeSanitize { crypto: false },
            FirmwareMethod::NvmeSanitize { crypto: true },
        ] {
            methods.push(MethodChoice::Firmware(fw));
        }
        // Default to the recommended overwrite scheme.
        let method_idx = schemes.iter().position(|s| s.recommended).unwrap_or(0);
        let verify = schemes[method_idx].default_verify;
        let mut app = App {
            provider,
            simulation,
            pid1,
            sys: sysinfo::collect(),
            disks: Vec::new(),
            cursor: 0,
            selected: HashSet::new(),
            supports: HashMap::new(),
            schemes,
            methods,
            method_idx,
            verify,
            rounds: 1,
            final_blank: false,
            screen: Screen::Disks,
            gate: None,
            jobs: Vec::new(),
            trackers: Vec::new(),
            session_report: None,
            saved_report_path: None,
            saved_report_fingerprint: None,
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

    /// Re-enumerate disks and re-probe firmware capability, preserving any
    /// still-present selections.
    pub fn refresh_disks(&mut self) {
        match self.provider.refresh() {
            Ok(disks) => {
                let present: HashSet<String> = disks.iter().map(|d| d.name.clone()).collect();
                self.selected.retain(|name| present.contains(name));
                // Probe firmware capability (non-destructive) for each disk.
                self.supports = disks
                    .iter()
                    .map(|d| (d.name.clone(), firmware::detect_support(d)))
                    .collect();
                self.disks = disks;
                if self.cursor >= self.disks.len() {
                    self.cursor = self.disks.len().saturating_sub(1);
                }
            }
            Err(e) => self.flash(format!("disk scan failed: {e}")),
        }
    }

    /// The currently selected method.
    pub fn current_method(&self) -> MethodChoice {
        self.methods[self.method_idx]
    }

    /// The overwrite scheme for the current method, or `None` if a firmware
    /// method is selected.
    pub fn current_scheme(&self) -> Option<&Scheme> {
        match self.current_method() {
            MethodChoice::Overwrite(i) => self.schemes.get(i),
            MethodChoice::Firmware(_) => None,
        }
    }

    /// The firmware method for the current selection, or `None` for overwrite.
    pub fn current_firmware(&self) -> Option<FirmwareMethod> {
        match self.current_method() {
            MethodChoice::Firmware(m) => Some(m),
            MethodChoice::Overwrite(_) => None,
        }
    }

    /// True when the selected method is a firmware erase.
    pub fn is_firmware(&self) -> bool {
        self.current_method().is_firmware()
    }

    /// Human-readable name of the current method.
    pub fn method_name(&self) -> &str {
        match self.current_method() {
            MethodChoice::Overwrite(i) => self.schemes[i].name,
            MethodChoice::Firmware(m) => m.name(),
        }
    }

    /// One-paragraph description of the current method.
    pub fn method_description(&self) -> &str {
        match self.current_method() {
            MethodChoice::Overwrite(i) => self.schemes[i].description,
            MethodChoice::Firmware(m) => m.description(),
        }
    }

    /// The overwrite `WipeSpec` for the current method, or `None` for firmware.
    pub fn spec(&self) -> Option<WipeSpec> {
        self.current_scheme().map(|scheme| WipeSpec {
            scheme: scheme.clone(),
            rounds: self.rounds,
            verify: self.verify,
            final_blank: self.final_blank,
        })
    }

    /// Does `disk` support the currently selected firmware method? Always true
    /// for overwrite methods (every disk can be overwritten).
    pub fn disk_supports_current(&self, disk: &Disk) -> bool {
        match self.current_firmware() {
            None => true,
            Some(method) => self
                .supports
                .get(&disk.name)
                .is_some_and(|s| s.supports(method)),
        }
    }

    /// All selected, unlocked disks.
    pub fn selected_disks(&self) -> Vec<&Disk> {
        self.disks
            .iter()
            .filter(|d| self.selected.contains(&d.name) && !d.is_locked())
            .collect()
    }

    /// The disks that will actually be erased by the current method: selected,
    /// unlocked, and (for firmware) capable of the chosen command.
    pub fn target_disks(&self) -> Vec<&Disk> {
        self.selected_disks()
            .into_iter()
            .filter(|d| self.disk_supports_current(d))
            .collect()
    }

    /// Show a transient status message in the status line.
    pub fn flash(&mut self, msg: String) {
        self.status = Some((msg, Instant::now()));
    }

    // -- input ---------------------------------------------------------------

    /// Handle a key press, dispatching to the active screen/modal.
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
                self.method_idx = (self.method_idx + self.methods.len() - 1) % self.methods.len();
                self.sync_verify_default();
            }
            KeyCode::Right | KeyCode::Char(']') => {
                self.method_idx = (self.method_idx + 1) % self.methods.len();
                self.sync_verify_default();
            }
            // The verify / rounds / blank options apply to overwrite only.
            KeyCode::Char('v') if !self.is_firmware() => self.verify = self.verify.cycle(),
            KeyCode::Char('+') | KeyCode::Char('=') if !self.is_firmware() => {
                self.rounds = (self.rounds + 1).min(9)
            }
            KeyCode::Char('-') if !self.is_firmware() => {
                self.rounds = self.rounds.saturating_sub(1).max(1)
            }
            KeyCode::Char('b') if !self.is_firmware() => self.final_blank = !self.final_blank,
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

    /// When the current method is an overwrite scheme, reset the verify mode to
    /// that scheme's default. No-op for firmware methods.
    fn sync_verify_default(&mut self) {
        if let Some(scheme) = self.current_scheme() {
            self.verify = scheme.default_verify;
        }
    }

    fn begin_confirmation(&mut self) {
        if self.selected_disks().is_empty() {
            self.flash("select at least one disk first (space toggles)".to_string());
            return;
        }
        // For firmware methods, only capable disks are targeted.
        let targets = self.target_disks().len();
        if targets == 0 {
            self.flash(format!(
                "no selected disk supports {} — pick another method",
                self.method_name()
            ));
            return;
        }
        if targets < self.selected_disks().len() {
            let skipped = self.selected_disks().len() - targets;
            self.flash(format!(
                "{skipped} selected disk(s) don't support {} and will be skipped",
                self.method_name()
            ));
        }
        match SafetyGate::new(targets) {
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
                self.saved_report_fingerprint = None;
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

    /// True while any worker thread is still running.
    pub fn jobs_running(&self) -> bool {
        self.jobs.iter().any(|j| !j.is_finished())
    }

    // -- tick ----------------------------------------------------------------

    /// Advance time: animations, the arming countdown, throughput sampling,
    /// completion detection, and deferred-quit handling. `elapsed` is the time
    /// since the previous tick.
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

    fn start_jobs(&mut self, token: dban_core::safety::ArmToken) {
        let method = self.current_method();
        let spec = self.spec();
        let targets: Vec<Disk> = self.target_disks().into_iter().cloned().collect();
        self.jobs.clear();
        self.trackers.clear();
        for disk in &targets {
            let result = match method {
                MethodChoice::Overwrite(_) => {
                    spawn_wipe(disk, spec.as_ref().expect("overwrite has a spec"), &token)
                }
                MethodChoice::Firmware(fw) => spawn_firmware(disk, fw, &token),
            };
            match result {
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
        match report.save_signed(&dir) {
            Ok(saved) => {
                let shown = saved.json_path.display().to_string();
                self.saved_report_path = Some(shown.clone());
                self.saved_report_fingerprint = Some(saved.key_fingerprint.clone());
                self.flash(format!(
                    "report + Ed25519 signature saved to {shown} (key {})",
                    saved.key_fingerprint
                ));
            }
            Err(e) => self.flash(format!("could not save report: {e}")),
        }
    }
}
