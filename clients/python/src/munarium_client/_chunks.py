# SPDX-License-Identifier: Apache-2.0
"""Replayable chunk sources for content-addressed uploads.

An upload is idempotent by content address, so a transient failure is safe to
retry — but retrying needs a FRESH iterator. Callers therefore pass either
raw ``bytes`` or a zero-arg callable that builds the chunk iterable, and the
transports call it once per attempt.
"""

from __future__ import annotations

from collections.abc import AsyncIterable, Callable, Iterable

from ._errors import InvalidInputError

#: bytes, or a factory producing a fresh chunk iterable per attempt.
ChunkSource = bytes | Callable[[], Iterable[bytes]] | Callable[[], AsyncIterable[bytes]]


def chunks_from_bytes(data: bytes, chunk_size: int = 64 * 1024) -> Callable[[], Iterable[bytes]]:
    """A replayable source that re-slices in-memory bytes per attempt."""

    def build() -> Iterable[bytes]:
        return (data[i : i + chunk_size] for i in range(0, len(data), chunk_size))

    return build


def chunks_from_list(chunks: list[bytes]) -> Callable[[], Iterable[bytes]]:
    """A replayable source over a pre-split chunk list."""

    def build() -> Iterable[bytes]:
        return iter(chunks)

    return build


def resolve_chunks(source: ChunkSource) -> bytes | Iterable[bytes] | AsyncIterable[bytes]:
    """Build one attempt's content from a chunk source.

    Rejects a bare iterator up front: a one-shot iterable would be exhausted
    by the first attempt and the retry would silently upload ZERO bytes —
    which, without a declared hash, the server happily stores and returns as
    ``already_existed``. Callers pass bytes or a factory.
    """
    if isinstance(source, (bytes, bytearray, memoryview)):
        return bytes(source)
    if callable(source):
        return source()
    raise InvalidInputError(
        "put_source needs a REPLAYABLE chunk source: pass bytes, or a zero-arg "
        "callable returning a fresh iterable (see chunks_from_bytes / "
        "chunks_from_list). A bare iterator cannot be re-read on retry.",
    )


async def as_async_chunks(
    built: bytes | Iterable[bytes] | AsyncIterable[bytes],
) -> AsyncIterable[bytes]:
    """Adapt any built source to the async iterable httpx's async transport
    requires — a sync generator would trip an assertion deep inside httpx."""
    if isinstance(built, (bytes, bytearray, memoryview)):
        yield bytes(built)
        return
    if isinstance(built, AsyncIterable):
        async for chunk in built:
            yield chunk
        return
    for chunk in built:
        yield chunk
