# Isolated PDF extraction: provisional measurements (B4)

Recorded 19 November 2026 for [R3/B4](airk-analysis.md#b4). These numbers
describe one developer machine and synthetic documents. **The production limits
are not frozen.** B4-T1 requires review of results from the supported
platforms, including Linux under the distroless musl image, before that
happens.

**Mechanism change (20 November 2026).** These rows were recorded on Windows
under a Job Object (`os=job`). By maintainer decision Munarium runs only on
Linux in production and the server carries no `unsafe` code, so the Job
Object binding has been removed. Windows builds are now for development and
testing only: they apply no OS memory or CPU limit, their identity records
`os=none`, and the wall-clock deadline, output cap, input-size check and
admission cap are the only bounds there. The harness on Windows therefore no
longer reproduces the `over_budget (memory)` rows below and prints `n/a` for
child CPU and peak memory. The rows remain as the record of what the limits
were chosen from; the production limits must be measured on Linux
(`os=rlimit`).

## What was measured

The harness is the ignored test `measure` in
[`munarium-server/tests/isolated_pdf.rs`](../../src/munarium-server/tests/isolated_pdf.rs).
It drives the real server binary through `IsolatedPdfExtractor` under
`IsolatedPdfLimits::PROVISIONAL` and prints one row per document:

```console
cd server
cargo test --release -p munarium-server --test isolated_pdf measure -- --ignored --nocapture
```

Every PDF is generated in the test; no binary fixture is committed.

- **text, N lines**: one page of `N` lines of prose, the normal shape.
- **heavy, N ops**: one page whose content stream holds `N` graphics-state
  operators and one short line of text. The parser materializes every
  operator, so memory and time grow with `N`. This is the near-limit and
  pathological shape.

Columns:

- **inline ms**: `pdf-text@1` in the harness process, for comparison.
- **isolated ms**: from spawn to reap, including the pipes.
- **overhead ms**: the difference between the two.
- **child CPU ms** and **peak memory MiB**: read from the Job Object after the
  child exits. Peak memory is the child's peak committed private memory.
  Neither is observed on any platform since the Job Object was removed, and
  the harness prints `n/a`.
- **output bytes**: the bytes read from the child's stdout, frame included.

## Environment

| Item | Value |
|---|---|
| CPU | Intel Core i7-10750H, 6 cores, 12 logical processors, 2.60 GHz |
| Memory | 64 GiB |
| OS | Windows 11 Pro (10.0.26300), x86_64 MSVC build |
| Limits mechanism | Job Object (`os=job`), removed 20 November 2026 |
| Build | `--release` (thin LTO, one codegen unit), Rust 1.98.0 |
| Other load | Other Cargo builds were running on the machine |

## Results under the provisional limits

Limits: 512 MiB memory, 10 s CPU, 30 s wall clock, 64 MiB output, two
children, 120 s queue wait.

| Document | Input bytes | Outcome | Inline ms | Isolated ms | Overhead ms | Child CPU ms | Peak memory MiB | Output bytes |
|---|---:|---|---:|---:|---:|---:|---:|---:|
| empty text layer | 543 | empty | 0.9 | 729.9 | 729.1 | 31 | 1.5 | 101 |
| 1 page, 40 lines | 3,248 | ok | 1.1 | 24.4 | 23.3 | 16 | 1.6 | 2,364 |
| text, 2k lines | 137,472 | ok | 48.7 | 82.4 | 33.7 | 78 | 4.8 | 120,904 |
| text, 20k lines | 1,389,474 | ok | 543.0 | 922.9 | 379.9 | 797 | 35.7 | 1,228,904 |
| heavy, 250k ops | 3,750,599 | empty | 712.4 | 894.7 | 182.3 | 594 | 270.0 | 101 |
| heavy, 1M ops | 15,000,601 | over_budget (memory) | 1,630.8 | 720.8 | n/a | 469 | 512.0 | 0 |
| heavy, 4M ops | 60,000,601 | over_budget (memory) | 5,592.9 | 604.3 | n/a | 422 | 496.8 | 0 |
| heavy, 8M ops | 120,000,603 | over_budget (memory) | 18,564.0 | 319.2 | n/a | 188 | 460.2 | 0 |

Concurrency, with a 2,000-line document and a cap of two:

| Documents at once | Total ms | Longest queue wait ms | Peak running |
|---:|---:|---:|---:|
| 1 | 69.6 | 0.0 | 1 |
| 2 | 74.2 | 0.0 | 2 |
| 4 | 186.4 | 84.6 | 2 |

## Observations

- **Per-document overhead** for a normal document was about 20 to 35 ms on top
  of the parse. The first child of the run took about 730 ms. That figure
  includes the cold start of a large executable and is likely to include an
  antivirus scan on Windows. Later spawns of the same binary did not repeat
  it. The 20k-line document's 380 ms overhead includes copying 1.2 MB of text
  through a pipe and decoding the frame, and was measured under other load.
- **Memory is the binding limit for pathological documents.** The parser held
  about 270 MiB for a 3.75 MB content stream, roughly 70 times the input. A
  15 MB stream already passed 512 MiB. In process, the same documents took up
  to 18.6 s and several GiB in the harness process. In the child, each was
  refused within 0.7 s with `over_budget (memory)` and the server process was
  unaffected.
- **CPU and wall clock were not reached** by any document here at the
  provisional values; the memory limit ended the heavy documents first. The
  tests show each of them firing on its own with tighter limits.
- **The cap queued work as designed.** Four documents through two slots
  waited at most one parse length.
- A negative overhead in the original output means the isolated run was
  refused early while the inline parse ran to the end; it is shown as `n/a`.

## Not yet measured

- **Linux.** The `setrlimit` path (through the pinned `rlimit` 0.11.0 crate
  since 20 November 2026, with no `unsafe` code of ours) compiles and passes
  clippy for `x86_64-unknown-linux-musl`, and its tests pass in the Alpine
  builder container, but it was not measured on this machine. `RLIMIT_AS`
  counts address space, including the mapped executable, so the same number
  admits less work than the former Windows commit limit did. CPU time and peak
  memory are not reported on Linux yet. Linux is the only production platform,
  so these are the measurements that decide the limits.
- **The distroless image.** The child is the same executable with no new
  file, but it has not been exercised in the container.
- **Real-world PDFs.** Only synthetic documents were used. Representative
  public PDFs, such as long government reports and scanned filings, should
  be measured before the limits are frozen.
- **Service overhead under load**: the effect of concurrent children on
  request latency on a serving replica.

## Provisional defaults and what would change them

| Setting | Provisional default | Reading of these results |
|---|---|---|
| Memory | 512 MiB | Admitted every normal document with a wide margin (36 MiB peak for 1.2 MB of text). Measure Linux address space before lowering it. |
| CPU | 10 s | No normal document used 1 s. |
| Wall clock | 30 s | Three times the CPU limit, to cover I/O and scheduling delay. |
| Output | 64 MiB | About 50 times the largest normal output here. |
| Concurrency | 2 | Two children at 512 MiB each bound the extraction memory per replica at about 1 GiB. |
| Queue wait | 120 s | Long enough for the queue to drain at these parse times. A deferred document is skipped by default and a rebuild picks it up; `MUNARIUM_PDF_EXTRACTION_DEFERRED=fail` fails the build instead. |
