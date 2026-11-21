// SPDX-License-Identifier: Apache-2.0
//! The isolated PDF extraction profile (R3/B4) against the real server
//! binary: each bound, child cleanup, failure classes and the measurement
//! harness. Every PDF here is synthetic and generated in the test; nothing
//! binary is committed.
//!
//! The memory and CPU bounds are OS-enforced by `setrlimit` on 64-bit Linux,
//! the only production platform, so those tests are Linux-only. Other builds,
//! Windows included, are for development and testing, apply no OS limit, and
//! are tested to say so and to extract normally.
//!
//! Measurement (not a pass/fail test; run in release for representative
//! numbers):
//!
//! ```console
//! cargo test --release -p munarium-server --test isolated_pdf measure -- --ignored --nocapture
//! ```
//!
//! Set `MUNARIUM_PDF_MEASURE_DIR` to a directory of PDFs to measure them too
//! (for example public reports downloaded for the purpose; they are read,
//! never committed). On Linux the harness also reports child CPU time and
//! the smallest memory limit each document extracts under.

use munarium_extract::{
    Budget, Cancellation, DeferredPolicy, ExtractionStatus, FailureClass, IsolatedPdfExtractor,
    IsolatedPdfLimits, IsolatedPdfPolicy, PdfTextExtractor, TextExtractor,
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

/// Neither setting escalates a document that was never read.
fn assert_never_escalates(extracted: &munarium_extract::Extracted) {
    for escalate_over_budget in [false, true] {
        let policy = IsolatedPdfPolicy {
            escalate_over_budget,
            ..Default::default()
        };
        assert!(!extracted.escalation_eligible(&policy), "{policy:?}");
    }
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
    assert_never_escalates(&run.extracted);
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

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn the_os_memory_limit_stops_a_parse_that_allocates_past_it() {
    let mut limits = generous();
    // Linux limits address space, which includes the mapped executable and
    // its libraries, so the limit needs headroom above the parse itself.
    limits.memory_bytes = 256 * MIB;
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

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
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

/// Development and test builds (Windows included) apply no OS limit: they
/// say so in the identity, and a parse that needs more memory than the
/// configured limit (about 270 MiB for this document, measured) still
/// completes, bounded only by the wall clock and the output cap.
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
#[test]
fn without_os_limits_the_identity_says_so_and_extraction_still_works() {
    assert_eq!(munarium_extract::isolated::os_limits(), "none");
    let mut limits = generous();
    limits.memory_bytes = 96 * MIB;
    let x = extractor(limits);
    assert!(
        x.identity().starts_with("pdf-text-isolated@1{os=none;"),
        "{}",
        x.identity()
    );
    let bytes = heavy_pdf(250_000);
    assert!((bytes.len() as u64) < limits.memory_bytes, "input fits");
    let inline = PdfTextExtractor.extract(&bytes).expect("inline");
    let run = x.run(&bytes, &Cancellation::new());
    assert_eq!(run.extracted.failure, None, "{:?}", run.extracted.note);
    assert_eq!(run.extracted.status, inline.status);
    assert_eq!(run.extracted.text, inline.text);
    assert_eq!(run.report.cpu, None);
    assert_eq!(run.report.peak_memory_bytes, None);
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
    assert_never_escalates(&deferred.extracted);
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
// a queued build stopped during an isolated extraction (R6 with R3)
// ---------------------------------------------------------------------------

const PDF: &str = "application/pdf";

/// The isolated extractor, observable from the test: it records each run's
/// child pid and outcome, and shares the admission counters.
struct Probe {
    inner: Arc<IsolatedPdfExtractor>,
    runs: Arc<std::sync::Mutex<Vec<munarium_extract::IsolatedRun>>>,
}

impl TextExtractor for Probe {
    fn media_types(&self) -> &[&str] {
        &[PDF]
    }

    fn extract(&self, bytes: &[u8]) -> munarium_core::Result<munarium_extract::Extracted> {
        self.extract_cancellable(bytes, &Cancellation::new())
    }

    fn id(&self) -> &'static str {
        "pdf-isolated-probe"
    }

    fn extract_cancellable(
        &self,
        bytes: &[u8],
        cancel: &Cancellation,
    ) -> munarium_core::Result<munarium_extract::Extracted> {
        let run = self.inner.run(bytes, cancel);
        let extracted = run.extracted.clone();
        self.runs.lock().expect("runs").push(run);
        Ok(extracted)
    }
}

/// ClearView R6 with R3: cancelling a queued direct build while its one
/// source is inside a slow isolated PDF extraction kills and reaps the
/// child at once, through the job's stop signal, and the build returns
/// (the builder loop's slot is free) with the stale-claim error and nothing
/// committed. The stop is driven exactly as the builder's lease watch
/// drives it: the job is cancelled, the fenced renewal is refused, and the
/// signal stops with the recorded reason. Needs PostgreSQL; skips loudly
/// without `MUNARIUM_TEST_DATABASE_URL`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_queued_build_kills_its_isolated_child_and_frees_the_builder() {
    use munarium_retrieval::mirror::{LocalStoreFactory, MirrorContext, VectorPolicy};
    use munarium_store_pg::artifacts::ArtifactCatalog;
    use munarium_store_pg::attempts::BuildAttempts;
    use munarium_store_pg::jobs::{
        BuildJobs, HistoryStatus, JobFence, JobId, JobSignal, StopReason, STALE_GENERATION_MESSAGE,
    };

    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let tenant = format!("tenant-r3-stop-{}", uuid::Uuid::new_v4().simple());
    let store = munarium_store_pg::PgStore::connect(&url, &tenant)
        .await
        .expect("connect");
    let pool = store.pool().clone();
    let probe = Arc::new(extractor(generous()));
    let runs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let registry = munarium_extract::ExtractorRegistry::with_extractors(vec![Box::new(Probe {
        inner: Arc::clone(&probe),
        runs: Arc::clone(&runs),
    })]);
    let pg = munarium_retrieval_pg::PgRetrieval::new(pool.clone(), &tenant)
        .with_extractors(Arc::new(registry));
    let col = pg
        .ensure_collection(
            &format!("col-r3-{}", uuid::Uuid::new_v4().simple()),
            "para",
            0,
            &[],
            Some("R3 child stop"),
        )
        .await
        .expect("collection");
    let (source_id, _, _) = pg
        .put_source("", PDF, "r3/slow.pdf", Some("para"), &heavy_pdf(2_000_000))
        .await
        .expect("source");
    pg.bind_source(&col.id, &source_id, None)
        .await
        .expect("bind");

    // A direct job claimed once by this builder, tenant-scoped and under an
    // infinite lease, so no other test's claimer or cleaner can take it.
    let job_id = format!("bjob-r3-{}", uuid::Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO index_build_jobs
             (tenant_id, job_id, kind, scope_kind, scope_id, params, state, claimed_by,
              claimed_at, attempts, requested_by)
         VALUES ($1,$2,'direct','collection',$3,'{}'::jsonb,'running','builder-r3',
                 'infinity',1,'test-r3')",
    )
    .bind(&tenant)
    .bind(&job_id)
    .bind(&col.id)
    .execute(&pool)
    .await
    .expect("seed job");
    sqlx::query(
        "INSERT INTO index_build_job_runs (tenant_id, job_id, job_generation, node_id)
         VALUES ($1,$2,1,'builder-r3')",
    )
    .bind(&tenant)
    .bind(&job_id)
    .execute(&pool)
    .await
    .expect("seed run");
    let fence = JobFence {
        tenant_id: tenant.clone(),
        job_id: job_id.clone(),
        node_id: "builder-r3".into(),
        generation: 1,
    };
    let signal = JobSignal::new();
    let artifacts = tempfile::tempdir().expect("artifacts");
    let staging = tempfile::tempdir().expect("staging");
    let ctx = MirrorContext {
        catalog: ArtifactCatalog::new(pool.clone(), &tenant),
        attempts: BuildAttempts::new(pool.clone(), &tenant),
        stores: Arc::new(LocalStoreFactory::new(artifacts.path())),
        node_id: "builder-r3".into(),
        staging_root: staging.path().to_path_buf(),
        artifact_prefix: "v1".into(),
        tenant_path_hash: "t0000".into(),
        faults: None,
        observer: None,
        vector_policy: VectorPolicy {
            approx_threshold: None,
        },
        job_fence: Some(fence.clone()),
        job_signal: Some(signal.clone()),
        publication_damage: None,
    };

    let collection_id = col.id.clone();
    let build = tokio::spawn(async move {
        munarium_retrieval::direct::build_collection_direct(&ctx, &pg, &collection_id, 400, 0).await
    });

    // Wait until the child is parsing.
    let waited = Instant::now();
    while probe.running() == 0 {
        assert!(
            waited.elapsed() < Duration::from_secs(60),
            "the extraction child never started"
        );
        assert!(!build.is_finished(), "the build ended before extracting");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Cancel, then what the lease watch does: a refused renewal stops the
    // signal with the recorded reason.
    let jobs = BuildJobs::new(pool.clone());
    assert!(jobs.cancel(&tenant, &job_id).await.expect("cancel"));
    assert!(!jobs
        .heartbeat_fenced(&tenant, &job_id, "builder-r3", 1)
        .await
        .expect("renewal"));
    let reason = jobs.stop_reason(&fence).await.expect("reason");
    assert_eq!(reason, Some(StopReason::Cancelled));
    let stopped = Instant::now();
    signal.stop(StopReason::Cancelled);

    let outcome = tokio::time::timeout(Duration::from_secs(10), build)
        .await
        .expect("the build returned promptly; its builder slot is free")
        .expect("build task");
    let took = stopped.elapsed();
    let err = outcome.expect_err("a cancelled build does not succeed");
    assert!(err.to_string().contains(STALE_GENERATION_MESSAGE), "{err}");
    assert!(took < Duration::from_secs(5), "took {took:?}");

    // The child was cancelled, killed and reaped; no slot is held.
    let runs = runs.lock().expect("runs").clone();
    assert_eq!(runs.len(), 1, "one extraction");
    assert_eq!(
        runs[0].extracted.failure,
        Some(FailureClass::Cancelled),
        "{:?}",
        runs[0].extracted.note
    );
    assert_reaped(runs[0].report.child_pid);
    assert_eq!(probe.running(), 0);

    // Nothing was committed, and the job stays cancelled.
    let history = jobs
        .version_history(&tenant, &JobId::parse(&job_id).expect("id"), None, 50)
        .await
        .expect("history")
        .expect("job");
    assert_eq!(history.job.state, "cancelled");
    assert_eq!(history.history, HistoryStatus::Recorded);
    assert!(history.versions.is_empty(), "{:?}", history.versions);
    let versions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM index_versions WHERE tenant_id = $1 AND collection_id = $2",
    )
    .bind(&tenant)
    .bind(&col.id)
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(versions, 0, "no index version committed");
}

// ---------------------------------------------------------------------------
// build policies: over-budget escalation and deferred documents (maintainer
// decision, 20 November 2026)
// ---------------------------------------------------------------------------

/// A document-intelligence provider that counts its calls.
#[derive(Default)]
struct CountingProvider {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl munarium_core::docintel::DocumentIntelligence for CountingProvider {
    fn supports(&self, media_type: &str) -> bool {
        media_type == PDF
    }
    async fn analyze(
        &self,
        _media_type: &str,
        _bytes: &[u8],
    ) -> munarium_core::Result<munarium_core::docintel::AnalyzedDocument> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(munarium_core::docintel::AnalyzedDocument {
            text: "recovered by the counting analyzer".into(),
            pages_analyzed: 1,
            provider_fingerprint: "counting/v1".into(),
        })
    }
    fn id(&self) -> &'static str {
        "counting"
    }
}

impl CountingProvider {
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Log lines written while the returned guard is held, on this thread.
#[derive(Clone, Default)]
struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("logs").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn capture(&self) -> tracing::subscriber::DefaultGuard {
        let writer = self.clone();
        tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_max_level(tracing::Level::WARN)
                .with_writer(move || writer.clone())
                .finish(),
        )
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("logs")).into_owned()
    }
}

/// A tenant with one collection holding one PDF, built through `extractor`
/// under `policy`, with a counting provider attached.
struct PolicyFixture {
    pool: sqlx::PgPool,
    tenant: String,
    pg: munarium_retrieval_pg::PgRetrieval,
    provider: Arc<CountingProvider>,
    collection_id: String,
    source_id: String,
}

impl PolicyFixture {
    async fn new(
        url: &str,
        extractor: Arc<IsolatedPdfExtractor>,
        policy: IsolatedPdfPolicy,
        pdf: &[u8],
    ) -> Self {
        let tenant = format!("tenant-r3-policy-{}", uuid::Uuid::new_v4().simple());
        let store = munarium_store_pg::PgStore::connect(url, &tenant)
            .await
            .expect("connect");
        let pool = store.pool().clone();
        let registry =
            munarium_extract::ExtractorRegistry::with_extractors(vec![Box::new(Probe {
                inner: extractor,
                runs: Arc::default(),
            })])
            .with_isolated_policy(policy);
        let provider = Arc::new(CountingProvider::default());
        let pg = munarium_retrieval_pg::PgRetrieval::new(pool.clone(), &tenant)
            .with_extractors(Arc::new(registry))
            .with_doc_intel(Some(
                Arc::clone(&provider) as Arc<dyn munarium_core::docintel::DocumentIntelligence>
            ));
        let col = pg
            .ensure_collection(
                &format!("col-r3-policy-{}", uuid::Uuid::new_v4().simple()),
                "para",
                0,
                &[],
                Some("R3 build policy"),
            )
            .await
            .expect("collection");
        let (source_id, _, _) = pg
            .put_source("", PDF, "r3/policy.pdf", Some("para"), pdf)
            .await
            .expect("source");
        pg.bind_source(&col.id, &source_id, None)
            .await
            .expect("bind");
        Self {
            pool,
            tenant,
            pg,
            provider,
            collection_id: col.id,
            source_id,
        }
    }

    async fn status(&self) -> String {
        sqlx::query_scalar(
            "SELECT extraction_status FROM sources WHERE tenant_id = $1 AND source_id = $2",
        )
        .bind(&self.tenant)
        .bind(&self.source_id)
        .fetch_one(&self.pool)
        .await
        .expect("status")
    }

    async fn versions(&self) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM index_versions WHERE tenant_id = $1 AND collection_id = $2",
        )
        .bind(&self.tenant)
        .bind(&self.collection_id)
        .fetch_one(&self.pool)
        .await
        .expect("count")
    }

    async fn chunks(&self, index_id: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM collection_chunks WHERE tenant_id = $1 AND index_version_id = $2",
        )
        .bind(&self.tenant)
        .bind(index_id)
        .fetch_one(&self.pool)
        .await
        .expect("chunks")
    }
}

/// A one-slot extractor: a document arriving while the slot is held is
/// deferred after 50 ms. A slow document holds the slot for up to 10 s.
fn one_slot_extractor() -> Arc<IsolatedPdfExtractor> {
    let mut limits = generous();
    limits.max_concurrent = 1;
    limits.queue_wait = Duration::from_millis(50);
    limits.wall_clock = Duration::from_secs(10);
    Arc::new(extractor(limits))
}

/// Hold `x`'s slot with a slow document. Returns once the child runs; the
/// thread finishes when the slot is free again. Called after the fixture is
/// set up, so database setup time cannot outlast the hold.
fn occupy(x: &Arc<IsolatedPdfExtractor>) -> std::thread::JoinHandle<()> {
    let busy = {
        let x = Arc::clone(x);
        let heavy = heavy_pdf(2_000_000);
        std::thread::spawn(move || {
            let run = x.run(&heavy, &Cancellation::new());
            assert_reaped(run.report.child_pid);
        })
    };
    let waited = Instant::now();
    while x.running() == 0 {
        assert!(
            waited.elapsed() < Duration::from_secs(10),
            "child never admitted"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    busy
}

fn database_url() -> Option<String> {
    let url = std::env::var("MUNARIUM_TEST_DATABASE_URL").ok();
    if url.is_none() {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
    }
    url
}

/// An over-budget document (here the output cap) is not escalated to a
/// configured provider by default: zero provider calls, `over_budget` on the
/// source and a warn line naming the bound but no document text. With
/// `MUNARIUM_PDF_EXTRACTION_ESCALATE_OVER_BUDGET=true` the same document
/// reaches the provider once and is indexed from its text. Needs
/// PostgreSQL; skips loudly without `MUNARIUM_TEST_DATABASE_URL`.
#[tokio::test]
async fn over_budget_documents_escalate_only_when_the_setting_enables_it() {
    let Some(url) = database_url() else {
        return;
    };
    // The cap is enforced with a 4 KiB frame allowance, so the document's
    // text must exceed both (as in `the_output_cap_refuses_oversized_text`).
    let mut limits = generous();
    limits.max_output_bytes = 4 * 1024;
    let pdf = text_pdf(400);

    let logs = Logs::default();
    let f = {
        let _capture = logs.capture();
        let f = PolicyFixture::new(
            &url,
            Arc::new(extractor(limits)),
            IsolatedPdfPolicy::default(),
            &pdf,
        )
        .await;
        f.pg.build_collection_index(&f.collection_id, 400, 1, true)
            .await
            .expect("the build succeeds without the document");
        f
    };
    assert_eq!(f.provider.calls(), 0, "no paid call by default");
    assert_eq!(f.status().await, "over_budget");
    let text = logs.text();
    let line = text
        .lines()
        .find(|l| l.contains("over-budget document not escalated"))
        .unwrap_or_else(|| panic!("no refusal logged:\n{text}"));
    assert!(line.contains("WARN"), "{line}");
    assert!(line.contains("budget=\"output\""), "{line}");
    assert!(line.contains("4096 byte output cap"), "{line}");
    assert!(!text.contains("quarterly settlement"), "no document text");

    let enabled = IsolatedPdfPolicy {
        escalate_over_budget: true,
        ..Default::default()
    };
    let f = PolicyFixture::new(&url, Arc::new(extractor(limits)), enabled, &pdf).await;
    let version =
        f.pg.build_collection_index(&f.collection_id, 400, 1, true)
            .await
            .expect("build");
    assert_eq!(f.provider.calls(), 1, "escalated once when enabled");
    assert_eq!(f.status().await, "ok");
    assert!(f.chunks(&version.id).await > 0, "indexed from the provider");
}

/// By default a deferred document is skipped: the build commits without it,
/// the source reads `deferred`, a warn line counts the skip, and the
/// provider is never called. A rebuild of the same inputs then extracts and
/// indexes it under a new version. Needs PostgreSQL.
#[tokio::test]
async fn deferred_documents_are_skipped_by_default_and_a_rebuild_picks_them_up() {
    let Some(url) = database_url() else {
        return;
    };
    let x = one_slot_extractor();
    let logs = Logs::default();
    let f = PolicyFixture::new(
        &url,
        Arc::clone(&x),
        IsolatedPdfPolicy::default(),
        &text_pdf(5),
    )
    .await;
    let busy = occupy(&x);
    let first = {
        let _capture = logs.capture();
        f.pg.build_collection_index(&f.collection_id, 400, 1, true)
            .await
            .expect("a deferred document does not fail the build by default")
    };
    assert_eq!(f.status().await, "deferred");
    assert_eq!(f.chunks(&first.id).await, 0, "the document was left out");
    assert_eq!(
        first.manifest["deferred_sources"],
        serde_json::json!([f.source_id]),
        "the version records what it skipped"
    );
    assert_eq!(f.provider.calls(), 0, "a deferred document never escalates");
    let text = logs.text();
    let line = text
        .lines()
        .find(|l| l.contains("build skipped 1 deferred document(s)"))
        .unwrap_or_else(|| panic!("no skip logged:\n{text}"));
    assert!(line.contains("WARN"), "{line}");
    assert!(line.contains("a rebuild picks it up"), "{line}");
    assert!(!line.contains(&f.source_id), "counts only: {line}");

    busy.join().expect("busy");
    let rebuilt =
        f.pg.build_collection_index(&f.collection_id, 400, 2, true)
            .await
            .expect("rebuild");
    assert_ne!(rebuilt.id, first.id, "the incomplete version is not reused");
    assert_eq!(f.status().await, "ok");
    assert!(f.chunks(&rebuilt.id).await > 0, "the rebuild indexed it");
    assert!(rebuilt.manifest.get("deferred_sources").is_none());
    // A complete version is reused as before.
    let again =
        f.pg.build_collection_index(&f.collection_id, 400, 3, true)
            .await
            .expect("idempotent");
    assert_eq!(again.id, rebuilt.id);
    assert_eq!(f.versions().await, 2);
}

/// With `MUNARIUM_PDF_EXTRACTION_DEFERRED=fail`, a deferred document fails
/// the build with the typed, safe capacity error and nothing is committed,
/// on the synchronous build and on the prepared (queued) path. Needs
/// PostgreSQL.
#[tokio::test]
async fn deferred_documents_fail_the_build_when_set_to_fail() {
    let Some(url) = database_url() else {
        return;
    };
    let x = one_slot_extractor();
    let fail = IsolatedPdfPolicy {
        deferred: DeferredPolicy::Fail,
        ..Default::default()
    };
    let f = PolicyFixture::new(&url, Arc::clone(&x), fail, &text_pdf(5)).await;
    let busy = occupy(&x);
    let err =
        f.pg.build_collection_index(&f.collection_id, 400, 1, true)
            .await
            .expect_err("a deferred document fails the build");
    let munarium_core::KernelError::RateLimited(message) = &err else {
        panic!("a capacity refusal is RateLimited, got {err:?}");
    };
    assert_eq!(message, munarium_retrieval_pg::DEFERRED_BUILD_MESSAGE);
    assert!(!message.contains(&f.source_id), "{message}");
    assert_eq!(f.versions().await, 0, "nothing committed");
    let Err(err) =
        f.pg.extract_collection_prepared(&f.collection_id, 400)
            .await
    else {
        panic!("the prepared build fails too");
    };
    assert!(
        matches!(err, munarium_core::KernelError::RateLimited(_)),
        "{err:?}"
    );
    assert_eq!(f.provider.calls(), 0);
    busy.join().expect("busy");
}

// ---------------------------------------------------------------------------
// measurement harness
// ---------------------------------------------------------------------------

/// CPU time (user + system) of this process's reaped children so far, from
/// `cutime` and `cstime` in `/proc/self/stat` (fields 16 and 17, in
/// `USER_HZ` = 100 ticks per second, so 10 ms resolution). The extractor
/// reaps each child before `run` returns, so the difference across one run
/// is that child's CPU time. Read from procfs, so no system-call binding.
#[cfg(target_os = "linux")]
fn children_cpu() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Field 2 (comm) may contain spaces; field 3 starts after its last ')'.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks = fields.get(13)?.parse::<u64>().ok()? + fields.get(14)?.parse::<u64>().ok()?;
    Some(Duration::from_millis(ticks * 10))
}

#[cfg(not(target_os = "linux"))]
fn children_cpu() -> Option<Duration> {
    None
}

fn outcome(run: &munarium_extract::IsolatedRun) -> String {
    match run.extracted.failure {
        Some(FailureClass::OverBudget(b)) => format!("over_budget({})", b.as_str()),
        Some(f) => f.as_str().to_string(),
        None => run.extracted.status.as_str().to_string(),
    }
}

/// The ceiling for the minimum-memory search.
const SEARCH_CEILING_MIB: u64 = 4096;

/// The smallest OS memory limit, to 1 MiB, under which `bytes` extracts with
/// the same outcome and text as under [`SEARCH_CEILING_MIB`], or `Err` with
/// the ceiling's outcome when even that does not extract. CPU and wall clock
/// are generous so that only memory decides. `None` without OS limits.
fn minimum_memory_mib(bytes: &[u8]) -> Option<Result<u64, String>> {
    if munarium_extract::isolated::os_limits() == "none" {
        return None;
    }
    let at = |mib: u64| {
        let limits = IsolatedPdfLimits {
            memory_bytes: mib * MIB,
            cpu_seconds: 300,
            wall_clock: Duration::from_secs(600),
            max_output_bytes: 256 * MIB as usize,
            max_concurrent: 1,
            queue_wait: Duration::from_secs(1),
        };
        extractor(limits).run(bytes, &Cancellation::new())
    };
    let reference = at(SEARCH_CEILING_MIB);
    if reference.extracted.failure.is_some() {
        return Some(Err(outcome(&reference)));
    }
    let same = |run: &munarium_extract::IsolatedRun| {
        run.extracted.failure.is_none()
            && run.extracted.status == reference.extracted.status
            && run.extracted.text == reference.extracted.text
    };
    // `lo` fails (or is below the 16 MiB floor or the input size), `hi` works.
    let mut lo = (bytes.len() as u64 / MIB).max(15);
    let mut hi = SEARCH_CEILING_MIB;
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if same(&at(mid)) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(Ok(hi))
}

/// Records CPU, the memory each document needs, elapsed time, output size
/// and the overhead over inline extraction for representative normal and
/// near-limit documents under the provisional limits, plus admission and the
/// effect of running children on in-process work. Prints Markdown tables.
#[test]
#[ignore = "measurement harness; run explicitly in release"]
fn measure() {
    let limits = IsolatedPdfLimits::PROVISIONAL;
    let x = extractor(limits);
    let mut cases: Vec<(String, Vec<u8>)> = vec![
        ("empty text layer".into(), pdf_with_content("")),
        ("1 page, 40 lines".into(), text_pdf(40)),
        ("text, 2k lines".into(), text_pdf(2_000)),
        ("text, 20k lines".into(), text_pdf(20_000)),
        ("heavy, 250k ops".into(), heavy_pdf(250_000)),
        ("heavy, 1M ops".into(), heavy_pdf(1_000_000)),
        ("heavy, 4M ops".into(), heavy_pdf(4_000_000)),
        ("heavy, 8M ops".into(), heavy_pdf(8_000_000)),
    ];
    if let Some(dir) = std::env::var_os("MUNARIUM_PDF_MEASURE_DIR") {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("MUNARIUM_PDF_MEASURE_DIR is readable")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")))
            .collect();
        files.sort();
        for f in files {
            let name = f.file_name().expect("name").to_string_lossy().into_owned();
            cases.push((name, std::fs::read(&f).expect("read pdf")));
        }
    }
    println!(
        "os_limits={} limits={limits:?}",
        munarium_extract::isolated::os_limits()
    );
    println!(
        "| document | input bytes | outcome | inline ms | isolated ms | overhead ms | child CPU ms | minimum memory limit MiB | output bytes |"
    );
    println!("|---|---:|---|---:|---:|---:|---:|---:|---:|");
    for (name, bytes) in &cases {
        let (inline_ms, inline_text) = {
            let t = Instant::now();
            let r = PdfTextExtractor.extract(bytes);
            (
                t.elapsed().as_secs_f64() * 1000.0,
                r.ok().map(|e| e.text.len()),
            )
        };
        let cpu_before = children_cpu();
        let run = x.run(bytes, &Cancellation::new());
        let cpu = match (cpu_before, children_cpu()) {
            (Some(a), Some(b)) => Some(b.saturating_sub(a)),
            _ => run.report.cpu,
        };
        let iso_ms = run.report.elapsed.as_secs_f64() * 1000.0;
        if run.extracted.failure.is_none() {
            assert_eq!(Some(run.extracted.text.len()), inline_text, "{name}");
        }
        let overhead = if run.extracted.failure.is_none() {
            format!("{:.1}", iso_ms - inline_ms)
        } else {
            // Refused early while the inline parse ran to the end.
            "n/a".into()
        };
        let minimum = match minimum_memory_mib(bytes) {
            None => "n/a".into(),
            Some(Ok(mib)) => mib.to_string(),
            Some(Err(o)) => format!("> {SEARCH_CEILING_MIB} ({o})"),
        };
        println!(
            "| {name} | {} | {} | {inline_ms:.1} | {iso_ms:.1} | {overhead} | {} | {minimum} | {} |",
            bytes.len(),
            outcome(&run),
            cpu.map(|c| format!("{:.0}", c.as_secs_f64() * 1000.0))
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

    // Service overhead: the latency of a small in-process parse (a stand-in
    // for request work on the replica) while idle and while the cap's
    // children parse continuously.
    let probe = text_pdf(40);
    let sample = |label: &str| {
        let mut ms: Vec<f64> = (0..400)
            .map(|_| {
                let t = Instant::now();
                PdfTextExtractor.extract(&probe).expect("probe");
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        println!(
            "service probe, {label}: p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms",
            ms[ms.len() / 2],
            ms[ms.len() * 95 / 100],
            ms[ms.len() * 99 / 100]
        );
    };
    sample("idle");
    for (label, doc) in [
        ("2 children on text, 20k lines", text_pdf(20_000)),
        ("2 children on heavy, 250k ops", heavy_pdf(250_000)),
    ] {
        let x = Arc::new(extractor(limits));
        let doc = Arc::new(doc);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let load: Vec<_> = (0..limits.max_concurrent)
            .map(|_| {
                let (x, doc, stop) = (Arc::clone(&x), Arc::clone(&doc), Arc::clone(&stop));
                std::thread::spawn(move || {
                    let mut runs = 0usize;
                    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                        x.run(&doc, &Cancellation::new());
                        runs += 1;
                    }
                    runs
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(200));
        sample(label);
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let runs: usize = load.into_iter().map(|h| h.join().expect("load")).sum();
        println!("  (background children completed: {runs})");
    }
}
