# SPDX-License-Identifier: Apache-2.0
"""REST transport (httpx): problem+json error decoding, automatic
idempotency keys on commands, bounded retries by request class:

- reads (+search): transport-level failures (connect, reset, timeout) and
  transient server outcomes (overloaded / 5xx gateway) retried with backoff;
- core commands: re-sent with the SAME idempotency key ONLY when the
  request provably never reached the server (a connect-phase failure) or
  the server shed it before executing — the server records an idempotency
  key AFTER a command completes, so a possibly-delivered command is never
  re-sent (it could execute twice);
- non-idempotent writes (turns, provider calls, ingest, …): sent exactly
  once.

Timeout posture: most requests carry the per-request deadline; specs marked
``timeout="exempt"`` (unary turns, the file/bulk ingest writes) ride
without one — a turn spends provider tokens a client-side abort cannot
stop, and bulk bodies run to 256 MiB. The SSE turn stream has no overall
deadline either, but a 60 s idle watchdog: the server heartbeats comment
keep-alives every 15 s, so a silent wire means a wedged peer, not a slow
completion.
"""

from __future__ import annotations

import email.utils
import json
import uuid
from collections.abc import AsyncIterator, Iterator
from datetime import UTC, datetime
from typing import Any, TypeVar

import httpx

from . import _retry
from . import models as m
from ._chunks import ChunkSource, as_async_chunks, resolve_chunks
from ._errors import (
    MunariumError,
    OverloadedError,
    TransportError,
    UnexpectedError,
    error_from_problem,
)
from ._options import ClientOptions
from ._specs import Spec
from ._sse import TurnEventMachine

T = TypeVar("T")

_SSE_HEADERS = {"accept": "text/event-stream", "content-type": "application/json"}
_SSE_ENDED_EARLY = "SSE stream ended without a terminal done/error event"
_SSE_IDLE_SECS = 60.0
_SSE_IDLE = (
    f"SSE stream idle for {_SSE_IDLE_SECS:.0f}s (the server heartbeats every 15s) — wedged "
    "peer; the turn may still be executing server-side (the completion was paid) — read "
    "the session transcript before re-sending"
)

_JSON_CHUNK_CHARS = 64 * 1024


def _iter_json_bytes(value: Any) -> Iterator[bytes]:
    """Encode JSON with bounded temporary allocations.

    ``httpx``'s ``json=`` convenience serializes the whole document to one
    byte string. File-ingest DTOs already retain their base64 text, so that
    doubles peak request memory near the server's 256 MiB ceiling. This
    small recursive encoder preserves standard JSON escaping while slicing
    large strings into bounded pieces and lets httpx stream the result.
    """
    if value is None:
        yield b"null"
    elif value is True:
        yield b"true"
    elif value is False:
        yield b"false"
    elif isinstance(value, str):
        yield b'"'
        for start in range(0, len(value), _JSON_CHUNK_CHARS):
            piece = value[start : start + _JSON_CHUNK_CHARS]
            # Dump a standalone string, then remove only its surrounding
            # quotes. Escapes are self-contained per Python character, so
            # concatenating the interiors remains one valid JSON string.
            escaped = json.dumps(piece, ensure_ascii=False)[1:-1]
            yield escaped.encode("utf-8")
        yield b'"'
    elif isinstance(value, (int, float)):
        yield json.dumps(value, allow_nan=False, separators=(",", ":")).encode()
    elif isinstance(value, dict):
        yield b"{"
        for index, (key, item) in enumerate(value.items()):
            if not isinstance(key, str):
                raise TypeError("streaming JSON object keys must be strings")
            if index:
                yield b","
            yield json.dumps(key, ensure_ascii=False).encode("utf-8")
            yield b":"
            yield from _iter_json_bytes(item)
        yield b"}"
    elif isinstance(value, (list, tuple)):
        yield b"["
        for index, item in enumerate(value):
            if index:
                yield b","
            yield from _iter_json_bytes(item)
        yield b"]"
    else:
        raise TypeError(f"unsupported streaming JSON value: {type(value).__name__}")


async def _aiter_json_bytes(value: Any) -> AsyncIterator[bytes]:
    for chunk in _iter_json_bytes(value):
        yield chunk


def _retry_after_seconds(resp: httpx.Response) -> float | None:
    """Parse Retry-After: delta-seconds or an RFC 9110 HTTP-date."""
    raw = resp.headers.get("retry-after")
    if raw is None:
        return None
    try:
        return float(raw)
    except ValueError:
        pass
    try:
        when = email.utils.parsedate_to_datetime(raw)
    except (TypeError, ValueError):
        return None
    return max((when - datetime.now(UTC)).total_seconds(), 0.0)


def _decode(resp: httpx.Response, *, raw: bool = False) -> Any:
    """The ONE error-decoding path for non-success responses — problem+json
    through the slug registry with the Retry-After header preserved. Every
    consumer of an error response (unary decode, raw-text reads, the SSE
    pre-stream refusal) goes through here so none can drift. ``raw`` makes
    the SUCCESS body text instead of JSON (chronology-rules readback)."""
    if resp.is_success:
        if raw:
            return resp.text
        try:
            return resp.json()
        except ValueError as e:
            raise UnexpectedError(f"undecodable success body: {e}", resp.status_code) from None
    try:
        body = resp.json()
    except ValueError:
        raise UnexpectedError(
            f"non-JSON error body (HTTP {resp.status_code})", resp.status_code
        ) from None
    raise error_from_problem(resp.status_code, body, _retry_after_seconds(resp))


def _command_key(spec: Spec[Any], idempotency_key: str | None) -> str | None:
    if spec.retry != "command":
        return None
    return idempotency_key or str(uuid.uuid4())


def _headers(spec: Spec[Any], idem_key: str | None) -> dict[str, str]:
    headers: dict[str, str] = {}
    if idem_key is not None:
        headers["idempotency-key"] = idem_key
    if spec.yaml is not None:
        headers["content-type"] = "text/yaml"
    elif spec.stream_json:
        headers["content-type"] = "application/json"
    return headers


def _retryable(spec: Spec[Any], attempt: int, retries: int, *, delivered: bool) -> bool:
    """Reads retry on any transient failure. Commands retry ONLY when the
    request provably never reached the server: the server records an
    idempotency key AFTER the command completes, so a retry that overtakes
    an in-flight attempt would execute it twice. un-keyed writes never retry."""
    if attempt > retries:
        return False
    if spec.retry == "read":
        return True
    return spec.retry == "command" and not delivered


def _delivered(e: Exception) -> bool:
    """False only for connect-phase failures — the request never left."""
    return not isinstance(e, (httpx.ConnectError, httpx.ConnectTimeout, httpx.ProxyError))


def _source_headers(
    declared_sha256: str,
    media_type: str | None,
    filename: str | None,
    shape_ref: str | None,
) -> dict[str, str]:
    headers = {"content-type": media_type or "application/octet-stream"}
    if declared_sha256:
        headers["x-content-sha256"] = declared_sha256
    if filename:
        headers["x-filename"] = filename
    if shape_ref:
        headers["x-shape-ref"] = shape_ref
    return headers


def _client_headers(options: ClientOptions) -> dict[str, str]:
    """Client-level default headers: bearer auth + the uid. Applied to
    every request (httpx merges client defaults into per-request headers), so
    the uid contract covers streaming source upload too."""
    headers: dict[str, str] = {}
    if options.token:
        headers["authorization"] = f"Bearer {options.token}"
    if options.uid:
        headers["x-munarium-uid"] = options.uid
    return headers


class SyncRestTransport:
    def __init__(
        self, options: ClientOptions, *, transport: httpx.BaseTransport | None = None
    ) -> None:
        # ``transport`` is the test seam (httpx.MockTransport pins the
        # retry rules offline); production always uses httpx's default.
        self._client = httpx.Client(
            base_url=options.endpoint.rstrip("/"),
            timeout=httpx.Timeout(options.request_timeout, connect=options.connect_timeout),
            headers=_client_headers(options),
            transport=transport,
        )
        # Deadline-exempt sends (streaming ingest, unary turns, file/bulk
        # ingest): same client (one connection pool), per-request override.
        self._stream_timeout = httpx.Timeout(None, connect=options.connect_timeout)
        # The SSE posture: no overall deadline (a capable-tier completion
        # can exceed 30 s), but a 60 s idle watchdog — the server heartbeats
        # keep-alive comments every 15 s, so a silent wire means a wedged
        # peer and the caller gets a typed transport error instead of
        # hanging forever.
        self._sse_timeout = httpx.Timeout(
            None, connect=options.connect_timeout, read=_SSE_IDLE_SECS
        )
        self._retries = options.read_retries

    def close(self) -> None:
        self._client.close()

    def run(self, spec: Spec[T], idempotency_key: str | None = None) -> T:
        idem = _command_key(spec, idempotency_key)
        attempt = 0
        while True:
            attempt += 1
            try:
                resp = self._client.request(
                    spec.method,
                    spec.path,
                    params=spec.params or None,
                    json=None if spec.stream_json else spec.json,
                    content=(_iter_json_bytes(spec.json) if spec.stream_json else spec.yaml),
                    headers=_headers(spec, idem),
                    timeout=(
                        self._stream_timeout
                        if spec.timeout == "exempt"
                        else httpx.USE_CLIENT_DEFAULT
                    ),
                )
            except httpx.HTTPError as e:
                # Connect failures, resets, and timeouts are all retryable
                # for reads (idempotent); commands only when provably
                # undelivered.
                if _retryable(spec, attempt, self._retries, delivered=_delivered(e)):
                    _retry.sleep_sync(attempt)
                    continue
                raise TransportError(str(e)) from None
            try:
                return spec.parse(_decode(resp, raw=spec.raw))
            except MunariumError as e:
                # Reads retry any typed transient. Commands retry ONLY the
                # typed `overloaded` — the server provably shed the request
                # BEFORE executing. A transient 502/504 from a gateway means
                # the command may still be executing upstream, so re-sending
                # could execute it twice (the C10 review's cross-client
                # finding; Rust always classified this correctly).
                shed = isinstance(e, OverloadedError)
                if e.transient and _retryable(spec, attempt, self._retries, delivered=not shed):
                    _retry.sleep_sync(attempt)
                    continue
                raise

    def turn_stream(self, path: str, body: dict[str, Any]) -> Iterator[m.TurnStreamEvent]:
        """Open the SSE turn stream and yield its typed events. Pre-stream
        failures (auth, refusals, shed) are plain problem+json — decoded by
        the ONE error path, Retry-After included; they raise on the first
        ``next()``. A stream that ends without a terminal done/error event
        raises a typed transport error — never a silent success. When the
        idle watchdog fires, the raised ``TransportError`` says so: the turn
        may still be executing server-side (the completion was paid)."""
        try:
            with self._client.stream(
                "POST", path, json=body, headers=_SSE_HEADERS, timeout=self._sse_timeout
            ) as resp:
                if not resp.is_success:
                    resp.read()
                    _decode(resp)  # raises the typed problem
                machine = TurnEventMachine()
                for chunk in resp.iter_bytes():
                    yield from machine.feed(chunk)
                    if machine.error is not None:
                        raise machine.error
                    if machine.terminal:
                        return
                raise TransportError(_SSE_ENDED_EARLY)
        except httpx.ReadTimeout:
            raise TransportError(_SSE_IDLE) from None
        except httpx.HTTPError as e:
            raise TransportError(str(e)) from None

    def put_source(
        self,
        data: ChunkSource,
        declared_sha256: str,
        media_type: str | None,
        filename: str | None,
        shape_ref: str | None,
    ) -> Any:
        # Uploads are idempotent by content address, so transient failures
        # retry — the chunk SOURCE is rebuilt per attempt.
        headers = _source_headers(declared_sha256, media_type, filename, shape_ref)
        attempt = 0
        while True:
            attempt += 1
            content = resolve_chunks(data)
            try:
                resp = self._client.put(
                    "/v1/sources",
                    content=content,
                    headers=headers,
                    timeout=self._stream_timeout,
                )
            except httpx.HTTPError as e:
                if attempt <= self._retries:
                    _retry.sleep_sync(attempt)
                    continue
                raise TransportError(str(e)) from None
            try:
                return _decode(resp)
            except MunariumError as e:
                if e.transient and attempt <= self._retries:
                    _retry.sleep_sync(attempt)
                    continue
                raise


class AsyncRestTransport:
    def __init__(
        self, options: ClientOptions, *, transport: httpx.AsyncBaseTransport | None = None
    ) -> None:
        # ``transport`` is the same test seam the sync transport has, so the
        # async twin's routes are pinned offline by the same mocks.
        self._client = httpx.AsyncClient(
            base_url=options.endpoint.rstrip("/"),
            timeout=httpx.Timeout(options.request_timeout, connect=options.connect_timeout),
            headers=_client_headers(options),
            transport=transport,
        )
        self._stream_timeout = httpx.Timeout(None, connect=options.connect_timeout)
        # See SyncRestTransport: 60 s idle watchdog over 15 s heartbeats.
        self._sse_timeout = httpx.Timeout(
            None, connect=options.connect_timeout, read=_SSE_IDLE_SECS
        )
        self._retries = options.read_retries

    async def close(self) -> None:
        await self._client.aclose()

    async def run(self, spec: Spec[T], idempotency_key: str | None = None) -> T:
        idem = _command_key(spec, idempotency_key)
        attempt = 0
        while True:
            attempt += 1
            try:
                resp = await self._client.request(
                    spec.method,
                    spec.path,
                    params=spec.params or None,
                    json=None if spec.stream_json else spec.json,
                    content=(_aiter_json_bytes(spec.json) if spec.stream_json else spec.yaml),
                    headers=_headers(spec, idem),
                    timeout=(
                        self._stream_timeout
                        if spec.timeout == "exempt"
                        else httpx.USE_CLIENT_DEFAULT
                    ),
                )
            except httpx.HTTPError as e:
                if _retryable(spec, attempt, self._retries, delivered=_delivered(e)):
                    await _retry.sleep_async(attempt)
                    continue
                raise TransportError(str(e)) from None
            try:
                return spec.parse(_decode(resp, raw=spec.raw))
            except MunariumError as e:
                # Reads retry any typed transient. Commands retry ONLY the
                # typed `overloaded` (see the sync twin's comment).
                shed = isinstance(e, OverloadedError)
                if e.transient and _retryable(spec, attempt, self._retries, delivered=not shed):
                    await _retry.sleep_async(attempt)
                    continue
                raise

    async def turn_stream(
        self, path: str, body: dict[str, Any]
    ) -> AsyncIterator[m.TurnStreamEvent]:
        """Async twin of ``SyncRestTransport.turn_stream`` — same event
        machine, same invariants (see there).

        Early exit: an async generator abandoned mid-stream is finalized by
        the event loop's garbage collector at an unspecified later time, and
        the pooled connection stays checked out until then. To leave a
        stream early, wrap it in ``contextlib.aclosing(...)`` — its
        ``__aexit__`` runs the generator's cleanup (closing the response and
        releasing the connection) deterministically::

            async with aclosing(client.sessions.turn_stream(sid, query=q)) as events:
                async for item in events:
                    if isinstance(item, TurnProgress) and item.stage == "model":
                        break
        """
        try:
            async with self._client.stream(
                "POST", path, json=body, headers=_SSE_HEADERS, timeout=self._sse_timeout
            ) as resp:
                if not resp.is_success:
                    await resp.aread()
                    _decode(resp)  # raises the typed problem
                machine = TurnEventMachine()
                async for chunk in resp.aiter_bytes():
                    for item in machine.feed(chunk):
                        yield item
                    if machine.error is not None:
                        raise machine.error
                    if machine.terminal:
                        return
                raise TransportError(_SSE_ENDED_EARLY)
        except httpx.ReadTimeout:
            raise TransportError(_SSE_IDLE) from None
        except httpx.HTTPError as e:
            raise TransportError(str(e)) from None

    async def put_source(
        self,
        data: ChunkSource,
        declared_sha256: str,
        media_type: str | None,
        filename: str | None,
        shape_ref: str | None,
    ) -> Any:
        headers = _source_headers(declared_sha256, media_type, filename, shape_ref)
        attempt = 0
        while True:
            attempt += 1
            content = as_async_chunks(resolve_chunks(data))
            try:
                resp = await self._client.put(
                    "/v1/sources",
                    content=content,
                    headers=headers,
                    timeout=self._stream_timeout,
                )
            except httpx.HTTPError as e:
                if attempt <= self._retries:
                    await _retry.sleep_async(attempt)
                    continue
                raise TransportError(str(e)) from None
            try:
                return _decode(resp)
            except MunariumError as e:
                if e.transient and attempt <= self._retries:
                    await _retry.sleep_async(attempt)
                    continue
                raise
