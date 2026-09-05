// SPDX-License-Identifier: Apache-2.0
//! Live-tier: Matrix against a REAL munarium-server.
//!
//! Skipped unless `MUNARIUM_MATRIX_LIVE_SERVER_URL` is set, so `cargo test`
//! stays offline by default. When it IS set, a connection failure is a
//! failure, not a skip — a configured server that quietly falls back to
//! nothing is how a tier reports green while testing nothing.
//!
//! ```powershell
//! $env:MUNARIUM_MATRIX_LIVE_SERVER_URL = "http://127.0.0.1:18099"
//! $env:MUNARIUM_MATRIX_LIVE_SERVER_TOKEN = "ev-rw"
//! cargo test -p munarium-matrix-workers --test live_server
//! ```
//!
//! # Why this file exists
//!
//! Because the `MockServer` accepted a request shape the real server rejects.
//! Mode C used to seal through its own `seal_observations`, posting
//! `{"batch": ...}` where the server's contract is `{"manifest": ...}`; the
//! mock took it, and the divergence surfaced only on first contact with a real
//! 0.4.0+ server, as `missing field 'manifest'`. That is the **third** time a
//! mock that did not enforce a peer's contract turned that contract into a
//! surprise — after `claim_id` vs `id`, and the missing `X-Munarium-Uid`.
//!
//! A test double will always be a claim about a peer. This file is where the
//! claim gets checked.

use std::time::Duration;

use munarium_matrix_adapter::Limits;
use munarium_matrix_adapter_landing::LandingAdapter;
use munarium_matrix_core::checkpoint::Checkpoint;
use munarium_matrix_core::{AuthorizationClass, ColumnType};
use munarium_matrix_server_client::{HttpServerClient, ServerClient};
use munarium_matrix_types::contract::*;
use munarium_matrix_types::ClaimMappingDoc;
use munarium_matrix_workers::evidence::{self, SealContext};
use munarium_matrix_workers::{
    observe, reconcile_with, rollback, ObserveContext, ProposalLedger, ProposalRecord,
    ReconcileOptions, RollbackRequest,
};

fn live() -> Option<HttpServerClient> {
    let url = std::env::var("MUNARIUM_MATRIX_LIVE_SERVER_URL").ok()?;
    let token =
        std::env::var("MUNARIUM_MATRIX_LIVE_SERVER_TOKEN").unwrap_or_else(|_| "ev-rw".to_string());
    Some(
        HttpServerClient::new_http1(&url, &token, Duration::from_secs(30))
            .expect("MUNARIUM_MATRIX_LIVE_SERVER_URL is set but the client could not be built"),
    )
}

fn tenant() -> String {
    std::env::var("MUNARIUM_MATRIX_LIVE_TENANT").unwrap_or_else(|_| "ev-live".to_string())
}

fn observation(row_key: &str, property: &str, value: &str) -> Observation {
    Observation {
        entity_candidates: vec![EntityCandidate {
            subject: format!("holder.{row_key}"),
            scope_path: None,
            confidence: 1.0,
            resolver: Some("exact".into()),
        }],
        property: property.to_string(),
        value: TypedValueDto {
            ty: ColumnType::String,
            value: serde_json::Value::String(value.to_string()),
            scale: None,
            element_type: None,
        },
        valid_time: None,
        transaction_time: None,
        change_kind: ChangeKind::Update,
        origin: ConnectorOrigin {
            kind: "connector".into(),
            source_id: "crm".into(),
            mapping_version: "captable-holdings@1".into(),
            row_key: row_key.to_string(),
            event_position: Some(format!("lsn/{row_key}")),
            observed_at: None,
            evidence_id: None,
        },
    }
}

fn batch(batch_id: &str) -> ObservationBatch {
    ObservationBatch {
        contract_version: munarium_matrix_core::CONTRACT_VERSION.to_string(),
        mapping: "captable-holdings@1".into(),
        batch_id: batch_id.to_string(),
        source_id: Some("crm".into()),
        run_id: Some("run-live-1".into()),
        sealed_evidence_id: None,
        observations: vec![
            observation("43", "shares", "90500"),
            observation("44", "shares", "1200"),
        ],
    }
}

fn seal_ctx(t: &str) -> SealContext {
    let now = chrono::Utc::now();
    SealContext {
        tenant: t.to_string(),
        kind: ArtifactKind::Observations,
        source_id: "crm".into(),
        source_version: 1,
        adapter: "matrix".into(),
        adapter_version: None,
        engine: None,
        versions: ManifestVersions {
            claim_mapping: Some("captable-holdings@1".into()),
            ..Default::default()
        },
        plan: None,
        snapshot_marker: Some("run-live-1".into()),
        isolation: None,
        replay_level: "sealed_result".into(),
        effective_principal: None,
        statement_id: None,
        started_at: now,
        ended_at: now,
        retention_days: None,
        declared_max_rows: None,
        rows_covered: Some(2),
        rows_excluded: None,
        exclusion_reason: None,
        freshness_watermark: None,
    }
}

/// The one that was broken: an observation batch seals against a real server.
#[tokio::test]
async fn an_observation_batch_seals_against_a_real_server() {
    let Some(client) = live() else { return };
    let t = tenant();
    let result =
        evidence::observation_batch_result(&batch("live-b1"), AuthorizationClass::default());
    let (id, manifest) = evidence::seal(&client, &result, &seal_ctx(&t), Some("live-b1"))
        .await
        .expect("sealing an observation batch must succeed against a real server");

    assert!(id.starts_with("ev-"), "got {id}");
    assert_eq!(manifest.kind, ArtifactKind::Observations);
    assert_eq!(manifest.evidence_id.as_deref(), Some(id.as_str()));
    // Both hashes present and DIFFERENT — the whole point of two hashes.
    assert!(manifest.logical_result_hash.starts_with("sha256:"));
    assert!(manifest.artifact_hash.starts_with("sha256:"));
    assert_ne!(manifest.logical_result_hash, manifest.artifact_hash);
}

/// Replaying a batch seals once. A reconciliation that retries must not leave
/// two artifacts behind for one set of observations.
#[tokio::test]
async fn a_replayed_batch_seals_exactly_once() {
    let Some(client) = live() else { return };
    let t = tenant();
    let result =
        evidence::observation_batch_result(&batch("live-b2"), AuthorizationClass::default());

    let (first, _) = evidence::seal(&client, &result, &seal_ctx(&t), Some("live-b2"))
        .await
        .expect("first seal");
    let (second, _) = evidence::seal(&client, &result, &seal_ctx(&t), Some("live-b2"))
        .await
        .expect("replayed seal");
    assert_eq!(
        first, second,
        "a replayed batch must resolve to the SAME artifact"
    );
}

/// Order is not part of what was observed: the batch hashes as a multiset.
#[tokio::test]
async fn reordering_observations_does_not_change_the_artifact() {
    let Some(client) = live() else { return };
    let t = tenant();

    let mut b = batch("live-b3");
    let forward = evidence::observation_batch_result(&b, AuthorizationClass::default());
    b.observations.reverse();
    let reversed = evidence::observation_batch_result(&b, AuthorizationClass::default());

    let (a, _) = evidence::seal(&client, &forward, &seal_ctx(&t), Some("live-b3"))
        .await
        .expect("seal forward");
    let (c, _) = evidence::seal(&client, &reversed, &seal_ctx(&t), Some("live-b3-rev"))
        .await
        .expect("seal reversed");
    assert_eq!(
        a, c,
        "keyed identity means the emission order of a connector is not part of \
         the observation; the two orderings are ONE artifact"
    );
}

/// The sealed artifact resolves back, and its rows are the observations.
#[tokio::test]
async fn the_sealed_batch_resolves_and_replays() {
    let Some(client) = live() else { return };
    let t = tenant();
    let result =
        evidence::observation_batch_result(&batch("live-b4"), AuthorizationClass::default());
    let (id, _) = evidence::seal(&client, &result, &seal_ctx(&t), Some("live-b4"))
        .await
        .expect("seal");

    let back = client
        .get_evidence(&id)
        .await
        .expect("resolve the manifest");
    assert_eq!(back.kind, ArtifactKind::Observations);
    assert_eq!(
        back.identity.rows, 2,
        "the manifest must report both observations"
    );
}

// ---------------------------------------------------------------------------
// Phases 4 and 5 against a real server.
//
// Everything above this line seals evidence. What follows RECONCILES: it reads
// the ledger, files findings, and proposes claims back into it.
// Those are the three writes the mock is least able to vouch for, because each
// one is a different route with a different body and its own gate behaviour.
//
// Seeding uses the server's OWN claims route directly rather than Matrix's
// client, and deliberately sends no `origin`. A seeded fact must look like what
// documents produce, or `document_over_source` precedence would be measured
// against connector claims and prove nothing.
// ---------------------------------------------------------------------------

/// A per-call unique token. No uuid crate here: a monotonic counter plus the
/// process start instant is unique within a test run, which is all an
/// idempotency key for a fresh seed has to be.
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// A raw call to the server, for the two things Matrix's client cannot do and
/// should not be able to: create a memory version, and write a claim that is
/// not a connector observation.
struct Seeder {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Seeder {
    fn new() -> Option<Self> {
        let base = std::env::var("MUNARIUM_MATRIX_LIVE_SERVER_URL").ok()?;
        Some(Self {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_string(),
            token: std::env::var("MUNARIUM_MATRIX_LIVE_SERVER_TOKEN")
                .unwrap_or_else(|_| "ev-rw".to_string()),
        })
    }

    /// Every command route requires an `Idempotency-Key`; the server refuses
    /// without one, which is the correct posture for a write plane and is why
    /// Matrix's own client always sends it. Each seed gets a unique key
    /// because a seed is a distinct write, not a replay of one.
    async fn post(&self, path: &str, body: serde_json::Value) -> serde_json::Value {
        let key = format!("seed-{}", uuid_like());
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .header("X-Munarium-Uid", "matrix")
            .header("Idempotency-Key", key)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST {path}: {e}"));
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        assert!(status.is_success(), "POST {path} -> {status}: {text}");
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
    }

    async fn get(&self, path: &str) -> serde_json::Value {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .header("X-Munarium-Uid", "matrix")
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        assert!(status.is_success(), "GET {path} -> {status}: {text}");
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
    }

    /// A fresh lineage per test. Sharing one would make these tests
    /// order-dependent, and an order-dependent gate test is a coin flip.
    async fn version(&self) -> String {
        self.post("/v1/versions", serde_json::json!({})).await["version_id"]
            .as_str()
            .expect("the server returns a version id")
            .to_string()
    }

    /// A document-derived fact: no `origin`, so the server records it as
    /// ordinary witnessed canon.
    async fn fact(&self, version_id: &str, subject: &str, key: &str, value: &str) -> String {
        let body = serde_json::json!({
            "claim_type": "fact",
            "subject": subject,
            "key": key,
            "value": value,
        });
        // `{"claim": {"id": ...}}`, not `claim_id` — the shape Matrix's own
        // client reads, and the one whose mock disagreement cost the first of
        // the defects listed at the top of this file.
        self.post(&format!("/v1/versions/{version_id}/claims"), body)
            .await["claim"]["id"]
            .as_str()
            .expect("the server returns a claim id")
            .to_string()
    }
}

/// The T0 cap-table mapping, parameterised by mode, with the alias table that
/// binds a keyed register to a named ledger.
fn recon_mapping(mode: &str, authority: &str) -> ClaimMappingDoc {
    let yaml = format!(
        r#"apiVersion: munarium.ioka.io/v1
kind: ClaimMapping
metadata: {{ name: captable-live, version: 1 }}
spec:
  source: crm
  mode: {mode}
  entity:
    table: holdings
    key: [holder_id]
    subjectTemplate: "shareholder.{{holder_id}}"
    scopeTemplate: "company.7.captable"
  properties:
    shares_outstanding: {{ column: shares, type: decimal, scale: 0 }}
  temporal:
    validTime: {{ column: effective_date }}
  changes:
    shares_outstanding: {{ onUpdate: update, onBackdated: requires_review }}
{authority}"#
    );
    match munarium_matrix_types::parse_asset(&yaml).expect("the live mapping parses") {
        munarium_matrix_types::Asset::ClaimMapping(m) => *m,
        _ => unreachable!(),
    }
}

fn recon_observation(holder: &str, value: &str, kind: ChangeKind) -> Observation {
    Observation {
        entity_candidates: vec![EntityCandidate {
            subject: format!("shareholder.{holder}"),
            scope_path: Some("company.7.captable".into()),
            confidence: 1.0,
            resolver: Some("entity_key".into()),
        }],
        property: "shares_outstanding".into(),
        value: TypedValueDto {
            ty: ColumnType::Decimal,
            value: serde_json::Value::String(value.into()),
            scale: Some(0),
            element_type: None,
        },
        valid_time: Some(ValidTime {
            from: Some("2026-04-01T00:00:00Z".parse().unwrap()),
            to: None,
        }),
        transaction_time: None,
        change_kind: kind,
        origin: ConnectorOrigin {
            kind: "connector".into(),
            source_id: "crm".into(),
            mapping_version: "captable-live@1".into(),
            row_key: format!("holder_id={holder}"),
            event_position: Some(format!("lsn/{holder}")),
            observed_at: None,
            evidence_id: None,
        },
    }
}

fn recon_batch(batch_id: &str, observations: Vec<Observation>) -> ObservationBatch {
    ObservationBatch {
        contract_version: munarium_matrix_core::CONTRACT_VERSION.to_string(),
        mapping: "captable-live@1".into(),
        batch_id: batch_id.to_string(),
        source_id: Some("crm".into()),
        run_id: Some(format!("run-{batch_id}")),
        sealed_evidence_id: None,
        observations,
    }
}

/// The workers' proposal ledger over a map — what the binary does over Postgres.
/// Insertion-ordered, because `rollback` reads its input in proposal order.
#[derive(Default)]
struct MemLedger {
    rows: std::sync::Mutex<Vec<ProposalRecord>>,
}

#[async_trait::async_trait]
impl ProposalLedger for MemLedger {
    async fn seen(
        &self,
        _t: &str,
        key: &str,
    ) -> std::result::Result<Option<String>, munarium_matrix_core::Refusal> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.idempotency_key == key)
            .map(|r| r.claim_id.clone()))
    }
    async fn record(
        &self,
        _t: &str,
        rec: &ProposalRecord,
    ) -> std::result::Result<(), munarium_matrix_core::Refusal> {
        let mut rows = self.rows.lock().unwrap();
        rows.retain(|r| r.idempotency_key != rec.idempotency_key);
        rows.push(rec.clone());
        Ok(())
    }
}

impl MemLedger {
    fn records(&self) -> Vec<ProposalRecord> {
        self.rows.lock().unwrap().clone()
    }
}

async fn live_reconcile(
    client: &HttpServerClient,
    mapping: &ClaimMappingDoc,
    version_id: &str,
    batch: &ObservationBatch,
    promoted: bool,
    proposals: Option<&MemLedger>,
) -> munarium_matrix_workers::ReconcileOutcome {
    let bytes = serde_json::to_vec(batch).unwrap();
    reconcile_with(
        client,
        mapping,
        version_id,
        batch,
        &bytes,
        &ReconcileOptions {
            tenant: &tenant(),
            promoted,
            source_id: "crm",
            proposals: proposals.map(|p| p as &dyn ProposalLedger),
            // Hand-built single-row batches: not a read of anything.
            source_complete: false,
        },
    )
    .await
    .expect("reconcile against a real server")
}

/// The shadow pass end to end against a real server: a real ledger
/// read, a real seal, a real finding, and canon byte-identical afterwards.
///
/// `slice_facts` is the same read the discrepancy pipeline uses, so this also
/// re-checks the `claim_id` vs `id` defect that first contact found.
#[tokio::test]
async fn a_shadow_pass_files_a_real_finding_and_leaves_canon_alone() {
    let Some(client) = live() else { return };
    let seeder = Seeder::new().unwrap();
    let version = seeder.version().await;
    seeder
        .fact(&version, "shareholder.43", "shares_outstanding", "90000")
        .await;

    let before = format!("{:?}", client.slice_facts(&version, None).await.unwrap());
    let mapping = recon_mapping("shadow", "");
    let batch = recon_batch(
        "live-shadow",
        vec![recon_observation("43", "90500", ChangeKind::Update)],
    );

    let out = live_reconcile(&client, &mapping, &version, &batch, false, None).await;
    assert_eq!(out.discrepancies, 1, "the planted disagreement is found");
    assert_eq!(out.findings_filed, 1, "and filed through the real route");
    assert!(out.canon_untouched);

    let after = format!("{:?}", client.slice_facts(&version, None).await.unwrap());
    assert_eq!(before, after, "shadow mode must not move a single byte");
}

/// A promoted mapping proposes into a real ledger, the claim
/// lands, and it carries the connector origin.
#[tokio::test]
async fn a_promoted_mapping_proposes_a_real_claim_with_its_origin() {
    let Some(client) = live() else { return };
    let seeder = Seeder::new().unwrap();
    let version = seeder.version().await;
    let seeded = seeder
        .fact(&version, "shareholder.43", "shares_outstanding", "90000")
        .await;

    let mapping = recon_mapping(
        "authoritative",
        "  authority:\n    - { property: shares_outstanding, precedence: source_over_document }\n",
    );
    let ledger = MemLedger::default();
    let batch = recon_batch(
        "live-auth",
        vec![recon_observation("43", "90500", ChangeKind::Update)],
    );

    let out = live_reconcile(&client, &mapping, &version, &batch, true, Some(&ledger)).await;
    assert_eq!(out.proposals, 1, "one in-scope property, one proposal");
    assert!(
        !out.canon_untouched,
        "an authoritative pass that wrote must say it wrote"
    );

    // The claim is really there, superseding the seeded one.
    let facts = client.slice_facts(&version, None).await.unwrap();
    let current = facts
        .iter()
        .find(|f| f.subject == "shareholder.43" && f.key == "shares_outstanding")
        .expect("the property still resolves");
    assert_eq!(
        current.value, "90500",
        "the source value supersedes under source_over_document"
    );
    assert_ne!(
        current.claim_id.as_deref(),
        Some(seeded.as_str()),
        "supersession, not mutation: the seeded claim id is not reused"
    );
    assert_eq!(
        current.origin_kind.as_deref(),
        Some("connector"),
        "the connector origin survives the round trip — without it a reader cannot \
         tell a register-derived claim from a document-derived one"
    );
}

/// The no-retry-storm property, against the server's own idempotency
/// store rather than the mock's.
///
/// The mock returns whatever it was told; a real server decides. Running the
/// same batch twice must leave ONE claim, whether the dedup came from Matrix's
/// content key or the server's idempotency record.
#[tokio::test]
async fn a_replayed_authoritative_pass_lands_exactly_one_claim() {
    let Some(client) = live() else { return };
    let seeder = Seeder::new().unwrap();
    let version = seeder.version().await;
    seeder
        .fact(&version, "shareholder.43", "shares_outstanding", "90000")
        .await;

    let mapping = recon_mapping(
        "authoritative",
        "  authority:\n    - { property: shares_outstanding, precedence: source_over_document }\n",
    );
    let ledger = MemLedger::default();
    let batch = recon_batch(
        "live-replay",
        vec![recon_observation("43", "90500", ChangeKind::Update)],
    );

    let first = live_reconcile(&client, &mapping, &version, &batch, true, Some(&ledger)).await;
    let head_after_first = client.head_seq(&version).await.unwrap();
    let second = live_reconcile(&client, &mapping, &version, &batch, true, Some(&ledger)).await;

    assert_eq!(first.proposals, 1);
    assert_eq!(second.proposals, 0, "the replay proposes nothing new");
    assert_eq!(
        client.head_seq(&version).await.unwrap(),
        head_after_first,
        "and the ledger does not advance — a retry storm would show up here \
         as a lineage growing one claim per attempt"
    );
}

/// The default precedence keeps a document's claim on top
/// against a REAL ledger: the discrepancy is filed, the write is withheld.
#[tokio::test]
async fn a_document_claim_outranks_the_source_on_a_real_ledger() {
    let Some(client) = live() else { return };
    let seeder = Seeder::new().unwrap();
    let version = seeder.version().await;
    seeder
        .fact(&version, "shareholder.43", "shares_outstanding", "90000")
        .await;

    // Authority declared, but with the default precedence.
    let mapping = recon_mapping(
        "authoritative",
        "  authority:\n    - { property: shares_outstanding }\n",
    );
    let ledger = MemLedger::default();
    let batch = recon_batch(
        "live-outrank",
        vec![recon_observation("43", "90500", ChangeKind::Update)],
    );

    let out = live_reconcile(&client, &mapping, &version, &batch, true, Some(&ledger)).await;
    assert_eq!(out.proposals, 0);
    assert_eq!(out.withheld_document_outranks, 1);
    assert_eq!(out.findings_filed, 1, "withheld is disclosed, never silent");
    assert!(out.canon_untouched);

    let facts = client.slice_facts(&version, None).await.unwrap();
    let current = facts
        .iter()
        .find(|f| f.subject == "shareholder.43" && f.key == "shares_outstanding")
        .expect("the document claim is still there");
    assert_eq!(current.value, "90000", "the document's value stands");
}

/// A rollback supersedes with `origin.kind = "rollback"` and
/// leaves the history intact, on a real ledger where supersession is the
/// server's own behaviour rather than the mock's.
#[tokio::test]
async fn a_rollback_supersedes_on_a_real_ledger_without_rewriting_history() {
    let Some(client) = live() else { return };
    let seeder = Seeder::new().unwrap();
    let version = seeder.version().await;
    let original = seeder
        .fact(&version, "shareholder.43", "shares_outstanding", "90000")
        .await;

    let mapping = recon_mapping(
        "authoritative",
        "  authority:\n    - { property: shares_outstanding, precedence: source_over_document }\n",
    );
    let ledger = MemLedger::default();
    let batch = recon_batch(
        "live-rollback",
        vec![recon_observation("43", "90500", ChangeKind::Update)],
    );
    live_reconcile(&client, &mapping, &version, &batch, true, Some(&ledger)).await;

    let head_before_rollback = client.head_seq(&version).await.unwrap();
    let proposals = ledger.records();
    assert_eq!(proposals.len(), 1, "one proposal to roll back");
    let out = rollback(
        &client,
        &RollbackRequest {
            tenant: &tenant(),
            source_id: "crm",
            mapping_ref: "captable-live@1",
            decision_id: "live-rollback-decision",
            proposals: &proposals,
            ledger: &ledger,
        },
    )
    .await
    .expect("rollback runs against a real server");

    assert!(out.superseded > 0, "the connector claim is rolled back");
    assert!(
        client.head_seq(&version).await.unwrap() > head_before_rollback,
        "a rollback APPENDS; history is never rewritten"
    );

    // The prior value is restored by APPENDING a correction, and the claim
    // that recorded 90500 is still in the lineage — that is what "history is
    // never rewritten" means, and it is the only reason a rollback is
    // auditable at all.
    let facts = client.slice_facts(&version, None).await.unwrap();
    let current = facts
        .iter()
        .find(|f| f.subject == "shareholder.43" && f.key == "shares_outstanding")
        .expect("the property still resolves after a rollback");
    assert_eq!(
        current.value, "90000",
        "the document's value is restored by supersession"
    );
    assert_ne!(
        current.claim_id.as_deref(),
        Some(original.as_str()),
        "restored, not resurrected: a NEW claim carries the old value"
    );
}

// ---------------------------------------------------------------------------
// The whole T0 fixture, against a real server.
//
// The offline `reconcile.*` scenarios run this exact pass against the mock.
// Here the ledger is real, the findings route is real, and — the part the mock
// cannot vouch for — the server's own content-identity rule decides whether
// two findings are one. It was that rule that would have collapsed the two
// contested rows' ambiguity findings into a single one, because their messages
// were identical; the mock filed both.
// ---------------------------------------------------------------------------

struct T0Fixture(std::path::PathBuf);

impl T0Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mx-live-t0-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), T0_MANIFEST).unwrap();
        std::fs::write(dir.join("rows.csv"), T0_ROWS).unwrap();
        Self(dir)
    }
}

impl Drop for T0Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const T0_MANIFEST: &str = r#"{
  "snapshotId": "s1",
  "format": "csv",
  "keys": ["holder_id", "company_id"],
  "schema": [
    { "name": "holder_id", "type": "int64" },
    { "name": "company_id", "type": "int64" },
    { "name": "holder_name", "type": "string" },
    { "name": "shares", "type": "decimal", "scale": 0 },
    { "name": "share_class", "type": "string" },
    { "name": "effective_date", "type": "date" }
  ],
  "files": [{ "path": "rows.csv" }]
}"#;

/// `crm.holdings` from `fixtures/t0/sql/02-crm-fixture.sql`, row for row.
const T0_ROWS: &str = "holder_id,company_id,holder_name,shares,share_class,effective_date\n\
    42,7,Jane Rowntree,125000,A,2026-04-01\n\
    43,7,Marcus Vane,90500,A,2026-04-01\n\
    51,8,J. Rowntree,40000,B,2026-01-01\n\
    58,8,\"Jane  Rowntree\",40000,B,2026-01-01\n\
    44,7,Priya Anand,15000,A,2025-11-15\n";

/// The committed T0 mapping, so this tier and the offline one are statements
/// about one asset.
fn committed_mapping() -> ClaimMappingDoc {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/assets/valid/mapping.captable.yaml");
    let text = std::fs::read_to_string(&path).expect("the committed T0 mapping is readable");
    match munarium_matrix_types::parse_asset(&text).expect("the committed T0 mapping parses") {
        munarium_matrix_types::Asset::ClaimMapping(m) => *m,
        _ => unreachable!(),
    }
}

/// The T0 answer key: `(row or subject, property, verdict)`.
const T0_EXPECTED: &[(&str, &str, &str)] = &[
    ("43|7", "shares_outstanding", "differ"),
    ("51|8", "shares_outstanding", "identity_ambiguous"),
    ("51|8", "share_class", "identity_ambiguous"),
    ("58|8", "shares_outstanding", "identity_ambiguous"),
    ("58|8", "share_class", "identity_ambiguous"),
    (
        "shareholder.tomas-berg",
        "shares_outstanding",
        "missing_in_source",
    ),
    ("shareholder.tomas-berg", "share_class", "missing_in_source"),
];

async fn t0_findings(seeder: &Seeder, version: &str) -> Vec<(String, String, String)> {
    let body = seeder
        .get(&format!(
            "/v1/versions/{version}/findings?rule_prefix=matrix."
        ))
        .await;
    body["findings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| {
            let finding = &f["finding"];
            let d = &finding["detail"];
            let verdict = if finding["rule_id"] == "matrix.identity-ambiguous" {
                "identity_ambiguous".to_string()
            } else {
                d["verdict"].as_str().unwrap_or_default().to_string()
            };
            let row = if verdict == "missing_in_source" {
                d["subject"].as_str().unwrap_or_default().to_string()
            } else {
                d["row_key"]
                    .as_str()
                    .or_else(|| d["source"]["row_key"].as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            (
                row,
                d["property"].as_str().unwrap_or_default().to_string(),
                verdict,
            )
        })
        .collect()
}

/// The mode-C exit gate against a REAL ledger: the whole T0 fixture through
/// the committed mapping, precision and recall over the planted answer key,
/// both evidence sides resolvable, canon byte-identical, and a replay that
/// files nothing twice — where "nothing twice" is now the server's decision,
/// not the mock's.
#[tokio::test]
async fn the_whole_t0_fixture_reconciles_against_a_real_ledger() {
    let Some(client) = live() else { return };
    let seeder = Seeder::new().unwrap();
    let version = seeder.version().await;
    for (subject, key, value) in [
        ("shareholder.jane-rowntree", "shares_outstanding", "125000"),
        ("shareholder.jane-rowntree", "share_class", "A"),
        ("shareholder.marcus-vane", "shares_outstanding", "90000"),
        ("shareholder.marcus-vane", "share_class", "A"),
        ("shareholder.priya-anand", "shares_outstanding", "15000"),
        ("shareholder.priya-anand", "share_class", "A"),
        ("shareholder.tomas-berg", "shares_outstanding", "5000"),
        ("shareholder.tomas-berg", "share_class", "B"),
    ] {
        seeder.fact(&version, subject, key, value).await;
    }
    let head_before = client.head_seq(&version).await.unwrap();

    let fixture = T0Fixture::new("recon");
    let adapter = LandingAdapter::new_file("crm", &fixture.0, "manifest.json");
    let identity = munarium_matrix_adapter::EffectiveIdentity {
        class: None,
        credential_ref: None,
        principal: "live".into(),
    };
    let mapping = committed_mapping();
    let batch_id = format!("live-t0-{}", uuid_like());
    let (batch, stats, _) = observe(
        &adapter,
        &mapping,
        &Checkpoint::start("crm", "holdings", "1"),
        &ObserveContext {
            tenant: &tenant(),
            source_id: "crm",
            batch_id: &batch_id,
            run_id: Some(&batch_id),
            limits: Limits {
                max_rows: 100,
                max_bytes: 1 << 20,
                timeout_ms: 5_000,
            },
            identity: &identity,
        },
    )
    .await
    .expect("the T0 export observes");
    assert!(stats.complete, "a whole landing export is a complete read");

    let run = |batch: &ObservationBatch| {
        let bytes = serde_json::to_vec(batch).unwrap();
        let client = &client;
        let mapping = &mapping;
        let version = version.clone();
        let batch = batch.clone();
        async move {
            reconcile_with(
                client,
                mapping,
                &version,
                &batch,
                &bytes,
                &ReconcileOptions {
                    tenant: &tenant(),
                    promoted: false,
                    source_id: "crm",
                    proposals: None,
                    source_complete: true,
                },
            )
            .await
            .expect("the shadow pass runs against a real server")
        }
    };

    let out = run(&batch).await;
    assert_eq!(out.ambiguous, 4);
    assert_eq!(out.missing_in_source, 2);

    let reported = t0_findings(&seeder, &version).await;
    let is_expected = |r: &(String, String, String)| {
        T0_EXPECTED
            .iter()
            .any(|(row, prop, v)| *row == r.0 && *prop == r.1 && *v == r.2)
    };
    let tp = reported.iter().filter(|r| is_expected(r)).count();
    let fp = reported.len() - tp;
    let fn_ = T0_EXPECTED.len() - tp;
    println!(
        "live reconcile precision={:.3} recall={:.3} tp={tp} fp={fp} fn={fn_}",
        tp as f64 / (tp + fp).max(1) as f64,
        tp as f64 / (tp + fn_).max(1) as f64
    );
    assert_eq!(
        fp,
        0,
        "false positives: {:#?}",
        reported
            .iter()
            .filter(|r| !is_expected(r))
            .collect::<Vec<_>>()
    );
    assert_eq!(fn_, 0, "reported: {reported:#?}");
    assert!(
        !reported.iter().any(|r| r.0 == "42|7" || r.0 == "44|7"),
        "the clean rows must be silent"
    );

    // Both sides resolve from the real server.
    let body = seeder
        .get(&format!(
            "/v1/versions/{version}/findings?rule_prefix=matrix."
        ))
        .await;
    let differ = body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["finding"]["detail"]["verdict"] == "differ")
        .expect("trap 10 filed");
    let evidence_id = differ["finding"]["detail"]["source"]["evidence_id"]
        .as_str()
        .unwrap();
    let manifest = client
        .get_evidence(evidence_id)
        .await
        .expect("the cited artifact resolves");
    assert_eq!(manifest.kind, ArtifactKind::Observations);
    let claim_id = differ["finding"]["detail"]["ledger"]["claim_id"]
        .as_str()
        .unwrap();
    let claim = seeder.get(&format!("/v1/claims/{claim_id}")).await;
    assert_eq!(claim["claim"]["value"], "90000", "the cited ledger claim");

    // Shadow: canon did not move.
    assert_eq!(client.head_seq(&version).await.unwrap(), head_before);

    // Replay: the SERVER's content identity, not the mock's.
    let again = run(&batch).await;
    assert_eq!(
        again.findings_filed, out.findings_filed,
        "the pass itself is deterministic"
    );
    let replayed = t0_findings(&seeder, &version).await;
    assert_eq!(
        replayed.len(),
        reported.len(),
        "a replayed pass must file nothing twice on a real server"
    );
}

/// Rollback on a real ledger, for a CHAIN. Two promoted passes write
/// 90500 then 91000 over a document's 90000; one rollback must leave exactly
/// one current fact, reading 90000, superseding the head — because the
/// server's `resolve_slice` returns every unsuperseded claim, and an
/// oldest-first undo would have left two.
#[tokio::test]
async fn a_rollback_of_a_chain_leaves_one_current_fact_on_a_real_ledger() {
    let Some(client) = live() else { return };
    let seeder = Seeder::new().unwrap();
    let version = seeder.version().await;
    seeder
        .fact(&version, "shareholder.43", "shares_outstanding", "90000")
        .await;

    let mapping = recon_mapping(
        "authoritative",
        "  authority:\n    - { property: shares_outstanding, precedence: source_over_document }\n",
    );
    let ledger = MemLedger::default();

    let first = recon_batch(
        "live-chain-1",
        vec![recon_observation("43", "90500", ChangeKind::Update)],
    );
    let out1 = live_reconcile(&client, &mapping, &version, &first, true, Some(&ledger)).await;
    assert_eq!(out1.proposals, 1);

    let mut o2 = recon_observation("43", "91000", ChangeKind::Update);
    o2.origin.event_position = Some("lsn/43-2".into());
    let second = recon_batch("live-chain-2", vec![o2]);
    let out2 = live_reconcile(&client, &mapping, &version, &second, true, Some(&ledger)).await;
    assert_eq!(
        out2.proposals, 1,
        "the second pass supersedes the connector's own claim"
    );

    let records = ledger.records();
    assert_eq!(records.len(), 2);
    let head_claim = records[1].claim_id.clone();

    let out = rollback(
        &client,
        &RollbackRequest {
            tenant: &tenant(),
            source_id: "crm",
            mapping_ref: "captable-live@1",
            decision_id: "live-chain-rollback",
            proposals: &records,
            ledger: &ledger,
        },
    )
    .await
    .expect("rollback runs against a real server");
    assert_eq!(out.superseded, 1, "one correction for the chain");
    assert_eq!(out.proposals_covered, 2);

    let current: Vec<_> = client
        .slice_facts(&version, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|f| f.subject == "shareholder.43" && f.key == "shares_outstanding")
        .collect();
    assert_eq!(
        current.len(),
        1,
        "the server's resolve_slice must see exactly one current claim: {current:#?}"
    );
    assert_eq!(current[0].value, "90000", "the document's value is back");
    assert_eq!(current[0].origin_kind.as_deref(), Some("rollback"));

    // Source-origin evidence survives supersession: the head the rollback
    // superseded is still readable, still says it was the connector's.
    let superseded = seeder.get(&format!("/v1/claims/{head_claim}")).await;
    assert_eq!(superseded["superseded"], true);
    assert_eq!(superseded["claim"]["origin"]["kind"], "connector");
}
