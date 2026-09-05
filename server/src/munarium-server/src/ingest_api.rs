// SPDX-License-Identifier: Apache-2.0
//! the file-ingestion plane. Files arrive one at a time or in batch,
//! gated by a capability token carrying the `ingest` scope. Binding into
//! collections is explicit (`collections: [...]`) or declarative — the
//! `sources:` matchers of every reachable, non-removed runbook are
//! evaluated against the file. Every bind is subject to the token's
//! level/compartments: an ingest token cannot write into a collection its
//! clearance could not read.

use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use base64::Engine as _;
use munarium_access::AccessCtx;
use munarium_api_types as dto;
use munarium_core::KernelError;
use munarium_runbooks::SourceBinding;
use sha2::Digest as _;
use std::sync::Arc;

type ApiResult<T> = std::result::Result<T, ApiError>;

/// Ingest-scope data-plane auth — the shared guard (same scope/revocation/
/// promotion logic the query plane uses; see rest::data_plane_access).
async fn auth_ingest(state: &AppState, headers: &HeaderMap, uid: &str) -> ApiResult<AccessCtx> {
    crate::rest::data_plane_access(state, headers, uid, munarium_access::SCOPE_INGEST).await
}

fn binding_matches(
    binding: &SourceBinding,
    filename: &str,
    media_type: &str,
    content_hash: &str,
) -> bool {
    if binding.content_hashes.iter().any(|h| h == content_hash) {
        return true;
    }
    let prefix_ok = match &binding.filename_prefix {
        Some(p) => filename.starts_with(p.as_str()),
        None => false,
    };
    let media_ok = binding.media_types.iter().any(|m| m == media_type);
    match (
        binding.filename_prefix.is_some(),
        !binding.media_types.is_empty(),
    ) {
        (true, true) => prefix_ok && media_ok,
        (true, false) => prefix_ok,
        (false, true) => media_ok,
        (false, false) => false,
    }
}

/// Collections the declarative matchers of reachable runbooks bind this
/// file into. Only non-removed runbooks; only runbooks the token's `rb`
/// allowlist reaches.
fn matcher_targets(
    runbooks: &[munarium_runbooks::RunbookDoc],
    access: &AccessCtx,
    filename: &str,
    media_type: &str,
    content_hash: &str,
) -> Vec<String> {
    let mut targets = Vec::new();
    for doc in runbooks {
        if !access.permits_runbook(&doc.metadata.name) {
            continue;
        }
        for col in &doc.spec.collections {
            if let Some(binding) = &col.sources {
                if binding_matches(binding, filename, media_type, content_hash)
                    && !targets.contains(&col.name)
                {
                    targets.push(col.name.clone());
                }
            }
        }
    }
    targets
}

/// Load + parse the tenant's non-removed runbooks ONCE per request, so a
/// batch does not re-fetch and re-parse them per file.
async fn load_matcher_runbooks(
    state: &AppState,
    access: &AccessCtx,
) -> munarium_core::Result<Vec<munarium_runbooks::RunbookDoc>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT runbook_ref, yaml FROM runbooks WHERE tenant_id = $1 AND status != 'removed'",
    )
    .bind(&access.tenant_id)
    .fetch_all(crate::runbooks_api::pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .filter_map(|(runbook_ref, yaml): (String, String)| {
            match munarium_runbooks::parse_runbook(&yaml) {
                Ok(doc) => Some(doc),
                // A stored runbook that stopped parsing (a grammar change
                // since it was applied) silently stops binding files. Say so.
                Err(e) => {
                    tracing::warn!(runbook_ref = %runbook_ref, error = %e, "stored runbook does not parse; it binds no ingested files");
                    None
                }
            }
        })
        .collect())
}

/// Ingest one file: store (content-addressed, idempotent), resolve targets,
/// then bind. Clearance is checked for EVERY target BEFORE any bind commits,
/// so a forbidden collection can never leave a partial binding behind.
async fn ingest_one(
    state: &AppState,
    access: &AccessCtx,
    runbooks: &[munarium_runbooks::RunbookDoc],
    file: &dto::IngestFileRequest,
) -> ApiResult<dto::IngestResultDto> {
    if file.filename.trim().is_empty() {
        return Err(ApiError::Mesh(KernelError::InvalidInput(
            "filename is required".into(),
        )));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(file.content_base64.trim())
        .map_err(|e| KernelError::InvalidInput(format!("content_base64: {e}")))?;
    if bytes.is_empty() {
        return Err(ApiError::Mesh(KernelError::InvalidInput(
            "empty file content".into(),
        )));
    }
    let retrieval = state.retrieval_for(&access.tenant_id)?;

    // Store the (content-addressed, idempotent) bytes first — a source with
    // no binding is harmless (sources are not compartmentalized; only
    // collection bindings are). Then resolve targets with the real hash and
    // CHECK CLEARANCE FOR ALL of them before ANY bind commits, so a
    // forbidden target can never leave a partial binding behind.
    let (source_id, hash, existed) = retrieval
        .put_source(
            file.sha256.as_deref().unwrap_or(""),
            &file.media_type,
            &file.filename,
            None,
            &bytes,
        )
        .await?;

    let target_names = match &file.collections {
        Some(names) => names.clone(),
        None => matcher_targets(runbooks, access, &file.filename, &file.media_type, &hash),
    };
    let mut targets = Vec::with_capacity(target_names.len());
    for name in &target_names {
        let info = retrieval.collection_by_name(name).await?;
        if !access.permits(info.access_level, &info.compartments) {
            return Err(ApiError::Mesh(KernelError::Forbidden(format!(
                "collection '{}' requires level {} {:?}; the ingest token does not clear it",
                info.name, info.access_level, info.compartments
            ))));
        }
        targets.push(info);
    }

    let mut bound_to = Vec::with_capacity(targets.len());
    for info in targets {
        retrieval
            .bind_source(&info.id, &source_id, Some(&access.uid))
            .await?;
        bound_to.push(info.name);
    }

    Ok(dto::IngestResultDto {
        filename: file.filename.clone(),
        source_id: Some(source_id),
        sha256: Some(hash),
        existed,
        bound_to,
        error: None,
    })
}

pub async fn op_ingest_batch(
    state: &AppState,
    access: &AccessCtx,
    files: &[dto::IngestFileRequest],
) -> ApiResult<Vec<dto::IngestResultDto>> {
    if files.is_empty() {
        return Err(ApiError::Mesh(KernelError::InvalidInput(
            "files must be non-empty".into(),
        )));
    }
    if files.len() > 500 {
        return Err(ApiError::Mesh(KernelError::InvalidInput(
            "at most 500 files per batch".into(),
        )));
    }
    let runbooks = load_matcher_runbooks(state, access).await?;
    let mut results = Vec::with_capacity(files.len());
    for file in files {
        // Per-item outcomes: one bad file never fails the batch.
        match ingest_one(state, access, &runbooks, file).await {
            Ok(r) => results.push(r),
            Err(e) => results.push(dto::IngestResultDto {
                filename: file.filename.clone(),
                source_id: None,
                sha256: None,
                existed: false,
                bound_to: Vec::new(),
                error: Some(crate::error::client_facing_error(&e)),
            }),
        }
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// REST handlers
// ---------------------------------------------------------------------------

use crate::middleware::uid_or_anonymous as uid_of;

/// POST /v1/ingest — one file.
#[utoipa::path(post, path = "/v1/ingest",
    request_body = dto::IngestFileRequest,
    responses(
        (status = 200, description = "stored (idempotent by content) and bound", body = dto::IngestResultDto),
        (status = 403, description = "ingest scope missing or collection clearance not met")
    ),
    tag = "ingest")]
pub async fn ingest_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::IngestFileRequest>,
) -> ApiResult<Json<dto::IngestResultDto>> {
    let uid = uid_of(uid.as_ref());
    let access = auth_ingest(&state, &headers, &uid).await?;
    let runbooks = load_matcher_runbooks(&state, &access).await?;
    Ok(Json(ingest_one(&state, &access, &runbooks, &req).await?))
}

/// POST /v1/ingest/batch — up to 500 files; per-item outcomes.
#[utoipa::path(post, path = "/v1/ingest/batch",
    request_body = dto::IngestBatchRequest,
    responses((status = 200, body = dto::IngestBatchResponse)),
    tag = "ingest")]
pub async fn ingest_batch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::IngestBatchRequest>,
) -> ApiResult<Json<dto::IngestBatchResponse>> {
    let uid = uid_of(uid.as_ref());
    let access = auth_ingest(&state, &headers, &uid).await?;
    Ok(Json(dto::IngestBatchResponse {
        results: op_ingest_batch(&state, &access, &req.files).await?,
    }))
}

// ---------------------------------------------------------------------------
// Bulk upload sessions — chunked, resumable corpus loading.
//
// A session opens with a manifest (filename + sha256 + size + media type per
// document). The server diffs the manifest against `sources` so the client
// learns exactly which documents still owe bytes; chunks then flow through
// the SAME `ingest_one` path as batch ingest (same storage, same `sources`
// rows, same collection matchers), so downstream buildIndex/binding behavior
// is unchanged. Everything is per-document idempotent: re-sending an entire
// failed chunk re-writes nothing already stored.
// ---------------------------------------------------------------------------

/// Manifest ceiling: bookkeeping rows, not bytes — a 100k-doc corpus opens
/// in one session.
const BULK_MANIFEST_MAX: usize = 100_000;
/// Filenames listed verbatim in complete/status responses are capped; counts
/// are always exact.
const BULK_LIST_CAP: usize = 100;
/// Chunk row/statement sizing for manifest inserts and sources diffs.
const BULK_SQL_CHUNK: usize = 5_000;

fn store_err(e: sqlx::Error) -> ApiError {
    ApiError::Mesh(KernelError::Storage(e.to_string()))
}

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::Mesh(KernelError::InvalidInput(msg.into()))
}

/// Declared sha-256: 64 hex chars, normalized to lowercase.
fn normalize_sha256(raw: &str, filename: &str) -> ApiResult<String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid(format!(
            "manifest entry '{filename}': sha256 must be 64 hex characters"
        )));
    }
    Ok(s)
}

struct BulkSessionRow {
    label: Option<String>,
    status: String,
    total: i64,
    created_at: String,
    expires_at: String,
    completed_at: Option<String>,
}

/// (label, status, total, created_at, expires_at, completed_at, lapsed)
type BulkSessionTuple = (
    Option<String>,
    String,
    i64,
    String,
    String,
    Option<String>,
    bool,
);

/// Load a session, lazily expiring an open session past its expires_at.
async fn load_bulk_session(
    pool: &sqlx::PgPool,
    tenant: &str,
    bulk_id: &str,
) -> ApiResult<BulkSessionRow> {
    let row: Option<BulkSessionTuple> = sqlx::query_as(
        "SELECT label, status, total::bigint, created_at::text, expires_at::text,
                    completed_at::text, (status = 'open' AND expires_at < now())
             FROM bulk_uploads WHERE tenant_id = $1 AND bulk_id = $2",
    )
    .bind(tenant)
    .bind(bulk_id)
    .fetch_optional(pool)
    .await
    .map_err(store_err)?;
    let Some((label, mut status, total, created_at, expires_at, completed_at, lapsed)) = row else {
        return Err(ApiError::Mesh(KernelError::NotFound {
            kind: "bulk upload session",
            id: bulk_id.to_string(),
        }));
    };
    if lapsed {
        sqlx::query(
            "UPDATE bulk_uploads SET status = 'expired'
             WHERE tenant_id = $1 AND bulk_id = $2 AND status = 'open'",
        )
        .bind(tenant)
        .bind(bulk_id)
        .execute(pool)
        .await
        .map_err(store_err)?;
        status = "expired".to_string();
    }
    Ok(BulkSessionRow {
        label,
        status,
        total,
        created_at,
        expires_at,
        completed_at,
    })
}

/// Exact per-status counts for a session.
async fn bulk_counts(
    pool: &sqlx::PgPool,
    tenant: &str,
    bulk_id: &str,
) -> ApiResult<(u64, u64, u64, u64)> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, count(*) FROM bulk_upload_files
         WHERE tenant_id = $1 AND bulk_id = $2 GROUP BY status",
    )
    .bind(tenant)
    .bind(bulk_id)
    .fetch_all(pool)
    .await
    .map_err(store_err)?;
    let (mut stored, mut skipped, mut pending, mut failed) = (0u64, 0u64, 0u64, 0u64);
    for (status, n) in rows {
        let n = n.max(0) as u64;
        match status.as_str() {
            "stored" => stored = n,
            "skipped_existing" => skipped = n,
            "pending" => pending = n,
            "failed" => failed = n,
            _ => {}
        }
    }
    Ok((stored, skipped, pending, failed))
}

/// Which of these source ids are bound to at least one collection. Chunked
/// like the hash lookup so a 66k-file manifest never builds one giant array.
async fn bound_source_ids(
    pool: &sqlx::PgPool,
    tenant: &str,
    source_ids: &[String],
) -> ApiResult<std::collections::HashSet<String>> {
    let mut out = std::collections::HashSet::with_capacity(source_ids.len());
    for chunk in source_ids.chunks(BULK_SQL_CHUNK) {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT source_id FROM collection_sources
             WHERE tenant_id = $1 AND source_id = ANY($2)",
        )
        .bind(tenant)
        .bind(chunk)
        .fetch_all(pool)
        .await
        .map_err(store_err)?;
        out.extend(rows.into_iter().map(|(s,)| s));
    }
    Ok(out)
}

/// `sources` rows for these filenames, chunked so a 66k-file manifest never
/// builds one giant bind array.
async fn existing_source_hashes(
    pool: &sqlx::PgPool,
    tenant: &str,
    filenames: &[String],
) -> ApiResult<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::with_capacity(filenames.len() / 4);
    for chunk in filenames.chunks(BULK_SQL_CHUNK) {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT filename, content_hash FROM sources
             WHERE tenant_id = $1 AND filename = ANY($2)",
        )
        .bind(tenant)
        .bind(chunk)
        .fetch_all(pool)
        .await
        .map_err(store_err)?;
        for (f, h) in rows {
            out.insert(f, h);
        }
    }
    Ok(out)
}

pub async fn op_bulk_open(
    state: &AppState,
    access: &AccessCtx,
    req: &dto::BulkOpenRequest,
) -> ApiResult<dto::BulkOpenResponse> {
    if req.files.is_empty() {
        return Err(invalid("manifest must be non-empty"));
    }
    if req.files.len() > BULK_MANIFEST_MAX {
        return Err(invalid(format!(
            "manifest holds {} entries; at most {BULK_MANIFEST_MAX} per session",
            req.files.len()
        )));
    }
    // Validate every entry BEFORE any row is written: path rules (the same
    // security boundary single ingest enforces), hash shape, duplicates.
    let mut manifest: Vec<(String, String, i64, String)> = Vec::with_capacity(req.files.len());
    let mut seen = std::collections::HashSet::with_capacity(req.files.len());
    for entry in &req.files {
        munarium_core::sources::validate_path(&entry.filename)?;
        if entry.media_type.trim().is_empty() {
            return Err(invalid(format!(
                "manifest entry '{}': media_type is required",
                entry.filename
            )));
        }
        let sha = normalize_sha256(&entry.sha256, &entry.filename)?;
        if !seen.insert(entry.filename.clone()) {
            return Err(invalid(format!(
                "manifest lists '{}' more than once",
                entry.filename
            )));
        }
        manifest.push((
            entry.filename.clone(),
            sha,
            entry.bytes_len as i64,
            entry.media_type.clone(),
        ));
    }

    let pool = crate::runbooks_api::pool(state)?;

    // Opportunistic cleanup: lapse this tenant's overdue open sessions so
    // abandoned manifests never need a background job.
    sqlx::query(
        "UPDATE bulk_uploads SET status = 'expired'
         WHERE tenant_id = $1 AND status = 'open' AND expires_at < now()",
    )
    .bind(&access.tenant_id)
    .execute(pool)
    .await
    .map_err(store_err)?;

    let bulk_id = format!("blk-{}", crate::runbooks_api::uuid_suffix());
    sqlx::query(
        "INSERT INTO bulk_uploads (tenant_id, bulk_id, label, total, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + interval '7 days')",
    )
    .bind(&access.tenant_id)
    .bind(&bulk_id)
    .bind(&req.label)
    .bind(manifest.len() as i32)
    .bind(&access.uid)
    .execute(pool)
    .await
    .map_err(store_err)?;

    for chunk in manifest.chunks(BULK_SQL_CHUNK) {
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO bulk_upload_files (tenant_id, bulk_id, filename, sha256, bytes_len, media_type) ",
        );
        qb.push_values(chunk, |mut b, (filename, sha, len, media)| {
            b.push_bind(&access.tenant_id)
                .push_bind(&bulk_id)
                .push_bind(filename)
                .push_bind(sha)
                .push_bind(len)
                .push_bind(media);
        });
        qb.build().execute(pool).await.map_err(store_err)?;
    }

    // Diff against already-stored sources: same path + same bytes = nothing
    // owed. Same path + DIFFERENT bytes stays pending — the client intends an
    // update and a rebuild will be owed.
    let filenames: Vec<String> = manifest.iter().map(|(f, ..)| f.clone()).collect();
    let existing = existing_source_hashes(pool, &access.tenant_id, &filenames).await?;

    // "The bytes are here" is NOT the same as "the document landed".
    // `ingest_one` stores bytes BEFORE it checks clearance on the target
    // collections, so a file that failed to bind still leaves a matching
    // `sources` row behind. Skipping those on the strength of the hash alone
    // would let a fresh session report `completed` over collections that are
    // still empty — the exact failure the finalize check exists to catch.
    // So a stored-but-unbound file is only skippable when the matchers say it
    // binds nowhere; otherwise it stays pending and the re-send re-binds it.
    let runbooks = load_matcher_runbooks(state, access).await?;
    let candidates: Vec<&(String, String, i64, String)> = manifest
        .iter()
        .filter(|(f, sha, ..)| existing.get(f).is_some_and(|h| h == sha))
        .collect();
    let candidate_ids: Vec<String> = candidates
        .iter()
        .map(|(f, ..)| munarium_core::sources::source_id(&access.tenant_id, f))
        .collect();
    let bound = bound_source_ids(pool, &access.tenant_id, &candidate_ids).await?;
    let matched: Vec<String> = candidates
        .iter()
        .zip(candidate_ids.iter())
        .filter(|((f, sha, _, media), sid)| {
            bound.contains(sid.as_str())
                || matcher_targets(&runbooks, access, f, media, sha).is_empty()
        })
        .map(|((f, ..), _)| f.clone())
        .collect();
    for chunk in matched.chunks(BULK_SQL_CHUNK) {
        sqlx::query(
            "UPDATE bulk_upload_files SET status = 'skipped_existing', updated_at = now()
             WHERE tenant_id = $1 AND bulk_id = $2 AND filename = ANY($3)",
        )
        .bind(&access.tenant_id)
        .bind(&bulk_id)
        .bind(chunk)
        .execute(pool)
        .await
        .map_err(store_err)?;
    }

    let matched_set: std::collections::HashSet<&String> = matched.iter().collect();
    let needed: Vec<String> = manifest
        .iter()
        .map(|(f, ..)| f)
        .filter(|f| !matched_set.contains(f))
        .cloned()
        .collect();
    Ok(dto::BulkOpenResponse {
        bulk_id,
        total: manifest.len() as u64,
        already_present: matched.len() as u64,
        needed,
    })
}

pub async fn op_bulk_chunk(
    state: &AppState,
    access: &AccessCtx,
    bulk_id: &str,
    files: &[dto::IngestFileRequest],
) -> ApiResult<dto::BulkChunkResponse> {
    if files.is_empty() {
        return Err(invalid("files must be non-empty"));
    }
    if files.len() > 500 {
        return Err(invalid("at most 500 files per chunk"));
    }
    let pool = crate::runbooks_api::pool(state)?;
    let session = load_bulk_session(pool, &access.tenant_id, bulk_id).await?;
    if session.status != "open" {
        return Err(invalid(format!(
            "bulk session {bulk_id} is {}; chunks require an open session",
            session.status
        )));
    }

    // Manifest rows for exactly this chunk's filenames.
    let names: Vec<String> = files.iter().map(|f| f.filename.clone()).collect();
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT filename, sha256, media_type FROM bulk_upload_files
         WHERE tenant_id = $1 AND bulk_id = $2 AND filename = ANY($3)",
    )
    .bind(&access.tenant_id)
    .bind(bulk_id)
    .bind(&names)
    .fetch_all(pool)
    .await
    .map_err(store_err)?;
    let manifest: std::collections::HashMap<String, (String, String)> = rows
        .into_iter()
        .map(|(f, sha, media)| (f, (sha, media)))
        .collect();

    let runbooks = load_matcher_runbooks(state, access).await?;
    let mut results = Vec::with_capacity(files.len());
    for file in files {
        // Per-file verdicts, mirroring batch ingest: one bad file never
        // fails the chunk, and the manifest row records the outcome so a
        // resume knows what is still owed.
        let fail = |error: String| dto::IngestResultDto {
            filename: file.filename.clone(),
            source_id: None,
            sha256: None,
            existed: false,
            bound_to: Vec::new(),
            error: Some(error),
        };
        let Some((declared_sha, declared_media)) = manifest.get(&file.filename) else {
            results.push(fail("not in the session manifest".into()));
            continue;
        };
        if &file.media_type != declared_media {
            let msg = format!(
                "media_type '{}' does not match the manifest's '{declared_media}'",
                file.media_type
            );
            mark_bulk_file(
                pool,
                access,
                bulk_id,
                &file.filename,
                "failed",
                Some(&msg),
                None,
            )
            .await?;
            results.push(fail(msg));
            continue;
        }
        // Verify the received bytes against the DECLARED hash before any
        // storage write — a corrupted chunk fails per-file, loudly.
        let bytes =
            match base64::engine::general_purpose::STANDARD.decode(file.content_base64.trim()) {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("content_base64: {e}");
                    mark_bulk_file(
                        pool,
                        access,
                        bulk_id,
                        &file.filename,
                        "failed",
                        Some(&msg),
                        None,
                    )
                    .await?;
                    results.push(fail(msg));
                    continue;
                }
            };
        let got_sha = hex::encode(sha2::Sha256::digest(&bytes));
        if &got_sha != declared_sha {
            let msg = format!(
                "sha256 mismatch: manifest declares {declared_sha}, received bytes hash {got_sha}"
            );
            mark_bulk_file(
                pool,
                access,
                bulk_id,
                &file.filename,
                "failed",
                Some(&msg),
                None,
            )
            .await?;
            results.push(fail(msg));
            continue;
        }
        // The one storage path: identical to batch ingest (put_source +
        // matcher binding + clearance), declared sha threaded through so the
        // store re-verifies the same contract.
        let mut req = file.clone();
        req.sha256 = Some(declared_sha.clone());
        match ingest_one(state, access, &runbooks, &req).await {
            Ok(r) => {
                let status = if r.existed {
                    "skipped_existing"
                } else {
                    "stored"
                };
                mark_bulk_file(
                    pool,
                    access,
                    bulk_id,
                    &file.filename,
                    status,
                    None,
                    r.source_id.as_deref(),
                )
                .await?;
                results.push(r);
            }
            Err(e) => {
                let msg = crate::error::client_facing_error(&e);
                mark_bulk_file(
                    pool,
                    access,
                    bulk_id,
                    &file.filename,
                    "failed",
                    Some(&msg),
                    None,
                )
                .await?;
                results.push(fail(msg));
            }
        }
    }

    let (stored, skipped_existing, pending, failed) =
        bulk_counts(pool, &access.tenant_id, bulk_id).await?;
    Ok(dto::BulkChunkResponse {
        bulk_id: bulk_id.to_string(),
        results,
        stored,
        skipped_existing,
        pending,
        failed,
    })
}

async fn mark_bulk_file(
    pool: &sqlx::PgPool,
    access: &AccessCtx,
    bulk_id: &str,
    filename: &str,
    status: &str,
    error: Option<&str>,
    source_id: Option<&str>,
) -> ApiResult<()> {
    // `stored` is a high-water mark within a session: re-sending a chunk that
    // already landed reports `existed: true`, and letting that rewrite the row
    // to `skipped_existing` would make a load that survived one retry finish
    // claiming it stored nothing.
    sqlx::query(
        "UPDATE bulk_upload_files
         SET status = CASE WHEN status = 'stored' AND $4 = 'skipped_existing'
                           THEN 'stored' ELSE $4 END,
             error = $5, source_id = COALESCE($6, source_id), updated_at = now()
         WHERE tenant_id = $1 AND bulk_id = $2 AND filename = $3",
    )
    .bind(&access.tenant_id)
    .bind(bulk_id)
    .bind(filename)
    .bind(status)
    .bind(error)
    .bind(source_id)
    .execute(pool)
    .await
    .map_err(store_err)?;
    Ok(())
}

pub async fn op_bulk_status(
    state: &AppState,
    access: &AccessCtx,
    bulk_id: &str,
    include_needed: bool,
) -> ApiResult<dto::BulkStatusResponse> {
    let pool = crate::runbooks_api::pool(state)?;
    let session = load_bulk_session(pool, &access.tenant_id, bulk_id).await?;
    let (stored, skipped_existing, pending, failed) =
        bulk_counts(pool, &access.tenant_id, bulk_id).await?;
    let failures: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT filename, error FROM bulk_upload_files
         WHERE tenant_id = $1 AND bulk_id = $2 AND status = 'failed'
         ORDER BY filename LIMIT $3",
    )
    .bind(&access.tenant_id)
    .bind(bulk_id)
    .bind(BULK_LIST_CAP as i64)
    .fetch_all(pool)
    .await
    .map_err(store_err)?;
    let needed = if include_needed {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT filename FROM bulk_upload_files
             WHERE tenant_id = $1 AND bulk_id = $2 AND status IN ('pending', 'failed')
             ORDER BY filename",
        )
        .bind(&access.tenant_id)
        .bind(bulk_id)
        .fetch_all(pool)
        .await
        .map_err(store_err)?;
        Some(rows.into_iter().map(|(f,)| f).collect())
    } else {
        None
    };
    Ok(dto::BulkStatusResponse {
        bulk_id: bulk_id.to_string(),
        label: session.label,
        status: session.status,
        total: session.total.max(0) as u64,
        stored,
        skipped_existing,
        pending,
        failed,
        failures: failures
            .into_iter()
            .map(|(filename, error)| dto::BulkFileErrorDto {
                filename,
                error: error.unwrap_or_default(),
            })
            .collect(),
        needed,
        created_at: session.created_at,
        expires_at: session.expires_at,
        completed_at: session.completed_at,
    })
}

pub async fn op_bulk_complete(
    state: &AppState,
    access: &AccessCtx,
    bulk_id: &str,
) -> ApiResult<dto::BulkCompleteResponse> {
    let pool = crate::runbooks_api::pool(state)?;
    let session = load_bulk_session(pool, &access.tenant_id, bulk_id).await?;
    if session.status == "expired" {
        return Err(invalid(format!("bulk session {bulk_id} is expired")));
    }
    let (stored, skipped_existing, pending, failed) =
        bulk_counts(pool, &access.tenant_id, bulk_id).await?;

    // Anything still owing bytes is "missing" from the finalize view.
    let mut missing: Vec<String> = {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT filename FROM bulk_upload_files
             WHERE tenant_id = $1 AND bulk_id = $2 AND status IN ('pending', 'failed')
             ORDER BY filename",
        )
        .bind(&access.tenant_id)
        .bind(bulk_id)
        .fetch_all(pool)
        .await
        .map_err(store_err)?;
        rows.into_iter().map(|(f,)| f).collect()
    };

    // Paranoid re-verify: every entry this session considers landed must
    // still exist in `sources` with the manifest's hash. A path overwritten
    // by a LATER upload with different bytes is a mismatch, not silence.
    let landed: Vec<(String, String)> = sqlx::query_as(
        "SELECT filename, sha256 FROM bulk_upload_files
         WHERE tenant_id = $1 AND bulk_id = $2 AND status IN ('stored', 'skipped_existing')
         ORDER BY filename",
    )
    .bind(&access.tenant_id)
    .bind(bulk_id)
    .fetch_all(pool)
    .await
    .map_err(store_err)?;
    let landed_names: Vec<String> = landed.iter().map(|(f, _)| f.clone()).collect();
    let current = existing_source_hashes(pool, &access.tenant_id, &landed_names).await?;
    let mut mismatched = Vec::new();
    for (filename, sha) in &landed {
        match current.get(filename) {
            None => missing.push(filename.clone()),
            Some(h) if h != sha => mismatched.push(filename.clone()),
            Some(_) => {}
        }
    }
    missing.sort();

    let missing_count = missing.len() as u64;
    let mismatched_count = mismatched.len() as u64;
    let complete = missing.is_empty() && mismatched.is_empty();
    if complete && session.status == "open" {
        sqlx::query(
            "UPDATE bulk_uploads SET status = 'completed', completed_at = now()
             WHERE tenant_id = $1 AND bulk_id = $2 AND status = 'open'",
        )
        .bind(&access.tenant_id)
        .bind(bulk_id)
        .execute(pool)
        .await
        .map_err(store_err)?;
    }
    missing.truncate(BULK_LIST_CAP);
    mismatched.truncate(BULK_LIST_CAP);
    let _ = (pending, failed);
    Ok(dto::BulkCompleteResponse {
        bulk_id: bulk_id.to_string(),
        status: if complete { "completed" } else { "incomplete" }.to_string(),
        total: session.total.max(0) as u64,
        stored,
        skipped_existing,
        missing,
        missing_count,
        mismatched,
        mismatched_count,
    })
}

// ---------------------------------------------------------------------------
// REST handlers — bulk
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct BulkStatusQuery {
    #[serde(default)]
    pub include_needed: bool,
}

/// POST /v1/ingest/bulk — open a chunked bulk-upload session from a manifest.
#[utoipa::path(post, path = "/v1/ingest/bulk",
    request_body = dto::BulkOpenRequest,
    responses(
        (status = 200, description = "session opened; `needed` is the upload work list", body = dto::BulkOpenResponse),
        (status = 403, description = "ingest scope missing")
    ),
    tag = "ingest")]
pub async fn bulk_open(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::BulkOpenRequest>,
) -> ApiResult<Json<dto::BulkOpenResponse>> {
    let uid = uid_of(uid.as_ref());
    let access = auth_ingest(&state, &headers, &uid).await?;
    Ok(Json(op_bulk_open(&state, &access, &req).await?))
}

/// POST /v1/ingest/bulk/{bulk_id}/chunk — one chunk of manifest files.
#[utoipa::path(post, path = "/v1/ingest/bulk/{bulk_id}/chunk",
    params(("bulk_id" = String, Path, description = "open bulk session")),
    request_body = dto::BulkChunkRequest,
    responses((status = 200, description = "per-file outcomes + running session counts", body = dto::BulkChunkResponse)),
    tag = "ingest")]
pub async fn bulk_chunk(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(bulk_id): axum::extract::Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::BulkChunkRequest>,
) -> ApiResult<Json<dto::BulkChunkResponse>> {
    let uid = uid_of(uid.as_ref());
    let access = auth_ingest(&state, &headers, &uid).await?;
    Ok(Json(
        op_bulk_chunk(&state, &access, &bulk_id, &req.files).await?,
    ))
}

/// GET /v1/ingest/bulk/{bulk_id} — session progress; `?include_needed=true`
/// adds the remaining work list for a resume.
#[utoipa::path(get, path = "/v1/ingest/bulk/{bulk_id}",
    params(
        ("bulk_id" = String, Path, description = "bulk session"),
        ("include_needed" = Option<bool>, Query, description = "include the remaining filenames")
    ),
    responses((status = 200, body = dto::BulkStatusResponse)),
    tag = "ingest")]
pub async fn bulk_status(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(bulk_id): axum::extract::Path<String>,
    axum::extract::Query(q): axum::extract::Query<BulkStatusQuery>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
) -> ApiResult<Json<dto::BulkStatusResponse>> {
    let uid = uid_of(uid.as_ref());
    let access = auth_ingest(&state, &headers, &uid).await?;
    Ok(Json(
        op_bulk_status(&state, &access, &bulk_id, q.include_needed).await?,
    ))
}

/// POST /v1/ingest/bulk/{bulk_id}/complete — finalize: verifies every
/// manifest entry is present in `sources` with its declared hash.
#[utoipa::path(post, path = "/v1/ingest/bulk/{bulk_id}/complete",
    params(("bulk_id" = String, Path, description = "bulk session")),
    responses((status = 200, description = "completed, or incomplete with the discrepancy lists", body = dto::BulkCompleteResponse)),
    tag = "ingest")]
pub async fn bulk_complete(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(bulk_id): axum::extract::Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
) -> ApiResult<Json<dto::BulkCompleteResponse>> {
    let uid = uid_of(uid.as_ref());
    let access = auth_ingest(&state, &headers, &uid).await?;
    Ok(Json(op_bulk_complete(&state, &access, &bulk_id).await?))
}

// ---------------------------------------------------------------------------
// Control-plane read for the /admin collections page (2026-08-27): the
// recent bulk upload sessions with their per-file tallies. SQL stays here.
// ---------------------------------------------------------------------------

pub struct BulkUploadRow {
    pub bulk_id: String,
    pub label: Option<String>,
    pub status: String,
    pub total: i32,
    pub created_by: String,
    pub created_at: String,
    pub expires_at: String,
    pub completed_at: Option<String>,
    /// Files stored this session OR skipped because the bytes already
    /// existed — both mean "the corpus holds it".
    pub stored: i64,
    pub failed: i64,
    pub pending: i64,
}

#[allow(clippy::type_complexity)]
pub async fn op_recent_bulk_uploads(
    state: &AppState,
    tenant: &str,
    limit: i64,
) -> Result<Vec<BulkUploadRow>, KernelError> {
    let rows: Vec<(
        String,
        Option<String>,
        String,
        i32,
        String,
        String,
        String,
        Option<String>,
        i64,
        i64,
        i64,
    )> = sqlx::query_as(
        "SELECT b.bulk_id, b.label, b.status, b.total, b.created_by,
                b.created_at::text, b.expires_at::text, b.completed_at::text,
                count(f.filename) FILTER (WHERE f.status IN ('stored', 'skipped_existing')),
                count(f.filename) FILTER (WHERE f.status = 'failed'),
                count(f.filename) FILTER (WHERE f.status = 'pending')
           FROM bulk_uploads b
           LEFT JOIN bulk_upload_files f
             ON f.tenant_id = b.tenant_id AND f.bulk_id = b.bulk_id
          WHERE b.tenant_id = $1
          GROUP BY b.tenant_id, b.bulk_id
          ORDER BY b.created_at DESC LIMIT $2",
    )
    .bind(tenant)
    .bind(limit)
    .fetch_all(crate::runbooks_api::pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(
                bulk_id,
                label,
                status,
                total,
                created_by,
                created_at,
                expires_at,
                completed_at,
                stored,
                failed,
                pending,
            )| BulkUploadRow {
                bulk_id,
                label,
                status,
                total,
                created_by,
                created_at,
                expires_at,
                completed_at,
                stored,
                failed,
                pending,
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(prefix: Option<&str>, media: &[&str], hashes: &[&str]) -> SourceBinding {
        SourceBinding {
            filename_prefix: prefix.map(String::from),
            media_types: media.iter().map(|s| s.to_string()).collect(),
            content_hashes: hashes.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn sha256_normalization() {
        let ok = "A".repeat(64);
        assert_eq!(
            normalize_sha256(&ok, "x.md").expect("64 hex accepted"),
            "a".repeat(64),
            "hashes normalize to lowercase"
        );
        for bad in ["", "abc", &"a".repeat(63), &"g".repeat(64)] {
            assert!(
                normalize_sha256(bad, "x.md").is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn matcher_semantics() {
        // prefix only
        assert!(binding_matches(
            &binding(Some("eng/"), &[], &[]),
            "eng/x.md",
            "text/plain",
            "h"
        ));
        assert!(!binding_matches(
            &binding(Some("eng/"), &[], &[]),
            "pub/x.md",
            "text/plain",
            "h"
        ));
        // media only
        assert!(binding_matches(
            &binding(None, &["text/plain"], &[]),
            "any",
            "text/plain",
            "h"
        ));
        // both = AND
        let both = binding(Some("eng/"), &["text/plain"], &[]);
        assert!(binding_matches(&both, "eng/x", "text/plain", "h"));
        assert!(!binding_matches(&both, "eng/x", "text/html", "h"));
        assert!(!binding_matches(&both, "pub/x", "text/plain", "h"));
        // explicit hash ORs in regardless
        assert!(binding_matches(
            &binding(Some("eng/"), &[], &["h1"]),
            "pub/x",
            "text/html",
            "h1"
        ));
        // empty binding matches nothing
        assert!(!binding_matches(&binding(None, &[], &[]), "x", "y", "h"));
    }

    #[test]
    fn prefix_is_literal_not_a_like_pattern() {
        // A prefix containing LIKE metacharacters must match literally — this
        // is the guarantee the pg-side `starts_with` (not LIKE) upholds so the
        // ingest matcher and the runbook resolveSources bind the same set.
        let b = binding(Some("eng_v2/"), &[], &[]);
        assert!(binding_matches(&b, "eng_v2/x.md", "text/plain", "h"));
        // '_' must NOT act as a single-char wildcard
        assert!(!binding_matches(&b, "engXv2/x.md", "text/plain", "h"));
        let pct = binding(Some("100%/"), &[], &[]);
        assert!(binding_matches(&pct, "100%/report.md", "text/plain", "h"));
        assert!(!binding_matches(
            &pct,
            "100abc/report.md",
            "text/plain",
            "h"
        ));
    }

    #[test]
    fn matcher_targets_dedups_and_honors_runbook_allowlist() {
        use munarium_runbooks::parse_runbook;
        let rb = |name: &str| {
            parse_runbook(&format!(
                "apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {{ name: {name}, version: 1 }}\n\
                 spec:\n  collections:\n    - {{ name: {name}-c, shape: s@1, sources: {{ filenamePrefix: \"docs/\" }} }}\n  steps:\n    - buildIndex: {{}}\n"
            ))
            .expect("parses")
        };
        let books = vec![rb("alpha"), rb("beta")];

        // No allowlist (rb = None): both runbooks' matching collections bind.
        let mut open = AccessCtx::unrestricted("u", "t");
        open.runbooks = None;
        let mut t = matcher_targets(&books, &open, "docs/x.md", "text/plain", "h");
        t.sort();
        assert_eq!(t, vec!["alpha-c".to_string(), "beta-c".to_string()]);

        // Allowlist restricts which runbooks' collections a token can reach.
        let mut restricted = AccessCtx::unrestricted("u", "t");
        restricted.runbooks = Some(vec!["alpha".to_string()]);
        assert_eq!(
            matcher_targets(&books, &restricted, "docs/x.md", "text/plain", "h"),
            vec!["alpha-c".to_string()]
        );

        // A non-matching file binds nothing.
        assert!(matcher_targets(&books, &open, "other/x.md", "text/plain", "h").is_empty());
    }
}
