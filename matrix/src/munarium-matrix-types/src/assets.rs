// SPDX-License-Identifier: Apache-2.0
//! The three asset kinds, v1 grammar.
//!
//! Every asset is `deny_unknown_fields`. That is the single most useful thing
//! in this file: a typo in a security-relevant key (`subjectToRowSecurty`)
//! must be a validation error, not a silently ignored field that leaves the
//! check off. The invalid-fixture tree in `matrix/fixtures/assets/invalid/`
//! has one file per fail-closed rule, and adding a rule without a fixture
//! fails the suite.

use munarium_matrix_core::checkpoint::{DeleteSemantics, DriftPolicy, SyncMode, WatermarkSpec};
use munarium_matrix_core::derivation::Derivation;
use munarium_matrix_core::value::ColumnType;
use serde::{Deserialize, Serialize};

pub const API_VERSION: &str = "munarium.ioka.io/v1";

/// `metadata: { name, version }` — the shape every asset shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub name: String,
    pub version: u32,
}

impl Metadata {
    /// `name@version` — the reference form used everywhere a version matters.
    pub fn asset_ref(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

// ---------------------------------------------------------------------------
// DataSource
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataSourceDoc {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: DataSourceSpec,
}

// `Hash` so the kind can key an adapter registry (server::adapters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    Postgres,
    /// MySQL 8 and compatible engines.
    Mysql,
    /// SQL Server 2016+ and Azure SQL, over TDS.
    Sqlserver,
    /// Snowflake, over its SQL API v2.
    Snowflake,
    /// BigQuery, over the `jobs.query` REST API.
    Bigquery,
    Databricks,
    Landing,
    /// A Cube deployment: its REST API answers bounded intents over the
    /// metrics its own schema defines.
    Cube,
    /// A dbt Semantic Layer (MetricFlow) environment, over its GraphQL API.
    Dbt,
}

impl AdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AdapterKind::Postgres => "postgres",
            AdapterKind::Mysql => "mysql",
            AdapterKind::Sqlserver => "sqlserver",
            AdapterKind::Snowflake => "snowflake",
            AdapterKind::Bigquery => "bigquery",
            AdapterKind::Databricks => "databricks",
            AdapterKind::Landing => "landing",
            AdapterKind::Cube => "cube",
            AdapterKind::Dbt => "dbt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataSourceSpec {
    pub adapter: AdapterKind,
    /// Adapter-specific connection settings. Kept as a free map ON PURPOSE:
    /// each adapter validates its own keys, so adding an adapter does not
    /// widen this crate. `credentialRef` never appears here.
    #[serde(default)]
    pub connection: serde_json::Map<String, serde_json::Value>,
    /// A NAME resolved through the secret resolver at call time. A literal
    /// secret here is a validation error — see `validate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub egress: EgressSpec,
    #[serde(default)]
    pub role: RolePosture,
    #[serde(default)]
    pub authorization: AuthorizationSpec,
    #[serde(default)]
    pub limits: SourceLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotSpec>,
    #[serde(default)]
    pub schema_fingerprint: SchemaFingerprintSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EgressSpec {
    /// Hosts this source may reach. Empty means "nothing is allowed", not
    /// "everything": egress is default-deny, so an unset allowlist refuses at
    /// probe rather than opening the door.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Private ranges are denied unless declared — the SSRF/rebinding defense.
    #[serde(default)]
    pub allow_private_ranges: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RolePosture {
    #[serde(default)]
    pub must_be: RoleRequirements,
}

/// What `introspect` proves about the connection role before the source is
/// usable. Defaults are the SAFE ones: read-only required, ownership refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoleRequirements {
    #[serde(default = "default_true")]
    pub read_only: bool,
    #[serde(default)]
    pub subject_to_row_security: bool,
    #[serde(default = "default_true")]
    pub not_owner: bool,
}

impl Default for RoleRequirements {
    fn default() -> Self {
        Self {
            read_only: true,
            subject_to_row_security: false,
            not_owner: true,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationStrategy {
    /// The source enforces per-principal policy (Postgres RLS, Unity Catalog).
    SourceNative,
    /// One least-privilege principal per authorization equivalence class.
    PerClassPrincipals,
    /// The source cannot express the needed policy; refuse the operation.
    Refuse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationSpec {
    pub strategy: AuthorizationStrategy,
    #[serde(default)]
    pub classes: Vec<AuthorizationClassSpec>,
    /// Cap on distinct classes (R5). Beyond it, a `too_many_classes` refusal —
    /// an unbounded class count means an unbounded collection count.
    #[serde(default = "default_max_classes")]
    pub max_authorization_classes: usize,
}

impl Default for AuthorizationSpec {
    fn default() -> Self {
        Self {
            strategy: AuthorizationStrategy::SourceNative,
            classes: Vec::new(),
            max_authorization_classes: default_max_classes(),
        }
    }
}

fn default_max_classes() -> usize {
    16
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationClassSpec {
    pub name: String,
    #[serde(default)]
    pub compartments: Vec<String>,
    #[serde(default)]
    pub access_level: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceLimits {
    #[serde(default = "default_max_rows")]
    pub max_rows: u64,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    #[serde(default = "default_statement_timeout")]
    pub statement_timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_per_hour: Option<u64>,
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_rows: default_max_rows(),
            max_bytes: default_max_bytes(),
            statement_timeout_ms: default_statement_timeout(),
            budget_per_hour: None,
        }
    }
}

fn default_max_rows() -> u64 {
    10_000
}
fn default_max_bytes() -> u64 {
    8 * 1024 * 1024
}
fn default_statement_timeout() -> u64 {
    8_000
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotSpec {
    /// Descriptive marker kind, e.g. `pg_snapshot`, `delta_version`, `manifest`.
    pub kind: String,
    /// G2 is only promised where this says `source_time_travel`.
    #[serde(default = "default_replay_level")]
    pub replay_level: String,
}

fn default_replay_level() -> String {
    "sealed_result".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaFingerprintSpec {
    #[serde(default)]
    pub on_drift: DriftPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncSpec {
    pub mode: SyncMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<WatermarkSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletes: Option<DeleteSemantics>,
    pub entity: EntitySpec,
    /// The columns that are read. Denied columns are simply not listed — there
    /// is no "exclude" form, because a projection that names what it excludes
    /// grows a hole every time the source adds a column.
    #[serde(default)]
    pub projection: Vec<String>,
    /// Postgres logical replication only (2026-08-30): the slot and
    /// publication this source reads, when the operator did not create them
    /// under the `munarium_matrix_<source>` convention. Matrix still creates
    /// neither; a refusal prints the statement for whichever name applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdc: Option<CdcSpec>,
}

/// The replication objects a `cdc` source reads. Both optional: an absent
/// name means the convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CdcSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitySpec {
    pub table: String,
    pub key: Vec<String>,
}

// ---------------------------------------------------------------------------
// QueryContract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryContractDoc {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: QueryContractSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryContractSpec {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: std::collections::BTreeMap<String, ParameterSpec>,
    pub statement_by_dialect: std::collections::BTreeMap<String, StatementSpec>,
    /// The tables and columns the statement is permitted to read.
    ///
    /// Separate from `result` on purpose. A statement reads SOURCE columns and
    /// returns RESULT columns, and the two sets overlap only in a pass-through
    /// projection: `SUM(amount) AS pipeline_amount` reads `amount` and returns
    /// `pipeline_amount`. Deriving the allowlist from `result` — which is what
    /// this code did until 2026-08-29 — refuses every aliased aggregate and
    /// every filter on an unprojected column.
    ///
    /// Declared by the author rather than introspected from the source, so the
    /// allowlist stays a statement of intent. "Whatever the source happens to
    /// have" is not an allowlist.
    #[serde(default)]
    pub reads: ReadsSpec,
    pub result: ResultSpec,
    #[serde(default)]
    pub policy: PolicySpec,
    #[serde(default)]
    pub limits: ContractLimits,
    #[serde(default)]
    pub evidence: EvidencePolicy,
    #[serde(default)]
    pub verified_questions: Vec<VerifiedQuestion>,
}

/// What a contract's statement may read.
///
/// Empty is legal and means "only the result columns", which is correct for a
/// pass-through projection and is what every contract written before this field
/// existed relied on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadsSpec {
    /// Tables the statement may name, bare or schema-qualified.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<String>,
    /// Source columns the statement may reference anywhere — projection,
    /// filter, GROUP BY, ORDER BY, or inside an aggregate.
    ///
    /// A column here is still refused if `policy.deniedColumns` names it: a
    /// read declaration cannot grant what policy denies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParameterSpec {
    #[serde(rename = "type")]
    pub ty: ColumnType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    /// A closed set of permitted values. A value outside it is `not_covered`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<Vec<String>>,
    /// Or: the set is whatever a declared column contains. Resolved at
    /// introspect time and pinned, never re-read per request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_values_from: Option<AllowedValuesFrom>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AllowedValuesFrom {
    pub column: String,
}

/// The statement, per dialect. Either inline or a reviewed file plus its hash;
/// the hash is what makes "operator-reviewed" checkable rather than claimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatementSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultSpec {
    /// Declared in order; the map is ordered so the column order is the
    /// author's, not a hash map's.
    pub columns: std::collections::BTreeMap<String, ResultColumnSpec>,
    /// Explicit order, because a BTreeMap sorts alphabetically and a result's
    /// column order is part of its identity.
    #[serde(default)]
    pub column_order: Vec<String>,
    #[serde(default)]
    pub order_by: Vec<String>,
    #[serde(default)]
    pub derivations: std::collections::BTreeMap<String, DerivationSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultColumnSpec {
    #[serde(rename = "type")]
    pub ty: ColumnType,
    #[serde(default)]
    pub key: bool,
    #[serde(default)]
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additivity: Option<munarium_matrix_core::result::Additivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_type: Option<ColumnType>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DerivationSpec {
    pub op: munarium_matrix_core::derivation::DerivationOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numerator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
}

impl DerivationSpec {
    pub fn to_derivation(&self, name: &str) -> Derivation {
        Derivation {
            name: name.to_string(),
            op: self.op,
            over: self.over.clone(),
            numerator: self.numerator.clone(),
            denominator: self.denominator.clone(),
            scale: self.scale,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicySpec {
    #[serde(default = "default_authorization")]
    pub authorization: AuthorizationStrategy,
    /// Columns this contract must never select. Enforced by the compiler
    /// against the parsed AST, not by string search.
    #[serde(default)]
    pub denied_columns: Vec<String>,
}

impl Default for PolicySpec {
    fn default() -> Self {
        Self {
            authorization: default_authorization(),
            denied_columns: Vec::new(),
        }
    }
}

fn default_authorization() -> AuthorizationStrategy {
    AuthorizationStrategy::SourceNative
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractLimits {
    #[serde(default = "default_contract_max_rows")]
    pub max_rows: u64,
    #[serde(default = "default_contract_max_bytes")]
    pub max_bytes: u64,
    #[serde(default = "default_contract_timeout")]
    pub timeout_ms: u64,
}

impl Default for ContractLimits {
    fn default() -> Self {
        Self {
            max_rows: default_contract_max_rows(),
            max_bytes: default_contract_max_bytes(),
            timeout_ms: default_contract_timeout(),
        }
    }
}

fn default_contract_max_rows() -> u64 {
    500
}
/// 1 MiB — the inline-seal ceiling, so a contract that stays inside it seals in
/// one round-trip on the turn path.
fn default_contract_max_bytes() -> u64 {
    1024 * 1024
}
fn default_contract_timeout() -> u64 {
    6_000
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidencePolicy {
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_replay_level")]
    pub replay_level: String,
}

impl Default for EvidencePolicy {
    fn default() -> Self {
        Self {
            retention_days: default_retention_days(),
            replay_level: default_replay_level(),
        }
    }
}

fn default_retention_days() -> u32 {
    400
}

/// A contract's own regression suite. `mxctl verify` runs these; the runbook's
/// `verifyDataViews` step runs the same code path, so the CLI and the API
/// cannot disagree about whether a contract is healthy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifiedQuestion {
    pub question: String,
    #[serde(default)]
    pub parameters: std::collections::BTreeMap<String, serde_json::Value>,
    pub expect: VerifiedExpectation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifiedExpectation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_result_hash: Option<String>,
    #[serde(default)]
    pub invariants: Vec<Invariant>,
}

/// An invariant is a claim about the result that must hold. Weaker than a hash
/// and much more durable: a hash breaks when the fixture changes, an invariant
/// breaks when the MEANING changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Invariant {
    pub op: munarium_matrix_core::derivation::DerivationOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_least: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_most: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingLimits {
    /// Most findings one pass may file on the ledger version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_findings_per_run: Option<u64>,
    /// Most claims one authoritative pass may propose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_proposals_per_run: Option<u64>,
}

// ---------------------------------------------------------------------------
// MetricView
// ---------------------------------------------------------------------------

/// A metric view the SOURCE owns, referenced by identity and bounded by this
/// overlay. Matrix never copies the measure formulas: it declares which
/// measures and dimensions a caller may ask for, which dimensions may be
/// filtered, and the questions that must keep answering the same way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricViewDoc {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: MetricViewSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricViewSpec {
    /// The DataSource that serves the view. Its adapter must declare the
    /// `metric_views` capability (Databricks does).
    pub source: String,
    /// The view's catalog identity as the source names it —
    /// `catalog.schema.name`, or `schema.name` under the source's catalog.
    pub view: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Measures a caller may ask for, with the type each arrives as.
    pub measures: std::collections::BTreeMap<String, MeasureSpec>,
    /// Dimensions a caller may group by.
    #[serde(default)]
    pub dimensions: std::collections::BTreeMap<String, DimensionSpec>,
    #[serde(default)]
    pub filters: MetricFilterPolicy,
    /// Most dimensions one question may group by. 0 means no ceiling.
    #[serde(default)]
    pub max_dimensions: usize,
    /// Words a question-selection layer may use for a measure or dimension.
    /// Never read by the compiler — a synonym is for choosing, not naming.
    #[serde(default)]
    pub synonyms: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub policy: PolicySpec,
    #[serde(default)]
    pub limits: ContractLimits,
    #[serde(default)]
    pub evidence: EvidencePolicy,
    #[serde(default)]
    pub verified_questions: Vec<MetricVerifiedQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MeasureSpec {
    #[serde(rename = "type")]
    pub ty: ColumnType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additivity: Option<munarium_matrix_core::result::Additivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DimensionSpec {
    #[serde(rename = "type")]
    pub ty: ColumnType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Which dimensions a question may filter on. Empty means every declared
/// dimension; naming some closes the rest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricFilterPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_dimensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricVerifiedQuestion {
    pub question: String,
    pub intent: MetricQuestionIntent,
    pub expect: VerifiedExpectation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricQuestionIntent {
    pub measures: Vec<String>,
    #[serde(default)]
    pub dimensions: Vec<String>,
    #[serde(default)]
    pub filters: Vec<MetricQuestionFilter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricQuestionFilter {
    pub dimension: String,
    #[serde(default = "default_filter_op")]
    pub op: String,
    pub value: serde_json::Value,
}

fn default_filter_op() -> String {
    "eq".into()
}

// ---------------------------------------------------------------------------
// DataView: the minimal native semantic view
// ---------------------------------------------------------------------------

/// A native semantic view: one fact table, measures as declared aggregates
/// over its columns, dimensions as its columns. No joins — the grain is the
/// table's, so fan-out cannot happen, and anything that needs a relationship
/// is `not_covered` until relationships are declared (they are not, yet).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataViewDoc {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: DataViewSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataViewSpec {
    /// The DataSource the table lives in. Postgres and Databricks serve it.
    pub source: String,
    /// The fact table — `schema.table`, or a bare name under the source's
    /// declared schema.
    pub table: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub measures: std::collections::BTreeMap<String, NativeMeasureSpec>,
    #[serde(default)]
    pub dimensions: std::collections::BTreeMap<String, NativeDimensionSpec>,
    #[serde(default)]
    pub filters: MetricFilterPolicy,
    #[serde(default)]
    pub max_dimensions: usize,
    #[serde(default)]
    pub synonyms: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub policy: PolicySpec,
    #[serde(default)]
    pub limits: ContractLimits,
    #[serde(default)]
    pub evidence: EvidencePolicy,
    #[serde(default)]
    pub verified_questions: Vec<MetricVerifiedQuestion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAggregate {
    Sum,
    Count,
    Min,
    Max,
    Avg,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeMeasureSpec {
    pub op: NativeAggregate,
    /// The column aggregated. Optional only for `count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(rename = "type")]
    pub ty: ColumnType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additivity: Option<munarium_matrix_core::result::Additivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeDimensionSpec {
    /// The source column. Defaults to the dimension's own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(rename = "type")]
    pub ty: ColumnType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// ClaimMapping
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimMappingDoc {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: ClaimMappingSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingMode {
    /// Default. Observations and discrepancy findings only; canon untouched.
    #[default]
    Shadow,
    /// Operator-enabled per mapping after the promotion gates. Proposes claims.
    Authoritative,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimMappingSpec {
    pub source: String,
    #[serde(default)]
    pub mode: MappingMode,
    pub entity: MappingEntity,
    pub properties: std::collections::BTreeMap<String, MappingProperty>,
    pub temporal: TemporalSpec,
    #[serde(default)]
    pub changes: std::collections::BTreeMap<String, ChangeRule>,
    /// Consulted only in authoritative mode.
    #[serde(default)]
    pub authority: Vec<AuthorityScope>,
    /// Per-run ceilings on what a pass may write. A pass that would
    /// exceed one is refused BEFORE it files anything, with the counts it
    /// would have produced, because a mapping that suddenly wants to write a
    /// thousand claims is more likely a broken join than a thousand truths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<MappingLimits>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingEntity {
    pub table: String,
    pub key: Vec<String>,
    /// `shareholder.{holder_id}` — placeholders are column names.
    pub subject_template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_template: Option<String>,
    #[serde(default)]
    pub identity: IdentitySpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentitySpec {
    #[serde(default)]
    pub resolver: Resolver,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
    #[serde(default)]
    pub on_ambiguous: AmbiguityPolicy,
    /// The declared alias table. Required by — and only meaningful to — the
    /// `terminology_alias` resolver; the validator refuses each without the
    /// other, because a resolver with nothing to resolve and a table nothing
    /// reads are the same lie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<AliasTable>,
}

impl Default for IdentitySpec {
    fn default() -> Self {
        Self {
            resolver: Resolver::default(),
            min_confidence: default_min_confidence(),
            on_ambiguous: AmbiguityPolicy::default(),
            aliases: None,
        }
    }
}

/// How a source row is bound to a ledger subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolver {
    /// The subject template over the entity key, and nothing else. The
    /// DEFAULT, because it is what the pipeline does with no alias table —
    /// and a default naming a resolver that needs configuration nobody
    /// supplied is a setting that lies about the behaviour.
    #[default]
    EntityKey,
    /// The key-derived subject, plus lower-confidence candidates from a
    /// declared alias table. The ledger's subjects come from documents and are
    /// named, not keyed; this is what binds `holder_id = 42` to the entity a
    /// signed document calls "Jane Rowntree".
    TerminologyAlias,
}

fn default_min_confidence() -> f64 {
    0.95
}

/// Surface forms declared to mean one ledger subject.
///
/// **Declared, never computed.** Experiment found the failure this shape exists
/// to avoid: an alias normalizer that turned a *similarity* into an
/// equivalence class, which is precisely the move that merges two people.
/// Normalization here folds case and whitespace only; `J. Rowntree` is a form
/// of Jane Rowntree because a human wrote that down, not because an edit
/// distance was small enough.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AliasTable {
    /// The source column carrying the human-readable name.
    pub column: String,
    /// Confidence carried by an alias-derived candidate. Below 1.0 by
    /// construction: an alias is evidence about identity, not proof of it.
    #[serde(default = "default_alias_confidence")]
    pub confidence: f64,
    pub entries: Vec<AliasEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AliasEntry {
    /// The ledger subject these forms name.
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_path: Option<String>,
    pub forms: Vec<String>,
}

fn default_alias_confidence() -> f64 {
    0.96
}

/// Fold a surface form to its comparable shape: trimmed, inner whitespace
/// collapsed, lowercased.
///
/// Deliberately no punctuation stripping and no initial expansion. Both would
/// make `J. Rowntree` and `Jane Rowntree` equal by RULE, and the whole point of
/// a declared table is that a human decides that, per person, in writing.
pub fn normalize_alias_form(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl AliasTable {
    /// The entry whose declared forms include `raw`, or `None`.
    ///
    /// The validator refuses a table where one normalized form appears under
    /// two subjects, so a hit is unique by construction.
    pub fn lookup(&self, raw: &str) -> Option<&AliasEntry> {
        let needle = normalize_alias_form(raw);
        self.entries
            .iter()
            .find(|e| e.forms.iter().any(|f| normalize_alias_form(f) == needle))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguityPolicy {
    /// File a finding and move on. Ambiguity NEVER merges — there is no
    /// `pick_best` variant, on purpose.
    #[default]
    FileFinding,
    /// Drop the observation silently (only for noisy, low-value properties).
    Skip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingProperty {
    pub column: String,
    #[serde(rename = "type")]
    pub ty: ColumnType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalSpec {
    pub valid_time: ValidTimeSpec,
    #[serde(default)]
    pub transaction_time: TransactionTimeSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidTimeSpec {
    pub column: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionTimeSpec {
    /// True: take it from the source (LSN, delta_version, observed_at).
    #[serde(default = "default_true")]
    pub from_source: bool,
}

impl Default for TransactionTimeSpec {
    fn default() -> Self {
        Self { from_source: true }
    }
}

/// The business rule for a property's changes. NOT inferred from CDC: an
/// `UPDATE` in the source can be a legitimate update or a correction of a past
/// mistake, and only the business knows which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeRule {
    #[serde(default)]
    pub on_update: ChangeKindDecision,
    /// What to do when the change alters a value whose valid-time is in the
    /// past. `RequiresReview` is the only safe default: backdating must never
    /// bypass a gate.
    #[serde(default)]
    pub on_backdated: ChangeKindDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKindDecision {
    /// A legitimate new value; supersedes.
    Update,
    /// The earlier value was wrong; supersedes and says so.
    Correction,
    /// File for a human. Never auto-applied — backdating must not bypass a gate.
    #[default]
    RequiresReview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityScope {
    pub property: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<String>,
    #[serde(default)]
    pub precedence: Precedence,
    #[serde(default)]
    pub conflict: ConflictPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precedence {
    SourceOverDocument,
    /// The safe default: a document the customer signed outranks a row until
    /// an operator declares otherwise for a specific property.
    #[default]
    DocumentOverSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Both sides stay visible and the answer discloses the disagreement.
    /// There is deliberately no `pick_a_winner` variant.
    #[default]
    PreserveAndDisclose,
}
