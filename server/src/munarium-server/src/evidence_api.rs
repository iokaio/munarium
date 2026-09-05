// SPDX-License-Identifier: Apache-2.0
//! The sealed evidence plane — seal, commit, resolve.
//!
//! An evidence artifact is the exact typed result an answer was computed from.
//! Munarium Matrix produces them; this module is where they become durable,
//! access-checked and replayable. The server owns the plane deliberately:
//! Matrix never writes a server table, so "seal" is a public, idempotent,
//! authorized API call and not a database reach-around.
//!
//! # The five decisions worth knowing
//!
//! **Both hashes are verified before anything is stored.** `artifact_hash`
//! must match the bytes received, and the bytes must be the length the
//! manifest declared. Verifying only the byte hash would let a re-serialized
//! result masquerade as a new one; not verifying it would let corruption
//! through. On mismatch nothing is written — an artifact whose bytes are not
//! the bytes it claims is worse than no artifact.
//!
//! **Two idempotency layers, named.** The request layer is the usual
//! `Idempotency-Key` header. The domain layer is
//! `(tenant, logical_result_hash, policy_version, authorization_class)` —
//! sealing the same logical result under the same policy and clearance is the
//! same seal, whatever the caller sends. The domain layer is what makes a
//! retrying Matrix replica safe; the header layer is what makes a retrying
//! HTTP client safe. They are different problems.
//!
//! **Authorization is per artifact, not per route.** The `evidence` scope says
//! "this principal participates in the evidence plane"; the artifact's
//! authorization class says "and only within this clearance". A resolving
//! session must *dominate* the class — at least the level, and every
//! compartment — and an under-cleared caller gets the same answer whether or
//! not the artifact exists, because "this exists and is above you" is itself a
//! disclosure.
//!
//! **The audit records that a read happened, never what was read.** An audit
//! table holding the regulated data it audits is a second copy of the problem.
//!
//! **Sealing is a data-plane write, so the uid rules apply.** Every call
//! carries `X-Munarium-Uid` like every other `/v1` request, and that uid is
//! what lands in the access log.

use std::sync::Arc;

use base64::Engine;
use munarium_core::evidence::{
    EvidenceAccess, EvidenceArtifact, EvidenceGrant, EvidenceManifest, EvidenceState, SealOutcome,
    DEFAULT_ROW_LIMIT, EVIDENCE_PATH_PREFIX, GRANT_TTL_SECS, INLINE_SEAL_MAX_BYTES, MAX_ROW_LIMIT,
    MEDIA_TYPE_CSV,
};
use munarium_core::sources::SourceKey;
use munarium_core::KernelError;

use crate::error::{ApiError, CustomError};

/// Local alias, matching every other api module in this crate.
pub type ApiResult<T> = std::result::Result<T, ApiError>;
use crate::state::AppState;

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The blob path for an artifact's bytes, inside the reserved keyspace.
///
/// The `evidence/` prefix is refused on every document ingress path, so a
/// document can never collide with an artifact. Note that authorization is
/// NEVER inferred from this path — it comes from the artifact row — which is
/// why the path can be this boring.
fn blob_path(evidence_id: &str) -> String {
    format!("{EVIDENCE_PATH_PREFIX}{evidence_id}")
}

fn new_evidence_id() -> String {
    format!("ev-{}", uuid::Uuid::new_v4().simple())
}

/// Parse and validate the manifest, and check the caller may seal at the class
/// it declares.
///
/// That last check is the one worth explaining. A principal may not seal an
/// artifact into a class it could not itself read: otherwise a low-clearance
/// service could mint high-clearance evidence, and every later reader would
/// trust a class nobody was ever authorized to assert. Sealing UP is the
/// forgery; sealing down is merely conservative.
fn parse_manifest(
    raw: &serde_json::Value,
    access: &munarium_access::AccessCtx,
) -> ApiResult<EvidenceManifest> {
    let mut manifest: EvidenceManifest = serde_json::from_value(raw.clone()).map_err(|e| {
        ApiError::Mesh(KernelError::InvalidInput(format!(
            "manifest is invalid: {e}"
        )))
    })?;
    manifest.validate().map_err(ApiError::Mesh)?;

    if manifest.tenant != access.tenant_id {
        return Err(ApiError::Mesh(KernelError::InvalidInput(format!(
            "manifest declares tenant '{}' but the token is scoped to '{}'",
            manifest.tenant, access.tenant_id
        ))));
    }
    if !manifest.authorization_class.dominated_by(
        access.level,
        &access.compartments,
        access.all_compartments,
    ) {
        return Err(ApiError::Custom(CustomError::evidence_forbidden()));
    }
    // The id is the server's to assign; a caller-supplied one is ignored
    // rather than honored, so a client cannot choose an id to collide with.
    manifest.evidence_id = None;
    Ok(manifest)
}

/// Verify the bytes are the bytes the manifest declared. Fails closed.
fn verify_bytes(manifest: &EvidenceManifest, bytes: &[u8]) -> ApiResult<()> {
    use sha2::Digest;
    if bytes.len() as i64 != manifest.bytes_len {
        return Err(ApiError::Custom(CustomError::evidence_hash_mismatch(
            format!(
                "manifest declares bytes_len {} but {} bytes arrived",
                manifest.bytes_len,
                bytes.len()
            ),
        )));
    }
    let actual = format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes)));
    if actual != manifest.artifact_hash {
        return Err(ApiError::Custom(CustomError::evidence_hash_mismatch(
            format!(
                "artifact_hash mismatch: manifest declares {}, the bytes hash to {actual}",
                manifest.artifact_hash
            ),
        )));
    }
    Ok(())
}

/// `POST /v1/evidence` — inline seal, or issue an upload grant.
pub async fn op_seal(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    raw_manifest: &serde_json::Value,
    bytes_base64: Option<&str>,
) -> ApiResult<munarium_api_types::SealEvidenceResponse> {
    let manifest = parse_manifest(raw_manifest, access)?;
    let tenant = manifest.tenant.clone();
    let evidence = state.evidence();

    // The domain idempotency layer, checked BEFORE any bytes are written. A
    // retrying replica must not re-upload a hundred megabytes to discover the
    // seal already happened.
    let domain_key = manifest.domain_key();
    if let Some(existing) = evidence.find_by_domain_key(&tenant, &domain_key).await? {
        return Ok(munarium_api_types::SealEvidenceResponse {
            evidence_id: existing.evidence_id,
            state: existing.state.as_str().to_string(),
            created: false,
            grant: None,
        });
    }

    let evidence_id = new_evidence_id();
    let path = blob_path(&evidence_id);

    match bytes_base64 {
        // ---- inline: verify, store, register committed, all in one call ----
        Some(b64) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    ApiError::Mesh(KernelError::InvalidInput(format!(
                        "bytes_base64 is not valid base64: {e}"
                    )))
                })?;
            if bytes.len() > INLINE_SEAL_MAX_BYTES {
                return Err(ApiError::Custom(CustomError::evidence_too_large(
                    bytes.len(),
                    INLINE_SEAL_MAX_BYTES,
                )));
            }
            verify_bytes(&manifest, &bytes)?;

            let now = now_rfc3339();
            let artifact = EvidenceArtifact {
                evidence_id: evidence_id.clone(),
                tenant: tenant.clone(),
                state: EvidenceState::Committed,
                manifest: manifest.clone(),
                blob_path: path.clone(),
                created_at: now.clone(),
                committed_at: Some(now),
            };
            // Bytes first, metadata second. The other order can leave a
            // committed row pointing at bytes that were never written — a
            // citation that resolves to nothing. This order can leave an
            // orphan blob, which costs storage and lies to nobody.
            let key = SourceKey::new(&tenant, &path, trim_hash(&manifest.artifact_hash))
                .map_err(ApiError::Mesh)?;
            state
                .source_store()
                .put(&key, &manifest.media_type, &bytes)
                .await?;

            let SealOutcome {
                evidence_id,
                created,
                ..
            } = evidence.register(&artifact, None).await?;
            // The state is THIS artifact's, not a constant: losing the
            // domain-key race to a concurrent GRANT-flow seal hands back the
            // winner's id, and that artifact is `pending` until its bytes
            // are committed. Reporting `committed` for it would send a
            // caller to cite evidence that does not resolve yet.
            let state = if created {
                EvidenceState::Committed
            } else {
                evidence
                    .get(&tenant, &evidence_id)
                    .await?
                    .map(|a| a.state)
                    .unwrap_or(EvidenceState::Committed)
            };
            Ok(munarium_api_types::SealEvidenceResponse {
                evidence_id,
                state: state.as_str().to_string(),
                created,
                grant: None,
            })
        }
        // ---- grant: register pending, hand back a single-use capability ----
        None => {
            let now = chrono::Utc::now();
            let artifact = EvidenceArtifact {
                evidence_id: evidence_id.clone(),
                tenant: tenant.clone(),
                state: EvidenceState::Pending,
                manifest,
                blob_path: path,
                created_at: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                committed_at: None,
            };
            let grant = EvidenceGrant {
                grant_id: format!("gr-{}", uuid::Uuid::new_v4().simple()),
                evidence_id: evidence_id.clone(),
                tenant: tenant.clone(),
                expires_at: (now + chrono::Duration::seconds(GRANT_TTL_SECS))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                used_at: None,
            };
            let outcome = evidence.register(&artifact, Some(&grant)).await?;
            Ok(munarium_api_types::SealEvidenceResponse {
                evidence_id: outcome.evidence_id,
                state: EvidenceState::Pending.as_str().to_string(),
                created: outcome.created,
                grant: outcome.grant.map(|g| munarium_api_types::EvidenceGrantDto {
                    grant_id: g.grant_id,
                    expires_at: g.expires_at,
                }),
            })
        }
    }
}

/// The object store keys blobs by content hash; the manifest carries the
/// `sha256:` prefix and `SourceKey` wants the bare digest.
fn trim_hash(hash: &str) -> &str {
    hash.strip_prefix("sha256:").unwrap_or(hash)
}

/// `PUT /v1/evidence/{id}/bytes` — spend a grant and store the bytes.
pub async fn op_put_bytes(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    evidence_id: &str,
    grant_id: &str,
    bytes: &[u8],
) -> ApiResult<()> {
    let tenant = &access.tenant_id;
    let evidence = state.evidence();
    let Some(artifact) = evidence.get(tenant, evidence_id).await? else {
        // Same answer as an invalid grant. A caller without a valid grant
        // learns nothing about which ids exist.
        return Err(ApiError::Custom(CustomError::evidence_grant_invalid()));
    };
    if !artifact.manifest.authorization_class.dominated_by(
        access.level,
        &access.compartments,
        access.all_compartments,
    ) {
        return Err(ApiError::Custom(CustomError::evidence_forbidden()));
    }

    // Verify BEFORE spending the grant. A caller who uploads corrupt bytes
    // should be able to retry; burning their single-use grant on a hash
    // mismatch would turn a client-side error into an unrecoverable one.
    verify_bytes(&artifact.manifest, bytes)?;

    let spent = evidence
        .consume_grant(tenant, evidence_id, grant_id, &now_rfc3339())
        .await?;
    if spent.is_none() {
        return Err(ApiError::Custom(CustomError::evidence_grant_invalid()));
    }

    let key = SourceKey::new(
        tenant,
        &artifact.blob_path,
        trim_hash(&artifact.manifest.artifact_hash),
    )
    .map_err(ApiError::Mesh)?;
    state
        .source_store()
        .put(&key, &artifact.manifest.media_type, bytes)
        .await?;
    Ok(())
}

/// `POST /v1/evidence/{id}/commit` — the grant flow's final step.
pub async fn op_commit(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    evidence_id: &str,
) -> ApiResult<munarium_api_types::CommitEvidenceResponse> {
    let tenant = &access.tenant_id;
    let evidence = state.evidence();
    let Some(artifact) = evidence.get(tenant, evidence_id).await? else {
        return Err(ApiError::Mesh(KernelError::NotFound {
            kind: "evidence",
            id: evidence_id.to_string(),
        }));
    };
    if !artifact.manifest.authorization_class.dominated_by(
        access.level,
        &access.compartments,
        access.all_compartments,
    ) {
        return Err(ApiError::Custom(CustomError::evidence_forbidden()));
    }

    // Re-read the stored bytes and verify both declared facts again. This is
    // the atomic step the plan asks for: commit is the moment the artifact
    // becomes citable, so it is the moment the claim "these bytes are that
    // hash" must be true — not the moment the upload happened to succeed.
    let key = SourceKey::new(
        tenant,
        &artifact.blob_path,
        trim_hash(&artifact.manifest.artifact_hash),
    )
    .map_err(ApiError::Mesh)?;
    // Only ABSENCE is the client's state (`evidence-not-committed`, 409:
    // upload first). A backend outage on the read is a storage error and
    // must not send the caller off to re-upload bytes that are there.
    let stored = state.source_store().get(&key).await.map_err(|e| match e {
        KernelError::NotFound { .. } => {
            ApiError::Custom(CustomError::evidence_not_committed(evidence_id))
        }
        other => ApiError::Mesh(other),
    })?;
    verify_bytes(&artifact.manifest, &stored)?;

    let committed = evidence.commit(tenant, evidence_id, &now_rfc3339()).await?;
    Ok(munarium_api_types::CommitEvidenceResponse {
        evidence_id: evidence_id.to_string(),
        state: EvidenceState::Committed.as_str().to_string(),
        committed,
    })
}

/// Resolve an artifact for reading: exists, dominated, committed, not purged.
///
/// One helper for both read routes so the four checks cannot drift between
/// them — and so the audit row is written on the denial paths too, which is
/// where an audit log actually earns its keep.
async fn resolve_readable(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    evidence_id: &str,
    kind: &str,
    row_from: Option<i64>,
    row_limit: Option<i64>,
) -> ApiResult<EvidenceArtifact> {
    let tenant = &access.tenant_id;
    let evidence = state.evidence();

    let mut audit = EvidenceAccess {
        evidence_id: evidence_id.to_string(),
        tenant: tenant.to_string(),
        uid: access.uid.clone(),
        kind: kind.to_string(),
        row_from,
        row_limit,
        outcome: "denied".into(),
        at: now_rfc3339(),
    };

    let found = evidence.get(tenant, evidence_id).await?;
    let Some(artifact) = found else {
        // Deliberately NOT audited: there is no artifact to attach the row to,
        // and recording every miss would let an unauthenticated scan fill the
        // table. A 404 here is also the honest answer — the id does not exist
        // in this tenant.
        return Err(ApiError::Mesh(KernelError::NotFound {
            kind: "evidence",
            id: evidence_id.to_string(),
        }));
    };

    if !artifact.manifest.authorization_class.dominated_by(
        access.level,
        &access.compartments,
        access.all_compartments,
    ) {
        let _ = evidence.record_access(&audit).await;
        return Err(ApiError::Custom(CustomError::evidence_forbidden()));
    }
    if artifact.state == EvidenceState::Purged {
        audit.outcome = "expired".into();
        let _ = evidence.record_access(&audit).await;
        return Err(ApiError::Custom(CustomError::evidence_expired(evidence_id)));
    }
    if artifact.state != EvidenceState::Committed {
        audit.outcome = "denied".into();
        let _ = evidence.record_access(&audit).await;
        return Err(ApiError::Custom(CustomError::evidence_not_committed(
            evidence_id,
        )));
    }
    audit.outcome = "ok".into();
    evidence.record_access(&audit).await?;
    Ok(artifact)
}

/// `GET /v1/evidence/{id}` — the manifest, access-checked and audited.
///
/// Returns the manifest UNWRAPPED, because that is what the contract says the
/// route returns. An earlier draft wrapped it in `{evidence_id, state,
/// manifest}`; the Matrix client — written against the contract — could not
/// read that, and the live tier is what caught it. The wrapper added nothing:
/// `evidence_id` is stamped onto the manifest here, and `state` could only
/// ever be `committed` (pending answers 409, purged answers 410), so a 200
/// already carries that information.
pub async fn op_get_manifest(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    evidence_id: &str,
) -> ApiResult<munarium_api_types::EvidenceManifestResponse> {
    let artifact = resolve_readable(state, access, evidence_id, "manifest", None, None).await?;
    let mut manifest = artifact.manifest;
    manifest.evidence_id = Some(artifact.evidence_id.clone());
    serde_json::to_value(&manifest).map_err(|e| ApiError::Mesh(KernelError::Storage(e.to_string())))
}

/// `GET /v1/evidence/{id}/rows` — a bounded, audited window over the bytes.
///
/// Only the canonical CSV form is decoded here. Parquet is sealed and replayed
/// byte-for-byte but not parsed by the server: pulling a Parquet reader into
/// the image to paginate rows would be a large dependency for a convenience,
/// and the guarantee that matters (G1, replay) is about the bytes, which are
/// intact either way. A caller wanting Parquet rows reads the artifact.
pub async fn op_get_rows(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    evidence_id: &str,
    from: usize,
    limit: usize,
) -> ApiResult<munarium_api_types::EvidenceRowsResponse> {
    let limit = limit.clamp(1, MAX_ROW_LIMIT);
    let artifact = resolve_readable(
        state,
        access,
        evidence_id,
        "rows",
        Some(from as i64),
        Some(limit as i64),
    )
    .await?;

    if artifact.manifest.media_type != MEDIA_TYPE_CSV {
        return Err(ApiError::Mesh(KernelError::InvalidInput(format!(
            "rows are served for '{MEDIA_TYPE_CSV}' artifacts only; this artifact is '{}'. \
             Its bytes are sealed and replayable, but this server does not decode them",
            artifact.manifest.media_type
        ))));
    }

    let key = SourceKey::new(
        &access.tenant_id,
        &artifact.blob_path,
        trim_hash(&artifact.manifest.artifact_hash),
    )
    .map_err(ApiError::Mesh)?;
    let bytes = state.source_store().get(&key).await?;
    let text = String::from_utf8(bytes)
        .map_err(|e| ApiError::Mesh(KernelError::Storage(format!("artifact is not UTF-8: {e}"))))?;

    let columns: Vec<&str> = artifact
        .manifest
        .schema
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();

    // The canonical CSV form: a header row, then data. The header is dropped
    // and the manifest's column NAMES are used instead — the schema is the
    // contract, and a header that disagreed with it would be the schema
    // drifting silently.
    let mut lines = text.lines();
    let _header = lines.next();
    // One pass: count every data row for `total` and keep only the page's
    // lines. Collecting every line first made each page O(rows) in
    // allocation on top of the whole-artifact read, on a route any
    // evidence-scoped token can page through.
    let mut total = 0usize;
    let mut page_lines: Vec<&str> = Vec::with_capacity(limit.min(1000));
    for line in lines.filter(|l| !l.is_empty()) {
        if total >= from && page_lines.len() < limit {
            page_lines.push(line);
        }
        total += 1;
    }

    let page: Vec<serde_json::Value> = page_lines
        .into_iter()
        .map(|line| {
            let cells = split_csv_row(line);
            let mut obj = serde_json::Map::new();
            for (i, name) in columns.iter().enumerate() {
                obj.insert(
                    (*name).to_string(),
                    cells
                        .get(i)
                        .map(|c| serde_json::Value::String(c.clone()))
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            serde_json::Value::Object(obj)
        })
        .collect();

    Ok(munarium_api_types::EvidenceRowsResponse {
        evidence_id: artifact.evidence_id,
        from,
        has_more: from + page.len() < total,
        rows: page,
        total: Some(total),
    })
}

/// Split one canonical-CSV row: comma-separated, `"` quoting, `""` escaping.
///
/// Deliberately small rather than a CSV crate. canon@1 fixes the serialization
/// this server has to read, so the general problem (embedded newlines, ragged
/// dialects, BOMs) is not in scope — and a dependency that solved the general
/// problem would also accept inputs canon@1 forbids.
fn split_csv_row(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => out.push(std::mem::take(&mut cur)),
            other => cur.push(other),
        }
    }
    out.push(cur);
    out
}

/// Default row window when the caller does not ask.
pub fn default_row_limit() -> usize {
    DEFAULT_ROW_LIMIT
}

/// Recent resolutions of an artifact — operator-facing, mgmt-gated at the
/// route. Returns *that* reads happened, never what was read.
pub async fn op_accesses(
    state: &AppState,
    tenant: &str,
    evidence_id: &str,
    limit: usize,
) -> ApiResult<Vec<EvidenceAccess>> {
    Ok(state
        .evidence()
        .accesses(tenant, evidence_id, limit)
        .await?)
}

/// One retention sweep: delete the bytes of every expired, unheld artifact,
/// then mark each purged.
///
/// Returns how many were purged. Safe to run on N instances at once — the byte
/// delete is idempotent and the mark is conditional, so a duplicated sweep is
/// wasted effort rather than a correctness problem, and no advisory lock is
/// needed.
///
/// **The order is deliberate: bytes first, then the row.** Delete-then-mark can
/// leave a row still reading `committed` while its bytes are gone — untidy for
/// one interval, but self-healing, because the next sweep still sees the row as
/// due and finishes the job. Mark-then-delete would instead leave an artifact
/// reporting itself purged while its regulated bytes sat on disk, and no later
/// sweep would revisit it. A retention system may be briefly untidy; it may not
/// quietly fail to delete.
pub async fn purge_once(state: &AppState, limit: usize) -> ApiResult<usize> {
    let now = now_rfc3339();
    let due = state.evidence().purge_due(&now, limit).await?;
    let mut purged = 0usize;
    for artifact in due {
        let key = match SourceKey::new(
            &artifact.tenant,
            &artifact.blob_path,
            trim_hash(&artifact.manifest.artifact_hash),
        ) {
            Ok(k) => k,
            Err(e) => {
                // A row whose blob path cannot form a key is a data problem,
                // not a reason to abandon the sweep — the remaining artifacts
                // still have a retention obligation.
                tracing::error!(
                    evidence_id = %artifact.evidence_id,
                    error = %e,
                    "evidence purge: unusable blob path; skipping this artifact"
                );
                continue;
            }
        };
        if let Err(e) = state.source_store().delete(&key).await {
            tracing::warn!(
                evidence_id = %artifact.evidence_id,
                error = %e,
                "evidence purge: byte delete failed; leaving the row due so the next sweep retries"
            );
            continue;
        }
        match state
            .evidence()
            .mark_purged(&artifact.tenant, &artifact.evidence_id, &now)
            .await
        {
            Ok(true) => purged += 1,
            // Another instance won the row. Its bytes are gone either way.
            Ok(false) => {}
            Err(e) => tracing::warn!(
                evidence_id = %artifact.evidence_id,
                error = %e,
                "evidence purge: bytes deleted but the row was not marked; the next sweep will finish it"
            ),
        }
    }
    Ok(purged)
}

/// How many artifacts one sweep will consider. Bounded so a deployment that
/// has just switched retention on does not try to delete a decade at once.
pub const PURGE_BATCH: usize = 500;

/// `DELETE /v1/evidence/{id}` — purge one artifact's bytes now.
///
/// The operator-facing twin of the janitor, and the reason `evidence-on-hold`
/// is a reachable refusal rather than dead vocabulary: an artifact under legal
/// hold refuses deletion here, which is the whole point of a hold.
///
/// The metadata row survives with `purged_at`, so every citation to this
/// artifact keeps resolving — as `evidence-expired`, an honest statement about
/// retention, rather than `not-found`, which would read as though the citation
/// had been fabricated.
pub async fn op_purge(state: &AppState, tenant: &str, evidence_id: &str) -> ApiResult<bool> {
    let Some(artifact) = state.evidence().get(tenant, evidence_id).await? else {
        return Err(ApiError::Mesh(KernelError::NotFound {
            kind: "evidence",
            id: evidence_id.to_string(),
        }));
    };
    if artifact
        .manifest
        .retention
        .as_ref()
        .is_some_and(|r| r.legal_hold)
    {
        return Err(ApiError::Custom(CustomError::evidence_on_hold(evidence_id)));
    }
    if artifact.state == EvidenceState::Purged {
        return Ok(false);
    }
    let key = SourceKey::new(
        tenant,
        &artifact.blob_path,
        trim_hash(&artifact.manifest.artifact_hash),
    )
    .map_err(ApiError::Mesh)?;
    state.source_store().delete(&key).await?;
    Ok(state
        .evidence()
        .mark_purged(tenant, evidence_id, &now_rfc3339())
        .await?)
}

/// `POST /v1/evidence/{id}/legal-hold` — place or lift a hold.
///
/// A hold blocks deletion and nothing else. Reads stay governed by the
/// authorization class exactly as before: an instruction to preserve evidence
/// that also hid it would be a strange instruction.
pub async fn op_set_legal_hold(
    state: &AppState,
    tenant: &str,
    evidence_id: &str,
    hold: bool,
) -> ApiResult<()> {
    if !state
        .evidence()
        .set_legal_hold(tenant, evidence_id, hold)
        .await?
    {
        return Err(ApiError::Mesh(KernelError::NotFound {
            kind: "evidence",
            id: evidence_id.to_string(),
        }));
    }
    Ok(())
}

/// Shared by the routes: the evidence plane's entry check.
pub async fn evidence_access(
    state: &Arc<AppState>,
    headers: &axum::http::HeaderMap,
    uid: &str,
) -> ApiResult<munarium_access::AccessCtx> {
    crate::rest::data_plane_access(state, headers, uid, munarium_access::SCOPE_EVIDENCE).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_rows_split_on_the_canonical_form() {
        assert_eq!(split_csv_row("a,b,c"), vec!["a", "b", "c"]);
        assert_eq!(split_csv_row("a,,c"), vec!["a", "", "c"]);
        // A quoted comma is one cell, not two — the difference between
        // "Acme, Inc" being one counterparty and two.
        assert_eq!(
            split_csv_row(r#"a,"b,still b",c"#),
            vec!["a", "b,still b", "c"]
        );
        // Doubled quotes are one literal quote.
        assert_eq!(split_csv_row(r#""say ""hi""",z"#), vec![r#"say "hi""#, "z"]);
        // Trailing empty cell survives; NULL vs empty is a distinction the
        // whole structured-evidence plane exists to preserve.
        assert_eq!(split_csv_row("a,"), vec!["a", ""]);
    }

    #[test]
    fn blob_paths_live_under_the_reserved_prefix() {
        let p = blob_path("ev-123");
        assert!(p.starts_with(EVIDENCE_PATH_PREFIX));
        assert_eq!(p, "evidence/ev-123");
    }

    #[test]
    fn the_stored_hash_drops_the_algorithm_prefix() {
        assert_eq!(trim_hash("sha256:abc"), "abc");
        assert_eq!(trim_hash("abc"), "abc");
    }
}
