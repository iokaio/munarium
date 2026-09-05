# SPDX-License-Identifier: Apache-2.0
"""Shared gRPC machinery: channel/target handling, metadata, structured
error decoding (google.rpc.ErrorInfo), and pb <-> model conversions used by
both the sync and asyncio transports.

Transport notes (documented parity gaps, not bugs):
- ``build_index`` and ``health_ai`` have no gRPC RPCs — typed
  ``UnsupportedError``.
- The REST-only platform surface is typed ``UnsupportedError`` too:
  ``turn_stream`` (SSE), the four bulk-upload routes, ``get_source``,
  ``findings``, chronology-rules, ``providers.list``, the max-tokens
  budgets (``providers.max_tokens`` / ``replace_max_tokens``), every
  reports method (AdminService.Usage is declared but UNIMPLEMENTED — not
  wired), the whole authoring plane, and ``server_version``.
- proto3 scalars cannot carry "explicitly zero": ``as_of_seq``/``limit``/
  ``top_k``/``fact_limit``/``budget_tokens``/``max_tokens``/``ttl_secs``
  of 0, a counter ``budget`` of 0, and a ``confidence``/``temperature`` of
  0.0 are rejected as ``InvalidInputError`` instead of silently meaning
  "absent" (REST carries them faithfully).
- proto3 ``repeated`` fields cannot carry "explicitly empty" either: an
  explicit ``IngestFile.collections=[]`` (REST: bind to nothing; gRPC: the
  matcher auto-bind) and ``mint(runbook_refs=[])`` (REST: no runbooks;
  gRPC: any runbook) are rejected as ``InvalidInputError``; ``None`` is
  fine on both.
- single-file ``ingest()`` rides IngestFiles (a batch of one): the REST
  route's typed 400 for a bad file becomes ``InvalidInputError`` (local
  decode) / ``UnexpectedError`` carrying the server's per-item error text
  — the wire has no problem slug for per-item errors.
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import json
from collections import deque
from collections.abc import Iterable
from typing import Any

import grpc
from google.rpc import error_details_pb2, status_pb2

from . import models as m
from ._errors import InvalidInputError, MunariumError, UnexpectedError, error_from_grpc
from ._proto.mmp.v1 import command_pb2, common_pb2, ingest_pb2, ledger_pb2

_GRPC_DETAILS_KEY = "grpc-status-details-bin"


def target_from_endpoint(endpoint: str) -> tuple[str, bool]:
    """Return (target, use_tls). Plaintext exactly when the scheme is
    http:// (or no scheme is given)."""
    if endpoint.startswith("https://"):
        return endpoint[len("https://") :].rstrip("/"), True
    if endpoint.startswith("http://"):
        return endpoint[len("http://") :].rstrip("/"), False
    return endpoint.rstrip("/"), False


def metadata(
    token: str | None,
    idem_key: str | None = None,
    uid: str | None = None,
) -> list[tuple[str, str]]:
    md: list[tuple[str, str]] = []
    if token:
        md.append(("authorization", f"Bearer {token}"))
    if uid:
        md.append(("munarium-uid", uid))
    if idem_key:
        md.append(("idempotency-key", idem_key))
    return md


def decode_error(e: grpc.RpcError) -> MunariumError:
    """Decode a grpc.RpcError (sync or aio) into a typed MunariumError via the
    ErrorInfo detail; code-based fallback when details are absent."""
    code = e.code().name if e.code() is not None else "UNKNOWN"
    detail = e.details() or ""
    error_info = None
    trailing = e.trailing_metadata() or ()
    for key, value in trailing:
        if key == _GRPC_DETAILS_KEY:
            try:
                status = status_pb2.Status()
                raw = value if isinstance(value, bytes) else value.encode("utf-8")
                status.MergeFromString(raw)
                for any_detail in status.details:
                    info = error_details_pb2.ErrorInfo()
                    if any_detail.Unpack(info):
                        error_info = info
                        break
            except Exception:  # noqa: BLE001 - fall back to code mapping
                error_info = None
            break
    return error_from_grpc(code, detail, error_info)


def reject_zero(name: str, value: int | None) -> None:
    if value == 0:
        raise InvalidInputError(
            f"{name} = 0 cannot be represented on the gRPC wire (proto3 uses 0 for "
            "'absent'); omit it, or use the REST transport"
        )


def reject_empty_list(name: str, value: list[Any] | None) -> None:
    """A proto3 ``repeated`` field cannot carry "explicitly empty": the
    server reads an empty list as ABSENT (``IngestFile.collections`` = []
    means "bind to nothing" on REST but "matcher auto-bind" on gRPC;
    ``runbook_refs`` = [] means "no runbooks" on REST but "any runbook" on
    gRPC). Refuse the explicit ``[]`` so the two transports never silently
    diverge — exactly like :func:`reject_zero`. ``None`` stays fine."""
    if value is not None and len(value) == 0:
        raise InvalidInputError(
            f"{name} = [] (an explicit empty list) cannot be represented on the gRPC "
            "wire (proto3 reads an empty repeated field as 'absent'); omit it, or use "
            "the REST transport"
        )


def yaml_hash(yaml: str) -> str:
    """The server defines yaml_hash as sha256(yaml bytes)."""
    return hashlib.sha256(yaml.encode("utf-8")).hexdigest()


# -- enum maps --------------------------------------------------------------

_CLAIM_TYPE_TO_PB: dict[str, Any] = {
    "fact": ledger_pb2.CLAIM_TYPE_FACT,
    "update": ledger_pb2.CLAIM_TYPE_UPDATE,
    "correction": ledger_pb2.CLAIM_TYPE_CORRECTION,
}
_CLAIM_TYPE_FROM_PB: dict[int, str] = {int(v): k for k, v in _CLAIM_TYPE_TO_PB.items()}

_PROVENANCE_TO_PB: dict[str, Any] = {
    "witnessed": ledger_pb2.PROVENANCE_WITNESSED,
    "backfilled": ledger_pb2.PROVENANCE_BACKFILLED,
    "repaired": ledger_pb2.PROVENANCE_REPAIRED,
    "emergent": ledger_pb2.PROVENANCE_EMERGENT,
    "coverage_repair": ledger_pb2.PROVENANCE_COVERAGE_REPAIR,
}
_PROVENANCE_FROM_PB: dict[int, str] = {int(v): k for k, v in _PROVENANCE_TO_PB.items()}

_SEVERITY_FROM_PB: dict[int, str] = {
    common_pb2.SEVERITY_BLOCK: "block",
    common_pb2.SEVERITY_WARN: "warn",
    common_pb2.SEVERITY_INFO: "info",
}

_STATUS_TO_PB: dict[str, Any] = {
    "accepted": common_pb2.CLAIM_STATUS_ACCEPTED,
    "disputed": common_pb2.CLAIM_STATUS_DISPUTED,
}


def _json_opt(s: str) -> Any | None:
    if not s:
        return None
    try:
        return json.loads(s)
    except ValueError:
        return None


def _opt(s: str) -> str | None:
    return s or None


# -- pb -> model ------------------------------------------------------------


def parse_claim(c: Any) -> m.Claim:
    return m.Claim(
        id=c.id,
        version_id=c.version_id,
        seq=c.seq,
        claim_type=_CLAIM_TYPE_FROM_PB.get(c.claim_type, "fact"),  # type: ignore[arg-type]
        subject=c.subject,
        key=c.key,
        value=c.value,
        normalized_text=c.normalized_text,
        scope_path=_opt(c.scope_path),
        status="disputed" if c.status == common_pb2.CLAIM_STATUS_DISPUTED else "accepted",
        provenance=_PROVENANCE_FROM_PB.get(c.provenance, "witnessed"),  # type: ignore[arg-type]
        supersedes_id=_opt(c.supersedes_id),
        entity_id=_opt(c.entity_id),
        evidence=_json_opt(c.evidence_json),
        # proto3 sentinel: 0.0 = absent (explicit 0.0 is rejected on send).
        confidence=c.confidence if c.confidence != 0.0 else None,
        shape_ref=_opt(c.shape_ref),
        # A message field has presence: HasField is the only honest "absent".
        origin=parse_origin(c.origin) if c.HasField("origin") else None,
    )


def parse_origin(o: Any) -> m.ClaimOrigin:
    return m.ClaimOrigin(
        kind=o.kind,
        source_id=o.source_id,
        mapping_version=o.mapping_version,
        row_key=o.row_key,
        event_position=_opt(o.event_position),
        observed_at=_opt(o.observed_at),
        evidence_id=_opt(o.evidence_id),
    )


def origin_to_pb(o: m.ClaimOrigin) -> Any:
    return ledger_pb2.ClaimOrigin(
        kind=o.kind,
        source_id=o.source_id,
        mapping_version=o.mapping_version,
        row_key=o.row_key,
        event_position=o.event_position or "",
        observed_at=o.observed_at or "",
        evidence_id=o.evidence_id or "",
    )


def parse_finding(f: Any) -> m.GateFinding:
    return m.GateFinding(
        rule_id=f.rule_id,
        severity=_SEVERITY_FROM_PB.get(f.severity, "info"),  # type: ignore[arg-type]
        message=f.message,
        scope_path=_opt(f.scope_path),
        detail=_json_opt(f.detail_json),
    )


def parse_anchor(a: Any) -> m.Anchor:
    return m.Anchor(
        id=a.id,
        version_id=a.version_id,
        detail_key=a.detail_key,
        locked_value=a.locked_value,
        locked_at_scope=_opt(a.locked_at_scope),
        status=a.status,
        seq=a.seq,
    )


def parse_promise(p: Any) -> m.Promise:
    return m.Promise(
        id=p.id,
        version_id=p.version_id,
        key=p.key,
        kind=p.kind,
        description=p.description,
        origin_scope=_opt(p.origin_scope),
        due_scope=_opt(p.due_scope),
        status=p.status,
        seq=p.seq,
        fulfilled_seq=p.fulfilled_seq if p.fulfilled_seq > 0 else None,
    )


def _hit_fields(h: Any) -> dict[str, Any]:
    """The field mapping every hit shape shares (search hits and the
    per-collection turn hits carry the same identity/provenance core)."""
    return {
        "chunk_id": h.chunk_id,
        "source_id": h.source_id,
        "source_path": h.source_path,
        "source_content_hash": h.source_content_hash,
        "text": h.text,
        "score": h.score,
    }


def parse_search_hit(h: Any) -> m.SearchHit:
    return m.SearchHit(
        **_hit_fields(h),
        # ranks are 1-based; the wire uses 0 for absent
        lexical_rank=int(h.lexical_rank) if h.lexical_rank > 0 else None,
        vector_rank=int(h.vector_rank) if h.vector_rank > 0 else None,
        metadata=_json_opt(h.metadata_json),
    )


def parse_envelope(e: Any) -> m.ProvenanceEnvelope:
    return m.ProvenanceEnvelope(
        chunk_ids=list(e.chunk_ids),
        source_ids=list(e.source_ids),
        source_paths=list(e.source_paths),
        source_content_hashes=list(e.source_content_hashes),
        index_version=e.index_version,
        event_watermark=e.event_watermark,
        provider_fingerprint=_opt(e.provider_fingerprint),
    )


def parse_run_status(r: Any) -> m.RunStatus:
    return m.RunStatus(
        run_id=r.run_id,
        runbook_ref=r.runbook_ref,
        state=r.state,
        version_id=_opt(r.version_id),
        steps=[
            m.RunbookStep(
                ordinal=s.ordinal,
                name=s.name,
                state=s.state,
                detail=_json_opt(s.detail_json),
            )
            for s in r.steps
        ],
    )


# -- model -> pb ------------------------------------------------------------


def build_propose(
    version_id: str, claim: m.ClaimInput, expected_head: int | None
) -> command_pb2.ProposeClaimRequest:
    if claim.confidence == 0.0:
        raise InvalidInputError(
            "confidence = 0.0 cannot be represented on the gRPC wire (proto3 uses "
            "0.0 for 'absent'); omit it, or use the REST transport"
        )
    msg = command_pb2.ProposeClaimRequest(
        version_id=version_id,
        claim_type=_CLAIM_TYPE_TO_PB[claim.claim_type],
        subject=claim.subject,
        key=claim.key,
        value=claim.value,
        scope_path=claim.scope_path or "",
        provenance=_PROVENANCE_TO_PB.get(claim.provenance or "", 0),
        supersedes_id=claim.supersedes_id or "",
        entity_id=claim.entity_id or "",
        evidence_json=json.dumps(claim.evidence) if claim.evidence is not None else "",
        confidence=claim.confidence or 0.0,
        shape_ref=claim.shape_ref or "",
    )
    if claim.origin is not None:
        msg.origin.CopyFrom(origin_to_pb(claim.origin))
    if expected_head is not None:
        msg.expected_head = expected_head
    return msg


def status_filter_pb(statuses: tuple[m.ClaimStatus, ...]) -> list[Any]:
    return [_STATUS_TO_PB[s] for s in statuses]


def source_chunks(
    data: bytes | Iterable[bytes],
    declared_sha256: str,
    media_type: str | None,
    filename: str | None,
    shape_ref: str | None,
    chunk_size: int = 64 * 1024,
) -> Any:
    """Header-then-chunks message iterator for the client-streaming upload.
    Iterables stream through in O(chunk) memory; oversized chunks re-slice."""
    yield ingest_pb2.PutSourceRequest(
        header=ingest_pb2.SourceHeader(
            declared_sha256=declared_sha256,
            media_type=media_type or "",
            filename=filename or "",
            shape_ref=shape_ref or "",
        )
    )
    pieces: Iterable[bytes] = [data] if isinstance(data, bytes) else data
    for piece in pieces:
        for i in range(0, len(piece), chunk_size):
            yield ingest_pb2.PutSourceRequest(chunk=piece[i : i + chunk_size])


# -- collections --------------------------------------------------------


def parse_collection(c: Any) -> m.Collection:
    return m.Collection(
        id=c.id,
        name=c.name,
        shape_ref=c.shape_ref,
        access_level=c.access_level,
        compartments=list(c.compartments),
        status=c.status,
        description=_opt(c.description),
        created_at=c.created_at,
        source_count=c.source_count,
        active_index=_opt(c.active_index),
    )


# -- runbook management v2 ---------------------------------------------


def parse_runbook_collection(c: Any) -> m.RunbookCollection:
    return m.RunbookCollection(
        name=c.name,
        collection_id=_opt(c.collection_id),
        shape_ref=c.shape_ref,
        access_level=c.access_level,
        compartments=list(c.compartments),
        active_index=_opt(c.active_index),
        source_count=c.source_count,
    )


def parse_runbook_summary(r: Any) -> m.RunbookSummary:
    return m.RunbookSummary(
        runbook_ref=r.runbook_ref,
        name=r.name,
        version=r.version,
        status=r.status,
        min_access_level=r.min_access_level,
        collections=[parse_runbook_collection(c) for c in r.collections],
        created_at=r.created_at,
    )


def parse_runbook_info(r: Any) -> m.RunbookInfo:
    return m.RunbookInfo(
        runbook_ref=r.runbook_ref,
        name=r.name,
        version=r.version,
        status=r.status,
        collections=[parse_runbook_collection(c) for c in r.collections],
        versions=list(r.versions),
        models=_json_opt(r.models_json),
        retrieval=_json_opt(r.retrieval_json),
        has_completion=r.has_completion,
        created_at=r.created_at,
    )


def parse_validate_result(r: Any) -> m.ValidateResult:
    return m.ValidateResult(
        valid=r.valid,
        findings=[
            m.ValidationFinding(severity=f.severity, code=f.code, message=f.message, path=f.path)
            for f in r.findings
        ],
        suggestions=[
            m.Suggestion(title=sg.title, rationale=sg.rationale, patch_hint=_opt(sg.patch_hint))
            for sg in r.suggestions
        ],
        suggest_note=_opt(r.suggest_note),
    )


# -- sessions + turns --------------------------------------------------


def parse_hierarchy(h: Any) -> m.EvidenceHierarchyDecision:
    """The decision off the wire. proto3 has no null, so the optional
    strings come back empty when the server left them out — `_opt` restores
    the None the REST twin parses, keeping the two transports' TurnResult
    identical rather than merely similar."""
    return m.EvidenceHierarchyDecision(
        profile=h.profile,
        intent_kind=_opt(h.intent_kind),
        intent_explicit=h.intent_explicit,
        layers=[
            m.LayerOutcome(
                layer=layer.layer,
                role=layer.role,
                requirement=layer.requirement,
                block=layer.block,
                evidence_id=_opt(layer.evidence_id),
                supports_completeness=layer.supports_completeness,
                refusal_code=_opt(layer.refusal_code),
                elapsed_ms=layer.elapsed_ms,
            )
            for layer in h.layers
        ],
        completeness_available=h.completeness_available,
        disclosed_conflicts=h.disclosed_conflicts,
        conflicts_policy=h.conflicts_policy,
    )


def parse_turn_response(resp: Any) -> m.TurnResult:
    completion = None
    if resp.HasField("completion"):
        c = resp.completion
        verification = None
        if c.HasField("verification"):
            v = c.verification
            verification = m.TurnVerification(
                checks=list(v.checks),
                retries=v.retries,
                first_pass_violations=list(v.first_pass_violations),
                violations=list(v.violations),
            )
        completion = m.TurnCompletion(
            provider=c.provider,
            model=c.model,
            was_override=c.was_override,
            text=c.text,
            input_tokens=c.input_tokens,
            output_tokens=c.output_tokens,
            verification=verification,
        )
    return m.TurnResult(
        session_id=resp.session_id,
        ordinal=resp.ordinal,
        collections_searched=list(resp.collections_searched),
        skipped=list(resp.skipped),
        hits=[m.TurnHit(collection=h.collection, **_hit_fields(h)) for h in resp.hits],
        envelopes=[
            m.CollectionEnvelope(collection=e.collection, envelope=parse_envelope(e.envelope))
            for e in resp.envelopes
        ],
        completion=completion,
        # A message field, so HasField distinguishes "no profile ran" from
        # a decision whose every field happens to be a proto3 default.
        hierarchy=parse_hierarchy(resp.hierarchy) if resp.HasField("hierarchy") else None,
    )


def parse_session(resp: Any) -> m.Session:
    return m.Session(
        session_id=resp.session_id,
        uid=resp.uid,
        runbook_ref=resp.runbook_ref,
        access_level=resp.access_level,
        compartments=list(resp.compartments),
        state=resp.state,
        created_at=resp.created_at,
        turns=[
            m.SessionTurn(
                ordinal=t.ordinal,
                query=t.query,
                collections_searched=list(t.collections_searched),
                # Stored transcript rows ride as JSON strings on the wire —
                # parse-or-None keeps a mangled row visible instead of
                # failing the whole session read.
                hits=_json_opt(t.hits_json),
                envelope=_json_opt(t.envelope_json),
                completion=_json_opt(t.completion_json),
                created_at=t.created_at,
            )
            for t in resp.turns
        ],
    )


# -- access tokens ------------------------------------------------------


def parse_token_info(t: Any) -> m.TokenInfo:
    return m.TokenInfo(
        jti=t.jti,
        uid=t.uid,
        access_level=t.access_level,
        compartments=list(t.compartments),
        scopes=list(t.scopes),
        runbook_refs=list(t.runbook_refs) if t.runbook_refs else None,
        issued_by=t.issued_by,
        issued_at=t.issued_at,
        expires_at=t.expires_at,
        revoked_at=_opt(t.revoked_at),
    )


# -- file ingest: the per-item base64 contract -------------------------
# The REST plane carries content as base64 INSIDE the JSON body; the gRPC
# message carries raw bytes — so the client decodes here. The per-item
# contract holds ACROSS transports: a file whose base64 cannot decode
# becomes its own error result (never sent), the valid remainder ships, and
# results splice back in input order — exactly the outcome the REST plane's
# server-side per-item handling produces.


_ASCII_WS = {ord(c): None for c in " \t\r\n\f\v"}


def prepare_ingest_files(files: list[m.IngestFile]) -> list[Any | m.IngestResult]:
    """One slot per input file, in order: the wire message for a decodable
    file, or the local error IngestResult for one whose base64 is bad."""
    slots: list[Any | m.IngestResult] = []
    for f in files:
        try:
            # The server's REST path trims ASCII whitespace before decoding
            # (a trailing newline from a shell pipeline is common), so the
            # gRPC path must accept the same input.
            content = base64.b64decode(f.content_base64.translate(_ASCII_WS), validate=True)
        except (binascii.Error, ValueError) as e:
            slots.append(
                m.IngestResult(
                    filename=f.filename,
                    existed=False,
                    bound_to=[],
                    error=f"content_base64 is not valid base64: {e}",
                )
            )
            continue
        reject_empty_list(f"{f.filename!r}.collections", f.collections)
        slots.append(
            ingest_pb2.IngestFile(
                filename=f.filename,
                media_type=f.media_type,
                content=content,
                sha256=f.sha256 or "",
                collections=f.collections or [],
            )
        )
    return slots


def parse_ingest_result(r: Any) -> m.IngestResult:
    return m.IngestResult(
        filename=r.filename,
        source_id=_opt(r.source_id),
        sha256=_opt(r.sha256),
        existed=r.existed,
        bound_to=list(r.bound_to),
        error=_opt(r.error),
    )


def splice_ingest_results(
    slots: list[Any | m.IngestResult], server_results: list[m.IngestResult]
) -> list[m.IngestResult]:
    """Merge the server's per-item results back into input order: local
    error slots stand as-is, each sent file consumes the next server
    result. A short server list raises — a missing outcome must never read
    as silent success — and so does a LONG one: surplus results mean the
    pairing is unknowable (a file could be credited with another's
    outcome), never something to drop quietly."""
    queue = deque(server_results)
    out: list[m.IngestResult] = []
    for slot in slots:
        if isinstance(slot, m.IngestResult):
            out.append(slot)
            continue
        if not queue:
            raise UnexpectedError(f"IngestFilesResponse carried no result for {slot.filename!r}")
        out.append(queue.popleft())
    if queue:
        sent = sum(1 for s in slots if not isinstance(s, m.IngestResult))
        raise UnexpectedError(
            f"IngestFilesResponse carried {sent + len(queue)} results for {sent} files sent "
            f"(surplus: {[r.filename for r in queue]!r}) — per-item pairing is unknowable"
        )
    return out
