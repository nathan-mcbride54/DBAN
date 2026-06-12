//! The wipe engine.
//!
//! Design notes:
//! * **One worker thread per disk.** Disks are independent spindles/controllers;
//!   wiping them concurrently is the multi-threading that actually pays off.
//!   Within one disk, I/O is strictly sequential — interleaving writes on a
//!   single device only causes seek thrash. PRNG generation (xoshiro256++,
//!   multiple GB/s) is never the bottleneck.
//! * **O_DIRECT when possible** (Linux block devices): bypasses the page cache
//!   so a verify pass reads the platters/cells, not RAM. Falls back to
//!   buffered I/O transparently for demo files and filesystems that refuse it.
//! * **Verifiable random passes.** Each random pass records its 64-bit seed;
//!   verification regenerates the identical stream instead of trusting cache.
//! * **No unattended start.** [`spawn_wipe`] demands an [`ArmToken`], which
//!   only the [`crate::safety::SafetyGate`] ceremony can mint.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::algorithm::{Pass, PassKind, Scheme, VerifyMode};
use crate::buffer::AlignedBuf;
use crate::device::Disk;
use crate::firmware::{self, FirmwareMethod};
use crate::prng::Prng;
use crate::safety::ArmToken;
use crate::CoreError;

/// I/O chunk size. 4 MiB keeps queue depth healthy on spinning rust and NVMe
/// alike without hogging RAM on small live systems.
pub const CHUNK_SIZE: usize = 4 << 20;

/// Everything the operator chose for this session.
#[derive(Clone)]
pub struct WipeSpec {
    /// The overwrite scheme to apply.
    pub scheme: Scheme,
    /// Repeat the whole scheme N times (the classic "rounds" feature).
    pub rounds: u32,
    /// Read-back verification policy.
    pub verify: VerifyMode,
    /// Append one final zero pass so the disk reads blank afterwards
    /// (useful after random-heavy schemes).
    pub final_blank: bool,
}

impl WipeSpec {
    /// The concrete pass sequence for one disk.
    pub fn pass_list(&self) -> Vec<Pass> {
        let mut passes = Vec::new();
        for _ in 0..self.rounds.max(1) {
            passes.extend(self.scheme.passes.iter().copied());
        }
        if self.final_blank {
            passes.push(Pass::zeros());
        }
        passes
    }

    /// Which passes get a read-back verification.
    pub fn verify_flags(&self, pass_count: usize) -> Vec<bool> {
        match self.verify {
            VerifyMode::None => vec![false; pass_count],
            VerifyMode::AllPasses => vec![true; pass_count],
            VerifyMode::LastPass => {
                let mut flags = vec![false; pass_count];
                if let Some(last) = flags.last_mut() {
                    *last = true;
                }
                flags
            }
        }
    }

    /// Total bytes of I/O (writes + verifying reads) for a disk of `size` bytes.
    /// Drives both the progress denominator and the UI estimate.
    pub fn work_bytes(&self, size: u64) -> u64 {
        let passes = self.pass_list();
        let verified = self
            .verify_flags(passes.len())
            .iter()
            .filter(|&&v| v)
            .count() as u64;
        size * (passes.len() as u64 + verified)
    }
}

/// The live phase of a running job, surfaced to the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[repr(u8)]
pub enum Phase {
    /// Not yet started.
    Queued = 0,
    /// Writing a pass (or issuing a firmware command).
    Writing = 1,
    /// Reading a pass back for verification.
    Verifying = 2,
    /// Finished successfully.
    Done = 3,
    /// Finished with an error.
    Failed = 4,
    /// Stopped at the operator's request.
    Cancelled = 5,
}

impl Phase {
    fn from_u8(v: u8) -> Phase {
        match v {
            1 => Phase::Writing,
            2 => Phase::Verifying,
            3 => Phase::Done,
            4 => Phase::Failed,
            5 => Phase::Cancelled,
            _ => Phase::Queued,
        }
    }

    /// Short display label for the phase.
    pub fn label(&self) -> &'static str {
        match self {
            Phase::Queued => "queued",
            Phase::Writing => "writing",
            Phase::Verifying => "verifying",
            Phase::Done => "done",
            Phase::Failed => "FAILED",
            Phase::Cancelled => "cancelled",
        }
    }

    /// True once the job has finished, one way or another.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Phase::Done | Phase::Failed | Phase::Cancelled)
    }
}

/// Lock-free progress shared between a worker and the UI.
pub struct Progress {
    work_done: AtomicU64,
    work_total: AtomicU64,
    pass_index: AtomicU32,
    pass_count: AtomicU32,
    phase: AtomicU8,
    /// True when the underlying operation reports no byte progress (firmware
    /// erase on real hardware): the UI should show a pulse, not a percentage.
    indeterminate: AtomicBool,
    pass_label: Mutex<String>,
    error: Mutex<Option<String>>,
}

impl Progress {
    fn new(work_total: u64, pass_count: u32) -> Self {
        Progress {
            work_done: AtomicU64::new(0),
            work_total: AtomicU64::new(work_total.max(1)),
            pass_index: AtomicU32::new(0),
            pass_count: AtomicU32::new(pass_count),
            phase: AtomicU8::new(Phase::Queued as u8),
            indeterminate: AtomicBool::new(false),
            pass_label: Mutex::new(String::new()),
            error: Mutex::new(None),
        }
    }

    fn set_work_done(&self, bytes: u64) {
        self.work_done.store(bytes, Ordering::Relaxed);
    }

    fn set_indeterminate(&self, value: bool) {
        self.indeterminate.store(value, Ordering::Relaxed);
    }

    fn add_work(&self, bytes: u64) {
        self.work_done.fetch_add(bytes, Ordering::Relaxed);
    }

    fn set_pass(&self, index: u32, label: String) {
        self.pass_index.store(index, Ordering::Relaxed);
        *self.pass_label.lock().unwrap() = label;
    }

    fn set_phase(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }

    fn set_error(&self, msg: String) {
        *self.error.lock().unwrap() = Some(msg);
    }

    /// Take a consistent point-in-time copy of the progress for rendering.
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            work_done: self.work_done.load(Ordering::Relaxed),
            work_total: self.work_total.load(Ordering::Relaxed),
            pass_index: self.pass_index.load(Ordering::Relaxed),
            pass_count: self.pass_count.load(Ordering::Relaxed),
            phase: Phase::from_u8(self.phase.load(Ordering::Relaxed)),
            indeterminate: self.indeterminate.load(Ordering::Relaxed),
            pass_label: self.pass_label.lock().unwrap().clone(),
            error: self.error.lock().unwrap().clone(),
        }
    }
}

/// An immutable copy of a job's [`Progress`] at one instant.
#[derive(Clone, Debug)]
pub struct ProgressSnapshot {
    /// Bytes of work completed so far (writes + verifying reads).
    pub work_done: u64,
    /// Total bytes of work the job will perform.
    pub work_total: u64,
    /// 1-based index of the pass currently in flight.
    pub pass_index: u32,
    /// Total number of passes for the job.
    pub pass_count: u32,
    /// Current phase.
    pub phase: Phase,
    /// True when there is no meaningful byte progress (firmware erase on real
    /// hardware); the UI shows a pulse instead of a percentage.
    pub indeterminate: bool,
    /// Label of the current pass (e.g. `"zeros (0x00)"`).
    pub pass_label: String,
    /// Error message if the job failed.
    pub error: Option<String>,
}

impl ProgressSnapshot {
    /// 0.0..=1.0, safe against a zero denominator and float overshoot.
    pub fn ratio(&self) -> f64 {
        (self.work_done as f64 / self.work_total as f64).clamp(0.0, 1.0)
    }
}

/// The terminal outcome of a job, recorded in the erasure report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum JobStatus {
    /// All passes (and verification) completed successfully.
    Success,
    /// A verification read-back did not match what was written.
    VerifyFailed,
    /// An I/O or device error occurred.
    Failed,
    /// The operator cancelled the job.
    Cancelled,
}

impl JobStatus {
    /// Short upper-case display label, e.g. `"SUCCESS"`.
    pub fn label(&self) -> &'static str {
        match self {
            JobStatus::Success => "SUCCESS",
            JobStatus::VerifyFailed => "VERIFY FAILED",
            JobStatus::Failed => "FAILED",
            JobStatus::Cancelled => "CANCELLED",
        }
    }
}

/// The permanent record of one disk's sanitization, embedded in erasure
/// reports. Covers both overwrite jobs and firmware-erase jobs; the
/// overwrite-specific fields (`rounds`, `pass_count`, byte counters) are zero
/// for firmware jobs, distinguished by `firmware`.
#[derive(Clone, Debug, Serialize)]
pub struct JobReport {
    /// Kernel/demo name of the disk.
    pub disk_name: String,
    /// Device model string.
    pub disk_model: String,
    /// Device serial number.
    pub disk_serial: String,
    /// Capacity in bytes.
    pub disk_size_bytes: u64,
    /// Stable id of the method used (scheme id or firmware-method id).
    pub method_id: String,
    /// Human-readable method name.
    pub method_name: String,
    /// True when this was a firmware/drive-internal erase rather than overwrite.
    pub firmware: bool,
    /// Number of scheme rounds (overwrite only; 0 for firmware).
    pub rounds: u32,
    /// Verification policy used (overwrite only).
    pub verify: VerifyMode,
    /// Total passes planned (overwrite only).
    pub pass_count: u32,
    /// Passes actually completed.
    pub passes_completed: u32,
    /// Final outcome.
    pub status: JobStatus,
    /// Error message if the job did not succeed.
    pub error: Option<String>,
    /// Unix start time (seconds).
    pub started_unix: u64,
    /// Unix finish time (seconds).
    pub finished_unix: u64,
    /// Wall-clock duration in seconds.
    pub duration_secs: f64,
    /// Total bytes written (overwrite only).
    pub bytes_written: u64,
    /// Total bytes read back for verification (overwrite only).
    pub bytes_verified: u64,
    /// Average write throughput in MiB/s (overwrite only).
    pub avg_write_mib_s: f64,
}

impl JobReport {
    /// Minimal report skeleton for `disk`, used for panics and as the base for
    /// both job kinds.
    fn skeleton(disk: &Disk, method_id: String, method_name: String, firmware: bool) -> Self {
        JobReport {
            disk_name: disk.name.clone(),
            disk_model: disk.model.clone(),
            disk_serial: disk.serial.clone(),
            disk_size_bytes: disk.size_bytes,
            method_id,
            method_name,
            firmware,
            rounds: 0,
            verify: VerifyMode::None,
            pass_count: 0,
            passes_completed: 0,
            status: JobStatus::Failed,
            error: None,
            started_unix: 0,
            finished_unix: 0,
            duration_secs: 0.0,
            bytes_written: 0,
            bytes_verified: 0,
            avg_write_mib_s: 0.0,
        }
    }
}

/// A running (or finished) sanitization job for one disk.
pub struct JobHandle {
    /// The disk being sanitized.
    pub disk: Disk,
    /// Shared progress the UI polls each frame.
    pub progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<JobReport>>,
    report: Option<JobReport>,
}

impl JobHandle {
    /// Ask the worker to stop at the next chunk boundary.
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// True if cancellation has been requested.
    pub fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// True once the worker thread has exited (successfully or not).
    pub fn is_finished(&self) -> bool {
        self.report.is_some() || self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Non-blocking: returns the report once the worker has exited.
    pub fn report(&mut self) -> Option<&JobReport> {
        if self.report.is_none() {
            let done = self.thread.as_ref().is_some_and(|t| t.is_finished());
            if done {
                if let Some(handle) = self.thread.take() {
                    self.report = Some(handle.join().unwrap_or_else(|_| {
                        let mut r =
                            JobReport::skeleton(&self.disk, String::new(), String::new(), false);
                        r.error = Some("worker thread panicked".to_string());
                        r
                    }));
                }
            }
        }
        self.report.as_ref()
    }
}

/// Start wiping one disk. The `ArmToken` parameter is the safety interlock:
/// it cannot be forged outside `dban_core::safety`.
pub fn spawn_wipe(disk: &Disk, spec: &WipeSpec, _token: &ArmToken) -> Result<JobHandle, CoreError> {
    if let Some(lock) = disk.lock {
        return Err(CoreError::DiskLocked(
            disk.name.clone(),
            lock.label().to_string(),
        ));
    }
    let passes = spec.pass_list();
    let progress = Arc::new(Progress::new(
        spec.work_bytes(disk.size_bytes),
        passes.len() as u32,
    ));
    let cancel = Arc::new(AtomicBool::new(false));

    let worker_disk = disk.clone();
    let worker_spec = spec.clone();
    let worker_progress = Arc::clone(&progress);
    let worker_cancel = Arc::clone(&cancel);

    let thread = thread::Builder::new()
        .name(format!("wipe-{}", disk.name))
        .spawn(move || run_job(worker_disk, worker_spec, worker_progress, worker_cancel))
        .map_err(CoreError::Io)?;

    Ok(JobHandle {
        disk: disk.clone(),
        progress,
        cancel,
        thread: Some(thread),
        report: None,
    })
}

/// Start a firmware/drive-internal erase of one disk (ATA Secure Erase, NVMe
/// Format / Sanitize). Like [`spawn_wipe`], requires the safety [`ArmToken`].
/// On real hardware the command reports no progress, so the job is marked
/// indeterminate; demo disks animate a determinate bar.
pub fn spawn_firmware(
    disk: &Disk,
    method: FirmwareMethod,
    _token: &ArmToken,
) -> Result<JobHandle, CoreError> {
    if let Some(lock) = disk.lock {
        return Err(CoreError::DiskLocked(
            disk.name.clone(),
            lock.label().to_string(),
        ));
    }
    let progress = Arc::new(Progress::new(disk.size_bytes.max(1), 1));
    // Real hardware gives no byte progress; simulated disks animate a bar.
    progress.set_indeterminate(!disk.simulated);
    let cancel = Arc::new(AtomicBool::new(false));

    let worker_disk = disk.clone();
    let worker_progress = Arc::clone(&progress);
    let worker_cancel = Arc::clone(&cancel);

    let thread = thread::Builder::new()
        .name(format!("fw-{}", disk.name))
        .spawn(move || run_firmware_job(worker_disk, method, worker_progress, worker_cancel))
        .map_err(CoreError::Io)?;

    Ok(JobHandle {
        disk: disk.clone(),
        progress,
        cancel,
        thread: Some(thread),
        report: None,
    })
}

fn run_firmware_job(
    disk: Disk,
    method: FirmwareMethod,
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
) -> JobReport {
    let started_unix = unix_now();
    let t0 = Instant::now();
    progress.set_pass(1, method.name().to_string());
    progress.set_phase(Phase::Writing);

    let size = disk.size_bytes;
    let result = firmware::execute(&disk, method, &cancel, |frac| {
        progress.set_work_done((frac * size as f64) as u64);
    });

    let (status, error) = match result {
        Ok(()) => (JobStatus::Success, None),
        Err(firmware::FirmwareError::Cancelled) => (JobStatus::Cancelled, None),
        Err(e) => (JobStatus::Failed, Some(e.to_string())),
    };

    let phase = match status {
        JobStatus::Success => {
            progress.set_work_done(size);
            Phase::Done
        }
        JobStatus::Cancelled => Phase::Cancelled,
        _ => Phase::Failed,
    };
    if let Some(msg) = &error {
        progress.set_error(msg.clone());
    }
    progress.set_phase(phase);

    let mut report = JobReport::skeleton(
        &disk,
        method.id().to_string(),
        method.name().to_string(),
        true,
    );
    report.pass_count = 1;
    report.passes_completed = if status == JobStatus::Success { 1 } else { 0 };
    report.status = status;
    report.error = error;
    report.started_unix = started_unix;
    report.finished_unix = unix_now();
    report.duration_secs = t0.elapsed().as_secs_f64();
    report.bytes_written = if status == JobStatus::Success {
        size
    } else {
        0
    };
    report
}

// ---------------------------------------------------------------------------
// Worker internals
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum JobError {
    Cancelled,
    Io(String),
    Verify { pass: u32, offset: u64 },
}

impl From<io::Error> for JobError {
    fn from(e: io::Error) -> Self {
        JobError::Io(e.to_string())
    }
}

#[derive(Default)]
struct Stats {
    bytes_written: u64,
    bytes_verified: u64,
    write_time: Duration,
    passes_completed: u32,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn run_job(
    disk: Disk,
    spec: WipeSpec,
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
) -> JobReport {
    let started_unix = unix_now();
    let t0 = Instant::now();
    let mut stats = Stats::default();
    let passes = spec.pass_list();

    let (status, error) = match execute(&disk, &spec, &passes, &progress, &cancel, &mut stats) {
        Ok(()) => (JobStatus::Success, None),
        Err(JobError::Cancelled) => (JobStatus::Cancelled, None),
        Err(JobError::Io(msg)) => (JobStatus::Failed, Some(format!("I/O error: {msg}"))),
        Err(JobError::Verify { pass, offset }) => (
            JobStatus::VerifyFailed,
            Some(format!(
                "verification mismatch in pass {pass} at byte offset {offset}"
            )),
        ),
    };

    let phase = match status {
        JobStatus::Success => Phase::Done,
        JobStatus::Cancelled => Phase::Cancelled,
        _ => Phase::Failed,
    };
    if let Some(msg) = &error {
        progress.set_error(msg.clone());
    }
    progress.set_phase(phase);

    let duration = t0.elapsed();
    let write_secs = stats.write_time.as_secs_f64();
    let mut report = JobReport::skeleton(
        &disk,
        spec.scheme.id.to_string(),
        spec.scheme.name.to_string(),
        false,
    );
    report.rounds = spec.rounds;
    report.verify = spec.verify;
    report.pass_count = passes.len() as u32;
    report.passes_completed = stats.passes_completed;
    report.status = status;
    report.error = error;
    report.started_unix = started_unix;
    report.finished_unix = unix_now();
    report.duration_secs = duration.as_secs_f64();
    report.bytes_written = stats.bytes_written;
    report.bytes_verified = stats.bytes_verified;
    report.avg_write_mib_s = if write_secs > 0.0 {
        stats.bytes_written as f64 / (1024.0 * 1024.0) / write_secs
    } else {
        0.0
    };
    report
}

fn execute(
    disk: &Disk,
    spec: &WipeSpec,
    passes: &[Pass],
    progress: &Progress,
    cancel: &AtomicBool,
    stats: &mut Stats,
) -> Result<(), JobError> {
    let size = disk.size_bytes;
    let verify_flags = spec.verify_flags(passes.len());
    let mut target = Target::open(&disk.path, size)?;
    let mut buf = AlignedBuf::zeroed(CHUNK_SIZE);
    let mut expected = AlignedBuf::zeroed(CHUNK_SIZE);

    for (i, pass) in passes.iter().enumerate() {
        let pass_no = (i + 1) as u32;
        progress.set_pass(pass_no, pass.label());
        progress.set_phase(Phase::Writing);

        let wt0 = Instant::now();
        let seed = write_pass(
            &mut target,
            size,
            pass,
            disk.throttle_bps,
            progress,
            cancel,
            &mut buf,
        )?;
        // Push every byte out of volatile caches before claiming the pass done.
        target.sync()?;
        stats.write_time += wt0.elapsed();
        stats.bytes_written += size;
        stats.passes_completed = pass_no;

        if verify_flags[i] {
            progress.set_phase(Phase::Verifying);
            verify_pass(
                &mut target,
                size,
                pass,
                seed,
                pass_no,
                disk.throttle_bps,
                progress,
                cancel,
                &mut buf,
                &mut expected,
            )?;
            stats.bytes_verified += size;
        }
    }
    Ok(())
}

/// Write one full pass. Returns the PRNG seed when the pass was random,
/// so verification can regenerate the stream.
fn write_pass(
    target: &mut Target,
    size: u64,
    pass: &Pass,
    throttle_bps: Option<u64>,
    progress: &Progress,
    cancel: &AtomicBool,
    buf: &mut AlignedBuf,
) -> Result<Option<u64>, JobError> {
    target.seek_start()?;
    let mut seed = None;
    let mut prng = match pass.kind {
        PassKind::Random => {
            let (p, s) = Prng::fresh();
            seed = Some(s);
            Some(p)
        }
        PassKind::Fill(_) => None,
    };
    // Single-byte fills (the overwhelmingly common case) fill the buffer once.
    if let PassKind::Fill(pattern) = pass.kind {
        if pattern.len() == 1 {
            buf.fill(pattern[0]);
        }
    }

    let t0 = Instant::now();
    let mut written: u64 = 0;
    while written < size {
        if cancel.load(Ordering::SeqCst) {
            return Err(JobError::Cancelled);
        }
        let chunk = (size - written).min(CHUNK_SIZE as u64) as usize;
        match pass.kind {
            PassKind::Random => prng.as_mut().unwrap().fill(&mut buf[..chunk]),
            PassKind::Fill(pattern) if pattern.len() > 1 => {
                fill_pattern_at(&mut buf[..chunk], pattern, written)
            }
            PassKind::Fill(_) => {}
        }
        target.write_chunk(&buf[..chunk])?;
        written += chunk as u64;
        progress.add_work(chunk as u64);
        if let Some(bps) = throttle_bps {
            throttle(t0, written, bps, cancel);
        }
    }
    Ok(seed)
}

/// Read the pass back and compare against what was (or should have been) written.
#[allow(clippy::too_many_arguments)]
fn verify_pass(
    target: &mut Target,
    size: u64,
    pass: &Pass,
    seed: Option<u64>,
    pass_no: u32,
    throttle_bps: Option<u64>,
    progress: &Progress,
    cancel: &AtomicBool,
    buf: &mut AlignedBuf,
    expected: &mut AlignedBuf,
) -> Result<(), JobError> {
    target.seek_start()?;
    let mut prng = match pass.kind {
        PassKind::Random => Some(Prng::from_seed(
            seed.expect("random pass verified without a recorded seed"),
        )),
        PassKind::Fill(_) => None,
    };
    if let PassKind::Fill(pattern) = pass.kind {
        if pattern.len() == 1 {
            expected.fill(pattern[0]);
        }
    }

    let t0 = Instant::now();
    let mut pos: u64 = 0;
    while pos < size {
        if cancel.load(Ordering::SeqCst) {
            return Err(JobError::Cancelled);
        }
        let chunk = (size - pos).min(CHUNK_SIZE as u64) as usize;
        match pass.kind {
            PassKind::Random => prng.as_mut().unwrap().fill(&mut expected[..chunk]),
            PassKind::Fill(pattern) if pattern.len() > 1 => {
                fill_pattern_at(&mut expected[..chunk], pattern, pos)
            }
            PassKind::Fill(_) => {}
        }
        target.read_chunk(&mut buf[..chunk])?;
        if buf[..chunk] != expected[..chunk] {
            let first_bad = buf[..chunk]
                .iter()
                .zip(expected[..chunk].iter())
                .position(|(a, b)| a != b)
                .unwrap_or(0) as u64;
            return Err(JobError::Verify {
                pass: pass_no,
                offset: pos + first_bad,
            });
        }
        pos += chunk as u64;
        progress.add_work(chunk as u64);
        if let Some(bps) = throttle_bps {
            throttle(t0, pos, bps, cancel);
        }
    }
    Ok(())
}

/// Fill `buf` with `pattern` as if the pattern repeats from absolute byte 0 of
/// the device and `buf` starts at absolute offset `abs_off`. Keeps multi-byte
/// patterns (e.g. Gutmann's 0x92 49 24) phase-continuous across chunks even
/// though the chunk size is not a multiple of the pattern length.
fn fill_pattern_at(buf: &mut [u8], pattern: &[u8], abs_off: u64) {
    let plen = pattern.len();
    let mut phase = (abs_off % plen as u64) as usize;
    for b in buf.iter_mut() {
        *b = pattern[phase];
        phase += 1;
        if phase == plen {
            phase = 0;
        }
    }
}

/// Demo-mode pacing: sleep so that `done` bytes take `done / bps` seconds,
/// staying responsive to cancellation.
fn throttle(t0: Instant, done: u64, bps: u64, cancel: &AtomicBool) {
    if bps == 0 {
        return;
    }
    let target_elapsed = Duration::from_secs_f64(done as f64 / bps as f64);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let actual = t0.elapsed();
        if actual >= target_elapsed {
            return;
        }
        let nap = (target_elapsed - actual).min(Duration::from_millis(50));
        thread::sleep(nap);
    }
}

/// An open wipe target with transparent O_DIRECT handling.
struct Target {
    file: File,
    path: PathBuf,
    direct: bool,
    /// Byte offset within the current pass (== file position).
    pos: u64,
}

impl Target {
    fn open(path: &Path, size: u64) -> io::Result<Target> {
        // O_DIRECT needs sector-multiple transfer sizes; only attempt it when
        // the target length cooperates (real block devices always do).
        #[cfg(target_os = "linux")]
        if size.is_multiple_of(512) {
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(file) = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_DIRECT)
                .open(path)
            {
                return Ok(Target {
                    file,
                    path: path.to_path_buf(),
                    direct: true,
                    pos: 0,
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = size;

        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Target {
            file,
            path: path.to_path_buf(),
            direct: false,
            pos: 0,
        })
    }

    fn seek_start(&mut self) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        self.pos = 0;
        Ok(())
    }

    fn write_chunk(&mut self, chunk: &[u8]) -> io::Result<()> {
        loop {
            match self.file.write_all(chunk) {
                Ok(()) => {
                    self.pos += chunk.len() as u64;
                    return Ok(());
                }
                // Filesystems that don't support O_DIRECT (tmpfs, some network
                // mounts) accept the open but fail the first write with EINVAL.
                // Only fall back at offset 0, where no bytes can have landed —
                // a retry rewrites the chunk from scratch with no ambiguity.
                Err(e)
                    if self.direct && self.pos == 0 && e.kind() == io::ErrorKind::InvalidInput =>
                {
                    self.reopen_buffered()?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn read_chunk(&mut self, chunk: &mut [u8]) -> io::Result<()> {
        self.file.read_exact(chunk)?;
        self.pos += chunk.len() as u64;
        Ok(())
    }

    fn reopen_buffered(&mut self) -> io::Result<()> {
        self.file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        self.file.seek(SeekFrom::Start(self.pos))?;
        self.direct = false;
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Bus, MediaKind};
    use std::sync::atomic::AtomicBool;

    fn temp_target(size: u64) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.img");
        let f = File::create(&path).unwrap();
        f.set_len(size).unwrap();
        (dir, path)
    }

    fn test_disk(path: &Path, size: u64) -> Disk {
        Disk {
            path: path.to_path_buf(),
            name: "test0".to_string(),
            model: "Unit Test Disk".to_string(),
            serial: "UT-0001".to_string(),
            size_bytes: size,
            bus: Bus::Demo,
            kind: MediaKind::Ssd,
            removable: false,
            lock: None,
            simulated: true,
            throttle_bps: None,
        }
    }

    #[test]
    fn write_then_verify_roundtrip_random() {
        const SIZE: u64 = 4096 * 5; // exercises a partial final chunk path
        let (_dir, path) = temp_target(SIZE);
        let disk = test_disk(&path, SIZE);
        let progress = Progress::new(SIZE * 2, 1);
        let cancel = AtomicBool::new(false);
        let mut target = Target::open(&disk.path, SIZE).unwrap();
        let mut buf = AlignedBuf::zeroed(CHUNK_SIZE);
        let mut expected = AlignedBuf::zeroed(CHUNK_SIZE);

        let pass = Pass::random();
        let seed =
            write_pass(&mut target, SIZE, &pass, None, &progress, &cancel, &mut buf).unwrap();
        assert!(seed.is_some());
        target.sync().unwrap();
        verify_pass(
            &mut target,
            SIZE,
            &pass,
            seed,
            1,
            None,
            &progress,
            &cancel,
            &mut buf,
            &mut expected,
        )
        .unwrap();
    }

    #[test]
    fn verify_detects_tampering() {
        const SIZE: u64 = 4096 * 4;
        let (_dir, path) = temp_target(SIZE);
        let disk = test_disk(&path, SIZE);
        let progress = Progress::new(SIZE * 2, 1);
        let cancel = AtomicBool::new(false);
        let mut target = Target::open(&disk.path, SIZE).unwrap();
        let mut buf = AlignedBuf::zeroed(CHUNK_SIZE);
        let mut expected = AlignedBuf::zeroed(CHUNK_SIZE);

        let pass = Pass::zeros();
        write_pass(&mut target, SIZE, &pass, None, &progress, &cancel, &mut buf).unwrap();
        target.sync().unwrap();

        // Corrupt one byte behind the engine's back.
        {
            let mut f = OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(SeekFrom::Start(8192 + 7)).unwrap();
            f.write_all(&[0xEE]).unwrap();
            f.sync_all().unwrap();
        }

        let err = verify_pass(
            &mut target,
            SIZE,
            &pass,
            None,
            1,
            None,
            &progress,
            &cancel,
            &mut buf,
            &mut expected,
        );
        match err {
            Err(JobError::Verify { pass: 1, offset }) => assert_eq!(offset, 8192 + 7),
            _ => panic!("tampering was not detected"),
        }
    }

    #[test]
    fn pattern_phase_is_continuous_across_chunks() {
        // A 3-byte pattern must not reset at chunk boundaries.
        let pattern: &[u8] = &[0x92, 0x49, 0x24];
        let mut whole = vec![0u8; 1000];
        fill_pattern_at(&mut whole, pattern, 0);

        let mut pieces = vec![0u8; 1000];
        let mut off = 0usize;
        for chunk_len in [333usize, 333, 334] {
            let piece = &mut pieces[off..off + chunk_len];
            fill_pattern_at(piece, pattern, off as u64);
            off += chunk_len;
        }
        assert_eq!(whole, pieces);
        // Spot-check the absolute formula.
        for (i, &b) in whole.iter().enumerate() {
            assert_eq!(b, pattern[i % 3]);
        }
    }

    #[test]
    fn work_accounting() {
        let scheme = crate::algorithm::all_schemes()
            .into_iter()
            .find(|s| s.id == "dod-short")
            .unwrap();
        let spec = WipeSpec {
            scheme,
            rounds: 2,
            verify: VerifyMode::LastPass,
            final_blank: true,
        };
        // 3 passes * 2 rounds + 1 blank = 7 passes, 1 verified.
        assert_eq!(spec.pass_list().len(), 7);
        assert_eq!(spec.work_bytes(1000), 1000 * 8);
        let flags = spec.verify_flags(7);
        assert_eq!(flags.iter().filter(|&&v| v).count(), 1);
        assert!(flags[6]);
    }
}
