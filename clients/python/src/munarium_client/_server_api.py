# SPDX-License-Identifier: Apache-2.0
"""Shared transports for the complete named Server API (Server 1.2+).

Calls are sent once. No write, model call, or interrupted stream is replayed.
JSON payloads use the normative REST schemas without losing null or integer precision.
"""

from __future__ import annotations

import json
from collections.abc import AsyncIterator, Iterator
from dataclasses import dataclass, field, replace
from typing import Any
from urllib.parse import quote, urlencode
from uuid import uuid4

import grpc
import httpx

from . import _grpc_common as gc
from ._errors import InvalidInputError, TransportError, error_from_problem
from ._options import ClientOptions
from ._proto.mmp.v1 import server_api_pb2 as pb
from ._proto.mmp.v1 import server_api_pb2_grpc as stubs

MAX_BYTES = 256 * 1024 * 1024


@dataclass
class ApiRequest:
    path: dict[str, str] = field(default_factory=dict)
    query: list[tuple[str, str]] = field(default_factory=list)
    body: bytes = b""
    content_type: str = ""
    source_headers: dict[str, str] = field(default_factory=dict)
    idempotency_key: str | None = None

    @classmethod
    def json(cls, value: Any, *, path: dict[str, str] | None = None) -> ApiRequest:
        return cls(
            path=path or {},
            body=json.dumps(value, ensure_ascii=False).encode("utf-8"),
            content_type="application/json",
        )

    def uri(self, template: str) -> str:
        for key, value in self.path.items():
            marker = "{" + key + "}"
            if marker not in template or value in {"", ".", ".."}:
                raise InvalidInputError("invalid path parameter")
            template = template.replace(marker, quote(value, safe=""))
        if "{" in template:
            raise InvalidInputError("missing path parameter")
        return template + ("?" + urlencode(self.query) if self.query else "")

    def proto(self) -> pb.ServerApiRequest:
        if len(self.body) > MAX_BYTES:
            raise InvalidInputError("request exceeds payload budget")
        return pb.ServerApiRequest(
            path_parameters=self.path,
            query_parameters=[pb.ServerApiParameter(name=k, value=v) for k, v in self.query],
            body=self.body,
            content_type=self.content_type,
            source_headers=self.source_headers,
        )


@dataclass(frozen=True)
class ApiResponse:
    status: int
    content_type: str
    body: bytes

    def json(self) -> Any:
        return json.loads(self.body)


class _Base:
    def __init__(self, options: ClientOptions, *, grpc_transport: bool = False) -> None:
        self.options = options
        self.grpc_transport = grpc_transport

    @staticmethod
    def _prepare(request: ApiRequest | None, method: str) -> ApiRequest:
        request = request or ApiRequest()
        if len(request.body) > MAX_BYTES:
            raise InvalidInputError("request exceeds payload budget")
        if method != "GET" and request.idempotency_key is None:
            request = replace(request, idempotency_key=str(uuid4()))
        return request

    def _headers(self, request: ApiRequest, media: str) -> dict[str, str]:
        headers = {"content-type": request.content_type or media}
        for name, value in request.source_headers.items():
            if name not in {"x-filename", "x-content-sha256", "x-shape-ref"}:
                raise InvalidInputError("unsupported source header")
            headers[name] = value
        if self.options.token:
            headers["authorization"] = "Bearer " + self.options.token
        if self.options.uid:
            headers["x-munarium-uid"] = self.options.uid
        if request.idempotency_key:
            headers["idempotency-key"] = request.idempotency_key
        return headers


class BaseServerApi(_Base):
    def __init__(self, options: ClientOptions, *, grpc_transport: bool = False) -> None:
        super().__init__(options, grpc_transport=grpc_transport)
        self.http = httpx.Client(timeout=httpx.Timeout(None, connect=options.connect_timeout))
        self.channel: Any = None
        self.stub: Any = None
        if grpc_transport:
            target, tls = gc.target_from_endpoint(options.endpoint)
            channel_options = [
                ("grpc.max_receive_message_length", MAX_BYTES + 65536),
                ("grpc.max_send_message_length", MAX_BYTES + 65536),
            ]
            self.channel = (
                grpc.secure_channel(target, grpc.ssl_channel_credentials(), options=channel_options)
                if tls
                else grpc.insecure_channel(target, options=channel_options)
            )
            self.stub = stubs.ServerApiServiceStub(self.channel)  # type: ignore[no-untyped-call]

    def close(self) -> None:
        self.http.close()
        if self.channel is not None:
            self.channel.close()

    def _call(
        self, rpc: str, method: str, path: str, media: str, request: ApiRequest | None = None
    ) -> ApiResponse:
        request = self._prepare(request, method)
        if self.grpc_transport:
            try:
                response = getattr(self.stub, rpc)(
                    request.proto(),
                    metadata=gc.metadata(
                        self.options.token, request.idempotency_key, self.options.uid
                    ),
                    timeout=self.options.request_timeout if method == "GET" else None,
                )
                return ApiResponse(response.status, response.content_type, response.body)
            except grpc.RpcError as error:
                raise gc.decode_error(error) from None
        try:
            with self.http.stream(
                method,
                self.options.endpoint.rstrip("/") + request.uri(path),
                headers=self._headers(request, media),
                content=request.body,
                timeout=self.options.request_timeout if method == "GET" else None,
            ) as response:
                body = bytearray()
                for part in response.iter_bytes():
                    if len(body) + len(part) > MAX_BYTES:
                        raise TransportError("response exceeds payload budget")
                    body.extend(part)
                if response.status_code >= 400:
                    raise error_from_problem(response.status_code, json.loads(body))
                return ApiResponse(
                    response.status_code, response.headers.get("content-type", ""), bytes(body)
                )
        except httpx.HTTPError as error:
            raise TransportError(str(error)) from None

    def _stream(
        self, rpc: str, method: str, path: str, media: str, request: ApiRequest | None = None
    ) -> Iterator[ApiResponse]:
        request = self._prepare(request, method)
        if self.grpc_transport:
            call = getattr(self.stub, rpc)(
                request.proto(),
                metadata=gc.metadata(self.options.token, request.idempotency_key, self.options.uid),
            )
            try:
                for part in call:
                    yield ApiResponse(part.status, part.content_type, part.body)
            except grpc.RpcError as error:
                raise gc.decode_error(error) from None
            finally:
                call.cancel()
            return
        try:
            with self.http.stream(
                method,
                self.options.endpoint.rstrip("/") + request.uri(path),
                headers=self._headers(request, media),
                content=request.body,
            ) as response:
                if response.status_code >= 400:
                    raise error_from_problem(response.status_code, json.loads(response.read()))
                for part in response.iter_bytes():
                    yield ApiResponse(
                        response.status_code, response.headers.get("content-type", ""), part
                    )
        except httpx.HTTPError as error:
            raise TransportError(str(error)) from None


class BaseAsyncServerApi(_Base):
    def __init__(self, options: ClientOptions, *, grpc_transport: bool = False) -> None:
        super().__init__(options, grpc_transport=grpc_transport)
        self.http = httpx.AsyncClient(timeout=httpx.Timeout(None, connect=options.connect_timeout))
        self.channel: Any = None
        self.stub: Any = None
        if grpc_transport:
            target, tls = gc.target_from_endpoint(options.endpoint)
            channel_options = [
                ("grpc.max_receive_message_length", MAX_BYTES + 65536),
                ("grpc.max_send_message_length", MAX_BYTES + 65536),
            ]
            self.channel = (
                grpc.aio.secure_channel(
                    target, grpc.ssl_channel_credentials(), options=channel_options
                )
                if tls
                else grpc.aio.insecure_channel(target, options=channel_options)
            )
            self.stub = stubs.ServerApiServiceStub(self.channel)  # type: ignore[no-untyped-call]

    async def close(self) -> None:
        await self.http.aclose()
        if self.channel is not None:
            await self.channel.close()

    async def _call(
        self, rpc: str, method: str, path: str, media: str, request: ApiRequest | None = None
    ) -> ApiResponse:
        request = self._prepare(request, method)
        if self.grpc_transport:
            try:
                response = await getattr(self.stub, rpc)(
                    request.proto(),
                    metadata=gc.metadata(
                        self.options.token, request.idempotency_key, self.options.uid
                    ),
                    timeout=self.options.request_timeout if method == "GET" else None,
                )
                return ApiResponse(response.status, response.content_type, response.body)
            except grpc.RpcError as error:
                raise gc.decode_error(error) from None
        try:
            async with self.http.stream(
                method,
                self.options.endpoint.rstrip("/") + request.uri(path),
                headers=self._headers(request, media),
                content=request.body,
                timeout=self.options.request_timeout if method == "GET" else None,
            ) as response:
                body = bytearray()
                async for part in response.aiter_bytes():
                    if len(body) + len(part) > MAX_BYTES:
                        raise TransportError("response exceeds payload budget")
                    body.extend(part)
                if response.status_code >= 400:
                    raise error_from_problem(response.status_code, json.loads(body))
                return ApiResponse(
                    response.status_code, response.headers.get("content-type", ""), bytes(body)
                )
        except httpx.HTTPError as error:
            raise TransportError(str(error)) from None

    async def _stream(
        self, rpc: str, method: str, path: str, media: str, request: ApiRequest | None = None
    ) -> AsyncIterator[ApiResponse]:
        request = self._prepare(request, method)
        if self.grpc_transport:
            call = getattr(self.stub, rpc)(
                request.proto(),
                metadata=gc.metadata(self.options.token, request.idempotency_key, self.options.uid),
            )
            try:
                async for part in call:
                    yield ApiResponse(part.status, part.content_type, part.body)
            except grpc.RpcError as error:
                raise gc.decode_error(error) from None
            finally:
                call.cancel()
            return
        try:
            async with self.http.stream(
                method,
                self.options.endpoint.rstrip("/") + request.uri(path),
                headers=self._headers(request, media),
                content=request.body,
            ) as response:
                if response.status_code >= 400:
                    raise error_from_problem(
                        response.status_code, json.loads(await response.aread())
                    )
                async for part in response.aiter_bytes():
                    yield ApiResponse(
                        response.status_code, response.headers.get("content-type", ""), part
                    )
        except httpx.HTTPError as error:
            raise TransportError(str(error)) from None
