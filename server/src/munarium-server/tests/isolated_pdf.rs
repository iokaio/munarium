// SPDX-License-Identifier: Apache-2.0
//! The isolated PDF extraction profile (R3/B4) against the real server
//! binary: each bound, child cleanup, failure classes and the measurement
//! harness. Every PDF here is synthetic and generated in the test; nothing
//! binary is committed.
//!
//! The memory and CPU bounds are OS-enforced and only exist where the
//! platform mechanism does (a Job Object on Windows, `setrlimit` on 64-bit
//! Linux), so those tests are gated to those platforms.
//!
//! Measurement (not a pass/fail test; run in release for representative
//! numbers):
//!
//! ```console
//! cargo test --release -p munarium-server --test isolated_pdf measure -- --ignored --nocapture
//! ```

use munarium_extract::{
    Budget, Cancellation, ExtractionStatus, FailureClass, IsolatedPdfExtractor, IsolatedPdfLimits,
    PdfTextExtractor, TextExtractor,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_munarium-server"))
}

/// Generous limits: nothing in a normal document comes near them.
fn generous() -> IsolatedPdfLimits {
    IsolatedPdfLimits {
        memory_bytes: 1024 * MIB,
        cpu_seconds: 120,
        wall_clock: Duration::from_secs(120),
        max_output_bytes: 64 * MIB as usize,
        max_concurrent: 2,
        queue_wait: Duration::from_secs(120),
    }
}

fn extractor(limits: IsolatedPdfLimits) -> IsolatedPdfExtractor {
    IsolatedPdfExtractor::new(binary(), limits).expect("valid limits")
}

/// A one-page PDF around `content`, with a correct xref table.
fn pdf_with_content(content: &str) -> Vec<u8> {
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .into(),
    ];
    objects.push(format!(
        "<< /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());

    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref_at = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    pdf.push_str("0000000000 65535 f \n");
    for off in &offsets {
        pdf.push_str(&format!("{off:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
        objects.len() + 1
    ));
    pdf.into_bytes()
}

/// A normal document: `lines` lines of prose.
fn text_pdf(lines: usize) -> Vec<u8> {
    let mut content = String::from("BT /F1 10 Tf 72 760 Td 12 TL\n");
    for i in 0..lines {
        content.push_str(&format!(
            "(Line {i}: the quarterly settlement was approved on 14 March) Tj T*\n"
        ));
    }
    content.push_str("ET");
    pdf_with_content(&content)
}

/// A document whose content stream is `ops` graphics-state operators: cheap
/// to generate, parsed and interpreted one by one, producing no text. Parse
/// time and memory grow with `ops`; this is the pathological shape.
fn heavy_pdf(ops: usize) -> Vec<u8> {
    let mut content = String::with_capacity(ops * 16);
    content.push_str("BT /F1 10 Tf 72 760 Td (Heavy document) Tj ET\n");
    for _ in 0..ops {
        content.push_str("1 0 0 1 0 0 cm\n");
    }
    pdf_with_content(&content)
}

/// Whether a process with this id still exists (running or unreaped).
#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(any(windows, unix)))]
fn process_exists(_pid: u32) -> bool {
    false
}

fn assert_reaped(pid: Option<u32>) {
    let pid = pid.expect("a child was started");
    assert!(
        !process_exists(pid),
        "extraction child {pid} outlived its extraction"
    );
}

fn budget(run: &munarium_extract::IsolatedRun) -> Option<Budget> {
    match run.extracted.failure {
        Some(FailureClass::OverBudget(b)) => Some(b),
        _ => None,
    }
}

#[test]
fn a_normal_pdf_yields_exactly_the_inline_text() {
    let bytes = text_pdf(40);
    let inline = PdfTextExtractor.extract(&bytes).expect("inline");
    let run = extractor(generous()).run(&bytes, &Cancellation::new());
    assert_eq!(
        run.extracted.status,
        ExtractionStatus::Ok,
        "{:?}",
        run.extracted.note
    );
    assert_eq!(run.extracted.failure, None);
    assert_eq!(run.extracted.text, inline.text);
    assert_eq!(run.extracted.status, inline.status);
    assert_eq!(run.extracted.method, inline.method);
    assert_eq!(run.extracted.note, inline.note);
    assert_eq!(run.extracted.pages, inline.pages);
    assert_reaped(run.report.child_pid);
}

#[test]
fn a_pdf_without_a_text_layer_is_empty_exactly_as_inline() {
    let bytes = pdf_with_content("");
    let inline = PdfTextExtractor.extract(&bytes).expect("inline");
    let run = extractor(generous()).run(&bytes, &Cancellation::new());
    assert_eq!(run.extracted.status, ExtractionStatus::Empty);
    assert_eq!(run.extracted.note, inline.note);
    assert_eq!(run.extracted.failure, None);
}

#[test]
fn a_malformed_pdf_is_a_parser_failure_not_over_budget() {
    let x = extractor(generous());
    let mut truncated = text_pdf(5);
    truncated.truncate(truncated.len() / 3);
    for bytes in [
        b"%PDF-1.4\nnot really a pdf".to_vec(),
        truncated,
        Vec::new(),
    ] {
        let run = x.run(&bytes, &Cancellation::new());
        assert_eq!(
            run.extracted.failure,
            Some(FailureClass::ParserFailed),
            "{:?}",
            run.extracted.note
        );
        assert_eq!(run.extracted.status_label(), "failed");
        assert!(run.extracted.text.is_empty());
        assert_reaped(run.report.child_pid);
    }
}

#[test]
fn the_wall_clock_deadline_kills_and_reaps_the_child() {
    let mut limits = generous();
    limits.wall_clock = Duration::from_millis(300);
    let bytes = heavy_pdf(2_000_000);
    let started = Instant::now();
    let run = extractor(limits).run(&bytes, &Cancellation::new());
    let took = started.elapsed();
    assert_eq!(
        budget(&run),
        Some(Budget::WallClock),
        "{:?}",
        run.extracted.note
    );
    assert_eq!(run.extracted.status_label(), "over_budget");
    // Killed at the deadline, not left to finish: the parse alone takes
    // seconds.
    assert!(took < Duration::from_secs(5), "took {took:?}");
    assert_reaped(run.report.child_pid);
}

#[test]
fn cancellation_kills_and_reaps_the_child() {
    let x = extractor(generous());
    let bytes = heavy_pdf(2_000_000);
    let cancel = Cancellation::new();
    let trigger = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        trigger.cancel();
    });
    let started = Instant::now();
    let run = x.run(&bytes, &cancel);
    canceller.join().expect("canceller");
    assert_eq!(run.extracted.failure, Some(FailureClass::Cancelled));
    assert!(!run.extracted.escalation_eligible());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_reaped(run.report.child_pid);
    assert_eq!(x.running(), 0);
}

#[test]
fn the_output_cap_refuses_oversized_text() {
    let mut limits = generous();
    limits.max_output_bytes = 4 * 1024;
    let run = extractor(limits).run(&text_pdf(400), &Cancellation::new());
    assert_eq!(
        budget(&run),
        Some(Budget::Output),
        "{:?}",
        run.extracted.note
    );
    assert!(run.report.output_bytes <= 4 * 1024 + 4 * 1024);
    assert_reaped(run.report.child_pid);
}

#[test]
fn a_document_larger_than_the_memory_limit_never_reaches_a_child() {
    let mut limits = generous();
    limits.memory_bytes = 16 * MIB;
    let run = extractor(limits).run(&vec![b' '; 16 * MIB as usize], &Cancellation::new());
    assert_eq!(budget(&run), Some(Budget::Input));
    assert_eq!(run.report.child_pid, None);
}

#[cfg(any(windows, all(target_os = "linux", target_pointer_width = "64")))]
#[test]
fn the_os_memory_limit_stops_a_parse_that_allocates_past_it() {
    let mut limits = generous();
    // Linux limits address space, which includes the mapped executable and
    // its libraries, so it needs headroom Windows commit charge does not.
    limits.memory_bytes = if cfg!(windows) { 96 * MIB } else { 256 * MIB };
    let bytes = heavy_pdf(4_000_000);
    assert!((bytes.len() as u64) < limits.memory_bytes, "input fits");
    let run = extractor(limits).run(&bytes, &Cancellation::new());
    assert_eq!(
        budget(&run),
        Some(Budget::Memory),
        "{:?}",
        run.extracted.note
    );
    assert_reaped(run.report.child_pid);
}

#[cfg(any(windows, all(target_os = "linux", target_pointer_width = "64")))]
#[test]
fn the_os_cpu_limit_stops_a_long_parse() {
    let mut limits = generous();
    limits.cpu_seconds = 1;
    let bytes = heavy_pdf(2_000_000);
    let started = Instant::now();
    let run = extractor(limits).run(&bytes, &Cancellation::new());
    assert_eq!(budget(&run), Some(Budget::Cpu), "{:?}", run.extracted.note);
    // Stopped by the OS near one CPU second, long before the wall clock.
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_reaped(run.report.child_pid);
}

#[test]
fn admission_caps_concurrent_children_and_defers_the_rest() {
    let mut limits = generous();
    limits.max_concurrent = 1;
    limits.queue_wait = Duration::from_millis(200);
    limits.wall_clock = Duration::from_secs(3);
    let x = Arc::new(extractor(limits));
    let heavy = heavy_pdf(2_000_000);
    let busy = {
        let x = Arc::clone(&x);
        std::thread::spawn(move || x.run(&heavy, &Cancellation::new()))
    };
    let waited = Instant::now();
    while x.running() == 0 {
        assert!(
            waited.elapsed() < Duration::from_secs(10),
            "child never admitted"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let deferred = x.run(&text_pdf(3), &Cancellation::new());
    assert_eq!(deferred.extracted.failure, Some(FailureClass::Deferred));
    assert_eq!(deferred.extracted.status_label(), "deferred");
    assert!(!deferred.extracted.escalation_eligible());
    assert_eq!(
        deferred.report.child_pid, None,
        "a deferred document is never parsed"
    );
    let first = busy.join().expect("busy");
    assert_reaped(first.report.child_pid);
    assert_eq!(x.running(), 0);
    assert_eq!(x.peak_running(), 1);
    // The slot is free again.
    let after = x.run(&text_pdf(3), &Cancellation::new());
    assert_eq!(after.extracted.status, ExtractionStatus::Ok);
}

#[test]
fn concurrent_documents_up_to_the_cap_all_run() {
    let mut limits = generous();
    limits.max_concurrent = 3;
    let x = Arc::new(extractor(limits));
    let workers: Vec<_> = (0..6)
        .map(|i| {
            let x = Arc::clone(&x);
            std::thread::spawn(move || x.run(&text_pdf(10 + i), &Cancellation::new()))
        })
        .collect();
    for w in workers {
        let run = w.join().expect("worker");
        assert_eq!(run.extracted.status, ExtractionStatus::Ok);
        assert_reaped(run.report.child_pid);
    }
    assert!(x.peak_running() <= 3, "peak {}", x.peak_running());
    assert_eq!(x.running(), 0);
}

#[test]
fn the_child_role_refuses_bad_arguments_without_reading_input() {
    let out = std::process::Command::new(binary())
        .arg(munarium_extract::isolated::CHILD_ROLE_ARG)
        .arg("--input-bytes=4")
        .env_clear()
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run child role");
    assert_eq!(out.status.code(), Some(64));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--memory-bytes is required"));
}

// ---------------------------------------------------------------------------
// measurement harness
// ---------------------------------------------------------------------------

/// Records CPU, peak memory, elapsed time, output size and the overhead over
/// inline extraction for representative normal and near-limit documents
/// under the provisional limits. Prints one row per document.
#[test]
#[ignore = "measurement harness; run explicitly in release"]
fn measure() {
    let limits = IsolatedPdfLimits::PROVISIONAL;
    let x = extractor(limits);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty text layer", pdf_with_content("")),
        ("1 page, 40 lines", text_pdf(40)),
        ("text, 2k lines", text_pdf(2_000)),
        ("text, 20k lines", text_pdf(20_000)),
        ("heavy, 250k ops", heavy_pdf(250_000)),
        ("heavy, 1M ops", heavy_pdf(1_000_000)),
        ("heavy, 4M ops", heavy_pdf(4_000_000)),
        ("heavy, 8M ops", heavy_pdf(8_000_000)),
    ];
    println!(
        "os_limits={} limits={limits:?}",
        munarium_extract::isolated::os_limits()
    );
    println!(
        "| document | input bytes | outcome | inline ms | isolated ms | overhead ms | child CPU ms | peak memory MiB | output bytes |"
    );
    println!("|---|---:|---|---:|---:|---:|---:|---:|---:|");
    for (name, bytes) in cases {
        // Inline only where it is safe to run in this process.
        let (inline_ms, inline_text) = {
            let t = Instant::now();
            let r = PdfTextExtractor.extract(&bytes);
            (
                t.elapsed().as_secs_f64() * 1000.0,
                r.ok().map(|e| e.text.len()),
            )
        };
        let run = x.run(&bytes, &Cancellation::new());
        let iso_ms = run.report.elapsed.as_secs_f64() * 1000.0;
        let outcome = match run.extracted.failure {
            Some(FailureClass::OverBudget(b)) => format!("over_budget({})", b.as_str()),
            Some(f) => f.as_str().to_string(),
            None => run.extracted.status.as_str().to_string(),
        };
        if run.extracted.failure.is_none() {
            assert_eq!(Some(run.extracted.text.len()), inline_text, "{name}");
        }
        println!(
            "| {name} | {} | {outcome} | {inline_ms:.1} | {iso_ms:.1} | {:.1} | {} | {} | {} |",
            bytes.len(),
            iso_ms - inline_ms,
            run.report
                .cpu
                .map(|c| format!("{:.0}", c.as_secs_f64() * 1000.0))
                .unwrap_or_else(|| "n/a".into()),
            run.report
                .peak_memory_bytes
                .map(|m| format!("{:.1}", m as f64 / MIB as f64))
                .unwrap_or_else(|| "n/a".into()),
            run.report.output_bytes,
        );
    }

    // Concurrency: N documents at once through the cap.
    for n in [1usize, 2, 4] {
        let x = Arc::new(extractor(limits));
        let doc = Arc::new(text_pdf(2_000));
        let t = Instant::now();
        let workers: Vec<_> = (0..n)
            .map(|_| {
                let x = Arc::clone(&x);
                let doc = Arc::clone(&doc);
                std::thread::spawn(move || x.run(&doc, &Cancellation::new()))
            })
            .collect();
        let runs: Vec<_> = workers.into_iter().map(|w| w.join().expect("w")).collect();
        let queued: Duration = runs
            .iter()
            .map(|r| r.report.queued)
            .max()
            .unwrap_or_default();
        println!(
            "concurrency: {n} documents, cap {}: total {:.1} ms, longest queue wait {:.1} ms, peak running {}",
            limits.max_concurrent,
            t.elapsed().as_secs_f64() * 1000.0,
            queued.as_secs_f64() * 1000.0,
            x.peak_running()
        );
    }
}
