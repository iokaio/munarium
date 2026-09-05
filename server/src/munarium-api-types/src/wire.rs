// SPDX-License-Identifier: Apache-2.0
//! The ONE pb <-> DTO boundary mapping, shared by the server's tonic impls
//! and every Rust gRPC client. Before this module both sides hand-wrote
//! inverse copies of the same conversions, so a sentinel-convention change
//! on one side silently decoded differently on the other.
//!
//! Wire conventions encoded here (proto3 has no field presence for scalars):
//! - empty string = absent (`scope_path`, `entity_id`, `shape_ref`, …);
//! - `confidence == 0.0` = absent — an explicitly-zero confidence cannot ride
//!   this wire, so senders must reject it rather than silently drop it;
//! - `fulfilled_seq == 0` / `budget == 0` = absent;
//! - unknown enum values decode to the proto's `_UNSPECIFIED` default.
//!
//! Enabled by the `proto` feature (server + Rust client turn it on; the
//! REST-only consumer keeps api-types tonic-free).

use crate::{
    AnchorDto, ClaimDto, ClaimOriginDto, ClaimStatusDto, ClaimTypeDto, ComposedContextDto,
    CounterDto, DigestDto, GateFindingDto, PromiseDto, ProposeClaimRequest, ProvenanceDto,
    ProvenanceEnvelopeDto, SearchHitDto, SectionDto, SeverityDto,
};
use munarium_proto::mmp::v1 as pb;

// ---------------------------------------------------------------------------
// scalar sentinels
// ---------------------------------------------------------------------------

/// Empty string = absent on the proto3 wire.
pub fn none_if_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn json_opt(s: &str) -> Option<serde_json::Value> {
    if s.is_empty() {
        None
    } else {
        serde_json::from_str(s).ok()
    }
}

fn json_str(v: &Option<serde_json::Value>) -> String {
    v.as_ref().map(|v| v.to_string()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// enums
// ---------------------------------------------------------------------------

impl From<ClaimTypeDto> for pb::ClaimType {
    fn from(v: ClaimTypeDto) -> Self {
        match v {
            ClaimTypeDto::Fact => pb::ClaimType::Fact,
            ClaimTypeDto::Update => pb::ClaimType::Update,
            ClaimTypeDto::Correction => pb::ClaimType::Correction,
        }
    }
}

impl From<pb::ClaimType> for ClaimTypeDto {
    fn from(v: pb::ClaimType) -> Self {
        match v {
            pb::ClaimType::Update => ClaimTypeDto::Update,
            pb::ClaimType::Correction => ClaimTypeDto::Correction,
            _ => ClaimTypeDto::Fact,
        }
    }
}

impl From<ClaimStatusDto> for pb::ClaimStatus {
    fn from(v: ClaimStatusDto) -> Self {
        match v {
            ClaimStatusDto::Accepted => pb::ClaimStatus::Accepted,
            ClaimStatusDto::Disputed => pb::ClaimStatus::Disputed,
        }
    }
}

impl From<pb::ClaimStatus> for ClaimStatusDto {
    /// Decodes fail-CLOSED: only an explicit ACCEPTED reads as accepted.
    /// UNSPECIFIED (the proto3 default) and any status a newer server adds
    /// decode as `Disputed`, so invariant #1 ("accepted means the gates
    /// passed") can never be satisfied by a tag this build cannot name.
    fn from(v: pb::ClaimStatus) -> Self {
        match v {
            pb::ClaimStatus::Accepted => ClaimStatusDto::Accepted,
            _ => ClaimStatusDto::Disputed,
        }
    }
}

impl From<ProvenanceDto> for pb::Provenance {
    fn from(v: ProvenanceDto) -> Self {
        match v {
            ProvenanceDto::Witnessed => pb::Provenance::Witnessed,
            ProvenanceDto::Backfilled => pb::Provenance::Backfilled,
            ProvenanceDto::Repaired => pb::Provenance::Repaired,
            ProvenanceDto::Emergent => pb::Provenance::Emergent,
            ProvenanceDto::CoverageRepair => pb::Provenance::CoverageRepair,
        }
    }
}

impl From<pb::Provenance> for ProvenanceDto {
    /// Fail-CLOSED: `Witnessed` is the strongest provenance a fact can
    /// carry, so only an explicit WITNESSED decodes to it. UNSPECIFIED and
    /// unknown tags read as `Emergent` — the weakest — so a consumer
    /// filtering on provenance never over-trusts an undecodable tag.
    fn from(v: pb::Provenance) -> Self {
        match v {
            pb::Provenance::Witnessed => ProvenanceDto::Witnessed,
            pb::Provenance::Backfilled => ProvenanceDto::Backfilled,
            pb::Provenance::Repaired => ProvenanceDto::Repaired,
            pb::Provenance::CoverageRepair => ProvenanceDto::CoverageRepair,
            _ => ProvenanceDto::Emergent,
        }
    }
}

impl From<SeverityDto> for pb::Severity {
    fn from(v: SeverityDto) -> Self {
        match v {
            SeverityDto::Info => pb::Severity::Info,
            SeverityDto::Warn => pb::Severity::Warn,
            SeverityDto::Block => pb::Severity::Block,
        }
    }
}

impl From<pb::Severity> for SeverityDto {
    /// Fail-CLOSED like `ClaimStatus`: an unknown severity is the most
    /// severe one, never silently downgraded to `Info`.
    fn from(v: pb::Severity) -> Self {
        match v {
            pb::Severity::Info => SeverityDto::Info,
            pb::Severity::Warn => SeverityDto::Warn,
            _ => SeverityDto::Block,
        }
    }
}

/// Decode a raw enum tag (prost stores enums as i32).
fn claim_type_of(v: i32) -> ClaimTypeDto {
    pb::ClaimType::try_from(v)
        .unwrap_or(pb::ClaimType::Fact)
        .into()
}

fn status_of(v: i32) -> ClaimStatusDto {
    // An undecodable tag is not accepted — see the From impl above.
    pb::ClaimStatus::try_from(v)
        .map(Into::into)
        .unwrap_or(ClaimStatusDto::Disputed)
}

fn provenance_of(v: i32) -> ProvenanceDto {
    pb::Provenance::try_from(v)
        .map(Into::into)
        .unwrap_or(ProvenanceDto::Emergent)
}

fn severity_of(v: i32) -> SeverityDto {
    pb::Severity::try_from(v)
        .map(Into::into)
        .unwrap_or(SeverityDto::Block)
}

// ---------------------------------------------------------------------------
// messages
// ---------------------------------------------------------------------------

impl From<pb::Claim> for ClaimDto {
    fn from(c: pb::Claim) -> Self {
        Self {
            id: c.id,
            version_id: c.version_id,
            seq: c.seq,
            claim_type: claim_type_of(c.claim_type),
            subject: c.subject,
            key: c.key,
            value: c.value,
            normalized_text: c.normalized_text,
            scope_path: none_if_empty(&c.scope_path),
            status: status_of(c.status),
            provenance: provenance_of(c.provenance),
            supersedes_id: none_if_empty(&c.supersedes_id),
            entity_id: none_if_empty(&c.entity_id),
            evidence: json_opt(&c.evidence_json),
            confidence: (c.confidence != 0.0).then_some(c.confidence),
            shape_ref: none_if_empty(&c.shape_ref),
            origin: c.origin.map(Into::into),
        }
    }
}

impl From<pb::ClaimOrigin> for ClaimOriginDto {
    fn from(o: pb::ClaimOrigin) -> Self {
        Self {
            kind: o.kind,
            source_id: o.source_id,
            mapping_version: o.mapping_version,
            row_key: o.row_key,
            event_position: none_if_empty(&o.event_position),
            observed_at: none_if_empty(&o.observed_at),
            evidence_id: none_if_empty(&o.evidence_id),
        }
    }
}

impl From<ClaimOriginDto> for pb::ClaimOrigin {
    fn from(o: ClaimOriginDto) -> Self {
        Self {
            kind: o.kind,
            source_id: o.source_id,
            mapping_version: o.mapping_version,
            row_key: o.row_key,
            event_position: o.event_position.unwrap_or_default(),
            observed_at: o.observed_at.unwrap_or_default(),
            evidence_id: o.evidence_id.unwrap_or_default(),
        }
    }
}

impl From<ClaimDto> for pb::Claim {
    fn from(c: ClaimDto) -> Self {
        Self {
            id: c.id,
            version_id: c.version_id,
            seq: c.seq,
            claim_type: pb::ClaimType::from(c.claim_type) as i32,
            subject: c.subject,
            key: c.key,
            value: c.value,
            normalized_text: c.normalized_text,
            scope_path: c.scope_path.unwrap_or_default(),
            status: pb::ClaimStatus::from(c.status) as i32,
            provenance: pb::Provenance::from(c.provenance) as i32,
            supersedes_id: c.supersedes_id.unwrap_or_default(),
            entity_id: c.entity_id.unwrap_or_default(),
            evidence_json: json_str(&c.evidence),
            confidence: c.confidence.unwrap_or(0.0),
            shape_ref: c.shape_ref.unwrap_or_default(),
            recorded_at: None,
            origin: c.origin.map(Into::into),
        }
    }
}

impl From<pb::GateFinding> for GateFindingDto {
    fn from(f: pb::GateFinding) -> Self {
        Self {
            rule_id: f.rule_id,
            severity: severity_of(f.severity),
            message: f.message,
            scope_path: none_if_empty(&f.scope_path),
            detail: json_opt(&f.detail_json),
        }
    }
}

impl From<GateFindingDto> for pb::GateFinding {
    fn from(f: GateFindingDto) -> Self {
        Self {
            rule_id: f.rule_id,
            severity: pb::Severity::from(f.severity) as i32,
            message: f.message,
            scope_path: f.scope_path.unwrap_or_default(),
            detail_json: json_str(&f.detail),
        }
    }
}

impl From<pb::Anchor> for AnchorDto {
    fn from(a: pb::Anchor) -> Self {
        Self {
            id: a.id,
            version_id: a.version_id,
            detail_key: a.detail_key,
            locked_value: a.locked_value,
            locked_at_scope: none_if_empty(&a.locked_at_scope),
            status: a.status,
            seq: a.seq,
        }
    }
}

impl From<AnchorDto> for pb::Anchor {
    fn from(a: AnchorDto) -> Self {
        Self {
            id: a.id,
            version_id: a.version_id,
            detail_key: a.detail_key,
            locked_value: a.locked_value,
            locked_at_scope: a.locked_at_scope.unwrap_or_default(),
            status: a.status,
            seq: a.seq,
        }
    }
}

impl From<pb::Promise> for PromiseDto {
    fn from(p: pb::Promise) -> Self {
        Self {
            id: p.id,
            version_id: p.version_id,
            key: p.key,
            kind: p.kind,
            description: p.description,
            origin_scope: none_if_empty(&p.origin_scope),
            due_scope: none_if_empty(&p.due_scope),
            status: p.status,
            seq: p.seq,
            fulfilled_seq: (p.fulfilled_seq > 0).then_some(p.fulfilled_seq),
        }
    }
}

impl From<PromiseDto> for pb::Promise {
    fn from(p: PromiseDto) -> Self {
        Self {
            id: p.id,
            version_id: p.version_id,
            key: p.key,
            kind: p.kind,
            description: p.description,
            origin_scope: p.origin_scope.unwrap_or_default(),
            due_scope: p.due_scope.unwrap_or_default(),
            status: p.status,
            seq: p.seq,
            fulfilled_seq: p.fulfilled_seq.unwrap_or(0),
        }
    }
}

impl From<pb::CounterState> for CounterDto {
    fn from(c: pb::CounterState) -> Self {
        Self {
            key: c.key,
            total: c.total,
            budget: (c.budget > 0).then_some(c.budget),
        }
    }
}

impl From<CounterDto> for pb::CounterState {
    fn from(c: CounterDto) -> Self {
        Self {
            key: c.key,
            total: c.total,
            budget: c.budget.unwrap_or(0),
        }
    }
}

impl From<pb::Digest> for DigestDto {
    fn from(d: pb::Digest) -> Self {
        Self {
            version_id: d.version_id,
            tier: d.tier as u8,
            scope_path: d.scope_path,
            content: d.content,
            content_hash: d.content_hash,
            built_from_seq: d.built_from_seq,
        }
    }
}

impl From<DigestDto> for pb::Digest {
    fn from(d: DigestDto) -> Self {
        Self {
            version_id: d.version_id,
            tier: d.tier as u32,
            scope_path: d.scope_path,
            content: d.content,
            content_hash: d.content_hash,
            built_from_seq: d.built_from_seq,
        }
    }
}

impl From<pb::ComposedContext> for ComposedContextDto {
    fn from(c: pb::ComposedContext) -> Self {
        Self {
            sections: c
                .sections
                .into_iter()
                .map(|s| SectionDto {
                    title: s.title,
                    body: s.body,
                })
                .collect(),
            text: c.text,
            estimated_tokens: c.estimated_tokens,
            content_hash: c.content_hash,
            as_of_seq: c.as_of_seq,
        }
    }
}

impl From<ComposedContextDto> for pb::ComposedContext {
    fn from(c: ComposedContextDto) -> Self {
        Self {
            sections: c
                .sections
                .into_iter()
                .map(|s| pb::composed_context::Section {
                    title: s.title,
                    body: s.body,
                })
                .collect(),
            text: c.text,
            estimated_tokens: c.estimated_tokens,
            content_hash: c.content_hash,
            as_of_seq: c.as_of_seq,
        }
    }
}

impl From<pb::SearchHit> for SearchHitDto {
    fn from(h: pb::SearchHit) -> Self {
        Self {
            chunk_id: h.chunk_id,
            source_id: h.source_id,
            source_path: h.source_path,
            source_content_hash: h.source_content_hash,
            text: h.text,
            score: h.score,
            // ranks are 1-based; the wire uses 0 for absent
            lexical_rank: (h.lexical_rank > 0.0).then_some(h.lexical_rank as u32),
            vector_rank: (h.vector_rank > 0.0).then_some(h.vector_rank as u32),
            // raw leg scores are not carried on the proto plane (additive
            // candidates for a future proto rev)
            lexical_score: None,
            vector_distance: None,
            metadata: json_opt(&h.metadata_json),
        }
    }
}

impl From<pb::ProvenanceEnvelope> for ProvenanceEnvelopeDto {
    fn from(e: pb::ProvenanceEnvelope) -> Self {
        Self {
            chunk_ids: e.chunk_ids,
            source_ids: e.source_ids,
            source_paths: e.source_paths,
            source_content_hashes: e.source_content_hashes,
            index_version: e.index_version,
            event_watermark: e.event_watermark,
            provider_fingerprint: none_if_empty(&e.provider_fingerprint),
        }
    }
}

impl From<ProvenanceEnvelopeDto> for pb::ProvenanceEnvelope {
    fn from(e: ProvenanceEnvelopeDto) -> Self {
        Self {
            chunk_ids: e.chunk_ids,
            source_ids: e.source_ids,
            source_paths: e.source_paths,
            source_content_hashes: e.source_content_hashes,
            index_version: e.index_version,
            event_watermark: e.event_watermark,
            provider_fingerprint: e.provider_fingerprint.unwrap_or_default(),
            generated_at: None,
        }
    }
}

impl From<pb::ProposeClaimRequest> for ProposeClaimRequest {
    fn from(p: pb::ProposeClaimRequest) -> Self {
        Self {
            expected_head: p.expected_head,
            claim_type: claim_type_of(p.claim_type),
            subject: p.subject,
            key: p.key,
            value: p.value,
            scope_path: none_if_empty(&p.scope_path),
            // UNSPECIFIED (0) means "server default", not Witnessed.
            provenance: (p.provenance != pb::Provenance::Unspecified as i32)
                .then(|| provenance_of(p.provenance)),
            supersedes_id: none_if_empty(&p.supersedes_id),
            entity_id: none_if_empty(&p.entity_id),
            evidence: json_opt(&p.evidence_json),
            confidence: (p.confidence != 0.0).then_some(p.confidence),
            shape_ref: none_if_empty(&p.shape_ref),
            origin: p.origin.map(Into::into),
        }
    }
}

/// DTO -> pb needs the lineage id, which the DTO does not carry.
pub fn propose_request_pb(version_id: &str, r: ProposeClaimRequest) -> pb::ProposeClaimRequest {
    pb::ProposeClaimRequest {
        version_id: version_id.to_string(),
        expected_head: r.expected_head,
        claim_type: pb::ClaimType::from(r.claim_type) as i32,
        subject: r.subject,
        key: r.key,
        value: r.value,
        scope_path: r.scope_path.unwrap_or_default(),
        provenance: r
            .provenance
            .map(|p| pb::Provenance::from(p) as i32)
            .unwrap_or(pb::Provenance::Unspecified as i32),
        supersedes_id: r.supersedes_id.unwrap_or_default(),
        entity_id: r.entity_id.unwrap_or_default(),
        evidence_json: json_str(&r.evidence),
        confidence: r.confidence.unwrap_or(0.0),
        shape_ref: r.shape_ref.unwrap_or_default(),
        origin: r.origin.map(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_round_trips_through_pb() {
        let dto = ClaimDto {
            id: "c-1".into(),
            version_id: "v-1".into(),
            seq: 7,
            claim_type: ClaimTypeDto::Correction,
            subject: "hero".into(),
            key: "eyes".into(),
            value: "blue".into(),
            normalized_text: "hero.eyes=blue".into(),
            scope_path: Some("ch2".into()),
            status: ClaimStatusDto::Disputed,
            provenance: ProvenanceDto::Backfilled,
            supersedes_id: Some("c-0".into()),
            entity_id: None,
            evidence: Some(serde_json::json!({"quote": "green eyes"})),
            confidence: Some(0.75),
            shape_ref: Some("tickets@1".into()),
            origin: None,
        };
        let back: ClaimDto = pb::Claim::from(dto.clone()).into();
        assert_eq!(back.id, dto.id);
        assert_eq!(back.claim_type, dto.claim_type);
        assert_eq!(back.status, dto.status);
        assert_eq!(back.provenance, dto.provenance);
        assert_eq!(back.scope_path, dto.scope_path);
        assert_eq!(back.entity_id, None);
        assert_eq!(back.evidence, dto.evidence);
        assert_eq!(back.confidence, dto.confidence);
    }

    #[test]
    fn absent_scalars_use_the_documented_sentinels() {
        let sparse = pb::Claim {
            id: "c".into(),
            version_id: "v".into(),
            ..Default::default()
        };
        let dto: ClaimDto = sparse.into();
        assert_eq!(dto.scope_path, None);
        assert_eq!(dto.confidence, None, "0.0 confidence decodes as absent");
        assert_eq!(dto.claim_type, ClaimTypeDto::Fact);
        // Governance enums decode fail-CLOSED: an UNSPECIFIED tag must never
        // read as the permissive value, or a caller could believe a claim
        // this build cannot decode was accepted by the gates.
        assert_eq!(dto.status, ClaimStatusDto::Disputed);
        assert_eq!(dto.provenance, ProvenanceDto::Emergent);

        let counter: CounterDto = pb::CounterState {
            key: "k".into(),
            total: 3,
            budget: 0,
        }
        .into();
        assert_eq!(counter.budget, None, "0 budget decodes as absent");
    }

    #[test]
    fn unspecified_provenance_means_server_default() {
        let req: ProposeClaimRequest = pb::ProposeClaimRequest {
            subject: "hero".into(),
            key: "eyes".into(),
            value: "green".into(),
            ..Default::default()
        }
        .into();
        assert_eq!(req.provenance, None, "UNSPECIFIED is not Witnessed");
        let back = propose_request_pb("v-1", req);
        assert_eq!(back.provenance, pb::Provenance::Unspecified as i32);
    }

    #[test]
    fn finding_and_promise_round_trip() {
        let f = GateFindingDto {
            rule_id: "gate.ledger-conflict".into(),
            severity: SeverityDto::Block,
            message: "boom".into(),
            scope_path: None,
            detail: Some(serde_json::json!({"claim_key": "hero.eyes"})),
        };
        let back: GateFindingDto = pb::GateFinding::from(f.clone()).into();
        assert_eq!(back.rule_id, f.rule_id);
        assert_eq!(back.severity, f.severity);
        assert_eq!(back.detail, f.detail);

        let p = PromiseDto {
            id: "p".into(),
            version_id: "v".into(),
            key: "reveal".into(),
            kind: "setup".into(),
            description: "open the letter".into(),
            origin_scope: Some("ch1".into()),
            due_scope: None,
            status: "open".into(),
            seq: 2,
            fulfilled_seq: None,
        };
        let back: PromiseDto = pb::Promise::from(p.clone()).into();
        assert_eq!(back.key, p.key);
        assert_eq!(back.due_scope, None);
        assert_eq!(back.fulfilled_seq, None);
    }
}
