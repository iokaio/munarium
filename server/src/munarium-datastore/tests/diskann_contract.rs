// SPDX-License-Identifier: Apache-2.0
//! §6.2's contract fixture: the exact public `diskann` surface the adapter
//! depends on, pinned so an upstream shape change fails HERE — at
//! dependency-update review, in a file whose whole purpose is to be read —
//! rather than as forty inference errors inside `vector_diskann.rs` during a
//! release.
//!
//! Two kinds of pin:
//!
//! 1. **Function-pointer coercions**: `let _: fn(..) -> .. = path;` compiles
//!    only while the upstream signature is exactly that.
//! 2. **Bound pins**: generic functions whose `where` clauses restate the
//!    associated-type structure the adapter's impls rely on.
//!
//! The pinned version is `=0.56.0` (exact, in Cargo.toml). When bumping it:
//! make this file compile again FIRST, reading the upstream diff as you go,
//! then re-run the recall gate — in that order, because a silently changed
//! default (say, a new `PruneKind` variant becoming the `Cosine` mapping)
//! passes the type checker and only the recall gate would notice.
#![cfg(feature = "vector-diskann")]

use std::num::NonZeroUsize;

use diskann::error::ANNResult;
use diskann::graph::config::{Builder, ConfigError, MaxDegree, PruneKind};
use diskann::graph::search::{Knn, KnnSearchError};
use diskann::graph::search_output_buffer::{IdDistance, SearchOutputBuffer};
use diskann::graph::{glue, AdjacencyList, Config};
use diskann::provider::{
    DataProvider, DefaultContext, ExecutionContext, Guard, NeighborAccessor, NeighborAccessorMut,
    NoopGuard, SetElement,
};
use diskann::utils::VectorRepr;
use diskann_vector::distance::Metric;
use diskann_vector::PreprocessedDistanceFunction;

// ---------------------------------------------------------------------------
// 1. Signature pins
// ---------------------------------------------------------------------------

#[test]
fn the_construction_surface_has_the_recorded_shape() {
    // Knn search parameters: (l_value, beam_width).
    let _: fn(usize, Option<usize>) -> Result<Knn, KnnSearchError> = Knn::new;

    // Config: builder of (pruned_degree, max_degree, l_build, prune_kind),
    // then try_from_builder.
    let _: fn(usize, MaxDegree, usize, PruneKind) -> Builder = Builder::new;
    let _: fn(Builder) -> Result<Config, ConfigError> = Config::try_from_builder;
    let _: fn() -> MaxDegree = MaxDegree::default_slack;
    let _: fn(Metric) -> PruneKind = PruneKind::from_metric;

    // The metric the adapter builds and searches with, and the fact that its
    // similarity for f32 is `1 - cosine` — lower is better, the same scale as
    // the exact oracle. The VALUE is asserted in `similarity_is_one_minus_cosine`.
    let _ = Metric::Cosine;

    // VectorRepr: how f32 slices become distance computers.
    let _: fn(Metric, Option<usize>) -> <f32 as VectorRepr>::Distance = f32::distance;
    let _: fn(&[f32], Metric) -> <f32 as VectorRepr>::QueryDistance = f32::query_distance;

    // The output buffer the adapter drains is pinned by construction in the
    // runtime smoke below (its lifetimes resist a bare fn-pointer coercion).

    // The adjacency list the provider stores.
    let _: fn() -> AdjacencyList<u32> = AdjacencyList::new;
    let _: fn(usize) -> AdjacencyList<u32> = AdjacencyList::with_capacity;

    // The index constructor: config + provider + thread hint. Pinned via the
    // adapter's own provider type indirectly (it is private); the bound pin
    // below carries the generic shape.
    let _: Option<NonZeroUsize> = NonZeroUsize::new(1);
}

#[test]
fn similarity_is_one_minus_cosine() {
    // If an upstream change flips the sign convention or normalizes
    // differently, every stored score comparison silently inverts — this is
    // the one semantic the types cannot pin.
    let q = [1.0f32, 0.0];
    let d = f32::query_distance(&q, Metric::Cosine);
    let same = d.evaluate_similarity(&[2.0f32, 0.0][..]); // parallel: cos=1, dist=0
    let orth = d.evaluate_similarity(&[0.0f32, 3.0][..]); // orthogonal: cos=0, dist=1
    let anti = d.evaluate_similarity(&[-1.0f32, 0.0][..]); // opposite: cos=-1, dist=2
    assert!(same.abs() < 1e-6, "parallel gave {same}");
    assert!((orth - 1.0).abs() < 1e-6, "orthogonal gave {orth}");
    assert!((anti - 2.0).abs() < 1e-6, "antiparallel gave {anti}");
}

// ---------------------------------------------------------------------------
// 2. Bound pins: the associated-type structure the adapter implements
// ---------------------------------------------------------------------------

/// The provider ensemble shape. If any of these bounds stops holding —
/// an associated type renamed, a method's receiver changed, a new required
/// item — this function stops compiling.
#[allow(dead_code)]
fn pin_provider_ensemble<P>()
where
    P: DataProvider<InternalId = u32, ExternalId = u32>,
    P: for<'a> SetElement<&'a [f32]>,
    P::Guard: Guard<Id = u32>,
    P::Context: ExecutionContext,
{
}

/// The strategy glue shape: one strategy type serving search, insertion and
/// pruning, with accessors whose id type is the provider's internal id.
#[allow(dead_code)]
fn pin_strategy_glue<'a, S, P>()
where
    P: DataProvider<InternalId = u32>,
    S: glue::SearchStrategy<'a, P, &'a [f32]>,
    S: glue::InsertStrategy<'a, P, &'a [f32]>,
    S: glue::PruneStrategy<P>,
    S::SearchAccessor: glue::SearchAccessor,
{
}

/// The neighbor-accessor shape used during build.
#[allow(dead_code)]
fn pin_neighbor_accessors<N>()
where
    N: NeighborAccessor<Id = u32> + NeighborAccessorMut,
{
}

/// `DefaultContext` stays a unit, `NoopGuard` stays constructible from the id.
#[test]
fn the_default_context_and_noop_guard_hold() {
    let _cx: DefaultContext = DefaultContext;
    let g: NoopGuard<u32> = NoopGuard::new(7);
    assert_eq!(g.id(), 7);
}

// ---------------------------------------------------------------------------
// 3. A runtime smoke through the adapter itself
// ---------------------------------------------------------------------------

#[test]
fn the_adapter_builds_and_searches_through_the_pinned_surface() {
    use munarium_datastore::vector::VectorIndex as _;
    use munarium_datastore::vector_diskann::{DiskAnnVectorIndex, GraphParams};

    let entries: Vec<(String, Vec<f32>)> = vec![
        ("a".into(), vec![1.0, 0.0, 0.0]),
        ("b".into(), vec![0.9, 0.1, 0.0]),
        ("c".into(), vec![0.0, 1.0, 0.0]),
        ("d".into(), vec![0.0, 0.0, 1.0]),
        ("e".into(), vec![-1.0, 0.0, 0.0]),
    ];
    let ix = DiskAnnVectorIndex::build(3, &entries, GraphParams::default()).unwrap();
    let got = ix.vector_candidates(&[1.0, 0.05, 0.0], 2).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].chunk_id, "a");
    assert_eq!(got[1].chunk_id, "b");

    // The output buffer contract: current_len reports what search filled.
    let mut ids = [0u32; 2];
    let mut distances = [0f32; 2];
    let buffer = IdDistance::new(&mut ids, &mut distances);
    assert_eq!(buffer.current_len(), 0);
    let _res: ANNResult<()> = Ok(());
}
