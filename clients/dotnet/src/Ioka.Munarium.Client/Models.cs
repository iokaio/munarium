// SPDX-License-Identifier: Apache-2.0
// Typed wire models mirroring the server's munarium-api-types (the JSON casing
// truth). System.Text.Json source generation keeps serialization AOT-safe;
// unknown members are ignored on read, so additive server fields never break.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace Ioka.Munarium.Client;

/// <summary>One claim proposal (propose_claim / append_events batches).</summary>
public sealed record ClaimInput
{
    [JsonPropertyName("subject")] public required string Subject { get; init; }
    [JsonPropertyName("key")] public required string Key { get; init; }
    [JsonPropertyName("value")] public required string Value { get; init; }
    /// <summary>fact | update | correction (default fact).</summary>
    [JsonPropertyName("claim_type")] public string ClaimType { get; init; } = "fact";
    [JsonPropertyName("scope_path")] public string? ScopePath { get; init; }
    [JsonPropertyName("provenance")] public string? Provenance { get; init; }
    [JsonPropertyName("supersedes_id")] public string? SupersedesId { get; init; }
    [JsonPropertyName("entity_id")] public string? EntityId { get; init; }
    [JsonPropertyName("evidence")] public JsonElement? Evidence { get; init; }
    [JsonPropertyName("confidence")] public double? Confidence { get; init; }
    [JsonPropertyName("shape_ref")] public string? ShapeRef { get; init; }
    [JsonPropertyName("origin")] public ClaimOrigin? Origin { get; init; }
    [JsonPropertyName("expected_head")] public ulong? ExpectedHead { get; init; }
}

/// <summary>Where a connector-originated claim came from. Absent on
/// every model-extracted claim; provenance, never a gate input.</summary>
public sealed record ClaimOrigin
{
    [JsonPropertyName("kind")] public required string Kind { get; init; }
    [JsonPropertyName("source_id")] public required string SourceId { get; init; }
    [JsonPropertyName("mapping_version")] public required string MappingVersion { get; init; }
    [JsonPropertyName("row_key")] public required string RowKey { get; init; }
    [JsonPropertyName("event_position")] public string? EventPosition { get; init; }
    [JsonPropertyName("observed_at")] public string? ObservedAt { get; init; }
    [JsonPropertyName("evidence_id")] public string? EvidenceId { get; init; }
}

public sealed record Claim
{
    [JsonPropertyName("id")] public required string Id { get; init; }
    [JsonPropertyName("version_id")] public required string VersionId { get; init; }
    [JsonPropertyName("seq")] public ulong Seq { get; init; }
    [JsonPropertyName("claim_type")] public required string ClaimType { get; init; }
    [JsonPropertyName("subject")] public required string Subject { get; init; }
    [JsonPropertyName("key")] public required string Key { get; init; }
    [JsonPropertyName("value")] public required string Value { get; init; }
    [JsonPropertyName("normalized_text")] public required string NormalizedText { get; init; }
    [JsonPropertyName("scope_path")] public string? ScopePath { get; init; }
    /// <summary>accepted | disputed.</summary>
    [JsonPropertyName("status")] public required string Status { get; init; }
    [JsonPropertyName("provenance")] public required string Provenance { get; init; }
    [JsonPropertyName("supersedes_id")] public string? SupersedesId { get; init; }
    [JsonPropertyName("entity_id")] public string? EntityId { get; init; }
    [JsonPropertyName("evidence")] public JsonElement? Evidence { get; init; }
    [JsonPropertyName("confidence")] public double? Confidence { get; init; }
    [JsonPropertyName("shape_ref")] public string? ShapeRef { get; init; }
    [JsonPropertyName("origin")] public ClaimOrigin? Origin { get; init; }
}

public sealed record GateFinding
{
    /// <summary>Dotted rule id, e.g. "gate.ledger-conflict".</summary>
    [JsonPropertyName("rule_id")] public required string RuleId { get; init; }
    /// <summary>info | warn | block.</summary>
    [JsonPropertyName("severity")] public required string Severity { get; init; }
    [JsonPropertyName("message")] public required string Message { get; init; }
    [JsonPropertyName("scope_path")] public string? ScopePath { get; init; }
    [JsonPropertyName("detail")] public JsonElement? Detail { get; init; }
}

/// <summary>Result of ProposeClaim. A gate-blocked claim is SUCCESS with
/// Status == "disputed" plus findings — recorded, never dropped.</summary>
public sealed record ClaimOutcome
{
    [JsonPropertyName("claim")] public required Claim Claim { get; init; }
    [JsonPropertyName("findings")] public required IReadOnlyList<GateFinding> Findings { get; init; }
    [JsonPropertyName("head_seq")] public ulong HeadSeq { get; init; }
    [JsonIgnore] public bool IsDisputed => Claim.Status == "disputed";
}

public sealed record EventsOutcome
{
    [JsonPropertyName("claims")] public required IReadOnlyList<Claim> Claims { get; init; }
    [JsonPropertyName("findings")] public required IReadOnlyList<GateFinding> Findings { get; init; }
    [JsonPropertyName("head_seq")] public ulong HeadSeq { get; init; }
    [JsonIgnore] public bool IsDisputed => Claims.Any(c => c.Status == "disputed");
}

public sealed record ClaimLookup
{
    [JsonPropertyName("claim")] public required Claim Claim { get; init; }
    [JsonPropertyName("superseded")] public bool Superseded { get; init; }
    [JsonPropertyName("superseded_by")] public string? SupersededBy { get; init; }
}

public sealed record FactsPage
{
    [JsonPropertyName("facts")] public required IReadOnlyList<Claim> Facts { get; init; }
    [JsonPropertyName("as_of_seq")] public ulong AsOfSeq { get; init; }
    [JsonPropertyName("head_seq")] public ulong HeadSeq { get; init; }
}

public sealed record Anchor
{
    [JsonPropertyName("id")] public required string Id { get; init; }
    [JsonPropertyName("version_id")] public required string VersionId { get; init; }
    [JsonPropertyName("detail_key")] public required string DetailKey { get; init; }
    [JsonPropertyName("locked_value")] public required string LockedValue { get; init; }
    [JsonPropertyName("locked_at_scope")] public string? LockedAtScope { get; init; }
    [JsonPropertyName("status")] public required string Status { get; init; }
    [JsonPropertyName("seq")] public ulong Seq { get; init; }
}

public sealed record Promise
{
    [JsonPropertyName("id")] public required string Id { get; init; }
    [JsonPropertyName("version_id")] public required string VersionId { get; init; }
    [JsonPropertyName("key")] public required string Key { get; init; }
    [JsonPropertyName("kind")] public required string Kind { get; init; }
    [JsonPropertyName("description")] public required string Description { get; init; }
    [JsonPropertyName("origin_scope")] public string? OriginScope { get; init; }
    [JsonPropertyName("due_scope")] public string? DueScope { get; init; }
    /// <summary>open | fulfilled | expired | violated (AS-OF status under a pin).</summary>
    [JsonPropertyName("status")] public required string Status { get; init; }
    [JsonPropertyName("seq")] public ulong Seq { get; init; }
    [JsonPropertyName("fulfilled_seq")] public ulong? FulfilledSeq { get; init; }
}

public sealed record Counter
{
    [JsonPropertyName("key")] public required string Key { get; init; }
    [JsonPropertyName("total")] public ulong Total { get; init; }
    [JsonPropertyName("budget")] public ulong? Budget { get; init; }
}

public sealed record Digest
{
    [JsonPropertyName("version_id")] public required string VersionId { get; init; }
    [JsonPropertyName("tier")] public byte Tier { get; init; }
    [JsonPropertyName("scope_path")] public required string ScopePath { get; init; }
    [JsonPropertyName("content")] public required string Content { get; init; }
    [JsonPropertyName("content_hash")] public required string ContentHash { get; init; }
    [JsonPropertyName("built_from_seq")] public ulong BuiltFromSeq { get; init; }
}

public sealed record Section
{
    [JsonPropertyName("title")] public required string Title { get; init; }
    [JsonPropertyName("body")] public required string Body { get; init; }
}

public sealed record ComposedContext
{
    [JsonPropertyName("sections")] public required IReadOnlyList<Section> Sections { get; init; }
    [JsonPropertyName("text")] public required string Text { get; init; }
    [JsonPropertyName("estimated_tokens")] public ulong EstimatedTokens { get; init; }
    [JsonPropertyName("content_hash")] public required string ContentHash { get; init; }
    [JsonPropertyName("as_of_seq")] public ulong AsOfSeq { get; init; }
}

public sealed record PutSourceResult
{
    /// <summary>Stable identity of the source, derived from its logical path.</summary>
    [JsonPropertyName("source_id")] public required string SourceId { get; init; }
    /// <summary>hex sha-256 — integrity of the stored bytes.</summary>
    [JsonPropertyName("content_hash")] public required string ContentHash { get; init; }
    [JsonPropertyName("bytes_len")] public ulong BytesLen { get; init; }
    /// <summary>
    /// True only when this path already held these exact bytes. Re-uploading a
    /// path with NEW content is an update and reports false.
    /// </summary>
    [JsonPropertyName("already_existed")] public bool AlreadyExisted { get; init; }
}

public sealed record RecordIngestResult
{
    [JsonPropertyName("event_id")] public required string EventId { get; init; }
    [JsonPropertyName("seq")] public ulong Seq { get; init; }
}

public sealed record IndexStatus
{
    [JsonPropertyName("index_version")] public required string IndexVersion { get; init; }
    [JsonPropertyName("shape_ref")] public required string ShapeRef { get; init; }
    [JsonPropertyName("event_watermark")] public ulong EventWatermark { get; init; }
    [JsonPropertyName("active")] public bool Active { get; init; }
    [JsonPropertyName("manifest")] public JsonElement? Manifest { get; init; }
}

public sealed record SearchHit
{
    [JsonPropertyName("chunk_id")] public required string ChunkId { get; init; }
    /// <summary>Stable identity of the source document.</summary>
    [JsonPropertyName("source_id")] public required string SourceId { get; init; }
    /// <summary>The logical path — which document answered.</summary>
    [JsonPropertyName("source_path")] public required string SourcePath { get; init; }
    /// <summary>hex sha-256 of the bytes that path held at index time.</summary>
    [JsonPropertyName("source_content_hash")] public required string SourceContentHash { get; init; }
    [JsonPropertyName("text")] public required string Text { get; init; }
    [JsonPropertyName("score")] public double Score { get; init; }
    [JsonPropertyName("lexical_rank")] public uint? LexicalRank { get; init; }
    [JsonPropertyName("vector_rank")] public uint? VectorRank { get; init; }
    [JsonPropertyName("metadata")] public JsonElement? Metadata { get; init; }
}

/// <summary>
/// Every retrieval answer carries one — surface it, don't hide it. Sources are
/// named three ways deliberately: ids are stable identity, paths say WHICH
/// DOCUMENT answered (a bare hash never did), and hashes prove which bytes.
/// </summary>
public sealed record ProvenanceEnvelope
{
    [JsonPropertyName("chunk_ids")] public required IReadOnlyList<string> ChunkIds { get; init; }
    [JsonPropertyName("source_ids")] public required IReadOnlyList<string> SourceIds { get; init; }
    [JsonPropertyName("source_paths")] public required IReadOnlyList<string> SourcePaths { get; init; }
    [JsonPropertyName("source_content_hashes")] public required IReadOnlyList<string> SourceContentHashes { get; init; }
    [JsonPropertyName("index_version")] public required string IndexVersion { get; init; }
    [JsonPropertyName("event_watermark")] public ulong EventWatermark { get; init; }
    [JsonPropertyName("provider_fingerprint")] public string? ProviderFingerprint { get; init; }
}

public sealed record SearchResult
{
    [JsonPropertyName("hits")] public required IReadOnlyList<SearchHit> Hits { get; init; }
    [JsonPropertyName("envelope")] public required ProvenanceEnvelope Envelope { get; init; }
}

public sealed record ApplyShapeResult
{
    [JsonPropertyName("shape_ref")] public required string ShapeRef { get; init; }
    [JsonPropertyName("yaml_hash")] public required string YamlHash { get; init; }
    [JsonPropertyName("event_id")] public string? EventId { get; init; }
}

public sealed record RunbookRun
{
    [JsonPropertyName("run_id")] public required string RunId { get; init; }
    /// <summary>running | awaiting_approval | done | failed.</summary>
    [JsonPropertyName("state")] public required string State { get; init; }
}

public sealed record RunbookStep
{
    [JsonPropertyName("ordinal")] public uint Ordinal { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("state")] public required string State { get; init; }
    [JsonPropertyName("detail")] public JsonElement? Detail { get; init; }
}

public sealed record RunStatus
{
    [JsonPropertyName("run_id")] public required string RunId { get; init; }
    [JsonPropertyName("runbook_ref")] public required string RunbookRef { get; init; }
    [JsonPropertyName("state")] public required string State { get; init; }
    /// <summary>Always null on the gRPC transport (no wire field today).</summary>
    [JsonPropertyName("version_id")] public string? VersionId { get; init; }
    [JsonPropertyName("steps")] public required IReadOnlyList<RunbookStep> Steps { get; init; }
}

public sealed record ProviderHealth
{
    [JsonPropertyName("healthy")] public bool Healthy { get; init; }
    [JsonPropertyName("provider")] public required string Provider { get; init; }
    [JsonPropertyName("endpoint_fingerprint")] public required string EndpointFingerprint { get; init; }
    [JsonPropertyName("detail")] public required string Detail { get; init; }
}

public sealed record CompleteResult
{
    [JsonPropertyName("text")] public required string Text { get; init; }
    [JsonPropertyName("stop_reason")] public required string StopReason { get; init; }
    [JsonPropertyName("input_tokens")] public ulong InputTokens { get; init; }
    [JsonPropertyName("output_tokens")] public ulong OutputTokens { get; init; }
    /// <summary>The provider family that served the request (anthropic|openai|openrouter).</summary>
    [JsonPropertyName("provider")] public string Provider { get; init; } = "";
    /// <summary>The resolved model id that served the request.</summary>
    [JsonPropertyName("model")] public string Model { get; init; } = "";
    [JsonPropertyName("invocation_event_id")] public string? InvocationEventId { get; init; }
}

/// <summary>One /healthai probe: a small live completion against one provider/tier.</summary>
public sealed record HealthAiCheck
{
    [JsonPropertyName("provider")] public required string Provider { get; init; }
    [JsonPropertyName("tier")] public required string Tier { get; init; }
    [JsonPropertyName("model")] public required string Model { get; init; }
    [JsonPropertyName("ok")] public bool Ok { get; init; }
    /// <summary>True when the probe was skipped because no credential is configured.</summary>
    [JsonPropertyName("skipped")] public bool Skipped { get; init; }
    [JsonPropertyName("latency_ms")] public ulong? LatencyMs { get; init; }
    [JsonPropertyName("detail")] public required string Detail { get; init; }
}

public sealed record HealthAiResult
{
    [JsonPropertyName("healthy")] public bool Healthy { get; init; }
    [JsonPropertyName("checks")] public required IReadOnlyList<HealthAiCheck> Checks { get; init; }
}

public sealed record EmbedResult
{
    [JsonPropertyName("vectors")] public required IReadOnlyList<IReadOnlyList<float>> Vectors { get; init; }
    [JsonPropertyName("dimensions")] public ulong Dimensions { get; init; }
    [JsonPropertyName("cache_hit")] public bool CacheHit { get; init; }
    /// <summary>The provider family that served the request.</summary>
    [JsonPropertyName("provider")] public string Provider { get; init; } = "";
    /// <summary>The resolved model id that served the request.</summary>
    [JsonPropertyName("model")] public string Model { get; init; } = "";
    [JsonPropertyName("invocation_event_id")] public string? InvocationEventId { get; init; }
}

// -- internal wire helper shapes (REST) -------------------------------------

internal sealed record HeadResponse
{
    [JsonPropertyName("head_seq")] public ulong HeadSeq { get; init; }
}

internal sealed record VersionCreated
{
    [JsonPropertyName("version_id")] public required string VersionId { get; init; }
}

internal sealed record FulfillResponse
{
    [JsonPropertyName("fulfilled")] public bool Fulfilled { get; init; }
}

internal sealed record LineageResponse
{
    [JsonPropertyName("version_ids")] public required IReadOnlyList<string> VersionIds { get; init; }
}

internal sealed record AnchorsResponse
{
    [JsonPropertyName("anchors")] public required IReadOnlyList<Anchor> Anchors { get; init; }
}

internal sealed record PromisesResponse
{
    [JsonPropertyName("promises")] public required IReadOnlyList<Promise> Promises { get; init; }
}

internal sealed record CountersResponse
{
    [JsonPropertyName("counters")] public required IReadOnlyList<Counter> Counters { get; init; }
}

internal sealed record DigestsResponse
{
    [JsonPropertyName("digests")] public required IReadOnlyList<Digest> Digests { get; init; }
}

internal sealed record RunbookApplied
{
    [JsonPropertyName("runbook_ref")] public required string RunbookRef { get; init; }
}

internal sealed record ProviderApplied
{
    [JsonPropertyName("config_name")] public required string ConfigName { get; init; }
}

internal sealed record Problem
{
    [JsonPropertyName("type")] public string? Type { get; init; }
    [JsonPropertyName("detail")] public string? Detail { get; init; }
    /// <summary>The problem's own HTTP status — the fallback when the
    /// carrier has none (the SSE error event).</summary>
    [JsonPropertyName("status")] public int? Status { get; init; }
    [JsonPropertyName("expected")] public ulong? Expected { get; init; }
    [JsonPropertyName("actual")] public ulong? Actual { get; init; }
    [JsonPropertyName("gate_findings")] public IReadOnlyList<GateFinding>? GateFindings { get; init; }
    [JsonPropertyName("findings_total")] public ulong? FindingsTotal { get; init; }
    [JsonPropertyName("findings_truncated")] public bool? FindingsTruncated { get; init; }
    [JsonPropertyName("shape_ref")] public string? ShapeRef { get; init; }
    [JsonPropertyName("kind")] public string? Kind { get; init; }
    [JsonPropertyName("id")] public string? Id { get; init; }
}

internal sealed record EventsBody
{
    [JsonPropertyName("claims")] public required IReadOnlyList<ClaimInput> Claims { get; init; }
    [JsonPropertyName("candidate_text")] public string? CandidateText { get; init; }
    [JsonPropertyName("expected_head")] public ulong? ExpectedHead { get; init; }
}

internal sealed record OpenPromiseBody
{
    [JsonPropertyName("key")] public required string Key { get; init; }
    [JsonPropertyName("kind")] public required string Kind { get; init; }
    [JsonPropertyName("description")] public required string Description { get; init; }
    [JsonPropertyName("origin_scope")] public string? OriginScope { get; init; }
    [JsonPropertyName("due_scope")] public string? DueScope { get; init; }
}

internal sealed record LockAnchorBody
{
    [JsonPropertyName("subject")] public required string Subject { get; init; }
    [JsonPropertyName("key")] public required string Key { get; init; }
    [JsonPropertyName("value")] public required string Value { get; init; }
    [JsonPropertyName("scope_path")] public string? ScopePath { get; init; }
    [JsonPropertyName("evidence")] public JsonElement? Evidence { get; init; }
}

internal sealed record RecordCountsBody
{
    [JsonPropertyName("key")] public required string Key { get; init; }
    [JsonPropertyName("scope_path")] public required string ScopePath { get; init; }
    [JsonPropertyName("count")] public ulong Count { get; init; }
    [JsonPropertyName("budget")] public ulong? Budget { get; init; }
}

internal sealed record CreateVersionBody
{
    [JsonPropertyName("parent_version_id")] public string? ParentVersionId { get; init; }
    [JsonPropertyName("metadata")] public JsonElement? Metadata { get; init; }
}

internal sealed record RecordIngestBody
{
    [JsonPropertyName("content_hash")] public required string ContentHash { get; init; }
    [JsonPropertyName("shape_ref")] public string? ShapeRef { get; init; }
}

internal sealed record SearchBody
{
    [JsonPropertyName("query")] public required string Query { get; init; }
    [JsonPropertyName("shape_ref")] public required string ShapeRef { get; init; }
    [JsonPropertyName("top_k")] public uint? TopK { get; init; }
    [JsonPropertyName("index_version")] public string? IndexVersion { get; init; }
    [JsonPropertyName("filter")] public JsonElement? Filter { get; init; }
}

internal sealed record CompleteBody
{
    [JsonPropertyName("prompt")] public required string Prompt { get; init; }
    [JsonPropertyName("model")] public string? Model { get; init; }
    [JsonPropertyName("provider")] public string? Provider { get; init; }
    [JsonPropertyName("tier")] public string? Tier { get; init; }
    [JsonPropertyName("system")] public string? System { get; init; }
    [JsonPropertyName("max_tokens")] public uint? MaxTokens { get; init; }
    [JsonPropertyName("temperature")] public double? Temperature { get; init; }
    [JsonPropertyName("version_id")] public string? VersionId { get; init; }
}

internal sealed record EmbedBody
{
    [JsonPropertyName("inputs")] public required IReadOnlyList<string> Inputs { get; init; }
    [JsonPropertyName("model")] public string? Model { get; init; }
    [JsonPropertyName("provider")] public string? Provider { get; init; }
    [JsonPropertyName("version_id")] public string? VersionId { get; init; }
}

internal sealed record RunbookParams
{
    [JsonPropertyName("version_id")] public required string VersionId { get; init; }
}

/// <summary>Source-generated serialization context (AOT-safe, casing pinned
/// by the JsonPropertyName attributes above).</summary>
[JsonSourceGenerationOptions(DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull)]
[JsonSerializable(typeof(ClaimOutcome))]
[JsonSerializable(typeof(EventsOutcome))]
[JsonSerializable(typeof(ClaimLookup))]
[JsonSerializable(typeof(FactsPage))]
[JsonSerializable(typeof(ComposedContext))]
[JsonSerializable(typeof(PutSourceResult))]
[JsonSerializable(typeof(RecordIngestResult))]
[JsonSerializable(typeof(IndexStatus))]
[JsonSerializable(typeof(SearchResult))]
[JsonSerializable(typeof(ApplyShapeResult))]
[JsonSerializable(typeof(RunbookRun))]
[JsonSerializable(typeof(RunStatus))]
[JsonSerializable(typeof(ProviderHealth))]
[JsonSerializable(typeof(CompleteResult))]
[JsonSerializable(typeof(EmbedResult))]
[JsonSerializable(typeof(HealthAiCheck))]
[JsonSerializable(typeof(HealthAiResult))]
[JsonSerializable(typeof(Digest))]
[JsonSerializable(typeof(HeadResponse))]
[JsonSerializable(typeof(VersionCreated))]
[JsonSerializable(typeof(FulfillResponse))]
[JsonSerializable(typeof(LineageResponse))]
[JsonSerializable(typeof(AnchorsResponse))]
[JsonSerializable(typeof(PromisesResponse))]
[JsonSerializable(typeof(CountersResponse))]
[JsonSerializable(typeof(DigestsResponse))]
[JsonSerializable(typeof(RunbookApplied))]
[JsonSerializable(typeof(ProviderApplied))]
[JsonSerializable(typeof(Problem))]
[JsonSerializable(typeof(ClaimInput))]
[JsonSerializable(typeof(ClaimOrigin))]
[JsonSerializable(typeof(RunbookParams))]
[JsonSerializable(typeof(EventsBody))]
[JsonSerializable(typeof(OpenPromiseBody))]
[JsonSerializable(typeof(LockAnchorBody))]
[JsonSerializable(typeof(RecordCountsBody))]
[JsonSerializable(typeof(CreateVersionBody))]
[JsonSerializable(typeof(RecordIngestBody))]
[JsonSerializable(typeof(SearchBody))]
[JsonSerializable(typeof(CompleteBody))]
[JsonSerializable(typeof(EmbedBody))]
[JsonSerializable(typeof(JsonElement))]
[JsonSerializable(typeof(List<GateFinding>), TypeInfoPropertyName = "ListGateFinding")]
// -- the platform surface (records in Models.Platform.cs) --------------------
[JsonSerializable(typeof(FindingsResponse))]
[JsonSerializable(typeof(IngestFile))]
[JsonSerializable(typeof(IngestResult))]
[JsonSerializable(typeof(IngestBatchBody))]
[JsonSerializable(typeof(IngestBatchResponse))]
[JsonSerializable(typeof(BulkManifestEntry))]
[JsonSerializable(typeof(BulkOpenBody))]
[JsonSerializable(typeof(BulkOpenResult))]
[JsonSerializable(typeof(BulkChunkResult))]
[JsonSerializable(typeof(BulkStatusResult))]
[JsonSerializable(typeof(BulkCompleteResult))]
[JsonSerializable(typeof(SourceInfo))]
[JsonSerializable(typeof(Collection))]
[JsonSerializable(typeof(CollectionsResponse))]
[JsonSerializable(typeof(CreateCollectionBody))]
[JsonSerializable(typeof(RunbooksResponse))]
[JsonSerializable(typeof(RunbookInfo))]
[JsonSerializable(typeof(RunbookValidation))]
[JsonSerializable(typeof(RemovalRequest))]
[JsonSerializable(typeof(RemovalConfirmBody))]
[JsonSerializable(typeof(RemovalConfirmation))]
[JsonSerializable(typeof(ChronologyRulesApplied))]
[JsonSerializable(typeof(ProviderListResponse))]
[JsonSerializable(typeof(MaxTokensBudgets))]
[JsonSerializable(typeof(MaxTokensResponse))]
[JsonSerializable(typeof(SessionCreated))]
[JsonSerializable(typeof(TurnRequest))]
[JsonSerializable(typeof(TurnResult))]
[JsonSerializable(typeof(TurnProgressEvent))]
[JsonSerializable(typeof(Session))]
[JsonSerializable(typeof(IssueTokenBody))]
[JsonSerializable(typeof(IssuedToken))]
[JsonSerializable(typeof(TokensResponse))]
[JsonSerializable(typeof(TokenRevocation))]
[JsonSerializable(typeof(UsageReport))]
[JsonSerializable(typeof(AuditReport))]
[JsonSerializable(typeof(CostReport))]
[JsonSerializable(typeof(TimeseriesReport))]
[JsonSerializable(typeof(EndpointsReport))]
[JsonSerializable(typeof(RunbookReport))]
[JsonSerializable(typeof(SessionsReport))]
[JsonSerializable(typeof(EvidenceReport))]
[JsonSerializable(typeof(MatrixReport))]
[JsonSerializable(typeof(PatternsResponse))]
[JsonSerializable(typeof(PatternDetail))]
[JsonSerializable(typeof(CreateDraftBody))]
[JsonSerializable(typeof(Draft))]
[JsonSerializable(typeof(DraftsResponse))]
[JsonSerializable(typeof(DraftDeletion))]
[JsonSerializable(typeof(UpdateAnswersBody))]
[JsonSerializable(typeof(DraftValidation))]
[JsonSerializable(typeof(AssistBody))]
[JsonSerializable(typeof(AssistResult))]
[JsonSerializable(typeof(DraftBundle))]
[JsonSerializable(typeof(ApplyDraftResponse))]
[JsonSerializable(typeof(ServerVersionInfo))]
[JsonSerializable(typeof(EvidenceRows))]
[JsonSerializable(typeof(JsonElement))]
internal sealed partial class MunariumJsonContext : JsonSerializerContext;
