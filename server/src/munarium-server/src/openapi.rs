// SPDX-License-Identifier: Apache-2.0
//! OpenAPI document, generated from the munarium-api-types DTOs and the REST
//! handler annotations. Served at /openapi.json; `munarium-server openapi`
//! prints it for the CI drift check against docs/api/openapi.json.
//!
//! Every /v1 route plus the meta routes is annotated; bearer auth is declared
//! globally (the meta routes opt out with `security(())`).

use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// Declares the uid contract once: every /v1 path takes a required
/// X-Munarium-Uid header (the end-user id asserted by the API-management
/// layer). Meta routes stay exempt.
struct UidHeaderAddon;

impl Modify for UidHeaderAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::path::{ParameterBuilder, ParameterIn};
        use utoipa::openapi::{ObjectBuilder, Required, Type};
        let uid_param = ParameterBuilder::new()
            .name("X-Munarium-Uid")
            .parameter_in(ParameterIn::Header)
            .required(Required::True)
            .description(Some(
                "End-user id asserted by the API-management layer. Required on every \
                 /v1 request (400 uid-required without it; MUNARIUM_REQUIRE_UID=false \
                 relaxes to 'anonymous'). When the bearer is a capability JWT, this \
                 must equal the token's sub claim (403 uid-mismatch otherwise).",
            ))
            .schema(Some(ObjectBuilder::new().schema_type(Type::String)))
            .build();
        for (path, item) in openapi.paths.paths.iter_mut() {
            if path.starts_with("/v1/") {
                item.parameters
                    .get_or_insert_with(Vec::new)
                    .push(uid_param.clone());
            }
        }
    }
}

/// Adds the bearerAuth scheme and applies it to every operation by default.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::default);
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "Static bearer token (MUNARIUM_AUTH_MODE=static). Tenant scope and \
                         rw/ro role derive from the token. Missing/invalid token -> 401 \
                         unauthenticated; ro token on a write -> 403 forbidden.",
                    ))
                    .build(),
            ),
        );
        openapi.security = Some(vec![utoipa::openapi::security::SecurityRequirement::new(
            "bearerAuth",
            Vec::<String>::new(),
        )]);
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "munarium-server",
        description = "Munarium governed-memory service — REST plane. gRPC twins live in proto/mmp/v1 (normative).",
        version = env!("CARGO_PKG_VERSION"),
        // Stated explicitly rather than inherited. Left unset, utoipa stamps
        // CARGO_PKG_LICENSE, which is now the same value — but the document
        // also ships verbatim inside the public MMP contract bundle
        // (contract/mmp/publish.py), and a contract document should declare its
        // own license rather than depend on the crate it was generated from.
        // OpenAPI 3.1 makes `identifier` and `url` mutually exclusive, so only
        // the SPDX id is given.
        license(name = "Apache-2.0", identifier = "Apache-2.0"),
    ),
    modifiers(&SecurityAddon, &UidHeaderAddon),
    paths(
        crate::rest::create_version,
        crate::rest::propose_claim,
        crate::rest::append_events,
        crate::rest::open_promise,
        crate::rest::fulfill_promise,
        crate::rest::lock_anchor,
        crate::rest::get_head,
        crate::rest::get_findings,
        crate::rest::record_findings,
        // The sealed evidence plane. REST-only in v1.
        crate::evidence_routes::seal,
        crate::evidence_routes::put_bytes,
        crate::evidence_routes::commit,
        crate::evidence_routes::get_manifest,
        crate::evidence_routes::get_rows,
        crate::evidence_routes::get_accesses,
        crate::evidence_routes::purge,
        crate::evidence_routes::legal_hold,
        crate::rest::get_claim,
        crate::rest::get_facts,
        crate::rest::get_lineage,
        crate::rest::get_anchors,
        crate::rest::get_promises,
        crate::rest::get_context,
        crate::rest::record_counts,
        crate::rest::get_counters,
        crate::rest::upsert_digest,
        crate::rest::get_digests,
        crate::rest::apply_shape,
        crate::chronology_api::apply_rules,
        crate::chronology_api::get_rules,
        crate::rest::put_source,
        crate::rest::get_source,
        crate::rest::record_ingest,
        crate::rest::build_index,
        // The derived-index tier.
        crate::datastore_builds::artifact_status,
        crate::datastore_builds::verify_artifacts,
        crate::datastore_builds::rebuild_artifact,
        crate::datastore_builds::bind_artifact,
        crate::datastore_builds::promote_artifact,
        crate::datastore_builds::activate_collection_index,
        crate::datastore_jobs::enqueue_job,
        crate::datastore_jobs::get_job,
        crate::datastore_jobs::list_jobs,
        crate::datastore_jobs::cancel_job,
        crate::datastore_serving::rollout_get,
        crate::datastore_serving::rollout_set,
        crate::datastore_builds::backfill,
        crate::rest::get_index,
        crate::rest::hybrid_search,
        crate::rest::healthz,
        crate::rest::readyz,
        crate::rest::version_info,
        crate::providers_api::apply_provider,
        crate::providers_api::list_providers,
        crate::providers_api::provider_health,
        crate::providers_api::provider_complete,
        crate::providers_api::provider_embed,
        crate::providers_api::healthai,
        crate::runbooks_api::apply_runbook,
        crate::runbooks_api::run_runbook,
        crate::runbooks_api::get_run,
        crate::runbooks_api::approve_step,
        crate::runbooks_api::list_runbooks,
        crate::runbooks_api::get_runbook_info,
        crate::runbooks_api::validate_runbook,
        crate::runbooks_api::remove_request,
        crate::runbooks_api::remove_confirm,
        crate::authoring_api::list_patterns,
        crate::authoring_api::get_pattern,
        crate::authoring_api::create_draft,
        crate::authoring_api::list_drafts,
        crate::authoring_api::get_draft,
        crate::authoring_api::delete_draft,
        crate::authoring_api::update_answers,
        crate::authoring_api::validate_draft,
        crate::authoring_api::assist_draft,
        crate::authoring_api::export_draft,
        crate::authoring_api::apply_draft,
        crate::ingest_api::ingest_file,
        crate::ingest_api::ingest_batch,
        crate::ingest_api::bulk_open,
        crate::ingest_api::bulk_chunk,
        crate::ingest_api::bulk_status,
        crate::ingest_api::bulk_complete,
        crate::tokens_api::issue_access_token,
        crate::reports_api::usage,
        crate::reports_api::audit,
        crate::reports_api::cost,
        crate::reports_api::budgets,
        crate::max_tokens_api::get_max_tokens,
        crate::max_tokens_api::replace_max_tokens,
        crate::reports_api::timeseries,
        crate::reports_api::endpoints,
        crate::reports_api::runbook_report,
        crate::reports_api::sessions_report,
        crate::reports_api::evidence_report,
        crate::reports_api::matrix_report,
        crate::reports_api::list_tokens,
        crate::reports_api::revoke_token,
        crate::sessions_api::create_session,
        crate::sessions_api::turn,
        crate::sessions_api::turn_stream,
        crate::sessions_api::get_session,
        crate::sessions_api::close_session,
        crate::collections_api::create_collection,
        crate::collections_api::list_collections,
        crate::collections_api::get_collection,
    ),
    components(schemas(
        munarium_api_types::Problem,
        munarium_api_types::GateFindingDto,
        munarium_api_types::StoredFindingDto,
        munarium_api_types::FindingsResponse,
        munarium_api_types::ClaimDto,
        munarium_api_types::AnchorDto,
        munarium_api_types::PromiseDto,
        munarium_api_types::ComposedContextDto,
        munarium_api_types::SectionDto,
        munarium_api_types::ClaimTypeDto,
        munarium_api_types::ClaimStatusDto,
        munarium_api_types::ProvenanceDto,
        munarium_api_types::SeverityDto,
        munarium_api_types::CreateVersionRequest,
        munarium_api_types::CreateVersionResponse,
        munarium_api_types::ProposeClaimRequest,
        munarium_api_types::ProposeClaimResponse,
        munarium_api_types::ClaimOriginDto,
        munarium_api_types::RecordFindingsRequest,
        munarium_api_types::RecordFindingsResponse,
        munarium_api_types::SealEvidenceRequest,
        munarium_api_types::SealEvidenceResponse,
        munarium_api_types::EvidenceGrantDto,
        munarium_api_types::CommitEvidenceResponse,
        munarium_api_types::EvidenceManifestResponse,
        munarium_api_types::EvidenceRowsResponse,
        munarium_api_types::EvidenceAccessDto,
        munarium_api_types::EvidenceAccessesResponse,
        munarium_api_types::LegalHoldRequest,
        munarium_api_types::PurgeEvidenceResponse,
        munarium_api_types::AppendEventsRequest,
        munarium_api_types::AppendEventsResponse,
        munarium_api_types::OpenPromiseRequest,
        munarium_api_types::FulfillPromiseResponse,
        munarium_api_types::LockAnchorRequest,
        munarium_api_types::HeadResponse,
        munarium_api_types::GetClaimResponse,
        munarium_api_types::FactsResponse,
        munarium_api_types::LineageResponse,
        munarium_api_types::AnchorsResponse,
        munarium_api_types::PromisesResponse,
        munarium_api_types::RecordCountsRequest,
        munarium_api_types::CounterDto,
        munarium_api_types::CountersResponse,
        munarium_api_types::DigestDto,
        munarium_api_types::DigestsResponse,
        munarium_api_types::OkResponse,
        munarium_api_types::ApplyShapeResponse,
        munarium_api_types::ApplyChronologyRulesResponse,
        munarium_api_types::PutSourceResponse,
        munarium_api_types::SourceInfoDto,
        munarium_api_types::RecordIngestRequest,
        munarium_api_types::RecordIngestResponse,
        munarium_api_types::IndexStatusResponse,
        munarium_api_types::SearchRequest,
        munarium_api_types::SearchHitDto,
        munarium_api_types::ProvenanceEnvelopeDto,
        munarium_api_types::SearchResponse,
        munarium_api_types::ApplyProviderConfigResponse,
        munarium_api_types::ProviderHealthResponse,
        munarium_api_types::CompleteRequest,
        munarium_api_types::CompleteResponse,
        munarium_api_types::EmbedRequest,
        munarium_api_types::EmbedResponse,
        munarium_api_types::HealthAiCheck,
        munarium_api_types::HealthAiResponse,
        munarium_api_types::ApplyRunbookResponse,
        munarium_api_types::RunbookRunResponse,
        munarium_api_types::RunbookStepDto,
        munarium_api_types::RunStatusResponse,
        munarium_api_types::IssueTokenRequest,
        munarium_api_types::IssueTokenResponse,
        munarium_api_types::RunbookCollectionDto,
        munarium_api_types::RunbookSummaryDto,
        munarium_api_types::RunbooksResponse,
        munarium_api_types::RunbookInfoResponse,
        munarium_api_types::ValidationFindingDto,
        munarium_api_types::SuggestionDto,
        munarium_api_types::ValidateRunbookResponse,
        munarium_api_types::ModelOverrideDto,
        munarium_api_types::PatternSummaryDto,
        munarium_api_types::PatternsResponse,
        munarium_api_types::NamedYamlDto,
        munarium_api_types::PatternDetailResponse,
        munarium_api_types::CreateDraftRequest,
        munarium_api_types::InterviewQuestionDto,
        munarium_api_types::InterviewSectionDto,
        munarium_api_types::DraftDocumentDto,
        munarium_api_types::DocumentFindingsDto,
        munarium_api_types::DraftValidationResponse,
        munarium_api_types::DraftSummaryDto,
        munarium_api_types::DraftsResponse,
        munarium_api_types::DraftResponse,
        munarium_api_types::UpdateAnswersRequest,
        munarium_api_types::DraftDeleteResponse,
        munarium_api_types::AssistDraftRequest,
        munarium_api_types::AssistDraftResponse,
        munarium_api_types::BundleToolDto,
        munarium_api_types::BundleValidationDto,
        munarium_api_types::ExportDraftResponse,
        munarium_api_types::AppliedDocDto,
        munarium_api_types::ApplyDraftResponse,
        munarium_api_types::UsageRow,
        munarium_api_types::UsageResponse,
        munarium_api_types::AuditEntryDto,
        munarium_api_types::AuditResponse,
        munarium_api_types::CostRow,
        munarium_api_types::CostResponse,
        munarium_api_types::BudgetRow,
        munarium_api_types::BudgetReportResponse,
        munarium_api_types::MaxTokensBudgets,
        munarium_api_types::MaxTokensResponse,
        munarium_api_types::TimeseriesBucket,
        munarium_api_types::TimeseriesResponse,
        munarium_api_types::EndpointRow,
        munarium_api_types::EndpointsResponse,
        munarium_api_types::RunbookRunsRow,
        munarium_api_types::RunbookStepsRow,
        munarium_api_types::RunbookReportResponse,
        munarium_api_types::SessionsBucket,
        munarium_api_types::SessionsReportResponse,
        munarium_api_types::EvidenceReportResponse,
        munarium_api_types::EvidenceLayerStatsDto,
        munarium_api_types::MatrixReportResponse,
        munarium_api_types::MatrixDataViewDto,
        munarium_api_types::EvidenceHierarchyDecisionDto,
        munarium_api_types::LayerOutcomeDto,
        munarium_api_types::TokenInfoDto,
        munarium_api_types::TokensResponse,
        munarium_api_types::RevokeTokenResponse,
        munarium_api_types::IngestFileRequest,
        munarium_api_types::IngestResultDto,
        munarium_api_types::IngestBatchRequest,
        munarium_api_types::IngestBatchResponse,
        munarium_api_types::BulkManifestEntry,
        munarium_api_types::BulkOpenRequest,
        munarium_api_types::BulkOpenResponse,
        munarium_api_types::BulkChunkRequest,
        munarium_api_types::BulkChunkResponse,
        munarium_api_types::BulkFileErrorDto,
        munarium_api_types::BulkStatusResponse,
        munarium_api_types::BulkCompleteResponse,
        munarium_api_types::RemovalRequestResponse,
        munarium_api_types::RemovalConfirmRequest,
        munarium_api_types::RemovalConfirmResponse,
        munarium_api_types::CreateSessionResponse,
        munarium_api_types::TurnRequest,
        munarium_api_types::TurnHitDto,
        munarium_api_types::CollectionEnvelopeDto,
        munarium_api_types::TurnCompletionDto,
        munarium_api_types::TurnVerificationDto,
        munarium_api_types::TurnResponse,
        munarium_api_types::TurnProgressEvent,
        munarium_api_types::ProviderModelsDto,
        munarium_api_types::ProviderListResponse,
        munarium_api_types::SessionTurnDto,
        munarium_api_types::SessionResponse,
        munarium_api_types::CreateCollectionRequest,
        munarium_api_types::CollectionDto,
        munarium_api_types::CollectionsResponse,
    )),
    tags(
        (name = "command", description = "Writes through the deterministic gates; Idempotency-Key required"),
        (name = "query", description = "Point-in-time reads; every route accepts as_of_seq"),
        (name = "shapes", description = "Declarative shape publication (YAML); violations record disputed claims"),
        (name = "ingest", description = "Content-addressed source upload + ingest events"),
        (name = "retrieval", description = "Hybrid search + versioned immutable indexes; every answer carries a ProvenanceEnvelope. Postgres store only"),
        (name = "providers", description = "BYOK provider gateway; invocations recorded as ledger events when a version is named"),
        (name = "runbooks", description = "Checkpointed step machines with approval gates; every transition is a ledger event. Postgres store only"),
        (name = "authoring", description = "Guided runbook-set authoring: the §19 pattern catalog, §16-ordered interview drafts, deterministic + BYOK-assisted composition, set-level validation, and hash-manifested export bundles. Drafts require the postgres store"),
        (name = "meta", description = "Unauthenticated service metadata"),
        (name = "access-tokens", description = "Management plane: capability-JWT issuance for the API-management layer (mgmt role required)"),
        (name = "collections", description = "Compartmentalized data collections: one LIST partition + own HNSW/GIN per collection; access_level + compartments gate retrieval. No delete API exists — see docs/ops/index-deletion-runbook.md"),
        (name = "sessions", description = "Data plane: multiturn sessions over a runbook's access-permitted collections; capability JWT with the query scope + X-Munarium-Uid; per-collection provenance envelopes; optional RAG completion with policy-gated model overrides"),
        (name = "reports", description = "Management plane: per-uid/session/runbook/collection usage, the uid-attributed audit trail, and model-spend rollups (mgmt role required)"),
    )
)]
pub struct ApiDoc;

pub fn doc() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
