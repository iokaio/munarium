# SPDX-License-Identifier: Apache-2.0
"""Connection + behavior options shared by both transports."""

from __future__ import annotations

from dataclasses import dataclass

#: The server version this client tracks (lockstep with the repo workspace).
TARGET_SERVER_VERSION = "1.0.0"


@dataclass(slots=True)
class ClientOptions:
    """Options for :class:`munarium_client.MunariumClient` /
    :class:`munarium_client.AsyncMunariumClient`.

    ``endpoint``: REST base URL (``http://host:8080``) or gRPC target
    (``host:50051`` or ``http[s]://host:50051``).
    ``token``: bearer token — a static token, or a capability JWT for the
    data plane; ``None`` only works against ``MUNARIUM_AUTH_MODE=disabled``.
    ``uid``: the acting end-user id (uid contract). Sent as
    ``X-Munarium-Uid`` (REST) / ``munarium-uid`` metadata (gRPC) on every request.
    Required by servers running ``MUNARIUM_REQUIRE_UID=true`` (the default);
    when the bearer is a capability JWT it must equal the token's ``sub``.
    ``read_retries``: extra attempts for READS (and search) on transient
    failures; commands are re-sent with the SAME idempotency key ONLY when
    the request provably never reached the server (a REST connect-phase
    failure) or the server shed it before executing (the typed
    ``overloaded``) — the server records an idempotency key AFTER a command
    completes, so a possibly-delivered command is never re-sent (it could
    execute twice). On gRPC, commands are NEVER re-sent on a transport
    failure: no gRPC failure is provably undelivered (a failed reconnect and
    a broken established stream both surface as UNAVAILABLE), so there the
    typed ``overloaded`` is the ONLY command retry.
    """

    endpoint: str
    token: str | None = None
    uid: str | None = None
    connect_timeout: float = 5.0
    #: Per-request deadline in seconds. Streaming ingest, unary session
    #: turns, and the file/bulk ingest writes are exempt (a client-side
    #: abort cannot stop a paid completion, and bulk bodies run to 256 MiB).
    request_timeout: float = 30.0
    read_retries: int = 2


@dataclass(slots=True)
class WriteLoopOptions:
    """Options for the head-conflict write loop."""

    #: Max attempts including the first.
    max_attempts: int = 3
