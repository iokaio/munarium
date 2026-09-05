# SPDX-License-Identifier: Apache-2.0
"""Official Python client for munarium-server.

One plane surface, two transports (REST + gRPC), sync and async, typed
errors keyed on the problem-slug registry, and the head-conflict write loop
built in.

>>> from munarium_client import MunariumClient, ClientOptions
>>> client = MunariumClient.rest(ClientOptions("http://127.0.0.1:8080", token="devtoken"))
>>> v = client.commands.create_version()
>>> outcome = client.commands.propose_claim(v, subject="hero", key="eyes", value="green")

The invariants this client encodes:

1. **Disputed != error.** A gate-blocked claim returns SUCCESS with
   ``outcome.is_disputed`` plus findings (governance records, never drops).
2. **Head conflicts are normal.** ``propose_claim_with_retry`` re-reads,
   rebuilds, retries with a fresh idempotency key per attempt.
3. **One pin bounds everything.** ``as_of_seq`` threads through every query.
4. **Every retrieval answer carries a ProvenanceEnvelope** — required,
   non-optional on :class:`munarium_client.models.SearchResult`.
5. **Append-only.** No update/delete methods; corrections name
   ``supersedes_id`` explicitly.
6. **Idempotency keys** auto-generate per command and are caller-overridable.
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator, Callable
from typing import Any, cast

from . import models
from ._chunks import ChunkSource, chunks_from_bytes, chunks_from_list
from ._errors import (
    ForbiddenError,
    HeadConflictError,
    IdempotencyMismatchError,
    InvalidInputError,
    MunariumError,
    NotFoundError,
    OverloadedError,
    PolicyRejectionError,
    ProviderError,
    RateLimitedError,
    RunLockedError,
    ShapeViolationError,
    StorageError,
    TransportError,
    UnauthenticatedError,
    UnexpectedError,
    UnsupportedError,
)
from ._options import TARGET_SERVER_VERSION, ClientOptions, WriteLoopOptions
from ._retry import sleep_async, sleep_sync

__all__ = [
    "TARGET_SERVER_VERSION",
    "AsyncMunariumClient",
    "ChunkSource",
    "ClientOptions",
    "ForbiddenError",
    "HeadConflictError",
    "IdempotencyMismatchError",
    "InvalidInputError",
    "MunariumClient",
    "MunariumError",
    "NotFoundError",
    "OverloadedError",
    "PolicyRejectionError",
    "ProviderError",
    "RateLimitedError",
    "RunLockedError",
    "ShapeViolationError",
    "StorageError",
    "TransportError",
    "UnauthenticatedError",
    "UnexpectedError",
    "UnsupportedError",
    "WriteLoopOptions",
    "chunks_from_bytes",
    "chunks_from_list",
    "models",
]


def _grpc_no_server_version() -> models.ServerVersion:
    raise UnsupportedError(
        "GET /version is a REST meta route — use the REST client, or gRPC server reflection"
    )


class MunariumClient:
    """Synchronous facade: eleven plane namespaces over one transport, plus
    the ``server_version()`` meta route. Every REST-only method surfaces a
    typed ``UnsupportedError`` on the gRPC transport — never silent."""

    def __init__(
        self,
        commands: Any,
        query: Any,
        ingest: Any,
        retrieval: Any,
        runbooks: Any,
        providers: Any,
        sessions: Any,
        tokens: Any,
        reports: Any,
        authoring: Any,
        evidence: Any,
        server_version: Callable[[], models.ServerVersion],
        closer: Callable[[], None],
    ) -> None:
        self.commands = commands
        self.query = query
        self.ingest = ingest
        self.retrieval = retrieval
        self.runbooks = runbooks
        self.providers = providers
        self.sessions = sessions
        self.tokens = tokens
        self.reports = reports
        self.authoring = authoring
        self.evidence = evidence
        self._server_version = server_version
        self._closer = closer

    @classmethod
    def rest(cls, options: ClientOptions) -> MunariumClient:
        from . import _specs
        from . import rest_planes as p
        from .rest import SyncRestTransport

        t = SyncRestTransport(options)
        return cls(
            p.RestCommands(t),
            p.RestQuery(t),
            p.RestIngest(t),
            p.RestRetrieval(t),
            p.RestRunbooks(t),
            p.RestProviders(t),
            p.RestSessions(t),
            p.RestTokens(t),
            p.RestReports(t),
            p.RestAuthoring(t),
            p.RestEvidence(t),
            lambda: t.run(_specs.server_version()),
            t.close,
        )

    @classmethod
    def grpc(cls, options: ClientOptions) -> MunariumClient:
        from . import grpc_transport as g

        core = g.sync_core(options)
        return cls(
            g.SyncGrpcCommands(core),
            g.SyncGrpcQuery(core),
            g.SyncGrpcIngest(core),
            g.SyncGrpcRetrieval(core),
            g.SyncGrpcRunbooks(core),
            g.SyncGrpcProviders(core),
            g.SyncGrpcSessions(core),
            g.SyncGrpcTokens(core),
            g.SyncGrpcReports(core),
            g.SyncGrpcAuthoring(core),
            g.SyncGrpcEvidence(),
            _grpc_no_server_version,
            core.channel.close,
        )

    def server_version(self) -> models.ServerVersion:
        """GET /version — the server's name + workspace version,
        unauthenticated. REST-only: gRPC has no version RPC (use server
        reflection there)."""
        return self._server_version()

    def close(self) -> None:
        self._closer()

    def __enter__(self) -> MunariumClient:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def propose_claim_with_retry(
        self,
        version_id: str,
        build: Callable[[int], dict[str, Any]],
        opts: WriteLoopOptions | None = None,
    ) -> models.ClaimOutcome:
        """The head-conflict write loop (invariant #2): read head, build the
        claim kwargs via ``build(head)``, propose with ``expected_head`` and a
        FRESH idempotency key; on ``HeadConflictError`` back off and rebuild
        against the actual head. Never retries other errors."""
        opts = opts or WriteLoopOptions()
        head = self.query.head(version_id)
        attempt = 0
        while True:
            attempt += 1
            try:
                return cast(
                    models.ClaimOutcome,
                    self.commands.propose_claim(version_id, expected_head=head, **build(head)),
                )
            except HeadConflictError as e:
                if attempt >= opts.max_attempts:
                    raise
                sleep_sync(attempt)
                # actual == 0 = the transport carried no seqs; re-read.
                head = e.actual if e.actual > 0 else self.query.head(version_id)


class AsyncMunariumClient:
    """Asyncio facade. REST is natively async (httpx.AsyncClient); the gRPC
    variant drives the thread-safe sync stubs via ``asyncio.to_thread``."""

    def __init__(
        self,
        commands: Any,
        query: Any,
        ingest: Any,
        retrieval: Any,
        runbooks: Any,
        providers: Any,
        sessions: Any,
        tokens: Any,
        reports: Any,
        authoring: Any,
        evidence: Any,
        server_version: Callable[[], Any],
        closer: Callable[[], Any],
    ) -> None:
        self.commands = commands
        self.query = query
        self.ingest = ingest
        self.retrieval = retrieval
        self.runbooks = runbooks
        self.providers = providers
        self.sessions = sessions
        self.tokens = tokens
        self.reports = reports
        self.authoring = authoring
        self.evidence = evidence
        self._server_version = server_version
        self._closer = closer

    @classmethod
    def rest(cls, options: ClientOptions) -> AsyncMunariumClient:
        from . import _specs
        from . import rest_planes as p
        from .rest import AsyncRestTransport

        t = AsyncRestTransport(options)
        return cls(
            p.AsyncRestCommands(t),
            p.AsyncRestQuery(t),
            p.AsyncRestIngest(t),
            p.AsyncRestRetrieval(t),
            p.AsyncRestRunbooks(t),
            p.AsyncRestProviders(t),
            p.AsyncRestSessions(t),
            p.AsyncRestTokens(t),
            p.AsyncRestReports(t),
            p.AsyncRestAuthoring(t),
            p.AsyncRestEvidence(t),
            lambda: t.run(_specs.server_version()),
            t.close,
        )

    @classmethod
    def grpc(cls, options: ClientOptions) -> AsyncMunariumClient:
        from . import grpc_transport as g

        core = g.sync_core(options)

        async def close() -> None:
            core.channel.close()

        async def server_version() -> models.ServerVersion:
            return _grpc_no_server_version()

        return cls(
            commands=_Threaded(g.SyncGrpcCommands(core)),
            query=_Threaded(g.SyncGrpcQuery(core)),
            ingest=_Threaded(g.SyncGrpcIngest(core)),
            retrieval=_Threaded(g.SyncGrpcRetrieval(core)),
            runbooks=_Threaded(g.SyncGrpcRunbooks(core)),
            providers=_Threaded(g.SyncGrpcProviders(core)),
            sessions=_ThreadedSessions(g.SyncGrpcSessions(core)),
            tokens=_Threaded(g.SyncGrpcTokens(core)),
            reports=_Threaded(g.SyncGrpcReports()),
            authoring=_Threaded(g.SyncGrpcAuthoring()),
            evidence=_Threaded(g.SyncGrpcEvidence()),
            server_version=server_version,
            closer=close,
        )

    async def server_version(self) -> models.ServerVersion:
        """GET /version — the server's name + workspace version,
        unauthenticated. REST-only: gRPC raises ``UnsupportedError``."""
        return cast(models.ServerVersion, await self._server_version())

    async def close(self) -> None:
        result = self._closer()
        if asyncio.iscoroutine(result):
            await result

    async def __aenter__(self) -> AsyncMunariumClient:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.close()

    async def propose_claim_with_retry(
        self,
        version_id: str,
        build: Callable[[int], dict[str, Any]],
        opts: WriteLoopOptions | None = None,
    ) -> models.ClaimOutcome:
        opts = opts or WriteLoopOptions()
        head = await self.query.head(version_id)
        attempt = 0
        while True:
            attempt += 1
            try:
                return cast(
                    models.ClaimOutcome,
                    await self.commands.propose_claim(
                        version_id, expected_head=head, **build(head)
                    ),
                )
            except HeadConflictError as e:
                if attempt >= opts.max_attempts:
                    raise
                await sleep_async(attempt)
                head = e.actual if e.actual > 0 else await self.query.head(version_id)


class _Threaded:
    """Async adapter over a sync plane: every method call runs in a worker
    thread (grpcio sync stubs are thread-safe). Wrappers are cached on
    first access, so hot loops pay a plain attribute lookup."""

    def __init__(self, inner: Any) -> None:
        self._inner = inner

    def __getattr__(self, name: str) -> Any:
        fn = getattr(self._inner, name)

        async def call(*args: Any, **kwargs: Any) -> Any:
            return await asyncio.to_thread(fn, *args, **kwargs)

        object.__setattr__(self, name, call)
        return call


class _ThreadedSessions(_Threaded):
    """The sessions plane's adapter: ``turn_stream`` must be an ASYNC
    GENERATOR (so ``async for`` works on every async client), and on gRPC
    it raises the typed ``UnsupportedError`` on the first iteration — the
    same moment the REST twin surfaces its pre-stream failures."""

    async def turn_stream(
        self,
        session_id: str,
        *,
        query: str,
        top_k: int | None = None,
        complete: bool | None = None,
        model_override: models.ModelOverride | dict[str, Any] | None = None,
        research_profile: str | None = None,
    ) -> AsyncIterator[models.TurnStreamEvent]:
        # The sync plane raises UnsupportedError; nothing is ever yielded.
        for item in self._inner.turn_stream(
            session_id,
            query=query,
            top_k=top_k,
            complete=complete,
            model_override=model_override,
            research_profile=research_profile,
        ):
            yield item
