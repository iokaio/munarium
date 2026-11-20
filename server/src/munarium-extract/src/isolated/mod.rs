// SPDX-License-Identifier: Apache-2.0
//! Opt-in PDF text extraction in a resource-limited child process (R3/B4).
//!
//! `pdf-text@1` parses tenant-supplied PDFs inside the server process. It
//! contains a panic, but not unbounded allocation or a parse that never ends,
//! and a blocking task cannot be cancelled. This profile runs the same parser
//! in a child process instead:
//!
//! - The parent re-executes the server binary it is running (no new runtime
//!   file, no shell, so the distroless musl image keeps working) with the
//!   private role argument [`CHILD_ROLE_ARG`] and an empty environment.
//! - The input goes over stdin with its exact length on the command line; the
//!   output comes back over stdout as one framed result, read up to a cap.
//! - Memory and CPU time are enforced by the operating system: `setrlimit` in
//!   the child on 64-bit Linux, a Job Object applied by the parent on
//!   Windows. Other platforms have no OS limit and say so in the identity.
//! - The parent enforces a wall-clock deadline and kills and reaps the child
//!   on timeout, on an output overflow and on cancellation.
//! - A process-wide FIFO admission caps concurrent children; a document that
//!   waits longer than the queue wait is deferred, not parsed.
//!
//! The text of a successful extraction is byte-identical to `pdf-text@1`:
//! the child runs [`PdfTextExtractor`] itself. What differs is which
//! documents succeed, so the profile has its own identity and an index built
//! with it never matches a v1.4 index. The limits are provisional; see
//! `server/docs/design/pdf-isolation-measurements.md`.

use crate::pdf::{PdfTextExtractor, PDF_MEDIA};
use crate::{
    Budget, Cancellation, Extracted, ExtractionMethod, ExtractionStatus, FailureClass,
    TextExtractor,
};
use munarium_core::Result;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod linux;
#[cfg(windows)]
mod windows;

/// The private role argument. `munarium-server` dispatches on it before it
/// starts its runtime, reads nothing else from its environment, and runs
/// [`run_child`].
pub const CHILD_ROLE_ARG: &str = "__munarium-extract-pdf-text";

/// The extractor id of the isolated profile. Its identity adds the settings.
pub const ISOLATED_ID: &str = "pdf-text-isolated@1";

const MIB: u64 = 1024 * 1024;
/// stderr kept for classification and the note; the rest is drained.
const STDERR_CAP: usize = 8 * 1024;
/// Room for the frame header and the note beside the text.
const FRAME_OVERHEAD: usize = 4 * 1024;
/// A child note longer than this is truncated.
const NOTE_CAP: usize = 1024;
const POLL: Duration = Duration::from_millis(5);
const MAGIC: &[u8; 4] = b"MXP1";

/// Child exit codes that mean the deployment, not the document, is at fault.
const EXIT_USAGE: i32 = 64;
const EXIT_LIMITS: i32 = 65;
const EXIT_IO: i32 = 66;

/// The bounds of the isolated profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolatedPdfLimits {
    /// OS-enforced memory limit for the child. On Linux this is address
    /// space (`RLIMIT_AS`), which includes the mapped executable; on Windows
    /// it is committed private memory (Job Object process memory limit).
    pub memory_bytes: u64,
    /// OS-enforced CPU time. Linux `RLIMIT_CPU` counts user and system time;
    /// the Windows Job Object per-process limit counts user time only, and
    /// the wall-clock deadline bounds the rest.
    pub cpu_seconds: u64,
    /// Parent-enforced deadline from spawn to exit.
    pub wall_clock: Duration,
    /// The largest extracted text accepted, in bytes.
    pub max_output_bytes: usize,
    /// Children allowed to run at once in this process.
    pub max_concurrent: usize,
    /// How long a document may wait for a slot before it is deferred.
    pub queue_wait: Duration,
}

impl IsolatedPdfLimits {
    /// Provisional defaults, chosen conservatively before measurement. They
    /// are not frozen production limits (B4-T1).
    pub const PROVISIONAL: Self = Self {
        memory_bytes: 512 * MIB,
        cpu_seconds: 10,
        wall_clock: Duration::from_secs(30),
        max_output_bytes: 64 * 1024 * 1024,
        max_concurrent: 2,
        queue_wait: Duration::from_secs(120),
    };

    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.memory_bytes < 16 * MIB {
            return Err("memory limit must be at least 16 MiB".into());
        }
        if self.cpu_seconds == 0 {
            return Err("CPU limit must be at least 1 second".into());
        }
        if self.wall_clock < Duration::from_millis(10) {
            return Err("wall-clock deadline must be at least 10 ms".into());
        }
        if self.max_output_bytes == 0 {
            return Err("output cap must be at least 1 byte".into());
        }
        if !(1..=64).contains(&self.max_concurrent) {
            return Err("concurrency must be between 1 and 64".into());
        }
        Ok(())
    }

    /// The identity contribution. It covers every setting that decides
    /// whether a document yields text, and the OS mechanism enforcing them,
    /// because Linux address space and Windows commit charge accept
    /// different documents at the same number. Concurrency and queue wait
    /// are scheduling: a deferred document is recorded `deferred` on its
    /// source, not silently indexed differently.
    pub fn identity(&self) -> String {
        format!(
            "{ISOLATED_ID}{{os={};mem_bytes={};cpu_s={};wall_ms={};out_bytes={}}}",
            os_limits(),
            self.memory_bytes,
            self.cpu_seconds,
            self.wall_clock.as_millis(),
            self.max_output_bytes
        )
    }
}

/// How this platform enforces the memory and CPU limits.
pub fn os_limits() -> &'static str {
    if cfg!(windows) {
        "job"
    } else if cfg!(all(target_os = "linux", target_pointer_width = "64")) {
        "rlimit"
    } else {
        "none"
    }
}

/// What one isolated extraction cost. Used by the measurement harness and
/// for diagnostics; fields a platform cannot observe are `None`.
#[derive(Debug, Clone, Default)]
pub struct RunReport {
    pub child_pid: Option<u32>,
    /// Time spent waiting for an admission slot.
    pub queued: Duration,
    /// Spawn to reap, including the parent's pipe handling.
    pub elapsed: Duration,
    /// Child CPU time (user + kernel). Windows only today.
    pub cpu: Option<Duration>,
    /// Child peak committed memory. Windows only today.
    pub peak_memory_bytes: Option<u64>,
    /// Bytes read from the child's stdout.
    pub output_bytes: usize,
    /// The child's exit status, when it was reaped.
    pub exit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IsolatedRun {
    pub extracted: Extracted,
    pub report: RunReport,
}

/// The isolated profile's PDF extractor.
pub struct IsolatedPdfExtractor {
    program: PathBuf,
    limits: IsolatedPdfLimits,
    admission: Arc<Admission>,
    identity: String,
}

impl IsolatedPdfExtractor {
    /// `program` must be a binary that dispatches [`CHILD_ROLE_ARG`] to
    /// [`run_child`]. In production that is the running server binary.
    pub fn new(program: PathBuf, limits: IsolatedPdfLimits) -> std::result::Result<Self, String> {
        limits.validate()?;
        Ok(Self {
            program,
            identity: limits.identity(),
            admission: Arc::new(Admission::new(limits.max_concurrent)),
            limits,
        })
    }

    /// Re-execute the binary this process is running.
    pub fn from_current_exe(limits: IsolatedPdfLimits) -> std::result::Result<Self, String> {
        let program = std::env::current_exe()
            .map_err(|e| format!("cannot locate the running executable: {e}"))?;
        Self::new(program, limits)
    }

    pub fn limits(&self) -> &IsolatedPdfLimits {
        &self.limits
    }

    /// Children running right now.
    pub fn running(&self) -> usize {
        self.admission.running()
    }

    /// Children that have run at the same time, at most, since construction.
    pub fn peak_running(&self) -> usize {
        self.admission.peak()
    }

    /// Extract one PDF in a child process. Never panics and never fails: every
    /// outcome is an [`Extracted`] with a [`FailureClass`] when it has no text.
    pub fn run(&self, bytes: &[u8], cancel: &Cancellation) -> IsolatedRun {
        let started = Instant::now();
        let mut report = RunReport::default();
        let extracted = self.run_admitted(bytes, cancel, started, &mut report);
        if extracted.status != ExtractionStatus::Ok {
            tracing::warn!(
                class = extracted.failure.map(|f| f.as_str()).unwrap_or("none"),
                budget = match extracted.failure {
                    Some(FailureClass::OverBudget(b)) => b.as_str(),
                    _ => "",
                },
                status = extracted.status_label(),
                queued_ms = report.queued.as_millis() as u64,
                elapsed_ms = report.elapsed.as_millis() as u64,
                note = extracted.note.as_deref().unwrap_or(""),
                "isolated pdf extraction produced no text"
            );
        }
        IsolatedRun { extracted, report }
    }

    fn run_admitted(
        &self,
        bytes: &[u8],
        cancel: &Cancellation,
        started: Instant,
        report: &mut RunReport,
    ) -> Extracted {
        // The child must hold the whole document before it parses; one that
        // cannot fit is over budget without spending a process on it.
        if bytes.len() as u64 >= self.limits.memory_bytes {
            return over_budget(
                Budget::Input,
                format!(
                    "document is {} bytes; the isolated memory limit is {} bytes",
                    bytes.len(),
                    self.limits.memory_bytes
                ),
            );
        }
        let _permit = match self.admission.acquire(self.limits.queue_wait, cancel) {
            Ok(permit) => permit,
            Err(Refused::Deferred) => {
                report.queued = started.elapsed();
                return Extracted::failed(
                    ExtractionMethod::PdfTextLayer,
                    FailureClass::Deferred,
                    format!(
                        "deferred: no isolated extraction slot within {} ms ({} running)",
                        self.limits.queue_wait.as_millis(),
                        self.limits.max_concurrent
                    ),
                );
            }
            Err(Refused::Cancelled) => {
                report.queued = started.elapsed();
                return cancelled();
            }
        };
        report.queued = started.elapsed();
        let extracted = self.supervise(bytes, cancel, report);
        report.elapsed = started.elapsed().saturating_sub(report.queued);
        extracted
    }

    fn supervise(&self, bytes: &[u8], cancel: &Cancellation, report: &mut RunReport) -> Extracted {
        let mut cmd = Command::new(&self.program);
        cmd.arg(CHILD_ROLE_ARG)
            .arg(format!("--input-bytes={}", bytes.len()))
            .arg(format!("--memory-bytes={}", self.limits.memory_bytes))
            .arg(format!("--cpu-seconds={}", self.limits.cpu_seconds))
            // Nothing from the server's environment reaches the parser:
            // no credentials, no database URL, no configuration.
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return isolation_failed(format!("could not start the extraction child: {e}"))
            }
        };
        report.child_pid = Some(child.id());

        // The child has not read its input yet; it blocks on stdin until the
        // job is in place, so the document is never parsed outside it.
        #[cfg(windows)]
        let job = match windows::Job::confine(&child, &self.limits) {
            Ok(job) => job,
            Err(e) => {
                kill_and_reap(&mut child);
                return isolation_failed(format!(
                    "could not confine the extraction child in a job object: {e}"
                ));
            }
        };

        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            kill_and_reap(&mut child);
            return isolation_failed("the extraction child has no pipes");
        };

        let overflow = AtomicBool::new(false);
        let out_cap = self.limits.max_output_bytes.saturating_add(FRAME_OVERHEAD);
        let (ending, stdout, stderr) = std::thread::scope(|scope| {
            scope.spawn(move || {
                let mut stdin = stdin;
                // A child that dies early breaks the pipe; that is its exit
                // status to report, not an error here.
                let _ = stdin.write_all(bytes);
            });
            let overflow_ref = &overflow;
            let out = scope.spawn(move || read_capped(stdout, out_cap, overflow_ref));
            let err = scope.spawn(move || read_prefix(stderr, STDERR_CAP));
            let ending = watch(&mut child, self.limits.wall_clock, cancel, &overflow);
            (
                ending,
                out.join().unwrap_or_default(),
                err.join().unwrap_or_default(),
            )
        });
        report.output_bytes = stdout.len();
        report.exit = ending.status().map(|s| s.to_string());

        #[cfg(windows)]
        let cpu_exhausted = {
            let usage = job.usage();
            report.cpu = usage.cpu;
            report.peak_memory_bytes = usage.peak_memory_bytes;
            usage.cpu_user >= Some(Duration::from_secs(self.limits.cpu_seconds))
        };
        #[cfg(not(windows))]
        let cpu_exhausted = false;

        classify(
            &ending.observed(),
            overflow.load(Ordering::SeqCst),
            cpu_exhausted,
            &stdout,
            &stderr,
            &self.limits,
        )
    }
}

impl TextExtractor for IsolatedPdfExtractor {
    fn media_types(&self) -> &[&str] {
        &[PDF_MEDIA]
    }

    fn id(&self) -> &'static str {
        ISOLATED_ID
    }

    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn extract(&self, bytes: &[u8]) -> Result<Extracted> {
        Ok(self.run(bytes, &Cancellation::new()).extracted)
    }

    fn extract_cancellable(&self, bytes: &[u8], cancel: &Cancellation) -> Result<Extracted> {
        Ok(self.run(bytes, cancel).extracted)
    }
}

// ---------------------------------------------------------------------------
// supervision
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kill {
    Deadline,
    Cancelled,
    Output,
}

enum Ending {
    Exited(ExitStatus),
    Killed(Kill, Option<ExitStatus>),
    WaitFailed(String),
}

impl Ending {
    fn status(&self) -> Option<ExitStatus> {
        match self {
            Ending::Exited(s) => Some(*s),
            Ending::Killed(_, s) => *s,
            Ending::WaitFailed(_) => None,
        }
    }

    fn observed(&self) -> Observed {
        match self {
            Ending::Exited(status) => Observed::Exited {
                success: status.success(),
                code: status.code(),
                signal: signal_of(status),
                display: status.to_string(),
            },
            Ending::Killed(kill, _) => Observed::Killed(*kill),
            Ending::WaitFailed(e) => Observed::WaitFailed(e.clone()),
        }
    }
}

#[cfg(unix)]
fn signal_of(status: &ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(status)
}

#[cfg(not(unix))]
fn signal_of(_status: &ExitStatus) -> Option<i32> {
    None
}

/// Wait for the child, killing it on the deadline, on cancellation or when
/// its output overflows. Every path that returns has reaped the child.
fn watch(
    child: &mut Child,
    wall_clock: Duration,
    cancel: &Cancellation,
    overflow: &AtomicBool,
) -> Ending {
    let deadline = Instant::now() + wall_clock;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ending::Exited(status),
            Ok(None) => {}
            Err(e) => {
                kill_and_reap(child);
                return Ending::WaitFailed(e.to_string());
            }
        }
        let kill = if cancel.is_cancelled() {
            Some(Kill::Cancelled)
        } else if overflow.load(Ordering::SeqCst) {
            Some(Kill::Output)
        } else if Instant::now() >= deadline {
            Some(Kill::Deadline)
        } else {
            None
        };
        if let Some(kill) = kill {
            let status = kill_and_reap(child);
            return Ending::Killed(kill, status);
        }
        std::thread::sleep(POLL);
    }
}

fn kill_and_reap(child: &mut Child) -> Option<ExitStatus> {
    // Kill fails only when the child already exited; the wait reaps it in
    // either case, so no zombie or orphan outlives this call.
    let _ = child.kill();
    child.wait().ok()
}

/// Read at most `cap` bytes. Past the cap, flag the overflow and stop
/// reading; the watcher then kills the child.
fn read_capped(mut from: impl Read, cap: usize, overflow: &AtomicBool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => return out,
            Ok(n) => {
                if out.len() + n > cap {
                    overflow.store(true, Ordering::SeqCst);
                    return out;
                }
                out.extend_from_slice(&buf[..n]);
            }
        }
    }
}

/// Keep the first `cap` bytes and drain the rest, so a chatty child never
/// blocks on a full stderr pipe.
fn read_prefix(mut from: impl Read, cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => return out,
            Ok(n) => {
                let room = cap.saturating_sub(out.len());
                out.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
}

/// What the parent observed, separated from `ExitStatus` so classification
/// is a pure function the tests can drive on every platform.
#[derive(Debug, Clone)]
enum Observed {
    Exited {
        success: bool,
        code: Option<i32>,
        signal: Option<i32>,
        display: String,
    },
    Killed(Kill),
    WaitFailed(String),
}

const SIGKILL: i32 = 9;
const SIGXCPU: i32 = 24;

fn classify(
    observed: &Observed,
    overflow: bool,
    cpu_exhausted: bool,
    stdout: &[u8],
    stderr: &[u8],
    limits: &IsolatedPdfLimits,
) -> Extracted {
    if overflow {
        return over_budget(
            Budget::Output,
            format!(
                "extracted text exceeded the {} byte output cap",
                limits.max_output_bytes
            ),
        );
    }
    let stderr = String::from_utf8_lossy(stderr);
    match observed {
        Observed::Killed(Kill::Cancelled) => cancelled(),
        Observed::Killed(Kill::Output) => over_budget(
            Budget::Output,
            format!(
                "extracted text exceeded the {} byte output cap",
                limits.max_output_bytes
            ),
        ),
        Observed::Killed(Kill::Deadline) => over_budget(
            Budget::WallClock,
            format!(
                "extraction did not finish within {} ms",
                limits.wall_clock.as_millis()
            ),
        ),
        Observed::WaitFailed(e) => isolation_failed(format!("waiting for the child failed: {e}")),
        Observed::Exited { success: true, .. } => match decode(stdout) {
            Ok(Frame::Text { status, note, text }) => Extracted {
                text,
                status,
                method: ExtractionMethod::PdfTextLayer,
                pages: Vec::new(),
                note,
                failure: None,
            },
            Ok(Frame::Parser(message)) => Extracted::failed(
                ExtractionMethod::PdfTextLayer,
                FailureClass::ParserFailed,
                format!("{ISOLATED_ID} failed: {message}"),
            ),
            Ok(Frame::OverMemory) => over_budget(
                Budget::Memory,
                format!(
                    "the document did not fit the {} byte memory limit",
                    limits.memory_bytes
                ),
            ),
            Err(e) => isolation_failed(format!("unreadable result from the child: {e}")),
        },
        Observed::Exited {
            code,
            signal,
            display,
            ..
        } => {
            // Rust's allocation-failure handler prints this before aborting;
            // it is how an OS memory limit surfaces on both platforms.
            if stderr.contains("memory allocation of") {
                return over_budget(
                    Budget::Memory,
                    format!(
                        "the parser exceeded the {} byte memory limit",
                        limits.memory_bytes
                    ),
                );
            }
            if cpu_exhausted || *signal == Some(SIGXCPU) {
                return over_budget(
                    Budget::Cpu,
                    format!(
                        "the parser exceeded the {} second CPU limit",
                        limits.cpu_seconds
                    ),
                );
            }
            if *signal == Some(SIGKILL) {
                // Not sent by the parent (that path is `Killed`), and the
                // hard CPU limit sits above the SIGXCPU soft limit, so this
                // is the system reclaiming memory.
                return over_budget(
                    Budget::Memory,
                    "the parser was killed by the system (memory pressure)",
                );
            }
            if matches!(code, Some(EXIT_USAGE | EXIT_LIMITS | EXIT_IO)) {
                return isolation_failed(format!(
                    "the extraction child refused to run ({display}): {}",
                    first_line(&stderr)
                ));
            }
            Extracted::failed(
                ExtractionMethod::PdfTextLayer,
                FailureClass::ParserFailed,
                format!(
                    "{ISOLATED_ID} failed: the parser process ended abnormally ({display}): {}",
                    first_line(&stderr)
                ),
            )
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}

fn over_budget(budget: Budget, note: impl Into<String>) -> Extracted {
    Extracted::failed(
        ExtractionMethod::PdfTextLayer,
        FailureClass::OverBudget(budget),
        format!("over budget ({}): {}", budget.as_str(), note.into()),
    )
}

fn cancelled() -> Extracted {
    Extracted::failed(
        ExtractionMethod::PdfTextLayer,
        FailureClass::Cancelled,
        "cancelled: the build stopped before extraction finished",
    )
}

fn isolation_failed(note: impl Into<String>) -> Extracted {
    Extracted::failed(
        ExtractionMethod::PdfTextLayer,
        FailureClass::IsolationFailed,
        note,
    )
}

// ---------------------------------------------------------------------------
// admission
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    Deferred,
    Cancelled,
}

#[derive(Default)]
struct AdmissionState {
    running: usize,
    peak: usize,
    next_ticket: u64,
    waiting: VecDeque<u64>,
}

/// A FIFO counting semaphore with a bounded wait and cancellation.
struct Admission {
    capacity: usize,
    state: Mutex<AdmissionState>,
    changed: Condvar,
    running: AtomicUsize,
}

struct Permit<'a>(&'a Admission);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.running = state.running.saturating_sub(1);
        self.0.running.store(state.running, Ordering::SeqCst);
        drop(state);
        self.0.changed.notify_all();
    }
}

impl Admission {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(AdmissionState::default()),
            changed: Condvar::new(),
            running: AtomicUsize::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, AdmissionState> {
        // The state is counters only; a poisoned lock still holds them.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    fn peak(&self) -> usize {
        self.lock().peak
    }

    fn acquire(
        &self,
        wait: Duration,
        cancel: &Cancellation,
    ) -> std::result::Result<Permit<'_>, Refused> {
        let deadline = Instant::now() + wait;
        let mut state = self.lock();
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        state.waiting.push_back(ticket);
        loop {
            if state.waiting.front() == Some(&ticket) && state.running < self.capacity {
                state.waiting.pop_front();
                state.running += 1;
                state.peak = state.peak.max(state.running);
                self.running.store(state.running, Ordering::SeqCst);
                drop(state);
                // The next ticket may also fit.
                self.changed.notify_all();
                return Ok(Permit(self));
            }
            let now = Instant::now();
            let refused = if cancel.is_cancelled() {
                Some(Refused::Cancelled)
            } else if now >= deadline {
                Some(Refused::Deferred)
            } else {
                None
            };
            if let Some(refused) = refused {
                state.waiting.retain(|t| *t != ticket);
                drop(state);
                self.changed.notify_all();
                return Err(refused);
            }
            // Short slices so a cancellation is noticed promptly.
            let slice = (deadline - now).min(Duration::from_millis(20));
            state = self
                .changed
                .wait_timeout(state, slice)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }
}

// ---------------------------------------------------------------------------
// the child protocol
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Frame {
    Text {
        status: ExtractionStatus,
        note: Option<String>,
        text: String,
    },
    Parser(String),
    OverMemory,
}

fn encode(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    match frame {
        Frame::Text { status, note, text } => {
            out.push(b'T');
            out.push(if *status == ExtractionStatus::Ok {
                b'o'
            } else {
                b'e'
            });
            let note = note.as_deref().map(|n| truncate(n, NOTE_CAP));
            match note {
                Some(n) => {
                    out.push(1);
                    out.extend_from_slice(&(n.len() as u32).to_le_bytes());
                    out.extend_from_slice(n.as_bytes());
                }
                None => out.push(0),
            }
            out.extend_from_slice(&(text.len() as u64).to_le_bytes());
            out.extend_from_slice(text.as_bytes());
        }
        Frame::Parser(message) => {
            out.push(b'P');
            let m = truncate(message, NOTE_CAP);
            out.extend_from_slice(&(m.len() as u32).to_le_bytes());
            out.extend_from_slice(m.as_bytes());
        }
        Frame::OverMemory => out.push(b'M'),
    }
    out
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn decode(bytes: &[u8]) -> std::result::Result<Frame, String> {
    let mut r = Cursor { bytes, at: 0 };
    if r.take(4)? != MAGIC {
        return Err("missing frame header".into());
    }
    let frame = match r.byte()? {
        b'T' => {
            let status = match r.byte()? {
                b'o' => ExtractionStatus::Ok,
                b'e' => ExtractionStatus::Empty,
                other => return Err(format!("unknown status byte {other}")),
            };
            let note = match r.byte()? {
                0 => None,
                1 => {
                    let n = r.u32()? as usize;
                    Some(r.utf8(n)?)
                }
                other => return Err(format!("unknown note flag {other}")),
            };
            let n = usize::try_from(r.u64()?).map_err(|_| "text length overflows")?;
            let text = r.utf8(n)?;
            Frame::Text { status, note, text }
        }
        b'P' => {
            let n = r.u32()? as usize;
            Frame::Parser(r.utf8(n)?)
        }
        b'M' => Frame::OverMemory,
        other => return Err(format!("unknown frame kind {other}")),
    };
    if r.at != bytes.len() {
        return Err("trailing bytes after the frame".into());
    }
    Ok(frame)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> std::result::Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or("truncated frame")?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn byte(&mut self) -> std::result::Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> std::result::Result<u32, String> {
        let mut b = [0u8; 4];
        b.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> std::result::Result<u64, String> {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(b))
    }

    fn utf8(&mut self, n: usize) -> std::result::Result<String, String> {
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "text is not UTF-8".into())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ChildArgs {
    input_bytes: usize,
    memory_bytes: u64,
    cpu_seconds: u64,
}

impl ChildArgs {
    fn parse(args: &[OsString]) -> std::result::Result<Self, String> {
        let mut input_bytes = None;
        let mut memory_bytes = None;
        let mut cpu_seconds = None;
        for arg in args {
            let arg = arg.to_str().ok_or("arguments must be UTF-8")?;
            let (key, value) = arg
                .split_once('=')
                .ok_or_else(|| format!("unexpected argument '{arg}'"))?;
            let n: u64 = value
                .parse()
                .map_err(|_| format!("{key} must be a number"))?;
            match key {
                "--input-bytes" => {
                    input_bytes = Some(usize::try_from(n).map_err(|_| "input too large")?)
                }
                "--memory-bytes" => memory_bytes = Some(n),
                "--cpu-seconds" => cpu_seconds = Some(n),
                _ => return Err(format!("unexpected argument '{key}'")),
            }
        }
        Ok(Self {
            input_bytes: input_bytes.ok_or("--input-bytes is required")?,
            memory_bytes: memory_bytes.ok_or("--memory-bytes is required")?,
            cpu_seconds: cpu_seconds.ok_or("--cpu-seconds is required")?,
        })
    }
}

/// The child role. `args` are the arguments after [`CHILD_ROLE_ARG`].
/// Returns the process exit code. Writes exactly one frame to stdout when it
/// returns 0.
pub fn run_child(args: &[OsString]) -> i32 {
    let args = match ChildArgs::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("extraction child: {e}");
            return EXIT_USAGE;
        }
    };
    // Limits first: nothing below runs without them.
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    if let Err(e) = linux::apply(args.memory_bytes, args.cpu_seconds) {
        eprintln!("extraction child: could not apply limits: {e}");
        return EXIT_LIMITS;
    }

    let frame = match read_input(args.input_bytes) {
        Ok(Some(input)) => match PdfTextExtractor.extract(&input) {
            Ok(e) => Frame::Text {
                status: e.status,
                note: e.note,
                text: e.text,
            },
            Err(e) => Frame::Parser(e.to_string()),
        },
        Ok(None) => Frame::OverMemory,
        Err(e) => {
            eprintln!("extraction child: reading input: {e}");
            return EXIT_IO;
        }
    };
    let mut out = std::io::stdout().lock();
    if out
        .write_all(&encode(&frame))
        .and_then(|()| out.flush())
        .is_err()
    {
        return EXIT_IO;
    }
    0
}

/// `None` when the input cannot be allocated under the memory limit.
fn read_input(len: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut input = Vec::new();
    if input.try_reserve_exact(len).is_err() {
        return Ok(None);
    }
    input.resize(len, 0);
    std::io::stdin().lock().read_exact(&mut input)?;
    Ok(Some(input))
}

#[cfg(test)]
mod tests;
