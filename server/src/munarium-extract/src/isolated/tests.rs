// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the parts of the isolated profile that need no child
//! process. The bounds themselves are exercised against the real server
//! binary in `munarium-server/tests/isolated_pdf.rs`.

use super::*;
use std::sync::mpsc;
use std::thread;

fn limits() -> IsolatedPdfLimits {
    IsolatedPdfLimits::DEFAULT_PROFILE
}

fn exited(success: bool, code: Option<i32>, signal: Option<i32>) -> Observed {
    Observed::Exited {
        success,
        code,
        signal,
        display: format!("code {code:?} signal {signal:?}"),
    }
}

#[test]
fn default_profile_is_valid_and_bad_limits_are_refused() {
    assert_eq!(limits().validate(), Ok(()));
    let mut l = limits();
    l.memory_bytes = 1024;
    assert!(l.validate().is_err());
    let mut l = limits();
    l.cpu_seconds = 0;
    assert!(l.validate().is_err());
    let mut l = limits();
    l.max_concurrent = 0;
    assert!(l.validate().is_err());
    let mut l = limits();
    l.max_output_bytes = 0;
    assert!(l.validate().is_err());
}

#[test]
fn only_64_bit_linux_reports_os_limits() {
    // Production runs on Linux, where the child applies `setrlimit` to
    // itself. Every other build, Windows included, is for development and
    // testing only: it applies no OS limit and the identity says so.
    let expected = if cfg!(all(target_os = "linux", target_pointer_width = "64")) {
        "rlimit"
    } else {
        "none"
    };
    assert_eq!(os_limits(), expected);
    assert!(
        limits().identity().contains(&format!("{{os={expected};")),
        "{}",
        limits().identity()
    );
}

#[test]
fn identity_covers_every_output_changing_setting_but_not_scheduling() {
    let base = limits().identity();
    assert!(base.starts_with("pdf-text-isolated@1{"), "{base}");
    assert!(base.contains(&format!("os={}", os_limits())), "{base}");
    let mut changed = Vec::new();
    let mut l = limits();
    l.memory_bytes += 1;
    changed.push(l);
    let mut l = limits();
    l.cpu_seconds += 1;
    changed.push(l);
    let mut l = limits();
    l.wall_clock += Duration::from_millis(1);
    changed.push(l);
    let mut l = limits();
    l.max_output_bytes += 1;
    changed.push(l);
    for l in changed {
        assert_ne!(l.identity(), base, "{l:?}");
    }
    let mut l = limits();
    l.max_concurrent = 7;
    l.queue_wait = Duration::from_secs(1);
    assert_eq!(l.identity(), base);
    // The registry joins ids with commas; the identity must not add any.
    assert!(!base.contains(','), "{base}");
}

#[test]
fn frames_round_trip() {
    let frames = [
        Frame::Text {
            status: ExtractionStatus::Ok,
            note: None,
            text: "The quarterly settlement — approved".into(),
        },
        Frame::Text {
            status: ExtractionStatus::Empty,
            note: Some("pdf has no usable text layer".into()),
            text: String::new(),
        },
        Frame::Parser("pdf text extraction: bad xref".into()),
        Frame::OverMemory,
    ];
    for f in frames {
        assert_eq!(decode(&encode(&f)).expect("decode"), f);
    }
}

#[test]
fn damaged_frames_are_refused() {
    let good = encode(&Frame::Text {
        status: ExtractionStatus::Ok,
        note: None,
        text: "hello".into(),
    });
    assert!(decode(&[]).is_err());
    assert!(decode(b"XXXX").is_err());
    assert!(decode(&good[..good.len() - 1]).is_err());
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(decode(&trailing).is_err());
    let mut huge = good[..6].to_vec();
    huge.push(0);
    huge.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(decode(&huge).is_err());
}

#[test]
fn a_clean_exit_with_text_is_ok_and_identical() {
    let frame = encode(&Frame::Text {
        status: ExtractionStatus::Ok,
        note: None,
        text: "same text".into(),
    });
    let out = classify(&exited(true, Some(0), None), false, &frame, b"", &limits());
    assert_eq!(out.status, ExtractionStatus::Ok);
    assert_eq!(out.text, "same text");
    assert_eq!(out.failure, None);
    assert_eq!(out.method, ExtractionMethod::PdfTextLayer);
}

#[test]
fn a_parser_error_frame_is_a_parser_failure_not_over_budget() {
    let frame = encode(&Frame::Parser(
        "pdf text extraction: invalid file header".into(),
    ));
    let out = classify(&exited(true, Some(0), None), false, &frame, b"", &limits());
    assert_eq!(out.failure, Some(FailureClass::ParserFailed));
    assert_eq!(out.status_label(), "failed");
}

#[test]
fn an_abnormal_exit_without_a_memory_message_is_a_parser_failure() {
    let out = classify(
        &exited(false, Some(-1073741571), None),
        false,
        b"",
        b"thread 'main' has overflowed its stack\n",
        &limits(),
    );
    assert_eq!(out.failure, Some(FailureClass::ParserFailed));
}

#[test]
fn each_budget_is_classified() {
    let l = limits();
    let cases = [
        (
            classify(&Observed::Killed(Kill::Deadline), false, b"", b"", &l),
            Budget::WallClock,
        ),
        (
            classify(&Observed::Killed(Kill::Output), false, b"", b"", &l),
            Budget::Output,
        ),
        (
            classify(&exited(true, Some(0), None), true, b"", b"", &l),
            Budget::Output,
        ),
        (
            classify(
                &exited(false, Some(134), None),
                false,
                b"",
                b"memory allocation of 1073741824 bytes failed\n",
                &l,
            ),
            Budget::Memory,
        ),
        (
            classify(&exited(false, None, Some(SIGXCPU)), false, b"", b"", &l),
            Budget::Cpu,
        ),
        (
            classify(
                &exited(true, Some(0), None),
                false,
                &encode(&Frame::OverMemory),
                b"",
                &l,
            ),
            Budget::Memory,
        ),
    ];
    for (out, budget) in cases {
        assert_eq!(
            out.failure,
            Some(FailureClass::OverBudget(budget)),
            "{out:?}"
        );
        assert_eq!(out.status_label(), "over_budget");
        assert!(out.text.is_empty());
    }
}

#[test]
fn cancellation_and_broken_children_are_not_document_failures() {
    let l = limits();
    let c = classify(&Observed::Killed(Kill::Cancelled), false, b"", b"", &l);
    assert_eq!(c.failure, Some(FailureClass::Cancelled));
    let usage = classify(
        &exited(false, Some(EXIT_USAGE), None),
        false,
        b"",
        b"extraction child: --input-bytes is required",
        &l,
    );
    assert_eq!(usage.failure, Some(FailureClass::IsolationFailed));
    let garbage = classify(&exited(true, Some(0), None), false, b"oops", b"", &l);
    assert_eq!(garbage.failure, Some(FailureClass::IsolationFailed));
}

#[test]
fn child_arguments_parse_strictly() {
    let args: Vec<OsString> = ["--input-bytes=10", "--memory-bytes=20", "--cpu-seconds=3"]
        .iter()
        .map(OsString::from)
        .collect();
    assert_eq!(
        ChildArgs::parse(&args),
        Ok(ChildArgs {
            input_bytes: 10,
            memory_bytes: 20,
            cpu_seconds: 3
        })
    );
    assert!(ChildArgs::parse(&args[..2]).is_err());
    let extra: Vec<OsString> = vec!["--input-bytes=1".into(), "--shell=sh".into()];
    assert!(ChildArgs::parse(&extra).is_err());
    assert_eq!(run_child(&extra), EXIT_USAGE);
}

#[test]
fn a_document_larger_than_the_memory_limit_is_refused_without_a_child() {
    let mut l = limits();
    l.memory_bytes = 16 * MIB;
    // A program that does not exist: reaching spawn would be isolation_failed.
    let x = IsolatedPdfExtractor::new("/nonexistent/munarium-server".into(), l).expect("limits");
    let run = x.run(&vec![0u8; (16 * MIB) as usize], &Cancellation::new());
    assert_eq!(
        run.extracted.failure,
        Some(FailureClass::OverBudget(Budget::Input))
    );
    assert_eq!(run.report.child_pid, None);
}

#[test]
fn a_missing_child_binary_is_an_isolation_failure() {
    let x =
        IsolatedPdfExtractor::new("/nonexistent/munarium-server".into(), limits()).expect("limits");
    let run = x.run(b"%PDF-1.4", &Cancellation::new());
    assert_eq!(run.extracted.failure, Some(FailureClass::IsolationFailed));
    for escalate_over_budget in [false, true] {
        let policy = IsolatedPdfPolicy {
            escalate_over_budget,
            ..Default::default()
        };
        assert!(!run.extracted.escalation_eligible(&policy));
    }
    assert_eq!(x.running(), 0, "the permit is released");
}

#[test]
fn admission_caps_concurrency_and_defers_after_the_wait() {
    let a = Admission::new(2);
    let c = Cancellation::new();
    let p1 = a.acquire(Duration::ZERO, &c).expect("first");
    let p2 = a.acquire(Duration::ZERO, &c).expect("second");
    assert_eq!(a.running(), 2);
    let t = Instant::now();
    assert_eq!(
        a.acquire(Duration::from_millis(50), &c).err(),
        Some(Refused::Deferred)
    );
    assert!(t.elapsed() >= Duration::from_millis(50));
    drop(p1);
    assert_eq!(a.running(), 1);
    let p3 = a
        .acquire(Duration::ZERO, &c)
        .expect("a released slot is reusable");
    assert_eq!(a.peak(), 2);
    drop((p2, p3));
    assert_eq!(a.running(), 0);
}

#[test]
fn admission_wakes_a_waiter_when_a_slot_frees() {
    let a = Arc::new(Admission::new(1));
    let held = Arc::clone(&a);
    let (tx, rx) = mpsc::channel();
    let holder = thread::spawn(move || {
        let _p = held
            .acquire(Duration::ZERO, &Cancellation::new())
            .expect("hold");
        tx.send(()).expect("send");
        thread::sleep(Duration::from_millis(100));
    });
    rx.recv().expect("held");
    let p = a.acquire(Duration::from_secs(10), &Cancellation::new());
    assert!(p.is_ok(), "the waiter is admitted once the holder finishes");
    holder.join().expect("holder");
}

#[test]
fn admission_is_first_in_first_out() {
    let a = Arc::new(Admission::new(1));
    let first = a
        .acquire(Duration::ZERO, &Cancellation::new())
        .expect("hold");
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut waiters = Vec::new();
    for i in 0..3 {
        let waiter = Arc::clone(&a);
        let order = Arc::clone(&order);
        waiters.push(thread::spawn(move || {
            let _p = waiter
                .acquire(Duration::from_secs(10), &Cancellation::new())
                .expect("admitted");
            order.lock().expect("order").push(i);
        }));
        // Let each waiter take its ticket before the next one.
        while a.lock().waiting.len() < i + 1 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    drop(first);
    for w in waiters {
        w.join().expect("waiter");
    }
    assert_eq!(*order.lock().expect("order"), vec![0, 1, 2]);
}

#[test]
fn a_cancelled_waiter_leaves_the_queue_promptly() {
    let a = Arc::new(Admission::new(1));
    let _held = a
        .acquire(Duration::ZERO, &Cancellation::new())
        .expect("hold");
    let c = Cancellation::new();
    let c2 = c.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        c2.cancel();
    });
    let t = Instant::now();
    assert_eq!(
        a.acquire(Duration::from_secs(30), &c).err(),
        Some(Refused::Cancelled)
    );
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(a.lock().waiting.is_empty());
    canceller.join().expect("canceller");
}

#[test]
fn the_child_input_reader_refuses_an_impossible_allocation() {
    assert!(matches!(read_input(usize::MAX), Ok(None)));
}

#[test]
fn notes_are_truncated_on_a_char_boundary() {
    let long = "é".repeat(NOTE_CAP);
    let t = truncate(&long, NOTE_CAP);
    assert!(t.len() <= NOTE_CAP);
    assert!(t.chars().all(|c| c == 'é'));
}
