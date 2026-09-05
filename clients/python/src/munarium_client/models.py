# SPDX-License-Identifier: Apache-2.0
"""Typed wire models mirroring the server's `munarium-api-types` (the JSON
casing truth). ``extra="allow"`` keeps the client forward-compatible with
additive server fields."""

from __future__ import annotations

from typing import Any, Literal, TypeVar

from pydantic import BaseModel, ConfigDict, Field

ClaimType = Literal["fact", "update", "correction"]

M = TypeVar("M", bound=BaseModel)


def coerce(model_cls: type[M], value: M | dict[str, Any]) -> M:
    """Accept a model instance OR its dict shape (the plane methods take
    both) and return the model — the one coercion path every transport
    uses for ClaimInput / IngestFile / BulkManifestEntry / ModelOverride."""
    return value if isinstance(value, model_cls) else model_cls.model_validate(value)


ClaimStatus = Literal["accepted", "disputed"]
Provenance = Literal["witnessed", "backfilled", "repaired", "emergent", "coverage_repair"]
Severity = Literal["info", "warn", "block"]


class _Model(BaseModel):
    model_config = ConfigDict(extra="allow")


class GateFinding(_Model):
    rule_id: str
    severity: Severity
    message: str
    scope_path: str | None = None
    detail: Any | None = None


class ClaimOrigin(_Model):
    """Where a connector-originated claim came from. Absent on
    every model-extracted claim; provenance, never a gate input."""

    kind: str
    source_id: str
    mapping_version: str
    row_key: str
    event_position: str | None = None
    observed_at: str | None = None
    evidence_id: str | None = None


class Claim(_Model):
    id: str
    version_id: str
    seq: int
    claim_type: ClaimType
    subject: str
    key: str
    value: str
    normalized_text: str
    scope_path: str | None = None
    status: ClaimStatus
    provenance: Provenance
    supersedes_id: str | None = None
    entity_id: str | None = None
    evidence: Any | None = None
    confidence: float | None = None
    shape_ref: str | None = None
    origin: ClaimOrigin | None = None


class ClaimInput(_Model):
    """One claim in an `append_events` batch (same fields as propose_claim)."""

    subject: str
    key: str
    value: str
    claim_type: ClaimType = "fact"
    scope_path: str | None = None
    provenance: Provenance | None = None
    supersedes_id: str | None = None
    entity_id: str | None = None
    evidence: Any | None = None
    confidence: float | None = None
    shape_ref: str | None = None
    origin: ClaimOrigin | None = None


class ClaimOutcome(_Model):
    """Result of propose_claim. A gate-blocked claim is SUCCESS with
    status == "disputed" plus findings — recorded, never dropped."""

    claim: Claim
    findings: list[GateFinding]
    head_seq: int

    @property
    def is_disputed(self) -> bool:
        return self.claim.status == "disputed"


class EventsOutcome(_Model):
    claims: list[Claim]
    findings: list[GateFinding]
    head_seq: int

    @property
    def is_disputed(self) -> bool:
        return any(c.status == "disputed" for c in self.claims)


class ClaimLookup(_Model):
    claim: Claim
    superseded: bool
    superseded_by: str | None = None


class FactsPage(_Model):
    facts: list[Claim]
    as_of_seq: int
    head_seq: int


class Anchor(_Model):
    id: str
    version_id: str
    detail_key: str
    locked_value: str
    locked_at_scope: str | None = None
    status: str
    seq: int


class Promise(_Model):
    id: str
    version_id: str
    key: str
    kind: str
    description: str
    origin_scope: str | None = None
    due_scope: str | None = None
    status: str
    seq: int
    fulfilled_seq: int | None = None


class Counter(_Model):
    key: str
    total: int
    budget: int | None = None


class Digest(_Model):
    version_id: str
    tier: int
    scope_path: str
    content: str
    content_hash: str
    built_from_seq: int


class Section(_Model):
    title: str
    body: str


class ComposedContext(_Model):
    sections: list[Section]
    text: str
    estimated_tokens: int
    content_hash: str
    as_of_seq: int


class PutSourceResult(_Model):
    #: Stable identity of the source, derived from its logical path.
    source_id: str
    #: hex sha-256 — integrity of the stored bytes.
    content_hash: str
    bytes_len: int
    #: True only when this path already held these exact bytes. Re-uploading a
    #: path with NEW content is an update and reports False.
    already_existed: bool


class RecordIngestResult(_Model):
    event_id: str
    seq: int


class IndexStatus(_Model):
    index_version: str
    shape_ref: str
    event_watermark: int
    active: bool
    manifest: Any = None


class SearchHit(_Model):
    chunk_id: str
    #: Stable identity of the source document.
    source_id: str
    #: The logical path — which document answered.
    source_path: str
    #: hex sha-256 of the bytes that path held at index time.
    source_content_hash: str
    text: str
    score: float
    lexical_rank: int | None = None
    vector_rank: int | None = None
    metadata: Any | None = None


class ProvenanceEnvelope(_Model):
    """Every retrieval answer carries one — surface it, don't hide it.

    Sources are named three ways deliberately: ``source_ids`` are stable
    identity, ``source_paths`` say *which document* answered (a bare hash
    never did), and ``source_content_hashes`` prove which bytes it held.
    """

    chunk_ids: list[str]
    source_ids: list[str]
    source_paths: list[str]
    source_content_hashes: list[str]
    index_version: str
    event_watermark: int
    provider_fingerprint: str | None = None


class SearchResult(_Model):
    hits: list[SearchHit]
    envelope: ProvenanceEnvelope


class ApplyShapeResult(_Model):
    shape_ref: str
    yaml_hash: str
    event_id: str | None = None


class RunbookRun(_Model):
    run_id: str
    state: str


class RunbookStep(_Model):
    ordinal: int
    name: str
    state: str
    detail: Any | None = None


class RunStatus(_Model):
    run_id: str
    runbook_ref: str
    state: str
    version_id: str | None = None
    steps: list[RunbookStep]


class ProviderHealth(_Model):
    healthy: bool
    provider: str
    endpoint_fingerprint: str
    detail: str


class CompleteResult(_Model):
    text: str
    stop_reason: str
    input_tokens: int
    output_tokens: int
    #: The provider family that served the request (anthropic|openai|openrouter).
    provider: str = ""
    #: The resolved model id that served the request.
    model: str = ""
    invocation_event_id: str | None = None


class HealthAiCheck(_Model):
    """One /healthai probe: a small live completion against one provider/tier."""

    provider: str
    tier: str
    model: str
    ok: bool
    #: True when the probe was skipped because no credential is configured.
    skipped: bool
    latency_ms: int | None = None
    detail: str


class HealthAiResult(_Model):
    healthy: bool
    checks: list[HealthAiCheck]


class EmbedResult(_Model):
    vectors: list[list[float]]
    dimensions: int
    cache_hit: bool
    #: The provider family that served the request.
    provider: str = ""
    #: The resolved model id that served the request.
    model: str = ""
    invocation_event_id: str | None = None


# ---------------------------------------------------------------------------
# sessions + turns
# ---------------------------------------------------------------------------


class ModelOverride(_Model):
    """API-level model override for a session turn — honored only under the
    runbook's ``models.allowOverrides`` policy. A disallowed override draws
    the typed ``override-not-allowed`` error, never a silent downgrade."""

    provider: str | None = None
    model: str | None = None
    #: fast | capable
    tier: str | None = None


class CreateSessionResult(_Model):
    session_id: str
    #: The pinned name@version this session will use for every turn.
    runbook_ref: str
    #: Collections the caller's access level/compartments permit — the
    #: least-privilege echo so a client knows what it can see.
    permitted_collections: list[str]


class TurnHit(_Model):
    #: Which collection this hit came from.
    collection: str
    chunk_id: str
    source_id: str
    #: The logical path — which document answered this turn.
    source_path: str
    source_content_hash: str
    text: str
    score: float


class CollectionEnvelope(_Model):
    collection: str
    envelope: ProvenanceEnvelope


class TurnVerification(_Model):
    """Deterministic turn-verification outcome (quotes resolve in served
    text, citations name served content). Violations are prefixed
    ``quote: `` / ``citation: ``."""

    #: Which checks ran ("quotes", "citations").
    checks: list[str]
    #: Corrective completions actually spent (each is a paid call).
    retries: int
    first_pass_violations: list[str]
    #: Violations remaining on the FINAL answer (empty = verified).
    violations: list[str]


class TurnCompletion(_Model):
    provider: str
    model: str
    #: Whether an API model override decided the provider/model.
    was_override: bool
    text: str
    #: Token totals across ALL completions this turn paid for, retries
    #: included.
    input_tokens: int
    output_tokens: int
    #: Present when the runbook declares completion.verification.
    verification: TurnVerification | None = None


class LayerOutcome(_Model):
    """What one evidence layer produced."""

    layer: str
    #: supporting | primary | controlling
    role: str
    #: required | optional | fallback
    requirement: str
    #: document_hits | complete_table | count | fact_slice | refusal
    block: str
    evidence_id: str | None = None
    #: Whether an answer may make a completeness claim on THIS layer.
    #: Document hits are always false: retrieval returns what it found,
    #: never a proof that nothing else exists.
    supports_completeness: bool
    refusal_code: str | None = None
    elapsed_ms: int


class EvidenceHierarchyDecision(_Model):
    """Why the model saw what it saw — the DECISION, not the
    content: which profile ran, which layers answered or refused, whether a
    completeness claim was permissible at all. No evidence rows appear
    here; resolve those through the ``evidence`` plane."""

    profile: str
    intent_kind: str | None = None
    #: True when the caller supplied the intent rather than a model
    #: producing it, so a keyless test result never reads as a planner one.
    intent_explicit: bool
    layers: list[LayerOutcome]
    completeness_available: bool
    disclosed_conflicts: int = 0
    conflicts_policy: str


class TurnResult(_Model):
    session_id: str
    ordinal: int
    #: Collections actually searched (post access filtering).
    collections_searched: list[str]
    #: Permitted but skipped (no active index).
    skipped: list[str] = []
    hits: list[TurnHit]
    envelopes: list[CollectionEnvelope]
    #: Absent when no completion ran.
    completion: TurnCompletion | None = None
    #: Present ONLY when a research profile ran. A turn taken
    #: without one carries no such key, so a legacy caller's parse is
    #: byte-for-byte what it always was.
    hierarchy: EvidenceHierarchyDecision | None = None


class TurnProgress(_Model):
    """One SSE progress event on the streaming turn plane. Deliberately
    permissive: only ``stage`` is declared, the per-stage fields ride as
    extras — a newer server may add stages this build cannot name, and
    progress is informational, so unknown shapes must never break
    iteration.

    Stages today: ``probe``, ``selection``, ``expansion``, ``retrieval``,
    ``merge``, ``model``, ``completion``, ``verify``, and this
    hierarchy set ``profile``, ``layer_start``, ``layer_source``,
    ``layer_complete``, ``coverage``, ``compose`` — the last six emitted
    only when a research profile runs, appended after the legacy stages so
    an existing turn's event sequence is unchanged. ``verify`` gained an
    optional ``layer`` extra for the same reason.
    """

    stage: str


#: One item yielded by ``sessions.turn_stream``: N progress events, then
#: exactly one TurnResult (always the LAST item).
TurnStreamEvent = TurnProgress | TurnResult


class SessionTurn(_Model):
    """One stored transcript row. The JSON-shaped fields come back as
    parsed values, not re-typed models — the transcript is a record."""

    ordinal: int
    query: str
    collections_searched: list[str]
    hits: Any = None
    envelope: Any = None
    completion: Any = None
    created_at: str


class Session(_Model):
    session_id: str
    uid: str
    runbook_ref: str
    access_level: int
    compartments: list[str]
    #: open | closed | expired
    state: str
    created_at: str
    turns: list[SessionTurn]


# ---------------------------------------------------------------------------
# access tokens (mgmt)
# ---------------------------------------------------------------------------


class TokenGrant(_Model):
    """A freshly minted capability JWT. The token material is returned ONCE
    and never persisted server-side — treat it as a secret."""

    token: str
    #: Token id — the audit/revocation key.
    jti: str
    #: RFC 3339 expiry.
    expires_at: str


class TokenInfo(_Model):
    """One issued token in the issuance audit — metadata only, never the
    token material."""

    jti: str
    uid: str
    access_level: int
    compartments: list[str]
    scopes: list[str]
    runbook_refs: list[str] | None = None
    issued_by: str
    issued_at: str
    expires_at: str
    revoked_at: str | None = None


class RevokeResult(_Model):
    jti: str
    revoked: bool
    #: The deny-list is only consulted when the server enables the check.
    revocation_check_enabled: bool


# ---------------------------------------------------------------------------
# management reports (mgmt)
# ---------------------------------------------------------------------------


class UsageRow(_Model):
    #: The grouping key value (a uid, session id, runbook ref, or collection).
    key: str
    interactions: int
    turns: int
    completion_input_tokens: int
    completion_output_tokens: int
    avg_latency_ms: float | None = None


class UsageReport(_Model):
    #: uid | session | runbook | collection
    group_by: str
    rows: list[UsageRow]


class AuditEntry(_Model):
    id: str
    uid: str
    session_id: str | None = None
    request_id: str | None = None
    plane: str
    method: str
    runbook_ref: str | None = None
    token_jti: str | None = None
    status: int | None = None
    latency_ms: int | None = None
    #: Captured bodies — present only when the query asked for them.
    request: Any = None
    response: Any = None
    created_at: str


class AuditPage(_Model):
    entries: list[AuditEntry]
    #: Keyset cursor for the next (older) page: pass it back as ``before``.
    #: Absent means the trail is exhausted.
    next_before: str | None = None


class CostRow(_Model):
    provider: str
    model: str
    turns: int
    overridden_turns: int
    input_tokens: int
    output_tokens: int


class CostReport(_Model):
    """Model-spend token rollup (dollar pricing lives upstream)."""

    rows: list[CostRow]


class TimeseriesBucket(_Model):
    #: Bucket start, RFC 3339 UTC.
    bucket: str
    requests: int
    errors_4xx: int
    errors_5xx: int
    p50_latency_ms: float | None = None
    p95_latency_ms: float | None = None


class TimeseriesReport(_Model):
    #: 1h | 24h | 7d | 30d
    window: str
    bucket_seconds: int
    #: rest | grpc when the query filtered by plane.
    plane: str | None = None
    buckets: list[TimeseriesBucket]


class EndpointRow(_Model):
    method: str
    requests: int
    #: Fraction of requests with status >= 400.
    error_rate: float
    avg_latency_ms: float | None = None
    p95_latency_ms: float | None = None


class EndpointsReport(_Model):
    window: str
    rows: list[EndpointRow]


class RunbookRunsRow(_Model):
    state: str
    runs: int
    avg_wall_ms: float | None = None


class RunbookStepsRow(_Model):
    state: str
    steps: int


class RunbookReport(_Model):
    window: str
    runs: list[RunbookRunsRow]
    steps: list[RunbookStepsRow]


class SessionsBucket(_Model):
    bucket: str
    sessions_opened: int
    turns: int
    #: Distinct uids that took a turn in the bucket.
    active_uids: int


class SessionsReport(_Model):
    window: str
    bucket_seconds: int
    buckets: list[SessionsBucket]


class EvidenceLayerStats(_Model):
    """One layer's aggregate behaviour over the report window."""

    profile: str
    layer: str
    turns: int
    #: Turns where this layer refused.
    refusals: int
    #: Turns where this layer could support a completeness claim.
    complete: int
    #: Refusal codes seen, most frequent first.
    refusal_codes: list[str]
    p50_ms: int
    p95_ms: int


class EvidenceReport(_Model):
    """How the evidence hierarchy actually behaved.

    The operational question is "which layer is quietly refusing?" — a layer
    refusing on most turns is misconfigured or pointed at something down, and
    either way the answers are thinner than the runbook claims while every one
    of those turns still returns 200.
    """

    window: str
    #: Turns that ran a research profile.
    hierarchy_turns: int
    #: Turns on the legacy document path.
    legacy_turns: int
    #: Hierarchy turns where at least one layer could support a completeness
    #: claim.
    completeness_available: int
    layers: list[EvidenceLayerStats]


class MatrixDataView(_Model):
    runbook_ref: str
    name: str
    contract: str
    access_level: int


class MatrixReport(_Model):
    """Munarium Matrix's health as the server sees it."""

    #: False when the server has no Matrix base URL — the plane is not
    #: wired, which is different from wired-and-failing and must not read
    #: the same.
    configured: bool
    #: Per-INSTANCE circuit-breaker state. Deliberately not per tenant: the
    #: breaker is shared, so a per-tenant reading would report a fact that
    #: does not exist.
    circuit_open: bool
    consecutive_failures: int
    #: Data views declared across the tenant's applied runbooks.
    data_views: list[MatrixDataView]


# ---------------------------------------------------------------------------
# file / bulk ingestion
# ---------------------------------------------------------------------------


class IngestFile(_Model):
    """One file for the ingest plane (INPUT). Content is base64 (JSON-safe
    on REST; the gRPC transport decodes it to native bytes client-side);
    the declared sha256, when present, is verified before commit."""

    filename: str
    media_type: str
    content_base64: str
    sha256: str | None = None
    #: Explicit collection names to bind into. Absent = auto-bind via the
    #: declarative ``sources:`` matchers of every reachable active runbook.
    collections: list[str] | None = None


class IngestResult(_Model):
    """Per-file outcome — one failed file never fails the batch; check
    ``error``."""

    filename: str
    source_id: str | None = None
    sha256: str | None = None
    #: True only on a genuine idempotent replay (same path, same bytes).
    existed: bool
    #: Collections this file is now bound to (from this call).
    bound_to: list[str]
    error: str | None = None


class BulkManifestEntry(_Model):
    """One bulk-manifest entry (INPUT): what the client intends to upload."""

    filename: str
    #: Declared content hash (hex), verified against every received chunk
    #: file — an identical re-run needs no bytes at all.
    sha256: str
    bytes_len: int
    media_type: str


class BulkOpenResult(_Model):
    bulk_id: str
    total: int
    #: Entries whose logical path already holds these exact bytes.
    already_present: int
    #: Filenames still owed bytes — the client's upload work list.
    needed: list[str]


class BulkChunkResult(_Model):
    bulk_id: str
    #: Per-file outcomes, same shape as batch ingest.
    results: list[IngestResult]
    stored: int
    skipped_existing: int
    pending: int
    failed: int


class BulkFileError(_Model):
    filename: str
    error: str


class BulkStatus(_Model):
    bulk_id: str
    label: str | None = None
    #: open | completed | expired
    status: str
    total: int
    stored: int
    skipped_existing: int
    pending: int
    failed: int
    #: Failed entries with their last error (capped at 100).
    failures: list[BulkFileError]
    #: Populated only when the request asked for it (include_needed).
    needed: list[str] | None = None
    created_at: str
    expires_at: str
    completed_at: str | None = None


class BulkCompleteResult(_Model):
    bulk_id: str
    #: completed | incomplete — incomplete leaves the session open.
    status: str
    total: int
    stored: int
    skipped_existing: int
    #: Manifest entries with no stored bytes (capped at 100; see counts).
    missing: list[str]
    missing_count: int
    #: Entries whose stored content hash no longer matches the manifest.
    mismatched: list[str]
    mismatched_count: int


class SourceInfo(_Model):
    """Where a document actually went. Metadata only — never the bytes."""

    source_id: str
    filename: str
    media_type: str
    content_hash: str
    bytes_len: int
    #: az | pg | mem.
    storage_backend: str
    blob_uri: str | None = None
    #: NULL until first indexed, then ok | empty | failed.
    extraction_status: str | None = None
    #: text | docx | pdf-text | ocr.
    extraction_method: str | None = None
    created_at: str


# ---------------------------------------------------------------------------
# collections
# ---------------------------------------------------------------------------


class Collection(_Model):
    id: str
    name: str
    shape_ref: str
    access_level: int
    compartments: list[str]
    #: active | retired — there is no delete anywhere; collections retire
    #: softly.
    status: str
    description: str | None = None
    created_at: str
    #: Sources currently bound to this collection.
    source_count: int
    #: The active index version id, if one has been cut over.
    active_index: str | None = None


# ---------------------------------------------------------------------------
# runbook management v2
# ---------------------------------------------------------------------------


class RunbookCollection(_Model):
    name: str
    #: The materialized collection id; None until the runbook is applied.
    collection_id: str | None = None
    shape_ref: str
    access_level: int
    compartments: list[str]
    active_index: str | None = None
    source_count: int


class RunbookSummary(_Model):
    #: name@version
    runbook_ref: str
    name: str
    version: int
    #: active | remove_requested | removed
    status: str
    #: The minimum access level that sees ANY of this runbook's collections.
    min_access_level: int
    collections: list[RunbookCollection]
    created_at: str


class RunbookInfo(_Model):
    runbook_ref: str
    name: str
    version: int
    status: str
    collections: list[RunbookCollection]
    #: Sibling versions of the same name (refs), including this one.
    versions: list[str]
    #: The models block (defaults per task level + override policy), echoed.
    models: Any = None
    #: Retrieval knobs in effect.
    retrieval: Any = None
    #: Whether session turns can run a RAG completion step.
    has_completion: bool
    created_at: str


class ValidationFinding(_Model):
    #: error | warn | info
    severity: str
    #: Stable dotted code, e.g. "steps.cutover-before-build".
    code: str
    message: str
    path: str


class Suggestion(_Model):
    """AI-assisted improvement suggestion (advisory only)."""

    title: str
    rationale: str
    patch_hint: str | None = None


class ValidateResult(_Model):
    #: False when any error-severity finding is present.
    valid: bool
    findings: list[ValidationFinding]
    #: Present when suggest=true and a provider is configured.
    suggestions: list[Suggestion] = []
    suggest_note: str | None = None


class RemovalRequest(_Model):
    """First pass of the double-pass soft removal."""

    runbook_ref: str
    #: Present this id to remove_confirm within the TTL.
    removal_id: str
    expires_at: str


class RemovalConfirmResult(_Model):
    runbook_ref: str
    #: Always "removed" on success. Removal is visibility-only — yaml, run
    #: history, collections, and index data are all retained.
    status: str


class ApplyChronologyResult(_Model):
    name: str
    #: Rule targets declared — a quick sanity echo, not a validation result.
    rule_count: int


# ---------------------------------------------------------------------------
# findings (2026-08-17)
# ---------------------------------------------------------------------------


class StoredFinding(_Model):
    """One persisted gate finding plus the head seq its write settled at,
    so pinned reads bound this store like every other."""

    seq: int
    finding: GateFinding


# ---------------------------------------------------------------------------
# provider disclosure (GET /v1/providers)
# ---------------------------------------------------------------------------


class ProviderModels(_Model):
    """One provider config's resolved tier models — free introspection,
    zero provider calls; the credential itself is never echoed."""

    #: Config name, or default-<family> for the synthesized env default.
    name: str
    #: anthropic | openai | openrouter.
    provider: str
    #: applied | default.
    source: str
    credential_ok: bool
    fast: str | None = None
    capable: str | None = None
    frontier: str | None = None


# ---------------------------------------------------------------------------
# max-tokens budgets (GET/POST /v1/max-tokens)
# ---------------------------------------------------------------------------


class MaxTokensBudgets(_Model):
    """The per-call output-token ceilings (``max_tokens``) the server hands a
    model provider, one per kind of paid call, as ONE object. Every field
    is REQUIRED on the wire: ``POST /v1/max-tokens`` replaces the whole
    set, never part of it — a partial set is refused here before it is
    sent, and an out-of-range value is the server's 400 ``invalid-input``
    (``turn_completion`` 256..=16384, ``query_expansion`` 32..=512, the
    rest 1..=65536).

    Precedence at call time: a runbook's own declaration where the grammar
    has one (``completion.maxTokens``, ``modelQueryExpansion.maxTokens``)
    > the tenant's replacement set through this API > the process's
    ``MUNARIUM_MAX_TOKENS_*`` environment > the built-ins."""

    #: A session turn's answer; a runbook's ``completion.maxTokens``
    #: overrides it. Built-in 2,048.
    turn_completion: int
    #: The ``modelQueryExpansion`` variant-generation call; a runbook's
    #: ``modelQueryExpansion.maxTokens`` overrides it. Built-in 256.
    query_expansion: int
    #: ``POST /v1/providers/{name}/complete`` when the request omits
    #: ``max_tokens``. Built-in 1,024.
    complete_default: int
    #: Each ``/healthai`` probe completion. Built-in 512.
    healthai_probe: int
    #: The evidence hierarchy's one-word question classifier. Built-in 32.
    hierarchy_classifier: int
    #: The evidence hierarchy's semantic-intent task (names only). Built-in 480.
    hierarchy_intent: int
    #: The runbook validation AI advisory pass. Built-in 2,048.
    runbook_advisory: int
    #: The guided-authoring assist draft. Built-in 8,192.
    authoring_assist: int


class MaxTokensResponse(MaxTokensBudgets):
    """``GET /v1/max-tokens``, and what ``POST`` answers with: the effective
    budgets plus where they come from. The wire shape is the eight budget
    fields FLATTENED beside ``source``, so this subclasses the budgets
    model and a read result passes straight back into
    ``providers.replace_max_tokens`` — the body sent is the eight budget
    fields only, never ``source`` or ``updated_at``."""

    #: ``tenant`` after the tenant replaced the set through the API;
    #: ``environment`` while the process defaults (env vars over the
    #: built-ins) apply.
    source: str
    #: RFC 3339 instant of the tenant's last replacement; ``None`` for
    #: ``environment``.
    updated_at: str | None = None


# ---------------------------------------------------------------------------
# guided authoring (patterns, drafts, bundles)
# ---------------------------------------------------------------------------


class PatternSummary(_Model):
    id: str
    name: str
    description: str
    #: The committed exemplar runbook to start from.
    start_from: str
    #: What this pattern is strongest at, and the failure mode to design against.
    guidance: str
    has_completion: bool


class NamedYaml(_Model):
    name: str
    yaml: str


class PatternDetail(_Model):
    id: str
    name: str
    description: str
    start_from: str
    #: What this pattern is strongest at, and the failure mode to design against.
    guidance: str
    has_completion: bool
    #: Design notes the deterministic validator cannot police.
    decision_notes: list[str]
    #: The exemplar runbook, verbatim.
    runbook_yaml: str
    #: The exemplar's shape dependencies, verbatim.
    shapes: list[NamedYaml]


class InterviewQuestion(_Model):
    id: str
    prompt: str
    guidance: str
    #: string | text | int | bool | enum | areas | fields | map
    kind: str
    required: bool
    default: Any = None
    choices: list[str] = []
    #: Documentation of the slot this answer lands in.
    maps_to: str


class InterviewSection(_Model):
    id: str
    title: str
    #: The document section that teaches this decision in full.
    doc_ref: str
    questions: list[InterviewQuestion]


class DraftDocument(_Model):
    #: Path within the set, e.g. "runbooks/<name>.yaml".
    path: str
    #: Shape | Runbook
    kind: str
    yaml: str
    #: sha256 hex of the YAML bytes.
    sha256: str


class DocumentFindings(_Model):
    path: str
    findings: list[ValidationFinding]


class DraftValidation(_Model):
    #: False when any error-severity finding exists anywhere in the set.
    valid: bool
    #: Per-document findings (parse + the document's own validator).
    documents: list[DocumentFindings]
    #: Cross-document findings (set.* codes).
    set_findings: list[ValidationFinding]
    #: What still needs answering ("red TODOs expected" on a fresh draft).
    todos: list[str] = []


class DraftSummary(_Model):
    draft_id: str
    name: str
    #: interview | drafted | validated | exported (progress display only).
    state: str
    pattern_id: str | None = None
    created_by: str
    updated_at: str


class Draft(_Model):
    draft_id: str
    name: str
    state: str
    pattern_id: str | None = None
    #: Flat map keyed by interview question id.
    answers: Any = None
    #: The interview for this draft (completion section is pattern-gated).
    interview: list[InterviewSection]
    documents: list[DraftDocument]
    #: Fresh validation of the current documents.
    validation: DraftValidation | None = None
    todos: list[str] = []
    assist_note: str | None = None
    created_by: str
    created_at: str
    updated_at: str


class DraftDeleteResult(_Model):
    draft_id: str
    #: Always "deleted" (soft; the row is retained). This is the client
    #: surface's ONE delete — a workspace draft, never ledger data.
    status: str


class AssistResult(_Model):
    """AI-assisted drafting pass. NEVER fails the request: a degraded pass
    (no provider, budget, parse failure) sets ``assist_note`` instead."""

    #: The documents after the pass (unchanged when the pass degraded).
    documents: list[DraftDocument]
    suggestions: list[Suggestion] = []
    assist_note: str | None = None
    validation: DraftValidation


class BundleTool(_Model):
    name: str
    version: str


class BundleValidation(_Model):
    valid: bool
    errors: int
    warns: int
    infos: int


class ExportBundle(_Model):
    """Self-contained hash-manifested bundle, applied to any instance via
    the existing shapes/runbooks routes in ``apply_order``. ``manifest_hash``
    = sha256 over the byte-sorted ``path\\0hash\\n`` lines."""

    #: Always "MunariumAuthoringBundle".
    kind: str
    api_version: str = Field(default="", alias="apiVersion")
    tool: BundleTool
    draft_id: str
    name: str
    created_at: str
    #: path -> YAML, verbatim.
    files: dict[str, str]
    #: path -> sha256 hex.
    hashes: dict[str, str]
    #: Shapes before runbooks.
    apply_order: list[str]
    manifest_hash: str
    validation: BundleValidation


class AppliedDoc(_Model):
    path: str
    #: Shape | Runbook
    kind: str
    #: shape_ref or runbook_ref (name@version).
    ref: str
    #: sha256 of the applied YAML.
    yaml_hash: str


class ApplyDraftResult(_Model):
    applied: list[AppliedDoc]


# ---------------------------------------------------------------------------
# meta
# ---------------------------------------------------------------------------


class ServerVersion(_Model):
    """GET /version body (unauthenticated) — handy for asserting the
    TARGET_SERVER_VERSION handshake."""

    name: str
    version: str


class EvidenceRows(BaseModel):
    """A bounded window over a sealed evidence artifact's rows.

    Served for canonical-CSV artifacts only. A Parquet artifact is sealed and
    replayable byte-for-byte, but the server does not decode it and says so
    rather than pretending the rows are unavailable.
    """

    evidence_id: str
    from_: int = Field(0, alias="from")
    rows: list[dict[str, Any]] = Field(default_factory=list)
    total: int | None = None
    has_more: bool = False

    model_config = ConfigDict(populate_by_name=True)
