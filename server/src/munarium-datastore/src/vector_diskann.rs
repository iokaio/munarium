// SPDX-License-Identifier: Apache-2.0
//! Approximate vector search through Microsoft's first-party `diskann` crate.
//!
//! The stage 0 spike (the DiskANN evaluation under the datastore design
//! tree, archived 2026-09-02) established that the
//! 2026 `diskann` crate is viable: MIT, pure Rust, pinned `=0.56.0`, a public
//! `DataProvider` seam — no fork. This module is the stage 8 adapter behind
//! the `vector-diskann` feature. The design decisions that matter:
//!
//! - **The crate ships an algorithm, not an index.** `DiskANNIndex` is generic
//!   over a `DataProvider` ensemble the caller implements: vector storage,
//!   adjacency storage, distance computation, and the strategy glue that picks
//!   accessors. The whole ensemble is implemented HERE, over our own memory —
//!   the crate's own reference provider lives behind its `testing` feature,
//!   which is not an API to ship on. The exact trait shapes this file depends
//!   on are pinned by `tests/diskann_contract.rs`, so an upstream shape change
//!   fails at dependency-update review rather than during a release (§6.2).
//!
//! - **Full precision throughout, no quantization in v1.** Vectors are stored
//!   as given (the same rule as `FlatVectorIndex`), distances during traversal
//!   are true `1 - cosine` — the same number the exact oracle computes — so
//!   the ONLY approximation is which nodes the beam visits. `quantization` and
//!   `rescore_depth` in the plan stay `None` and mean it.
//!
//! - **The graph is built once and frozen.** Chunks are known in full before
//!   seal, insertion is single-threaded in chunk ordinal order, and the sealed
//!   artifact stores vectors + adjacency + the start point. Open reconstructs
//!   the provider from those bytes; nothing is ever inserted after seal. All
//!   of the crate's delete/consolidate machinery is deliberately unused —
//!   immutable artifacts are the tier's whole identity.
//!
//! - **The start point is a synthetic centroid**, not a data point. The
//!   crate's `FilterStartPoints` post-processor removes start points from
//!   results; electing a real chunk would make that chunk unreturnable.
//!   The centroid gets the one id past the last ordinal.
//!
//! - **The crate's futures never actually wait here.** Every provider method
//!   is in-memory and immediately ready, and the single-insert and search
//!   paths spawn no tasks (only `multi_insert`/`multi_inplace_delete` do,
//!   and they are not called). `block_on` below is therefore a plain poll
//!   loop with a park-based waker — no runtime, no executor dependency.

use std::collections::BTreeMap;
use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::RwLock;

use diskann::error::{ANNError, ANNResult};
use diskann::graph::config::MaxDegree;
use diskann::graph::config::PruneKind;
use diskann::graph::search::Knn;
use diskann::graph::search_output_buffer::IdDistance;
use diskann::graph::{self, glue, workingset, AdjacencyList, Config, DiskANNIndex};
use diskann::provider::{self, DefaultContext, NoopGuard};
use diskann::utils::VectorRepr;
use diskann_vector::distance::Metric;
use diskann_vector::PreprocessedDistanceFunction;

use crate::vector::{Candidate, VectorIndex};
use crate::Error;

/// Engine identity as recorded in `ArtifactBuildPlan.vector` and the
/// manifest's `EngineRef`. The revision is the pinned crate version — the
/// Cargo.toml pin is `=0.56.0`, so the two cannot drift apart silently.
pub const ENGINE_ID: &str = "diskann";
pub const ENGINE_REVISION: &str = "0.56.0";

/// The envelope feature bit an approximate artifact requires. A reader built
/// without `vector-diskann` does not advertise it, so verification refuses the
/// artifact BEFORE any traffic is switched to it (§5.3) — the same mechanism
/// that gates `records.v1`.
pub const FEATURE_BIT: &str = "vector.diskann.v1";

// ---------------------------------------------------------------------------
// Graph parameters
// ---------------------------------------------------------------------------

/// The physical knobs, recorded in the plan's `vector.graph` map (§6.3: every
/// physical decision lands in the plan, never in runtime state).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphParams {
    /// Vamana R: out-degree after pruning.
    pub max_degree: u32,
    /// Construction-time search list size (build quality vs build time).
    pub l_build: u32,
    /// Pruning diversity factor; > 1.0 keeps longer-range edges.
    pub alpha: f32,
    /// Query-time search list floor. The effective L is
    /// `max(l_search, requested limit)`, so a caller asking for more
    /// candidates than the floor still gets a wide enough beam.
    pub l_search: u32,
}

impl Default for GraphParams {
    fn default() -> Self {
        // Calibrated on the corpus sizes this tier actually serves (see
        // the recorded benchmark baseline): R=32/L=100 is the
        // literature's robust middle ground for <10M vectors, and alpha 1.2
        // is the DiskANN paper's default.
        Self {
            max_degree: 32,
            l_build: 100,
            alpha: 1.2,
            l_search: 100,
        }
    }
}

impl GraphParams {
    /// Render into the plan's `graph:` map.
    pub fn to_plan_map(self) -> BTreeMap<String, crate::model::Param> {
        use crate::model::Param;
        [
            ("max_degree", Param::Int(self.max_degree as i64)),
            ("l_build", Param::Int(self.l_build as i64)),
            // Text, not float: `Param` deliberately has no float variant
            // because floats in hashed documents invite representation drift.
            ("alpha", Param::Text(format!("{}", self.alpha))),
            ("l_search", Param::Int(self.l_search as i64)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    /// Parse from the plan's `graph:` map. A diskann plan that omits a knob is
    /// refused rather than defaulted: the plan is the record of physical
    /// decisions, and a default silently applied at seal would be a decision
    /// the record does not show.
    pub fn from_plan_map(map: &BTreeMap<String, crate::model::Param>) -> Result<Self, Error> {
        use crate::model::Param;
        let int = |key: &str| -> Result<u32, Error> {
            match map.get(key) {
                Some(Param::Int(v)) if *v > 0 && *v <= u32::MAX as i64 => Ok(*v as u32),
                Some(other) => Err(Error::Invalid(format!(
                    "plan graph parameter {key:?} is {other:?}, expected a positive integer"
                ))),
                None => Err(Error::Invalid(format!(
                    "plan names the diskann engine but its graph map lacks {key:?}"
                ))),
            }
        };
        let alpha = match map.get("alpha") {
            Some(Param::Text(v)) => v.parse::<f32>().ok().filter(|a| a.is_finite() && *a >= 1.0),
            _ => None,
        }
        .ok_or_else(|| {
            Error::Invalid(
                "plan graph parameter \"alpha\" must be a finite text number >= 1.0".into(),
            )
        })?;
        Ok(Self {
            max_degree: int("max_degree")?,
            l_build: int("l_build")?,
            alpha,
            l_search: int("l_search")?,
        })
    }
}

// ---------------------------------------------------------------------------
// A minimal block_on
// ---------------------------------------------------------------------------

/// Poll a future to completion on the current thread.
///
/// Sound for any future; sized for ours: the provider below is pure in-memory,
/// so every future in the insert and search paths is ready on the first poll
/// and the park branch never runs in practice. It exists so a genuinely
/// pending future would wait correctly instead of spinning.
fn block_on<F: Future>(fut: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct ThreadWaker(std::thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let mut fut = std::pin::pin!(fut);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => std::thread::park(),
        }
    }
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// The provider's error. Ids come only from the graph itself, so an
/// out-of-range id means corrupted state, never a caller mistake — it always
/// escalates (the crate's term for "not transient, stop the operation").
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum ProviderError {
    #[error("vector id {0} is out of range")]
    OutOfRange(u32),
}

diskann::always_escalate!(ProviderError);

impl From<ProviderError> for ANNError {
    fn from(e: ProviderError) -> Self {
        ANNError::message(e.to_string())
    }
}

/// In-memory vector + adjacency storage for one sealed shard.
///
/// Ids are chunk ordinals; internal and external ids are the same u32. The
/// start point (the corpus centroid) is id `count` — one past the last chunk.
/// Vectors are immutable from construction; adjacency is behind per-node
/// RwLocks, written during the single-threaded build and only read after.
struct Store {
    dims: usize,
    count: usize,
    /// `count * dims`, ordinal-major.
    data: Vec<f32>,
    /// The centroid start point (id == count).
    start: Vec<f32>,
    /// `count + 1` entries; the last is the start point's list.
    adjacency: Vec<RwLock<AdjacencyList<u32>>>,
}

impl Store {
    fn start_id(&self) -> u32 {
        self.count as u32
    }

    fn vector_of(&self, id: u32) -> Result<&[f32], ProviderError> {
        let i = id as usize;
        if i < self.count {
            Ok(&self.data[i * self.dims..(i + 1) * self.dims])
        } else if i == self.count {
            Ok(&self.start)
        } else {
            Err(ProviderError::OutOfRange(id))
        }
    }

    fn read_neighbors(&self, id: u32, out: &mut AdjacencyList<u32>) -> Result<(), ProviderError> {
        let node = self
            .adjacency
            .get(id as usize)
            .ok_or(ProviderError::OutOfRange(id))?;
        out.clear();
        out.extend_from_slice(node.read().expect("adjacency lock poisoned").as_ref());
        Ok(())
    }
}

impl provider::DataProvider for Store {
    type Context = DefaultContext;
    type InternalId = u32;
    type ExternalId = u32;
    type Error = ProviderError;
    type Guard = NoopGuard<u32>;

    fn to_internal_id(&self, _cx: &DefaultContext, gid: &u32) -> Result<u32, ProviderError> {
        if (*gid as usize) <= self.count {
            Ok(*gid)
        } else {
            Err(ProviderError::OutOfRange(*gid))
        }
    }

    fn to_external_id(&self, _cx: &DefaultContext, id: u32) -> Result<u32, ProviderError> {
        if (id as usize) <= self.count {
            Ok(id)
        } else {
            Err(ProviderError::OutOfRange(id))
        }
    }
}

/// Insertion assigns nothing: vectors are pre-loaded at construction and the
/// id mapping is the identity, so `set_element` only validates and hands back
/// the guard the insert path requires.
impl provider::SetElement<&[f32]> for Store {
    type SetError = ProviderError;

    async fn set_element(
        &self,
        _cx: &DefaultContext,
        id: &u32,
        element: &[f32],
    ) -> Result<NoopGuard<u32>, ProviderError> {
        // The element must be the vector already stored at that ordinal —
        // anything else means the build loop and the store disagree.
        let held = self.vector_of(*id)?;
        debug_assert_eq!(held.len(), element.len());
        Ok(NoopGuard::new(*id))
    }
}

// --- neighbor accessor -----------------------------------------------------

struct Neighbors<'a> {
    store: &'a Store,
}

impl provider::HasId for Neighbors<'_> {
    type Id = u32;
}

impl provider::NeighborAccessor for Neighbors<'_> {
    async fn get_neighbors(&mut self, id: u32, out: &mut AdjacencyList<u32>) -> ANNResult<()> {
        self.store.read_neighbors(id, out)?;
        Ok(())
    }
}

impl provider::NeighborAccessorMut for Neighbors<'_> {
    async fn set_neighbors(&mut self, id: u32, neighbors: &[u32]) -> ANNResult<()> {
        let node = self
            .store
            .adjacency
            .get(id as usize)
            .ok_or(ProviderError::OutOfRange(id))?;
        let mut list = node.write().expect("adjacency lock poisoned");
        list.clear();
        list.extend_from_slice(neighbors);
        Ok(())
    }

    async fn append_vector(&mut self, id: u32, neighbors: &[u32]) -> ANNResult<()> {
        let node = self
            .store
            .adjacency
            .get(id as usize)
            .ok_or(ProviderError::OutOfRange(id))?;
        let mut list = node.write().expect("adjacency lock poisoned");
        list.extend_from_slice(neighbors);
        Ok(())
    }
}

// --- search accessor -------------------------------------------------------

struct SearchAccessor<'a> {
    store: &'a Store,
    distance: <f32 as VectorRepr>::QueryDistance,
    scratch: AdjacencyList<u32>,
}

impl<'a> SearchAccessor<'a> {
    fn new(store: &'a Store, query: &[f32]) -> Result<Self, ProviderError> {
        // Dimension mismatches are caught before this is constructed; the
        // distance function would panic on them.
        debug_assert_eq!(query.len(), store.dims);
        Ok(Self {
            store,
            distance: f32::query_distance(query, Metric::Cosine),
            scratch: AdjacencyList::new(),
        })
    }

    fn distance_to(&self, id: u32) -> Result<f32, ProviderError> {
        let v = self.store.vector_of(id)?;
        let d = self.distance.evaluate_similarity(v);
        // `1 - cosine` is NaN against a zero-norm vector. The exact oracle
        // maps that case to distance 1.0 ("no direction, no similarity to
        // anything"); the graph must agree, and a NaN inside the beam would
        // poison every comparison after it.
        Ok(if d.is_finite() { d } else { 1.0 })
    }
}

impl provider::HasId for SearchAccessor<'_> {
    type Id = u32;
}

impl glue::SearchAccessor for SearchAccessor<'_> {
    async fn starting_points(&self) -> ANNResult<Vec<u32>> {
        Ok(vec![self.store.start_id()])
    }

    async fn start_point_distances<F>(&mut self, mut f: F) -> ANNResult<()>
    where
        F: FnMut(u32, f32) + Send,
    {
        let id = self.store.start_id();
        let d = self.distance_to(id)?;
        f(id, d);
        Ok(())
    }

    async fn expand_beam<Itr, P, F>(
        &mut self,
        ids: Itr,
        mut pred: P,
        mut on_neighbors: F,
    ) -> ANNResult<()>
    where
        Itr: Iterator<Item = u32> + Send,
        P: glue::HybridPredicate<u32> + Send + Sync,
        F: FnMut(u32, f32) + Send,
    {
        // Take the scratch list so `self.distance_to` can borrow `self`
        // while the list is iterated (the reference provider's own pattern).
        let mut neighbors = std::mem::take(&mut self.scratch);
        for id in ids {
            self.store.read_neighbors(id, &mut neighbors)?;
            for &n in neighbors.iter().filter(|i| pred.eval_mut(i)) {
                let d = self.distance_to(n)?;
                on_neighbors(n, d);
            }
        }
        self.scratch = neighbors;
        Ok(())
    }
}

// --- prune accessor --------------------------------------------------------

type WorkingSet = workingset::Map<u32, Box<[f32]>, workingset::map::Ref<[f32]>>;
type WorkingView<'a> = workingset::map::View<'a, u32, Box<[f32]>, workingset::map::Ref<[f32]>>;

struct PruneAccessor<'a> {
    store: &'a Store,
    distance: <f32 as VectorRepr>::Distance,
    set: WorkingSet,
}

impl provider::HasId for PruneAccessor<'_> {
    type Id = u32;
}

impl glue::PruneAccessor for PruneAccessor<'_> {
    type ElementRef<'x> = &'x [f32];
    type View<'x>
        = WorkingView<'x>
    where
        Self: 'x;
    type Distance<'x>
        = <f32 as VectorRepr>::Distance
    where
        Self: 'x;
    type Neighbors<'x>
        = Neighbors<'x>
    where
        Self: 'x;

    async fn fill<Itr>(&mut self, itr: Itr) -> ANNResult<(Self::View<'_>, Self::Distance<'_>)>
    where
        Itr: ExactSizeIterator<Item = u32> + Clone + Send + Sync,
    {
        let view = self.set.fill(itr, |id| {
            let v = self.store.vector_of(id)?;
            Ok::<_, ProviderError>(Some(v.into()))
        })?;
        Ok((view, self.distance))
    }

    fn neighbors(&mut self) -> Neighbors<'_> {
        Neighbors { store: self.store }
    }
}

// --- strategy --------------------------------------------------------------

/// One strategy serves search, insertion and pruning: the accessors above are
/// the behavior, the strategy only selects them.
#[derive(Debug, Clone, Default)]
struct Strategy;

impl<'a> glue::SearchStrategy<'a, Store, &'a [f32]> for Strategy {
    type SearchAccessorError = ProviderError;
    type SearchAccessor = SearchAccessor<'a>;

    fn search_accessor(
        &'a self,
        provider: &'a Store,
        _cx: &'a DefaultContext,
        query: &'a [f32],
    ) -> Result<SearchAccessor<'a>, ProviderError> {
        SearchAccessor::new(provider, query)
    }
}

impl<'a> glue::DefaultPostProcessor<'a, Store, &'a [f32]> for Strategy {
    diskann::default_post_processor!(glue::Pipeline<glue::FilterStartPoints, glue::CopyIds>);
}

impl glue::PruneStrategy<Store> for Strategy {
    type PruneAccessor<'a> = PruneAccessor<'a>;
    type PruneAccessorError = ProviderError;

    fn prune_accessor<'a>(
        &'a self,
        provider: &'a Store,
        _cx: &'a DefaultContext,
        capacity: usize,
    ) -> Result<PruneAccessor<'a>, ProviderError> {
        let set = workingset::map::Builder::new(workingset::map::Capacity::Default).build(capacity);
        Ok(PruneAccessor {
            store: provider,
            distance: f32::distance(Metric::Cosine, Some(provider.dims)),
            set,
        })
    }
}

impl<'a> glue::InsertStrategy<'a, Store, &'a [f32]> for Strategy {
    type PruneStrategy = Self;

    fn prune_strategy(&self) -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

/// A sealed, immutable DiskANN (Vamana) index over one shard's embeddings.
pub struct DiskAnnVectorIndex {
    index: DiskANNIndex<Store>,
    strategy: Strategy,
    context: DefaultContext,
    params: GraphParams,
    /// Ordinal-indexed; the id space of the graph.
    chunk_ids: Vec<String>,
    dims: usize,
}

impl std::fmt::Debug for DiskAnnVectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskAnnVectorIndex")
            .field("dims", &self.dims)
            .field("len", &self.chunk_ids.len())
            .field("params", &self.params)
            .finish()
    }
}

fn config_for(params: GraphParams) -> Result<Config, Error> {
    let mut builder = graph::config::Builder::new(
        params.max_degree as usize,
        MaxDegree::default_slack(),
        params.l_build as usize,
        PruneKind::from_metric(Metric::Cosine),
    );
    builder.alpha(params.alpha);
    Config::try_from_builder(builder)
        .map_err(|e| Error::Invalid(format!("diskann config refused: {e}")))
}

fn ann(e: ANNError) -> Error {
    Error::Invalid(format!("diskann: {e}"))
}

impl DiskAnnVectorIndex {
    /// Build the graph from `(chunk_id, embedding)` pairs.
    ///
    /// Single-threaded, insertion in the given order — chunk ordinal order at
    /// seal — so construction memory is the store plus one search scratch, and
    /// the build is as reproducible as the algorithm allows. Construction cost
    /// is one graph search per vector; the seal path is already the expensive
    /// path, and §10.4 keeps it off serving nodes.
    pub fn build(
        dims: usize,
        entries: &[(String, Vec<f32>)],
        params: GraphParams,
    ) -> Result<Self, Error> {
        if dims == 0 {
            return Err(Error::Invalid("vector index with zero dimensions".into()));
        }
        if entries.is_empty() {
            return Err(Error::Invalid(
                "a diskann index needs at least one vector; an empty corpus has no start point"
                    .into(),
            ));
        }
        if entries.len() >= u32::MAX as usize {
            return Err(Error::Limit(format!(
                "{} vectors exceeds the u32 id space",
                entries.len()
            )));
        }
        let count = entries.len();
        let mut data = Vec::with_capacity(count * dims);
        let mut chunk_ids = Vec::with_capacity(count);
        let mut start = vec![0.0f64; dims];
        for (chunk_id, embedding) in entries {
            if embedding.len() != dims {
                return Err(Error::Invalid(format!(
                    "embedding for {chunk_id:?} has {} dimensions, index expects {dims}",
                    embedding.len()
                )));
            }
            if embedding.iter().any(|v| !v.is_finite()) {
                return Err(Error::Invalid(format!(
                    "embedding for {chunk_id:?} holds a non-finite value"
                )));
            }
            for (acc, v) in start.iter_mut().zip(embedding) {
                *acc += *v as f64;
            }
            data.extend_from_slice(embedding);
            chunk_ids.push(chunk_id.clone());
        }
        // The centroid start point. f64 accumulation so a large corpus does
        // not lose the mean to f32 cancellation.
        let start: Vec<f32> = start.iter().map(|v| (*v / count as f64) as f32).collect();

        let store = Store {
            dims,
            count,
            data,
            start,
            adjacency: (0..=count)
                .map(|_| RwLock::new(AdjacencyList::new()))
                .collect(),
        };
        let index = DiskANNIndex::new(config_for(params)?, store, NonZeroUsize::new(1));
        let strategy = Strategy;
        let context = DefaultContext;

        for (ordinal, (_, embedding)) in entries.iter().enumerate() {
            let ordinal = ordinal as u32;
            block_on(index.insert(&strategy, &context, &ordinal, &embedding[..])).map_err(ann)?;
        }

        Ok(Self {
            index,
            strategy,
            context,
            params,
            chunk_ids,
            dims,
        })
    }

    pub fn params(&self) -> GraphParams {
        self.params
    }

    /// Serialize: everything `from_bytes` needs to reconstruct the provider
    /// byte-for-byte — params, ids, vectors, centroid, adjacency.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let store = self.index.provider();
        let mut out = Vec::new();
        out.extend_from_slice(&1u32.to_le_bytes()); // format version
        out.extend_from_slice(&(self.dims as u64).to_le_bytes());
        out.extend_from_slice(&(store.count as u64).to_le_bytes());
        out.extend_from_slice(&self.params.max_degree.to_le_bytes());
        out.extend_from_slice(&self.params.l_build.to_le_bytes());
        out.extend_from_slice(&self.params.alpha.to_le_bytes());
        out.extend_from_slice(&self.params.l_search.to_le_bytes());
        for id in &self.chunk_ids {
            let b = id.as_bytes();
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        for v in &store.data {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in &store.start {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for node in &store.adjacency {
            let list = node.read().expect("adjacency lock poisoned");
            out.extend_from_slice(&(list.len() as u32).to_le_bytes());
            for n in list.iter() {
                out.extend_from_slice(&n.to_le_bytes());
            }
        }
        Ok(out)
    }

    /// Parse and reconstruct. Every declared length is checked against the
    /// bytes that remain before it allocates — these bytes come from an
    /// untrusted store (the same discipline as `FlatVectorIndex::from_bytes`),
    /// and every neighbor id is checked against the id space, because a
    /// corrupted edge would otherwise become an out-of-range panic mid-query.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let bad = |what: &str| Error::Integrity(format!("diskann index: {what}"));
        let mut pos = 0usize;
        let take = |pos: &mut usize, n: usize, what: &str| -> Result<&[u8], Error> {
            if *pos + n > bytes.len() {
                return Err(bad(&format!("truncated in {what}")));
            }
            let s = &bytes[*pos..*pos + n];
            *pos += n;
            Ok(s)
        };
        let u32_at = |pos: &mut usize, what: &str| -> Result<u32, Error> {
            Ok(u32::from_le_bytes(take(pos, 4, what)?.try_into().unwrap()))
        };
        let u64_at = |pos: &mut usize, what: &str| -> Result<u64, Error> {
            Ok(u64::from_le_bytes(take(pos, 8, what)?.try_into().unwrap()))
        };

        let format = u32_at(&mut pos, "header")?;
        if format != 1 {
            return Err(Error::Unsupported(format!(
                "diskann index format {format}, this reader supports 1"
            )));
        }
        let dims = u64_at(&mut pos, "header")? as usize;
        let count = u64_at(&mut pos, "header")? as usize;
        if dims == 0 || count == 0 {
            return Err(bad("declares an empty index"));
        }
        let params = GraphParams {
            max_degree: u32_at(&mut pos, "params")?,
            l_build: u32_at(&mut pos, "params")?,
            alpha: f32::from_le_bytes(take(&mut pos, 4, "params")?.try_into().unwrap()),
            l_search: u32_at(&mut pos, "params")?,
        };
        if !params.alpha.is_finite() || params.alpha < 1.0 {
            return Err(bad("alpha is not a finite number >= 1.0"));
        }
        if params.max_degree == 0 || params.l_build == 0 || params.l_search == 0 {
            return Err(bad("a graph parameter is zero"));
        }

        let mut chunk_ids = Vec::with_capacity(count.min(4096));
        for i in 0..count {
            let len = u32_at(&mut pos, "id table")? as usize;
            let b = take(&mut pos, len, "id table")?;
            chunk_ids.push(
                String::from_utf8(b.to_vec()).map_err(|_| bad(&format!("id {i} is not UTF-8")))?,
            );
        }

        let vec_bytes = count
            .checked_mul(dims)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| bad("declared size overflows"))?;
        let mut data = Vec::with_capacity(count * dims);
        let (quads, rest) = take(&mut pos, vec_bytes, "vectors")?.as_chunks::<4>();
        debug_assert!(rest.is_empty());
        for c in quads {
            data.push(f32::from_le_bytes(*c));
        }
        let mut start = Vec::with_capacity(dims);
        let (quads, rest) = take(&mut pos, dims * 4, "start point")?.as_chunks::<4>();
        debug_assert!(rest.is_empty());
        for c in quads {
            start.push(f32::from_le_bytes(*c));
        }

        let id_ceiling = count as u32; // valid ids: 0..=count (count == start)
        let mut adjacency = Vec::with_capacity(count + 1);
        for node in 0..=count {
            let deg = u32_at(&mut pos, "adjacency")? as usize;
            let raw = take(&mut pos, deg * 4, "adjacency")?;
            let mut list = AdjacencyList::with_capacity(deg);
            for c in raw.as_chunks::<4>().0 {
                let n = u32::from_le_bytes(*c);
                if n > id_ceiling {
                    return Err(bad(&format!(
                        "node {node} names neighbor {n}, outside the id space"
                    )));
                }
                list.push(n);
            }
            adjacency.push(RwLock::new(list));
        }
        if pos != bytes.len() {
            return Err(bad(&format!(
                "{} trailing bytes after the adjacency lists",
                bytes.len() - pos
            )));
        }

        let store = Store {
            dims,
            count,
            data,
            start,
            adjacency,
        };
        let index = DiskANNIndex::new(config_for(params)?, store, NonZeroUsize::new(1));
        Ok(Self {
            index,
            strategy: Strategy,
            context: DefaultContext,
            params,
            chunk_ids,
            dims,
        })
    }
}

impl VectorIndex for DiskAnnVectorIndex {
    fn vector_candidates(&self, embedding: &[f32], limit: usize) -> Result<Vec<Candidate>, Error> {
        if embedding.len() != self.dims {
            return Err(Error::Invalid(format!(
                "query has {} dimensions, index holds {}",
                embedding.len(),
                self.dims
            )));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        // A zero-norm query has no direction; every cosine distance is the
        // oracle's 1.0 and any traversal is arbitrary. Answer exactly what the
        // exact index answers: ids ascending at distance 1.0.
        if embedding.iter().all(|v| *v == 0.0) {
            let mut ids: Vec<&String> = self.chunk_ids.iter().collect();
            ids.sort();
            return Ok(ids
                .into_iter()
                .take(limit)
                .map(|id| Candidate {
                    chunk_id: id.clone(),
                    score: 1.0,
                })
                .collect());
        }

        let k = limit.min(self.chunk_ids.len());
        let l = (self.params.l_search as usize).max(k);
        let knn =
            Knn::new(l, None).map_err(|e| Error::Invalid(format!("diskann search params: {e}")))?;
        let mut ids = vec![0u32; k];
        let mut distances = vec![0f32; k];
        let mut output = IdDistance::new(&mut ids, &mut distances);
        block_on(
            self.index
                .search(knn, &self.strategy, &self.context, embedding, &mut output),
        )
        .map_err(ann)?;
        use diskann::graph::search_output_buffer::SearchOutputBuffer as _;
        let filled = output.current_len();

        let mut scored: Vec<Candidate> = ids[..filled]
            .iter()
            .zip(&distances[..filled])
            .map(|(id, d)| Candidate {
                chunk_id: self.chunk_ids[*id as usize].clone(),
                score: *d,
            })
            .collect();
        // The same deterministic order as the exact index: distance
        // ascending, then chunk id — so two equally close chunks come back in
        // the same order from either engine.
        scored.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        Ok(scored)
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn len(&self) -> usize {
        self.chunk_ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::FlatVectorIndex;

    /// Deterministic pseudo-vectors: splitmix64 over (i, d), so every run and
    /// machine builds the identical corpus AND every vector is distinct. (The
    /// first version of this used a 97-element lattice, which collapsed 2,000
    /// "vectors" to 97 distinct points and measured tie-breaking, not recall.)
    fn synth(i: usize, dims: usize) -> Vec<f32> {
        fn splitmix(mut x: u64) -> u64 {
            x = x.wrapping_add(0x9E3779B97F4A7C15);
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
            x ^ (x >> 31)
        }
        (0..dims)
            .map(|d| {
                let h = splitmix((i as u64) << 32 | d as u64);
                (h as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    fn corpus(n: usize, dims: usize) -> Vec<(String, Vec<f32>)> {
        (0..n)
            .map(|i| (format!("chunk-{i:04}"), synth(i, dims)))
            .collect()
    }

    #[test]
    fn recall_against_the_exact_oracle_clears_the_gate() {
        let dims = 32;
        let entries = corpus(2_000, dims);
        let approx = DiskAnnVectorIndex::build(dims, &entries, GraphParams::default()).unwrap();
        let mut exact = FlatVectorIndex::new(dims);
        for (id, v) in &entries {
            exact.push(id.clone(), v).unwrap();
        }

        let k = 10;
        let queries = 25;
        let mut hits = 0usize;
        for q in 0..queries {
            // Query ids far outside the corpus id range, so queries are never
            // corpus members.
            let query = synth(1_000_000 + q, dims);
            let oracle = exact.vector_candidates(&query, k).unwrap();
            // Distance-threshold recall: a returned candidate whose TRUE
            // distance is within the oracle's k-th distance counts, so a tie
            // at the boundary is not scored as a miss.
            let kth = oracle.last().unwrap().score + 1e-6;
            let got = approx.vector_candidates(&query, k).unwrap();
            assert!(got.len() <= k);
            hits += got.iter().filter(|c| c.score <= kth).count();
        }
        let recall = hits as f64 / (queries * k) as f64;
        // The stage 8 gate: recall@10 >= 0.95 at the default parameters.
        assert!(
            recall >= 0.95,
            "recall@{k} was {recall:.3}, below the 0.95 gate"
        );
    }

    #[test]
    fn scores_are_the_oracle_scores_for_the_vectors_it_returns() {
        let dims = 16;
        let entries = corpus(300, dims);
        let approx = DiskAnnVectorIndex::build(dims, &entries, GraphParams::default()).unwrap();
        let mut exact = FlatVectorIndex::new(dims);
        for (id, v) in &entries {
            exact.push(id.clone(), v).unwrap();
        }
        let query = synth(9_999, dims);
        let exact_scores: std::collections::HashMap<String, f32> = exact
            .vector_candidates(&query, 300)
            .unwrap()
            .into_iter()
            .map(|c| (c.chunk_id, c.score))
            .collect();
        for c in approx.vector_candidates(&query, 10).unwrap() {
            let want = exact_scores[&c.chunk_id];
            // Full precision, no quantization: the distance for a returned
            // chunk is the SAME number the oracle computes, modulo the
            // accumulation-order wobble of a SIMD-specialized kernel.
            assert!(
                (c.score - want).abs() < 1e-5,
                "{}: approx {} vs exact {want}",
                c.chunk_id,
                c.score
            );
        }
    }

    #[test]
    fn serialization_round_trips_and_answers_identically() {
        let dims = 24;
        let entries = corpus(500, dims);
        let built = DiskAnnVectorIndex::build(dims, &entries, GraphParams::default()).unwrap();
        let bytes = built.to_bytes().unwrap();
        let opened = DiskAnnVectorIndex::from_bytes(&bytes).unwrap();
        assert_eq!(opened.len(), 500);
        assert_eq!(opened.dimensions(), dims);
        assert_eq!(opened.params(), built.params());
        for q in 0..5 {
            let query = synth(q * 53 + 3, dims);
            let a = built.vector_candidates(&query, 8).unwrap();
            let b = opened.vector_candidates(&query, 8).unwrap();
            assert_eq!(a, b, "query {q} answered differently after reopen");
        }
        // And the bytes are stable: serializing the reopened index yields
        // the identical artifact component.
        assert_eq!(bytes, opened.to_bytes().unwrap());
    }

    #[test]
    fn corrupted_and_truncated_bytes_are_refused_as_integrity() {
        let dims = 8;
        let entries = corpus(50, dims);
        let built = DiskAnnVectorIndex::build(dims, &entries, GraphParams::default()).unwrap();
        let bytes = built.to_bytes().unwrap();

        // Truncation anywhere.
        for cut in [3usize, 20, bytes.len() / 2, bytes.len() - 1] {
            match DiskAnnVectorIndex::from_bytes(&bytes[..cut]) {
                Err(Error::Integrity(_)) => {}
                other => panic!("truncation at {cut} gave {other:?}"),
            }
        }
        // An out-of-range neighbor id in the adjacency region: rewrite the
        // LAST four bytes (the tail of the final adjacency list) to a huge id.
        let mut evil = bytes.clone();
        let n = evil.len();
        evil[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        match DiskAnnVectorIndex::from_bytes(&evil) {
            Err(Error::Integrity(msg)) => assert!(msg.contains("id space"), "{msg}"),
            other => panic!("corrupted edge gave {other:?}"),
        }
        // Trailing garbage is refused, not ignored.
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[0u8; 7]);
        match DiskAnnVectorIndex::from_bytes(&padded) {
            Err(Error::Integrity(msg)) => assert!(msg.contains("trailing"), "{msg}"),
            other => panic!("padding gave {other:?}"),
        }
    }

    #[test]
    fn a_zero_query_matches_the_oracle_exactly() {
        let dims = 8;
        let entries = corpus(20, dims);
        let approx = DiskAnnVectorIndex::build(dims, &entries, GraphParams::default()).unwrap();
        let mut exact = FlatVectorIndex::new(dims);
        for (id, v) in &entries {
            exact.push(id.clone(), v).unwrap();
        }
        let zero = vec![0.0f32; dims];
        assert_eq!(
            approx.vector_candidates(&zero, 5).unwrap(),
            exact.vector_candidates(&zero, 5).unwrap()
        );
    }

    #[test]
    fn refusals_at_the_door() {
        let dims = 4;
        assert!(matches!(
            DiskAnnVectorIndex::build(dims, &[], GraphParams::default()),
            Err(Error::Invalid(_))
        ));
        let mut entries = corpus(3, dims);
        entries[1].1[2] = f32::NAN;
        assert!(matches!(
            DiskAnnVectorIndex::build(dims, &entries, GraphParams::default()),
            Err(Error::Invalid(_))
        ));
        let entries = corpus(3, dims);
        let ix = DiskAnnVectorIndex::build(dims, &entries, GraphParams::default()).unwrap();
        assert!(matches!(
            ix.vector_candidates(&[0.0; 7], 5),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn plan_map_round_trips_and_refuses_partial_maps() {
        let p = GraphParams::default();
        let map = p.to_plan_map();
        assert_eq!(GraphParams::from_plan_map(&map).unwrap(), p);
        let mut partial = map.clone();
        partial.remove("l_build");
        match GraphParams::from_plan_map(&partial) {
            Err(Error::Invalid(msg)) => assert!(msg.contains("l_build"), "{msg}"),
            other => panic!("partial map gave {other:?}"),
        }
    }

    #[test]
    fn a_single_vector_corpus_works() {
        let dims = 4;
        let entries = corpus(1, dims);
        let ix = DiskAnnVectorIndex::build(dims, &entries, GraphParams::default()).unwrap();
        let got = ix.vector_candidates(&synth(0, dims), 3).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].chunk_id, "chunk-0000");
        assert!(got[0].score < 1e-6);
    }
}
