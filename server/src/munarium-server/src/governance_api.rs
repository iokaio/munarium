// SPDX-License-Identifier: Apache-2.0
//! Persistent, versioned collection publication policy. Only the authoring plane
//! writes it; query callers name collections, never governing versions/passages.
use crate::{error::ApiError, state::AppState};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use chrono::{DateTime, NaiveDate, Utc};
use munarium_core::{retrieval::CollectionInfo, KernelError, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    pub id: String,
    pub document_id: String,
    /// Internal immutable index scope, resolved and checked by Server at authoring.
    pub collection: String,
    pub index_version: String,
    pub source_id: String,
    pub source_content_hash: String,
    pub effective_from: String,
    pub effective_until: Option<String>,
    pub published_at: String,
    /// approved, superseded, or withdrawn. A withdrawal never revives an older revision.
    pub state: String,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentRelation {
    pub from_document: String,
    pub to_document: String,
    pub kind: String,
    pub effective_from: String,
    pub effective_until: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct QueryPolicy {
    pub enabled: bool,
    /// Historical queries require this clearance as well as collection access.
    pub historical_access_level: i32,
    pub max_passages: usize,
    pub max_context_characters: usize,
    pub retrieval_concurrency: usize,
    pub candidates_per_index: usize,
    /// None inherits the Server vocabulary model routing configuration.
    pub provider: Option<String>,
    pub tier: Option<String>,
    pub max_output_tokens: Option<u32>,
    /// Exact clearance-level overrides, authored independently of query callers.
    pub model_routes: Vec<AccessModelRoute>,
    pub allow_external_processing: bool,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AccessModelRoute {
    pub access_level: i32,
    pub provider: String,
    pub tier: String,
    pub max_context_characters: usize,
    pub max_output_tokens: u32,
    pub enabled: bool,
}
impl QueryPolicy {
    pub fn for_access_level(&self, level: i32) -> Self {
        let mut resolved = self.clone();
        if let Some(route) = self.model_routes.iter().find(|r| r.access_level == level) {
            resolved.provider = Some(route.provider.clone());
            resolved.tier = Some(route.tier.clone());
            resolved.max_context_characters = self
                .max_context_characters
                .min(route.max_context_characters);
            resolved.max_output_tokens = Some(route.max_output_tokens);
            resolved.enabled &= route.enabled;
        }
        resolved
    }
}
impl Default for QueryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            historical_access_level: 2,
            max_passages: 24,
            max_context_characters: 60_000,
            retrieval_concurrency: 20,
            candidates_per_index: 12,
            provider: None,
            tier: None,
            max_output_tokens: None,
            model_routes: vec![],
            allow_external_processing: false,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CollectionGovernance {
    /// Expected current revision on PUT; the returned revision is incremented.
    pub revision: i64,
    pub publications: Vec<Publication>,
    pub relations: Vec<DocumentRelation>,
    pub query: QueryPolicy,
}

fn invalid(s: &str) -> KernelError {
    KernelError::InvalidInput(s.into())
}
fn storage(e: sqlx::Error) -> KernelError {
    KernelError::Storage(e.to_string())
}
fn date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| invalid("expected ISO calendar date"))
}
fn active(from: &str, until: Option<&str>, at: NaiveDate) -> Result<bool> {
    Ok(date(from)? <= at && until.map(date).transpose()?.is_none_or(|d| d > at))
}
pub async fn collection(state: &AppState, tenant: &str, name: &str) -> Result<CollectionInfo> {
    let retrieval = state.retrieval_for(tenant)?;
    match retrieval.collection_by_id(name).await {
        Ok(info) => Ok(info),
        Err(KernelError::NotFound { .. }) => retrieval.collection_by_name(name).await,
        Err(e) => Err(e),
    }
}
pub async fn load(
    state: &AppState,
    tenant: &str,
    id: &str,
) -> Result<Option<CollectionGovernance>> {
    let row: Option<(serde_json::Value, i64)> = sqlx::query_as(
        "SELECT policy,revision FROM collection_governance WHERE tenant_id=$1 AND collection_id=$2 ORDER BY revision DESC LIMIT 1")
        .bind(tenant).bind(id).fetch_optional(crate::runbooks_api::pool(state)?).await.map_err(storage)?;
    row.map(|(v, revision)| {
        let mut policy: CollectionGovernance = serde_json::from_value(v)
            .map_err(|_| KernelError::Storage("invalid stored governance policy".into()))?;
        policy.revision = revision;
        Ok(policy)
    })
    .transpose()
}

fn validate(policy: &CollectionGovernance, previous: Option<&CollectionGovernance>) -> Result<()> {
    let q = &policy.query;
    let mut levels = HashSet::new();
    if q.max_output_tokens.is_some_and(|n| n == 0 || n > 100_000)
        || q.model_routes.iter().any(|r| {
            r.access_level < 0
                || !levels.insert(r.access_level)
                || r.provider.is_empty()
                || r.provider.len() > 100
                || !["fast", "capable", "frontier"].contains(&r.tier.as_str())
                || !(2000..=100_000).contains(&r.max_context_characters)
                || r.max_output_tokens == 0
                || r.max_output_tokens > 100_000
        })
    {
        return Err(invalid("invalid or duplicate access-level model route"));
    }
    if !(1..=32).contains(&q.max_passages)
        || q.historical_access_level < 0
        || !(2000..=100_000).contains(&q.max_context_characters)
        || !(1..=64).contains(&q.retrieval_concurrency)
        || !(1..=100).contains(&q.candidates_per_index)
        || q.tier
            .as_deref()
            .is_some_and(|t| !["fast", "capable", "frontier"].contains(&t))
        || q.provider
            .as_ref()
            .is_some_and(|p| p.is_empty() || p.len() > 100)
    {
        return Err(invalid("invalid collection query budget or model routing"));
    }
    let mut ids = HashSet::new();
    for p in &policy.publications {
        if [
            &p.id,
            &p.document_id,
            &p.collection,
            &p.index_version,
            &p.source_id,
        ]
        .iter()
        .any(|s| s.is_empty() || s.len() > 300 || s.chars().any(char::is_control))
            || !ids.insert(&p.id)
            || !["approved", "superseded", "withdrawn"].contains(&p.state.as_str())
        {
            return Err(invalid("invalid or duplicate publication"));
        }
        crate::rest::validate_content_hash(&p.source_content_hash)?;
        let from = date(&p.effective_from)?;
        if p.effective_until
            .as_deref()
            .map(date)
            .transpose()?
            .is_some_and(|d| d <= from)
        {
            return Err(invalid("publication end must follow its start"));
        }
        DateTime::parse_from_rfc3339(&p.published_at)
            .map_err(|_| invalid("invalid publication timestamp"))?;
    }
    for r in &policy.relations {
        if r.from_document == r.to_document
            || ![
                "supersedes",
                "conflicts",
                "requires",
                "amends",
                "applies-to",
                "exception-to",
            ]
            .contains(&r.kind.as_str())
        {
            return Err(invalid("invalid document relation"));
        }
        let from = date(&r.effective_from)?;
        if r.effective_until
            .as_deref()
            .map(date)
            .transpose()?
            .is_some_and(|d| d <= from)
        {
            return Err(invalid("invalid relation dates"));
        }
    }
    if let Some(previous) = previous {
        for old in &previous.publications {
            let new = policy
                .publications
                .iter()
                .find(|p| p.id == old.id)
                .ok_or_else(|| {
                    invalid("retain publication history; use withdrawal instead of deletion")
                })?;
            if old.document_id != new.document_id
                || old.source_id != new.source_id
                || old.source_content_hash != new.source_content_hash
                || old.effective_from != new.effective_from
                || old.published_at != new.published_at
                || (old.state == "withdrawn" && new.state != "withdrawn")
            {
                return Err(invalid(
                    "published identity and chronology are immutable; publish a new revision",
                ));
            }
        }
    }
    Ok(())
}

/// Resolve dates and withdrawal BEFORE retrieval. A missing prerequisite or
/// unresolved supersession chain is a review decision, never an LLM inference.
pub fn governing(
    policy: &CollectionGovernance,
    at: NaiveDate,
) -> Result<Option<Vec<&Publication>>> {
    let mut latest: BTreeMap<&str, (&Publication, NaiveDate, DateTime<Utc>)> = BTreeMap::new();
    let mut ambiguous = HashSet::new();
    for p in &policy.publications {
        let from = date(&p.effective_from)?;
        if from > at {
            continue;
        }
        let published = DateTime::parse_from_rfc3339(&p.published_at)
            .map_err(|_| invalid("invalid publication timestamp"))?
            .with_timezone(&Utc);
        if let Some((old, old_from, old_time)) = latest.get(p.document_id.as_str()) {
            if (from, published) == (*old_from, *old_time) && old.id != p.id {
                ambiguous.insert(p.document_id.as_str());
                continue;
            }
            if (from, published) < (*old_from, *old_time) {
                continue;
            }
        }
        ambiguous.remove(p.document_id.as_str());
        latest.insert(&p.document_id, (p, from, published));
    }
    if !ambiguous.is_empty() {
        return Ok(None);
    }
    let mut selected = Vec::new();
    for (p, _, _) in latest.values() {
        if p.state != "withdrawn" && active(&p.effective_from, p.effective_until.as_deref(), at)? {
            selected.push(*p);
        }
    }
    let relations = policy
        .relations
        .iter()
        .filter_map(
            |r| match active(&r.effective_from, r.effective_until.as_deref(), at) {
                Ok(true) => Some(Ok(r)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let families: HashSet<_> = selected.iter().map(|p| p.document_id.as_str()).collect();
    let superseded: HashSet<_> = relations
        .iter()
        .filter(|r| r.kind == "supersedes" && families.contains(r.from_document.as_str()))
        .map(|r| r.to_document.as_str())
        .collect();
    if relations
        .iter()
        .any(|r| r.kind == "supersedes" && superseded.contains(r.from_document.as_str()))
    {
        return Ok(None);
    }
    selected.retain(|p| !superseded.contains(p.document_id.as_str()));
    let families: HashSet<_> = selected.iter().map(|p| p.document_id.as_str()).collect();
    if relations.iter().any(|r| {
        families.contains(r.from_document.as_str())
            && (r.kind == "conflicts"
                || matches!(
                    r.kind.as_str(),
                    "requires" | "amends" | "applies-to" | "exception-to"
                ) && !families.contains(r.to_document.as_str()))
    }) {
        return Ok(None);
    }
    Ok(Some(selected))
}

pub fn needs_amendment_review(
    policy: &CollectionGovernance,
    at: NaiveDate,
    documents: &HashSet<String>,
) -> Result<bool> {
    for r in &policy.relations {
        if matches!(r.kind.as_str(), "amends" | "applies-to" | "exception-to")
            && active(&r.effective_from, r.effective_until.as_deref(), at)?
            && (documents.contains(&r.from_document) || documents.contains(&r.to_document))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[utoipa::path(get, path="/v1.2/collections/{id}/governance", params(("id"=String,Path)), responses((status=200,body=CollectionGovernance)), tag="governance")]
pub async fn get_collection_governance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Json<CollectionGovernance>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    let info = collection(&state, &ctx.tenant_id, &id).await?;
    Ok(Json(
        load(&state, &ctx.tenant_id, &info.id)
            .await?
            .unwrap_or_default(),
    ))
}

#[utoipa::path(put, path="/v1.2/collections/{id}/governance", params(("id"=String,Path)), request_body=CollectionGovernance, responses((status=200,body=CollectionGovernance),(status=409,description="revision changed")), tag="governance")]
pub async fn replace_collection_governance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(mut body): crate::rest::ProblemJson<CollectionGovernance>,
) -> std::result::Result<Json<CollectionGovernance>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    let info = collection(&state, &ctx.tenant_id, &id).await?;
    let pool = crate::runbooks_api::pool(&state)?;
    validate(&body, None)?;
    for p in &mut body.publications {
        let member = collection(&state, &ctx.tenant_id, &p.collection).await?;
        // Binding is deliberate authoring authority. Restrict internal indexes
        // to this collection's clearance, never silently lower their level.
        if member.id == info.id || member.access_level > info.access_level {
            return Err(
                invalid("member index must be separate and not exceed parent clearance").into(),
            );
        }
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM collection_chunks c JOIN index_versions i ON i.tenant_id=c.tenant_id AND i.id=c.index_version_id WHERE c.tenant_id=$1 AND c.index_version_id=$2 AND c.collection_id=$3 AND c.source_id=$4 AND c.source_hash=$5 AND i.activated_at IS NOT NULL)")
            .bind(&ctx.tenant_id).bind(&p.index_version).bind(&member.id).bind(&p.source_id).bind(&p.source_content_hash).fetch_one(pool).await.map_err(storage)?;
        if !exists && p.state != "withdrawn" {
            return Err(invalid("publication does not match its indexed source").into());
        }
        p.collection = member.id;
    }
    let mut tx = pool.begin().await.map_err(storage)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("governance/{}/{}", ctx.tenant_id, info.id))
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
    let row: Option<(serde_json::Value,i64)> = sqlx::query_as("SELECT policy,revision FROM collection_governance WHERE tenant_id=$1 AND collection_id=$2 ORDER BY revision DESC LIMIT 1")
        .bind(&ctx.tenant_id).bind(&info.id).fetch_optional(&mut *tx).await.map_err(storage)?;
    let previous = row
        .map(|(value, revision)| {
            let mut p: CollectionGovernance = serde_json::from_value(value)
                .map_err(|_| invalid("invalid stored governance policy"))?;
            p.revision = revision;
            Ok::<_, KernelError>(p)
        })
        .transpose()?;
    let actual = previous.as_ref().map_or(0, |p| p.revision);
    if actual != body.revision || body.revision < 0 {
        return Err(KernelError::HeadConflict {
            expected: body.revision.max(0) as u64,
            actual: actual as u64,
        }
        .into());
    }
    validate(&body, previous.as_ref())?;
    body.revision = actual + 1;
    sqlx::query("INSERT INTO collection_governance(tenant_id,collection_id,revision,policy,actor_uid) VALUES($1,$2,$3,$4,$5)")
        .bind(&ctx.tenant_id).bind(&info.id).bind(body.revision).bind(serde_json::to_value(&body).map_err(|_| invalid("invalid policy"))?)
        .bind(crate::middleware::uid_or_anonymous(uid.as_ref())).execute(&mut *tx).await.map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_overrides_require_exact_clearance_and_reject_duplicate_routes() {
        let mut policy = CollectionGovernance::default();
        policy.query.provider = Some("base".into());
        policy.query.model_routes.push(AccessModelRoute {
            access_level: 2,
            provider: "review".into(),
            tier: "fast".into(),
            max_context_characters: 12000,
            max_output_tokens: 1500,
            enabled: true,
        });
        validate(&policy, None).unwrap();
        assert_eq!(
            policy.query.for_access_level(1).provider.as_deref(),
            Some("base")
        );
        let review = policy.query.for_access_level(2);
        assert_eq!(review.provider.as_deref(), Some("review"));
        assert_eq!(review.max_context_characters, 12000);
        assert_eq!(review.max_output_tokens, Some(1500));
        assert_eq!(
            policy.query.for_access_level(3).provider.as_deref(),
            Some("base")
        );
        policy
            .query
            .model_routes
            .push(policy.query.model_routes[0].clone());
        assert!(validate(&policy, None).is_err());
    }
    fn publication(id: &str, from: &str) -> Publication {
        Publication {
            id: id.into(),
            document_id: "ordering".into(),
            collection: format!("index-{id}"),
            index_version: format!("build-{id}"),
            source_id: format!("src-{id}"),
            source_content_hash: "a".repeat(64),
            effective_from: from.into(),
            effective_until: None,
            published_at: format!("{from}T00:00:00Z"),
            state: "approved".into(),
        }
    }
    fn policy(publications: Vec<Publication>) -> CollectionGovernance {
        CollectionGovernance {
            publications,
            ..Default::default()
        }
    }
    #[test]
    fn withdrawals_and_expiry_do_not_resurrect_earlier_publications() {
        let mut p = policy(vec![
            publication("one", "2025-01-01"),
            publication("two", "2025-06-01"),
        ]);
        assert_eq!(
            governing(&p, date("2025-05-31").unwrap()).unwrap().unwrap()[0].id,
            "one"
        );
        assert_eq!(
            governing(&p, date("2025-06-01").unwrap()).unwrap().unwrap()[0].id,
            "two"
        );
        p.publications[1].state = "withdrawn".into();
        assert!(governing(&p, date("2025-06-01").unwrap())
            .unwrap()
            .unwrap()
            .is_empty());
        p.publications[1].state = "approved".into();
        p.publications[1].effective_until = Some("2025-07-01".into());
        assert!(governing(&p, date("2025-07-01").unwrap())
            .unwrap()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn ambiguous_latest_publication_requires_review_regardless_of_input_order() {
        let mut p = policy(vec![
            publication("one", "2025-01-01"),
            publication("two", "2025-01-01"),
        ]);
        assert!(governing(&p, date("2025-08-01").unwrap())
            .unwrap()
            .is_none());
        p.publications.push(publication("three", "2025-06-01"));
        for _ in 0..3 {
            assert_eq!(
                governing(&p, date("2025-08-01").unwrap()).unwrap().unwrap()[0].id,
                "three"
            );
            p.publications.rotate_left(1);
        }
    }
    #[test]
    fn history_cannot_be_deleted_rewritten_or_unwithdrawn() {
        let mut old = policy(vec![publication("one", "2025-01-01")]);
        assert!(validate(&policy(vec![]), Some(&old)).is_err());
        let mut new = old.clone();
        new.publications[0].effective_from = "2024-01-01".into();
        assert!(validate(&new, Some(&old)).is_err());
        old.publications[0].state = "withdrawn".into();
        assert!(validate(&new, Some(&old)).is_err());
        new = old.clone();
        new.publications[0].state = "approved".into();
        assert!(validate(&new, Some(&old)).is_err());
        assert!(validate(&old, Some(&old)).is_ok());
    }
    fn relation(kind: &str, from: &str, to: &str) -> DocumentRelation {
        DocumentRelation {
            from_document: from.into(),
            to_document: to.into(),
            kind: kind.into(),
            effective_from: "2025-01-01".into(),
            effective_until: None,
        }
    }
    #[test]
    fn missing_prerequisites_and_supersession_chains_require_review() {
        let mut p = policy(vec![publication("one", "2025-01-01")]);
        p.relations
            .push(relation("requires", "ordering", "missing"));
        assert!(governing(&p, date("2025-06-01").unwrap())
            .unwrap()
            .is_none());
        p.relations = vec![
            relation("supersedes", "ordering", "old"),
            relation("supersedes", "old", "oldest"),
        ];
        assert!(governing(&p, date("2025-06-01").unwrap())
            .unwrap()
            .is_none());
    }
    #[test]
    fn amendments_only_require_review_when_related_content_was_selected() {
        let mut p = policy(vec![publication("one", "2025-01-01")]);
        p.relations.push(relation("amends", "ordering", "base"));
        assert!(!needs_amendment_review(
            &p,
            date("2025-06-01").unwrap(),
            &HashSet::from(["unrelated".into()])
        )
        .unwrap());
        assert!(needs_amendment_review(
            &p,
            date("2025-06-01").unwrap(),
            &HashSet::from(["base".into()])
        )
        .unwrap());
    }
    #[test]
    fn collection_size_is_not_limited_by_answer_context_budget() {
        let p = policy(
            (0..500)
                .map(|i| {
                    let mut p = publication(&i.to_string(), "2025-01-01");
                    p.document_id = i.to_string();
                    p
                })
                .collect(),
        );
        validate(&p, None).unwrap();
        assert_eq!(
            governing(&p, date("2025-06-01").unwrap())
                .unwrap()
                .unwrap()
                .len(),
            500
        );
    }
}
