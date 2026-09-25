# Panic boundaries: production policy and P15 record

P15 (R32 in [the implementation plan](lessons-from-vcp-impl.md#132-incremental-panic-boundary-audit--r32))
inventoried every production `unwrap`, `expect`, `panic!`, `unreachable!`,
`todo!` and `unimplemented!` in the Server workspace at `4e79d69` (Server 1.2.1),
fixed each site by category, and then denied those shortcuts crate by crate. This
page is the policy contributors follow and the record of what changed. It is not
a claim that Server cannot panic: the lint cannot see indexing, arithmetic
overflow or allocation failure, and the limits of this work are listed below.

## The policy

Every production crate root in [the Server workspace](../Cargo.toml) carries:

```rust
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]
```

That is 18 library crates, the conformance library, and four binaries
(`munarium-server`, `mmctl`, `mmp-conformance` and `gen-grpc-docs`). The
existing clippy steps enforce it with no workflow change: `--all-targets` lints
each library and binary target without `cfg(test)`. Unit-test modules, `#[path]`
test modules, integration tests under `tests/`, build scripts and doctests are
exempt by construction. A plain `cargo build` ignores `clippy::` lints, so the
attribute changes nothing for anyone compiling these crates.

`expect_used` is denied alongside `unwrap_used` on purpose: replacing an
`unwrap` with an `expect` must not pass as safer error handling. A new crate
root without the attribute fails
[`panic_policy`](../src/munarium-server/src/panic_policy.rs), a test that reads
the workspace members and asserts the attribute in each root. It also checks
that at least 23 roots were found, so broken discovery cannot pass vacuously.

### Exemptions

An exemption is a scoped `#[expect(lint, reason = "...")]`, never `#[allow]`:
if the code stops needing it, `unfulfilled_lint_expectations` fails the build.
The register has two entries.

| Site | Why a panic is the right outcome |
|---|---|
| [`constant_regex`](../src/munarium-core/src/chrono_gate.rs) in core | Compiles the chronology grammar's ten fixed patterns. A failure is a programming error that the first `parse_when` test exposes, not a runtime condition. |
| [`http_client`](../src/munarium-providers/src/lib.rs) in providers | The builder fails only when the TLS backend cannot initialise, which means the binary is broken. Its callers are infallible public constructors, and a default client would silently drop the request timeouts. |

### Remedies by category

| Category | Rule |
|---|---|
| Untrusted bytes (artifact components) | Read through the bounds-checked readers in [`bytes.rs`](../src/munarium-datastore/src/bytes.rs): every range end is `checked_add`ed and a miss is `Error::Integrity`. Size an allocation from bytes already taken, never from a declared count. |
| Startup and process I/O | One `startup error:` line on stderr and exit status 1. Configuration errors keep exit 2. Tools print to stderr and exit non-zero. |
| Lock poisoning | Chosen per lock (table below). Recover the guard only when every critical section is one operation that cannot unwind part-way. Otherwise, where the enclosing function is already fallible, return a typed error. |
| Dates | Checked chrono arithmetic (`checked_add_days`, `Duration::try_days`, `checked_add_signed`), as in the chronology deadline code. |
| Guarded invariants | `let`-`else`, `match` or a restructure that carries the proof in the type, returning the enclosing function's existing error. |
| Serialization of known-good values | `error::to_json` in Server (a storage error), `map_err` elsewhere. Never a `null`, `{}` or empty-string default for a value that failed to serialize. |
| Operator tools | Exit through the tool's existing `die` with a message. |

Returning an empty or default value to hide a storage failure or corrupt
evidence is not a remedy. The silent defaults this work found are listed below.

## Lock decisions

`std::sync` locks are poisoned when a thread panics while holding a guard.
Tokio locks, which hold all ledger, evidence and budget state in the memory
store, do not poison and were not affected.

| Lock | Decision and argument |
|---|---|
| Datastore `L1Cache` state and its condvar | Recovered. Every critical section is a map or set operation, a clone, or a saturating sum. Resident bytes were summed with `sum()`, which could overflow while the lock was held. The in-flight guard now always releases its key, so waiters cannot hang behind a skipped cleanup. |
| Datastore DiskANN adjacency (per node) | Recovered. Each write is one clear/extend of a plain list. The public `ProviderError` enum is unchanged. |
| Retrieval `L0Cache` | Recovered. Map and deque operations only fail by aborting. At worst a key missing from the eviction order would outlive the count cap. |
| Server `Metrics` (every request) | Recovered. The maps are append-only, with atomic updates. The unpoisoned path is the same `Ok` branch `expect` took, so the hot path is unchanged. |
| Server datastore readiness diagnostics | Recovered. The list is replaced whole, and the admission bit is atomic. |
| Shapes registry and validation cache | Recovered. They hold settled `Arc<Shape>` values and cached outcomes. |
| Azure managed-identity token cache | Recovered. It is one `Option` replaced whole. |
| Memory-store source blobs | `put`, `get`, `exists` and `delete` return `KernelError::Storage` (fail closed). `len()` is a diagnostic and recovers. |
| Provider `RateBudget` | Refuses with a storage-class error (500), not a 429. An admission decision is not guessed after a peer panic. |

## Inventory and dispositions

Before P15: 103 sites in service and library crates and 43 in tooling, per
clippy with the policy enabled.

| Crate | Sites | Disposition |
|---|---|---|
| access, api-conv, api-types, authoring, docintel-az, extract, store-objects, proto (lib) | 0 | Attribute only. |
| core | 16 | Ten grammar patterns through `constant_regex`. `month_end` returns `Option` (a four-digit grammar cannot reach the panic today, so this is a refactor with a test). Two rule-gap unwraps use `Option::filter`. The composer's `unreachable!` becomes array destructuring. A governance profile that fails to serialize is a storage error. |
| store-mem | 6 | Source-store locks per the table above. The budget ledger skips released rows in a `match` arm instead of `unreachable!`, pinned by a new control. |
| store-pg | 1 | A claim origin that fails to serialize is a storage error, and the append transaction rolls back. |
| retrieval-pg | 1 | An outcome naming an unprepared source is a storage error, and the build transaction rolls back. |
| runbooks | 2 | `ok_or_else` with the existing single-key-map error; `Option::is_none_or` for build order. New controls pin both rules. |
| azure-auth | 2 | Cache recovered. The unflagged `SystemTime + Duration` overflow on an out-of-range `expires_in` returns the token uncached. |
| providers | 2 | `RateBudget` per the table above. Its sums are checked: a huge estimate overflowed under the lock, which panicked in debug builds and in release admitted the request under a tpm limit. The `http_client` exemption stays. |
| shapes | 5 | Recovered. |
| retrieval | 5 | L0 recovered. Activation without a plane returns the existing `DatastoreUnavailable` error. |
| datastore | 30 | Nine artifact reads through `bytes.rs`. Eleven L1 and four DiskANN lock sites recovered. Five fusion sites carry each member's measure with its index, keeping the historical comparison and accumulation order. One tokenizer slice pattern. |
| server | 33 | Startup (binds, reflection, signal handler) exits 1 through `startup_failure`. Serving tasks log a failure. Metrics and readiness recovered. Nine guarded invariants use explicit matches. Serialization goes through `error::to_json`. The Retry-After midnight is checked. |
| mmctl (tooling) | 37 | `pretty` and the client builders exit through `die`. |
| gen-grpc-docs (tooling) | 2 | Decode and write errors go to stderr with a non-zero exit. The generated reference is unchanged. |
| conformance (tooling) | 4 | The bearer token is parsed once in `connect` and returns `InvalidInput`. `request` is fallible. The tenant suffix is a UUIDv7. |

Panics the lint cannot see, fixed because untrusted input reaches them:

- The datastore lexical archive added a declared `u64` body length to its offset
  unchecked. That panicked on overflow in debug builds and on a slice with a
  wrapped end in release.
- The DiskANN reader reserved `count * dims` floats before checking that the
  bytes held them. That was a capacity-overflow panic, or an abort on a merely
  enormous reservation, and its `take` added offsets unchecked.
- The two overflows noted in the table above (Azure token lifetime and
  provider budget).

Both datastore paths are reachable by any embedder of the public
`FlatVectorIndex`/`DiskAnnVectorIndex::from_bytes` and lexical open APIs. In
Server they are reachable after a component hash check, so only a
hash-consistent crafted artifact triggers them.

## Silent defaults

Fixed, each now a typed error or an explicit failure:

- `verifyDataViews` read a 200 with an unparseable body, or no integer `failed`,
  as zero failed questions, and marked the view **verified**. It now fails with
  "malformed verify response". This was a real false pass, and it amends
  [dev-guide §13](guides/dev-guide.md#13-known-gaps-ledger-kept-current-deliberately-last)
  entry 23 (entry 29).
- Stored session-turn hits, envelopes and hierarchy were written as `null` or
  NULL when they failed to serialize. NULL on the hierarchy column claims that
  no profile ran.
- A turn whose result could not be rendered ended its SSE stream with `done`
  and data `{}`. It now sends an `error` event carrying a constant, valid
  storage-error problem. A progress event that cannot be rendered is skipped
  with a warning instead of sent as `{}`.
- Stored draft validations, runbook `models`, and the findings and answers placed
  in model prompts.
- `mmctl matrix` printed an empty line if a response could not be rendered.

Kept, because they are not evidence or storage outcomes: the dashboard's display
of a `Value` (which cannot fail) and `mmctl matrix`'s tolerant parse of bodies
that are legitimately empty.

## Regression evidence

Each behavioural fix has a test written before the fix, recorded failing on the
pre-fix code, with an adjacent valid control. The run results are in
[§13.4 of the implementation plan](lessons-from-vcp-impl.md#134-p15-implementation-and-local-qualification).

| Behaviour | Test |
|---|---|
| Crafted lexical length | `lexical::tests::a_declared_body_length_near_u64_max_is_an_integrity_error` (control: exact length accepted, one byte short refused) |
| Crafted DiskANN size | `vector_diskann::tests::a_header_that_declares_an_impossible_size_is_an_integrity_error` (control: an honest header fails the same way) |
| Malformed verify 200 | `runbooks_api::tests::a_malformed_200_verify_response_fails_the_step` (controls: the existing `failed: 0` and `failed: 1` tests) |
| Occupied ports | `tests/startup_failures.rs`: REST and gRPC occupied ports (control: free ports reach "REST plane listening") |
| Poisoned locks | One test per recovering or refusing lock: L1, L0, metrics, readiness, shapes, token cache, memory source store, rate budget |
| Budget overflow | `budget_arithmetic_cannot_overflow_into_a_bypass`, debug and release (control: tokens equal to tpm accepted, one more refused) |
| Token lifetime overflow | `an_unrepresentable_token_lifetime_is_not_a_panic` |
| Serialization defaults | `to_json_reports_a_serialization_failure_as_a_storage_error`, `an_unrenderable_turn_result_ends_the_stream_with_an_error_not_an_empty_done` |
| Dates | `month_end_is_none_outside_the_calendar_instead_of_panicking`, `seconds_to_midnight_is_positive_and_does_not_panic_at_the_calendar_end` |

## Adding or changing a crate

1. Add the attribute to each new crate root. `panic_policy` fails until you do.
2. Fix sites by the rules above rather than adding exemptions. If an exemption
   is truly needed, add it as `#[expect(..., reason = "...")]` and a row in the
   exemption register.
3. Check that the lint is live. Seed a non-test `.unwrap()` in the crate and
   run `cargo clippy -p <crate> --all-targets -- -D warnings`: it must fail.
   The same line inside `#[cfg(test)]` or under `tests/` must pass. Revert the
   seed and confirm with `git diff --exit-code`.

## Remaining boundaries

- The lint does not cover slice indexing, integer overflow in general, or
  allocation. This work fixed those where untrusted input reaches them, but did
  not audit them exhaustively.
- A serving task that fails after startup is logged, and the process keeps
  running without that plane, as it did when the panic ended only that task.
  This is recorded as open gap 30 in the
  [dev-guide ledger](guides/dev-guide.md#13-known-gaps-ledger-kept-current-deliberately-last).
- The DiskANN adjacency recovery has no poisoning test: the store is owned by
  the `diskann` index and cannot be reached to poison from a unit test. It uses
  the same idiom as the tested locks.
- The Azure token source still builds its HTTP client with
  `unwrap_or_default()`, which drops the ten-second timeout if the TLS backend
  cannot initialise. That is not a panic, and its constructor is infallible; it
  is left as recorded.
- Tolerant `if let Ok(guard) = lock()` sites in Server middleware and session
  streaming skip their bookkeeping when a lock is poisoned. They do not panic
  and were left unchanged.
