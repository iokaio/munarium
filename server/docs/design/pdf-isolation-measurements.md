# Isolated PDF extraction: provisional measurements (B4)

Recorded 19 November 2026 (Windows) and 20 November 2026 (Linux) for
[R3/B4](airk-analysis.md#b4). The numbers describe one developer machine,
synthetic documents and eleven public PDFs. **The production limits are not
frozen.** B4-T1 freezes them only after the maintainer reviews these results;
the [Linux measurements](#linux-measurements-20-november-2026) are the ones that
decide them, and the Windows rows further down are kept as the record the
provisional values were chosen from.

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

## Linux measurements (20 November 2026)

**Environment.** The same machine as below, inside Docker Desktop's Linux VM
(WSL 2 kernel 5.15, 8 vCPUs and 16 GiB given to Docker), building and running
in the pinned `rust:1-alpine` builder image of the [Dockerfile](../../Dockerfile)
for `x86_64-unknown-linux-musl`, Rust 1.98.0, `--release`. Limits mechanism
`os=rlimit` (`RLIMIT_AS`, `RLIMIT_CPU`, `RLIMIT_CORE`) plus the non-dumpable
flag described below. The same test binaries then ran in the pinned
`distroless/static-debian12:nonroot` runtime image as uid 65532: all 16
`isolated_pdf` tests passed there (the three PostgreSQL-backed cases skip
without a database URL there; they passed against PostgreSQL in the main
suite).

**Harness additions.** On Linux the harness now reports the child's CPU time,
from the reaped-children counters in `/proc/self/stat` (10 ms resolution), and
the **minimum memory limit**: a binary search, to 1 MiB, for the smallest
`RLIMIT_AS` under which the document extracts the same text as under 4 GiB,
with CPU and wall clock relaxed so only memory decides. Address space, not
resident memory, is what the limit counts, so this is the number to size the
limit against. Set `MUNARIUM_PDF_MEASURE_DIR` to measure a directory of PDFs as
well as the synthetic set.

**Public documents.** Downloaded for the run into a scratch directory, read
only, not committed: NIST SP 800-53 Rev. 5 (492 pages), the NIST Cybersecurity
Framework 2.0, IRS Publication 17 and the Form 1040 instructions, the Federal
Register of 2 January 2024, 40 CFR volume 1 (2024), the U.S. Constitution
annotated edition (CDOC-110hdoc50), the EU AI Act as published in the Official
Journal, arXiv 1706.03762, and one scanned National Archives release with no
text layer (104-10003-10041).

### Results under the provisional limits

Second run, with the non-dumpable fix. Another Cargo build was running on the
machine at the same time, which inflates CPU times; the first run, on a quieter
machine, is given for CPU comparison (its over-budget elapsed times were
inflated by core dumps, see below).

| Document | Input bytes | Outcome | Inline ms | Isolated ms | Child CPU ms | CPU ms, quiet run | Minimum memory limit MiB | Output bytes |
|---|---:|---|---:|---:|---:|---:|---:|---:|
| empty text layer | 543 | empty | 0.2 | 5.7 | 0 | 0 | 43 | 101 |
| 1 page, 40 lines | 3,248 | ok | 1.8 | 5.8 | 10 | 0 | 43 | 2,364 |
| text, 2k lines | 137,472 | ok | 69.6 | 62.1 | 60 | 40 | 46 | 120,904 |
| text, 20k lines | 1,389,474 | ok | 680.5 | 699.4 | 690 | 340 | 75 | 1,228,904 |
| heavy, 250k ops | 3,750,599 | empty | 608.2 | 570.2 | 560 | 390 | 318 | 101 |
| heavy, 1M ops | 15,000,601 | over_budget (memory) | 1,280.0 | 458.3 | 440 | 1,000 | 1,144 | 0 |
| heavy, 4M ops | 60,000,601 | over_budget (memory) | 10,954.4 | 732.5 | 660 | 970 | > 4,096 | 0 |
| heavy, 8M ops | 120,000,603 | over_budget (memory) | 21,373.5 | 396.0 | 270 | 720 | > 4,096 | 0 |
| arXiv 1706.03762 | 2,215,244 | ok | 295.1 | 416.7 | 400 | 200 | 72 | 39,705 |
| 40 CFR vol. 1 (2024) | 3,574,113 | ok | 2,106.5 | 1,832.7 | 1,820 | 1,060 | 62 | 3,654,583 |
| EU AI Act (OJ) | 2,583,319 | ok | 659.1 | 600.5 | 600 | 460 | 63 | 651,465 |
| Federal Register, 2 Jan 2024 | 13,257,131 | ok | 1,397.3 | 1,128.9 | 1,110 | 800 | 126 | 1,436,541 |
| IRS Form 1040 instructions | 4,434,643 | ok | 2,301.3 | 2,495.3 | 2,480 | 1,550 | 238 | 661,681 |
| IRS Publication 17 | 3,088,632 | ok | 2,515.1 | 2,323.8 | 2,320 | 1,410 | 235 | 1,044,561 |
| NARA scan, no text layer | 59,643 | empty | 0.3 | 5.9 | 0 | 0 | 43 | 101 |
| NIST CSF 2.0 | 1,518,858 | ok | 164.6 | 164.8 | 160 | 120 | 52 | 69,613 |
| NIST SP 800-53 Rev. 5 | 6,073,678 | ok | 7,521.0 | 7,559.8 | 7,560 | 4,080 | 218 | 1,618,011 |
| U.S. Constitution (annotated) | 4,020,598 | ok | 150.6 | 154.3 | 140 | 120 | 56 | 306,809 |

Every successful isolated extraction produced exactly the inline text length.
Concurrency with a 2,000-line document and a cap of two: one document 67.8 ms,
two 62.9 ms with no queueing, four 140.6 ms with a longest queue wait of
67.7 ms and at most two children running.

Service probe (a 40-line inline parse in the parent, 400 samples, as a stand-in
for request work on the replica):

| Load | p50 ms | p95 ms | p99 ms |
|---|---:|---:|---:|
| idle | 1.82 | 2.91 | 3.77 |
| two children on a 20k-line document | 2.63 | 3.93 | 5.52 |
| two children on a 250k-operator document | 2.57 | 3.63 | 5.61 |

On the quieter first run the same probe moved from 2.19 to 2.49 ms at p95.

### Findings

- **Core dumps escaped `RLIMIT_CORE` (fixed).** In the first run a document
  refused for memory took 5.9 to 7.4 s of wall time for under 1 s of CPU.
  Docker Desktop's kernel pipes core dumps to a crash handler
  (`core_pattern` = `|/wsl-capture-crash …`), and for a piped pattern Linux
  ignores `RLIMIT_CORE`, so every child that reached its memory limit and
  aborted streamed its address space, tenant text included, to the host's
  handler. Production nodes commonly pipe dumps too (systemd-coredump stores
  them on the node's disk). The child now marks itself non-dumpable
  (`PR_SET_DUMPABLE` 0, through rustix's safe wrapper) before it reads input,
  which stops every dump whatever the pattern or `fs.suid_dumpable`; a Linux
  test checks the flag and each limit in a fresh process, and fails when the
  call is removed. After the fix the same refusals took 0.4 to 0.7 s.
- **Per-document overhead is about 5 ms** on Linux (spawn, pipes and frame),
  against 20 to 35 ms and a 730 ms first spawn on Windows. Differences larger
  than that in the table are noise between the inline and the isolated run,
  in either direction.
- **The address-space floor is 43 MiB**: the static executable and runtime
  mapped before any parsing. Normal text documents needed 46 to 75 MiB.
- **Real documents needed at most 238 MiB** (the IRS Form 1040 instructions),
  so 512 MiB leaves a margin of about 2.1 times. The synthetic 250k-operator
  document needed 318 MiB and the 1M-operator one 1,144 MiB; the second is
  refused at 512 MiB, as intended, in under a second.
- **CPU is the tightest real-world limit.** NIST SP 800-53 (492 pages) used
  4.1 s of CPU on the quiet run and 7.6 s with another build running, against
  the provisional 10 s. A longer document of the same kind, or a busier node,
  would be refused as `over_budget (cpu)`, and with the default no-escalation
  policy it would be indexed with no text.
- **Children barely affect in-process work** with two running on eight vCPUs:
  the probe's p95 moved by about 1 ms at most.
- **A scanned document without a text layer** extracts as `empty` in about
  6 ms, as inline does; OCR is a separate feature and was not measured.

### Proposed production limits (for review)

| Setting | Provisional | Proposed | Reason |
|---|---|---|---|
| Memory | 512 MiB | 512 MiB | Real documents peaked at 238 MiB of address space; two children stay within about 1 GiB per replica. |
| CPU | 10 s | 30 s | The largest real document used 7.6 s under load. 30 s gives about 4 times that while still stopping a pathological parse. |
| Wall clock | 30 s | 60 s | Twice the CPU limit, for scheduling delay on a busy node. |
| Output | 64 MiB | 64 MiB | The largest real output was 3.7 MB. |
| Concurrency | 2 | 2 | No measurable effect on the probe at two. |
| Queue wait | 120 s | 120 s | Still long enough to drain two queued parses at the proposed CPU limit. |

These limits stay provisional until the maintainer accepts them (B4-T1). The
measurements here come from one machine's Docker VM. They are not a Kubernetes
node with cgroup limits, and the service probe stands in for request work
rather than measuring REST latency.

## Windows measurements (19 November 2026)

### What was measured

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
  Windows no longer observes either since the Job Object was removed, and
  the harness prints `n/a` there; on Linux it reports CPU time and the
  minimum memory limit instead (see the Linux section).
- **output bytes**: the bytes read from the child's stdout, frame included.

### Environment

| Item | Value |
|---|---|
| CPU | Intel Core i7-10750H, 6 cores, 12 logical processors, 2.60 GHz |
| Memory | 64 GiB |
| OS | Windows 11 Pro (10.0.26300), x86_64 MSVC build |
| Limits mechanism | Job Object (`os=job`), removed 20 November 2026 |
| Build | `--release` (thin LTO, one codegen unit), Rust 1.98.0 |
| Other load | Other Cargo builds were running on the machine |

### Results under the provisional limits

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

### Observations

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

- **A Kubernetes node.** The Linux rows come from Docker Desktop's VM. The
  workstation qualification run ([cluster plan](cluster-qualification-plan.md))
  should repeat the harness in the candidate image on the cluster node,
  including under the pod's cgroup memory limit.
- **REST latency under extraction load.** The service probe measures in-process
  work, not served requests.
- **OCR.** Scanned documents need the `ocr` feature, which is outside this
  profile.

## Provisional defaults as chosen from the Windows run

| Setting | Provisional default | Reading of these results |
|---|---|---|
| Memory | 512 MiB | Admitted every normal document with a wide margin (36 MiB peak for 1.2 MB of text). Measure Linux address space before lowering it. |
| CPU | 10 s | No normal document used 1 s. |
| Wall clock | 30 s | Three times the CPU limit, to cover I/O and scheduling delay. |
| Output | 64 MiB | About 50 times the largest normal output here. |
| Concurrency | 2 | Two children at 512 MiB each bound the extraction memory per replica at about 1 GiB. |
| Queue wait | 120 s | Long enough for the queue to drain at these parse times. A deferred document is skipped by default and a rebuild picks it up; `MUNARIUM_PDF_EXTRACTION_DEFERRED=fail` fails the build instead. |
