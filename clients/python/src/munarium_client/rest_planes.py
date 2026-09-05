# SPDX-License-Identifier: Apache-2.0
"""The ten plane namespaces over the REST transport — sync and async. Every
method builds a shared call spec (``_specs``) and executes it, so the two
variants cannot drift on paths, params, or parsing. The one exception is
``sessions.turn_stream`` — SSE has no call spec; both variants ride the
transport's shared event machine instead."""

from __future__ import annotations

from collections.abc import AsyncIterator, Iterator
from typing import Any

from . import _specs as s
from . import models as m
from ._chunks import ChunkSource
from .rest import AsyncRestTransport, SyncRestTransport

# ---------------------------------------------------------------------------
# sync
# ---------------------------------------------------------------------------


class RestCommands:
    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def create_version(
        self,
        parent_version_id: str | None = None,
        metadata: Any | None = None,
        idempotency_key: str | None = None,
    ) -> str:
        return self._t.run(s.create_version(parent_version_id, metadata), idempotency_key)

    def propose_claim(
        self,
        version_id: str,
        *,
        expected_head: int | None = None,
        idempotency_key: str | None = None,
        **claim: Any,
    ) -> m.ClaimOutcome:
        return self._t.run(
            s.propose_claim(version_id, m.ClaimInput(**claim), expected_head),
            idempotency_key,
        )

    def append_events(
        self,
        version_id: str,
        claims: list[m.ClaimInput | dict[str, Any]],
        *,
        candidate_text: str | None = None,
        expected_head: int | None = None,
        idempotency_key: str | None = None,
    ) -> m.EventsOutcome:
        inputs = [m.coerce(m.ClaimInput, c) for c in claims]
        return self._t.run(
            s.append_events(version_id, inputs, candidate_text, expected_head),
            idempotency_key,
        )

    def open_promise(
        self,
        version_id: str,
        *,
        key: str,
        kind: str,
        description: str,
        origin_scope: str | None = None,
        due_scope: str | None = None,
        idempotency_key: str | None = None,
    ) -> m.Promise:
        return self._t.run(
            s.open_promise(version_id, key, kind, description, origin_scope, due_scope),
            idempotency_key,
        )

    def fulfill_promise(
        self, version_id: str, key: str, idempotency_key: str | None = None
    ) -> bool:
        return self._t.run(s.fulfill_promise(version_id, key), idempotency_key)

    def lock_anchor(
        self,
        version_id: str,
        *,
        subject: str,
        key: str,
        value: str,
        scope_path: str | None = None,
        evidence: Any | None = None,
        idempotency_key: str | None = None,
    ) -> m.Anchor:
        return self._t.run(
            s.lock_anchor(version_id, subject, key, value, scope_path, evidence),
            idempotency_key,
        )

    def record_counts(
        self,
        version_id: str,
        *,
        key: str,
        scope_path: str,
        count: int,
        budget: int | None = None,
        idempotency_key: str | None = None,
    ) -> None:
        self._t.run(s.record_counts(version_id, key, scope_path, count, budget), idempotency_key)

    def upsert_digest(self, digest: m.Digest) -> None:
        self._t.run(s.upsert_digest(digest))


class RestQuery:
    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def head(self, version_id: str) -> int:
        return self._t.run(s.head(version_id))

    def get_claim(self, claim_id: str) -> m.ClaimLookup:
        return self._t.run(s.get_claim(claim_id))

    def facts(
        self,
        version_id: str,
        *,
        scope_prefix: str | None = None,
        as_of_seq: int | None = None,
        statuses: tuple[m.ClaimStatus, ...] = (),
        limit: int | None = None,
    ) -> m.FactsPage:
        return self._t.run(s.facts(version_id, scope_prefix, as_of_seq, statuses, limit))

    def lineage(self, version_id: str) -> list[str]:
        return self._t.run(s.lineage(version_id))

    def anchors(self, version_id: str, as_of_seq: int | None = None) -> list[m.Anchor]:
        return self._t.run(s.anchors(version_id, as_of_seq))

    def promises(
        self,
        version_id: str,
        as_of_seq: int | None = None,
        status: str | None = None,
    ) -> list[m.Promise]:
        return self._t.run(s.promises(version_id, as_of_seq, status))

    def counters(self, version_id: str, as_of_seq: int | None = None) -> list[m.Counter]:
        return self._t.run(s.counters(version_id, as_of_seq))

    def digests(self, version_id: str) -> list[m.Digest]:
        return self._t.run(s.digests(version_id))

    def compose_context(
        self,
        version_id: str,
        *,
        scope: str | None = None,
        budget_tokens: int | None = None,
        fact_limit: int | None = None,
        as_of_seq: int | None = None,
    ) -> m.ComposedContext:
        return self._t.run(
            s.compose_context(version_id, scope, budget_tokens, fact_limit, as_of_seq)
        )

    def findings(
        self,
        version_id: str,
        *,
        as_of_seq: int | None = None,
        severity: str | None = None,
        rule_id: str | None = None,
        limit: int | None = None,
    ) -> list[m.StoredFinding]:
        """Persisted gate findings with the head seq each write settled at."""
        return self._t.run(s.findings(version_id, as_of_seq, severity, rule_id, limit))


class RestIngest:
    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def put_source(
        self,
        data: ChunkSource,
        *,
        declared_sha256: str = "",
        media_type: str | None = None,
        filename: str | None = None,
        shape_ref: str | None = None,
    ) -> m.PutSourceResult:
        raw = self._t.put_source(data, declared_sha256, media_type, filename, shape_ref)
        return m.PutSourceResult.model_validate(raw)

    def record_ingest(
        self, version_id: str, *, content_hash: str, shape_ref: str | None = None
    ) -> m.RecordIngestResult:
        return self._t.run(s.record_ingest(version_id, content_hash, shape_ref))

    def ingest(self, file: m.IngestFile | dict[str, Any]) -> m.IngestResult:
        """Ingest ONE document through the file plane (base64 body,
        declarative collection auto-binding). Requires the ingest scope."""
        return self._t.run(s.ingest(m.coerce(m.IngestFile, file)))

    def ingest_batch(self, files: list[m.IngestFile | dict[str, Any]]) -> list[m.IngestResult]:
        """Batch ingest (1..=500 files, client-guarded) with per-item
        outcomes — one failed file does not fail the batch."""
        return self._t.run(s.ingest_batch([m.coerce(m.IngestFile, f) for f in files]))

    def bulk_open(
        self,
        files: list[m.BulkManifestEntry | dict[str, Any]],
        *,
        label: str | None = None,
    ) -> m.BulkOpenResult:
        """Open a bulk upload session from a manifest. The response's
        ``needed`` is the upload work list — entries already stored
        byte-identically are skipped, so an identical re-run uploads
        nothing."""
        return self._t.run(s.bulk_open([m.coerce(m.BulkManifestEntry, e) for e in files], label))

    def bulk_chunk(
        self, bulk_id: str, files: list[m.IngestFile | dict[str, Any]]
    ) -> m.BulkChunkResult:
        """Upload one chunk of files (at most 500 — a larger list is a typed
        client-side error, not a server round-trip). Per-document
        idempotent."""
        return self._t.run(s.bulk_chunk(bulk_id, [m.coerce(m.IngestFile, f) for f in files]))

    def bulk_status(self, bulk_id: str, *, include_needed: bool = False) -> m.BulkStatus:
        return self._t.run(s.bulk_status(bulk_id, include_needed))

    def bulk_complete(self, bulk_id: str) -> m.BulkCompleteResult:
        """Close the session against its manifest: "completed" when every
        entry is stored and hash-matched, else "incomplete" (session stays
        open) with the missing/mismatched lists."""
        return self._t.run(s.bulk_complete(bulk_id))

    def get_source(self, source_id: str) -> m.SourceInfo:
        """Metadata for one stored source (never the bytes)."""
        return self._t.run(s.get_source(source_id))


class RestRetrieval:
    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def search(
        self,
        *,
        query: str,
        shape_ref: str,
        top_k: int | None = None,
        index_version: str | None = None,
        filter: Any | None = None,
    ) -> m.SearchResult:
        return self._t.run(s.search(query, shape_ref, top_k, index_version, filter))

    def index_status(self, shape_ref: str) -> m.IndexStatus:
        return self._t.run(s.index_status(shape_ref))

    def build_index(self, shape_ref: str, version_id: str | None = None) -> m.IndexStatus:
        return self._t.run(s.build_index(shape_ref, version_id))

    def create_collection(
        self,
        *,
        name: str,
        shape_ref: str,
        access_level: int = 0,
        compartments: list[str] | None = None,
        description: str | None = None,
    ) -> m.Collection:
        """Create-or-update a compartmentalized collection. There is no
        delete anywhere — collections retire softly."""
        return self._t.run(
            s.create_collection(name, shape_ref, access_level, compartments or [], description)
        )

    def list_collections(self) -> list[m.Collection]:
        return self._t.run(s.list_collections())

    def get_collection(self, id: str) -> m.Collection:
        return self._t.run(s.get_collection(id))


class RestRunbooks:
    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def apply_shape(self, yaml: str, version_id: str | None = None) -> m.ApplyShapeResult:
        return self._t.run(s.apply_shape(yaml, version_id))

    def apply_runbook(self, yaml: str) -> str:
        return self._t.run(s.apply_runbook(yaml))

    def run_runbook(self, name: str, version_id: str | None = None) -> m.RunbookRun:
        return self._t.run(s.run_runbook(name, version_id))

    def get_run(self, run_id: str) -> m.RunStatus:
        return self._t.run(s.get_run(run_id))

    def approve_step(self, run_id: str, ordinal: int) -> m.RunbookRun:
        return self._t.run(s.approve_step(run_id, ordinal))

    def list(self, include_removed: bool = False) -> list[m.RunbookSummary]:
        """Every hosted runbook (all versions) with per-collection access
        requirements."""
        return self._t.run(s.list_runbooks(include_removed))

    def get_info(self, name: str) -> m.RunbookInfo:
        """One runbook's collections, sibling versions, models block, and
        retrieval knobs. ``name`` is a bare name (latest) or exact
        name@version."""
        return self._t.run(s.runbook_info(name))

    def validate(
        self,
        yaml: str,
        *,
        suggest: bool = False,
        provider: str | None = None,
        model: str | None = None,
        tier: str | None = None,
    ) -> m.ValidateResult:
        """Deterministic validation findings; ``suggest`` adds AI
        improvement suggestions (a BYOK provider call, policy-gated
        override)."""
        return self._t.run(s.validate_runbook(yaml, suggest, provider, model, tier))

    def remove_request(self, name: str) -> m.RemovalRequest:
        """First pass of the double-pass soft removal: returns the
        removal_id to present to ``remove_confirm`` within the TTL.
        ``name`` must be an EXACT name@version."""
        return self._t.run(s.remove_request(name))

    def remove_confirm(self, name: str, removal_id: str) -> m.RemovalConfirmResult:
        """Second pass: confirm with the removal_id. Removal is
        visibility-only — yaml, run history, collections, and index data
        are all retained."""
        return self._t.run(s.remove_confirm(name, removal_id))

    def apply_chronology_rules(self, yaml: str) -> m.ApplyChronologyResult:
        """Apply (upsert) a chronology-rules asset — the sixth gate's
        arming surface. text/yaml like shapes."""
        return self._t.run(s.apply_chronology_rules(yaml))

    def get_chronology_rules(self, name: str) -> str:
        """The applied rules YAML back, verbatim."""
        return self._t.run(s.get_chronology_rules(name))


class RestProviders:
    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def apply_config(self, yaml: str) -> str:
        return self._t.run(s.apply_provider(yaml))

    def health(self, name: str) -> m.ProviderHealth:
        return self._t.run(s.provider_health(name))

    def health_ai(self) -> m.HealthAiResult:
        """Live probe of the server's six built-in default models (three
        provider families x two tiers) — spends real provider tokens."""
        return self._t.run(s.health_ai())

    def complete(
        self,
        name: str,
        *,
        prompt: str,
        model: str | None = None,
        provider: str | None = None,
        tier: str | None = None,
        system: str | None = None,
        max_tokens: int | None = None,
        temperature: float | None = None,
        version_id: str | None = None,
    ) -> m.CompleteResult:
        return self._t.run(
            s.complete(
                name,
                prompt,
                model,
                system,
                max_tokens,
                temperature,
                version_id,
                provider=provider,
                tier=tier,
            )
        )

    def embed(
        self,
        name: str,
        *,
        inputs: list[str],
        model: str | None = None,
        provider: str | None = None,
        version_id: str | None = None,
    ) -> m.EmbedResult:
        return self._t.run(s.embed(name, inputs, model, version_id, provider=provider))

    def list(self) -> list[m.ProviderModels]:
        """Free disclosure of every provider config visible to the tenant —
        applied configs plus synthesized env defaults, each with its
        resolved fast/capable tier models and ``credential_ok``. Zero
        provider calls; the credential itself is never echoed."""
        return self._t.run(s.list_providers())

    def max_tokens(self) -> m.MaxTokensResponse:
        """GET /v1/max-tokens — the effective per-call output-token budgets
        for the caller's tenant and where they come from (``source`` is
        ``tenant`` after a replacement, else ``environment``). Any
        authenticated role; zero provider calls. REST-only."""
        return self._t.run(s.get_max_tokens())

    def replace_max_tokens(
        self, budgets: m.MaxTokensBudgets | dict[str, Any]
    ) -> m.MaxTokensResponse:
        """POST /v1/max-tokens — replace the tenant's WHOLE budget set.
        There is no partial update: all eight fields are sent (a dict
        missing one fails model validation before the wire; an
        out-of-range value is the server's 400 ``invalid-input``), and the
        answer is the same shape ``max_tokens()`` returns, so a read result
        edited in place round-trips — only the eight budget fields go on
        the wire. Static **rw** role only (``ForbiddenError`` otherwise).
        REST-only."""
        return self._t.run(s.replace_max_tokens(m.coerce(m.MaxTokensBudgets, budgets)))


class RestSessions:
    """Multiturn sessions over a runbook's access-permitted collections.
    Auth is the data plane's: a capability JWT with the query scope (or a
    static token), and the uid contract applies to every call."""

    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def create(self, runbook_name: str) -> m.CreateSessionResult:
        """Open a session on a runbook (bare name = latest non-removed
        version, or exact name@version). The response echoes the
        collections the caller's access level/compartments actually
        permit."""
        return self._t.run(s.create_session(runbook_name))

    def turn(
        self,
        session_id: str,
        *,
        query: str,
        top_k: int | None = None,
        complete: bool | None = None,
        model_override: m.ModelOverride | dict[str, Any] | None = None,
        research_profile: str | None = None,
    ) -> m.TurnResult:
        """One retrieval turn (+ optional completion when the runbook
        declares one). ``model_override`` is honored only under the
        runbook's ``models.allowOverrides`` policy — a disallowed override
        draws the typed ``override-not-allowed`` error, never a silent
        downgrade.

        ``research_profile`` runs the turn through a named evidence
        hierarchy and fills :attr:`models.TurnResult.hierarchy` with the
        decision. Omit it and the turn executes and serializes exactly as
        it always has — same request bytes, same response keys."""
        return self._t.run(
            s.turn(session_id, query, top_k, complete, model_override, research_profile)
        )

    def turn_stream(
        self,
        session_id: str,
        *,
        query: str,
        top_k: int | None = None,
        complete: bool | None = None,
        model_override: m.ModelOverride | dict[str, Any] | None = None,
        research_profile: str | None = None,
    ) -> Iterator[m.TurnStreamEvent]:
        """The same turn, streamed over SSE: N :class:`models.TurnProgress`
        events at real stage boundaries, then exactly one
        :class:`models.TurnResult` — always the LAST item yielded. A
        server-side failure (pre-stream or mid-stream) raises the typed
        error during iteration; a stream that ends without a terminal
        done/error event raises ``TransportError`` — never a silent
        success.

        Under a ``research_profile`` the stream also carries the hierarchy
        stages (``profile``, ``layer_start``, ``layer_source``,
        ``layer_complete``, ``coverage``, ``compose``)."""
        return self._t.turn_stream(
            s.turn_stream_path(session_id),
            s.turn_body(query, top_k, complete, model_override, research_profile),
        )

    def get(self, session_id: str) -> m.Session:
        """The session envelope + stored turn transcript."""
        return self._t.run(s.get_session(session_id))

    def close(self, session_id: str) -> m.Session:
        """Close the session (a write — ro tokens are refused). Idempotent:
        closing a closed/expired session returns its state unchanged."""
        return self._t.run(s.close_session(session_id))


class RestTokens:
    """Capability-token management (mgmt role). "Tokens" here are the
    short-lived end-user capability JWTs — not the bearer this client
    authenticates with."""

    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def mint(
        self,
        *,
        uid: str,
        access_level: int = 0,
        compartments: list[str] | None = None,
        scopes: list[str],
        runbook_refs: list[str] | None = None,
        ttl_secs: int | None = None,
    ) -> m.TokenGrant:
        """Mint a capability JWT for an authenticated end user. The token
        material is returned ONCE and never persisted server-side."""
        return self._t.run(
            s.mint_token(uid, access_level, compartments or [], scopes, runbook_refs, ttl_secs)
        )

    def list(self, *, uid: str | None = None, active: bool | None = None) -> list[m.TokenInfo]:
        """The issuance audit — metadata only, never token material.
        ``active=True`` = unexpired + unrevoked only."""
        return self._t.run(s.list_tokens(uid, active))

    def revoke(self, jti: str) -> m.RevokeResult:
        """Deny-list a token by jti. Note ``revocation_check_enabled`` in
        the response: the list is only consulted when the server enables
        it."""
        return self._t.run(s.revoke_token(jti))


class RestReports:
    """Management reports over the interactions audit trail (mgmt role)."""

    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def usage(
        self,
        *,
        group_by: str | None = None,
        from_: str | None = None,
        to: str | None = None,
    ) -> m.UsageReport:
        """Usage rollup. ``group_by``: uid | session | runbook | collection
        (server default: uid); ``from_``/``to`` are RFC 3339 bounds."""
        return self._t.run(s.usage_report(group_by, from_, to))

    def audit(
        self,
        *,
        uid: str | None = None,
        session_id: str | None = None,
        runbook: str | None = None,
        from_: str | None = None,
        to: str | None = None,
        limit: int | None = None,
        bodies: bool = False,
        before: str | None = None,
    ) -> m.AuditPage:
        """The audit trail. ``bodies`` includes the captured
        request/response bodies (heavy; off by default); ``before`` is the
        previous page's ``next_before``, verbatim."""
        return self._t.run(
            s.audit_report(uid, session_id, runbook, from_, to, limit, bodies, before)
        )

    def cost(self, *, from_: str | None = None, to: str | None = None) -> m.CostReport:
        """Model-spend token rollup (dollar pricing lives upstream)."""
        return self._t.run(s.cost_report(from_, to))

    def timeseries(
        self, *, window: str | None = None, plane: str | None = None
    ) -> m.TimeseriesReport:
        """Bucketed request/error/latency series. ``window``: 1h | 24h |
        7d | 30d (server default 24h); ``plane``: rest | grpc."""
        return self._t.run(s.timeseries_report(window, plane))

    def endpoints(
        self, *, window: str | None = None, limit: int | None = None
    ) -> m.EndpointsReport:
        return self._t.run(s.endpoints_report(window, limit))

    def runbooks(self, *, window: str | None = None) -> m.RunbookReport:
        return self._t.run(s.runbooks_report(window))

    def sessions(self, *, window: str | None = None) -> m.SessionsReport:
        return self._t.run(s.sessions_report(window))

    def evidence(self, *, window: str | None = None) -> m.EvidenceReport:
        """How the evidence hierarchy behaved. ``window``: 24h |
        7d | 30d (server default 24h). Answers the operator question no
        error rate can — which layer is quietly refusing — because a
        refusing layer still returns 200."""
        return self._t.run(s.evidence_report(window))

    def matrix(self) -> m.MatrixReport:
        """Munarium Matrix's health as this server sees it.
        ``configured=False`` means the plane was never wired, which is not
        the same as wired-and-failing."""
        return self._t.run(s.matrix_report())


class RestAuthoring:
    """Guided runbook authoring: pattern catalog, interview-driven drafts,
    deterministic validation, optional AI assist, hash-manifested export,
    and apply.

    ``delete_draft`` is the client surface's ONE delete — it removes a
    workspace draft (soft), never ledger data, so the append-only invariant
    is untouched."""

    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def list_patterns(self) -> list[m.PatternSummary]:
        return self._t.run(s.list_patterns())

    def get_pattern(self, id: str) -> m.PatternDetail:
        return self._t.run(s.get_pattern(id))

    def create_draft(
        self,
        *,
        name: str,
        pattern_id: str | None = None,
        seed_from_exemplar: bool = False,
    ) -> m.Draft:
        return self._t.run(s.create_draft(name, pattern_id, seed_from_exemplar))

    def list_drafts(self) -> list[m.DraftSummary]:
        return self._t.run(s.list_drafts())

    def get_draft(self, draft_id: str) -> m.Draft:
        return self._t.run(s.get_draft(draft_id))

    def delete_draft(self, draft_id: str) -> m.DraftDeleteResult:
        return self._t.run(s.delete_draft(draft_id))

    def put_answers(self, draft_id: str, answers: Any, *, materialize: bool = True) -> m.Draft:
        """Replace the stored answers (and by default re-materialize
        documents)."""
        return self._t.run(s.put_answers(draft_id, answers, materialize))

    def validate(self, draft_id: str) -> m.DraftValidation:
        return self._t.run(s.validate_draft(draft_id))

    def assist(
        self,
        draft_id: str,
        *,
        description: str | None = None,
        instructions: str | None = None,
        provider: str | None = None,
        model: str | None = None,
        tier: str | None = None,
    ) -> m.AssistResult:
        """AI-assisted drafting pass. NEVER fails the request: a degraded
        pass (no provider, budget, parse failure) sets ``assist_note``
        instead."""
        return self._t.run(
            s.assist_draft(draft_id, description, instructions, provider, model, tier)
        )

    def export(self, draft_id: str) -> m.ExportBundle:
        """Self-contained hash-manifested bundle (shapes before runbooks in
        ``apply_order``)."""
        return self._t.run(s.export_draft(draft_id))

    def apply(self, draft_id: str) -> m.ApplyDraftResult:
        """Apply the draft's documents to THIS server (validates inline)."""
        return self._t.run(s.apply_draft(draft_id))


# ---------------------------------------------------------------------------
# async
# ---------------------------------------------------------------------------


class AsyncRestCommands:
    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def create_version(
        self,
        parent_version_id: str | None = None,
        metadata: Any | None = None,
        idempotency_key: str | None = None,
    ) -> str:
        return await self._t.run(s.create_version(parent_version_id, metadata), idempotency_key)

    async def propose_claim(
        self,
        version_id: str,
        *,
        expected_head: int | None = None,
        idempotency_key: str | None = None,
        **claim: Any,
    ) -> m.ClaimOutcome:
        return await self._t.run(
            s.propose_claim(version_id, m.ClaimInput(**claim), expected_head),
            idempotency_key,
        )

    async def append_events(
        self,
        version_id: str,
        claims: list[m.ClaimInput | dict[str, Any]],
        *,
        candidate_text: str | None = None,
        expected_head: int | None = None,
        idempotency_key: str | None = None,
    ) -> m.EventsOutcome:
        inputs = [m.coerce(m.ClaimInput, c) for c in claims]
        return await self._t.run(
            s.append_events(version_id, inputs, candidate_text, expected_head),
            idempotency_key,
        )

    async def open_promise(
        self,
        version_id: str,
        *,
        key: str,
        kind: str,
        description: str,
        origin_scope: str | None = None,
        due_scope: str | None = None,
        idempotency_key: str | None = None,
    ) -> m.Promise:
        return await self._t.run(
            s.open_promise(version_id, key, kind, description, origin_scope, due_scope),
            idempotency_key,
        )

    async def fulfill_promise(
        self, version_id: str, key: str, idempotency_key: str | None = None
    ) -> bool:
        return await self._t.run(s.fulfill_promise(version_id, key), idempotency_key)

    async def lock_anchor(
        self,
        version_id: str,
        *,
        subject: str,
        key: str,
        value: str,
        scope_path: str | None = None,
        evidence: Any | None = None,
        idempotency_key: str | None = None,
    ) -> m.Anchor:
        return await self._t.run(
            s.lock_anchor(version_id, subject, key, value, scope_path, evidence),
            idempotency_key,
        )

    async def record_counts(
        self,
        version_id: str,
        *,
        key: str,
        scope_path: str,
        count: int,
        budget: int | None = None,
        idempotency_key: str | None = None,
    ) -> None:
        await self._t.run(
            s.record_counts(version_id, key, scope_path, count, budget), idempotency_key
        )

    async def upsert_digest(self, digest: m.Digest) -> None:
        await self._t.run(s.upsert_digest(digest))


class AsyncRestQuery:
    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def head(self, version_id: str) -> int:
        return await self._t.run(s.head(version_id))

    async def get_claim(self, claim_id: str) -> m.ClaimLookup:
        return await self._t.run(s.get_claim(claim_id))

    async def facts(
        self,
        version_id: str,
        *,
        scope_prefix: str | None = None,
        as_of_seq: int | None = None,
        statuses: tuple[m.ClaimStatus, ...] = (),
        limit: int | None = None,
    ) -> m.FactsPage:
        return await self._t.run(s.facts(version_id, scope_prefix, as_of_seq, statuses, limit))

    async def lineage(self, version_id: str) -> list[str]:
        return await self._t.run(s.lineage(version_id))

    async def anchors(self, version_id: str, as_of_seq: int | None = None) -> list[m.Anchor]:
        return await self._t.run(s.anchors(version_id, as_of_seq))

    async def promises(
        self,
        version_id: str,
        as_of_seq: int | None = None,
        status: str | None = None,
    ) -> list[m.Promise]:
        return await self._t.run(s.promises(version_id, as_of_seq, status))

    async def counters(self, version_id: str, as_of_seq: int | None = None) -> list[m.Counter]:
        return await self._t.run(s.counters(version_id, as_of_seq))

    async def digests(self, version_id: str) -> list[m.Digest]:
        return await self._t.run(s.digests(version_id))

    async def compose_context(
        self,
        version_id: str,
        *,
        scope: str | None = None,
        budget_tokens: int | None = None,
        fact_limit: int | None = None,
        as_of_seq: int | None = None,
    ) -> m.ComposedContext:
        return await self._t.run(
            s.compose_context(version_id, scope, budget_tokens, fact_limit, as_of_seq)
        )

    async def findings(
        self,
        version_id: str,
        *,
        as_of_seq: int | None = None,
        severity: str | None = None,
        rule_id: str | None = None,
        limit: int | None = None,
    ) -> list[m.StoredFinding]:
        """Persisted gate findings with the head seq each write settled at."""
        return await self._t.run(s.findings(version_id, as_of_seq, severity, rule_id, limit))


class AsyncRestIngest:
    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def put_source(
        self,
        data: ChunkSource,
        *,
        declared_sha256: str = "",
        media_type: str | None = None,
        filename: str | None = None,
        shape_ref: str | None = None,
    ) -> m.PutSourceResult:
        raw = await self._t.put_source(data, declared_sha256, media_type, filename, shape_ref)
        return m.PutSourceResult.model_validate(raw)

    async def record_ingest(
        self, version_id: str, *, content_hash: str, shape_ref: str | None = None
    ) -> m.RecordIngestResult:
        return await self._t.run(s.record_ingest(version_id, content_hash, shape_ref))

    async def ingest(self, file: m.IngestFile | dict[str, Any]) -> m.IngestResult:
        """Ingest ONE document through the file plane (base64 body,
        declarative collection auto-binding). Requires the ingest scope."""
        return await self._t.run(s.ingest(m.coerce(m.IngestFile, file)))

    async def ingest_batch(
        self, files: list[m.IngestFile | dict[str, Any]]
    ) -> list[m.IngestResult]:
        """Batch ingest (1..=500 files, client-guarded) with per-item
        outcomes — one failed file does not fail the batch."""
        return await self._t.run(s.ingest_batch([m.coerce(m.IngestFile, f) for f in files]))

    async def bulk_open(
        self,
        files: list[m.BulkManifestEntry | dict[str, Any]],
        *,
        label: str | None = None,
    ) -> m.BulkOpenResult:
        """Open a bulk upload session from a manifest. The response's
        ``needed`` is the upload work list."""
        return await self._t.run(
            s.bulk_open([m.coerce(m.BulkManifestEntry, e) for e in files], label)
        )

    async def bulk_chunk(
        self, bulk_id: str, files: list[m.IngestFile | dict[str, Any]]
    ) -> m.BulkChunkResult:
        """Upload one chunk of files (at most 500 — a larger list is a
        typed client-side error). Per-document idempotent."""
        return await self._t.run(s.bulk_chunk(bulk_id, [m.coerce(m.IngestFile, f) for f in files]))

    async def bulk_status(self, bulk_id: str, *, include_needed: bool = False) -> m.BulkStatus:
        return await self._t.run(s.bulk_status(bulk_id, include_needed))

    async def bulk_complete(self, bulk_id: str) -> m.BulkCompleteResult:
        """Close the session against its manifest: "completed" when every
        entry is stored and hash-matched, else "incomplete"."""
        return await self._t.run(s.bulk_complete(bulk_id))

    async def get_source(self, source_id: str) -> m.SourceInfo:
        """Metadata for one stored source (never the bytes)."""
        return await self._t.run(s.get_source(source_id))


class AsyncRestRetrieval:
    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def search(
        self,
        *,
        query: str,
        shape_ref: str,
        top_k: int | None = None,
        index_version: str | None = None,
        filter: Any | None = None,
    ) -> m.SearchResult:
        return await self._t.run(s.search(query, shape_ref, top_k, index_version, filter))

    async def index_status(self, shape_ref: str) -> m.IndexStatus:
        return await self._t.run(s.index_status(shape_ref))

    async def build_index(self, shape_ref: str, version_id: str | None = None) -> m.IndexStatus:
        return await self._t.run(s.build_index(shape_ref, version_id))

    async def create_collection(
        self,
        *,
        name: str,
        shape_ref: str,
        access_level: int = 0,
        compartments: list[str] | None = None,
        description: str | None = None,
    ) -> m.Collection:
        """Create-or-update a compartmentalized collection. There is no
        delete anywhere — collections retire softly."""
        return await self._t.run(
            s.create_collection(name, shape_ref, access_level, compartments or [], description)
        )

    async def list_collections(self) -> list[m.Collection]:
        return await self._t.run(s.list_collections())

    async def get_collection(self, id: str) -> m.Collection:
        return await self._t.run(s.get_collection(id))


class AsyncRestRunbooks:
    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def apply_shape(self, yaml: str, version_id: str | None = None) -> m.ApplyShapeResult:
        return await self._t.run(s.apply_shape(yaml, version_id))

    async def apply_runbook(self, yaml: str) -> str:
        return await self._t.run(s.apply_runbook(yaml))

    async def run_runbook(self, name: str, version_id: str | None = None) -> m.RunbookRun:
        return await self._t.run(s.run_runbook(name, version_id))

    async def get_run(self, run_id: str) -> m.RunStatus:
        return await self._t.run(s.get_run(run_id))

    async def approve_step(self, run_id: str, ordinal: int) -> m.RunbookRun:
        return await self._t.run(s.approve_step(run_id, ordinal))

    async def list(self, include_removed: bool = False) -> list[m.RunbookSummary]:
        """Every hosted runbook (all versions) with per-collection access
        requirements."""
        return await self._t.run(s.list_runbooks(include_removed))

    async def get_info(self, name: str) -> m.RunbookInfo:
        """One runbook's collections, sibling versions, models block, and
        retrieval knobs. ``name`` is a bare name (latest) or exact
        name@version."""
        return await self._t.run(s.runbook_info(name))

    async def validate(
        self,
        yaml: str,
        *,
        suggest: bool = False,
        provider: str | None = None,
        model: str | None = None,
        tier: str | None = None,
    ) -> m.ValidateResult:
        """Deterministic validation findings; ``suggest`` adds AI
        improvement suggestions (a BYOK provider call)."""
        return await self._t.run(s.validate_runbook(yaml, suggest, provider, model, tier))

    async def remove_request(self, name: str) -> m.RemovalRequest:
        """First pass of the double-pass soft removal. ``name`` must be an
        EXACT name@version."""
        return await self._t.run(s.remove_request(name))

    async def remove_confirm(self, name: str, removal_id: str) -> m.RemovalConfirmResult:
        """Second pass: confirm with the removal_id. Removal is
        visibility-only."""
        return await self._t.run(s.remove_confirm(name, removal_id))

    async def apply_chronology_rules(self, yaml: str) -> m.ApplyChronologyResult:
        """Apply (upsert) a chronology-rules asset — the sixth gate's
        arming surface. text/yaml like shapes."""
        return await self._t.run(s.apply_chronology_rules(yaml))

    async def get_chronology_rules(self, name: str) -> str:
        """The applied rules YAML back, verbatim."""
        return await self._t.run(s.get_chronology_rules(name))


class AsyncRestProviders:
    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def apply_config(self, yaml: str) -> str:
        return await self._t.run(s.apply_provider(yaml))

    async def health(self, name: str) -> m.ProviderHealth:
        return await self._t.run(s.provider_health(name))

    async def health_ai(self) -> m.HealthAiResult:
        """Live probe of the server's six built-in default models (three
        provider families x two tiers) — spends real provider tokens."""
        return await self._t.run(s.health_ai())

    async def complete(
        self,
        name: str,
        *,
        prompt: str,
        model: str | None = None,
        provider: str | None = None,
        tier: str | None = None,
        system: str | None = None,
        max_tokens: int | None = None,
        temperature: float | None = None,
        version_id: str | None = None,
    ) -> m.CompleteResult:
        return await self._t.run(
            s.complete(
                name,
                prompt,
                model,
                system,
                max_tokens,
                temperature,
                version_id,
                provider=provider,
                tier=tier,
            )
        )

    async def embed(
        self,
        name: str,
        *,
        inputs: list[str],
        model: str | None = None,
        provider: str | None = None,
        version_id: str | None = None,
    ) -> m.EmbedResult:
        return await self._t.run(s.embed(name, inputs, model, version_id, provider=provider))

    async def list(self) -> list[m.ProviderModels]:
        """Free disclosure of every provider config visible to the tenant —
        zero provider calls; the credential itself is never echoed."""
        return await self._t.run(s.list_providers())

    async def max_tokens(self) -> m.MaxTokensResponse:
        """GET /v1/max-tokens — the effective per-call output-token budgets
        for the caller's tenant and where they come from (``source`` is
        ``tenant`` after a replacement, else ``environment``). Any
        authenticated role; zero provider calls. REST-only."""
        return await self._t.run(s.get_max_tokens())

    async def replace_max_tokens(
        self, budgets: m.MaxTokensBudgets | dict[str, Any]
    ) -> m.MaxTokensResponse:
        """POST /v1/max-tokens — replace the tenant's WHOLE budget set (no
        partial update: all eight fields are sent, a dict missing one fails
        model validation before the wire, an out-of-range value is the
        server's 400 ``invalid-input``). Answers with the same shape
        ``max_tokens()`` returns. Static **rw** role only. REST-only."""
        return await self._t.run(s.replace_max_tokens(m.coerce(m.MaxTokensBudgets, budgets)))


class AsyncRestSessions:
    """Async twin of :class:`RestSessions` (see there for the contracts)."""

    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def create(self, runbook_name: str) -> m.CreateSessionResult:
        return await self._t.run(s.create_session(runbook_name))

    async def turn(
        self,
        session_id: str,
        *,
        query: str,
        top_k: int | None = None,
        complete: bool | None = None,
        model_override: m.ModelOverride | dict[str, Any] | None = None,
        research_profile: str | None = None,
    ) -> m.TurnResult:
        return await self._t.run(
            s.turn(session_id, query, top_k, complete, model_override, research_profile)
        )

    def turn_stream(
        self,
        session_id: str,
        *,
        query: str,
        top_k: int | None = None,
        complete: bool | None = None,
        model_override: m.ModelOverride | dict[str, Any] | None = None,
        research_profile: str | None = None,
    ) -> AsyncIterator[m.TurnStreamEvent]:
        """The streamed turn as an async iterator: N TurnProgress events,
        then exactly one TurnResult — always the LAST item yielded. Typed
        errors (pre-stream or mid-stream) raise during iteration."""
        return self._t.turn_stream(
            s.turn_stream_path(session_id),
            s.turn_body(query, top_k, complete, model_override, research_profile),
        )

    async def get(self, session_id: str) -> m.Session:
        return await self._t.run(s.get_session(session_id))

    async def close(self, session_id: str) -> m.Session:
        return await self._t.run(s.close_session(session_id))


class AsyncRestTokens:
    """Async twin of :class:`RestTokens`."""

    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def mint(
        self,
        *,
        uid: str,
        access_level: int = 0,
        compartments: list[str] | None = None,
        scopes: list[str],
        runbook_refs: list[str] | None = None,
        ttl_secs: int | None = None,
    ) -> m.TokenGrant:
        return await self._t.run(
            s.mint_token(uid, access_level, compartments or [], scopes, runbook_refs, ttl_secs)
        )

    async def list(
        self, *, uid: str | None = None, active: bool | None = None
    ) -> list[m.TokenInfo]:
        return await self._t.run(s.list_tokens(uid, active))

    async def revoke(self, jti: str) -> m.RevokeResult:
        return await self._t.run(s.revoke_token(jti))


class AsyncRestReports:
    """Async twin of :class:`RestReports`."""

    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def usage(
        self,
        *,
        group_by: str | None = None,
        from_: str | None = None,
        to: str | None = None,
    ) -> m.UsageReport:
        return await self._t.run(s.usage_report(group_by, from_, to))

    async def audit(
        self,
        *,
        uid: str | None = None,
        session_id: str | None = None,
        runbook: str | None = None,
        from_: str | None = None,
        to: str | None = None,
        limit: int | None = None,
        bodies: bool = False,
        before: str | None = None,
    ) -> m.AuditPage:
        return await self._t.run(
            s.audit_report(uid, session_id, runbook, from_, to, limit, bodies, before)
        )

    async def cost(self, *, from_: str | None = None, to: str | None = None) -> m.CostReport:
        return await self._t.run(s.cost_report(from_, to))

    async def timeseries(
        self, *, window: str | None = None, plane: str | None = None
    ) -> m.TimeseriesReport:
        return await self._t.run(s.timeseries_report(window, plane))

    async def endpoints(
        self, *, window: str | None = None, limit: int | None = None
    ) -> m.EndpointsReport:
        return await self._t.run(s.endpoints_report(window, limit))

    async def runbooks(self, *, window: str | None = None) -> m.RunbookReport:
        return await self._t.run(s.runbooks_report(window))

    async def sessions(self, *, window: str | None = None) -> m.SessionsReport:
        return await self._t.run(s.sessions_report(window))

    async def evidence(self, *, window: str | None = None) -> m.EvidenceReport:
        return await self._t.run(s.evidence_report(window))

    async def matrix(self) -> m.MatrixReport:
        return await self._t.run(s.matrix_report())


class AsyncRestAuthoring:
    """Async twin of :class:`RestAuthoring`."""

    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def list_patterns(self) -> list[m.PatternSummary]:
        return await self._t.run(s.list_patterns())

    async def get_pattern(self, id: str) -> m.PatternDetail:
        return await self._t.run(s.get_pattern(id))

    async def create_draft(
        self,
        *,
        name: str,
        pattern_id: str | None = None,
        seed_from_exemplar: bool = False,
    ) -> m.Draft:
        return await self._t.run(s.create_draft(name, pattern_id, seed_from_exemplar))

    async def list_drafts(self) -> list[m.DraftSummary]:
        return await self._t.run(s.list_drafts())

    async def get_draft(self, draft_id: str) -> m.Draft:
        return await self._t.run(s.get_draft(draft_id))

    async def delete_draft(self, draft_id: str) -> m.DraftDeleteResult:
        return await self._t.run(s.delete_draft(draft_id))

    async def put_answers(
        self, draft_id: str, answers: Any, *, materialize: bool = True
    ) -> m.Draft:
        return await self._t.run(s.put_answers(draft_id, answers, materialize))

    async def validate(self, draft_id: str) -> m.DraftValidation:
        return await self._t.run(s.validate_draft(draft_id))

    async def assist(
        self,
        draft_id: str,
        *,
        description: str | None = None,
        instructions: str | None = None,
        provider: str | None = None,
        model: str | None = None,
        tier: str | None = None,
    ) -> m.AssistResult:
        return await self._t.run(
            s.assist_draft(draft_id, description, instructions, provider, model, tier)
        )

    async def export(self, draft_id: str) -> m.ExportBundle:
        return await self._t.run(s.export_draft(draft_id))

    async def apply(self, draft_id: str) -> m.ApplyDraftResult:
        return await self._t.run(s.apply_draft(draft_id))


class RestEvidence:
    """Sealed evidence READS. REST-only; the gRPC transport raises
    ``UnsupportedError``.

    Sealing is deliberately not here. An artifact's manifest is a statement
    about work the sealer did — a general SDK offering ``seal_evidence`` would
    invite an application to assert provenance it cannot vouch for. What an
    application legitimately needs is the other direction: an answer cites
    ``[evidence/<id>#<row>]`` and the application resolves that citation to
    show a reader what the number was computed from.

    Access is checked per artifact against the **session's** clearance, not the
    sealer's. Expect ``evidence-forbidden`` (403), ``evidence-expired`` (410 —
    retention purged the bytes, and the citation was real) and
    ``evidence-not-committed`` (409).
    """

    def __init__(self, t: SyncRestTransport) -> None:
        self._t = t

    def get(self, evidence_id: str) -> dict[str, Any]:
        """The manifest, verbatim as the contract defines it."""
        return self._t.run(s.evidence(evidence_id))

    def rows(
        self,
        evidence_id: str,
        *,
        from_: int | None = None,
        limit: int | None = None,
    ) -> m.EvidenceRows:
        """A bounded, audited window over the sealed rows."""
        return self._t.run(s.evidence_rows(evidence_id, from_, limit))


class AsyncRestEvidence:
    """Async twin of :class:`RestEvidence`."""

    def __init__(self, t: AsyncRestTransport) -> None:
        self._t = t

    async def get(self, evidence_id: str) -> dict[str, Any]:
        return await self._t.run(s.evidence(evidence_id))

    async def rows(
        self,
        evidence_id: str,
        *,
        from_: int | None = None,
        limit: int | None = None,
    ) -> m.EvidenceRows:
        return await self._t.run(s.evidence_rows(evidence_id, from_, limit))
