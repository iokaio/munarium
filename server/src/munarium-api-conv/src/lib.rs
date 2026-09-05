// SPDX-License-Identifier: Apache-2.0
//! The `munarium-core` <-> `munarium-api-types` conversions.
//!
//! These were once `From` impls inside the DTO crate, which made
//! that crate depend on `munarium-core` — and through it every Rust client that
//! re-exports the DTOs linked the server's domain core. They live here so
//! `munarium-api-types` depends on serde, utoipa and (optionally) the proto
//! crate alone, and can ship in the public contract bundle.
//!
//! Why a trait and not `From`: the orphan rule forbids a third crate from
//! implementing `From<core::Claim> for ClaimDto` — neither type is local. So
//! this crate owns [`Convert`], which is `Into` in everything but name: one
//! generic trait, implemented once per (core, wire) pair in each direction, and
//! resolved by the target type exactly as `Into` was. Call sites say
//! `claim.convert()`, `dtos.into_iter().map(convert)`, or `convert(dto)`.
//!
//! The wire-string vocabularies ("fulfilled"/"released"/…) are still parsed in
//! exactly ONE place: here. Conformance adapters used to hand-roll these and
//! could disagree about what a new status string meant.

use munarium_api_types::*;
use munarium_core::types as core;

/// The core <-> wire conversion. `Into` by another name, owned here because the
/// orphan rule keeps a `From` between two foreign types out of a third crate.
pub trait Convert<T> {
    fn convert(self) -> T;
}

/// Free-function form for iterators and `Option::map`: `.map(convert)`.
pub fn convert<S: Convert<T>, T>(s: S) -> T {
    s.convert()
}

impl Convert<core::ClaimType> for ClaimTypeDto {
    fn convert(self) -> core::ClaimType {
        let v = self;
        match v {
            ClaimTypeDto::Fact => core::ClaimType::Fact,
            ClaimTypeDto::Update => core::ClaimType::Update,
            ClaimTypeDto::Correction => core::ClaimType::Correction,
        }
    }
}

impl Convert<ClaimTypeDto> for core::ClaimType {
    fn convert(self) -> ClaimTypeDto {
        let v = self;
        match v {
            core::ClaimType::Fact => ClaimTypeDto::Fact,
            core::ClaimType::Update => ClaimTypeDto::Update,
            core::ClaimType::Correction => ClaimTypeDto::Correction,
        }
    }
}

impl Convert<core::ClaimStatus> for ClaimStatusDto {
    fn convert(self) -> core::ClaimStatus {
        let v = self;
        match v {
            ClaimStatusDto::Accepted => core::ClaimStatus::Accepted,
            ClaimStatusDto::Disputed => core::ClaimStatus::Disputed,
        }
    }
}

impl Convert<ClaimStatusDto> for core::ClaimStatus {
    fn convert(self) -> ClaimStatusDto {
        let v = self;
        match v {
            core::ClaimStatus::Accepted => ClaimStatusDto::Accepted,
            core::ClaimStatus::Disputed => ClaimStatusDto::Disputed,
        }
    }
}

impl Convert<core::Provenance> for ProvenanceDto {
    fn convert(self) -> core::Provenance {
        let v = self;
        match v {
            ProvenanceDto::Witnessed => core::Provenance::Witnessed,
            ProvenanceDto::Backfilled => core::Provenance::Backfilled,
            ProvenanceDto::Repaired => core::Provenance::Repaired,
            ProvenanceDto::Emergent => core::Provenance::Emergent,
            ProvenanceDto::CoverageRepair => core::Provenance::CoverageRepair,
        }
    }
}

impl Convert<ProvenanceDto> for core::Provenance {
    fn convert(self) -> ProvenanceDto {
        let v = self;
        match v {
            core::Provenance::Witnessed => ProvenanceDto::Witnessed,
            core::Provenance::Backfilled => ProvenanceDto::Backfilled,
            core::Provenance::Repaired => ProvenanceDto::Repaired,
            core::Provenance::Emergent => ProvenanceDto::Emergent,
            core::Provenance::CoverageRepair => ProvenanceDto::CoverageRepair,
        }
    }
}

impl Convert<SeverityDto> for core::Severity {
    fn convert(self) -> SeverityDto {
        let v = self;
        match v {
            core::Severity::Info => SeverityDto::Info,
            core::Severity::Warn => SeverityDto::Warn,
            core::Severity::Block => SeverityDto::Block,
        }
    }
}

impl Convert<GateFindingDto> for core::GateFinding {
    fn convert(self) -> GateFindingDto {
        let f = self;
        GateFindingDto {
            rule_id: f.rule_id,
            severity: f.severity.convert(),
            message: f.message,
            scope_path: f.scope_path,
            detail: f.detail,
        }
    }
}

impl Convert<GateFindingDto> for &core::GateFinding {
    fn convert(self) -> GateFindingDto {
        let f = self;
        GateFindingDto {
            rule_id: f.rule_id.clone(),
            severity: f.severity.convert(),
            message: f.message.clone(),
            scope_path: f.scope_path.clone(),
            detail: f.detail.clone(),
        }
    }
}

impl Convert<ClaimOriginDto> for core::ClaimOrigin {
    fn convert(self) -> ClaimOriginDto {
        let o = self;
        ClaimOriginDto {
            kind: o.kind,
            source_id: o.source_id,
            mapping_version: o.mapping_version,
            row_key: o.row_key,
            event_position: o.event_position,
            observed_at: o.observed_at,
            evidence_id: o.evidence_id,
        }
    }
}

impl Convert<core::ClaimOrigin> for ClaimOriginDto {
    fn convert(self) -> core::ClaimOrigin {
        let o = self;
        core::ClaimOrigin {
            kind: o.kind,
            source_id: o.source_id,
            mapping_version: o.mapping_version,
            row_key: o.row_key,
            event_position: o.event_position,
            observed_at: o.observed_at,
            evidence_id: o.evidence_id,
        }
    }
}

impl Convert<ClaimDto> for core::Claim {
    fn convert(self) -> ClaimDto {
        let c = self;
        let normalized_text = c.normalized_text();
        ClaimDto {
            id: c.id,
            version_id: c.version_id,
            seq: c.seq,
            claim_type: c.claim_type.convert(),
            subject: c.subject,
            key: c.key,
            value: c.value,
            normalized_text,
            scope_path: c.scope_path,
            status: c.status.convert(),
            provenance: c.provenance.convert(),
            supersedes_id: c.supersedes_id,
            entity_id: c.entity_id,
            evidence: c.evidence,
            confidence: c.confidence,
            shape_ref: c.shape_ref,
            origin: c.origin.map(convert),
        }
    }
}

impl Convert<AnchorDto> for core::Anchor {
    fn convert(self) -> AnchorDto {
        let a = self;
        AnchorDto {
            id: a.id,
            version_id: a.version_id,
            detail_key: a.detail_key,
            locked_value: a.locked_value,
            locked_at_scope: a.locked_at_scope,
            status: match a.status {
                core::AnchorStatus::Locked => "locked".into(),
                core::AnchorStatus::Released => "released".into(),
            },
            seq: a.seq,
        }
    }
}

impl Convert<PromiseDto> for core::Promise {
    fn convert(self) -> PromiseDto {
        let p = self;
        PromiseDto {
            id: p.id,
            version_id: p.version_id,
            key: p.key,
            kind: p.kind,
            description: p.description,
            origin_scope: p.origin_scope,
            due_scope: p.due_scope,
            status: match p.status {
                core::PromiseStatus::Open => "open".into(),
                core::PromiseStatus::Fulfilled => "fulfilled".into(),
                core::PromiseStatus::Expired => "expired".into(),
                core::PromiseStatus::Violated => "violated".into(),
            },
            seq: p.seq,
            fulfilled_seq: p.fulfilled_seq,
        }
    }
}

impl Convert<CounterDto> for core::CounterTotal {
    fn convert(self) -> CounterDto {
        let c = self;
        CounterDto {
            key: c.key,
            total: c.total,
            budget: c.budget,
        }
    }
}

impl Convert<DigestDto> for core::Digest {
    fn convert(self) -> DigestDto {
        let d = self;
        DigestDto {
            version_id: d.version_id,
            tier: d.tier,
            scope_path: d.scope_path,
            content: d.content,
            content_hash: d.content_hash,
            built_from_seq: d.built_from_seq,
        }
    }
}

impl Convert<core::Digest> for DigestDto {
    fn convert(self) -> core::Digest {
        let d = self;
        core::Digest {
            version_id: d.version_id,
            tier: d.tier,
            scope_path: d.scope_path,
            content: d.content,
            content_hash: d.content_hash,
            built_from_seq: d.built_from_seq,
        }
    }
}

impl Convert<IndexStatusResponse> for munarium_core::retrieval::IndexVersion {
    fn convert(self) -> IndexStatusResponse {
        let iv = self;
        let munarium_core::retrieval::IndexVersion {
            id,
            shape_ref,
            manifest,
            event_watermark,
            active,
        } = iv;
        IndexStatusResponse {
            index_version: id,
            shape_ref,
            event_watermark,
            active,
            manifest,
        }
    }
}

impl Convert<SearchHitDto> for munarium_core::retrieval::SearchHit {
    fn convert(self) -> SearchHitDto {
        let h = self;
        // Exhaustive destructuring: a new core field is a compile error here,
        // not a silently dropped wire member.
        let munarium_core::retrieval::SearchHit {
            chunk_id,
            source_id,
            source_path,
            source_content_hash,
            text,
            score,
            lexical_rank,
            vector_rank,
            lexical_score,
            vector_distance,
            metadata,
        } = h;
        SearchHitDto {
            chunk_id,
            source_id,
            source_path,
            source_content_hash,
            text,
            score,
            lexical_rank,
            vector_rank,
            lexical_score,
            vector_distance,
            metadata,
        }
    }
}

impl Convert<ProvenanceEnvelopeDto> for munarium_core::retrieval::ProvenanceEnvelope {
    fn convert(self) -> ProvenanceEnvelopeDto {
        let e = self;
        let munarium_core::retrieval::ProvenanceEnvelope {
            chunk_ids,
            source_ids,
            source_paths,
            source_content_hashes,
            index_version,
            event_watermark,
            provider_fingerprint,
        } = e;
        ProvenanceEnvelopeDto {
            chunk_ids,
            source_ids,
            source_paths,
            source_content_hashes,
            index_version,
            event_watermark,
            provider_fingerprint,
        }
    }
}

impl Convert<SearchResponse> for munarium_core::retrieval::SearchResult {
    fn convert(self) -> SearchResponse {
        let r = self;
        SearchResponse {
            hits: r.hits.into_iter().map(convert).collect(),
            envelope: r.envelope.convert(),
        }
    }
}

impl Convert<core::Claim> for ClaimDto {
    fn convert(self) -> core::Claim {
        let c = self;
        core::Claim {
            id: c.id,
            version_id: c.version_id,
            seq: c.seq,
            claim_type: c.claim_type.convert(),
            subject: c.subject,
            key: c.key,
            value: c.value,
            scope_path: c.scope_path,
            status: c.status.convert(),
            provenance: c.provenance.convert(),
            supersedes_id: c.supersedes_id,
            entity_id: c.entity_id,
            evidence: c.evidence,
            confidence: c.confidence,
            shape_ref: c.shape_ref,
            origin: c.origin.map(convert),
        }
    }
}

impl Convert<core::Anchor> for AnchorDto {
    fn convert(self) -> core::Anchor {
        let a = self;
        core::Anchor {
            id: a.id,
            version_id: a.version_id,
            detail_key: a.detail_key,
            locked_value: a.locked_value,
            locked_at_scope: a.locked_at_scope,
            status: match a.status.as_str() {
                "released" => core::AnchorStatus::Released,
                _ => core::AnchorStatus::Locked,
            },
            seq: a.seq,
            // Anchor evidence is not carried on either wire today.
            evidence: None,
        }
    }
}

impl Convert<core::Promise> for PromiseDto {
    fn convert(self) -> core::Promise {
        let p = self;
        core::Promise {
            id: p.id,
            version_id: p.version_id,
            key: p.key,
            kind: p.kind,
            description: p.description,
            origin_scope: p.origin_scope,
            due_scope: p.due_scope,
            status: match p.status.as_str() {
                "fulfilled" => core::PromiseStatus::Fulfilled,
                "expired" => core::PromiseStatus::Expired,
                "violated" => core::PromiseStatus::Violated,
                _ => core::PromiseStatus::Open,
            },
            seq: p.seq,
            fulfilled_seq: p.fulfilled_seq,
        }
    }
}

impl Convert<core::CounterTotal> for CounterDto {
    fn convert(self) -> core::CounterTotal {
        let c = self;
        core::CounterTotal {
            key: c.key,
            total: c.total,
            budget: c.budget,
        }
    }
}

impl Convert<core::Severity> for SeverityDto {
    fn convert(self) -> core::Severity {
        let s = self;
        match s {
            SeverityDto::Info => core::Severity::Info,
            SeverityDto::Warn => core::Severity::Warn,
            SeverityDto::Block => core::Severity::Block,
        }
    }
}

impl Convert<core::GateFinding> for GateFindingDto {
    fn convert(self) -> core::GateFinding {
        let f = self;
        core::GateFinding {
            rule_id: f.rule_id,
            severity: f.severity.convert(),
            message: f.message,
            scope_path: f.scope_path,
            detail: f.detail,
        }
    }
}
