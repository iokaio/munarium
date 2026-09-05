// SPDX-License-Identifier: Apache-2.0
// Typed wire models for the platform surface (sessions + streaming turns,
// access tokens, reports, guided authoring, collections, runbook management,
// the file/bulk ingest planes, findings, provider disclosure + the
// max_tokens budgets, /version) —
// mirroring the server's munarium-api-types (the JSON casing truth), same as
// Models.cs. Unknown members are ignored on read, so additive server fields
// never break.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace Ioka.Munarium.Client;

// -- query: persisted findings ----------------------------------------------

/// <summary>One persisted gate finding plus the head seq its write settled
/// at, so pinned reads bound this store like every other (2026-08-17).</summary>
public sealed record StoredFinding
{
    [JsonPropertyName("seq")] public ulong Seq { get; init; }
    [JsonPropertyName("finding")] public required GateFinding Finding { get; init; }
}

// -- sealed evidence, reads only ------------------------------------

/// <summary>A bounded window over a sealed evidence artifact's rows.
///
/// Served for canonical-CSV artifacts only. A Parquet artifact is sealed and
/// replayable byte-for-byte, but the server does not decode it and says so
/// rather than pretending the rows are unavailable.</summary>
public sealed record EvidenceRows
{
    [JsonPropertyName("evidence_id")] public required string EvidenceId { get; init; }
    /// <summary>Zero-based index of the first row returned.</summary>
    [JsonPropertyName("from")] public int From { get; init; }
    [JsonPropertyName("rows")] public IReadOnlyList<JsonElement> Rows { get; init; } = [];
    /// <summary>Total rows in the artifact, when the serialization allows
    /// counting them without decoding everything.</summary>
    [JsonPropertyName("total")] public int? Total { get; init; }
    [JsonPropertyName("has_more")] public bool HasMore { get; init; }
}

// -- ingest: file plane + bulk upload sessions ------------------------------

/// <summary>One file for the ingest plane. Content is base64 (JSON-safe);
/// the declared sha256, when present, is verified before commit (same
/// content-addressing contract as PUT /v1/sources).</summary>
public sealed record IngestFile
{
    /// <summary>Identity + storage path; required.</summary>
    [JsonPropertyName("filename")] public required string Filename { get; init; }
    [JsonPropertyName("media_type")] public required string MediaType { get; init; }
    [JsonPropertyName("content_base64")] public required string ContentBase64 { get; init; }
    [JsonPropertyName("sha256")] public string? Sha256 { get; init; }
    /// <summary>Explicit collection names to bind into. Null = auto-bind via
    /// the declarative <c>sources:</c> matchers of every active runbook the
    /// token may reach.</summary>
    [JsonPropertyName("collections")] public IReadOnlyList<string>? Collections { get; init; }
}

/// <summary>Where a document actually went — per-item outcome; a failed file
/// never fails the batch, check <see cref="Error"/>.</summary>
public sealed record IngestResult
{
    [JsonPropertyName("filename")] public required string Filename { get; init; }
    /// <summary>Stable identity of the stored source; null on per-item error.</summary>
    [JsonPropertyName("source_id")] public string? SourceId { get; init; }
    [JsonPropertyName("sha256")] public string? Sha256 { get; init; }
    /// <summary>True only when this path already held these exact bytes — a
    /// genuine idempotent replay.</summary>
    [JsonPropertyName("existed")] public bool Existed { get; init; }
    /// <summary>Collections this file is now bound to (from this call).</summary>
    [JsonPropertyName("bound_to")] public IReadOnlyList<string> BoundTo { get; init; } = [];
    [JsonPropertyName("error")] public string? Error { get; init; }
}

/// <summary>One bulk-session manifest entry: what the client intends to
/// upload. The sha256 is verified against every received chunk file, and the
/// diff against already-stored sources also compares it, so an identical
/// re-run needs no bytes at all.</summary>
public sealed record BulkManifestEntry
{
    [JsonPropertyName("filename")] public required string Filename { get; init; }
    [JsonPropertyName("sha256")] public required string Sha256 { get; init; }
    [JsonPropertyName("bytes_len")] public ulong BytesLen { get; init; }
    [JsonPropertyName("media_type")] public required string MediaType { get; init; }
}

public sealed record BulkOpenResult
{
    [JsonPropertyName("bulk_id")] public required string BulkId { get; init; }
    [JsonPropertyName("total")] public ulong Total { get; init; }
    /// <summary>Manifest entries whose logical path already holds these
    /// exact bytes — nothing to upload for these.</summary>
    [JsonPropertyName("already_present")] public ulong AlreadyPresent { get; init; }
    /// <summary>Filenames still owed bytes (the upload work list).</summary>
    [JsonPropertyName("needed")] public IReadOnlyList<string> Needed { get; init; } = [];
}

public sealed record BulkChunkResult
{
    [JsonPropertyName("bulk_id")] public required string BulkId { get; init; }
    /// <summary>Per-file outcomes, same shape as batch ingest.</summary>
    [JsonPropertyName("results")] public required IReadOnlyList<IngestResult> Results { get; init; }
    [JsonPropertyName("stored")] public ulong Stored { get; init; }
    [JsonPropertyName("skipped_existing")] public ulong SkippedExisting { get; init; }
    [JsonPropertyName("pending")] public ulong Pending { get; init; }
    [JsonPropertyName("failed")] public ulong Failed { get; init; }
}

public sealed record BulkFileError
{
    [JsonPropertyName("filename")] public required string Filename { get; init; }
    [JsonPropertyName("error")] public required string Error { get; init; }
}

public sealed record BulkStatusResult
{
    [JsonPropertyName("bulk_id")] public required string BulkId { get; init; }
    [JsonPropertyName("label")] public string? Label { get; init; }
    /// <summary>open | completed | expired.</summary>
    [JsonPropertyName("status")] public required string Status { get; init; }
    [JsonPropertyName("total")] public ulong Total { get; init; }
    [JsonPropertyName("stored")] public ulong Stored { get; init; }
    [JsonPropertyName("skipped_existing")] public ulong SkippedExisting { get; init; }
    [JsonPropertyName("pending")] public ulong Pending { get; init; }
    [JsonPropertyName("failed")] public ulong Failed { get; init; }
    /// <summary>Failed entries with their last error (capped at 100).</summary>
    [JsonPropertyName("failures")] public IReadOnlyList<BulkFileError> Failures { get; init; } = [];
    /// <summary>Filenames still owed bytes; populated only when the request
    /// asked (<c>includeNeeded: true</c>) — the resume work list.</summary>
    [JsonPropertyName("needed")] public IReadOnlyList<string>? Needed { get; init; }
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
    [JsonPropertyName("expires_at")] public required string ExpiresAt { get; init; }
    [JsonPropertyName("completed_at")] public string? CompletedAt { get; init; }
}

public sealed record BulkCompleteResult
{
    [JsonPropertyName("bulk_id")] public required string BulkId { get; init; }
    /// <summary>completed | incomplete — incomplete leaves the session open.</summary>
    [JsonPropertyName("status")] public required string Status { get; init; }
    [JsonPropertyName("total")] public ulong Total { get; init; }
    [JsonPropertyName("stored")] public ulong Stored { get; init; }
    [JsonPropertyName("skipped_existing")] public ulong SkippedExisting { get; init; }
    /// <summary>Manifest entries with no stored bytes (capped at 100).</summary>
    [JsonPropertyName("missing")] public IReadOnlyList<string> Missing { get; init; } = [];
    [JsonPropertyName("missing_count")] public ulong MissingCount { get; init; }
    /// <summary>Entries whose stored content hash no longer matches the
    /// manifest (capped at 100).</summary>
    [JsonPropertyName("mismatched")] public IReadOnlyList<string> Mismatched { get; init; } = [];
    [JsonPropertyName("mismatched_count")] public ulong MismatchedCount { get; init; }
}

/// <summary>Metadata for one stored source — never the bytes.</summary>
public sealed record SourceInfo
{
    [JsonPropertyName("source_id")] public required string SourceId { get; init; }
    /// <summary>The logical path: identity, and the blob name under the
    /// tenant prefix.</summary>
    [JsonPropertyName("filename")] public required string Filename { get; init; }
    [JsonPropertyName("media_type")] public required string MediaType { get; init; }
    /// <summary>hex sha-256 — integrity of the stored bytes.</summary>
    [JsonPropertyName("content_hash")] public required string ContentHash { get; init; }
    [JsonPropertyName("bytes_len")] public ulong BytesLen { get; init; }
    /// <summary>az | pg | mem.</summary>
    [JsonPropertyName("storage_backend")] public required string StorageBackend { get; init; }
    /// <summary>Backend-resolved URI. Never carries a SAS.</summary>
    [JsonPropertyName("blob_uri")] public string? BlobUri { get; init; }
    /// <summary>Null until first indexed, then ok | empty | failed.</summary>
    [JsonPropertyName("extraction_status")] public string? ExtractionStatus { get; init; }
    /// <summary>text | docx | pdf-text | ocr.</summary>
    [JsonPropertyName("extraction_method")] public string? ExtractionMethod { get; init; }
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
}

// -- retrieval: collections --------------------------------------------

/// <summary>A compartmentalized data collection. There is no delete anywhere
/// — collections retire softly.</summary>
public sealed record Collection
{
    [JsonPropertyName("id")] public required string Id { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("shape_ref")] public required string ShapeRef { get; init; }
    /// <summary>Access level a token must dominate to search this collection.</summary>
    [JsonPropertyName("access_level")] public int AccessLevel { get; init; }
    /// <summary>Need-to-know tags; a token must carry all of them.</summary>
    [JsonPropertyName("compartments")] public IReadOnlyList<string> Compartments { get; init; } = [];
    /// <summary>active | retired.</summary>
    [JsonPropertyName("status")] public required string Status { get; init; }
    [JsonPropertyName("description")] public string? Description { get; init; }
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
    [JsonPropertyName("source_count")] public long SourceCount { get; init; }
    /// <summary>The active index version id, if one has been cut over.</summary>
    [JsonPropertyName("active_index")] public string? ActiveIndex { get; init; }
}

// -- runbooks: management + chronology rules --------------------------

/// <summary>One collection a runbook spans, with its access requirements —
/// the unit of compartmentalization a caller must clear to see results.</summary>
public sealed record RunbookCollection
{
    [JsonPropertyName("name")] public required string Name { get; init; }
    /// <summary>The materialized collection id; null until applied.</summary>
    [JsonPropertyName("collection_id")] public string? CollectionId { get; init; }
    [JsonPropertyName("shape_ref")] public required string ShapeRef { get; init; }
    [JsonPropertyName("access_level")] public int AccessLevel { get; init; }
    [JsonPropertyName("compartments")] public IReadOnlyList<string> Compartments { get; init; } = [];
    [JsonPropertyName("active_index")] public string? ActiveIndex { get; init; }
    [JsonPropertyName("source_count")] public long SourceCount { get; init; }
}

public sealed record RunbookSummary
{
    /// <summary>name@version.</summary>
    [JsonPropertyName("runbook_ref")] public required string RunbookRef { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("version")] public uint Version { get; init; }
    /// <summary>active | remove_requested | removed.</summary>
    [JsonPropertyName("status")] public required string Status { get; init; }
    /// <summary>The minimum access level that sees ANY of this runbook's
    /// collections.</summary>
    [JsonPropertyName("min_access_level")] public int MinAccessLevel { get; init; }
    [JsonPropertyName("collections")] public IReadOnlyList<RunbookCollection> Collections { get; init; } = [];
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
}

public sealed record RunbookInfo
{
    [JsonPropertyName("runbook_ref")] public required string RunbookRef { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("version")] public uint Version { get; init; }
    [JsonPropertyName("status")] public required string Status { get; init; }
    [JsonPropertyName("collections")] public IReadOnlyList<RunbookCollection> Collections { get; init; } = [];
    /// <summary>Sibling versions of the same name (refs), including this one.</summary>
    [JsonPropertyName("versions")] public IReadOnlyList<string> Versions { get; init; } = [];
    /// <summary>The models block (defaults per task level + override
    /// policy), echoed.</summary>
    [JsonPropertyName("models")] public JsonElement? Models { get; init; }
    /// <summary>Retrieval knobs in effect. Always present on REST; null on
    /// gRPC only when the wire JSON fails to parse.</summary>
    [JsonPropertyName("retrieval")] public JsonElement? Retrieval { get; init; }
    /// <summary>Whether session turns can run a RAG completion step.</summary>
    [JsonPropertyName("has_completion")] public bool HasCompletion { get; init; }
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
}

public sealed record ValidationFinding
{
    /// <summary>error | warn | info.</summary>
    [JsonPropertyName("severity")] public required string Severity { get; init; }
    /// <summary>Stable dotted code, e.g. "steps.cutover-before-build".</summary>
    [JsonPropertyName("code")] public required string Code { get; init; }
    [JsonPropertyName("message")] public required string Message { get; init; }
    [JsonPropertyName("path")] public required string Path { get; init; }
}

/// <summary>AI-assisted improvement suggestion (advisory only).</summary>
public sealed record Suggestion
{
    [JsonPropertyName("title")] public required string Title { get; init; }
    [JsonPropertyName("rationale")] public required string Rationale { get; init; }
    [JsonPropertyName("patch_hint")] public string? PatchHint { get; init; }
}

public sealed record RunbookValidation
{
    /// <summary>False when any error-severity finding is present.</summary>
    [JsonPropertyName("valid")] public bool Valid { get; init; }
    [JsonPropertyName("findings")] public IReadOnlyList<ValidationFinding> Findings { get; init; } = [];
    /// <summary>Present when suggest was requested and a provider is configured.</summary>
    [JsonPropertyName("suggestions")] public IReadOnlyList<Suggestion> Suggestions { get; init; } = [];
    [JsonPropertyName("suggest_note")] public string? SuggestNote { get; init; }
}

public sealed record RemovalRequest
{
    [JsonPropertyName("runbook_ref")] public required string RunbookRef { get; init; }
    /// <summary>Present this id to RemoveConfirmAsync within the TTL.</summary>
    [JsonPropertyName("removal_id")] public required string RemovalId { get; init; }
    [JsonPropertyName("expires_at")] public required string ExpiresAt { get; init; }
}

public sealed record RemovalConfirmation
{
    [JsonPropertyName("runbook_ref")] public required string RunbookRef { get; init; }
    /// <summary>Always "removed" on success. All data is retained — removal
    /// is visibility-only.</summary>
    [JsonPropertyName("status")] public required string Status { get; init; }
}

/// <summary>Applied chronology-rules asset — the sixth gate's arming surface
/// (2026-08-17).</summary>
public sealed record ChronologyRulesApplied
{
    [JsonPropertyName("name")] public required string Name { get; init; }
    /// <summary>Rule targets declared across the rule kinds — a sanity echo,
    /// not a validation result.</summary>
    [JsonPropertyName("rule_count")] public ulong RuleCount { get; init; }
}

// -- providers: free tier-model disclosure ----------------------------------

/// <summary>One provider config's resolved tier models — free introspection,
/// zero provider calls; the credential itself is never echoed.</summary>
public sealed record ProviderModels
{
    /// <summary>Config name, or default-&lt;family&gt; for the synthesized
    /// env-backed default.</summary>
    [JsonPropertyName("name")] public required string Name { get; init; }
    /// <summary>Provider family (anthropic | openai | openrouter).</summary>
    [JsonPropertyName("provider")] public required string Provider { get; init; }
    /// <summary>applied (tenant-applied config) or default (synthesized).</summary>
    [JsonPropertyName("source")] public required string Source { get; init; }
    /// <summary>Whether the config's credential currently resolves.</summary>
    [JsonPropertyName("credential_ok")] public bool CredentialOk { get; init; }
    [JsonPropertyName("fast")] public string? Fast { get; init; }
    [JsonPropertyName("capable")] public string? Capable { get; init; }
    [JsonPropertyName("frontier")] public string? Frontier { get; init; }
}

// -- providers: per-call max_tokens budgets ---------------------------------

/// <summary>The per-call output-token ceilings (<c>max_tokens</c>) the
/// server hands a model provider, one per kind of paid call, as ONE object
/// — the body of <c>POST /v1/max-tokens</c>. Every field is REQUIRED on the
/// wire: the POST replaces the tenant's whole set, never part of it, so a
/// body missing a field is <c>invalid-input</c>, not a partial update (which
/// is why every member here is <c>required</c>). The server range-checks
/// each — <see cref="TurnCompletion"/> 256..16384, <see cref="QueryExpansion"/>
/// 32..512, the rest 1..65536 — and answers 400 <c>invalid-input</c> outside
/// them.
///
/// Precedence at call time: a runbook's own declaration where the grammar
/// has one (<c>completion.maxTokens</c>, <c>modelQueryExpansion.maxTokens</c>)
/// &gt; the tenant's replacement set through this API &gt; the process's
/// <c>MUNARIUM_MAX_TOKENS_*</c> environment &gt; the built-ins.</summary>
public sealed record MaxTokensBudgets
{
    /// <summary>A session turn's answer; a runbook's <c>completion.maxTokens</c>
    /// overrides it. The truncation-aware retry pays one re-ask at 4x.
    /// Built-in 2,048.</summary>
    [JsonPropertyName("turn_completion")] public required uint TurnCompletion { get; init; }
    /// <summary>The <c>modelQueryExpansion</c> variant-generation call; a
    /// runbook's <c>modelQueryExpansion.maxTokens</c> overrides it.
    /// Built-in 256.</summary>
    [JsonPropertyName("query_expansion")] public required uint QueryExpansion { get; init; }
    /// <summary><c>POST /v1/providers/{name}/complete</c> when the request
    /// omits <c>max_tokens</c>. Built-in 1,024.</summary>
    [JsonPropertyName("complete_default")] public required uint CompleteDefault { get; init; }
    /// <summary>Each <c>/healthai</c> probe completion. Built-in 512.</summary>
    [JsonPropertyName("healthai_probe")] public required uint HealthaiProbe { get; init; }
    /// <summary>The evidence hierarchy's one-word question classifier.
    /// Built-in 32.</summary>
    [JsonPropertyName("hierarchy_classifier")] public required uint HierarchyClassifier { get; init; }
    /// <summary>The evidence hierarchy's semantic-intent task (names only).
    /// Built-in 480.</summary>
    [JsonPropertyName("hierarchy_intent")] public required uint HierarchyIntent { get; init; }
    /// <summary>The runbook validation AI advisory pass. Built-in 2,048.</summary>
    [JsonPropertyName("runbook_advisory")] public required uint RunbookAdvisory { get; init; }
    /// <summary>The guided-authoring assist draft. Built-in 8,192.</summary>
    [JsonPropertyName("authoring_assist")] public required uint AuthoringAssist { get; init; }
}

/// <summary><c>GET /v1/max-tokens</c>, and what <c>POST /v1/max-tokens</c>
/// answers with: the effective budgets — FLATTENED, the same eight fields
/// <see cref="MaxTokensBudgets"/> carries — plus where they come from.
/// <see cref="ToBudgets"/> lifts them back into a POST body, so a
/// read-modify-replace is
/// <c>(await GetMaxTokensAsync()).ToBudgets() with { TurnCompletion = 8192 }</c>.</summary>
public sealed record MaxTokensResponse
{
    [JsonPropertyName("turn_completion")] public uint TurnCompletion { get; init; }
    [JsonPropertyName("query_expansion")] public uint QueryExpansion { get; init; }
    [JsonPropertyName("complete_default")] public uint CompleteDefault { get; init; }
    [JsonPropertyName("healthai_probe")] public uint HealthaiProbe { get; init; }
    [JsonPropertyName("hierarchy_classifier")] public uint HierarchyClassifier { get; init; }
    [JsonPropertyName("hierarchy_intent")] public uint HierarchyIntent { get; init; }
    [JsonPropertyName("runbook_advisory")] public uint RunbookAdvisory { get; init; }
    [JsonPropertyName("authoring_assist")] public uint AuthoringAssist { get; init; }
    /// <summary><c>tenant</c> after the tenant replaced the set through the
    /// API; <c>environment</c> while the process defaults (env vars over
    /// built-ins) apply.</summary>
    [JsonPropertyName("source")] public required string Source { get; init; }
    /// <summary>RFC 3339 instant of the tenant's last replacement; null when
    /// <see cref="Source"/> is <c>environment</c>.</summary>
    [JsonPropertyName("updated_at")] public string? UpdatedAt { get; init; }

    /// <summary>The eight budgets as a <c>POST /v1/max-tokens</c> body —
    /// the wire shape round-trips; <see cref="Source"/> and
    /// <see cref="UpdatedAt"/> are not part of it.</summary>
    public MaxTokensBudgets ToBudgets() => new()
    {
        TurnCompletion = TurnCompletion,
        QueryExpansion = QueryExpansion,
        CompleteDefault = CompleteDefault,
        HealthaiProbe = HealthaiProbe,
        HierarchyClassifier = HierarchyClassifier,
        HierarchyIntent = HierarchyIntent,
        RunbookAdvisory = RunbookAdvisory,
        AuthoringAssist = AuthoringAssist,
    };
}

// -- sessions + turns -------------------------------------------------

public sealed record SessionCreated
{
    [JsonPropertyName("session_id")] public required string SessionId { get; init; }
    /// <summary>The pinned name@version this session will use for every turn.</summary>
    [JsonPropertyName("runbook_ref")] public required string RunbookRef { get; init; }
    /// <summary>Collections the caller's access level/compartments permit —
    /// the least-privilege echo.</summary>
    [JsonPropertyName("permitted_collections")] public IReadOnlyList<string> PermittedCollections { get; init; } = [];
}

/// <summary>API-level model override — honored only under the runbook's
/// <c>models.allowOverrides</c> policy; a disallowed override draws the
/// typed <see cref="ForbiddenException"/>, never a silent downgrade.</summary>
public sealed record ModelOverride
{
    [JsonPropertyName("provider")] public string? Provider { get; init; }
    [JsonPropertyName("model")] public string? Model { get; init; }
    /// <summary>fast | capable | frontier.</summary>
    [JsonPropertyName("tier")] public string? Tier { get; init; }
}

/// <summary>One retrieval turn's request.</summary>
public sealed record TurnRequest
{
    [JsonPropertyName("query")] public required string Query { get; init; }
    /// <summary>Null = runbook default.</summary>
    [JsonPropertyName("top_k")] public uint? TopK { get; init; }
    /// <summary>Run the runbook's completion step (when the spec declares one).</summary>
    [JsonPropertyName("complete")] public bool? Complete { get; init; }
    [JsonPropertyName("model_override")] public ModelOverride? ModelOverride { get; init; }
    /// <summary>Run this turn through a named research profile — an evidence
    /// hierarchy declared on the runbook. Null (the default) is the
    /// legacy single-layer document path: the key is omitted entirely, so an
    /// existing caller's request bytes are unchanged.</summary>
    [JsonPropertyName("research_profile")] public string? ResearchProfile { get; init; }
}

public sealed record TurnHit
{
    /// <summary>Which collection this hit came from.</summary>
    [JsonPropertyName("collection")] public required string Collection { get; init; }
    [JsonPropertyName("chunk_id")] public required string ChunkId { get; init; }
    [JsonPropertyName("source_id")] public required string SourceId { get; init; }
    /// <summary>The logical path — which document answered this turn.</summary>
    [JsonPropertyName("source_path")] public required string SourcePath { get; init; }
    [JsonPropertyName("source_content_hash")] public required string SourceContentHash { get; init; }
    [JsonPropertyName("text")] public required string Text { get; init; }
    [JsonPropertyName("score")] public double Score { get; init; }
}

public sealed record CollectionEnvelope
{
    [JsonPropertyName("collection")] public required string Collection { get; init; }
    [JsonPropertyName("envelope")] public required ProvenanceEnvelope Envelope { get; init; }
}

/// <summary>Deterministic turn-verification outcome (quotes resolve in
/// served text, citations name served content). Violations are prefixed
/// "quote: " / "citation: "; non-empty final <see cref="Violations"/> mean
/// the answer stands UNVERIFIED after the retry budget.</summary>
public sealed record TurnVerification
{
    /// <summary>Which checks ran (quotes, citations).</summary>
    [JsonPropertyName("checks")] public IReadOnlyList<string> Checks { get; init; } = [];
    /// <summary>Corrective completions actually spent (each is a paid call).</summary>
    [JsonPropertyName("retries")] public uint Retries { get; init; }
    [JsonPropertyName("first_pass_violations")] public IReadOnlyList<string> FirstPassViolations { get; init; } = [];
    /// <summary>Violations remaining on the FINAL answer (empty = verified).</summary>
    [JsonPropertyName("violations")] public IReadOnlyList<string> Violations { get; init; } = [];
}

public sealed record TurnCompletion
{
    [JsonPropertyName("provider")] public required string Provider { get; init; }
    [JsonPropertyName("model")] public required string Model { get; init; }
    /// <summary>Whether an API model override decided the provider/model.</summary>
    [JsonPropertyName("was_override")] public bool WasOverride { get; init; }
    [JsonPropertyName("text")] public required string Text { get; init; }
    /// <summary>Token totals across ALL completions this turn paid for,
    /// verification retries included.</summary>
    [JsonPropertyName("input_tokens")] public ulong InputTokens { get; init; }
    [JsonPropertyName("output_tokens")] public ulong OutputTokens { get; init; }
    [JsonPropertyName("verification")] public TurnVerification? Verification { get; init; }
}

/// <summary>What one evidence layer produced.</summary>
public sealed record LayerOutcome
{
    [JsonPropertyName("layer")] public required string Layer { get; init; }
    /// <summary>supporting | primary | controlling.</summary>
    [JsonPropertyName("role")] public required string Role { get; init; }
    /// <summary>required | optional | fallback.</summary>
    [JsonPropertyName("requirement")] public required string Requirement { get; init; }
    /// <summary>document_hits | complete_table | count | fact_slice | refusal.</summary>
    [JsonPropertyName("block")] public required string Block { get; init; }
    /// <summary>The sealed artifact this layer's block was cited from, when it
    /// sealed one.</summary>
    [JsonPropertyName("evidence_id")] public string? EvidenceId { get; init; }
    /// <summary>Whether an answer may make a completeness claim on THIS layer.
    /// Document hits are always false: retrieval returns what it found, never
    /// a proof that nothing else exists.</summary>
    [JsonPropertyName("supports_completeness")] public bool SupportsCompleteness { get; init; }
    /// <summary>Set when <see cref="Block"/> is <c>refusal</c> — a layer that
    /// declined still ran, and the turn still returned 200.</summary>
    [JsonPropertyName("refusal_code")] public string? RefusalCode { get; init; }
    [JsonPropertyName("elapsed_ms")] public ulong ElapsedMs { get; init; }
}

/// <summary>Why the model saw what it saw. About the DECISION, not
/// the content: which profile ran, which layers answered, which refused,
/// whether a completeness claim was permissible at all. No evidence rows
/// appear here — resolve those through <see cref="IEvidencePlane"/>.</summary>
public sealed record EvidenceHierarchyDecision
{
    [JsonPropertyName("profile")] public required string Profile { get; init; }
    [JsonPropertyName("intent_kind")] public string? IntentKind { get; init; }
    /// <summary>True when the caller supplied the intent rather than a model
    /// producing it, so a keyless test result never reads as a planner
    /// result.</summary>
    [JsonPropertyName("intent_explicit")] public bool IntentExplicit { get; init; }
    [JsonPropertyName("layers")] public IReadOnlyList<LayerOutcome> Layers { get; init; } = [];
    /// <summary>Whether ANY layer could support a completeness claim.</summary>
    [JsonPropertyName("completeness_available")] public bool CompletenessAvailable { get; init; }
    /// <summary>Cross-layer conflicts preserved for disclosure rather than
    /// resolved away.</summary>
    [JsonPropertyName("disclosed_conflicts")] public uint DisclosedConflicts { get; init; }
    [JsonPropertyName("conflicts_policy")] public required string ConflictsPolicy { get; init; }
}

public sealed record TurnResult
{
    [JsonPropertyName("session_id")] public required string SessionId { get; init; }
    [JsonPropertyName("ordinal")] public uint Ordinal { get; init; }
    /// <summary>Collections actually searched (post access filtering).</summary>
    [JsonPropertyName("collections_searched")] public IReadOnlyList<string> CollectionsSearched { get; init; } = [];
    /// <summary>Permitted collections skipped for lack of an active index.</summary>
    [JsonPropertyName("skipped")] public IReadOnlyList<string> Skipped { get; init; } = [];
    [JsonPropertyName("hits")] public IReadOnlyList<TurnHit> Hits { get; init; } = [];
    [JsonPropertyName("envelopes")] public IReadOnlyList<CollectionEnvelope> Envelopes { get; init; } = [];
    [JsonPropertyName("completion")] public TurnCompletion? Completion { get; init; }
    /// <summary>Present only when a research profile ran. A legacy
    /// turn's response carries no <c>hierarchy</c> key at all.</summary>
    [JsonPropertyName("hierarchy")] public EvidenceHierarchyDecision? Hierarchy { get; init; }
}

/// <summary>One progress event on the streaming turn plane. The wire tags
/// each event with <see cref="Stage"/> (retrieval | merge | model |
/// completion | verify, plus this hierarchy stages profile |
/// layer_start | layer_source | layer_complete | coverage | compose) and
/// flattens the stage's fields beside it, so this record carries the union
/// as optionals — and an unrecognized stage string decodes fine
/// (forward-compat: a newer server may add stages this build cannot name;
/// progress is informational).</summary>
public sealed record TurnProgressEvent
{
    [JsonPropertyName("stage")] public required string Stage { get; init; }
    /// <summary>retrieval: the collection searched.</summary>
    [JsonPropertyName("collection")] public string? Collection { get; init; }
    /// <summary>retrieval / merge: hit count.</summary>
    [JsonPropertyName("hits")] public uint? Hits { get; init; }
    /// <summary>retrieval: the collection had no active index.</summary>
    [JsonPropertyName("skipped")] public bool? Skipped { get; init; }
    /// <summary>model / completion / expansion: provider family.
    /// layer_source: which evidence provider served the source
    /// (documents | facts | matrix).</summary>
    [JsonPropertyName("provider")] public string? Provider { get; init; }
    [JsonPropertyName("model")] public string? Model { get; init; }
    [JsonPropertyName("tier")] public string? Tier { get; init; }
    [JsonPropertyName("was_override")] public bool? WasOverride { get; init; }
    /// <summary>completion / verify: 0 for the first answer, 1.. for retries.</summary>
    [JsonPropertyName("attempt")] public uint? Attempt { get; init; }
    /// <summary>completion / expansion: tokens the paid call consumed.</summary>
    [JsonPropertyName("input_tokens")] public ulong? InputTokens { get; init; }
    /// <summary>completion / expansion: tokens the paid call produced.</summary>
    [JsonPropertyName("output_tokens")] public ulong? OutputTokens { get; init; }
    /// <summary>verify: which checks ran.</summary>
    [JsonPropertyName("checks")] public IReadOnlyList<string>? Checks { get; init; }
    /// <summary>verify: violation count on this attempt.</summary>
    [JsonPropertyName("violations")] public uint? Violations { get; init; }

    // selection / expansion (server-side since 2026-08-25). These were absent
    // until 2026-08-29: the events decoded with only `stage` populated and
    // everything that made them worth emitting silently dropped. A progress
    // type that names some stages and quietly discards others is worse than a
    // permissive one, because a caller reading `Probed == null` cannot tell
    // "the server did not send it" from "this client cannot see it".

    /// <summary>selection: permitted collections probed with the original
    /// query.</summary>
    [JsonPropertyName("probed")] public uint? Probed { get; init; }
    /// <summary>selection: how many of them won the deep, expanded search.
    /// The rest still contribute their probe pools to the merge — selection
    /// decides where the deep search is spent, never what a turn may
    /// cite.</summary>
    [JsonPropertyName("selected")] public uint? Selected { get; init; }
    /// <summary>selection: the selected collections, in the runbook's
    /// order.</summary>
    [JsonPropertyName("collections")] public IReadOnlyList<string>? Collections { get; init; }
    /// <summary>expansion: the accepted lexical variants from the runbook's
    /// modelQueryExpansion step. Possibly empty, in which case the original
    /// query searched alone — which is why an empty list and a null are
    /// different readings. <see cref="Provider"/>, <see cref="Model"/>,
    /// <see cref="InputTokens"/> and <see cref="OutputTokens"/> carry the
    /// paid call this stage reports.</summary>
    [JsonPropertyName("terms")] public IReadOnlyList<string>? Terms { get; init; }

    // The hierarchy stages. Every one of these is absent on a legacy
    // turn's events, verify's `layer` included — which is what keeps an
    // existing turn's SSE sequence byte-identical.

    /// <summary>layer_start / layer_source / layer_complete: the layer's
    /// name. Also on verify, naming which layer's evidence the check ran
    /// against.</summary>
    [JsonPropertyName("layer")] public string? Layer { get; init; }
    /// <summary>layer_start: supporting | primary | controlling.</summary>
    [JsonPropertyName("role")] public string? Role { get; init; }
    /// <summary>layer_start: required | optional | fallback.</summary>
    [JsonPropertyName("requirement")] public string? Requirement { get; init; }
    /// <summary>layer_source: the source within the layer that answered
    /// (<see cref="Provider"/> carries which evidence provider served it).</summary>
    [JsonPropertyName("source")] public string? Source { get; init; }
    /// <summary>layer_complete: document_hits | complete_table | count |
    /// fact_slice | refusal.</summary>
    [JsonPropertyName("block")] public string? Block { get; init; }
    /// <summary>layer_complete: whether this layer can carry a completeness
    /// claim.</summary>
    [JsonPropertyName("supports_completeness")] public bool? SupportsCompleteness { get; init; }
    /// <summary>layer_complete: set when the layer refused.</summary>
    [JsonPropertyName("refusal_code")] public string? RefusalCode { get; init; }
    /// <summary>layer_complete: wall time this layer spent.</summary>
    [JsonPropertyName("elapsed_ms")] public ulong? ElapsedMs { get; init; }
    /// <summary>profile: the resolved research profile's name.</summary>
    [JsonPropertyName("profile")] public string? Profile { get; init; }
    /// <summary>profile: the layer names about to execute, in order.</summary>
    [JsonPropertyName("layers")] public IReadOnlyList<string>? Layers { get; init; }
    /// <summary>profile: the classified question intent.</summary>
    [JsonPropertyName("intent_kind")] public string? IntentKind { get; init; }
    /// <summary>profile: true when the caller supplied the intent rather than
    /// a model producing it.</summary>
    [JsonPropertyName("intent_explicit")] public bool? IntentExplicit { get; init; }
    /// <summary>coverage: whether ANY layer can support a completeness
    /// claim.</summary>
    [JsonPropertyName("completeness_available")] public bool? CompletenessAvailable { get; init; }
    /// <summary>coverage: cross-layer conflicts preserved for disclosure.</summary>
    [JsonPropertyName("disclosed_conflicts")] public uint? DisclosedConflicts { get; init; }
    /// <summary>compose: layers whose blocks reached the model's context.</summary>
    [JsonPropertyName("layers_used")] public uint? LayersUsed { get; init; }
    /// <summary>compose: characters of context the blocks occupied.</summary>
    [JsonPropertyName("context_chars")] public uint? ContextChars { get; init; }
    /// <summary>compose: layers dropped for want of budget. A
    /// preserveCompleteResult layer is dropped WHOLE or kept whole — half a
    /// table is not a smaller true answer, it is a false one.</summary>
    [JsonPropertyName("layers_dropped")] public IReadOnlyList<string>? LayersDropped { get; init; }
}

/// <summary>One item on the streaming turn plane: N <see cref="Progress"/>
/// events at real stage boundaries, then exactly one <see cref="Done"/>
/// carrying the full <see cref="TurnResult"/>. A server-side failure
/// mid-stream throws the typed error (decoded through the standard problem
/// registry) and ends the enumeration.</summary>
public abstract record TurnStreamEvent
{
    private TurnStreamEvent() { }

    public sealed record Progress(TurnProgressEvent Event) : TurnStreamEvent;

    public sealed record Done(TurnResult Response) : TurnStreamEvent;
}

public sealed record SessionTurn
{
    [JsonPropertyName("ordinal")] public uint Ordinal { get; init; }
    [JsonPropertyName("query")] public required string Query { get; init; }
    [JsonPropertyName("collections_searched")] public IReadOnlyList<string> CollectionsSearched { get; init; } = [];
    /// <summary>Stored transcript rows are JSON documents.</summary>
    [JsonPropertyName("hits")] public JsonElement? Hits { get; init; }
    [JsonPropertyName("envelope")] public JsonElement? Envelope { get; init; }
    [JsonPropertyName("completion")] public JsonElement? Completion { get; init; }
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
}

/// <summary>The session envelope + stored turn transcript.</summary>
public sealed record Session
{
    [JsonPropertyName("session_id")] public required string SessionId { get; init; }
    [JsonPropertyName("uid")] public required string Uid { get; init; }
    [JsonPropertyName("runbook_ref")] public required string RunbookRef { get; init; }
    [JsonPropertyName("access_level")] public int AccessLevel { get; init; }
    [JsonPropertyName("compartments")] public IReadOnlyList<string> Compartments { get; init; } = [];
    /// <summary>open | closed | expired.</summary>
    [JsonPropertyName("state")] public required string State { get; init; }
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
    [JsonPropertyName("turns")] public IReadOnlyList<SessionTurn> Turns { get; init; } = [];
}

// -- access tokens (mgmt) -----------------------------------------------

/// <summary>A freshly minted capability JWT. The token material is returned
/// ONCE and never persisted server-side — treat it as a secret.</summary>
public sealed record IssuedToken
{
    [JsonPropertyName("token")] public required string Token { get; init; }
    /// <summary>Token id — the audit/revocation key.</summary>
    [JsonPropertyName("jti")] public required string Jti { get; init; }
    [JsonPropertyName("expires_at")] public required string ExpiresAt { get; init; }
}

/// <summary>One issued capability token (audit view — never the material).</summary>
public sealed record TokenInfo
{
    [JsonPropertyName("jti")] public required string Jti { get; init; }
    [JsonPropertyName("uid")] public required string Uid { get; init; }
    [JsonPropertyName("access_level")] public int AccessLevel { get; init; }
    [JsonPropertyName("compartments")] public IReadOnlyList<string> Compartments { get; init; } = [];
    [JsonPropertyName("scopes")] public IReadOnlyList<string> Scopes { get; init; } = [];
    [JsonPropertyName("runbook_refs")] public IReadOnlyList<string>? RunbookRefs { get; init; }
    [JsonPropertyName("issued_by")] public required string IssuedBy { get; init; }
    [JsonPropertyName("issued_at")] public required string IssuedAt { get; init; }
    [JsonPropertyName("expires_at")] public required string ExpiresAt { get; init; }
    [JsonPropertyName("revoked_at")] public string? RevokedAt { get; init; }
}

public sealed record TokenRevocation
{
    [JsonPropertyName("jti")] public required string Jti { get; init; }
    [JsonPropertyName("revoked")] public bool Revoked { get; init; }
    /// <summary>The deny-list is only consulted when the server enables the
    /// revocation check.</summary>
    [JsonPropertyName("revocation_check_enabled")] public bool RevocationCheckEnabled { get; init; }
}

// -- reports (mgmt) ----------------------------------------------------

public sealed record UsageRow
{
    /// <summary>The grouping key value (a uid, session id, runbook ref, or
    /// collection id).</summary>
    [JsonPropertyName("key")] public required string Key { get; init; }
    [JsonPropertyName("interactions")] public long Interactions { get; init; }
    [JsonPropertyName("turns")] public long Turns { get; init; }
    [JsonPropertyName("completion_input_tokens")] public long CompletionInputTokens { get; init; }
    [JsonPropertyName("completion_output_tokens")] public long CompletionOutputTokens { get; init; }
    [JsonPropertyName("avg_latency_ms")] public double? AvgLatencyMs { get; init; }
}

public sealed record UsageReport
{
    /// <summary>uid | session | runbook | collection.</summary>
    [JsonPropertyName("group_by")] public required string GroupBy { get; init; }
    [JsonPropertyName("from")] public string? From { get; init; }
    [JsonPropertyName("to")] public string? To { get; init; }
    [JsonPropertyName("rows")] public IReadOnlyList<UsageRow> Rows { get; init; } = [];
}

public sealed record AuditEntry
{
    [JsonPropertyName("id")] public required string Id { get; init; }
    [JsonPropertyName("uid")] public required string Uid { get; init; }
    [JsonPropertyName("session_id")] public string? SessionId { get; init; }
    [JsonPropertyName("request_id")] public string? RequestId { get; init; }
    [JsonPropertyName("plane")] public required string Plane { get; init; }
    [JsonPropertyName("method")] public required string Method { get; init; }
    [JsonPropertyName("runbook_ref")] public string? RunbookRef { get; init; }
    [JsonPropertyName("token_jti")] public string? TokenJti { get; init; }
    [JsonPropertyName("status")] public int? Status { get; init; }
    [JsonPropertyName("latency_ms")] public int? LatencyMs { get; init; }
    [JsonPropertyName("request")] public JsonElement? Request { get; init; }
    [JsonPropertyName("response")] public JsonElement? Response { get; init; }
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
}

public sealed record AuditReport
{
    [JsonPropertyName("entries")] public IReadOnlyList<AuditEntry> Entries { get; init; } = [];
    /// <summary>Keyset cursor for the next (older) page: pass it back as
    /// <c>before</c>. Absence means the trail is exhausted.</summary>
    [JsonPropertyName("next_before")] public string? NextBefore { get; init; }
}

/// <summary>Model-spend token rollup (dollar pricing lives upstream).</summary>
public sealed record CostRow
{
    [JsonPropertyName("provider")] public required string Provider { get; init; }
    [JsonPropertyName("model")] public required string Model { get; init; }
    [JsonPropertyName("turns")] public long Turns { get; init; }
    [JsonPropertyName("overridden_turns")] public long OverriddenTurns { get; init; }
    [JsonPropertyName("input_tokens")] public long InputTokens { get; init; }
    [JsonPropertyName("output_tokens")] public long OutputTokens { get; init; }
}

public sealed record CostReport
{
    [JsonPropertyName("from")] public string? From { get; init; }
    [JsonPropertyName("to")] public string? To { get; init; }
    [JsonPropertyName("rows")] public IReadOnlyList<CostRow> Rows { get; init; } = [];
}

public sealed record TimeseriesBucket
{
    /// <summary>Bucket start, RFC 3339 UTC.</summary>
    [JsonPropertyName("bucket")] public required string Bucket { get; init; }
    [JsonPropertyName("requests")] public long Requests { get; init; }
    [JsonPropertyName("errors_4xx")] public long Errors4xx { get; init; }
    [JsonPropertyName("errors_5xx")] public long Errors5xx { get; init; }
    [JsonPropertyName("p50_latency_ms")] public double? P50LatencyMs { get; init; }
    [JsonPropertyName("p95_latency_ms")] public double? P95LatencyMs { get; init; }
}

public sealed record TimeseriesReport
{
    /// <summary>1h | 24h | 7d | 30d.</summary>
    [JsonPropertyName("window")] public required string Window { get; init; }
    [JsonPropertyName("bucket_seconds")] public long BucketSeconds { get; init; }
    /// <summary>rest | grpc when the query filtered by plane.</summary>
    [JsonPropertyName("plane")] public string? Plane { get; init; }
    [JsonPropertyName("buckets")] public IReadOnlyList<TimeseriesBucket> Buckets { get; init; } = [];
}

public sealed record EndpointRow
{
    [JsonPropertyName("method")] public required string Method { get; init; }
    [JsonPropertyName("requests")] public long Requests { get; init; }
    /// <summary>Fraction of requests with status &gt;= 400.</summary>
    [JsonPropertyName("error_rate")] public double ErrorRate { get; init; }
    [JsonPropertyName("avg_latency_ms")] public double? AvgLatencyMs { get; init; }
    [JsonPropertyName("p95_latency_ms")] public double? P95LatencyMs { get; init; }
}

public sealed record EndpointsReport
{
    [JsonPropertyName("window")] public required string Window { get; init; }
    [JsonPropertyName("rows")] public IReadOnlyList<EndpointRow> Rows { get; init; } = [];
}

public sealed record RunbookRunsRow
{
    [JsonPropertyName("state")] public required string State { get; init; }
    [JsonPropertyName("runs")] public long Runs { get; init; }
    [JsonPropertyName("avg_wall_ms")] public double? AvgWallMs { get; init; }
}

public sealed record RunbookStepsRow
{
    [JsonPropertyName("state")] public required string State { get; init; }
    [JsonPropertyName("steps")] public long Steps { get; init; }
}

public sealed record RunbookReport
{
    [JsonPropertyName("window")] public required string Window { get; init; }
    [JsonPropertyName("runs")] public IReadOnlyList<RunbookRunsRow> Runs { get; init; } = [];
    [JsonPropertyName("steps")] public IReadOnlyList<RunbookStepsRow> Steps { get; init; } = [];
}

public sealed record SessionsBucket
{
    [JsonPropertyName("bucket")] public required string Bucket { get; init; }
    [JsonPropertyName("sessions_opened")] public long SessionsOpened { get; init; }
    [JsonPropertyName("turns")] public long Turns { get; init; }
    /// <summary>Distinct uids that took a turn in the bucket.</summary>
    [JsonPropertyName("active_uids")] public long ActiveUids { get; init; }
}

public sealed record SessionsReport
{
    [JsonPropertyName("window")] public required string Window { get; init; }
    [JsonPropertyName("bucket_seconds")] public long BucketSeconds { get; init; }
    [JsonPropertyName("buckets")] public IReadOnlyList<SessionsBucket> Buckets { get; init; } = [];
}

/// <summary>One layer's aggregate behaviour over the report window
///.</summary>
public sealed record EvidenceLayerStats
{
    [JsonPropertyName("profile")] public required string Profile { get; init; }
    [JsonPropertyName("layer")] public required string Layer { get; init; }
    [JsonPropertyName("turns")] public long Turns { get; init; }
    /// <summary>Turns where this layer refused.</summary>
    [JsonPropertyName("refusals")] public long Refusals { get; init; }
    /// <summary>Turns where this layer could support a completeness claim.</summary>
    [JsonPropertyName("complete")] public long Complete { get; init; }
    /// <summary>Refusal codes seen, most frequent first.</summary>
    [JsonPropertyName("refusal_codes")] public IReadOnlyList<string> RefusalCodes { get; init; } = [];
    [JsonPropertyName("p50_ms")] public long P50Ms { get; init; }
    [JsonPropertyName("p95_ms")] public long P95Ms { get; init; }
}

/// <summary>How the evidence hierarchy actually behaved.
///
/// The operational question this answers is "which layer is quietly
/// refusing?" — a layer refusing on most turns is either misconfigured or
/// pointed at something that is down, and either way the answers being served
/// are thinner than the runbook claims while every one of those turns still
/// returned 200.</summary>
public sealed record EvidenceReport
{
    [JsonPropertyName("window")] public required string Window { get; init; }
    /// <summary>Turns that ran a research profile.</summary>
    [JsonPropertyName("hierarchy_turns")] public long HierarchyTurns { get; init; }
    /// <summary>Turns on the legacy document path.</summary>
    [JsonPropertyName("legacy_turns")] public long LegacyTurns { get; init; }
    /// <summary>Hierarchy turns where at least one layer could support a
    /// completeness claim.</summary>
    [JsonPropertyName("completeness_available")] public long CompletenessAvailable { get; init; }
    [JsonPropertyName("layers")] public IReadOnlyList<EvidenceLayerStats> Layers { get; init; } = [];
}

/// <summary>One data view a runbook declares over Munarium Matrix.</summary>
public sealed record MatrixDataView
{
    [JsonPropertyName("runbook_ref")] public required string RunbookRef { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    /// <summary>The named, pre-declared query contract it executes.</summary>
    [JsonPropertyName("contract")] public required string Contract { get; init; }
    [JsonPropertyName("access_level")] public int AccessLevel { get; init; }
}

/// <summary>Munarium Matrix's health as the server sees it.</summary>
public sealed record MatrixReport
{
    /// <summary>False when the server has no Matrix base URL — the plane is
    /// not wired, which is different from wired-and-failing and must not read
    /// the same.</summary>
    [JsonPropertyName("configured")] public bool Configured { get; init; }
    /// <summary>Per-INSTANCE circuit-breaker state. Deliberately not per
    /// tenant: the breaker is shared, so a per-tenant reading would report a
    /// fact that does not exist.</summary>
    [JsonPropertyName("circuit_open")] public bool CircuitOpen { get; init; }
    [JsonPropertyName("consecutive_failures")] public ulong ConsecutiveFailures { get; init; }
    /// <summary>Data views declared across the tenant's applied runbooks.</summary>
    [JsonPropertyName("data_views")] public IReadOnlyList<MatrixDataView> DataViews { get; init; } = [];
}

// -- guided authoring -------------------------------------------------------

/// <summary>One application pattern, summarized for the catalog listing.</summary>
public sealed record PatternSummary
{
    [JsonPropertyName("id")] public required string Id { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("description")] public required string Description { get; init; }
    /// <summary>The committed exemplar runbook to start from.</summary>
    [JsonPropertyName("start_from")] public required string StartFrom { get; init; }
    /// <summary>What this pattern is strongest at, and the failure mode to design against.</summary>
    [JsonPropertyName("guidance")] public required string Guidance { get; init; }
    [JsonPropertyName("has_completion")] public bool HasCompletion { get; init; }
}

public sealed record NamedYaml
{
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("yaml")] public required string Yaml { get; init; }
}

public sealed record PatternDetail
{
    [JsonPropertyName("id")] public required string Id { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("description")] public required string Description { get; init; }
    [JsonPropertyName("start_from")] public required string StartFrom { get; init; }
    /// <summary>What this pattern is strongest at, and the failure mode to design against.</summary>
    [JsonPropertyName("guidance")] public required string Guidance { get; init; }
    [JsonPropertyName("has_completion")] public bool HasCompletion { get; init; }
    /// <summary>Design notes the deterministic validator cannot police.</summary>
    [JsonPropertyName("decision_notes")] public IReadOnlyList<string> DecisionNotes { get; init; } = [];
    /// <summary>The exemplar runbook, verbatim.</summary>
    [JsonPropertyName("runbook_yaml")] public required string RunbookYaml { get; init; }
    /// <summary>The exemplar's shape dependencies, verbatim.</summary>
    [JsonPropertyName("shapes")] public IReadOnlyList<NamedYaml> Shapes { get; init; } = [];
}

public sealed record InterviewQuestion
{
    [JsonPropertyName("id")] public required string Id { get; init; }
    [JsonPropertyName("prompt")] public required string Prompt { get; init; }
    [JsonPropertyName("guidance")] public required string Guidance { get; init; }
    /// <summary>string | text | int | bool | enum | areas | fields | map.</summary>
    [JsonPropertyName("kind")] public required string Kind { get; init; }
    [JsonPropertyName("required")] public bool Required { get; init; }
    [JsonPropertyName("default")] public JsonElement? Default { get; init; }
    [JsonPropertyName("choices")] public IReadOnlyList<string> Choices { get; init; } = [];
    /// <summary>Documentation of the slot this answer lands in.</summary>
    [JsonPropertyName("maps_to")] public required string MapsTo { get; init; }
}

public sealed record InterviewSection
{
    [JsonPropertyName("id")] public required string Id { get; init; }
    [JsonPropertyName("title")] public required string Title { get; init; }
    /// <summary>The document section that teaches this decision in full.</summary>
    [JsonPropertyName("doc_ref")] public required string DocRef { get; init; }
    [JsonPropertyName("questions")] public IReadOnlyList<InterviewQuestion> Questions { get; init; } = [];
}

public sealed record DraftDocument
{
    /// <summary>Path within the set, e.g. "runbooks/&lt;name&gt;.yaml".</summary>
    [JsonPropertyName("path")] public required string Path { get; init; }
    /// <summary>Shape | Runbook.</summary>
    [JsonPropertyName("kind")] public required string Kind { get; init; }
    [JsonPropertyName("yaml")] public required string Yaml { get; init; }
    /// <summary>sha256 hex of the YAML bytes.</summary>
    [JsonPropertyName("sha256")] public required string Sha256 { get; init; }
}

public sealed record DocumentFindings
{
    [JsonPropertyName("path")] public required string Path { get; init; }
    [JsonPropertyName("findings")] public IReadOnlyList<ValidationFinding> Findings { get; init; } = [];
}

public sealed record DraftValidation
{
    /// <summary>False when any error-severity finding exists in the set.</summary>
    [JsonPropertyName("valid")] public bool Valid { get; init; }
    /// <summary>Per-document findings (parse + the document's validator).</summary>
    [JsonPropertyName("documents")] public IReadOnlyList<DocumentFindings> Documents { get; init; } = [];
    /// <summary>Cross-document findings (set.* codes).</summary>
    [JsonPropertyName("set_findings")] public IReadOnlyList<ValidationFinding> SetFindings { get; init; } = [];
    /// <summary>What still needs answering.</summary>
    [JsonPropertyName("todos")] public IReadOnlyList<string> Todos { get; init; } = [];
}

public sealed record DraftSummary
{
    [JsonPropertyName("draft_id")] public required string DraftId { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    /// <summary>interview | drafted | validated | exported (progress display
    /// only — export and apply always re-validate inline).</summary>
    [JsonPropertyName("state")] public required string State { get; init; }
    [JsonPropertyName("pattern_id")] public string? PatternId { get; init; }
    [JsonPropertyName("created_by")] public required string CreatedBy { get; init; }
    [JsonPropertyName("updated_at")] public required string UpdatedAt { get; init; }
}

public sealed record Draft
{
    [JsonPropertyName("draft_id")] public required string DraftId { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("state")] public required string State { get; init; }
    [JsonPropertyName("pattern_id")] public string? PatternId { get; init; }
    /// <summary>Flat map keyed by interview question id.</summary>
    [JsonPropertyName("answers")] public JsonElement? Answers { get; init; }
    [JsonPropertyName("interview")] public IReadOnlyList<InterviewSection> Interview { get; init; } = [];
    [JsonPropertyName("documents")] public IReadOnlyList<DraftDocument> Documents { get; init; } = [];
    /// <summary>Fresh validation of the current documents.</summary>
    [JsonPropertyName("validation")] public DraftValidation? Validation { get; init; }
    [JsonPropertyName("todos")] public IReadOnlyList<string> Todos { get; init; } = [];
    [JsonPropertyName("assist_note")] public string? AssistNote { get; init; }
    [JsonPropertyName("created_by")] public required string CreatedBy { get; init; }
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
    [JsonPropertyName("updated_at")] public required string UpdatedAt { get; init; }
}

public sealed record DraftDeletion
{
    [JsonPropertyName("draft_id")] public required string DraftId { get; init; }
    /// <summary>Always "deleted" (soft; the row is retained).</summary>
    [JsonPropertyName("status")] public required string Status { get; init; }
}

/// <summary>The AI-assisted drafting pass result. Assist NEVER fails the
/// request: a degraded pass (no provider, budget, parse failure) sets
/// <see cref="AssistNote"/> instead.</summary>
public sealed record AssistResult
{
    /// <summary>The documents after the pass (unchanged when degraded).</summary>
    [JsonPropertyName("documents")] public IReadOnlyList<DraftDocument> Documents { get; init; } = [];
    [JsonPropertyName("suggestions")] public IReadOnlyList<Suggestion> Suggestions { get; init; } = [];
    [JsonPropertyName("assist_note")] public string? AssistNote { get; init; }
    [JsonPropertyName("validation")] public required DraftValidation Validation { get; init; }
}

public sealed record BundleTool
{
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("version")] public required string Version { get; init; }
}

public sealed record BundleValidation
{
    [JsonPropertyName("valid")] public bool Valid { get; init; }
    [JsonPropertyName("errors")] public ulong Errors { get; init; }
    [JsonPropertyName("warns")] public ulong Warns { get; init; }
    [JsonPropertyName("infos")] public ulong Infos { get; init; }
}

/// <summary>The export bundle: self-contained, hash-manifested, applied to
/// any instance via /v1/shapes + /v1/runbooks in <see cref="ApplyOrder"/>.
/// <see cref="ManifestHash"/> = sha256 over the byte-sorted "path\0hash\n"
/// lines.</summary>
public sealed record DraftBundle
{
    /// <summary>Always "MunariumAuthoringBundle".</summary>
    [JsonPropertyName("kind")] public required string Kind { get; init; }
    [JsonPropertyName("apiVersion")] public required string ApiVersion { get; init; }
    [JsonPropertyName("tool")] public required BundleTool Tool { get; init; }
    [JsonPropertyName("draft_id")] public required string DraftId { get; init; }
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("created_at")] public required string CreatedAt { get; init; }
    /// <summary>path → YAML, verbatim.</summary>
    [JsonPropertyName("files")] public required IReadOnlyDictionary<string, string> Files { get; init; }
    /// <summary>path → sha256 hex.</summary>
    [JsonPropertyName("hashes")] public required IReadOnlyDictionary<string, string> Hashes { get; init; }
    /// <summary>Shapes before runbooks.</summary>
    [JsonPropertyName("apply_order")] public IReadOnlyList<string> ApplyOrder { get; init; } = [];
    [JsonPropertyName("manifest_hash")] public required string ManifestHash { get; init; }
    [JsonPropertyName("validation")] public required BundleValidation Validation { get; init; }
}

public sealed record AppliedDoc
{
    [JsonPropertyName("path")] public required string Path { get; init; }
    /// <summary>Shape | Runbook.</summary>
    [JsonPropertyName("kind")] public required string Kind { get; init; }
    /// <summary>shape_ref or runbook_ref (name@version).</summary>
    [JsonPropertyName("ref")] public required string Ref { get; init; }
    /// <summary>sha256 of the applied YAML.</summary>
    [JsonPropertyName("yaml_hash")] public required string YamlHash { get; init; }
}

// -- meta -------------------------------------------------------------------

/// <summary>GET /version body (a meta route; unauthenticated). Handy for
/// asserting the <see cref="MunariumClient.TargetServerVersion"/> handshake.</summary>
public sealed record ServerVersionInfo
{
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("version")] public required string Version { get; init; }
}

// -- internal wire helper shapes (REST) -------------------------------------

internal sealed record FindingsResponse
{
    [JsonPropertyName("findings")] public required IReadOnlyList<StoredFinding> Findings { get; init; }
}

internal sealed record IngestBatchBody
{
    [JsonPropertyName("files")] public required IReadOnlyList<IngestFile> Files { get; init; }
}

internal sealed record IngestBatchResponse
{
    [JsonPropertyName("results")] public required IReadOnlyList<IngestResult> Results { get; init; }
}

internal sealed record BulkOpenBody
{
    [JsonPropertyName("files")] public required IReadOnlyList<BulkManifestEntry> Files { get; init; }
    [JsonPropertyName("label")] public string? Label { get; init; }
}

internal sealed record CollectionsResponse
{
    [JsonPropertyName("collections")] public required IReadOnlyList<Collection> Collections { get; init; }
}

internal sealed record CreateCollectionBody
{
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("shape_ref")] public required string ShapeRef { get; init; }
    [JsonPropertyName("access_level")] public int AccessLevel { get; init; }
    [JsonPropertyName("compartments")] public IReadOnlyList<string> Compartments { get; init; } = [];
    [JsonPropertyName("description")] public string? Description { get; init; }
}

internal sealed record RunbooksResponse
{
    [JsonPropertyName("runbooks")] public required IReadOnlyList<RunbookSummary> Runbooks { get; init; }
}

internal sealed record RemovalConfirmBody
{
    [JsonPropertyName("removal_id")] public required string RemovalId { get; init; }
}

internal sealed record ProviderListResponse
{
    [JsonPropertyName("providers")] public required IReadOnlyList<ProviderModels> Providers { get; init; }
}

internal sealed record IssueTokenBody
{
    [JsonPropertyName("uid")] public required string Uid { get; init; }
    [JsonPropertyName("access_level")] public int AccessLevel { get; init; }
    [JsonPropertyName("compartments")] public IReadOnlyList<string> Compartments { get; init; } = [];
    [JsonPropertyName("scopes")] public required IReadOnlyList<string> Scopes { get; init; }
    [JsonPropertyName("runbook_refs")] public IReadOnlyList<string>? RunbookRefs { get; init; }
    [JsonPropertyName("ttl_secs")] public ulong? TtlSecs { get; init; }
}

internal sealed record TokensResponse
{
    [JsonPropertyName("tokens")] public required IReadOnlyList<TokenInfo> Tokens { get; init; }
}

internal sealed record PatternsResponse
{
    [JsonPropertyName("patterns")] public required IReadOnlyList<PatternSummary> Patterns { get; init; }
}

internal sealed record CreateDraftBody
{
    [JsonPropertyName("name")] public required string Name { get; init; }
    [JsonPropertyName("pattern_id")] public string? PatternId { get; init; }
    [JsonPropertyName("seed_from_exemplar")] public bool SeedFromExemplar { get; init; }
}

internal sealed record DraftsResponse
{
    [JsonPropertyName("drafts")] public required IReadOnlyList<DraftSummary> Drafts { get; init; }
}

internal sealed record UpdateAnswersBody
{
    [JsonPropertyName("answers")] public required JsonElement Answers { get; init; }
    [JsonPropertyName("materialize")] public bool Materialize { get; init; } = true;
}

internal sealed record AssistBody
{
    [JsonPropertyName("description")] public string? Description { get; init; }
    [JsonPropertyName("instructions")] public string? Instructions { get; init; }
    [JsonPropertyName("provider")] public string? Provider { get; init; }
    [JsonPropertyName("model")] public string? Model { get; init; }
    [JsonPropertyName("tier")] public string? Tier { get; init; }
}

internal sealed record ApplyDraftResponse
{
    [JsonPropertyName("applied")] public required IReadOnlyList<AppliedDoc> Applied { get; init; }
}
