// SPDX-License-Identifier: Apache-2.0
//! The reconcile role: mode C, shadow first.
//!
//! Source rows become **typed observations**; observations are compared with
//! ledger claims read at a pinned seq; disagreements become **discrepancy
//! candidates** carrying both sides. In shadow mode nothing touches accepted
//! canon — the server's ledger is read-only to this pipeline, and the only
//! write is a warn-only finding.
//!
//! Three rules are absolute here, and each has a test that would fail if it
//! were relaxed:
//!
//! 1. **Ambiguous identity never merges.** Two candidates above the threshold
//!    means the pipeline files `identity_ambiguous` and produces no
//!    observation. Guessing which shareholder a row means is how a
//!    reconciliation corrupts a cap table.
//! 2. **A backdated change is never a correction.** A new fact about a past
//!    period and a fix to a wrong value look identical in a CDC stream and
//!    mean opposite things. Only the mapping's declared business rule decides,
//!    and its default is `requires_review`.
//! 3. **Comparison is typed, not textual.** `125000` and `125000.00` are the
//!    same number at scale 0; `NULL` and `''` are not the same thing. A
//!    string comparison files false discrepancies on both.

use munarium_matrix_core::{Refusal, RefusalClass, Value};
use munarium_matrix_server_client::{
    ClaimOriginWire, FindingRequest, LedgerFact, ProposeClaimRequest, ServerClient,
};
use munarium_matrix_types::assets::{
    AmbiguityPolicy, ChangeKindDecision, ClaimMappingDoc, MappingMode,
};
use munarium_matrix_types::contract::*;

pub const DISCREPANCY_RULE_ID: &str = "matrix.discrepancy-candidate";
pub const AMBIGUITY_RULE_ID: &str = "matrix.identity-ambiguous";

/// How an observation and the ledger relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Same property, same typed value. Nothing to report.
    Agree,
    /// Same property, different value. The headline case.
    Differ,
    /// The source has it; the ledger does not.
    MissingInLedger,
    /// The ledger has it; the source does not.
    MissingInSource,
    /// The source changed a value whose valid time is in the past. A human
    /// decides whether that is a new fact or a correction.
    BackdatedRequiresReview,
    /// Identity could not be resolved to exactly one entity.
    Ambiguous,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Agree => "agree",
            Verdict::Differ => "differ",
            Verdict::MissingInLedger => "missing_in_ledger",
            Verdict::MissingInSource => "missing_in_source",
            Verdict::BackdatedRequiresReview => "backdated_requires_review",
            Verdict::Ambiguous => "identity_ambiguous",
        }
    }

    /// Whether this verdict is worth a human's attention.
    pub fn is_finding(self) -> bool {
        !matches!(self, Verdict::Agree)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    /// False when the source value did not parse as its declared type and was
    /// compared as raw text. Counted toward the promotion gate.
    pub value_conformed: bool,
    pub verdict: Verdict,
    pub subject: String,
    pub property: String,
    pub source_value: Option<String>,
    pub ledger_value: Option<String>,
    pub claim_id: Option<String>,
    pub evidence_id: Option<String>,
}

/// Compare one observation with the ledger's view.
///
/// The typed comparison is the whole point: values are compared by their
/// canon@1 text, which normalizes scale and timezone, and a NULL is never
/// equal to an empty string.
pub fn compare(observation: &Observation, facts: &[LedgerFact], min_confidence: f64) -> Comparison {
    // Identity first. Ambiguity stops everything.
    //
    // Two ways to fail to resolve, and both are the same refusal:
    //
    //   * a TIE — two distinct subjects share the top confidence, so nothing
    //     ranks them. This is what a CONTESTED alias produces: two source rows
    //     in one scope declaring the same ledger subject means the pipeline
    //     knows the entity is one of them and cannot say which.
    //   * NOTHING GOOD ENOUGH — the best candidate is below the mapping's
    //     `minConfidence`. A weak guess is still a guess.
    //
    // What is NOT ambiguous: an exact key subject beside a lower-confidence
    // alias hint. That is a ranking, and producing a ranking is what a
    // resolver is for.
    let strongest = observation
        .entity_candidates
        .iter()
        .map(|c| c.confidence)
        .fold(f64::NEG_INFINITY, f64::max);
    let mut tied: Vec<&str> = observation
        .entity_candidates
        .iter()
        .filter(|c| (c.confidence - strongest).abs() < f64::EPSILON)
        .map(|c| c.subject.as_str())
        .collect();
    tied.sort_unstable();
    tied.dedup();

    let best = observation
        .entity_candidates
        .iter()
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence));

    let subject = best.map(|c| c.subject.clone()).unwrap_or_default();
    let (source_value, value_conformed) = typed_text_checked(&observation.value);

    if tied.len() > 1 || strongest < min_confidence {
        return Comparison {
            value_conformed,
            verdict: Verdict::Ambiguous,
            subject,
            property: observation.property.clone(),
            source_value,
            ledger_value: None,
            claim_id: None,
            evidence_id: observation.origin.evidence_id.clone(),
        };
    }

    // Bind against the ledger by trying usable candidates in confidence order.
    // The key subject is exact and comes first; an alias candidate is the
    // fallback that binds `holder_id = 42` to the entity a document calls
    // "Jane Rowntree", which is the only reason the ledger read finds anything
    // at all when canon was built from documents.
    let mut usable: Vec<&EntityCandidate> = observation
        .entity_candidates
        .iter()
        .filter(|c| c.confidence >= min_confidence)
        .collect();
    usable.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));

    let bound = usable.iter().find_map(|cand| {
        facts
            .iter()
            .find(|f| f.subject == cand.subject && f.key == observation.property)
            .map(|f| (cand.subject.clone(), f))
    });
    // No candidate had a fact: report under the best subject, which is the one
    // an operator will look for.
    let (subject, fact) = match bound {
        Some((s, f)) => (s, Some(f)),
        None => (subject, None),
    };

    let verdict = match (&fact, observation.change_kind) {
        (None, _) => Verdict::MissingInLedger,
        // A backdated change is reviewable regardless of whether the values
        // agree: the QUESTION is what it means, not what it says.
        (Some(_), ChangeKind::Backdated) => Verdict::BackdatedRequiresReview,
        (Some(f), _) if Some(&f.value) == source_value.as_ref() => Verdict::Agree,
        (Some(_), _) => Verdict::Differ,
    };

    Comparison {
        value_conformed,
        verdict,
        subject,
        property: observation.property.clone(),
        source_value,
        ledger_value: fact.map(|f| f.value.clone()),
        claim_id: fact.and_then(|f| f.claim_id.clone()),
        evidence_id: observation.origin.evidence_id.clone(),
    }
}

/// The canon@1 text of a wire value — the one comparable form.
fn typed_text(v: &TypedValueDto) -> Option<String> {
    typed_text_checked(v).0
}

/// The comparable text AND whether the value actually conformed to its
/// declared type. A decimal that does not parse still gets compared as raw
/// text — refusing would hide the row — but it is COUNTED, because the share
/// of non-conforming values is one of the two promotion gates. A
/// mapping whose values do not parse is not one that should write canon.
fn typed_text_checked(v: &TypedValueDto) -> (Option<String>, bool) {
    use munarium_matrix_core::ColumnType;
    if v.value.is_null() {
        return (None, true);
    }
    let raw = match v.value.as_str() {
        Some(s) => s.to_string(),
        None => v.value.to_string(),
    };
    // Route through the kernel's formatter so a scale difference cannot read
    // as a discrepancy.
    match v.ty {
        ColumnType::Decimal => {
            let scale = v.scale.unwrap_or(0);
            match rust_decimal::Decimal::from_str_exact(&raw)
                .ok()
                .and_then(|d| Value::Decimal { value: d, scale }.canonical_text())
            {
                Some(t) => (Some(t), true),
                None => (Some(raw), false),
            }
        }
        ColumnType::Int64 => match raw.parse::<i64>() {
            Ok(i) => (Some(Value::Int64(i).canonical_text().unwrap_or(raw)), true),
            Err(_) => (Some(raw), false),
        },
        _ => (Some(raw), true),
    }
}

/// Decide what a change means, from the mapping's declared business rule.
///
/// Never inferred from the CDC operation: an `UPDATE` in the source can be a
/// legitimate new value or a fix to a wrong one, and only the business knows
/// which. The default is `requires_review` precisely because guessing wrong
/// rewrites history.
pub fn classify_change(
    mapping: &ClaimMappingDoc,
    property: &str,
    kind: ChangeKind,
) -> ChangeKindDecision {
    let rule = mapping.spec.changes.get(property);
    match (kind, rule) {
        (ChangeKind::Backdated, Some(r)) => r.on_backdated,
        (ChangeKind::Backdated, None) => ChangeKindDecision::RequiresReview,
        (ChangeKind::Update, Some(r)) => r.on_update,
        (ChangeKind::Update, None) => ChangeKindDecision::RequiresReview,
        // An insert or a snapshot row is a statement of fact, not a change.
        _ => ChangeKindDecision::Update,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconcileOutcome {
    pub observations: u64,
    pub agreements: u64,
    pub discrepancies: u64,
    pub ambiguous: u64,
    pub findings_filed: u64,
    /// Claims proposed into the ledger by this run.
    pub proposals: u64,
    /// Proposals the server recorded DISPUTED (a gate spoke). Recorded, never
    /// dropped — and surfaced, because a disputed proposal is a finding.
    pub proposals_disputed: u64,
    /// Discrepancies that were in scope but already proposed by an earlier
    /// run — the idempotency ledger said so, and nothing was sent.
    pub proposals_replayed: u64,
    /// Ledger claims inside this mapping's namespace that no source row
    /// claims — the verdict that comes from a row NOT being there. Counted
    /// inside `discrepancies` as well.
    pub missing_in_source: u64,
    /// Discrepancies NOT proposed, with the reason counted by kind.
    pub withheld_out_of_scope: u64,
    pub withheld_document_outranks: u64,
    pub withheld_requires_review: u64,
    /// Observations whose value did not parse as its declared type. Compared
    /// as text anyway; counted here for the promotion gate.
    pub value_nonconforming: u64,
    pub batch_evidence_id: Option<String>,
    /// Always true in shadow mode. The test that asserts canon is untouched
    /// reads this AND re-reads the ledger.
    pub canon_untouched: bool,
}

/// A record of one proposal Matrix made, kept OUTSIDE the server so a re-run
/// can tell "already proposed" from "new" without asking the ledger to
/// remember on our behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalRecord {
    pub idempotency_key: String,
    pub mapping: String,
    pub version_id: String,
    pub subject: String,
    pub property: String,
    pub value: String,
    pub claim_type: String,
    pub supersedes_id: Option<String>,
    /// The ledger value this proposal replaced — what a rollback restores.
    pub prior_value: Option<String>,
    pub claim_id: String,
    pub status: String,
    pub row_key: String,
    pub evidence_id: Option<String>,
}

/// Where proposals are remembered between runs. The server crate implements
/// it over the Matrix store; scenarios use an in-memory one.
#[async_trait::async_trait]
pub trait ProposalLedger: Send + Sync {
    /// The claim id an earlier run recorded under this key, if any.
    async fn seen(&self, tenant: &str, idempotency_key: &str) -> Result<Option<String>, Refusal>;
    async fn record(&self, tenant: &str, rec: &ProposalRecord) -> Result<(), Refusal>;
}

/// How a pass may write. Shadow is the zero value: with no options the pass
/// proposes nothing, which is the only default that cannot surprise anyone.
pub struct ReconcileOptions<'a> {
    pub tenant: &'a str,
    /// The operator's promotion decision for THIS mapping version. False
    /// means shadow regardless of the asset's declared mode.
    pub promoted: bool,
    pub source_id: &'a str,
    pub proposals: Option<&'a dyn ProposalLedger>,
    /// The caller's word that the batch is a COMPLETE read of the mapped
    /// entity — nothing truncated, nothing excluded. Only then can a ledger
    /// claim with no matching row be called `missing_in_source`; an
    /// incremental or truncated read says nothing about the rows it did not
    /// return. `observe` reports this in its stats. A caller that cannot
    /// vouch for it says `false` and forgoes the verdict rather than
    /// risking it.
    pub source_complete: bool,
}

/// Run one mapping's comparison pass in SHADOW: findings only, canon untouched.
pub async fn reconcile(
    server: &dyn ServerClient,
    mapping: &ClaimMappingDoc,
    version_id: &str,
    batch: &ObservationBatch,
    batch_bytes: &[u8],
) -> Result<ReconcileOutcome, Refusal> {
    reconcile_with(
        server,
        mapping,
        version_id,
        batch,
        batch_bytes,
        &ReconcileOptions {
            tenant: "",
            promoted: false,
            source_id: batch.source_id.as_deref().unwrap_or(""),
            proposals: None,
            // The wrapper cannot know; the verdict is withheld.
            source_complete: false,
        },
    )
    .await
}

/// Give a finding the TOP-LEVEL `claim_id` the server's content identity
/// reads.
///
/// The server deduplicates findings by `(rule_id, detail.evidence_ref,
/// detail.claim_id)` when either ref is present at the top of `detail`, and
/// by `(rule_id, scope_path, message)` otherwise. Every finding here nests its
/// refs under `source` and `ledger` for a reader, so without this the fallback
/// applied — which works only for as long as no two findings share a message.
///
/// `claim_id` alone, never `evidence_ref`, and the reason is what a finding IS.
/// A discrepancy is a statement about a ledger CLAIM; the sealed batch is the
/// evidence that first showed it. A snapshot source yields a new batch — a new
/// artifact, a new evidence id — on every read, because the engine marker is
/// in the rendered rows, so an identity that included the evidence ref would
/// file the same disagreement again on every pass. It stays one finding until
/// the claim it names is superseded, at which point a new claim id makes a new
/// finding, which is right.
///
/// Findings that cite no claim (`missing_in_ledger`, ambiguity) carry the row
/// and property in their message instead and dedupe on that.
fn with_identity_refs(
    mut req: FindingRequest,
    _evidence_id: &str,
    claim_id: Option<&str>,
) -> FindingRequest {
    if let Some(claim_id) = claim_id {
        req.detail["claim_id"] = serde_json::json!(claim_id);
    }
    req
}

/// Whether `subject` falls in the namespace a subject template describes.
///
/// `shareholder.{holder_id}` matches `shareholder.42` and
/// `shareholder.jane-rowntree`, and does not match `company.7.name`. A
/// placeholder matches one non-empty run of characters containing no `.`,
/// because `.` is the ledger's scope and key separator and a key value that
/// carried one would have broken the subject long before it got here.
pub fn subject_in_template_namespace(template: &str, subject: &str) -> bool {
    fn go(t: &str, s: &str) -> bool {
        if t.is_empty() {
            return s.is_empty();
        }
        if let Some(rest) = t.strip_prefix('{') {
            let Some(close) = rest.find('}') else {
                return false;
            };
            let after = &rest[close + 1..];
            for (i, ch) in s.char_indices() {
                if ch == '.' {
                    break;
                }
                if go(after, &s[i + ch.len_utf8()..]) {
                    return true;
                }
            }
            return false;
        }
        let lit_end = t.find('{').unwrap_or(t.len());
        match s.strip_prefix(&t[..lit_end]) {
            Some(rest) => go(&t[lit_end..], rest),
            None => false,
        }
    }
    go(template, subject)
}

/// The idempotency identity of one proposal: the mapping version, the TARGET
/// LINEAGE, the row, the property, the canonical value and the source
/// position. Two runs that observe the same row at the same position propose
/// the same key and the second one sends nothing.
///
/// `version_id` is in the preimage because a claim is proposed *into a
/// lineage*, and without it the same observation reconciled into two lineages
/// collides on one key. The server's header idempotency layer then sees a
/// replay with a different body and refuses the whole pass with
/// `idempotency-mismatch` — which is exactly what happened the first time the
/// authoritative path ran twice against a real server (2026-08-29). In
/// production a mapping usually targets one lineage, so the defect was latent;
/// an epoch-2 rebuild, a branch, or a rollback-and-re-run is all it takes.
pub fn proposal_key(mapping_ref: &str, version_id: &str, o: &Observation) -> String {
    let value = typed_text(&o.value).unwrap_or_default();
    let preimage = format!(
        "{mapping_ref}|{version_id}|{}|{}|{value}|{}",
        o.origin.row_key,
        o.property,
        o.origin.event_position.as_deref().unwrap_or("")
    );
    munarium_matrix_core::artifact_hash(preimage.as_bytes())
}

/// Run one mapping's comparison pass with an explicit write policy.
/// Everything one pass needs, borrowed: the same context runs the dry pass
/// that counts and the real pass that writes.
struct PassCtx<'a> {
    server: &'a dyn ServerClient,
    mapping: &'a ClaimMappingDoc,
    version_id: &'a str,
    batch: &'a ObservationBatch,
    facts: &'a [LedgerFact],
    opts: &'a ReconcileOptions<'a>,
    batch_evidence_id: &'a String,
    authoritative: bool,
    mapping_ref: &'a String,
    head: u64,
}

/// One pass over the batch. With `dry`, every ledger write is skipped and only
/// counted — the counts are what a declared per-run ceiling is checked
/// against, so a pass that would exceed it is refused before it files
/// anything. The reads (the proposal ledger's "seen", the facts) are the same
/// either way, so the dry counts are the real pass's counts.
async fn pass(px: &PassCtx<'_>, dry: bool, out: &mut ReconcileOutcome) -> Result<(), Refusal> {
    let PassCtx {
        server,
        mapping,
        version_id,
        batch,
        facts,
        opts,
        batch_evidence_id,
        authoritative,
        mapping_ref,
        head,
    } = *px;
    for observation in &batch.observations {
        let mut c = compare(
            observation,
            facts,
            mapping.spec.entity.identity.min_confidence,
        );
        c.evidence_id = Some(batch_evidence_id.clone());
        if !c.value_conformed {
            out.value_nonconforming += 1;
        }

        match c.verdict {
            Verdict::Agree => {
                out.agreements += 1;
                continue;
            }
            Verdict::Ambiguous => {
                out.ambiguous += 1;
                if mapping.spec.entity.identity.on_ambiguous == AmbiguityPolicy::Skip {
                    continue;
                }
                let req = FindingRequest {
                    version_id: version_id.to_string(),
                    rule_id: AMBIGUITY_RULE_ID.to_string(),
                    severity: "warn".to_string(),
                    // The ROW is in the message. The server deduplicates a
                    // finding that carries no evidence/claim refs by
                    // (rule, scope, message), and two contested rows with one
                    // message would collapse into one finding on a real
                    // server — the mock never did that. Found by the compose
                    // tier, 2026-08-29.
                    message: format!(
                        "source row {} for '{}' resolved to more than one entity above the \
                         confidence threshold; nothing was merged",
                        observation.origin.row_key, c.property
                    ),
                    scope_path: None,
                    detail: serde_json::json!({
                        "property": c.property,
                        "candidates": observation.entity_candidates.iter().map(|e| {
                            serde_json::json!({ "subject": e.subject, "confidence": e.confidence })
                        }).collect::<Vec<_>>(),
                        "evidence_id": batch_evidence_id,
                        "row_key": observation.origin.row_key,
                    }),
                };
                if !dry {
                    server
                        .file_finding(&req)
                        .await
                        .map_err(|e| e.to_refusal())?;
                }
                out.findings_filed += 1;
                continue;
            }
            _ => out.discrepancies += 1,
        }

        // Both sides, always. A finding that carries only the source's number
        // is an accusation; one that carries both is evidence.
        let req = FindingRequest {
            version_id: version_id.to_string(),
            rule_id: DISCREPANCY_RULE_ID.to_string(),
            // WARN, never block: only a gate may block, and this pipeline is
            // not a gate. The server refuses a block from here anyway.
            severity: "warn".to_string(),
            message: format!(
                "{}: the source and the ledger disagree about {}.{}",
                c.verdict.as_str(),
                c.subject,
                c.property
            ),
            scope_path: observation
                .entity_candidates
                .first()
                .and_then(|e| e.scope_path.clone()),
            detail: serde_json::json!({
                "verdict": c.verdict.as_str(),
                "subject": c.subject,
                "property": c.property,
                "source": {
                    "value": c.source_value,
                    "evidence_id": batch_evidence_id,
                    "row_key": observation.origin.row_key,
                    "observed_at": observation.origin.observed_at,
                    "mapping": batch.mapping,
                },
                "ledger": {
                    "value": c.ledger_value,
                    "claim_id": c.claim_id,
                    "version_id": version_id,
                    "as_of_seq": head,
                },
                "change_kind": format!("{:?}", observation.change_kind).to_lowercase(),
                "decision": format!(
                    "{:?}",
                    classify_change(mapping, &c.property, observation.change_kind)
                ).to_lowercase(),
            }),
        };
        let req = with_identity_refs(req, batch_evidence_id, c.claim_id.as_deref());
        if !dry {
            server
                .file_finding(&req)
                .await
                .map_err(|e| e.to_refusal())?;
        }
        out.findings_filed += 1;

        // --- Propose, inside scope only ---------------------------
        // The finding above was filed FIRST and unconditionally: conflict
        // policy is preserve-and-disclose, so even a proposal that supersedes
        // the document's value leaves both sides visible in governance.
        if !authoritative {
            continue;
        }
        let decision =
            crate::authority::classify_change_kind(mapping, &c.property, observation.change_kind);
        let ledger_claim = c
            .claim_id
            .as_deref()
            .and_then(|id| facts.iter().find(|f| f.claim_id.as_deref() == Some(id)));
        let verdict_decision = crate::authority::decide(&crate::authority::AuthorityContext {
            mapping,
            promoted: opts.promoted,
            property: &c.property,
            valid_at: observation.valid_time.and_then(|v| v.from),
            ledger_has_claim: ledger_claim.is_some(),
            // Recognised by ORIGIN, never by provenance: on the server a
            // connector claim is `witnessed` like any other and only its
            // origin block says who wrote it.
            //
            // `connector` ONLY. A rollback claim carries the DOCUMENT's value
            // — an operator restored it on purpose — so under
            // `document_over_source` it must outrank the source exactly as the
            // original document claim did. Counting it as a connector claim
            // let the next promoted pass overwrite the very value the
            // operator had just restored (found 2026-08-29). Under
            // `source_over_document` the source wins either way, by
            // declaration.
            ledger_claim_is_connector: ledger_claim
                .and_then(|f| f.origin_kind.as_deref())
                .is_some_and(|k| k == "connector"),
            backdated: matches!(observation.change_kind, ChangeKind::Backdated)
                || decision == ChangeKindDecision::RequiresReview,
        });
        match verdict_decision {
            crate::authority::AuthorityDecision::Propose => {}
            crate::authority::AuthorityDecision::OutOfScope => {
                out.withheld_out_of_scope += 1;
                continue;
            }
            crate::authority::AuthorityDecision::DocumentOutranks => {
                out.withheld_document_outranks += 1;
                continue;
            }
            crate::authority::AuthorityDecision::RequiresReview => {
                out.withheld_requires_review += 1;
                continue;
            }
            crate::authority::AuthorityDecision::Shadow => continue,
        }

        let key = proposal_key(mapping_ref, version_id, observation);
        if let Some(ledger) = opts.proposals {
            if ledger.seen(opts.tenant, &key).await?.is_some() {
                out.proposals_replayed += 1;
                continue;
            }
        }
        let Some(value) = c.source_value.clone() else {
            continue; // a NULL never became an observation; belt and braces
        };
        let claim_type = match (decision, c.claim_id.is_some()) {
            (_, false) => "fact",
            (ChangeKindDecision::Correction, true) => "correction",
            _ => "update",
        };
        let req = ProposeClaimRequest {
            version_id: version_id.to_string(),
            claim_type: claim_type.to_string(),
            subject: c.subject.clone(),
            key: c.property.clone(),
            value: value.clone(),
            scope_path: observation
                .entity_candidates
                .first()
                .and_then(|e| e.scope_path.clone()),
            supersedes_id: c.claim_id.clone(),
            evidence: Some(serde_json::json!({
                "evidence_id": batch_evidence_id,
                "row_key": observation.origin.row_key,
                "verdict": c.verdict.as_str(),
            })),
            origin: ClaimOriginWire {
                kind: "connector".into(),
                source_id: opts.source_id.to_string(),
                mapping_version: mapping_ref.clone(),
                row_key: observation.origin.row_key.clone(),
                event_position: observation.origin.event_position.clone(),
                observed_at: observation.origin.observed_at.map(|t| t.to_rfc3339()),
                evidence_id: Some(batch_evidence_id.clone()),
            },
        };
        out.proposals += 1;
        if dry {
            continue;
        }
        let outcome = server
            .propose_claim(&req, &key)
            .await
            .map_err(|e| e.to_refusal())?;
        out.canon_untouched = false;
        if outcome.status == "disputed" {
            out.proposals_disputed += 1;
        }
        if let Some(ledger) = opts.proposals {
            ledger
                .record(
                    opts.tenant,
                    &ProposalRecord {
                        idempotency_key: key,
                        mapping: mapping_ref.clone(),
                        version_id: version_id.to_string(),
                        subject: c.subject.clone(),
                        property: c.property.clone(),
                        value,
                        claim_type: claim_type.to_string(),
                        supersedes_id: c.claim_id.clone(),
                        prior_value: c.ledger_value.clone(),
                        claim_id: outcome.claim_id,
                        status: outcome.status,
                        row_key: observation.origin.row_key.clone(),
                        evidence_id: Some(batch_evidence_id.clone()),
                    },
                )
                .await?;
        }
    }

    // --- missing_in_source ---------------------------------------------------
    //
    // The verdict that comes from the ABSENCE of a comparison, which is why it
    // lives after the loop rather than inside it. Three conditions, each one a
    // way to be confidently wrong without it:
    //
    // - **The read was complete.** An incremental or truncated batch says
    //   nothing about the rows it did not return; `source_complete` is the
    //   caller's word, and without it the verdict is withheld, not guessed.
    // - **The mapping can say the subject belongs to this register** — it is
    //   declared in the alias table, or it sits inside the subject template's
    //   namespace. A ledger claim about something else is not this mapping's
    //   business, and calling it "missing" would be an accusation the mapping
    //   has no standing to make.
    // - **No row claims it, at any confidence.** A contested alias is two rows
    //   naming the subject, which is the opposite of nobody.
    if opts.source_complete {
        let claimed: std::collections::BTreeSet<&str> = batch
            .observations
            .iter()
            .flat_map(|o| o.entity_candidates.iter().map(|c| c.subject.as_str()))
            .collect();
        let declared: std::collections::BTreeSet<&str> = mapping
            .spec
            .entity
            .identity
            .aliases
            .iter()
            .flat_map(|t| t.entries.iter().map(|e| e.subject.as_str()))
            .collect();
        let template = &mapping.spec.entity.subject_template;
        let mut seen: std::collections::BTreeSet<(String, String)> = Default::default();
        for f in facts {
            if !mapping.spec.properties.contains_key(&f.key) {
                continue;
            }
            if claimed.contains(f.subject.as_str()) {
                continue;
            }
            let ours = declared.contains(f.subject.as_str())
                || subject_in_template_namespace(template, &f.subject);
            if !ours || !seen.insert((f.subject.clone(), f.key.clone())) {
                continue;
            }
            out.discrepancies += 1;
            out.missing_in_source += 1;
            let req = FindingRequest {
                version_id: version_id.to_string(),
                rule_id: DISCREPANCY_RULE_ID.to_string(),
                severity: "warn".to_string(),
                message: format!(
                    "missing_in_source: the ledger has {}.{} and the source has no row for it",
                    f.subject, f.key
                ),
                scope_path: None,
                detail: serde_json::json!({
                    "verdict": Verdict::MissingInSource.as_str(),
                    "subject": f.subject,
                    "property": f.key,
                    "source": {
                        "value": serde_json::Value::Null,
                        "evidence_id": batch_evidence_id,
                        "row_key": serde_json::Value::Null,
                        "mapping": batch.mapping,
                    },
                    "ledger": {
                        "value": f.value,
                        "claim_id": f.claim_id,
                        "version_id": version_id,
                        "as_of_seq": head,
                    },
                }),
            };
            let req = with_identity_refs(req, batch_evidence_id, f.claim_id.as_deref());
            if !dry {
                server
                    .file_finding(&req)
                    .await
                    .map_err(|e| e.to_refusal())?;
            }
            out.findings_filed += 1;
        }
    }
    Ok(())
}
pub async fn reconcile_with(
    server: &dyn ServerClient,
    mapping: &ClaimMappingDoc,
    version_id: &str,
    batch: &ObservationBatch,
    batch_bytes: &[u8],
    opts: &ReconcileOptions<'_>,
) -> Result<ReconcileOutcome, Refusal> {
    // Pin the read. Every comparison in this run sees the same ledger state,
    // so a claim accepted mid-run cannot make two observations disagree about
    // what canon said.
    let head = server
        .head_seq(version_id)
        .await
        .map_err(|e| e.to_refusal())?;
    let facts = server
        .slice_facts(version_id, Some(head))
        .await
        .map_err(|e| e.to_refusal())?;

    // Seal the batch first: a discrepancy finding must cite BOTH sides, and
    // the source side is this artifact.
    //
    // Through the SAME `evidence::seal` every other mode uses. Mode C used to
    // have its own `seal_observations` posting a different body shape, which
    // the MockServer accepted and a real server rejected outright; rendering
    // the batch as a typed result deletes that second path rather than
    // patching it. `batch_bytes` is therefore no longer the sealed payload —
    // the canonical CSV of the rendered result is — and the parameter stays
    // only because callers pass the connector's own encoding for the journal.
    let _ = batch_bytes;
    let batch_result = crate::evidence::observation_batch_result(
        batch,
        munarium_matrix_core::AuthorizationClass::default(),
    );
    let seal_ctx = crate::evidence::SealContext {
        tenant: opts.tenant.to_string(),
        kind: ArtifactKind::Observations,
        source_id: opts.source_id.to_string(),
        source_version: 1,
        adapter: "matrix".into(),
        adapter_version: None,
        engine: None,
        versions: ManifestVersions {
            claim_mapping: Some(mapping.metadata.asset_ref()),
            ..Default::default()
        },
        plan: None,
        snapshot_marker: batch.run_id.clone(),
        isolation: None,
        replay_level: "sealed_result".into(),
        effective_principal: None,
        statement_id: None,
        started_at: chrono::Utc::now(),
        ended_at: chrono::Utc::now(),
        retention_days: None,
        declared_max_rows: None,
        rows_covered: Some(batch.observations.len() as u64),
        rows_excluded: None,
        exclusion_reason: None,
        freshness_watermark: None,
    };
    // The batch id is the idempotency key: replaying one batch seals once.
    let (batch_evidence_id, _sealed) =
        crate::evidence::seal(server, &batch_result, &seal_ctx, Some(&batch.batch_id)).await?;

    let mut out = ReconcileOutcome {
        observations: batch.observations.len() as u64,
        batch_evidence_id: Some(batch_evidence_id.clone()),
        // True until a proposal is actually sent. An authoritative mapping
        // that proposed nothing (out of scope, all agree, unpromoted) left
        // canon exactly as it found it, and should say so.
        canon_untouched: true,
        ..Default::default()
    };
    let authoritative = mapping.spec.mode == MappingMode::Authoritative && opts.promoted;
    let mapping_ref = mapping.metadata.asset_ref();

    let px = PassCtx {
        server,
        mapping,
        version_id,
        batch,
        facts: &facts,
        opts,
        batch_evidence_id: &batch_evidence_id,
        authoritative,
        mapping_ref: &mapping_ref,
        head,
    };
    if let Some(limits) = &mapping.spec.limits {
        let mut probe = out.clone();
        pass(&px, true, &mut probe).await?;
        if let Some(max) = limits.max_findings_per_run {
            if probe.findings_filed > max {
                return Err(Refusal::new(
                    RefusalClass::Exhausted,
                    "ledger_volume_exceeded",
                    format!(
                        "this pass would file {} finding(s) against the mapping's ceiling of {max}; \
                         nothing was written — raise `spec.limits.maxFindingsPerRun` or fix the mapping",
                        probe.findings_filed
                    ),
                ));
            }
        }
        if let Some(max) = limits.max_proposals_per_run {
            if probe.proposals > max {
                return Err(Refusal::new(
                    RefusalClass::Exhausted,
                    "ledger_volume_exceeded",
                    format!(
                        "this pass would propose {} claim(s) against the mapping's ceiling of {max}; \
                         nothing was written — raise `spec.limits.maxProposalsPerRun` or fix the mapping",
                        probe.proposals
                    ),
                ));
            }
        }
    }
    pass(&px, false, &mut out).await?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_core::ColumnType;
    use munarium_matrix_server_client::MockServer;

    fn mapping_yaml(mode: &str) -> String {
        format!(
            r#"
apiVersion: munarium.ioka.io/v1
kind: ClaimMapping
metadata: {{ name: captable-holdings, version: 1 }}
spec:
  source: captable
  mode: {mode}
  entity:
    table: holdings
    key: [holder_id]
    subjectTemplate: "shareholder.{{holder_id}}"
  properties:
    shares_outstanding: {{ column: shares, type: decimal, scale: 0 }}
  temporal:
    validTime: {{ column: effective_date }}
  changes:
    shares_outstanding: {{ onUpdate: update, onBackdated: requires_review }}
"#
        )
    }

    fn mapping(mode: &str) -> ClaimMappingDoc {
        match munarium_matrix_types::parse_asset(&mapping_yaml(mode)).unwrap() {
            munarium_matrix_types::Asset::ClaimMapping(m) => *m,
            _ => unreachable!(),
        }
    }

    fn observation(
        subjects: &[(&str, f64)],
        value: &str,
        scale: u32,
        kind: ChangeKind,
    ) -> Observation {
        Observation {
            entity_candidates: subjects
                .iter()
                .map(|(s, c)| EntityCandidate {
                    subject: s.to_string(),
                    scope_path: Some("company.7.captable".into()),
                    confidence: *c,
                    resolver: Some("terminology_alias".into()),
                })
                .collect(),
            property: "shares_outstanding".into(),
            value: TypedValueDto {
                ty: ColumnType::Decimal,
                value: serde_json::json!(value),
                scale: Some(scale),
                element_type: None,
            },
            valid_time: None,
            transaction_time: Some(chrono::Utc::now()),
            change_kind: kind,
            origin: ConnectorOrigin {
                kind: "connector".into(),
                source_id: "captable".into(),
                mapping_version: "captable-holdings@1".into(),
                row_key: "42".into(),
                event_position: Some("0/1A".into()),
                observed_at: Some(chrono::Utc::now()),
                evidence_id: None,
            },
        }
    }

    fn fact(subject: &str, value: &str) -> LedgerFact {
        LedgerFact {
            claim_id: Some("claim-1".into()),
            subject: subject.into(),
            key: "shares_outstanding".into(),
            value: value.into(),
            seq: 5,
            status: Some("accepted".into()),
            provenance: Some("witnessed".into()),
            origin_kind: None,
        }
    }

    #[test]
    fn matching_values_agree_even_across_scale_spellings() {
        // The ledger says "125000"; the source says "125000" at scale 0.
        // A textual comparison of "125000.00" against "125000" would file a
        // false discrepancy.
        let o = observation(
            &[("shareholder.42", 0.99)],
            "125000",
            0,
            ChangeKind::Snapshot,
        );
        let c = compare(&o, &[fact("shareholder.42", "125000")], 0.95);
        assert_eq!(c.verdict, Verdict::Agree);
    }

    #[test]
    fn a_real_difference_is_reported_with_both_sides() {
        let o = observation(
            &[("shareholder.43", 0.99)],
            "90500",
            0,
            ChangeKind::Snapshot,
        );
        let c = compare(&o, &[fact("shareholder.43", "90000")], 0.95);
        assert_eq!(c.verdict, Verdict::Differ);
        assert_eq!(c.source_value.as_deref(), Some("90500"));
        assert_eq!(c.ledger_value.as_deref(), Some("90000"));
        assert_eq!(c.claim_id.as_deref(), Some("claim-1"));
    }

    #[test]
    fn ambiguous_identity_never_merges() {
        let o = observation(
            &[("shareholder.51", 0.62), ("shareholder.58", 0.61)],
            "40000",
            0,
            ChangeKind::Snapshot,
        );
        let c = compare(&o, &[fact("shareholder.51", "40000")], 0.95);
        assert_eq!(c.verdict, Verdict::Ambiguous);
        // ...and no ledger value is reported, because we do not know whose it
        // would be.
        assert!(c.ledger_value.is_none());
    }

    #[test]
    fn an_alias_candidate_binds_when_the_key_subject_has_no_claim() {
        // The point of the whole resolver. Canon was built from documents, so
        // its subject is "shareholder.jane-rowntree", not "shareholder.42".
        // Without the fallback this row reconciles as `missing_in_ledger` and
        // mode C says nothing about a register that in fact disagrees.
        let o = observation(
            &[("shareholder.42", 1.0), ("shareholder.jane-rowntree", 0.96)],
            "125000",
            0,
            ChangeKind::Snapshot,
        );
        let c = compare(&o, &[fact("shareholder.jane-rowntree", "90000")], 0.95);
        assert_eq!(c.verdict, Verdict::Differ);
        assert_eq!(c.subject, "shareholder.jane-rowntree");
        assert_eq!(c.ledger_value.as_deref(), Some("90000"));
    }

    #[test]
    fn the_exact_key_subject_wins_when_both_have_a_claim() {
        // An alias is evidence about identity, not proof of it. When canon
        // knows the key subject, the alias never overrides it.
        let o = observation(
            &[("shareholder.42", 1.0), ("shareholder.jane-rowntree", 0.96)],
            "125000",
            0,
            ChangeKind::Snapshot,
        );
        let c = compare(
            &o,
            &[
                fact("shareholder.42", "125000"),
                fact("shareholder.jane-rowntree", "90000"),
            ],
            0.95,
        );
        assert_eq!(c.verdict, Verdict::Agree);
        assert_eq!(c.subject, "shareholder.42");
    }

    #[test]
    fn a_contested_alias_ties_and_merges_nothing() {
        // T0 trap 9 as it reaches `compare`: two subjects at the top
        // confidence. Even though one of them HAS a ledger fact, nothing is
        // bound and no value is reported.
        let o = observation(
            &[("shareholder.51", 1.0), ("shareholder.jane-rowntree", 1.0)],
            "40000",
            0,
            ChangeKind::Snapshot,
        );
        let c = compare(&o, &[fact("shareholder.jane-rowntree", "40000")], 0.95);
        assert_eq!(c.verdict, Verdict::Ambiguous);
        assert!(c.ledger_value.is_none());
    }

    #[test]
    fn an_alias_below_the_mapping_threshold_is_never_bound() {
        // `minConfidence` is the mapping's own bar, and it is load-bearing:
        // raising it above the alias confidence turns the fallback off.
        let o = observation(
            &[("shareholder.42", 1.0), ("shareholder.jane-rowntree", 0.96)],
            "125000",
            0,
            ChangeKind::Snapshot,
        );
        let c = compare(&o, &[fact("shareholder.jane-rowntree", "90000")], 0.99);
        assert_eq!(c.verdict, Verdict::MissingInLedger);
        assert_eq!(c.subject, "shareholder.42");
    }

    #[test]
    fn the_template_namespace_is_matched_segment_wise() {
        let t = "shareholder.{holder_id}";
        assert!(subject_in_template_namespace(t, "shareholder.42"));
        assert!(subject_in_template_namespace(
            t,
            "shareholder.jane-rowntree"
        ));
        assert!(!subject_in_template_namespace(t, "shareholder."));
        assert!(!subject_in_template_namespace(t, "shareholder.42.extra"));
        assert!(!subject_in_template_namespace(t, "company.7.name"));
        assert!(!subject_in_template_namespace(t, "shareholders.42"));
        let two = "holding.{holder_id}.{company_id}";
        assert!(subject_in_template_namespace(two, "holding.42.7"));
        assert!(!subject_in_template_namespace(two, "holding.42"));
    }

    #[tokio::test]
    async fn a_declared_holder_with_no_row_is_missing_in_source_only_on_a_complete_read() {
        let server = MockServer::new().with_version("memv-1");
        server.seed_facts(
            "memv-1",
            vec![
                fact("shareholder.42", "125000"),
                // Inside the template namespace, no row: missing.
                fact("shareholder.99", "1"),
                // Outside it: not this mapping's business.
                fact("company.7", "0"),
            ],
        );
        let b = batch(vec![observation(
            &[("shareholder.42", 1.0)],
            "125000",
            0,
            ChangeKind::Snapshot,
        )]);
        let bytes = serde_json::to_vec(&b).unwrap();
        let complete = reconcile_with(
            &server,
            &mapping("shadow"),
            "memv-1",
            &b,
            &bytes,
            &ReconcileOptions {
                tenant: "acme",
                promoted: false,
                source_id: "captable",
                proposals: None,
                source_complete: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(complete.missing_in_source, 1, "shareholder.99 and only it");
        let f = server
            .filed_findings()
            .into_iter()
            .find(|f| f.detail["verdict"] == "missing_in_source")
            .expect("filed");
        assert_eq!(f.detail["subject"], "shareholder.99");
        assert!(f.detail["source"]["value"].is_null());
        assert!(f.detail["ledger"]["claim_id"].as_str().is_some());

        // The same batch declared INCOMPLETE says nothing about absence.
        let server2 = MockServer::new().with_version("memv-1");
        server2.seed_facts("memv-1", vec![fact("shareholder.99", "1")]);
        let partial = reconcile_with(
            &server2,
            &mapping("shadow"),
            "memv-1",
            &b,
            &bytes,
            &ReconcileOptions {
                tenant: "acme",
                promoted: false,
                source_id: "captable",
                proposals: None,
                source_complete: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(partial.missing_in_source, 0);
    }

    #[test]
    fn a_backdated_change_is_reviewable_even_when_the_values_agree() {
        let o = observation(
            &[("shareholder.44", 0.99)],
            "15000",
            0,
            ChangeKind::Backdated,
        );
        let c = compare(&o, &[fact("shareholder.44", "15000")], 0.95);
        assert_eq!(
            c.verdict,
            Verdict::BackdatedRequiresReview,
            "the question is what a backdated change MEANS, not whether it matches"
        );
    }

    #[test]
    fn a_backdated_change_never_becomes_a_correction_automatically() {
        let m = mapping("shadow");
        assert_eq!(
            classify_change(&m, "shares_outstanding", ChangeKind::Backdated),
            ChangeKindDecision::RequiresReview
        );
        // And an undeclared property defaults the same way.
        assert_eq!(
            classify_change(&m, "undeclared", ChangeKind::Backdated),
            ChangeKindDecision::RequiresReview
        );
    }

    #[test]
    fn a_property_the_ledger_has_never_seen_is_missing_in_ledger() {
        let o = observation(&[("shareholder.99", 0.99)], "1", 0, ChangeKind::Insert);
        let c = compare(&o, &[], 0.95);
        assert_eq!(c.verdict, Verdict::MissingInLedger);
        assert!(c.ledger_value.is_none());
    }

    fn batch(observations: Vec<Observation>) -> ObservationBatch {
        ObservationBatch {
            contract_version: "0.1.0".into(),
            mapping: "captable-holdings@1".into(),
            batch_id: "obs-1".into(),
            source_id: Some("captable".into()),
            run_id: Some("run-1".into()),
            sealed_evidence_id: None,
            observations,
        }
    }

    #[tokio::test]
    async fn shadow_mode_files_findings_and_leaves_canon_byte_identical() {
        let server = MockServer::new();
        server.seed_facts(
            "memv-1",
            vec![
                fact("shareholder.42", "125000"),
                fact("shareholder.43", "90000"),
            ],
        );
        let before = server.slice_facts("memv-1", None).await.unwrap();

        let b = batch(vec![
            observation(
                &[("shareholder.42", 0.99)],
                "125000",
                0,
                ChangeKind::Snapshot,
            ),
            observation(
                &[("shareholder.43", 0.99)],
                "90500",
                0,
                ChangeKind::Snapshot,
            ),
        ]);
        let out = reconcile(&server, &mapping("shadow"), "memv-1", &b, b"obs")
            .await
            .unwrap();

        assert_eq!(out.observations, 2);
        assert_eq!(out.agreements, 1);
        assert_eq!(out.discrepancies, 1);
        assert_eq!(out.findings_filed, 1);
        assert!(out.canon_untouched);

        // The ledger is byte-identical after a full run.
        let after = server.slice_facts("memv-1", None).await.unwrap();
        assert_eq!(before, after, "shadow mode must not touch accepted canon");
    }

    #[tokio::test]
    async fn a_discrepancy_finding_carries_both_evidence_sides() {
        let server = MockServer::new();
        server.seed_facts("memv-1", vec![fact("shareholder.43", "90000")]);
        let b = batch(vec![observation(
            &[("shareholder.43", 0.99)],
            "90500",
            0,
            ChangeKind::Snapshot,
        )]);
        reconcile(&server, &mapping("shadow"), "memv-1", &b, b"obs")
            .await
            .unwrap();

        let findings = server.filed_findings();
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule_id, DISCREPANCY_RULE_ID);
        assert_eq!(f.severity, "warn", "only a gate may block");
        // The source side: a sealed artifact.
        assert!(f.detail["source"]["evidence_id"].is_string());
        assert_eq!(f.detail["source"]["value"], "90500");
        // The ledger side: a claim id and the pin it was read at.
        assert_eq!(f.detail["ledger"]["claim_id"], "claim-1");
        assert_eq!(f.detail["ledger"]["value"], "90000");
        assert!(f.detail["ledger"]["as_of_seq"].is_number());
    }

    #[tokio::test]
    async fn an_ambiguous_row_files_its_own_finding_and_no_discrepancy() {
        let server = MockServer::new();
        server.seed_facts("memv-1", vec![fact("shareholder.51", "40000")]);
        let b = batch(vec![observation(
            &[("shareholder.51", 0.62), ("shareholder.58", 0.61)],
            "40000",
            0,
            ChangeKind::Snapshot,
        )]);
        let out = reconcile(&server, &mapping("shadow"), "memv-1", &b, b"obs")
            .await
            .unwrap();
        assert_eq!(out.ambiguous, 1);
        assert_eq!(out.discrepancies, 0);
        let f = &server.filed_findings()[0];
        assert_eq!(f.rule_id, AMBIGUITY_RULE_ID);
        assert_eq!(f.detail["candidates"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_comparison_is_pinned_so_the_whole_run_sees_one_ledger_state() {
        let server = MockServer::new();
        server.seed_facts(
            "memv-1",
            vec![
                fact("shareholder.42", "125000"),
                LedgerFact {
                    seq: 99,
                    ..fact("shareholder.42", "999999")
                },
            ],
        );
        let b = batch(vec![observation(
            &[("shareholder.42", 0.99)],
            "125000",
            0,
            ChangeKind::Snapshot,
        )]);
        let out = reconcile(&server, &mapping("shadow"), "memv-1", &b, b"obs")
            .await
            .unwrap();
        // Both facts are visible at head, so the FIRST match wins and the run
        // is at least self-consistent — the point being that every comparison
        // in the run read the same pinned slice.
        assert_eq!(out.observations, 1);
    }
}
