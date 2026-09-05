# SPDX-License-Identifier: Apache-2.0
"""REST call specifications shared by the sync and async transports: path
building (percent-encoded segments), request bodies, and response parsing.
Keeping these in one place means the two clients cannot drift."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any, Generic, Literal, TypeVar
from urllib.parse import quote

from . import models as m
from ._errors import check_bulk_files, check_promise_status

RetryClass = Literal["read", "command", "write"]

T = TypeVar("T")


def seg(s: str) -> str:
    """Percent-encode a path segment — promise keys, shape refs, and runbook
    names are free-form; a raw '/' or '?' must not change the route shape."""
    return quote(s, safe="")


@dataclass(slots=True)
class Spec(Generic[T]):
    method: str
    path: str
    parse: Callable[[Any], T]
    params: dict[str, str] = field(default_factory=dict)
    json: dict[str, Any] | None = None
    yaml: str | None = None
    retry: RetryClass = "read"
    #: "exempt" sends WITHOUT the per-request deadline (streaming-ingest
    #: posture): unary turns spend provider tokens a client-side abort
    #: cannot stop, and the file/bulk ingest bodies run to the server's
    #: 256 MiB ceiling — a 30 s cap on either is a trap.
    timeout: Literal["default", "exempt"] = "default"
    #: Stream JSON incrementally instead of asking httpx to encode a second
    #: request-sized byte array. Reserved for the file/bulk ingest surface,
    #: whose DTOs may already hold hundreds of MiB of base64 content.
    stream_json: bool = False
    #: True = the success body is text, not JSON (chronology-rules readback
    #: returns the applied YAML verbatim); errors stay problem+json.
    raw: bool = False


def _drop_none(d: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in d.items() if v is not None}


def _params(**kw: str | int | bool | None) -> dict[str, str]:
    """Build a query-param dict: ``None`` dropped, ints stringified, bools
    rendered ``"true"``/``"false"``. A trailing underscore is stripped from
    the key (``from_`` -> ``from``, the reserved word). Flag params whose
    False means "absent" (``suggest``, ``bodies``, ``include_*``) are passed
    as ``flag or None`` by the caller so an explicit ``False`` is omitted,
    while a tri-state ``bool | None`` (``active``) carries both values."""
    out: dict[str, str] = {}
    for k, v in kw.items():
        if v is None:
            continue
        out[k.rstrip("_")] = ("true" if v else "false") if isinstance(v, bool) else str(v)
    return out


def claim_body(c: m.ClaimInput, expected_head: int | None = None) -> dict[str, Any]:
    body = c.model_dump(exclude_none=True)
    if expected_head is not None:
        body["expected_head"] = expected_head
    return body


# -- commands ---------------------------------------------------------------


def create_version(parent_version_id: str | None, metadata: Any | None) -> Spec[str]:
    return Spec(
        "POST",
        "/v1/versions",
        parse=lambda v: str(v["version_id"]),
        json=_drop_none({"parent_version_id": parent_version_id, "metadata": metadata}),
        retry="command",
    )


def propose_claim(
    version_id: str, claim: m.ClaimInput, expected_head: int | None
) -> Spec[m.ClaimOutcome]:
    return Spec(
        "POST",
        f"/v1/versions/{seg(version_id)}/claims",
        parse=m.ClaimOutcome.model_validate,
        json=claim_body(claim, expected_head),
        retry="command",
    )


def append_events(
    version_id: str,
    claims: list[m.ClaimInput],
    candidate_text: str | None,
    expected_head: int | None,
) -> Spec[m.EventsOutcome]:
    body: dict[str, Any] = {"claims": [claim_body(c) for c in claims]}
    if candidate_text is not None:
        body["candidate_text"] = candidate_text
    if expected_head is not None:
        body["expected_head"] = expected_head
    return Spec(
        "POST",
        f"/v1/versions/{seg(version_id)}/events",
        parse=m.EventsOutcome.model_validate,
        json=body,
        retry="command",
    )


def open_promise(
    version_id: str,
    key: str,
    kind: str,
    description: str,
    origin_scope: str | None,
    due_scope: str | None,
) -> Spec[m.Promise]:
    return Spec(
        "POST",
        f"/v1/versions/{seg(version_id)}/promises",
        parse=m.Promise.model_validate,
        json=_drop_none(
            {
                "key": key,
                "kind": kind,
                "description": description,
                "origin_scope": origin_scope,
                "due_scope": due_scope,
            }
        ),
        retry="command",
    )


def fulfill_promise(version_id: str, key: str) -> Spec[bool]:
    return Spec(
        "POST",
        f"/v1/versions/{seg(version_id)}/promises/{seg(key)}/fulfill",
        parse=lambda v: bool(v["fulfilled"]),
        json={},
        retry="command",
    )


def lock_anchor(
    version_id: str,
    subject: str,
    key: str,
    value: str,
    scope_path: str | None,
    evidence: Any | None,
) -> Spec[m.Anchor]:
    return Spec(
        "POST",
        f"/v1/versions/{seg(version_id)}/anchors",
        parse=m.Anchor.model_validate,
        json=_drop_none(
            {
                "subject": subject,
                "key": key,
                "value": value,
                "scope_path": scope_path,
                "evidence": evidence,
            }
        ),
        retry="command",
    )


def record_counts(
    version_id: str, key: str, scope_path: str, count: int, budget: int | None
) -> Spec[None]:
    return Spec(
        "POST",
        f"/v1/versions/{seg(version_id)}/counters",
        parse=lambda _v: None,
        json=_drop_none({"key": key, "scope_path": scope_path, "count": count, "budget": budget}),
        retry="command",
    )


def upsert_digest(digest: m.Digest) -> Spec[None]:
    return Spec(
        "PUT",
        f"/v1/versions/{seg(digest.version_id)}/digests",
        parse=lambda _v: None,
        json=digest.model_dump(),
        retry="write",  # upsert by definition — outside REST idempotency scope
    )


# -- query ------------------------------------------------------------------


def head(version_id: str) -> Spec[int]:
    return Spec(
        "GET",
        f"/v1/versions/{seg(version_id)}/head",
        parse=lambda v: int(v["head_seq"]),
    )


def get_claim(claim_id: str) -> Spec[m.ClaimLookup]:
    return Spec("GET", f"/v1/claims/{seg(claim_id)}", parse=m.ClaimLookup.model_validate)


def facts(
    version_id: str,
    scope_prefix: str | None,
    as_of_seq: int | None,
    statuses: tuple[m.ClaimStatus, ...],
    limit: int | None,
) -> Spec[m.FactsPage]:
    return Spec(
        "GET",
        f"/v1/versions/{seg(version_id)}/facts",
        parse=m.FactsPage.model_validate,
        params=_params(
            scope_prefix=scope_prefix,
            as_of_seq=as_of_seq,
            statuses=",".join(statuses) or None,
            limit=limit,
        ),
    )


def lineage(version_id: str) -> Spec[list[str]]:
    return Spec(
        "GET",
        f"/v1/versions/{seg(version_id)}/lineage",
        parse=lambda v: [str(x) for x in v["version_ids"]],
    )


def anchors(version_id: str, as_of_seq: int | None) -> Spec[list[m.Anchor]]:
    return Spec(
        "GET",
        f"/v1/versions/{seg(version_id)}/anchors",
        parse=lambda v: [m.Anchor.model_validate(a) for a in v["anchors"]],
        params=_params(as_of_seq=as_of_seq),
    )


def promises(version_id: str, as_of_seq: int | None, status: str | None) -> Spec[list[m.Promise]]:
    check_promise_status(status)
    return Spec(
        "GET",
        f"/v1/versions/{seg(version_id)}/promises",
        parse=lambda v: [m.Promise.model_validate(p) for p in v["promises"]],
        params=_params(as_of_seq=as_of_seq, status=status),
    )


def counters(version_id: str, as_of_seq: int | None) -> Spec[list[m.Counter]]:
    return Spec(
        "GET",
        f"/v1/versions/{seg(version_id)}/counters",
        parse=lambda v: [m.Counter.model_validate(c) for c in v["counters"]],
        params=_params(as_of_seq=as_of_seq),
    )


def digests(version_id: str) -> Spec[list[m.Digest]]:
    return Spec(
        "GET",
        f"/v1/versions/{seg(version_id)}/digests",
        parse=lambda v: [m.Digest.model_validate(d) for d in v["digests"]],
    )


def compose_context(
    version_id: str,
    scope: str | None,
    budget_tokens: int | None,
    fact_limit: int | None,
    as_of_seq: int | None,
) -> Spec[m.ComposedContext]:
    return Spec(
        "GET",
        f"/v1/versions/{seg(version_id)}/context",
        parse=m.ComposedContext.model_validate,
        params=_params(
            scope=scope, budget_tokens=budget_tokens, fact_limit=fact_limit, as_of_seq=as_of_seq
        ),
    )


# -- ingest / retrieval / runbooks / providers ------------------------------


def record_ingest(
    version_id: str, content_hash: str, shape_ref: str | None
) -> Spec[m.RecordIngestResult]:
    return Spec(
        "POST",
        f"/v1/versions/{seg(version_id)}/ingests",
        parse=m.RecordIngestResult.model_validate,
        json=_drop_none({"content_hash": content_hash, "shape_ref": shape_ref}),
        retry="write",
    )


def search(
    query: str,
    shape_ref: str,
    top_k: int | None,
    index_version: str | None,
    filter: Any | None,
) -> Spec[m.SearchResult]:
    return Spec(
        "POST",
        "/v1/search",
        parse=m.SearchResult.model_validate,
        json=_drop_none(
            {
                "query": query,
                "shape_ref": shape_ref,
                "top_k": top_k,
                "index_version": index_version,
                "filter": filter,
            }
        ),
        retry="read",  # a read that happens to be a POST
    )


def index_status(shape_ref: str) -> Spec[m.IndexStatus]:
    return Spec("GET", f"/v1/indexes/{seg(shape_ref)}", parse=m.IndexStatus.model_validate)


def build_index(shape_ref: str, version_id: str | None) -> Spec[m.IndexStatus]:
    return Spec(
        "POST",
        f"/v1/indexes/{seg(shape_ref)}/build",
        parse=m.IndexStatus.model_validate,
        params=_params(version_id=version_id),
        retry="write",
    )


def apply_shape(yaml: str, version_id: str | None) -> Spec[m.ApplyShapeResult]:
    return Spec(
        "POST",
        "/v1/shapes",
        parse=m.ApplyShapeResult.model_validate,
        params=_params(version_id=version_id),
        yaml=yaml,
        retry="write",
    )


def apply_runbook(yaml: str) -> Spec[str]:
    return Spec(
        "POST",
        "/v1/runbooks",
        parse=lambda v: str(v["runbook_ref"]),
        yaml=yaml,
        retry="write",
    )


def run_runbook(name: str, version_id: str | None) -> Spec[m.RunbookRun]:
    return Spec(
        "POST",
        f"/v1/runbooks/{seg(name)}/runs",
        parse=m.RunbookRun.model_validate,
        params=_params(version_id=version_id),
        retry="write",
    )


def get_run(run_id: str) -> Spec[m.RunStatus]:
    return Spec("GET", f"/v1/runs/{seg(run_id)}", parse=m.RunStatus.model_validate)


def approve_step(run_id: str, ordinal: int) -> Spec[m.RunbookRun]:
    return Spec(
        "POST",
        f"/v1/runs/{seg(run_id)}/steps/{ordinal}/approve",
        parse=m.RunbookRun.model_validate,
        retry="write",
    )


def apply_provider(yaml: str) -> Spec[str]:
    return Spec(
        "POST",
        "/v1/providers",
        parse=lambda v: str(v["config_name"]),
        yaml=yaml,
        retry="write",
    )


def provider_health(name: str) -> Spec[m.ProviderHealth]:
    return Spec(
        "GET",
        f"/v1/providers/{seg(name)}/health",
        parse=m.ProviderHealth.model_validate,
    )


def health_ai() -> Spec[m.HealthAiResult]:
    return Spec(
        "GET",
        "/healthai",
        parse=m.HealthAiResult.model_validate,
    )


def complete(
    name: str,
    prompt: str,
    model: str | None,
    system: str | None,
    max_tokens: int | None,
    temperature: float | None,
    version_id: str | None,
    provider: str | None = None,
    tier: str | None = None,
) -> Spec[m.CompleteResult]:
    return Spec(
        "POST",
        f"/v1/providers/{seg(name)}/complete",
        parse=m.CompleteResult.model_validate,
        json=_drop_none(
            {
                "prompt": prompt,
                "model": model,
                "provider": provider,
                "tier": tier,
                "system": system,
                "max_tokens": max_tokens,
                "temperature": temperature,
                "version_id": version_id,
            }
        ),
        retry="write",
    )


def embed(
    name: str,
    inputs: list[str],
    model: str | None,
    version_id: str | None,
    provider: str | None = None,
) -> Spec[m.EmbedResult]:
    return Spec(
        "POST",
        f"/v1/providers/{seg(name)}/embed",
        parse=m.EmbedResult.model_validate,
        json=_drop_none(
            {
                "inputs": inputs,
                "model": model,
                "provider": provider,
                "version_id": version_id,
            }
        ),
        retry="write",
    )


# -- findings (2026-08-17) ---------------------------------------------------


def findings(
    version_id: str,
    as_of_seq: int | None,
    severity: str | None,
    rule_id: str | None,
    limit: int | None,
) -> Spec[list[m.StoredFinding]]:
    return Spec(
        "GET",
        f"/v1/versions/{seg(version_id)}/findings",
        parse=lambda v: [m.StoredFinding.model_validate(f) for f in v["findings"]],
        params=_params(as_of_seq=as_of_seq, severity=severity, rule_id=rule_id, limit=limit),
    )


# -- sealed evidence, reads only -------------------------------------
#
# Sealing is absent on purpose. An artifact's manifest is a statement about
# work the sealer did; an SDK offering `seal_evidence` would invite an
# application to assert provenance it cannot vouch for. What an application
# legitimately needs is the other direction: resolving an
# `[evidence/<id>#<row>]` citation to what a number was computed from.


def evidence(evidence_id: str) -> Spec[dict[str, Any]]:
    """The manifest, access-checked and audited.

    Returned UNWRAPPED — the route answers with the contract's
    `EvidenceManifest` itself — so this parses to a plain dict rather than
    inventing a Python mirror of a schema the contract already owns.
    """
    return Spec(
        "GET",
        f"/v1/evidence/{seg(evidence_id)}",
        parse=lambda v: v,
    )


def evidence_rows(
    evidence_id: str,
    from_: int | None,
    limit: int | None,
) -> Spec[m.EvidenceRows]:
    return Spec(
        "GET",
        f"/v1/evidence/{seg(evidence_id)}/rows",
        parse=m.EvidenceRows.model_validate,
        params=_params(**{"from": from_, "limit": limit}),
    )


# -- file / bulk ingestion ---------------------------------------------


def _ingest_file_body(f: m.IngestFile) -> dict[str, Any]:
    return f.model_dump(exclude_none=True)


def ingest(file: m.IngestFile) -> Spec[m.IngestResult]:
    return Spec(
        "POST",
        "/v1/ingest",
        parse=m.IngestResult.model_validate,
        json=_ingest_file_body(file),
        retry="write",
        timeout="exempt",
        stream_json=True,
    )


def ingest_batch(files: list[m.IngestFile]) -> Spec[list[m.IngestResult]]:
    check_bulk_files("batch", len(files))
    return Spec(
        "POST",
        "/v1/ingest/batch",
        parse=lambda v: [m.IngestResult.model_validate(r) for r in v["results"]],
        json={"files": [_ingest_file_body(f) for f in files]},
        retry="write",
        timeout="exempt",
        stream_json=True,
    )


def bulk_open(files: list[m.BulkManifestEntry], label: str | None) -> Spec[m.BulkOpenResult]:
    return Spec(
        "POST",
        "/v1/ingest/bulk",
        parse=m.BulkOpenResult.model_validate,
        json=_drop_none({"files": [f.model_dump() for f in files], "label": label}),
        retry="write",
        timeout="exempt",
        stream_json=True,
    )


def bulk_chunk(bulk_id: str, files: list[m.IngestFile]) -> Spec[m.BulkChunkResult]:
    check_bulk_files("bulk chunk", len(files))
    return Spec(
        "POST",
        f"/v1/ingest/bulk/{seg(bulk_id)}/chunk",
        parse=m.BulkChunkResult.model_validate,
        json={"files": [_ingest_file_body(f) for f in files]},
        retry="write",
        timeout="exempt",
        stream_json=True,
    )


def bulk_status(bulk_id: str, include_needed: bool) -> Spec[m.BulkStatus]:
    return Spec(
        "GET",
        f"/v1/ingest/bulk/{seg(bulk_id)}",
        parse=m.BulkStatus.model_validate,
        params=_params(include_needed=include_needed or None),
    )


def bulk_complete(bulk_id: str) -> Spec[m.BulkCompleteResult]:
    return Spec(
        "POST",
        f"/v1/ingest/bulk/{seg(bulk_id)}/complete",
        parse=m.BulkCompleteResult.model_validate,
        retry="write",
    )


def get_source(source_id: str) -> Spec[m.SourceInfo]:
    return Spec("GET", f"/v1/sources/{seg(source_id)}", parse=m.SourceInfo.model_validate)


# -- collections --------------------------------------------------------


def create_collection(
    name: str,
    shape_ref: str,
    access_level: int,
    compartments: list[str],
    description: str | None,
) -> Spec[m.Collection]:
    return Spec(
        "POST",
        "/v1/collections",
        parse=m.Collection.model_validate,
        json=_drop_none(
            {
                "name": name,
                "shape_ref": shape_ref,
                "access_level": access_level,
                "compartments": compartments,
                "description": description,
            }
        ),
        retry="write",
    )


def list_collections() -> Spec[list[m.Collection]]:
    return Spec(
        "GET",
        "/v1/collections",
        parse=lambda v: [m.Collection.model_validate(c) for c in v["collections"]],
    )


def get_collection(id: str) -> Spec[m.Collection]:
    return Spec("GET", f"/v1/collections/{seg(id)}", parse=m.Collection.model_validate)


# -- runbook management v2 ---------------------------------------------


def list_runbooks(include_removed: bool) -> Spec[list[m.RunbookSummary]]:
    return Spec(
        "GET",
        "/v1/runbooks",
        parse=lambda v: [m.RunbookSummary.model_validate(r) for r in v["runbooks"]],
        params=_params(include_removed=include_removed or None),
    )


def runbook_info(name: str) -> Spec[m.RunbookInfo]:
    return Spec("GET", f"/v1/runbooks/{seg(name)}", parse=m.RunbookInfo.model_validate)


def validate_runbook(
    yaml: str,
    suggest: bool,
    provider: str | None,
    model: str | None,
    tier: str | None,
) -> Spec[m.ValidateResult]:
    return Spec(
        "POST",
        "/v1/runbooks/validate",
        parse=m.ValidateResult.model_validate,
        params=_params(suggest=suggest or None, provider=provider, model=model, tier=tier),
        yaml=yaml,
        retry="write",  # with suggest=true this spends provider tokens
    )


def remove_request(name: str) -> Spec[m.RemovalRequest]:
    return Spec(
        "POST",
        f"/v1/runbooks/{seg(name)}/remove-request",
        parse=m.RemovalRequest.model_validate,
        retry="write",
    )


def remove_confirm(name: str, removal_id: str) -> Spec[m.RemovalConfirmResult]:
    return Spec(
        "POST",
        f"/v1/runbooks/{seg(name)}/remove-confirm",
        parse=m.RemovalConfirmResult.model_validate,
        json={"removal_id": removal_id},
        retry="write",
    )


def apply_chronology_rules(yaml: str) -> Spec[m.ApplyChronologyResult]:
    return Spec(
        "POST",
        "/v1/chronology-rules",
        parse=m.ApplyChronologyResult.model_validate,
        yaml=yaml,
        retry="write",
    )


def get_chronology_rules(name: str) -> Spec[str]:
    # The applied rules YAML back, verbatim — a text body, not JSON.
    return Spec("GET", f"/v1/chronology-rules/{seg(name)}", parse=str, raw=True)


# -- provider disclosure -----------------------------------------------------


def list_providers() -> Spec[list[m.ProviderModels]]:
    return Spec(
        "GET",
        "/v1/providers",
        parse=lambda v: [m.ProviderModels.model_validate(p) for p in v["providers"]],
    )


# -- max-tokens budgets ------------------------------------------------------


def max_tokens_body(budgets: m.MaxTokensBudgets) -> dict[str, Any]:
    """Exactly the eight budget fields. A ``MaxTokensResponse`` (the
    subclass carrying ``source``/``updated_at``) or a dict-coerced instance
    with extras still sends the replacement shape and nothing else."""
    return budgets.model_dump(include=set(m.MaxTokensBudgets.model_fields))


def get_max_tokens() -> Spec[m.MaxTokensResponse]:
    return Spec(
        "GET",
        "/v1/max-tokens",
        parse=m.MaxTokensResponse.model_validate,
    )


def replace_max_tokens(budgets: m.MaxTokensBudgets) -> Spec[m.MaxTokensResponse]:
    return Spec(
        "POST",
        "/v1/max-tokens",
        parse=m.MaxTokensResponse.model_validate,
        json=max_tokens_body(budgets),
        retry="write",  # a whole-set replace, sent once — like apply_provider
    )


# -- sessions + turns --------------------------------------------------


def create_session(runbook_name: str) -> Spec[m.CreateSessionResult]:
    return Spec(
        "POST",
        f"/v1/runbooks/{seg(runbook_name)}/sessions",
        parse=m.CreateSessionResult.model_validate,
        retry="write",  # opens server-side state — send once
    )


def turn_body(
    query: str,
    top_k: int | None,
    complete: bool | None,
    model_override: m.ModelOverride | dict[str, Any] | None,
    research_profile: str | None = None,
) -> dict[str, Any]:
    override = (
        m.coerce(m.ModelOverride, model_override).model_dump(exclude_none=True)
        if model_override is not None
        else None
    )
    # research_profile rides through _drop_none like every other optional:
    # a caller who does not use a profile must send the bytes it always did,
    # which is the governing invariant of the whole S-3.x surface.
    return _drop_none(
        {
            "query": query,
            "top_k": top_k,
            "complete": complete,
            "model_override": override,
            "research_profile": research_profile,
        }
    )


def turn(
    session_id: str,
    query: str,
    top_k: int | None,
    complete: bool | None,
    model_override: m.ModelOverride | dict[str, Any] | None,
    research_profile: str | None = None,
) -> Spec[m.TurnResult]:
    # A turn spends provider tokens — send-once, never auto-retried, and
    # DEADLINE-EXEMPT: a client-side abort does not stop the server's paid
    # completion (the transcript ordinal still advances), so a 30 s cap on
    # a capable-tier completion is a double-spend invitation. turn_stream
    # is the way to watch a long turn.
    return Spec(
        "POST",
        f"/v1/sessions/{seg(session_id)}/turns",
        parse=m.TurnResult.model_validate,
        json=turn_body(query, top_k, complete, model_override, research_profile),
        retry="write",
        timeout="exempt",
    )


def turn_stream_path(session_id: str) -> str:
    return f"/v1/sessions/{seg(session_id)}/turns/stream"


def get_session(session_id: str) -> Spec[m.Session]:
    return Spec("GET", f"/v1/sessions/{seg(session_id)}", parse=m.Session.model_validate)


def close_session(session_id: str) -> Spec[m.Session]:
    # Idempotent server-side (closing a closed session returns its state
    # unchanged), but still a write — sent once.
    return Spec(
        "POST",
        f"/v1/sessions/{seg(session_id)}/close",
        parse=m.Session.model_validate,
        retry="write",
    )


# -- access tokens (mgmt) ------------------------------------------------


def mint_token(
    uid: str,
    access_level: int,
    compartments: list[str],
    scopes: list[str],
    runbook_refs: list[str] | None,
    ttl_secs: int | None,
) -> Spec[m.TokenGrant]:
    return Spec(
        "POST",
        "/v1/access-tokens",
        parse=m.TokenGrant.model_validate,
        json=_drop_none(
            {
                "uid": uid,
                "access_level": access_level,
                "compartments": compartments,
                "scopes": scopes,
                "runbook_refs": runbook_refs,
                "ttl_secs": ttl_secs,
            }
        ),
        retry="write",  # minting twice issues two live tokens — send once
    )


def list_tokens(uid: str | None, active: bool | None) -> Spec[list[m.TokenInfo]]:
    return Spec(
        "GET",
        "/v1/access-tokens",
        parse=lambda v: [m.TokenInfo.model_validate(t) for t in v["tokens"]],
        params=_params(uid=uid, active=active),
    )


def revoke_token(jti: str) -> Spec[m.RevokeResult]:
    return Spec(
        "POST",
        f"/v1/access-tokens/{seg(jti)}/revoke",
        parse=m.RevokeResult.model_validate,
        retry="write",
    )


# -- management reports (mgmt) ------------------------------------------


def usage_report(group_by: str | None, from_: str | None, to: str | None) -> Spec[m.UsageReport]:
    return Spec(
        "GET",
        "/v1/reports/usage",
        parse=m.UsageReport.model_validate,
        params=_params(group_by=group_by, from_=from_, to=to),
    )


def audit_report(
    uid: str | None,
    session_id: str | None,
    runbook: str | None,
    from_: str | None,
    to: str | None,
    limit: int | None,
    bodies: bool,
    before: str | None,
) -> Spec[m.AuditPage]:
    return Spec(
        "GET",
        "/v1/reports/audit",
        parse=m.AuditPage.model_validate,
        params=_params(
            uid=uid,
            session_id=session_id,
            runbook=runbook,
            from_=from_,
            to=to,
            limit=limit,
            bodies=bodies or None,
            before=before,
        ),
    )


def cost_report(from_: str | None, to: str | None) -> Spec[m.CostReport]:
    return Spec(
        "GET",
        "/v1/reports/cost",
        parse=m.CostReport.model_validate,
        params=_params(from_=from_, to=to),
    )


def timeseries_report(window: str | None, plane: str | None) -> Spec[m.TimeseriesReport]:
    return Spec(
        "GET",
        "/v1/reports/timeseries",
        parse=m.TimeseriesReport.model_validate,
        params=_params(window=window, plane=plane),
    )


def endpoints_report(window: str | None, limit: int | None) -> Spec[m.EndpointsReport]:
    return Spec(
        "GET",
        "/v1/reports/endpoints",
        parse=m.EndpointsReport.model_validate,
        params=_params(window=window, limit=limit),
    )


def runbooks_report(window: str | None) -> Spec[m.RunbookReport]:
    return Spec(
        "GET",
        "/v1/reports/runbooks",
        parse=m.RunbookReport.model_validate,
        params=_params(window=window),
    )


def sessions_report(window: str | None) -> Spec[m.SessionsReport]:
    return Spec(
        "GET",
        "/v1/reports/sessions",
        parse=m.SessionsReport.model_validate,
        params=_params(window=window),
    )


def evidence_report(window: str | None) -> Spec[m.EvidenceReport]:
    return Spec(
        "GET",
        "/v1/reports/evidence",
        parse=m.EvidenceReport.model_validate,
        params=_params(window=window),
    )


def matrix_report() -> Spec[m.MatrixReport]:
    return Spec("GET", "/v1/reports/matrix", parse=m.MatrixReport.model_validate)


# -- guided authoring --------------------------------------------------------


def list_patterns() -> Spec[list[m.PatternSummary]]:
    return Spec(
        "GET",
        "/v1/authoring/patterns",
        parse=lambda v: [m.PatternSummary.model_validate(p) for p in v["patterns"]],
    )


def get_pattern(id: str) -> Spec[m.PatternDetail]:
    return Spec("GET", f"/v1/authoring/patterns/{seg(id)}", parse=m.PatternDetail.model_validate)


def create_draft(name: str, pattern_id: str | None, seed_from_exemplar: bool) -> Spec[m.Draft]:
    return Spec(
        "POST",
        "/v1/authoring/drafts",
        parse=m.Draft.model_validate,
        json=_drop_none(
            {
                "name": name,
                "pattern_id": pattern_id,
                "seed_from_exemplar": seed_from_exemplar,
            }
        ),
        retry="write",
    )


def list_drafts() -> Spec[list[m.DraftSummary]]:
    return Spec(
        "GET",
        "/v1/authoring/drafts",
        parse=lambda v: [m.DraftSummary.model_validate(d) for d in v["drafts"]],
    )


def get_draft(draft_id: str) -> Spec[m.Draft]:
    return Spec("GET", f"/v1/authoring/drafts/{seg(draft_id)}", parse=m.Draft.model_validate)


def delete_draft(draft_id: str) -> Spec[m.DraftDeleteResult]:
    # The client surface's ONE delete — a workspace draft (soft), never
    # ledger data, so the append-only invariant is untouched.
    return Spec(
        "DELETE",
        f"/v1/authoring/drafts/{seg(draft_id)}",
        parse=m.DraftDeleteResult.model_validate,
        retry="write",
    )


def put_answers(draft_id: str, answers: Any, materialize: bool) -> Spec[m.Draft]:
    return Spec(
        "PUT",
        f"/v1/authoring/drafts/{seg(draft_id)}/answers",
        parse=m.Draft.model_validate,
        json={"answers": answers, "materialize": materialize},
        retry="write",
    )


def validate_draft(draft_id: str) -> Spec[m.DraftValidation]:
    return Spec(
        "POST",
        f"/v1/authoring/drafts/{seg(draft_id)}/validate",
        parse=m.DraftValidation.model_validate,
        retry="write",
    )


def assist_draft(
    draft_id: str,
    description: str | None,
    instructions: str | None,
    provider: str | None,
    model: str | None,
    tier: str | None,
) -> Spec[m.AssistResult]:
    # A BYOK provider call rides behind this — send once.
    return Spec(
        "POST",
        f"/v1/authoring/drafts/{seg(draft_id)}/assist",
        parse=m.AssistResult.model_validate,
        json=_drop_none(
            {
                "description": description,
                "instructions": instructions,
                "provider": provider,
                "model": model,
                "tier": tier,
            }
        ),
        retry="write",
    )


def export_draft(draft_id: str) -> Spec[m.ExportBundle]:
    return Spec(
        "POST",
        f"/v1/authoring/drafts/{seg(draft_id)}/export",
        parse=m.ExportBundle.model_validate,
        retry="write",
    )


def apply_draft(draft_id: str) -> Spec[m.ApplyDraftResult]:
    return Spec(
        "POST",
        f"/v1/authoring/drafts/{seg(draft_id)}/apply",
        parse=m.ApplyDraftResult.model_validate,
        retry="write",
    )


# -- meta --------------------------------------------------------------------


def server_version() -> Spec[m.ServerVersion]:
    # GET /version — the server's name + workspace version, unauthenticated.
    return Spec("GET", "/version", parse=m.ServerVersion.model_validate)
