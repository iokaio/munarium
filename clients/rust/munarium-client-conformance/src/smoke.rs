// SPDX-License-Identifier: Apache-2.0
//! Per-plane smokes beyond the 7 core scenarios: ingest streaming, retrieval
//! with the ProvenanceEnvelope, shape publication, the runbook approve flow,
//! and the provider gateway surface. Requires the postgres store.
//!
//! Index builds are REST-only, so the REST client drives the full flow; the
//! gRPC client then proves cross-plane visibility (same tenant, same runs,
//! same index) and that `build_index` reports `Unsupported` honestly.

use munarium_client::{
    chunks_from_vec, dto, ChunkSource, MunariumClient, MunariumError, SourceMeta,
};
use sha2::Digest as _;

const SHAPE_YAML: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: Shape
metadata: { name: climoke-tickets, version: 1 }
spec:
  fact:
    schema:
      type: object
      properties:
        subject: { type: string }
        key: { type: string }
        value: { type: string, minLength: 1 }
      required: [subject, key, value]
    supersession:
      identity: [subject, key]
  chunking: { max_chars: 400 }
  indexing: { rrf_k: 60 }
"#;

const RUNBOOK_YAML: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: climoke-reindex, version: 1 }
spec:
  shape: climoke-tickets@1
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
"#;

const PROVIDER_YAML: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: { name: climoke-anthropic }
spec:
  provider: anthropic
  models: { complete: [claude-sonnet-4-6] }
  credentialRef: { env: MUNARIUM_SECRET_CLISMOKE_UNSET }
"#;

const SHAPE_REF: &str = "climoke-tickets@1";

pub(crate) fn nonce() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

pub(crate) struct Report {
    pub(crate) failed: usize,
}

impl Report {
    pub(crate) fn check(&mut self, name: &str, outcome: Result<(), String>) {
        match outcome {
            Ok(()) => println!("  PASS  {name}"),
            Err(msg) => {
                self.failed += 1;
                println!("  FAIL  {name}\n        {msg}");
            }
        }
    }
}

/// A REPLAYABLE two-chunk source — the factory shape the transports need
/// to retry a transient upload failure.
fn two_chunk_source(bytes: Vec<u8>) -> ChunkSource {
    let mid = bytes.len() / 2;
    chunks_from_vec(vec![bytes[..mid].to_vec(), bytes[mid..].to_vec()])
}

pub async fn run(rest: &MunariumClient, grpc: Option<&MunariumClient>) -> usize {
    let mut r = Report { failed: 0 };
    println!("munarium-client plane smokes (postgres store required)");
    println!("{}", "-".repeat(56));

    // A lineage to witness publications and ingests in.
    let version = match rest.commands.create_version(Default::default(), None).await {
        Ok(v) => v.version_id,
        Err(e) => {
            println!("  FAIL  smoke.setup\n        create_version: {e}");
            return r.failed + 1;
        }
    };

    // -- shapes ------------------------------------------------------------
    let outcome = match rest.runbooks.apply_shape(SHAPE_YAML, Some(&version)).await {
        Ok(resp) if resp.shape_ref == SHAPE_REF && resp.event_id.is_some() => Ok(()),
        Ok(resp) => Err(format!(
            "expected {SHAPE_REF} with a publication event_id, got {resp:?}"
        )),
        Err(e) => Err(e.to_string()),
    };
    r.check("shapes.apply-and-witness (REST)", outcome);

    // -- ingest: streamed, content-addressed, idempotent by content --------
    let content = format!(
        "alpha ticket {n}: the printer prints. beta ticket {n}: the printer does not print.",
        n = nonce()
    );
    let bytes = content.clone().into_bytes();
    let declared = hex::encode(sha2::Sha256::digest(&bytes));
    let meta = SourceMeta {
        declared_sha256: declared.clone(),
        media_type: Some("text/plain".into()),
        filename: Some("climoke.txt".into()),
        shape_ref: Some(SHAPE_REF.into()),
    };
    let outcome = match rest
        .ingest
        .put_source(meta.clone(), two_chunk_source(bytes.clone()))
        .await
    {
        Ok(first) if first.content_hash == declared && !first.already_existed => {
            match rest
                .ingest
                .put_source(meta.clone(), two_chunk_source(bytes.clone()))
                .await
            {
                Ok(second) if second.already_existed => Ok(()),
                Ok(second) => Err(format!("re-upload must be already_existed, got {second:?}")),
                Err(e) => Err(e.to_string()),
            }
        }
        Ok(first) => Err(format!(
            "fresh upload must return the declared hash and already_existed=false, got {first:?}"
        )),
        Err(e) => Err(e.to_string()),
    };
    r.check("ingest.put-source-streamed (REST)", outcome);

    let outcome = match rest
        .ingest
        .record_ingest(
            &version,
            dto::RecordIngestRequest {
                content_hash: declared.clone(),
                shape_ref: Some(SHAPE_REF.into()),
            },
        )
        .await
    {
        Ok(resp) if !resp.event_id.is_empty() && resp.seq > 0 => Ok(()),
        Ok(resp) => Err(format!("expected a recorded event, got {resp:?}")),
        Err(e) => Err(e.to_string()),
    };
    r.check("ingest.record (REST)", outcome);

    // -- retrieval: build + search, envelope is load-bearing ---------------
    let outcome = match rest.retrieval.build_index(SHAPE_REF, Some(&version)).await {
        Ok(iv) if iv.active && !iv.index_version.is_empty() => Ok(()),
        Ok(iv) => Err(format!("expected an active index, got {iv:?}")),
        Err(e) => Err(e.to_string()),
    };
    r.check("retrieval.build-index (REST)", outcome);

    let search_req = dto::SearchRequest {
        query: Some("printer".into()),
        shape_ref: Some(SHAPE_REF.into()),
        top_k: Some(5),
        ..Default::default()
    };
    let outcome = match rest.retrieval.search(search_req.clone()).await {
        Ok(resp) if !resp.hits.is_empty() && !resp.envelope.index_version.is_empty() => Ok(()),
        Ok(resp) => Err(format!(
            "expected hits with a ProvenanceEnvelope, got {} hits, envelope {:?}",
            resp.hits.len(),
            resp.envelope
        )),
        Err(e) => Err(e.to_string()),
    };
    r.check("retrieval.search-with-envelope (REST)", outcome);

    // -- runbooks: run -> awaiting_approval -> approve -> done -------------
    let run_outcome: Result<String, String> = async {
        rest.runbooks
            .apply_runbook(RUNBOOK_YAML)
            .await
            .map_err(|e| e.to_string())?;
        let run = rest
            .runbooks
            .run_runbook("climoke-reindex", Some(&version))
            .await
            .map_err(|e| e.to_string())?;
        if run.state != "awaiting_approval" {
            return Err(format!(
                "run must pause at the approval gate, got '{}'",
                run.state
            ));
        }
        let status = rest
            .runbooks
            .get_run(&run.run_id)
            .await
            .map_err(|e| e.to_string())?;
        let awaiting = status
            .steps
            .iter()
            .find(|s| s.state == "awaiting_approval")
            .ok_or("no step awaiting approval")?;
        let approved = rest
            .runbooks
            .approve_step(&run.run_id, awaiting.ordinal)
            .await
            .map_err(|e| e.to_string())?;
        if approved.state != "done" {
            return Err(format!(
                "approved run must complete, got '{}'",
                approved.state
            ));
        }
        Ok(run.run_id)
    }
    .await;
    let (outcome, run_id) = match run_outcome {
        Ok(id) => (Ok(()), Some(id)),
        Err(e) => (Err(e), None),
    };
    r.check("runbooks.approve-flow (REST)", outcome);

    // -- providers: BYOK surface (no live LLM call — fail-closed creds) ----
    let outcome = match rest.providers.apply_config(PROVIDER_YAML).await {
        Ok(resp) if resp.config_name == "climoke-anthropic" => {
            // Health must ANSWER (healthy or typed failure) — never hang/crash.
            match rest.providers.health("climoke-anthropic").await {
                Ok(h) => {
                    println!(
                        "        provider health: healthy={} ({})",
                        h.healthy, h.detail
                    );
                    Ok(())
                }
                Err(MunariumError::Provider { detail }) => {
                    println!("        provider health: typed provider-error ({detail})");
                    Ok(())
                }
                Err(e) => Err(format!("expected health or provider-error, got {e}")),
            }
        }
        Ok(resp) => Err(format!("unexpected config name {resp:?}")),
        Err(e) => Err(e.to_string()),
    };
    r.check("providers.apply-and-health (REST)", outcome);

    // -- gRPC cross-plane half ---------------------------------------------
    if let Some(grpc) = grpc {
        let outcome = match grpc.runbooks.apply_shape(SHAPE_YAML, None).await {
            Ok(resp) if resp.shape_ref == SHAPE_REF => Ok(()),
            Ok(resp) => Err(format!("expected {SHAPE_REF}, got {resp:?}")),
            Err(e) => Err(e.to_string()),
        };
        r.check("shapes.apply (gRPC)", outcome);

        let content2 = format!("gamma ticket {}: the scanner scans.", nonce());
        let bytes2 = content2.into_bytes();
        let declared2 = hex::encode(sha2::Sha256::digest(&bytes2));
        let outcome = match grpc
            .ingest
            .put_source(
                SourceMeta {
                    declared_sha256: declared2.clone(),
                    media_type: Some("text/plain".into()),
                    filename: Some("climoke-grpc.txt".into()),
                    shape_ref: Some(SHAPE_REF.into()),
                },
                two_chunk_source(bytes2),
            )
            .await
        {
            Ok(resp) if resp.content_hash == declared2 => Ok(()),
            Ok(resp) => Err(format!("hash mismatch: {resp:?}")),
            Err(e) => Err(e.to_string()),
        };
        r.check("ingest.put-source-streamed (gRPC)", outcome);

        let outcome = match grpc
            .ingest
            .record_ingest(
                &version,
                dto::RecordIngestRequest {
                    content_hash: declared2,
                    shape_ref: Some(SHAPE_REF.into()),
                },
            )
            .await
        {
            Ok(resp) if resp.seq > 0 => Ok(()),
            Ok(resp) => Err(format!("expected a recorded event, got {resp:?}")),
            Err(e) => Err(e.to_string()),
        };
        r.check("ingest.record (gRPC)", outcome);

        let outcome = match grpc.retrieval.search(search_req).await {
            Ok(resp) if !resp.hits.is_empty() && !resp.envelope.index_version.is_empty() => Ok(()),
            Ok(resp) => Err(format!(
                "expected hits with a ProvenanceEnvelope, got {} hits",
                resp.hits.len()
            )),
            Err(e) => Err(e.to_string()),
        };
        r.check("retrieval.search-with-envelope (gRPC)", outcome);

        let outcome = match grpc.retrieval.build_index(SHAPE_REF, None).await {
            Err(MunariumError::Unsupported { .. }) => Ok(()),
            other => Err(format!(
                "build_index must be honestly Unsupported on gRPC, got {other:?}"
            )),
        };
        r.check("retrieval.build-index-unsupported (gRPC)", outcome);

        if let Some(run_id) = &run_id {
            // version_id now rides the wire (C5) — no more None on gRPC.
            let outcome = match grpc.runbooks.get_run(run_id).await {
                Ok(status)
                    if status.state == "done"
                        && status.version_id.as_deref() == Some(&version[..]) =>
                {
                    Ok(())
                }
                Ok(status) => Err(format!(
                    "expected done + version_id {version}, got '{}' / {:?}",
                    status.state, status.version_id
                )),
                Err(e) => Err(e.to_string()),
            };
            r.check("runbooks.cross-plane-get-run (gRPC)", outcome);
        }

        let outcome = match grpc.providers.health("climoke-anthropic").await {
            Ok(_) | Err(MunariumError::Provider { .. }) => Ok(()),
            Err(e) => Err(format!("expected health or provider-error, got {e}")),
        };
        r.check("providers.health (gRPC)", outcome);

        // Invocation provenance is accepted on gRPC (C5): the fail-closed
        // credential error proves the call reached the provider gateway
        // rather than being refused as an unsupported transport.
        let outcome = match grpc
            .providers
            .complete(
                "climoke-anthropic",
                dto::CompleteRequest {
                    prompt: Some("hi".into()),
                    version_id: Some(version.clone()),
                    ..Default::default()
                },
            )
            .await
        {
            Err(MunariumError::Provider { .. }) => Ok(()),
            Err(MunariumError::Unsupported { detail }) => Err(format!(
                "provenance must be supported on gRPC now: {detail}"
            )),
            other => Err(format!(
                "expected a fail-closed provider error, got {other:?}"
            )),
        };
        r.check("providers.complete-with-provenance (gRPC)", outcome);
    }

    println!("{}", "-".repeat(56));
    println!("smokes: {} failed\n", r.failed);
    r.failed
}
