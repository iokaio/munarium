# Deterministic governance baseline

`MemStore::new()` and `Default` retain production UUID v4 IDs with their existing
prefixes. `MemStore::with_id_generator` accepts a thread-safe generator for
versions, single/batch claims, anchors and promises. Injected generators must
return unique IDs within the store. Fixed IDs make sequential fixtures replayable;
they do not fix the scheduling order of concurrent callers.

`MemBudgetStore::with_dependencies` accepts a UTC clock and an ID generator.
Its existing constructors retain production UTC time and UUID v4 IDs. Reservation
day and creation time come from one clock sample under the reservation lock.
Stale sweeping retains whole-second, inclusive age boundaries. A backward wall
clock delays expiry; it does not refund spend or move an existing reservation to
a new day. Settlement after midnight still belongs to the original day. APIs
already receiving timestamps from their callers do not acquire another clock.

## Reproduction

From `server/`:

```powershell
cargo test -p munarium-store-mem
cargo test -p munarium-server deterministic_governance_trace
cargo test -p munarium-server --release governance_baseline -- --ignored --nocapture
```

The last command is an explicitly selected measurement, not a CI timing gate.
It requires no database, credentials or provider. Use the repository release
profile: optimization level 3, thin LTO, one codegen unit. Keep the host otherwise
idle for comparison; collect compiler, OS and CPU identity alongside the emitted
JSON. The output includes fixture/source and Cargo.lock SHA-256 identities and
all three repetitions; retain them when comparing a later implementation.

## Workloads and stages

The fixed corpus has 64, 256 or 1,024 claims across four lineage versions and eight
scopes. Approximately 10% are corrections, 10% are disputed and there is one
anchor per ten claims. IDs, ordering and values are deterministic. A candidate
conflicts with a known anchor, so the gate stage exercises actual findings.
The fixture test also sends that candidate through the shared command service,
checks that it is recorded disputed, and compares repeated traces byte for byte.

Each size has three repetitions. Setup is outside timed regions; gate and
serialization stages are warmed once, and the snapshot used for gate evaluation
is loaded before timing. Stages are:

| Stage | Timed work |
|---|---|
| Gate | `run_gates` against an existing snapshot; average of 100 calls |
| Snapshot | `load_snapshot` at a fixed sequence pin, including resolution and digest rebuilding; average of 10 calls |
| Serialization | JSON serialization of snapshot fields in an explicit tuple; average of 10 calls |
| Raw growing writes | N single-claim appends through `MemStore`, starting empty; total duration |
| Governed growing writes | N calls through the actual shared `service::append_events` path, starting empty, including head/snapshot/gates/append/findings; total duration |

The growing workloads use unique accepted facts, one version, no anchors or
corrections, and one sequential writer. They expose history-growth costs without
mixing fixture construction into the fixed-snapshot measurements. The governed
path loads an unpinned current snapshot; the separately measured read stage uses
a pinned snapshot, so subtracting the raw times is not an exact cost decomposition.
These are memory-backend measurements, not PostgreSQL, network, provider, load,
allocation or peak-memory measurements. They establish a baseline, not a performance
threshold or a reason to change gate semantics.

## Recorded run

Recorded on 2026-09-24 using Rust 1.98.1 (LLVM 22.1.8), the repository's unmodified
release profile, Windows 11 Pro 10.0.26200 and an Intel Core i7-10750H (6 cores,
12 logical processors, approximately 64 GiB RAM). The host was a shared workstation;
no competing build or test was deliberately running during sampling.

[Raw repetitions and source/dependency identities](governance-baseline.json) retain
all samples. Values below are medians of the three repetitions. The first three
columns are microseconds per operation; growing-write columns are milliseconds
for all N writes, not per-write latency.

| Claims / N writes | Gate (µs) | Snapshot (µs) | Serialization (µs) | Raw growing total (ms) | Governed growing total (ms) |
|---|---|---|---|---|---|
| 64 | 19.249 | 215.120 | 24.080 | 0.086 | 7.172 |
| 256 | 68.135 | 785.140 | 103.430 | 0.365 | 99.024 |
| 1024 | 275.701 | 2529.460 | 351.120 | 1.902 | 1458.510 |

These observations separate the stage costs on one host. Three repetitions do
not calibrate a regression threshold. Retain the raw variation and workload
identities before drawing conclusions about another backend or machine.
