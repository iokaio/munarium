// SPDX-License-Identifier: Apache-2.0
//! The `SourceAdapter` seam.
//!
//! Every source — Postgres, Databricks, a landing export — implements this one
//! trait, and everything above it (the sync role, the query role, the
//! reconcile role) is written against the trait alone. The rule that keeps
//! that honest is [`Capabilities`]: an adapter **declares** what it can do,
//! and the layers above refuse rather than assume. A protocol name is not a
//! support claim; a capability row plus a passing conformance suite is.
//!
//! This crate depends on no driver. Adapters bring their own.

#![forbid(unsafe_code)]
#![allow(clippy::result_large_err)]

pub mod binding;
/// The conversational-planner seam, re-exported from the kernel.
///
/// It lives in `munarium-matrix-core` because the asset validator needs the
/// same types and cannot depend on this crate. Re-exported here so an adapter
/// implementing `planner_ask` reaches for it in the obvious place.
pub use munarium_matrix_core::planner;
pub mod capabilities;

use async_trait::async_trait;
use munarium_matrix_core::checkpoint::{Checkpoint, SyncMode, WatermarkSpec};
use munarium_matrix_core::{Refusal, TypedResult, Value};

pub use binding::{bind_named, bind_parameters, BoundParameters};
pub use capabilities::{Capabilities, PolicyStrategy};

pub type Result<T> = std::result::Result<T, Refusal>;

/// The identity the source will see, chosen from the caller's authorization
/// snapshot. Passing this explicitly (rather than letting an adapter reach for
/// "the" credential) is what makes per-class principals possible without every
/// adapter re-implementing the selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveIdentity {
    /// The class this identity serves, if the source uses per-class principals.
    pub class: Option<String>,
    /// The secret reference to resolve at call time. Never the secret itself.
    pub credential_ref: Option<String>,
    /// What to record in the evidence as the principal the source saw.
    pub principal: String,
}

/// Hard ceilings for one call. The adapter must enforce these SOURCE-SIDE
/// where the engine supports it (`statement_timeout`, `row_limit`,
/// `byte_limit`), not only by truncating what it already fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_rows: u64,
    pub max_bytes: u64,
    pub timeout_ms: u64,
}

/// What `introspect` proved about the connection role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolePosture {
    pub principal: String,
    pub checks: Vec<PostureCheck>,
}

impl RolePosture {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
    /// The failures, for a refusal message that says which requirement failed
    /// rather than "posture check failed".
    pub fn failures(&self) -> Vec<&PostureCheck> {
        self.checks.iter().filter(|c| !c.ok).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostureCheck {
    pub name: String,
    pub required: bool,
    pub observed: bool,
    pub ok: bool,
    pub detail: Option<String>,
}

impl PostureCheck {
    /// A requirement is met when the observation matches what was required.
    /// Spelled out because the polarity flips per check (`read_only` must be
    /// true, `is_owner` must be false) and inlining that at each site is how
    /// a security check silently inverts.
    pub fn new(name: &str, required: bool, observed: bool) -> Self {
        Self {
            name: name.into(),
            required,
            observed,
            ok: required == observed,
            detail: None,
        }
    }
    pub fn with_detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
}

/// The source's shape, as observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaFingerprint {
    /// A stable hash over (table, column, type, nullability), sorted. Changes
    /// when the source changes in a way that could change a result.
    pub fingerprint: String,
    pub tables: Vec<TableShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableShape {
    pub name: String,
    pub columns: Vec<ColumnShape>,
    pub row_security_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnShape {
    pub name: String,
    /// The source's own type name, verbatim.
    pub source_type: String,
    /// The canon@1 type it maps to. `None` means the column cannot be used —
    /// which is reported, not silently coerced to a string.
    pub logical_type: Option<munarium_matrix_core::ColumnType>,
    pub nullable: bool,
}

impl SchemaFingerprint {
    /// Deterministic fingerprint over the observed shape.
    pub fn compute(tables: &[TableShape]) -> String {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        let mut sorted: Vec<&TableShape> = tables.iter().collect();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        for t in sorted {
            h.update(t.name.as_bytes());
            h.update([0x1d]);
            let mut cols: Vec<&ColumnShape> = t.columns.iter().collect();
            cols.sort_by(|a, b| a.name.cmp(&b.name));
            for c in cols {
                h.update(c.name.as_bytes());
                h.update([0x1f]);
                h.update(c.source_type.as_bytes());
                h.update([0x1f]);
                h.update(if c.nullable { b"n" } else { b"-" });
                h.update([0x1e]);
            }
        }
        format!("sha256:{}", hex::encode(h.finalize()))
    }
}

/// One record from a snapshot or change stream.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRecord {
    /// Values positional to the projection, in declared order.
    pub cells: Vec<Value>,
    /// The encoded primary key — the document's logical path and the
    /// observation's `row_key`.
    pub row_key: String,
    /// Engine position for idempotency: LSN, delta version, manifest offset.
    pub event_position: Option<String>,
    /// What the source did. `Snapshot` for a full read.
    pub change_kind: munarium_matrix_types::contract::ChangeKind,
}

/// A batch of records plus where to resume.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordBatch {
    pub records: Vec<SourceRecord>,
    pub columns: Vec<munarium_matrix_core::Column>,
    /// Where the next call resumes. `None` when the stream is exhausted.
    pub next_checkpoint: Option<Checkpoint>,
    /// Rows the source returned that policy or drift excluded. Reported so a
    /// collection can state its coverage honestly (G4).
    pub excluded: u64,
    /// The engine-native snapshot marker for this read.
    pub snapshot_marker: Option<String>,
}

/// What `probe` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub reachable: bool,
    pub latency_ms: Option<u64>,
    pub detail: Option<String>,
}

/// What kind of read, and — inseparably — the declaration it reads by.
///
/// The two travel together as one value on purpose. `read_batch` took a bare
/// `SyncMode` until 2026-08-30, and five adapters answered a `Watermark` mode
/// with columns of their own invention because nothing obliged the caller to
/// hand the declaration over. A type that cannot express "watermark mode with
/// no watermark spec" as anything but `None` is what makes the refusal in
/// `Watermark::resolve` reachable from every engine rather than remembered by
/// each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadMode<'a> {
    pub mode: SyncMode,
    /// The DataSource's `spec.sync.watermark`, for `SyncMode::Watermark`.
    pub watermark: Option<&'a WatermarkSpec>,
}

impl<'a> ReadMode<'a> {
    /// A mode that reads no watermark: snapshot, manifest, cdf, cdc.
    pub fn of(mode: SyncMode) -> Self {
        Self {
            mode,
            watermark: None,
        }
    }

    /// A watermark read by the columns the source declared.
    pub fn watermark(spec: &'a WatermarkSpec) -> Self {
        Self {
            mode: SyncMode::Watermark,
            watermark: Some(spec),
        }
    }

    pub fn new(mode: SyncMode, watermark: Option<&'a WatermarkSpec>) -> Self {
        Self { mode, watermark }
    }
}

/// The columns a watermark read actually uses, resolved from the DataSource's
/// own `spec.sync.watermark` rather than from an adapter's convention.
///
/// Five adapters used to hard-code `(updated_at, id)`. The declaration was
/// validated (`validate_sync`) and then read by nobody, so a source naming
/// `modified_on` was queried by a column it had never mentioned. This is the
/// one place the declaration turns into columns, so the five agree by
/// construction and a sixth engine cannot quietly invent its own pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watermark<'a> {
    pub column: &'a str,
    /// A strictly-ordered secondary column. `None` is only legitimate for an
    /// inclusive watermark, which re-reads the boundary rows every run.
    pub tie_break: Option<&'a str>,
    pub inclusive: bool,
}

impl<'a> Watermark<'a> {
    /// `Ok(None)` for every mode that does not read a watermark. A watermark
    /// read with no declaration is a refusal, never a fallback to a
    /// convention: reading the wrong column is worse than not reading.
    pub fn resolve(mode: SyncMode, spec: Option<&'a WatermarkSpec>) -> Result<Option<Self>> {
        if mode != SyncMode::Watermark {
            return Ok(None);
        }
        let Some(w) = spec else {
            return Err(Refusal::invalid(
                "not_covered",
                "watermark mode needs spec.sync.watermark on the DataSource",
            ));
        };
        if !w.inclusive && w.tie_break.is_none() {
            return Err(Refusal::invalid(
                "not_covered",
                "an exclusive watermark needs spec.sync.watermark.tieBreak: without \
                 it two rows sharing a watermark value straddle the boundary and one \
                 is lost",
            ));
        }
        Ok(Some(Self {
            column: &w.column,
            tie_break: w.tie_break.as_deref(),
            inclusive: w.inclusive,
        }))
    }

    /// Both declared columns must be projected, or the checkpoint could not be
    /// advanced from what was read. `sync.watermark-outside-projection` catches
    /// it at validation; this catches a caller that never went through it.
    pub fn require_projected(&self, projection: &[String]) -> Result<()> {
        let has = |c: &str| projection.iter().any(|p| p == c);
        if has(self.column) && self.tie_break.is_none_or(has) {
            return Ok(());
        }
        Err(Refusal::invalid(
            "not_covered",
            match self.tie_break {
                Some(t) => format!(
                    "watermark mode reads the declared `{}` and `{t}`; project both",
                    self.column
                ),
                None => format!(
                    "watermark mode reads the declared `{}`; project it",
                    self.column
                ),
            },
        ))
    }

    /// `>=` when the declaration is inclusive, `>` when it is not.
    pub fn cmp(&self) -> &'static str {
        if self.inclusive {
            ">="
        } else {
            ">"
        }
    }
}

/// The one trait every source implements.
#[async_trait]
pub trait SourceAdapter: Send + Sync {
    /// `postgres` | `databricks` | `landing` — matches the asset's `adapter`.
    fn kind(&self) -> &'static str;

    /// Version of the adapter itself; part of evidence provenance, because a
    /// decoder change can change a result.
    fn adapter_version(&self) -> &'static str;

    /// What this adapter can actually do. Consulted before every operation.
    fn capabilities(&self) -> Capabilities;

    /// Cheap connectivity + policy probe. Feeds `/healthdata` and the breaker.
    async fn probe(&self) -> Result<ProbeResult>;

    /// Prove the role posture and read the schema. Refuses a role that is
    /// superuser, an owner, or holds DML.
    async fn introspect(&self) -> Result<(RolePosture, SchemaFingerprint)>;

    /// Read a batch of records for mode A / C.
    ///
    /// `watermark` is the DataSource's own `spec.sync.watermark` — the column
    /// to order and compare by, whether the comparison includes the boundary,
    /// and the tie-break that stops two rows sharing a watermark value from
    /// straddling it. Until 2026-08-30 the trait did not carry it, and five
    /// adapters read `(updated_at, id)` by convention instead: a source that
    /// declared `column: modified_on` validated, and was then read by a column
    /// it had not named. An asset field nothing reads is a lie waiting to be
    /// believed, so it is a parameter now, and `SyncMode::Watermark` without
    /// one is a refusal rather than a fallback to the old convention.
    async fn read_batch(
        &self,
        entity: &str,
        projection: &[String],
        checkpoint: &Checkpoint,
        read: ReadMode<'_>,
        identity: &EffectiveIdentity,
        limits: Limits,
    ) -> Result<RecordBatch>;

    /// Execute a compiled statement for mode B. The statement is already
    /// parsed, allowlisted and parameterized by the compiler — an adapter
    /// never sees a string it must trust.
    async fn execute(
        &self,
        statement: &str,
        parameters: &BoundParameters,
        identity: &EffectiveIdentity,
        limits: Limits,
    ) -> Result<ExecutedResult>;

    /// The source's own definition of a catalog object — for a metric view,
    /// the statement that created it. The workers fingerprint it; an adapter
    /// without a semantic layer keeps the default, which refuses by name.
    async fn definition_of(&self, _object: &str, _limits: Limits) -> Result<String> {
        Err(Refusal::metric_not_covered(
            "this adapter cannot report a catalog object's definition",
        ))
    }

    /// Answer a bounded semantic intent NATIVELY.
    ///
    /// `Ok(None)` — the default, and what every warehouse adapter returns —
    /// means "I have no semantic layer of my own; compile the intent to SQL
    /// and call `execute`". A provider that owns the metric definitions
    /// (dbt MetricFlow, Cube) returns `Ok(Some(..))` instead: Matrix never
    /// duplicates their formulas, and there is no statement for it to walk,
    /// so the bound it enforces is the asset's closed lists — checked before
    /// this is called — rather than an allowlist over SQL.
    ///
    /// Everything after the answer is identical for both: the declared result
    /// shape wins, the rows are sealed through the same `evidence::seal`, and
    /// a citation reads back the same way.
    async fn semantic_execute(
        &self,
        _ask: &SemanticAsk,
        _limits: Limits,
    ) -> Result<Option<ExecutedResult>> {
        Ok(None)
    }

    /// Ask a conversational planner a question.
    ///
    /// `Ok(None)` means "I have no planner surface", which is what every
    /// adapter but Databricks says — the same shape as `semantic_execute`, and
    /// for the same reason: a capability nobody else has should cost the
    /// others nothing but a default.
    ///
    /// A planner proposes; **Matrix decides**. Nothing here executes anything:
    /// the message comes back, `munarium-matrix-workers::genie` applies the
    /// allowlist, and a proposal that survives goes through the ordinary
    /// contract path so the compiler's allowlist walk and the budget apply.
    async fn planner_ask(
        &self,
        _space_id: &str,
        _question: &str,
        _limits: Limits,
    ) -> Result<Option<planner::PlannerMessage>> {
        Ok(None)
    }
}

/// An executed query result plus the provenance the manifest needs.
/// A bounded semantic intent, owned, as an adapter receives it.
///
/// The borrowed `core::semantic::SemanticRequest` is what the compiler walks;
/// this is what crosses a trait object to a provider that answers semantics
/// NATIVELY — dbt, Cube — where there is no SQL for Matrix to compile. Names
/// only: every one has already been checked against the asset's closed lists.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticAsk {
    /// The view as the asset names it, in the provider's own vocabulary.
    pub view: String,
    pub measures: Vec<String>,
    pub dimensions: Vec<String>,
    /// `(dimension, op, canonical value text)`. Only `eq` today.
    pub filters: Vec<(String, String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutedResult {
    pub result: TypedResult,
    pub snapshot_marker: Option<String>,
    pub isolation: Option<String>,
    pub engine: Option<String>,
    pub statement_id: Option<String>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posture_check_is_ok_when_observation_matches_requirement() {
        // read_only: required true, observed true -> ok
        assert!(PostureCheck::new("read_only", true, true).ok);
        assert!(!PostureCheck::new("read_only", true, false).ok);
        // not_owner: required TRUE means "must not be owner"; observing that
        // the role IS not-owner is true.
        assert!(PostureCheck::new("not_owner", true, true).ok);
        assert!(!PostureCheck::new("not_owner", true, false).ok);
        // A requirement that is switched off passes either way only when the
        // observation matches — which is what makes `required: false` mean
        // "must be absent", not "do not care".
        assert!(PostureCheck::new("subject_to_row_security", false, false).ok);
        assert!(!PostureCheck::new("subject_to_row_security", false, true).ok);
    }

    #[test]
    fn posture_failures_are_named_individually() {
        let p = RolePosture {
            principal: "matrix_bad_reader".into(),
            checks: vec![
                PostureCheck::new("read_only", true, false),
                PostureCheck::new("not_owner", true, true),
            ],
        };
        assert!(!p.ok());
        let failures = p.failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "read_only");
    }

    #[test]
    fn the_fingerprint_is_stable_under_ordering_and_moves_on_a_real_change() {
        let col = |n: &str, t: &str| ColumnShape {
            name: n.into(),
            source_type: t.into(),
            logical_type: Some(munarium_matrix_core::ColumnType::String),
            nullable: false,
        };
        let a = vec![TableShape {
            name: "t".into(),
            columns: vec![col("a", "text"), col("b", "text")],
            row_security_enabled: true,
        }];
        let reordered = vec![TableShape {
            name: "t".into(),
            columns: vec![col("b", "text"), col("a", "text")],
            row_security_enabled: true,
        }];
        assert_eq!(
            SchemaFingerprint::compute(&a),
            SchemaFingerprint::compute(&reordered),
            "column order in the catalog is not a schema change"
        );

        let retyped = vec![TableShape {
            name: "t".into(),
            columns: vec![col("a", "integer"), col("b", "text")],
            row_security_enabled: true,
        }];
        assert_ne!(
            SchemaFingerprint::compute(&a),
            SchemaFingerprint::compute(&retyped),
            "a type change IS a schema change and must trip drift"
        );

        let dropped = vec![TableShape {
            name: "t".into(),
            columns: vec![col("a", "text")],
            row_security_enabled: true,
        }];
        assert_ne!(
            SchemaFingerprint::compute(&a),
            SchemaFingerprint::compute(&dropped)
        );
    }
}

/// Choose the process-level rustls crypto provider, explicitly and once.
///
/// **Why this has to exist.** rustls 0.23 picks a default provider from crate
/// features, and refuses to guess when more than one is enabled: it panics
/// with *"Could not automatically determine the process-level CryptoProvider
/// from Rustls crate features"* at the first TLS handshake. The workspace
/// resolves `ring` through reqwest, hyper-rustls and tonic — and
/// `tiberius-ng`, adopted on 2026-08-30 to answer three certificate CVEs,
/// brings `tokio-rustls` with default features, which enables `aws-lc-rs`
/// beside it. Cargo features are additive, so no `default-features = false`
/// anywhere in this tree can take that back.
///
/// **How it was found.** Not by reading the graph. A live run behind real
/// TLS ingress failed five gRPC scenarios — and only the five that open
/// a TLS channel; the two in the same module that use REST passed. Compose
/// could never have caught it: compose serves gRPC as **h2c**, so rustls is
/// never reached there, and the tier was 106/106 green the whole time. TLS on
/// the gRPC client path happens only over real ingress.
///
/// Calling this is **idempotent and never fatal**: a second call, or a call
/// after some other component installed a provider, is a no-op. Every binary
/// and the conformance harness call it before any TLS.
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // `install_default` errors only if one is already installed, which is
        // a perfectly good outcome — the point is that ONE is chosen, not that
        // this call is the one that chose it.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod crypto_provider_tests {
    /// After `install_crypto_provider`, rustls has a process-level default.
    ///
    /// This asserts the exact condition rustls checks before it panics —
    /// `CryptoProvider::get_default()` being `None` with two provider features
    /// enabled is what produced *"Could not automatically determine the
    /// process-level CryptoProvider"* on cycle `3wnsdqum`.
    ///
    /// It is worth having as a unit test precisely because the failure it
    /// guards is invisible to the compose tier: compose serves gRPC as h2c,
    /// so no TLS handshake happens there and 106 scenarios passed while this
    /// was broken.
    #[test]
    fn installing_the_provider_gives_rustls_a_default() {
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_none(),
            "a provider was already installed before this test ran — the \
             assertion below would then prove nothing"
        );
        super::install_crypto_provider();
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_some(),
            "no process-level provider after install_crypto_provider(); every \
             TLS handshake in this process would panic"
        );
        // Idempotent: a second call must not panic or unset anything.
        super::install_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
