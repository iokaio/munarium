// SPDX-License-Identifier: Apache-2.0
//! The shared command/query service layer. BOTH planes (REST handlers, tonic
//! impls) convert to these calls, so gate behavior, supersession, and pin
//! semantics cannot diverge between planes — the conformance suite asserts it.
//!
//! The command path IS the governance path: ProposeClaim/AppendEvents load
//! the snapshot, run the deterministic gates, and record block-flagged claims
//! as DISPUTED (never dropped).

use munarium_api_conv::{convert, Convert};
use munarium_api_types as dto;
use munarium_core::composer::compose;
use munarium_core::gates::{blocked_claim_keys, run_gates};
use munarium_core::ledger::FactQuery;
use munarium_core::storage::{load_snapshot, NewClaim, StorageBackend};
use munarium_core::types::*;
use munarium_core::{KernelError, Result};

pub struct CommandOutcome {
    pub claims: Vec<Claim>,
    pub findings: Vec<GateFinding>,
    pub head_seq: Seq,
}

fn to_new_claim(req: &dto::ProposeClaimRequest, status: ClaimStatus) -> NewClaim {
    NewClaim {
        claim_type: req.claim_type.convert(),
        subject: req.subject.clone(),
        key: req.key.clone(),
        value: req.value.clone(),
        scope_path: req.scope_path.clone(),
        status,
        provenance: req.provenance.map(convert).unwrap_or(Provenance::Witnessed),
        supersedes_id: req.supersedes_id.clone(),
        entity_id: req.entity_id.clone(),
        evidence: req.evidence.clone(),
        confidence: req.confidence,
        shape_ref: req.shape_ref.clone(),
        origin: req.origin.clone().map(convert),
    }
}

/// Shape validation for the command path: a claim bearing `shape_ref` whose
/// body fails the shape's JSON Schema draws a `shape.schema-violation` BLOCK
/// finding — same lifecycle as every gate: blocked, recorded disputed, never
/// dropped.
fn shape_findings(
    shapes: &munarium_shapes::ShapeRegistry,
    tenant: &str,
    claims: &[dto::ProposeClaimRequest],
    scope: Option<&str>,
) -> Vec<GateFinding> {
    let mut findings = Vec::new();
    for c in claims {
        let Some(shape_ref) = &c.shape_ref else {
            continue;
        };
        let body = munarium_shapes::claim_body(&c.subject, &c.key, &c.value, c.evidence.as_ref());
        if let Err(citation) = shapes.validate(tenant, shape_ref, &body) {
            findings.push(GateFinding {
                rule_id: "shape.schema-violation".into(),
                severity: Severity::Block,
                message: format!(
                    "claim '{}.{}' violates shape {shape_ref}: {citation}",
                    c.subject, c.key
                ),
                scope_path: scope.map(String::from),
                detail: Some(serde_json::json!({
                    "claim_key": format!("{}.{}", c.subject, c.key),
                    "shape_ref": shape_ref,
                    "policy_citation": citation,
                })),
            });
        }
    }
    findings
}

/// Gate + append a batch as ONE candidate unit (ProposeClaim is a batch of 1).
pub async fn append_events(
    store: &dyn StorageBackend,
    shapes: &munarium_shapes::ShapeRegistry,
    tenant: &str,
    version_id: &str,
    claims: &[dto::ProposeClaimRequest],
    candidate_text: Option<&str>,
    expected_head: Option<Seq>,
    chronology: Option<&munarium_core::chrono_gate::ChronologyRules>,
) -> Result<CommandOutcome> {
    if claims.is_empty() && candidate_text.is_none() {
        return Err(KernelError::InvalidInput("empty command".into()));
    }
    // Gate decisions are only valid for the head they were computed against,
    // so the append pins the OBSERVED head even when the caller sent none:
    // a concurrent writer advancing the head between snapshot and append
    // fails the batch instead of landing claims gated against stale canon.
    // Without a caller pin that conflict is re-gated and retried here (the
    // caller asked for "append", not "append at head N"); with a caller pin
    // it surfaces as the HeadConflict it is. The batch itself is transactional
    // in the store (append_claims): every claim lands or none does.
    const MAX_REGATE_ATTEMPTS: usize = 3;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let head_now = store.head(version_id).await?;
        if let Some(expected) = expected_head {
            if expected != head_now {
                return Err(KernelError::HeadConflict {
                    expected,
                    actual: head_now,
                });
            }
        }

        let scope = claims.iter().find_map(|c| c.scope_path.clone());
        let snapshot = load_snapshot(store, version_id, None, None, None).await?;
        let mut candidate = Candidate {
            scope_path: scope,
            text: candidate_text.unwrap_or_default().to_string(),
            ..Default::default()
        };
        for c in claims {
            let p = ProposedClaim {
                claim_type: c.claim_type.convert(),
                subject: c.subject.clone(),
                key: c.key.clone(),
                value: c.value.clone(),
                supersedes_id: c.supersedes_id.clone(),
            };
            match p.claim_type {
                ClaimType::Correction => candidate.corrections.push(p),
                _ => candidate.claims.push(p),
            }
        }

        let mut findings = run_gates(&snapshot, &candidate);
        // The sixth gate (2026-08-17, §13 entry 13): runs only when the
        // version arms a rules asset, right after the five always-on gates,
        // merging into the same block/dispute lifecycle. `now: None`
        // deliberately — deadline-ABSENCE checks need an as-of clock the
        // server does not carry (the same missing assertion-date source
        // that keeps as_of_date rejected); order/contains/overlap/duration
        // violations fire on certainty alone.
        if let Some(rules) = chronology {
            findings.extend(munarium_core::gates::check_chronology(
                &snapshot, &candidate, rules, None,
            ));
        }
        findings.extend(shape_findings(
            shapes,
            tenant,
            claims,
            candidate.scope_path.as_deref(),
        ));
        let blocked = blocked_claim_keys(&findings);

        let batch: Vec<NewClaim> = claims
            .iter()
            .map(|c| {
                let claim_key = format!("{}.{}", c.subject, c.key);
                let status = if blocked.contains(&claim_key) {
                    ClaimStatus::Disputed // blocked, never dropped
                } else {
                    ClaimStatus::Accepted
                };
                to_new_claim(c, status)
            })
            .collect();

        if batch.is_empty() {
            // Text-only candidate: findings but nothing to append.
            persist_findings(store, version_id, head_now, &findings).await;
            return Ok(CommandOutcome {
                claims: Vec::new(),
                findings,
                head_seq: head_now,
            });
        }

        match store.append_claims(version_id, batch, Some(head_now)).await {
            Ok(stored) => {
                let head_seq = stored.last().map(|c| c.seq).unwrap_or(head_now);
                persist_findings(store, version_id, head_seq, &findings).await;
                return Ok(CommandOutcome {
                    claims: stored,
                    findings,
                    head_seq,
                });
            }
            Err(KernelError::HeadConflict { .. })
                if expected_head.is_none() && attempt < MAX_REGATE_ATTEMPTS =>
            {
                continue; // head moved under us: re-snapshot, re-gate, retry
            }
            Err(e) => return Err(e),
        }
    }
}

/// Persist a write's findings (2026-08-17, §13 entry 12) — best-effort
/// RELATIVE TO THE WRITE, deliberately: the claims are already appended, so
/// failing the request here would push clients into a retry that re-appends.
/// The write response remains the authoritative carrier; the store is the
/// queryable record, and a persistence failure is a loud warn.
async fn persist_findings(
    store: &dyn StorageBackend,
    version_id: &str,
    seq: Seq,
    findings: &[GateFinding],
) {
    if findings.is_empty() {
        return;
    }
    if let Err(e) = store.record_findings(version_id, seq, findings).await {
        tracing::warn!(error = %e, version_id, "gate findings returned in the response but NOT persisted");
    }
}

/// The persisted-findings read behind `GET /v1/versions/{id}/findings`.
pub async fn get_findings(
    store: &dyn StorageBackend,
    version_id: &str,
    q: &munarium_core::storage::FindingsQuery,
) -> Result<dto::FindingsResponse> {
    let rows = store.findings(version_id, q).await?;
    Ok(dto::FindingsResponse {
        findings: rows
            .into_iter()
            .map(|r| dto::StoredFindingDto {
                seq: r.seq,
                finding: (&r.finding).convert(),
            })
            .collect(),
    })
}

pub async fn slice_facts(
    store: &dyn StorageBackend,
    version_id: &str,
    scope_prefix: Option<String>,
    as_of_seq: Option<Seq>,
    statuses: Vec<ClaimStatus>,
    limit: Option<usize>,
) -> Result<dto::FactsResponse> {
    let facts = store
        .slice_facts(
            version_id,
            &FactQuery {
                scope_prefix,
                as_of_seq,
                statuses,
                limit,
            },
        )
        .await?;
    let head_seq = store.head(version_id).await?;
    Ok(dto::FactsResponse {
        facts: facts.into_iter().map(convert).collect(),
        as_of_seq: as_of_seq.unwrap_or(0),
        head_seq,
    })
}

pub async fn compose_context(
    store: &dyn StorageBackend,
    version_id: &str,
    scope: Option<&str>,
    budget_tokens: Option<u64>,
    fact_limit: Option<usize>,
    as_of_seq: Option<Seq>,
    as_of_date: Option<&str>,
) -> Result<dto::ComposedContextDto> {
    if as_of_date.is_some() {
        // Rejected BY DECISION, not by omission (re-affirmed 2026-08-17
        // after the Part I audit): a calendar-date pin needs an
        // assertion-date source — the original kernel resolved dates through
        // per-unit `as_of:` metadata stamped into version metadata, and the server
        // records no equivalent. Resolving against `recorded_at` (ingestion
        // time) would SILENTLY diverge from those semantics whenever
        // ingestion lags assertion, which is exactly the corpus where date
        // pins matter. Until per-event assertion-date metadata exists,
        // rejecting here (the one shared implementation) keeps both planes
        // honest and identical; a future date->seq resolver replaces this
        // guard in place. docs/api/rest.md carries the caller-facing note.
        return Err(KernelError::InvalidInput(
            "as_of_date is not yet implemented; pin with as_of_seq".into(),
        ));
    }
    let snapshot = load_snapshot(store, version_id, None, fact_limit, as_of_seq).await?;
    let ctx = compose(&snapshot, scope, budget_tokens);
    Ok(dto::ComposedContextDto {
        sections: ctx
            .sections
            .iter()
            .map(|(title, body)| dto::SectionDto {
                title: title.clone(),
                body: body.clone(),
            })
            .collect(),
        text: ctx.text(),
        estimated_tokens: ctx.estimated_tokens(),
        content_hash: ctx.content_hash(),
        as_of_seq: as_of_seq.unwrap_or(0),
    })
}

pub async fn get_claim(
    store: &dyn StorageBackend,
    claim_id: &str,
) -> Result<dto::GetClaimResponse> {
    let claim = store
        .get_claim(claim_id)
        .await?
        .ok_or(KernelError::NotFound {
            kind: "claim",
            id: claim_id.to_string(),
        })?;
    let superseded_by = store.superseded_by(claim_id).await?;
    Ok(dto::GetClaimResponse {
        claim: claim.convert(),
        superseded: superseded_by.is_some(),
        superseded_by,
    })
}
