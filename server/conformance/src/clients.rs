// SPDX-License-Identifier: Apache-2.0
//! Black-box client adapters: `StorageBackend` implemented over the running
//! server's REST and gRPC planes. The SAME scenario set runs against both,
//! which IS the cross-plane parity check — any semantic divergence between
//! planes fails a scenario on one side.

use async_trait::async_trait;
use munarium_api_conv::{convert, Convert};
use munarium_api_types as dto;
use munarium_core::ledger::FactQuery;
use munarium_core::storage::{NewClaim, StorageBackend};
use munarium_core::types::*;
use munarium_core::{KernelError, Result};
use munarium_proto::mmp::v1 as pb;
use std::collections::BTreeMap;
use tonic::transport::Channel;

/// The uid the conformance suite acts as (M7 uid contract).
pub const CONFORMANCE_UID: &str = "conformance";

fn idem() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn parse_head_conflict(detail: &str) -> Option<(u64, u64)> {
    // "head conflict: expected seq X, actual Y"
    let rest = detail.strip_prefix("head conflict: expected seq ")?;
    let (e, a) = rest.split_once(", actual ")?;
    Some((e.trim().parse().ok()?, a.trim().parse().ok()?))
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

pub struct RestClientStore {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl RestClientStore {
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn check(&self, resp: reqwest::Response) -> Result<serde_json::Value> {
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| KernelError::Storage(format!("bad response body: {e}")))?;
        if status.is_success() {
            return Ok(body);
        }
        let slug = body["type"].as_str().unwrap_or_default();
        let detail = body["detail"].as_str().unwrap_or_default().to_string();
        Err(match slug.rsplit('/').next().unwrap_or_default() {
            "head-conflict" => KernelError::HeadConflict {
                expected: body["expected"].as_u64().unwrap_or(0),
                actual: body["actual"].as_u64().unwrap_or(0),
            },
            "not-found" => KernelError::NotFound {
                kind: "resource",
                id: detail,
            },
            "invalid-input" => KernelError::InvalidInput(detail),
            "unauthenticated" => KernelError::Unauthenticated(detail),
            "forbidden" => KernelError::Forbidden(detail),
            _ => KernelError::Storage(format!("{status}: {detail}")),
        })
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .header("idempotency-key", idem())
            // M7 uid contract: every /v1 call names the acting end user.
            .header("x-munarium-uid", CONFORMANCE_UID)
            .json(&body)
            .send()
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
        self.check(resp).await
    }

    async fn get(&self, path_and_query: &str) -> Result<serde_json::Value> {
        let resp = self
            .http
            .get(format!("{}{path_and_query}", self.base))
            .bearer_auth(&self.token)
            .header("x-munarium-uid", CONFORMANCE_UID)
            .send()
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
        self.check(resp).await
    }
}

fn dto_claim(v: &serde_json::Value) -> Result<Claim> {
    let d: dto::ClaimDto = serde_json::from_value(v.clone())
        .map_err(|e| KernelError::Storage(format!("claim decode: {e}")))?;
    Ok(Claim {
        id: d.id,
        version_id: d.version_id,
        seq: d.seq,
        claim_type: d.claim_type.convert(),
        subject: d.subject,
        key: d.key,
        value: d.value,
        scope_path: d.scope_path,
        status: d.status.convert(),
        provenance: match d.provenance {
            dto::ProvenanceDto::Witnessed => Provenance::Witnessed,
            dto::ProvenanceDto::Backfilled => Provenance::Backfilled,
            dto::ProvenanceDto::Repaired => Provenance::Repaired,
            dto::ProvenanceDto::Emergent => Provenance::Emergent,
            dto::ProvenanceDto::CoverageRepair => Provenance::CoverageRepair,
        },
        supersedes_id: d.supersedes_id,
        entity_id: d.entity_id,
        evidence: d.evidence,
        confidence: d.confidence,
        shape_ref: d.shape_ref,
        origin: d.origin.map(convert),
    })
}

#[async_trait]
impl StorageBackend for RestClientStore {
    async fn create_version(
        &self,
        parent_id: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        let body = serde_json::json!({ "parent_version_id": parent_id, "metadata": metadata });
        let v = self.post("/v1/versions", body).await?;
        Ok(v["version_id"].as_str().unwrap_or_default().to_string())
    }

    async fn lineage(&self, version_id: &str) -> Result<Vec<String>> {
        let v = self
            .get(&format!("/v1/versions/{version_id}/lineage"))
            .await?;
        Ok(serde_json::from_value(v["version_ids"].clone()).unwrap_or_default())
    }

    async fn head(&self, version_id: &str) -> Result<Seq> {
        let v = self.get(&format!("/v1/versions/{version_id}/head")).await?;
        Ok(v["head_seq"].as_u64().unwrap_or(0))
    }

    async fn append_claim(
        &self,
        version_id: &str,
        claim: NewClaim,
        expected_head: Option<Seq>,
    ) -> Result<Claim> {
        // The API decides accepted/disputed by running the gates server-side;
        // the client-declared status is intentionally not part of the wire.
        let body = serde_json::json!({
            "expected_head": expected_head,
            "claim_type": match claim.claim_type {
                ClaimType::Fact => "fact", ClaimType::Update => "update",
                ClaimType::Correction => "correction",
            },
            "subject": claim.subject,
            "key": claim.key,
            "value": claim.value,
            "scope_path": claim.scope_path,
            "supersedes_id": claim.supersedes_id,
            "evidence": claim.evidence,
            "confidence": claim.confidence,
            "shape_ref": claim.shape_ref,
            "origin": claim.origin,
        });
        let v = self
            .post(&format!("/v1/versions/{version_id}/claims"), body)
            .await?;
        dto_claim(&v["claim"])
    }

    async fn slice_facts(&self, version_id: &str, q: &FactQuery) -> Result<Vec<Claim>> {
        let mut query = Vec::new();
        if let Some(s) = &q.scope_prefix {
            query.push(format!("scope_prefix={s}"));
        }
        if let Some(pin) = q.as_of_seq {
            query.push(format!("as_of_seq={pin}"));
        }
        if let Some(l) = q.limit {
            query.push(format!("limit={l}"));
        }
        if !q.statuses.is_empty() {
            let s: Vec<&str> = q
                .statuses
                .iter()
                .map(|s| match s {
                    ClaimStatus::Accepted => "accepted",
                    ClaimStatus::Disputed => "disputed",
                })
                .collect();
            query.push(format!("statuses={}", s.join(",")));
        }
        let qs = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        let v = self
            .get(&format!("/v1/versions/{version_id}/facts{qs}"))
            .await?;
        v["facts"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(dto_claim)
            .collect()
    }

    async fn get_claim(&self, claim_id: &str) -> Result<Option<Claim>> {
        match self.get(&format!("/v1/claims/{claim_id}")).await {
            Ok(v) => Ok(Some(dto_claim(&v["claim"])?)),
            Err(KernelError::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn superseded_by(&self, claim_id: &str) -> Result<Option<String>> {
        match self.get(&format!("/v1/claims/{claim_id}")).await {
            Ok(v) => Ok(v["superseded_by"].as_str().map(String::from)),
            Err(KernelError::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn lock_anchor(
        &self,
        version_id: &str,
        subject: &str,
        key: &str,
        value: &str,
        scope_path: Option<&str>,
        evidence: Option<serde_json::Value>,
    ) -> Result<Anchor> {
        let body = serde_json::json!({
            "subject": subject, "key": key, "value": value,
            "scope_path": scope_path, "evidence": evidence,
        });
        let v = self
            .post(&format!("/v1/versions/{version_id}/anchors"), body)
            .await?;
        Ok(Anchor {
            id: v["id"].as_str().unwrap_or_default().into(),
            version_id: v["version_id"].as_str().unwrap_or_default().into(),
            detail_key: v["detail_key"].as_str().unwrap_or_default().into(),
            locked_value: v["locked_value"].as_str().unwrap_or_default().into(),
            locked_at_scope: v["locked_at_scope"].as_str().map(String::from),
            status: AnchorStatus::Locked,
            seq: v["seq"].as_u64().unwrap_or(0),
            evidence: None,
        })
    }

    async fn anchors(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<BTreeMap<String, Anchor>> {
        let qs = as_of_seq
            .map(|p| format!("?as_of_seq={p}"))
            .unwrap_or_default();
        let v = self
            .get(&format!("/v1/versions/{version_id}/anchors{qs}"))
            .await?;
        let mut out = BTreeMap::new();
        for a in v["anchors"].as_array().unwrap_or(&Vec::new()) {
            let anchor = Anchor {
                id: a["id"].as_str().unwrap_or_default().into(),
                version_id: a["version_id"].as_str().unwrap_or_default().into(),
                detail_key: a["detail_key"].as_str().unwrap_or_default().into(),
                locked_value: a["locked_value"].as_str().unwrap_or_default().into(),
                locked_at_scope: a["locked_at_scope"].as_str().map(String::from),
                status: AnchorStatus::Locked,
                seq: a["seq"].as_u64().unwrap_or(0),
                evidence: None,
            };
            out.insert(anchor.detail_key.clone(), anchor);
        }
        Ok(out)
    }

    async fn register_promise(
        &self,
        version_id: &str,
        key: &str,
        kind: &str,
        description: &str,
        origin_scope: Option<&str>,
        due_scope: Option<&str>,
    ) -> Result<Promise> {
        let body = serde_json::json!({
            "key": key, "kind": kind, "description": description,
            "origin_scope": origin_scope, "due_scope": due_scope,
        });
        let v = self
            .post(&format!("/v1/versions/{version_id}/promises"), body)
            .await?;
        Ok(promise_from_json(&v))
    }

    async fn promises(&self, version_id: &str, as_of_seq: Option<Seq>) -> Result<Vec<Promise>> {
        let qs = as_of_seq
            .map(|p| format!("?as_of_seq={p}"))
            .unwrap_or_default();
        let v = self
            .get(&format!("/v1/versions/{version_id}/promises{qs}"))
            .await?;
        Ok(v["promises"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(promise_from_json)
            .collect())
    }

    async fn fulfill_promise(&self, version_id: &str, key: &str) -> Result<bool> {
        let v = self
            .post(
                &format!("/v1/versions/{version_id}/promises/{key}/fulfill"),
                serde_json::json!({}),
            )
            .await?;
        Ok(v["fulfilled"].as_bool().unwrap_or(false))
    }

    async fn record_counts(
        &self,
        version_id: &str,
        key: &str,
        scope_path: &str,
        count: u64,
        budget: Option<u64>,
    ) -> Result<()> {
        self.post(
            &format!("/v1/versions/{version_id}/counters"),
            serde_json::json!({
                "key": key, "scope_path": scope_path, "count": count, "budget": budget,
            }),
        )
        .await?;
        Ok(())
    }

    async fn counter_totals(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<Vec<CounterTotal>> {
        let qs = as_of_seq
            .map(|p| format!("?as_of_seq={p}"))
            .unwrap_or_default();
        let v = self
            .get(&format!("/v1/versions/{version_id}/counters{qs}"))
            .await?;
        Ok(v["counters"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|c| CounterTotal {
                key: c["key"].as_str().unwrap_or_default().into(),
                total: c["total"].as_u64().unwrap_or(0),
                budget: c["budget"].as_u64(),
            })
            .collect())
    }

    async fn upsert_digest(&self, digest: &Digest) -> Result<()> {
        let resp = self
            .http
            .put(format!(
                "{}/v1/versions/{}/digests",
                self.base, digest.version_id
            ))
            .bearer_auth(&self.token)
            .header("x-munarium-uid", CONFORMANCE_UID)
            .json(&convert(digest.clone()))
            .send()
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
        self.check(resp).await?;
        Ok(())
    }

    async fn digests(&self, version_id: &str) -> Result<Vec<Digest>> {
        let v = self
            .get(&format!("/v1/versions/{version_id}/digests"))
            .await?;
        Ok(v["digests"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|d| Digest {
                version_id: d["version_id"].as_str().unwrap_or_default().into(),
                tier: d["tier"].as_u64().unwrap_or(0) as u8,
                scope_path: d["scope_path"].as_str().unwrap_or_default().into(),
                content: d["content"].as_str().unwrap_or_default().into(),
                content_hash: d["content_hash"].as_str().unwrap_or_default().into(),
                built_from_seq: d["built_from_seq"].as_u64().unwrap_or(0),
            })
            .collect())
    }
}

fn promise_from_json(v: &serde_json::Value) -> Promise {
    Promise {
        id: v["id"].as_str().unwrap_or_default().into(),
        version_id: v["version_id"].as_str().unwrap_or_default().into(),
        key: v["key"].as_str().unwrap_or_default().into(),
        kind: v["kind"].as_str().unwrap_or_default().into(),
        description: v["description"].as_str().unwrap_or_default().into(),
        origin_scope: v["origin_scope"].as_str().map(String::from),
        due_scope: v["due_scope"].as_str().map(String::from),
        status: match v["status"].as_str().unwrap_or("open") {
            "fulfilled" => PromiseStatus::Fulfilled,
            "expired" => PromiseStatus::Expired,
            "violated" => PromiseStatus::Violated,
            _ => PromiseStatus::Open,
        },
        seq: v["seq"].as_u64().unwrap_or(0),
        fulfilled_seq: v["fulfilled_seq"].as_u64(),
    }
}

// ---------------------------------------------------------------------------
// gRPC
// ---------------------------------------------------------------------------

pub struct GrpcClientStore {
    channel: Channel,
    token: String,
}

impl GrpcClientStore {
    pub async fn connect(endpoint: &str, token: &str) -> Result<Self> {
        let channel = Channel::from_shared(endpoint.to_string())
            .map_err(|e| KernelError::Storage(e.to_string()))?
            .connect()
            .await
            .map_err(|e| KernelError::Storage(format!("grpc connect {endpoint}: {e}")))?;
        Ok(Self {
            channel,
            token: token.to_string(),
        })
    }

    fn command(&self) -> pb::command_service_client::CommandServiceClient<Channel> {
        pb::command_service_client::CommandServiceClient::new(self.channel.clone())
    }

    fn query(&self) -> pb::query_service_client::QueryServiceClient<Channel> {
        pb::query_service_client::QueryServiceClient::new(self.channel.clone())
    }

    fn request<T>(&self, msg: T, with_idem: bool) -> tonic::Request<T> {
        let mut req = tonic::Request::new(msg);
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", self.token).parse().expect("ascii"),
        );
        // M7 uid contract: every mmp.v1 call names the acting end user.
        req.metadata_mut()
            .insert("munarium-uid", CONFORMANCE_UID.parse().expect("ascii"));
        if with_idem {
            req.metadata_mut()
                .insert("idempotency-key", idem().parse().expect("ascii"));
        }
        req
    }
}

fn status_to_err(s: tonic::Status) -> KernelError {
    match s.code() {
        tonic::Code::Aborted => {
            let (expected, actual) = parse_head_conflict(s.message()).unwrap_or((0, 0));
            KernelError::HeadConflict { expected, actual }
        }
        tonic::Code::NotFound => KernelError::NotFound {
            kind: "resource",
            id: s.message().to_string(),
        },
        tonic::Code::InvalidArgument => KernelError::InvalidInput(s.message().to_string()),
        tonic::Code::Unauthenticated => KernelError::Unauthenticated(s.message().to_string()),
        tonic::Code::PermissionDenied => KernelError::Forbidden(s.message().to_string()),
        _ => KernelError::Storage(format!("{}: {}", s.code(), s.message())),
    }
}

fn pb_to_promise(p: pb::Promise) -> Promise {
    Promise {
        id: p.id,
        version_id: p.version_id,
        key: p.key,
        kind: p.kind,
        description: p.description,
        origin_scope: if p.origin_scope.is_empty() {
            None
        } else {
            Some(p.origin_scope)
        },
        due_scope: if p.due_scope.is_empty() {
            None
        } else {
            Some(p.due_scope)
        },
        status: match p.status.as_str() {
            "fulfilled" => PromiseStatus::Fulfilled,
            "expired" => PromiseStatus::Expired,
            "violated" => PromiseStatus::Violated,
            _ => PromiseStatus::Open,
        },
        seq: p.seq,
        fulfilled_seq: if p.fulfilled_seq > 0 {
            Some(p.fulfilled_seq)
        } else {
            None
        },
    }
}

fn origin_to_pb(o: munarium_core::types::ClaimOrigin) -> pb::ClaimOrigin {
    pb::ClaimOrigin {
        kind: o.kind,
        source_id: o.source_id,
        mapping_version: o.mapping_version,
        row_key: o.row_key,
        event_position: o.event_position.unwrap_or_default(),
        observed_at: o.observed_at.unwrap_or_default(),
        evidence_id: o.evidence_id.unwrap_or_default(),
    }
}

fn pb_to_origin(o: pb::ClaimOrigin) -> munarium_core::types::ClaimOrigin {
    let opt = |s: String| if s.is_empty() { None } else { Some(s) };
    munarium_core::types::ClaimOrigin {
        kind: o.kind,
        source_id: o.source_id,
        mapping_version: o.mapping_version,
        row_key: o.row_key,
        event_position: opt(o.event_position),
        observed_at: opt(o.observed_at),
        evidence_id: opt(o.evidence_id),
    }
}

fn pb_to_claim(c: pb::Claim) -> Claim {
    Claim {
        id: c.id,
        version_id: c.version_id,
        seq: c.seq,
        claim_type: match pb::ClaimType::try_from(c.claim_type) {
            Ok(pb::ClaimType::Update) => ClaimType::Update,
            Ok(pb::ClaimType::Correction) => ClaimType::Correction,
            _ => ClaimType::Fact,
        },
        subject: c.subject,
        key: c.key,
        value: c.value,
        scope_path: if c.scope_path.is_empty() {
            None
        } else {
            Some(c.scope_path)
        },
        status: match pb::ClaimStatus::try_from(c.status) {
            Ok(pb::ClaimStatus::Disputed) => ClaimStatus::Disputed,
            _ => ClaimStatus::Accepted,
        },
        provenance: match pb::Provenance::try_from(c.provenance) {
            Ok(pb::Provenance::Backfilled) => Provenance::Backfilled,
            Ok(pb::Provenance::Repaired) => Provenance::Repaired,
            Ok(pb::Provenance::Emergent) => Provenance::Emergent,
            Ok(pb::Provenance::CoverageRepair) => Provenance::CoverageRepair,
            _ => Provenance::Witnessed,
        },
        supersedes_id: if c.supersedes_id.is_empty() {
            None
        } else {
            Some(c.supersedes_id)
        },
        entity_id: if c.entity_id.is_empty() {
            None
        } else {
            Some(c.entity_id)
        },
        evidence: if c.evidence_json.is_empty() {
            None
        } else {
            serde_json::from_str(&c.evidence_json).ok()
        },
        confidence: if c.confidence != 0.0 {
            Some(c.confidence)
        } else {
            None
        },
        shape_ref: if c.shape_ref.is_empty() {
            None
        } else {
            Some(c.shape_ref)
        },
        origin: c.origin.map(pb_to_origin),
    }
}

#[async_trait]
impl StorageBackend for GrpcClientStore {
    async fn create_version(
        &self,
        parent_id: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        let resp = self
            .command()
            .create_version(self.request(
                pb::CreateVersionRequest {
                    parent_version_id: parent_id.unwrap_or_default().to_string(),
                    metadata_json: metadata.map(|m| m.to_string()).unwrap_or_default(),
                },
                true,
            ))
            .await
            .map_err(status_to_err)?;
        Ok(resp.into_inner().version_id)
    }

    async fn lineage(&self, version_id: &str) -> Result<Vec<String>> {
        let resp = self
            .query()
            .get_lineage(self.request(
                pb::GetLineageRequest {
                    version_id: version_id.to_string(),
                },
                false,
            ))
            .await
            .map_err(status_to_err)?;
        Ok(resp
            .into_inner()
            .lineage
            .map(|l| l.version_ids)
            .unwrap_or_default())
    }

    async fn head(&self, version_id: &str) -> Result<Seq> {
        let resp = self
            .query()
            .get_head(self.request(
                pb::GetHeadRequest {
                    version_id: version_id.to_string(),
                },
                false,
            ))
            .await
            .map_err(status_to_err)?;
        Ok(resp.into_inner().head_seq)
    }

    async fn append_claim(
        &self,
        version_id: &str,
        claim: NewClaim,
        expected_head: Option<Seq>,
    ) -> Result<Claim> {
        let resp = self
            .command()
            .propose_claim(self.request(
                pb::ProposeClaimRequest {
                    version_id: version_id.to_string(),
                    expected_head,
                    claim_type: match claim.claim_type {
                        ClaimType::Fact => pb::ClaimType::Fact as i32,
                        ClaimType::Update => pb::ClaimType::Update as i32,
                        ClaimType::Correction => pb::ClaimType::Correction as i32,
                    },
                    subject: claim.subject,
                    key: claim.key,
                    value: claim.value,
                    scope_path: claim.scope_path.unwrap_or_default(),
                    provenance: pb::Provenance::Unspecified as i32,
                    supersedes_id: claim.supersedes_id.unwrap_or_default(),
                    entity_id: claim.entity_id.unwrap_or_default(),
                    evidence_json: claim.evidence.map(|v| v.to_string()).unwrap_or_default(),
                    confidence: claim.confidence.unwrap_or(0.0),
                    shape_ref: claim.shape_ref.unwrap_or_default(),
                    origin: claim.origin.map(origin_to_pb),
                },
                true,
            ))
            .await
            .map_err(status_to_err)?;
        resp.into_inner()
            .claim
            .map(pb_to_claim)
            .ok_or_else(|| KernelError::Storage("empty claim in response".into()))
    }

    async fn slice_facts(&self, version_id: &str, q: &FactQuery) -> Result<Vec<Claim>> {
        let resp = self
            .query()
            .slice_facts(
                self.request(
                    pb::SliceFactsRequest {
                        version_id: version_id.to_string(),
                        scope_prefix: q.scope_prefix.clone().unwrap_or_default(),
                        as_of_seq: q.as_of_seq.unwrap_or(0),
                        statuses: q
                            .statuses
                            .iter()
                            .map(|s| match s {
                                ClaimStatus::Accepted => pb::ClaimStatus::Accepted as i32,
                                ClaimStatus::Disputed => pb::ClaimStatus::Disputed as i32,
                            })
                            .collect(),
                        limit: q.limit.unwrap_or(0) as u32,
                    },
                    false,
                ),
            )
            .await
            .map_err(status_to_err)?;
        Ok(resp
            .into_inner()
            .slice
            .map(|s| s.facts.into_iter().map(pb_to_claim).collect())
            .unwrap_or_default())
    }

    async fn get_claim(&self, claim_id: &str) -> Result<Option<Claim>> {
        match self
            .query()
            .get_claim(self.request(
                pb::GetClaimRequest {
                    claim_id: claim_id.to_string(),
                },
                false,
            ))
            .await
        {
            Ok(resp) => Ok(resp.into_inner().claim.map(pb_to_claim)),
            Err(s) if s.code() == tonic::Code::NotFound => Ok(None),
            Err(s) => Err(status_to_err(s)),
        }
    }

    async fn superseded_by(&self, claim_id: &str) -> Result<Option<String>> {
        match self
            .query()
            .get_claim(self.request(
                pb::GetClaimRequest {
                    claim_id: claim_id.to_string(),
                },
                false,
            ))
            .await
        {
            Ok(resp) => {
                let inner = resp.into_inner();
                Ok(if inner.superseded_by.is_empty() {
                    None
                } else {
                    Some(inner.superseded_by)
                })
            }
            Err(s) if s.code() == tonic::Code::NotFound => Ok(None),
            Err(s) => Err(status_to_err(s)),
        }
    }

    async fn lock_anchor(
        &self,
        version_id: &str,
        subject: &str,
        key: &str,
        value: &str,
        _scope_path: Option<&str>,
        evidence: Option<serde_json::Value>,
    ) -> Result<Anchor> {
        let resp = self
            .command()
            .lock_anchor(self.request(
                pb::LockAnchorRequest {
                    version_id: version_id.to_string(),
                    subject: subject.to_string(),
                    key: key.to_string(),
                    value: value.to_string(),
                    scope_path: String::new(),
                    evidence_json: evidence.map(|v| v.to_string()).unwrap_or_default(),
                },
                true,
            ))
            .await
            .map_err(status_to_err)?;
        let a = resp
            .into_inner()
            .anchor
            .ok_or_else(|| KernelError::Storage("empty anchor".into()))?;
        Ok(Anchor {
            id: a.id,
            version_id: a.version_id,
            detail_key: a.detail_key,
            locked_value: a.locked_value,
            locked_at_scope: if a.locked_at_scope.is_empty() {
                None
            } else {
                Some(a.locked_at_scope)
            },
            status: AnchorStatus::Locked,
            seq: a.seq,
            evidence: None,
        })
    }

    async fn anchors(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<BTreeMap<String, Anchor>> {
        let resp = self
            .query()
            .list_anchors(self.request(
                pb::ListAnchorsRequest {
                    version_id: version_id.to_string(),
                    as_of_seq: as_of_seq.unwrap_or(0),
                },
                false,
            ))
            .await
            .map_err(status_to_err)?;
        let mut out = BTreeMap::new();
        for a in resp.into_inner().anchors {
            out.insert(
                a.detail_key.clone(),
                Anchor {
                    id: a.id,
                    version_id: a.version_id,
                    detail_key: a.detail_key,
                    locked_value: a.locked_value,
                    locked_at_scope: if a.locked_at_scope.is_empty() {
                        None
                    } else {
                        Some(a.locked_at_scope)
                    },
                    status: AnchorStatus::Locked,
                    seq: a.seq,
                    evidence: None,
                },
            );
        }
        Ok(out)
    }

    async fn register_promise(
        &self,
        version_id: &str,
        key: &str,
        kind: &str,
        description: &str,
        origin_scope: Option<&str>,
        due_scope: Option<&str>,
    ) -> Result<Promise> {
        let resp = self
            .command()
            .open_promise(self.request(
                pb::OpenPromiseRequest {
                    version_id: version_id.to_string(),
                    key: key.to_string(),
                    kind: kind.to_string(),
                    description: description.to_string(),
                    origin_scope: origin_scope.unwrap_or_default().to_string(),
                    due_scope: due_scope.unwrap_or_default().to_string(),
                },
                true,
            ))
            .await
            .map_err(status_to_err)?;
        let p = resp
            .into_inner()
            .promise
            .ok_or_else(|| KernelError::Storage("empty promise".into()))?;
        Ok(pb_to_promise(p))
    }

    async fn promises(&self, version_id: &str, as_of_seq: Option<Seq>) -> Result<Vec<Promise>> {
        let resp = self
            .query()
            .list_promises(self.request(
                pb::ListPromisesRequest {
                    version_id: version_id.to_string(),
                    status: String::new(),
                    as_of_seq: as_of_seq.unwrap_or(0),
                },
                false,
            ))
            .await
            .map_err(status_to_err)?;
        Ok(resp
            .into_inner()
            .promises
            .into_iter()
            .map(pb_to_promise)
            .collect())
    }

    async fn fulfill_promise(&self, version_id: &str, key: &str) -> Result<bool> {
        let resp = self
            .command()
            .fulfill_promise(self.request(
                pb::FulfillPromiseRequest {
                    version_id: version_id.to_string(),
                    key: key.to_string(),
                    result_ref: String::new(),
                },
                true,
            ))
            .await
            .map_err(status_to_err)?;
        Ok(resp.into_inner().fulfilled)
    }

    async fn record_counts(
        &self,
        version_id: &str,
        key: &str,
        scope_path: &str,
        count: u64,
        budget: Option<u64>,
    ) -> Result<()> {
        self.command()
            .record_counts(self.request(
                pb::RecordCountsRequest {
                    version_id: version_id.to_string(),
                    key: key.to_string(),
                    scope_path: scope_path.to_string(),
                    count,
                    budget: budget.unwrap_or(0),
                },
                true,
            ))
            .await
            .map_err(status_to_err)?;
        Ok(())
    }

    async fn counter_totals(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<Vec<CounterTotal>> {
        let resp = self
            .query()
            .counter_totals(self.request(
                pb::CounterTotalsRequest {
                    version_id: version_id.to_string(),
                    as_of_seq: as_of_seq.unwrap_or(0),
                },
                false,
            ))
            .await
            .map_err(status_to_err)?;
        Ok(resp
            .into_inner()
            .counters
            .into_iter()
            .map(|c| CounterTotal {
                key: c.key,
                total: c.total,
                budget: if c.budget > 0 { Some(c.budget) } else { None },
            })
            .collect())
    }

    async fn upsert_digest(&self, digest: &Digest) -> Result<()> {
        self.command()
            .upsert_digest(self.request(
                pb::UpsertDigestRequest {
                    digest: Some(pb::Digest {
                        version_id: digest.version_id.clone(),
                        tier: digest.tier as u32,
                        scope_path: digest.scope_path.clone(),
                        content: digest.content.clone(),
                        content_hash: digest.content_hash.clone(),
                        built_from_seq: digest.built_from_seq,
                    }),
                },
                true,
            ))
            .await
            .map_err(status_to_err)?;
        Ok(())
    }

    async fn digests(&self, version_id: &str) -> Result<Vec<Digest>> {
        let resp = self
            .query()
            .list_digests(self.request(
                pb::ListDigestsRequest {
                    version_id: version_id.to_string(),
                },
                false,
            ))
            .await
            .map_err(status_to_err)?;
        Ok(resp
            .into_inner()
            .digests
            .into_iter()
            .map(|d| Digest {
                version_id: d.version_id,
                tier: d.tier as u8,
                scope_path: d.scope_path,
                content: d.content,
                content_hash: d.content_hash,
                built_from_seq: d.built_from_seq,
            })
            .collect())
    }
}
