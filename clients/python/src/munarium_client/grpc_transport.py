# SPDX-License-Identifier: Apache-2.0
"""gRPC transport (grpcio): direct :50051 plane or :443 via the gateway.
Errors decode the google.rpc.ErrorInfo structured detail. Commands carry
auto-generated idempotency keys and are re-sent with the SAME key for
exactly ONE failure: the typed ``overloaded`` (the server provably shed the
request before executing it). On gRPC commands are NEVER re-sent on a
transport failure — no gRPC failure is provably undelivered (a failed
reconnect and a broken established stream both surface as UNAVAILABLE; a
deadline expiry may have reached the server), and the server records an
idempotency key only AFTER a command completes, so a retry that overtakes
an in-flight attempt could execute it twice. (REST additionally retries the
connect-phase failure it CAN prove undelivered; see ``rest.py``.) Reads
retry any transient; non-replayable writes send once. See ``_grpc_common``
for the documented parity gaps (the REST-only Unsupported set, proto3
zero/empty-list sentinels)."""

from __future__ import annotations

import json
import uuid
from collections.abc import AsyncIterable, Iterable, Iterator
from typing import Any, Literal

import grpc

from . import _grpc_common as gc
from . import _retry
from . import models as m
from ._chunks import ChunkSource, resolve_chunks
from ._errors import (
    InvalidInputError,
    OverloadedError,
    TransportError,
    UnexpectedError,
    UnsupportedError,
    check_bulk_files,
    check_promise_status,
)
from ._options import ClientOptions
from ._proto.mmp.v1 import (
    admin_pb2,
    admin_pb2_grpc,
    command_pb2,
    command_pb2_grpc,
    ingest_pb2,
    ingest_pb2_grpc,
    ledger_pb2,
    provider_pb2,
    provider_pb2_grpc,
    query_pb2,
    query_pb2_grpc,
    retrieval_pb2,
    retrieval_pb2_grpc,
    runbook_pb2,
    runbook_pb2_grpc,
    session_pb2,
    session_pb2_grpc,
)

_BUILD_INDEX_UNSUPPORTED = (
    "index builds have no gRPC RPC today — use the REST client (POST /v1/indexes/{shape_ref}/build)"
)


def _bulk_unsupported() -> UnsupportedError:
    return UnsupportedError(
        "bulk upload sessions have no gRPC RPCs today — use the REST client "
        "(POST /v1/ingest/bulk ...), or stream single sources via put_source"
    )


def _reports_unsupported() -> UnsupportedError:
    return UnsupportedError(
        "reports have no gRPC RPCs today (AdminService.Usage is declared but "
        "UNIMPLEMENTED) — use the REST client (GET /v1/reports/...)"
    )


def _authoring_unsupported() -> UnsupportedError:
    return UnsupportedError(
        "guided authoring has no gRPC RPCs — use the REST client (/v1/authoring/...)"
    )


def _evidence_unsupported(route: str) -> UnsupportedError:
    return UnsupportedError(
        f"the sealed evidence plane is REST-only in v1 — use the REST client ({route})"
    )


def _chronology_unsupported(route: str) -> UnsupportedError:
    return UnsupportedError(
        f"chronology rules have no gRPC RPC today — use the REST client ({route})"
    )


def _max_tokens_unsupported(route: str) -> UnsupportedError:
    return UnsupportedError(
        f"max-tokens budgets have no gRPC RPC today — use the REST client ({route})"
    )


def _resolve_chunks(data: ChunkSource) -> bytes | Iterable[bytes]:
    """Resolve a chunk SOURCE for one upload attempt. Bytes pass through; a
    zero-arg callable is invoked to build a fresh iterable (that is what makes
    a retry possible). Async iterators cannot drive the sync stubs — fail
    typed instead of leaking a TypeError from the worker thread."""
    resolved = resolve_chunks(data)
    if isinstance(resolved, AsyncIterable):
        raise InvalidInputError(
            "the gRPC transport put_source takes bytes or a callable returning "
            "a sync iterable of chunks (async iterators are REST-only today)"
        )
    if not isinstance(resolved, bytes) and not isinstance(resolved, Iterable):
        raise InvalidInputError(
            "put_source takes bytes or a zero-arg callable returning an iterable of chunks"
        )
    return resolved


class _Core:
    """Channel + stubs + the one retry loop both plane sets share."""

    def __init__(self, options: ClientOptions) -> None:
        target, use_tls = gc.target_from_endpoint(options.endpoint)
        if use_tls:
            self.channel = grpc.secure_channel(target, grpc.ssl_channel_credentials())
        else:
            self.channel = grpc.insecure_channel(target)
        self.token = options.token
        self.uid = options.uid
        self.timeout = options.request_timeout
        self.retries = options.read_retries
        self.commands = command_pb2_grpc.CommandServiceStub(self.channel)  # type: ignore[no-untyped-call]
        self.queries = query_pb2_grpc.QueryServiceStub(self.channel)  # type: ignore[no-untyped-call]
        self.ingest = ingest_pb2_grpc.IngestServiceStub(self.channel)  # type: ignore[no-untyped-call]
        self.retrieval = retrieval_pb2_grpc.RetrievalServiceStub(self.channel)  # type: ignore[no-untyped-call]
        self.runbooks = runbook_pb2_grpc.RunbookServiceStub(self.channel)  # type: ignore[no-untyped-call]
        self.providers = provider_pb2_grpc.ProviderServiceStub(self.channel)  # type: ignore[no-untyped-call]
        self.sessions = session_pb2_grpc.SessionServiceStub(self.channel)  # type: ignore[no-untyped-call]
        self.admin = admin_pb2_grpc.AdminServiceStub(self.channel)  # type: ignore[no-untyped-call]

    def call(
        self,
        fn: Any,
        msg: Any,
        *,
        retry_class: Literal["read", "command", "write"] = "write",
        idem: str | None = None,
        exempt_deadline: bool = False,
    ) -> Any:
        """One retry policy for every RPC, derived from the request class
        (mirroring the REST Spec.retry table and Rust's
        ``is_command_retry_safe``).

        Reads retry any transient failure. Commands re-send the SAME key
        ONLY on the typed ``overloaded`` — the server shed the request
        before executing it. NEVER on a transport failure: the server
        records an idempotency key only AFTER the command completes, so a
        retry that overtakes an in-flight attempt executes it twice, and
        UNAVAILABLE on an established HTTP/2 stream cannot be distinguished
        from a call the server is still running. Non-replayable writes send
        once. The default is the SAFE direction: no retry, no key.

        ``exempt_deadline`` drops the per-request deadline (the unary turn:
        a client-side abort does not stop the server's paid completion, so
        a 30 s cap is a double-spend invitation; IngestFiles: bodies run to
        256 MiB, like the REST file/bulk sends)."""
        if retry_class == "command" and idem is None:
            idem = str(uuid.uuid4())
        attempt = 0
        while True:
            attempt += 1
            try:
                return fn(
                    msg,
                    metadata=gc.metadata(self.token, idem, self.uid),
                    timeout=None if exempt_deadline else self.timeout,
                )
            except grpc.RpcError as e:
                err = gc.decode_error(e)
                if retry_class == "read":
                    again = err.transient
                elif retry_class == "command":
                    again = isinstance(err, OverloadedError)
                else:
                    again = False
                if again and attempt <= self.retries:
                    _retry.sleep_sync(attempt)
                    continue
                raise err from None
            except Exception as e:  # noqa: BLE001 - channel-level failures
                raise TransportError(str(e)) from None


def _parse_claim_outcome(resp: Any) -> m.ClaimOutcome:
    if not resp.HasField("claim"):
        raise UnexpectedError("ProposeClaimResponse without claim")
    return m.ClaimOutcome(
        claim=gc.parse_claim(resp.claim),
        findings=[gc.parse_finding(f) for f in resp.findings],
        head_seq=resp.head_seq,
    )


def _parse_events_outcome(resp: Any) -> m.EventsOutcome:
    return m.EventsOutcome(
        claims=[gc.parse_claim(c) for c in resp.claims],
        findings=[gc.parse_finding(f) for f in resp.findings],
        head_seq=resp.head_seq,
    )


def _parse_search(resp: Any) -> m.SearchResult:
    if not resp.HasField("envelope"):
        raise UnexpectedError("HybridSearchResponse without ProvenanceEnvelope")
    return m.SearchResult(
        hits=[gc.parse_search_hit(h) for h in resp.hits],
        envelope=gc.parse_envelope(resp.envelope),
    )


def _events_msg(
    version_id: str,
    claims: list[m.ClaimInput],
    candidate_text: str | None,
    expected_head: int | None,
) -> command_pb2.AppendEventsRequest:
    msg = command_pb2.AppendEventsRequest(
        version_id=version_id,
        claims=[gc.build_propose(version_id, c, None) for c in claims],
        candidate_text=candidate_text or "",
    )
    if expected_head is not None:
        msg.expected_head = expected_head
    return msg


# ---------------------------------------------------------------------------
# sync planes
# ---------------------------------------------------------------------------


class SyncGrpcCommands:
    def __init__(self, core: _Core) -> None:
        self._c = core

    def create_version(
        self,
        parent_version_id: str | None = None,
        metadata: Any | None = None,
        idempotency_key: str | None = None,
    ) -> str:
        msg = command_pb2.CreateVersionRequest(
            parent_version_id=parent_version_id or "",
            metadata_json=json.dumps(metadata) if metadata is not None else "",
        )
        resp = self._c.call(
            self._c.commands.CreateVersion, msg, retry_class="command", idem=idempotency_key
        )
        return str(resp.version_id)

    def propose_claim(
        self,
        version_id: str,
        *,
        expected_head: int | None = None,
        idempotency_key: str | None = None,
        **claim: Any,
    ) -> m.ClaimOutcome:
        msg = gc.build_propose(version_id, m.ClaimInput(**claim), expected_head)
        resp = self._c.call(
            self._c.commands.ProposeClaim, msg, retry_class="command", idem=idempotency_key
        )
        return _parse_claim_outcome(resp)

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
        msg = _events_msg(version_id, inputs, candidate_text, expected_head)
        resp = self._c.call(
            self._c.commands.AppendEvents, msg, retry_class="command", idem=idempotency_key
        )
        return _parse_events_outcome(resp)

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
        msg = command_pb2.OpenPromiseRequest(
            version_id=version_id,
            key=key,
            kind=kind,
            description=description,
            origin_scope=origin_scope or "",
            due_scope=due_scope or "",
        )
        resp = self._c.call(
            self._c.commands.OpenPromise, msg, retry_class="command", idem=idempotency_key
        )
        if not resp.HasField("promise"):
            raise UnexpectedError("OpenPromiseResponse without promise")
        return gc.parse_promise(resp.promise)

    def fulfill_promise(
        self, version_id: str, key: str, idempotency_key: str | None = None
    ) -> bool:
        msg = command_pb2.FulfillPromiseRequest(version_id=version_id, key=key)
        resp = self._c.call(
            self._c.commands.FulfillPromise, msg, retry_class="command", idem=idempotency_key
        )
        return bool(resp.fulfilled)

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
        msg = command_pb2.LockAnchorRequest(
            version_id=version_id,
            subject=subject,
            key=key,
            value=value,
            scope_path=scope_path or "",
            evidence_json=json.dumps(evidence) if evidence is not None else "",
        )
        resp = self._c.call(
            self._c.commands.LockAnchor, msg, retry_class="command", idem=idempotency_key
        )
        if not resp.HasField("anchor"):
            raise UnexpectedError("LockAnchorResponse without anchor")
        return gc.parse_anchor(resp.anchor)

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
        gc.reject_zero("budget", budget)
        msg = command_pb2.RecordCountsRequest(
            version_id=version_id,
            key=key,
            scope_path=scope_path,
            count=count,
            budget=budget or 0,
        )
        self._c.call(
            self._c.commands.RecordCounts, msg, retry_class="command", idem=idempotency_key
        )

    def upsert_digest(self, digest: m.Digest) -> None:
        # gRPC UpsertDigest is a command RPC: idempotency-key required
        # (unlike the REST PUT, which is exempt by design).
        msg = command_pb2.UpsertDigestRequest(
            digest=ledger_pb2.Digest(
                version_id=digest.version_id,
                tier=digest.tier,
                scope_path=digest.scope_path,
                content=digest.content,
                content_hash=digest.content_hash,
                built_from_seq=digest.built_from_seq,
            )
        )
        self._c.call(self._c.commands.UpsertDigest, msg, retry_class="command")


class SyncGrpcQuery:
    def __init__(self, core: _Core) -> None:
        self._c = core

    def head(self, version_id: str) -> int:
        resp = self._c.call(
            self._c.queries.GetHead,
            query_pb2.GetHeadRequest(version_id=version_id),
            retry_class="read",
        )
        return int(resp.head_seq)

    def get_claim(self, claim_id: str) -> m.ClaimLookup:
        resp = self._c.call(
            self._c.queries.GetClaim,
            query_pb2.GetClaimRequest(claim_id=claim_id),
            retry_class="read",
        )
        if not resp.HasField("claim"):
            raise UnexpectedError("GetClaimResponse without claim")
        return m.ClaimLookup(
            claim=gc.parse_claim(resp.claim),
            superseded=resp.superseded,
            superseded_by=resp.superseded_by or None,
        )

    def facts(
        self,
        version_id: str,
        *,
        scope_prefix: str | None = None,
        as_of_seq: int | None = None,
        statuses: tuple[m.ClaimStatus, ...] = (),
        limit: int | None = None,
    ) -> m.FactsPage:
        gc.reject_zero("as_of_seq", as_of_seq)
        gc.reject_zero("limit", limit)
        msg = query_pb2.SliceFactsRequest(
            version_id=version_id,
            scope_prefix=scope_prefix or "",
            as_of_seq=as_of_seq or 0,
            statuses=gc.status_filter_pb(statuses),
            limit=limit or 0,
        )
        resp = self._c.call(self._c.queries.SliceFacts, msg, retry_class="read")
        if not resp.HasField("slice"):
            raise UnexpectedError("SliceFactsResponse without slice")
        return m.FactsPage(
            facts=[gc.parse_claim(c) for c in resp.slice.facts],
            as_of_seq=resp.slice.as_of_seq,
            head_seq=resp.slice.head_seq,
        )

    def lineage(self, version_id: str) -> list[str]:
        resp = self._c.call(
            self._c.queries.GetLineage,
            query_pb2.GetLineageRequest(version_id=version_id),
            retry_class="read",
        )
        return list(resp.lineage.version_ids)

    def anchors(self, version_id: str, as_of_seq: int | None = None) -> list[m.Anchor]:
        gc.reject_zero("as_of_seq", as_of_seq)
        resp = self._c.call(
            self._c.queries.ListAnchors,
            query_pb2.ListAnchorsRequest(version_id=version_id, as_of_seq=as_of_seq or 0),
            retry_class="read",
        )
        return [gc.parse_anchor(a) for a in resp.anchors]

    def promises(
        self,
        version_id: str,
        as_of_seq: int | None = None,
        status: str | None = None,
    ) -> list[m.Promise]:
        check_promise_status(status)
        gc.reject_zero("as_of_seq", as_of_seq)
        resp = self._c.call(
            self._c.queries.ListPromises,
            query_pb2.ListPromisesRequest(
                version_id=version_id, status=status or "", as_of_seq=as_of_seq or 0
            ),
            retry_class="read",
        )
        return [gc.parse_promise(p) for p in resp.promises]

    def counters(self, version_id: str, as_of_seq: int | None = None) -> list[m.Counter]:
        gc.reject_zero("as_of_seq", as_of_seq)
        resp = self._c.call(
            self._c.queries.CounterTotals,
            query_pb2.CounterTotalsRequest(version_id=version_id, as_of_seq=as_of_seq or 0),
            retry_class="read",
        )
        return [
            m.Counter(key=c.key, total=c.total, budget=c.budget if c.budget > 0 else None)
            for c in resp.counters
        ]

    def digests(self, version_id: str) -> list[m.Digest]:
        resp = self._c.call(
            self._c.queries.ListDigests,
            query_pb2.ListDigestsRequest(version_id=version_id),
            retry_class="read",
        )
        return [
            m.Digest(
                version_id=d.version_id,
                tier=d.tier,
                scope_path=d.scope_path,
                content=d.content,
                content_hash=d.content_hash,
                built_from_seq=d.built_from_seq,
            )
            for d in resp.digests
        ]

    def compose_context(
        self,
        version_id: str,
        *,
        scope: str | None = None,
        budget_tokens: int | None = None,
        fact_limit: int | None = None,
        as_of_seq: int | None = None,
    ) -> m.ComposedContext:
        gc.reject_zero("as_of_seq", as_of_seq)
        gc.reject_zero("fact_limit", fact_limit)
        gc.reject_zero("budget_tokens", budget_tokens)
        msg = query_pb2.ComposeContextRequest(
            version_id=version_id,
            scope=scope or "",
            budget_tokens=budget_tokens or 0,
            fact_limit=fact_limit or 0,
            as_of_seq=as_of_seq or 0,
        )
        resp = self._c.call(self._c.queries.ComposeContext, msg, retry_class="read")
        if not resp.HasField("context"):
            raise UnexpectedError("ComposeContextResponse without context")
        ctx = resp.context
        return m.ComposedContext(
            sections=[m.Section(title=s.title, body=s.body) for s in ctx.sections],
            text=ctx.text,
            estimated_tokens=ctx.estimated_tokens,
            content_hash=ctx.content_hash,
            as_of_seq=ctx.as_of_seq,
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
        raise UnsupportedError(
            "findings have no gRPC RPC today — use the REST client (GET /v1/versions/{id}/findings)"
        )


class SyncGrpcIngest:
    def __init__(self, core: _Core) -> None:
        self._c = core

    def put_source(
        self,
        data: ChunkSource,
        *,
        declared_sha256: str = "",
        media_type: str | None = None,
        filename: str | None = None,
        shape_ref: str | None = None,
    ) -> m.PutSourceResult:
        """Upload source bytes. ``data`` is bytes or a zero-arg FACTORY
        returning an iterable of chunks: uploads are idempotent by content
        address, so transient failures retry — and retrying needs a fresh
        iterator."""
        attempt = 0
        while True:
            attempt += 1
            chunks = _resolve_chunks(data)
            try:
                # Streaming uploads run without the per-request deadline.
                resp = self._c.ingest.PutSource(
                    gc.source_chunks(chunks, declared_sha256, media_type, filename, shape_ref),
                    metadata=gc.metadata(self._c.token, None, self._c.uid),
                )
            except grpc.RpcError as e:
                err = gc.decode_error(e)
                if err.transient and attempt <= self._c.retries:
                    _retry.sleep_sync(attempt)
                    continue
                raise err from None
            return m.PutSourceResult(
                source_id=resp.source_id,
                content_hash=resp.content_hash,
                bytes_len=resp.bytes_len,
                already_existed=resp.already_existed,
            )

    def record_ingest(
        self, version_id: str, *, content_hash: str, shape_ref: str | None = None
    ) -> m.RecordIngestResult:
        msg = ingest_pb2.RecordIngestRequest(
            version_id=version_id, content_hash=content_hash, shape_ref=shape_ref or ""
        )
        resp = self._c.call(self._c.ingest.RecordIngest, msg)
        return m.RecordIngestResult(event_id=resp.event_id, seq=resp.seq)

    def _send_slots(self, slots: list[Any | m.IngestResult]) -> list[m.IngestResult]:
        """The per-item contract holds ACROSS transports: a file whose
        base64 cannot decode becomes its own error result (never sent), the
        valid remainder ships, and results splice back in input order —
        exactly the outcome the REST plane's server-side per-item handling
        produces."""
        to_send = [s for s in slots if not isinstance(s, m.IngestResult)]
        server_results: list[m.IngestResult] = []
        if to_send:
            # Content-addressed and per-item idempotent, but a batch can
            # partially apply — send once, like the REST file plane, and
            # DEADLINE-EXEMPT like it too (bodies run to 256 MiB).
            msg = ingest_pb2.IngestFilesRequest(files=to_send)
            resp = self._c.call(self._c.ingest.IngestFiles, msg, exempt_deadline=True)
            server_results = [gc.parse_ingest_result(r) for r in resp.results]
        return gc.splice_ingest_results(slots, server_results)

    def ingest(self, file: m.IngestFile | dict[str, Any]) -> m.IngestResult:
        """Ingest ONE document via the IngestFiles twin (a batch of one),
        with the REST ``POST /v1/ingest`` outcome shape: that route REJECTS
        a bad file (typed 400) instead of returning a per-item error, so
        here (1) a local base64 decode failure raises ``InvalidInputError``
        and (2) a server-side per-item ``error`` raises ``UnexpectedError``
        carrying the server's text — documented parity gap: the gRPC wire
        carries per-item errors as plain strings, no problem slug, so the
        REST route's typed refusal cannot be reproduced more precisely.
        ``ingest_batch`` keeps per-item outcomes."""
        slots = gc.prepare_ingest_files([m.coerce(m.IngestFile, file)])
        local = slots[0]
        if isinstance(local, m.IngestResult):
            raise InvalidInputError(f"{local.filename!r}: {local.error}")
        result = self._send_slots(slots)[0]
        if result.error is not None:
            raise UnexpectedError(f"ingest of {result.filename!r} failed: {result.error}")
        return result

    def ingest_batch(self, files: list[m.IngestFile | dict[str, Any]]) -> list[m.IngestResult]:
        """Batch ingest (1..=500 files, client-guarded) with per-item
        outcomes — one failed file does not fail the batch."""
        check_bulk_files("batch", len(files))
        return self._send_slots(gc.prepare_ingest_files([m.coerce(m.IngestFile, f) for f in files]))

    def bulk_open(
        self,
        files: list[m.BulkManifestEntry | dict[str, Any]],
        *,
        label: str | None = None,
    ) -> m.BulkOpenResult:
        raise _bulk_unsupported()

    def bulk_chunk(
        self, bulk_id: str, files: list[m.IngestFile | dict[str, Any]]
    ) -> m.BulkChunkResult:
        raise _bulk_unsupported()

    def bulk_status(self, bulk_id: str, *, include_needed: bool = False) -> m.BulkStatus:
        raise _bulk_unsupported()

    def bulk_complete(self, bulk_id: str) -> m.BulkCompleteResult:
        raise _bulk_unsupported()

    def get_source(self, source_id: str) -> m.SourceInfo:
        raise UnsupportedError(
            "source metadata has no gRPC RPC today — use the REST client "
            "(GET /v1/sources/{source_id})"
        )


class SyncGrpcRetrieval:
    def __init__(self, core: _Core) -> None:
        self._c = core

    def search(
        self,
        *,
        query: str,
        shape_ref: str,
        top_k: int | None = None,
        index_version: str | None = None,
        filter: Any | None = None,
    ) -> m.SearchResult:
        gc.reject_zero("top_k", top_k)
        msg = retrieval_pb2.HybridSearchRequest(
            query=query,
            shape_ref=shape_ref,
            top_k=top_k or 0,
            filter_json=json.dumps(filter) if filter is not None else "",
            index_version=index_version or "",
        )
        return _parse_search(self._c.call(self._c.retrieval.HybridSearch, msg, retry_class="read"))

    def index_status(self, shape_ref: str) -> m.IndexStatus:
        resp = self._c.call(
            self._c.retrieval.GetIndexVersion,
            retrieval_pb2.GetIndexVersionRequest(shape_ref=shape_ref),
            retry_class="read",
        )
        try:
            manifest = json.loads(resp.manifest_json) if resp.manifest_json else None
        except ValueError:
            manifest = None
        return m.IndexStatus(
            index_version=resp.index_version,
            shape_ref=shape_ref,
            event_watermark=resp.event_watermark,
            active=resp.active,
            manifest=manifest,
        )

    def build_index(self, shape_ref: str, version_id: str | None = None) -> m.IndexStatus:
        raise UnsupportedError(_BUILD_INDEX_UNSUPPORTED)

    def create_collection(
        self,
        *,
        name: str,
        shape_ref: str,
        access_level: int = 0,
        compartments: list[str] | None = None,
        description: str | None = None,
    ) -> m.Collection:
        """Create-or-update a compartmentalized collection. Create-or-update
        — but not replay-keyed: send once."""
        msg = retrieval_pb2.CreateCollectionRequest(
            name=name,
            shape_ref=shape_ref,
            access_level=access_level,
            compartments=compartments or [],
            description=description or "",
        )
        return gc.parse_collection(self._c.call(self._c.retrieval.CreateCollection, msg))

    def list_collections(self) -> list[m.Collection]:
        resp = self._c.call(
            self._c.retrieval.ListCollections,
            retrieval_pb2.ListCollectionsRequest(),
            retry_class="read",
        )
        return [gc.parse_collection(c) for c in resp.collections]

    def get_collection(self, id: str) -> m.Collection:
        resp = self._c.call(
            self._c.retrieval.GetCollection,
            retrieval_pb2.GetCollectionRequest(id=id),
            retry_class="read",
        )
        return gc.parse_collection(resp)


class SyncGrpcRunbooks:
    def __init__(self, core: _Core) -> None:
        self._c = core

    def apply_shape(self, yaml: str, version_id: str | None = None) -> m.ApplyShapeResult:
        msg = runbook_pb2.ApplyShapeRequest(yaml=yaml, version_id=version_id or "")
        resp = self._c.call(self._c.runbooks.ApplyShape, msg)
        return m.ApplyShapeResult(
            shape_ref=resp.shape_ref,
            # The wire doesn't carry the hash, but it is sha256(yaml bytes).
            yaml_hash=gc.yaml_hash(yaml),
            event_id=resp.event_id or None,
        )

    def apply_runbook(self, yaml: str) -> str:
        resp = self._c.call(
            self._c.runbooks.ApplyRunbook,
            runbook_pb2.ApplyRunbookRequest(yaml=yaml),
        )
        return str(resp.runbook_ref)

    def run_runbook(self, name: str, version_id: str | None = None) -> m.RunbookRun:
        params = json.dumps({"version_id": version_id}) if version_id else ""
        resp = self._c.call(
            self._c.runbooks.RunRunbook,
            runbook_pb2.RunRunbookRequest(runbook_ref=name, params_json=params),
        )
        state = resp.state or self.get_run(resp.run_id).state
        return m.RunbookRun(run_id=resp.run_id, state=state)

    def get_run(self, run_id: str) -> m.RunStatus:
        resp = self._c.call(
            self._c.runbooks.GetRun,
            runbook_pb2.GetRunRequest(run_id=run_id),
            retry_class="read",
        )
        return gc.parse_run_status(resp)

    def approve_step(self, run_id: str, ordinal: int) -> m.RunbookRun:
        resp = self._c.call(
            self._c.runbooks.ApproveStep,
            runbook_pb2.ApproveStepRequest(run_id=run_id, step_ordinal=ordinal),
        )
        state = resp.state or self.get_run(run_id).state
        return m.RunbookRun(run_id=run_id, state=state)

    def list(self, include_removed: bool = False) -> list[m.RunbookSummary]:
        resp = self._c.call(
            self._c.runbooks.ListRunbooks,
            runbook_pb2.ListRunbooksRequest(include_removed=include_removed),
            retry_class="read",
        )
        return [gc.parse_runbook_summary(r) for r in resp.runbooks]

    def get_info(self, name: str) -> m.RunbookInfo:
        resp = self._c.call(
            self._c.runbooks.GetRunbookInfo,
            runbook_pb2.GetRunbookInfoRequest(name=name),
            retry_class="read",
        )
        return gc.parse_runbook_info(resp)

    def validate(
        self,
        yaml: str,
        *,
        suggest: bool = False,
        provider: str | None = None,
        model: str | None = None,
        tier: str | None = None,
    ) -> m.ValidateResult:
        # With suggest=true this spends provider tokens — send once.
        msg = runbook_pb2.ValidateRunbookRequest(
            yaml=yaml,
            suggest=suggest,
            provider=provider or "",
            model=model or "",
            tier=tier or "",
        )
        return gc.parse_validate_result(self._c.call(self._c.runbooks.ValidateRunbook, msg))

    def remove_request(self, name: str) -> m.RemovalRequest:
        resp = self._c.call(
            self._c.runbooks.RequestRemoval,
            runbook_pb2.RequestRemovalRequest(runbook_ref=name),
        )
        return m.RemovalRequest(
            runbook_ref=resp.runbook_ref,
            removal_id=resp.removal_id,
            expires_at=resp.expires_at,
        )

    def remove_confirm(self, name: str, removal_id: str) -> m.RemovalConfirmResult:
        resp = self._c.call(
            self._c.runbooks.ConfirmRemoval,
            runbook_pb2.ConfirmRemovalRequest(runbook_ref=name, removal_id=removal_id),
        )
        return m.RemovalConfirmResult(runbook_ref=resp.runbook_ref, status=resp.status)

    def apply_chronology_rules(self, yaml: str) -> m.ApplyChronologyResult:
        raise _chronology_unsupported("POST /v1/chronology-rules")

    def get_chronology_rules(self, name: str) -> str:
        raise _chronology_unsupported("GET /v1/chronology-rules/{name}")


class SyncGrpcProviders:
    def __init__(self, core: _Core) -> None:
        self._c = core

    def apply_config(self, yaml: str) -> str:
        resp = self._c.call(
            self._c.providers.ApplyProviderConfig,
            provider_pb2.ApplyProviderConfigRequest(yaml=yaml),
        )
        return str(resp.config_name)

    def health(self, name: str) -> m.ProviderHealth:
        resp = self._c.call(
            self._c.providers.ProviderHealth,
            provider_pb2.ProviderHealthRequest(config_name=name),
            retry_class="read",
        )
        return m.ProviderHealth(
            healthy=resp.healthy,
            provider=resp.provider,
            endpoint_fingerprint=resp.endpoint_fingerprint,
            detail=resp.detail,
        )

    def health_ai(self) -> m.HealthAiResult:
        raise UnsupportedError(
            "healthai has no gRPC RPC today — use the REST client (GET /healthai)"
        )

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
        if temperature == 0.0:
            raise InvalidInputError(
                "temperature = 0.0 cannot be represented on the gRPC wire (proto3 "
                "uses 0.0 for 'absent'); omit it, or use the REST transport"
            )
        gc.reject_zero("max_tokens", max_tokens)
        msg = provider_pb2.CompleteRequest(
            config_name=name,
            model=model or "",
            provider=provider or "",
            tier=tier or "",
            system=system or "",
            prompt=prompt,
            max_tokens=max_tokens or 0,
            temperature=temperature or 0.0,
            version_id=version_id or "",
        )
        resp = self._c.call(self._c.providers.Complete, msg)
        return m.CompleteResult(
            text=resp.text,
            stop_reason=resp.stop_reason,
            input_tokens=resp.input_tokens,
            output_tokens=resp.output_tokens,
            provider=resp.provider,
            model=resp.model,
            invocation_event_id=resp.invocation_event_id or None,
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
        msg = provider_pb2.EmbedRequest(
            config_name=name,
            model=model or "",
            provider=provider or "",
            inputs=inputs,
            version_id=version_id or "",
        )
        resp = self._c.call(self._c.providers.Embed, msg)
        return m.EmbedResult(
            vectors=[list(v.values) for v in resp.vectors],
            dimensions=resp.dimensions,
            cache_hit=resp.cache_hit,
            provider=resp.provider,
            model=resp.model,
            invocation_event_id=resp.invocation_event_id or None,
        )

    def list(self) -> list[m.ProviderModels]:
        raise UnsupportedError(
            "provider disclosure has no gRPC RPC today — use the REST client (GET /v1/providers)"
        )

    def max_tokens(self) -> m.MaxTokensResponse:
        raise _max_tokens_unsupported("GET /v1/max-tokens")

    def replace_max_tokens(
        self, budgets: m.MaxTokensBudgets | dict[str, Any]
    ) -> m.MaxTokensResponse:
        raise _max_tokens_unsupported("POST /v1/max-tokens")


class SyncGrpcSessions:
    """SessionService: the data-plane session twins. ``turn_stream`` is
    REST-only (SessionService has no streaming RPC) — typed Unsupported,
    never silent."""

    def __init__(self, core: _Core) -> None:
        self._c = core

    def create(self, runbook_name: str) -> m.CreateSessionResult:
        # Opens server-side state — send once.
        resp = self._c.call(
            self._c.sessions.CreateSession,
            session_pb2.CreateSessionRequest(runbook_name=runbook_name),
        )
        return m.CreateSessionResult(
            session_id=resp.session_id,
            runbook_ref=resp.runbook_ref,
            permitted_collections=list(resp.permitted_collections),
        )

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
        gc.reject_zero("top_k", top_k)
        override = None
        if model_override is not None:
            o = m.coerce(m.ModelOverride, model_override)
            override = session_pb2.SessionModelOverride(
                provider=o.provider or "", model=o.model or "", tier=o.tier or ""
            )
        msg = session_pb2.TurnRequest(
            session_id=session_id,
            query=query,
            top_k=top_k or 0,
            complete=complete or False,
            model_override=override,
            # Empty string IS the proto's "no profile" — unlike top_k's zero
            # it is not an ambiguous sentinel (no profile can be named ""),
            # so None needs no reject_zero guard, just the default.
            research_profile=research_profile or "",
        )
        # A turn spends provider tokens — send once, never auto-retried,
        # and DEADLINE-EXEMPT like the REST twin: aborting client-side does
        # not stop the server's paid completion.
        resp = self._c.call(self._c.sessions.Turn, msg, exempt_deadline=True)
        return gc.parse_turn_response(resp)

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
        raise UnsupportedError(
            "streaming turns have no gRPC RPC today — use the REST client "
            "(POST /v1/sessions/{id}/turns/stream), or the unary turn here"
        )

    def get(self, session_id: str) -> m.Session:
        resp = self._c.call(
            self._c.sessions.GetSession,
            session_pb2.GetSessionRequest(session_id=session_id),
            retry_class="read",
        )
        return gc.parse_session(resp)

    def close(self, session_id: str) -> m.Session:
        # Idempotent by construction server-side, but still a write — sent
        # once, matching the REST transport.
        resp = self._c.call(
            self._c.sessions.CloseSession,
            session_pb2.CloseSessionRequest(session_id=session_id),
        )
        return gc.parse_session(resp)


class SyncGrpcTokens:
    """AdminService's served access-token trio (mgmt role)."""

    def __init__(self, core: _Core) -> None:
        self._c = core

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
        gc.reject_zero("ttl_secs", ttl_secs)
        # REST: runbook_refs=[] = "no runbooks"; gRPC empty repeated = ANY
        # runbook — a proto3 sentinel, refused like a zero.
        gc.reject_empty_list("runbook_refs", runbook_refs)
        msg = admin_pb2.IssueAccessTokenRequest(
            uid=uid,
            access_level=access_level,
            compartments=compartments or [],
            scopes=scopes,
            runbook_refs=runbook_refs or [],
            ttl_secs=ttl_secs or 0,
        )
        # Minting twice issues two live tokens — send once.
        resp = self._c.call(self._c.admin.IssueAccessToken, msg)
        return m.TokenGrant(token=resp.token, jti=resp.jti, expires_at=resp.expires_at)

    def list(self, *, uid: str | None = None, active: bool | None = None) -> list[m.TokenInfo]:
        # proto3 bool: active=False and active=None land on the SAME wire
        # value (false) BY DESIGN — the server treats active=false as "all",
        # identical to the REST default — so unlike the zero/empty-list
        # sentinels this is NOT a reject case: nothing is lost.
        msg = admin_pb2.ListAccessTokensRequest(uid=uid or "", active=active or False)
        resp = self._c.call(self._c.admin.ListAccessTokens, msg, retry_class="read")
        return [gc.parse_token_info(t) for t in resp.tokens]

    def revoke(self, jti: str) -> m.RevokeResult:
        resp = self._c.call(
            self._c.admin.RevokeAccessToken,
            admin_pb2.RevokeAccessTokenRequest(jti=jti),
        )
        return m.RevokeResult(
            jti=resp.jti,
            revoked=resp.revoked,
            revocation_check_enabled=resp.revocation_check_enabled,
        )


class SyncGrpcReports:
    """REST-only surface, honestly typed — the signatures mirror
    ``rest_planes.RestReports`` exactly (so mypy/IDEs see one surface) and
    every method raises ``UnsupportedError`` (AdminService.Usage is declared
    but UNIMPLEMENTED). Takes no core: nothing here touches the wire."""

    def __init__(self, core: _Core | None = None) -> None:
        pass

    def usage(
        self,
        *,
        group_by: str | None = None,
        from_: str | None = None,
        to: str | None = None,
    ) -> m.UsageReport:
        raise _reports_unsupported()

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
        raise _reports_unsupported()

    def cost(self, *, from_: str | None = None, to: str | None = None) -> m.CostReport:
        raise _reports_unsupported()

    def timeseries(
        self, *, window: str | None = None, plane: str | None = None
    ) -> m.TimeseriesReport:
        raise _reports_unsupported()

    def endpoints(
        self, *, window: str | None = None, limit: int | None = None
    ) -> m.EndpointsReport:
        raise _reports_unsupported()

    def runbooks(self, *, window: str | None = None) -> m.RunbookReport:
        raise _reports_unsupported()

    def sessions(self, *, window: str | None = None) -> m.SessionsReport:
        raise _reports_unsupported()

    def evidence(self, *, window: str | None = None) -> m.EvidenceReport:
        raise _reports_unsupported()

    def matrix(self) -> m.MatrixReport:
        raise _reports_unsupported()


class SyncGrpcAuthoring:
    """REST-only surface, honestly typed — the signatures mirror
    ``rest_planes.RestAuthoring`` exactly; no authoring RPCs exist, so every
    method raises ``UnsupportedError``. Takes no core."""

    def __init__(self, core: _Core | None = None) -> None:
        pass

    def list_patterns(self) -> list[m.PatternSummary]:
        raise _authoring_unsupported()

    def get_pattern(self, id: str) -> m.PatternDetail:
        raise _authoring_unsupported()

    def create_draft(
        self,
        *,
        name: str,
        pattern_id: str | None = None,
        seed_from_exemplar: bool = False,
    ) -> m.Draft:
        raise _authoring_unsupported()

    def list_drafts(self) -> list[m.DraftSummary]:
        raise _authoring_unsupported()

    def get_draft(self, draft_id: str) -> m.Draft:
        raise _authoring_unsupported()

    def delete_draft(self, draft_id: str) -> m.DraftDeleteResult:
        raise _authoring_unsupported()

    def put_answers(self, draft_id: str, answers: Any, *, materialize: bool = True) -> m.Draft:
        raise _authoring_unsupported()

    def validate(self, draft_id: str) -> m.DraftValidation:
        raise _authoring_unsupported()

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
        raise _authoring_unsupported()

    def export(self, draft_id: str) -> m.ExportBundle:
        raise _authoring_unsupported()

    def apply(self, draft_id: str) -> m.ApplyDraftResult:
        raise _authoring_unsupported()


def sync_core(options: ClientOptions) -> _Core:
    return _Core(options)


class SyncGrpcEvidence:
    """REST-only surface, honestly typed — the signatures mirror
    ``rest_planes.RestEvidence`` exactly; the evidence plane has no gRPC twin
    in v1, so every method raises ``UnsupportedError``. Takes no core."""

    def __init__(self, core: _Core | None = None) -> None:
        pass

    def get(self, evidence_id: str) -> dict[str, Any]:
        raise _evidence_unsupported("GET /v1/evidence/{id}")

    def rows(
        self,
        evidence_id: str,
        *,
        from_: int | None = None,
        limit: int | None = None,
    ) -> m.EvidenceRows:
        raise _evidence_unsupported("GET /v1/evidence/{id}/rows")
