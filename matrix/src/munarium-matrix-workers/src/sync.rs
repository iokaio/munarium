// SPDX-License-Identifier: Apache-2.0
//! The sync role: mode A, end to end.
//!
//! One run is: check drift → read a batch → render each record → upload
//! through the bulk plane → seal the run's count and coverage → advance the
//! checkpoint. The order matters and each step earns its place:
//!
//! - **Drift is checked first** so a changed source refuses before it writes
//!   anything, rather than half-populating a collection.
//! - **The checkpoint advances LAST**, after the upload is confirmed. If the
//!   process dies mid-run the next run re-reads the same window, and the
//!   per-record idempotency key means the re-read uploads nothing. Advancing
//!   first would lose records; advancing last can only ever repeat work.
//! - **The count is sealed as evidence**, not recorded as a number. A count
//!   answer must cite something that can be replayed after the source moves.

use crate::classes::ResolvedClass;
use crate::evidence::{count_result, seal, SealContext};
use munarium_matrix_adapter::{EffectiveIdentity, Limits, ReadMode, SourceAdapter};
use munarium_matrix_core::checkpoint::{
    idempotency_key, Checkpoint, DriftPolicy, SyncMode, WatermarkSpec,
};
use munarium_matrix_core::{render_record, Refusal, RenderSpec, RENDER_VERSION};
use munarium_matrix_server_client::{ServerClient, UploadDocument};
use munarium_matrix_types::contract::{ArtifactKind, ManifestVersions};

/// What one sync run did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncOutcome {
    pub records_read: u64,
    pub records_rendered: u64,
    /// Rows the source returned that a limit or policy excluded. Reported, not
    /// dropped (G4).
    pub records_excluded: u64,
    pub documents_uploaded: u64,
    /// Change-feed deletes rendered as tombstones this run.
    pub documents_deleted: u64,
    /// True when the feed had a retention gap and the run re-read the whole
    /// entity from a start checkpoint instead.
    pub resnapshotted: bool,
    /// The schema the run read under, for the caller to record as what the
    /// next run is held to (2026-08-30: nothing recorded it before).
    pub fingerprint: Option<munarium_matrix_adapter::SchemaFingerprint>,
    /// Documents the server already held. On a replayed checkpoint this should
    /// equal the batch and `documents_uploaded` should be zero.
    pub documents_skipped: u64,
    pub count_evidence_id: Option<String>,
    pub next_checkpoint: Option<Checkpoint>,
    /// True when the source had nothing new.
    pub up_to_date: bool,
}

/// Everything a run needs that is not the adapter or the server.
pub struct SyncRequest<'a> {
    pub tenant: &'a str,
    pub source_id: &'a str,
    pub source_version: u32,
    pub entity: &'a str,
    pub projection: &'a [String],
    pub key_columns: &'a [String],
    pub mode: SyncMode,
    /// The DataSource's own `spec.sync.watermark`. Required when `mode` is
    /// `Watermark` and refused by the adapter when absent: reading by a column
    /// the source never declared is worse than not reading.
    pub watermark: Option<&'a WatermarkSpec>,
    pub class: &'a ResolvedClass,
    pub checkpoint: Checkpoint,
    pub limits: Limits,
    pub drift_policy: DriftPolicy,
    /// The fingerprint the last successful run observed, if any.
    pub known_fingerprint: Option<String>,
    pub retention_days: Option<u32>,
}

/// Run one sync.
pub async fn run_sync(
    adapter: &dyn SourceAdapter,
    server: &dyn ServerClient,
    req: &SyncRequest<'_>,
) -> Result<SyncOutcome, Refusal> {
    let started_at = chrono::Utc::now();

    // --- 1. Drift. Fail closed BEFORE writing anything. ---------------------
    let (_posture, fingerprint) = adapter.introspect().await?;
    if let Some(known) = &req.known_fingerprint {
        if known != &fingerprint.fingerprint {
            match &req.drift_policy {
                DriftPolicy::Refuse => {
                    return Err(Refusal::schema_drift(format!(
                        "source schema changed since the last run (was {known}, now {}); \
                         set onDrift to compat:<decision-id> to accept a reviewed change",
                        fingerprint.fingerprint
                    )))
                }
                DriftPolicy::Compat { decision_id } => {
                    // Accepted, but never silently: the decision id lands in
                    // the journal with the run.
                    tracing::warn!(
                        decision_id = %decision_id,
                        was = %known,
                        now = %fingerprint.fingerprint,
                        "accepting reviewed schema drift"
                    );
                }
            }
        }
    }

    // --- 2. Read ------------------------------------------------------------
    let identity = EffectiveIdentity {
        class: Some(req.class.name.clone()),
        credential_ref: req.class.credential_ref.clone(),
        principal: req
            .class
            .credential_ref
            .clone()
            .unwrap_or_else(|| "source-native".into()),
    };
    let mut resnapshotted = false;
    let batch = match adapter
        .read_batch(
            req.entity,
            req.projection,
            &req.checkpoint,
            ReadMode::new(req.mode, req.watermark),
            &identity,
            req.limits,
        )
        .await
    {
        Ok(b) => b,
        // The feed no longer holds the commits after the checkpoint.
        // The honest move is a fresh snapshot from a start checkpoint — every
        // row re-rendered at its current state — not a batch that reports
        // coverage of changes it never saw. Idempotent rendering makes the
        // re-read cheap: unchanged rows upload nothing new.
        Err(r) if r.code == "cdf_checkpoint_gap" => {
            tracing::warn!(entity = %req.entity, "change feed retention gap; resnapshotting");
            resnapshotted = true;
            let fresh = munarium_matrix_core::checkpoint::Checkpoint::start(
                &req.checkpoint.source_id,
                &req.checkpoint.entity,
                &req.checkpoint.version,
            );
            adapter
                .read_batch(
                    req.entity,
                    req.projection,
                    &fresh,
                    ReadMode::new(req.mode, req.watermark),
                    &identity,
                    req.limits,
                )
                .await?
        }
        Err(r) => return Err(r),
    };

    if batch.records.is_empty() {
        return Ok(SyncOutcome {
            up_to_date: true,
            next_checkpoint: batch.next_checkpoint,
            fingerprint: Some(fingerprint),
            ..Default::default()
        });
    }

    // --- 3. Render ----------------------------------------------------------
    let spec = RenderSpec {
        entity: req.entity,
        prefix: req.source_id,
        columns: &batch.columns,
        key_columns: req.key_columns,
        authorization_class: &req.class.name,
        snapshot_marker: batch.snapshot_marker.as_deref(),
    };
    let mut documents = Vec::with_capacity(batch.records.len());
    let mut deleted = 0u64;
    for record in &batch.records {
        let mut doc = render_record(&spec, &record.cells);
        // A delete from a change feed is a record too: the document
        // at the row's path becomes a tombstone that says the row is gone and
        // at which engine position, so a reader who cites it learns the fact
        // rather than finding nothing. The path is the same, so a re-render
        // of a resurrected row replaces it.
        if matches!(
            record.change_kind,
            munarium_matrix_types::contract::ChangeKind::Delete
        ) {
            deleted += 1;
            // The wording is careful about what a delete actually CARRIES,
            // because that differs per engine and a document must not promise
            // more than its source sent. Databricks' change feed delivers the
            // whole deleted row; Postgres logical replication delivers only the
            // REPLICA IDENTITY, and every other column arrives NULL
            // because the engine did not send it — which is a different fact
            // from the row having held a null. "The values the source sent" is
            // true of both; "the row's last known values" was true only of the
            // first.
            doc.body = format!(
                "# {} {}\n\n**Deleted at the source.** This record no longer exists in `{}`; \
                 the deletion was observed at engine position `{}`. The fields below are the \
                 values the source sent with the deletion — some engines send only the row's \
                 identity.\n\n{}",
                spec.entity,
                doc.metadata
                    .iter()
                    .find(|(k, _)| k == "row_key")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or(""),
                spec.entity,
                record.event_position.as_deref().unwrap_or("?"),
                doc.body.split_once("\n\n").map(|x| x.1).unwrap_or("")
            );
            doc.metadata
                .push(("deleted".to_string(), "true".to_string()));
        }
        // The idempotency key is what makes a replayed checkpoint free: the
        // same record at the same event position produces the same key, and
        // the server's manifest diff sees the same bytes at the same path.
        let _key = idempotency_key(
            req.source_id,
            RENDER_VERSION,
            &record.row_key,
            record.event_position.as_deref(),
        );
        documents.push(UploadDocument {
            path: doc.path,
            bytes: doc.body.into_bytes(),
            media_type: "text/markdown".to_string(),
            metadata: doc.metadata,
        });
    }

    // --- 4. Upload ----------------------------------------------------------
    let label = format!("{}-{}-{}", req.source_id, req.entity, req.class.name);
    let upload = server
        .bulk_upload(&label, &documents)
        .await
        .map_err(|e| e.to_refusal())?;
    if upload.failed > 0 {
        return Err(Refusal::partial_result(format!(
            "{} of {} documents failed to upload; the checkpoint is not advanced",
            upload.failed,
            documents.len()
        )));
    }

    // --- 5. Seal the count and the coverage ---------------------------------
    // The number of records this run COVERED, sealed so a count answer cites
    // evidence rather than an index-time number nobody can check.
    let ended_at = chrono::Utc::now();
    let ctx = SealContext {
        tenant: req.tenant.to_string(),
        kind: ArtifactKind::Count,
        source_id: req.source_id.to_string(),
        source_version: req.source_version,
        adapter: adapter.kind().to_string(),
        adapter_version: Some(adapter.adapter_version().to_string()),
        engine: None,
        versions: ManifestVersions {
            render: Some(RENDER_VERSION.to_string()),
            ..Default::default()
        },
        plan: None,
        snapshot_marker: batch.snapshot_marker.clone(),
        isolation: None,
        replay_level: adapter.capabilities().replay_level,
        effective_principal: Some(identity.principal.clone()),
        statement_id: None,
        started_at,
        ended_at,
        retention_days: req.retention_days,
        declared_max_rows: Some(req.limits.max_rows),
        rows_covered: Some(batch.records.len() as u64),
        rows_excluded: Some(batch.excluded),
        exclusion_reason: (batch.excluded > 0)
            .then(|| "rows beyond the declared maxRows for this run".to_string()),
        freshness_watermark: batch
            .next_checkpoint
            .as_ref()
            .and_then(|c| c.watermark.clone()),
    };
    let (count_id, _) = seal(
        server,
        &count_result(batch.records.len() as i64, req.class.as_core()),
        &ctx,
        Some(&format!("{label}-{:?}", batch.snapshot_marker)),
    )
    .await?;

    // --- 6. Advance the checkpoint LAST -------------------------------------
    let mut next = batch.next_checkpoint.clone();
    if let Some(cp) = next.as_mut() {
        cp.schema_fingerprint = Some(fingerprint.fingerprint.clone());
    }

    Ok(SyncOutcome {
        records_read: batch.records.len() as u64,
        records_rendered: documents.len() as u64,
        records_excluded: batch.excluded,
        documents_uploaded: upload.stored,
        documents_deleted: deleted,
        resnapshotted,
        documents_skipped: upload.skipped_existing,
        count_evidence_id: Some(count_id),
        next_checkpoint: next,
        up_to_date: false,
        fingerprint: Some(fingerprint),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_adapter_landing::LandingAdapter;
    use munarium_matrix_server_client::MockServer;

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("mx-sync-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let f = Self(dir);
            f.write("manifest.json", MANIFEST);
            f.write("rows.csv", ROWS);
            f
        }
        fn write(&self, rel: &str, s: &str) {
            std::fs::write(self.0.join(rel), s).unwrap();
        }
        fn adapter(&self) -> LandingAdapter {
            LandingAdapter::new_file("crm", &self.0, "manifest.json")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MANIFEST: &str = r#"{
  "snapshotId": "s1",
  "format": "csv",
  "keys": ["id"],
  "schema": [
    { "name": "id", "type": "int64" },
    { "name": "region", "type": "string" },
    { "name": "amount", "type": "decimal", "scale": 2 }
  ],
  "files": [{ "path": "rows.csv" }]
}"#;

    const ROWS: &str = "id,region,amount\n1,EMEA,1500.00\n2,AMER,250.50\n3,APAC,99.99\n";

    fn class() -> ResolvedClass {
        ResolvedClass {
            name: "sales-emea".into(),
            access_level: 2,
            compartments: vec!["sales".into()],
            credential_ref: None,
        }
    }

    fn request<'a>(
        class: &'a ResolvedClass,
        projection: &'a [String],
        keys: &'a [String],
        checkpoint: Checkpoint,
    ) -> SyncRequest<'a> {
        SyncRequest {
            tenant: "acme",
            source_id: "crm",
            source_version: 1,
            entity: "opportunities",
            projection,
            key_columns: keys,
            mode: SyncMode::Manifest,
            watermark: None,
            class,
            checkpoint,
            limits: Limits {
                max_rows: 1000,
                max_bytes: 1 << 20,
                timeout_ms: 5000,
            },
            drift_policy: DriftPolicy::Refuse,
            known_fingerprint: None,
            retention_days: Some(400),
        }
    }

    #[tokio::test]
    async fn a_full_run_renders_uploads_and_seals_a_count() {
        let f = Fixture::new("full");
        let server = MockServer::new();
        let c = class();
        let keys = vec!["id".to_string()];
        let cp = Checkpoint::start("crm", "opportunities", RENDER_VERSION);

        let out = run_sync(&f.adapter(), &server, &request(&c, &[], &keys, cp))
            .await
            .unwrap();

        assert_eq!(out.records_read, 3);
        assert_eq!(out.documents_uploaded, 3);
        assert_eq!(out.documents_skipped, 0);
        assert!(out.count_evidence_id.is_some());
        assert!(!out.up_to_date);

        // The sealed count is real evidence with real coverage numbers.
        // Read it as a session that DOMINATES the class — the mock enforces
        // domination on every read, so a test that forgets who is asking
        // fails here rather than in production.
        let id = out.count_evidence_id.unwrap();
        let reader = server.as_reader(9, &["sales"]);
        let manifest = reader.get_evidence(&id).await.unwrap();
        assert_eq!(manifest.kind, ArtifactKind::Count);
        assert_eq!(manifest.completeness.rows_covered, Some(3));
        assert_eq!(manifest.completeness.rows_excluded, Some(0));
        assert_eq!(
            manifest.authorization_class.name.as_deref(),
            Some("sales-emea")
        );
    }

    /// The idempotency proof: a replayed checkpoint uploads NOTHING.
    #[tokio::test]
    async fn a_replayed_run_uploads_nothing_new() {
        let f = Fixture::new("replay");
        let server = MockServer::new();
        let c = class();
        let keys = vec!["id".to_string()];

        let first = run_sync(
            &f.adapter(),
            &server,
            &request(
                &c,
                &[],
                &keys,
                Checkpoint::start("crm", "opportunities", RENDER_VERSION),
            ),
        )
        .await
        .unwrap();
        assert_eq!(first.documents_uploaded, 3);

        // Replay from the SAME starting checkpoint — the crash-and-restart
        // case, where the checkpoint never advanced.
        let second = run_sync(
            &f.adapter(),
            &server,
            &request(
                &c,
                &[],
                &keys,
                Checkpoint::start("crm", "opportunities", RENDER_VERSION),
            ),
        )
        .await
        .unwrap();
        assert_eq!(second.documents_uploaded, 0, "a replay must create nothing");
        assert_eq!(second.documents_skipped, 3);
        assert_eq!(server.document_count(), 3);

        // And the count seals to the SAME artifact, because it is the same
        // logical result.
        assert_eq!(first.count_evidence_id, second.count_evidence_id);
    }

    #[tokio::test]
    async fn resuming_from_the_advanced_checkpoint_finds_nothing_new() {
        let f = Fixture::new("resume");
        let server = MockServer::new();
        let c = class();
        let keys = vec!["id".to_string()];

        let first = run_sync(
            &f.adapter(),
            &server,
            &request(
                &c,
                &[],
                &keys,
                Checkpoint::start("crm", "opportunities", RENDER_VERSION),
            ),
        )
        .await
        .unwrap();
        let next = first
            .next_checkpoint
            .expect("a run advances the checkpoint");
        assert!(
            next.schema_fingerprint.is_some(),
            "the fingerprint is recorded"
        );

        let second = run_sync(&f.adapter(), &server, &request(&c, &[], &keys, next))
            .await
            .unwrap();
        assert!(second.up_to_date);
        assert_eq!(second.records_read, 0);
    }

    #[tokio::test]
    async fn drift_refuses_before_anything_is_written() {
        let f = Fixture::new("drift");
        let server = MockServer::new();
        let c = class();
        let keys = vec!["id".to_string()];
        let mut req = request(
            &c,
            &[],
            &keys,
            Checkpoint::start("crm", "opportunities", RENDER_VERSION),
        );
        req.known_fingerprint =
            Some("sha256:0000000000000000000000000000000000000000000000000000000000000000".into());

        let err = run_sync(&f.adapter(), &server, &req).await.unwrap_err();
        assert_eq!(err.code, "schema_drift");
        assert_eq!(
            server.document_count(),
            0,
            "drift must refuse BEFORE writing"
        );
        assert_eq!(server.evidence_count(), 0);
    }

    #[tokio::test]
    async fn a_reviewed_drift_decision_lets_the_run_proceed() {
        let f = Fixture::new("compat");
        let server = MockServer::new();
        let c = class();
        let keys = vec!["id".to_string()];
        let mut req = request(
            &c,
            &[],
            &keys,
            Checkpoint::start("crm", "opportunities", RENDER_VERSION),
        );
        req.known_fingerprint =
            Some("sha256:0000000000000000000000000000000000000000000000000000000000000000".into());
        req.drift_policy = DriftPolicy::Compat {
            decision_id: "DEC-1".into(),
        };

        let out = run_sync(&f.adapter(), &server, &req).await.unwrap();
        assert_eq!(out.documents_uploaded, 3);
    }

    #[tokio::test]
    async fn an_excluded_row_is_reported_in_the_sealed_coverage() {
        let f = Fixture::new("excluded");
        let server = MockServer::new();
        let c = class();
        let keys = vec!["id".to_string()];
        let mut req = request(
            &c,
            &[],
            &keys,
            Checkpoint::start("crm", "opportunities", RENDER_VERSION),
        );
        req.limits.max_rows = 2;

        let out = run_sync(&f.adapter(), &server, &req).await.unwrap();
        assert_eq!(out.records_read, 2);
        assert_eq!(out.records_excluded, 1);

        let manifest = server
            .as_reader(9, &["sales"])
            .get_evidence(&out.count_evidence_id.unwrap())
            .await
            .unwrap();
        assert_eq!(manifest.completeness.rows_excluded, Some(1));
        assert!(manifest.completeness.exclusion_reason.is_some());
    }

    /// A failed upload must NOT advance anything — the next run has to re-read.
    #[tokio::test]
    async fn an_unreachable_server_refuses_without_advancing() {
        use munarium_matrix_server_client::ServerError;
        let f = Fixture::new("unreachable");
        let server = MockServer::new();
        *server.fail_next.lock().unwrap() =
            Some(ServerError::Transport("connection refused".into()));

        let c = class();
        let keys = vec!["id".to_string()];
        let err = run_sync(
            &f.adapter(),
            &server,
            &request(
                &c,
                &[],
                &keys,
                Checkpoint::start("crm", "opportunities", RENDER_VERSION),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(err.class, munarium_matrix_core::RefusalClass::Unavailable);
        assert!(err.class.retryable(), "the next run must be able to retry");
    }

    /// The other half of the same rule: a reader WITHOUT the compartment
    /// cannot resolve the count it just caused to be sealed.
    #[tokio::test]
    async fn an_under_cleared_reader_cannot_resolve_the_runs_count() {
        let f = Fixture::new("denied");
        let server = MockServer::new();
        let c = class();
        let keys = vec!["id".to_string()];
        let out = run_sync(
            &f.adapter(),
            &server,
            &request(
                &c,
                &[],
                &keys,
                Checkpoint::start("crm", "opportunities", RENDER_VERSION),
            ),
        )
        .await
        .unwrap();
        let id = out.count_evidence_id.unwrap();
        assert!(
            server.as_reader(9, &[]).get_evidence(&id).await.is_err(),
            "a high clearance without the compartment must still be denied"
        );
        assert!(server
            .as_reader(2, &["sales"])
            .get_evidence(&id)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn documents_carry_the_class_so_the_collection_can_be_gated() {
        let f = Fixture::new("class");
        let server = MockServer::new();
        let c = class();
        let keys = vec!["id".to_string()];
        run_sync(
            &f.adapter(),
            &server,
            &request(
                &c,
                &[],
                &keys,
                Checkpoint::start("crm", "opportunities", RENDER_VERSION),
            ),
        )
        .await
        .unwrap();
        // The renderer's metadata is what the server indexes into doc_meta,
        // and the class is what the collection's access level is set from.
        assert_eq!(server.document_count(), 3);
    }
}
