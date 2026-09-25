// SPDX-License-Identifier: Apache-2.0
//! The declared public surface of munarium-datastore's supported embedded tier.
//!
//! Every item below is covered by the tier's semantic-versioning promise
//! (server/docs/embedded-support.md). Naming them here makes that promise
//! checkable: removing or renaming one fails this fixture's build, and the
//! included datastore tests (`tests/datastore_suite.rs`) exercise their
//! behaviour, including build, seal, reopen, verify and query, through this
//! same public API. Other public items of the crate are Server-facing and are
//! not promised.

pub use munarium_datastore::{
    canonical::{canonical_bytes, canonical_sha256},
    fusion::FusionWeights,
    model,
    shard::{OpenShard, ShardWriter, MANIFEST, RECORDS_BODY, VECTOR_DATA},
    store::{ArtifactStore, LocalFileStore},
    vector::{Candidate, FlatVectorIndex, VectorIndex},
    verify::{Limits, ReaderCapabilities},
    Error, PreparedChunk,
};

/// The lexical query surface, present with the `lexical-tantivy` feature.
#[cfg(feature = "lexical-tantivy")]
pub use munarium_datastore::lexical::{Demotion, LexicalPlan, PlanTerm};

/// The approximate vector engine, present with the `vector-diskann` feature.
#[cfg(feature = "vector-diskann")]
pub use munarium_datastore::{
    shard::VECTOR_DISKANN_DATA,
    vector_diskann::{DiskAnnVectorIndex, GraphParams},
};
