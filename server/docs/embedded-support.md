# Embedded library support

Decision D5 of [the implementation plan](lessons-from-vcp-impl.md#131-p15-choose-the-embedded-support-contract--r31),
taken in P15 (R31) on 2026-09-25: which Server crates are supported as Rust
libraries outside Server, what that support promises, and how it is qualified.
The wire contract, the `MUNARIUM_*` configuration contract and additive-only
migrations keep their existing 1.x policy; this page is only about Rust APIs.

## Crate classes

| Class | Crates | What is promised |
|---|---|---|
| Supported embedded library | `munarium-datastore` | The declared surface below follows Cargo semantic versioning. The minimum compiler, feature sets and dependency closure are qualified from an isolated consumer. Consumed as pinned source; not published to a registry. |
| Published wire crates | `munarium-proto`, `munarium-api-types` | Their existing crates.io publication and N/N−1 wire-compatibility policy ([wire compatibility](wire-compatibility.md), [client libraries](../../clients/README.md)). P15 does not change them. |
| Internal | every other workspace crate, including `munarium-core`, `munarium-access` and `munarium-store-mem` | Nothing beyond the source at a revision you pin. Rust APIs may change in any release. Wire compatibility does not imply a stable Rust API. Existing constructors and `Default` implementations are kept where practical, but not promised. |

The architecture calls the library "the product and the canonical SDK". Only
the datastore carries a Rust API promise; the other crates are the product's
implementation, and applications integrate through the wire contract and the
client libraries.

## The supported surface

The promise covers the items named in the isolated consumer's
[`src/lib.rs`](../tests/embedded-datastore/src/lib.rs), together with the
behaviour its test suite exercises through them:

- crate root: `Error` and `PreparedChunk`;
- `model` (the artifact contract types: `BuildSpec`, `ArtifactBuildPlan`,
  `ArtifactManifest` and their parts), `canonical::{canonical_bytes, canonical_sha256}`;
- `shard::{ShardWriter, OpenShard}` and the component path constants;
- `store::{ArtifactStore, LocalFileStore}`, `verify::{Limits, ReaderCapabilities}`;
- `vector::{Candidate, FlatVectorIndex, VectorIndex}`, `fusion::FusionWeights`;
- with `lexical-tantivy`: `lexical::{LexicalPlan, PlanTerm, Demotion}`;
- with `vector-diskann`: `vector_diskann::{DiskAnnVectorIndex, GraphParams}` and
  `shard::VECTOR_DISKANN_DATA`.

Other public items (the L1 cache in `hydrate`, `routing`, `diagnostics`,
`records`, `tokenizer`, `stopwords`) serve Server and are not promised. Removing
or renaming a promised item fails the consumer's build, so a breaking change is
visible in review. Such a change needs a major version, and because the
workspace shares one version, that means the whole Server release.

API baseline rules, from the first release that includes P15:

- `Error` is `#[non_exhaustive]`: a match on it outside the crate needs a
  wildcard arm, so a new refusal class is not a breaking change. This was
  itself a source change for any exhaustive matcher, made before the promise
  began.
- `PreparedChunk` stays constructible with a struct literal; its fields are
  frozen. New build inputs arrive as a new type.
- A new `ArtifactStore` method must have a default body.
- The on-disk artifact format is governed separately, by
  [the datastore contract](../contract/datastore/README.md) and its vectors.

## Feature sets

| Feature set | What it can do |
|---|---|
| `default` (`lexical-tantivy`, `vector-flat`, `artifact-file`) | Build, seal, publish, open, verify and query artifacts: lexical, exact vector and hybrid. |
| `default` + `vector-diskann` | Also seal and open approximate-vector artifacts. A reader without it refuses them. |
| `default` + `json-arbitrary-precision` | The same, with `serde_json`'s `arbitrary_precision`. Artifacts exchange with the default configuration in both directions. |
| no default features | Contract types, canonical hashing, manifest verification, flat vectors and fusion. It refuses to seal (every plan names a lexical engine) and to open any artifact carrying a lexical index, with `Error::Unsupported`. |

`vector-flat` and `artifact-file` are markers: flat vectors and `LocalFileStore`
always compile, and no code checks either feature.

## Minimum supported Rust and the lock

`rust-version = "1.92"` is declared on `munarium-datastore` only; no other
crate declares one. Server itself builds with the pinned toolchain in
[rust-toolchain.toml](../rust-toolchain.toml). That pin is not a minimum.

The value was measured, not assumed:

- The source's own floor is 1.88: `<[T]>::as_chunks` in the vector readers,
  found by clippy's `incompatible_msrv`.
- The declared dependency floor of every set with the lexical engine is 1.88.0
  (`time` 0.3.55, through Tantivy). The no-default set's is 1.77.
- `diskann` 0.56.0 and `diskann-wide` declare no `rust-version` (edition 2024),
  and need more:
  - On 1.88.0, `diskann-wide`'s AVX-512 intrinsics are still unstable.
  - On 1.89.0, 1.90.0 and 1.91.0, `diskann` itself fails to compile with
    lifetime and `Send`-generality errors.
  - 1.92.0 compiles it.
- On 1.92.0 all four feature sets build and pass their tests. On 1.88.0 the
  no-default, default and `json-arbitrary-precision` sets also pass.

Cargo's `rust-version` cannot differ by feature, so the declared value is the
maximum across the supported sets. A consumer that never enables
`vector-diskann` measured fine on 1.88.0, but that is recorded evidence, not a
promise, and needs `--ignore-rust-version`.

The claim is exact: this source, with the consumer's committed
[Cargo.lock](../tests/embedded-datastore/Cargo.lock) (resolved fresh against
crates.io on 2026-09-25), in the four feature sets, builds and passes on 1.92.0.
A fresh resolution may select newer dependencies that need a newer compiler.
An older toolchain can use Cargo's `incompatible-rust-versions = "fallback"`
(Cargo 1.84+) or the consumer lock's versions. Clippy's `incompatible_msrv`
lint in the existing workspace checks now holds the datastore's source to the
declared value.

## Dependency closure

In every supported set, a consumer's normal dependency graph contains no Server
crate (`munarium-*` other than the datastore), and no `sqlx`, `axum`, `tonic`,
`reqwest`, `utoipa` or `openssl-sys`. With `vector-diskann`, the `diskann`
crates bring `tokio` into the graph. The consumer's `serde_json` has no
`raw_value`, which a Server build unifies in from `munarium-api-types`. It has
`arbitrary_precision` only in that set. The workspace's own datastore boundary
check (`server-ci.yml`, `gates.ps1`) still runs; the consumer checks a real
standalone resolution rather than a slice of the workspace graph.

## Consuming it

Depend on the source at a tag or revision you pin, for example:

```toml
[dependencies]
munarium-datastore = { git = "https://github.com/iokaio/munarium", tag = "<server release tag>", default-features = false, features = ["lexical-tantivy"] }
```

or vendor the `server/src/munarium-datastore` directory with the workspace
manifest it inherits from. Only path resolution is exercised by the qualifying
consumer; a git dependency resolves the same manifest but is not itself tested.
Registry publication, a lower minimum compiler, and support on Windows
AppContainer are separate decisions, not part of this tier. The P12
filesystem-permission qualification ([datastore guide](guides/datastore.md))
remains specific to the default Tantivy and flat-vector configuration on Linux.

## How the tier is qualified

The consumer at [server/tests/embedded-datastore](../tests/embedded-datastore/Cargo.toml)
is its own Cargo workspace, not a Server member. It includes the datastore's own
public-API integration tests (`round_trip`, `contract_vectors`,
`retrieval_characterization`) through `#[path]`, so the same assertions run
under the consumer's resolution. The notices generator skips it because it is
never shipped (`NOT_SHIPPED_WORKSPACES` in `tools/third_party_notices.py`).

```powershell
# from server/: every check below, with a validation receipt
pwsh tools/test-embedded-datastore.ps1
```

For each feature set the runner checks the dependency closure, the `serde_json`
feature graph, the tests on the pinned compiler with warnings denied, and the
tests on the declared minimum compiler. It then exchanges artifacts between the
Server workspace's resolution and the consumer's, in both directions and both
JSON configurations. Artifact ids are not compared, because the lexical index
is not byte-reproducible. A missing minimum toolchain is reported as not run
(exit 3), never as passed; install it with
`rustup toolchain install 1.92.0 --profile minimal`. `gates.ps1` includes the
same steps, and the `embedded-datastore` job in
[server-ci.yml](../../.github/workflows/server-ci.yml) mirrors them on Linux.
