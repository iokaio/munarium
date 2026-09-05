# SPDX-License-Identifier: Apache-2.0
"""A minimal incremental Server-Sent-Events parser for the streaming turn
plane (``POST /v1/sessions/{id}/turns/stream``). Hand-rolled by design —
the server's emitter is simple (named events, single-line JSON data,
comment keep-alives) and a dependency would be heavier than the format.

The parser is PURE: feed it byte chunks as they arrive (chunk boundaries
may fall anywhere, including mid-line and mid-UTF-8-codepoint) and it
yields complete events. Per the SSE grammar it handles ``event:``/``data:``
fields, multi-line data accumulation, ``\\n``/``\\r\\n``/``\\r`` line
endings, comment lines (leading ``:``, the keep-alive form), and ignores
fields it does not know (``id:``, ``retry:``).

Retention is bounded: a stream that never terminates its lines or events
cannot grow client memory without limit — past :data:`MAX_EVENT_BYTES` the
parser reports overflow and the caller ends the stream with a typed error
instead of buffering toward an OOM kill.

The turn-stream event semantics (shared by the sync and async transports
via :class:`TurnEventMachine`): N ``progress`` events, then exactly one
terminal ``done`` (the full TurnResult) or ``error`` (problem+json, decoded
through the standard slug registry). Undecodable PROGRESS data is skipped —
a newer server may add stages this build cannot name, and progress is
informational — but an undecodable terminal event is an error: the caller
was owed a TurnResult. Nothing rides after the terminal event.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field

from . import models as m
from ._errors import MunariumError, UnexpectedError, error_from_problem

#: Upper bound on one event's buffered bytes (pending line + accumulated
#: data). A terminal ``done`` event carries a whole TurnResult — hits text
#: included — so the cap is generous; anything past it is a misbehaving
#: peer, not a real event.
MAX_EVENT_BYTES = 16 * 1024 * 1024

_EOL = re.compile(rb"\r\n|\r|\n")


@dataclass(slots=True)
class SseEvent:
    """One dispatched SSE event: the event name (empty = the spec's default
    "message") and the joined data payload."""

    event: str
    data: str


class SseOverflow(Exception):
    """The parser refused to buffer further — the peer sent more than
    :data:`MAX_EVENT_BYTES` without completing an event."""


@dataclass(slots=True)
class SseParser:
    #: Undelivered raw bytes (a partial line, possibly a partial codepoint).
    _buf: bytearray = field(default_factory=bytearray)
    #: Accumulated ``data:`` lines for the event being built.
    _data: list[str] = field(default_factory=list)
    #: Bytes across ``_data`` (tracked so the cap is O(1) to enforce).
    _data_bytes: int = 0
    #: The pending ``event:`` name.
    _event: str = ""
    #: True when the previous byte was CR — a following LF is the same
    #: line ending, not an extra blank line.
    _saw_cr: bool = False
    #: Set once the cap was exceeded. The push that tripped it still
    #: returned the events it completed (a ``done`` followed by oversized
    #: trailing bytes must not lose the done); the NEXT push raises.
    _poisoned: bool = False

    def push(self, chunk: bytes) -> list[SseEvent]:
        """Feed one chunk; returns every event completed by it, in order.

        The retention cap is exact: a ``data:`` line is never appended past
        it, and the pending partial line counts too. Exceeding it raises
        :class:`SseOverflow` immediately when the chunk completed nothing;
        otherwise the completed events are returned and the NEXT push
        raises (the parser is poisoned — nothing after the overflow can be
        trusted, but what completed before it was real).
        """
        if self._poisoned:
            raise SseOverflow
        out: list[SseEvent] = []
        buf = self._buf
        buf += chunk
        pos = 0  # scan offset: every byte is examined once
        if self._saw_cr and buf:
            if buf[0] == 0x0A:
                pos = 1  # the LF completing a CRLF split across chunks
            self._saw_cr = False
        while True:
            match = _EOL.search(buf, pos)
            if match is None:
                break
            start, end = pos, match.start()
            pos = match.end()
            if pos == len(buf) and match.group() == b"\r":
                self._saw_cr = True  # a CRLF may straddle the chunk boundary
            if not self._line(buf, start, end, out):
                self._poisoned = True
                break
        del buf[:pos]
        if not self._poisoned and len(buf) + self._data_bytes > MAX_EVENT_BYTES:
            self._poisoned = True
        if self._poisoned and not out:
            raise SseOverflow
        return out

    def _line(self, buf: bytearray, start: int, end: int, out: list[SseEvent]) -> bool:
        """Handle the line ``buf[start:end]``. Returns False when a data
        line would push retention past the cap (nothing is appended)."""
        if start == end:
            # Blank line = dispatch. An event with no data lines is dropped
            # per the SSE spec (this is what makes comment keep-alives free)
            # — and the stale event name must not leak into the next event.
            if self._data:
                out.append(SseEvent(event=self._event, data="\n".join(self._data)))
                self._data = []
            self._event = ""
            self._data_bytes = 0
            return True
        if buf[start] == 0x3A:  # ':' — comment / keep-alive
            return True
        colon = buf.find(b":", start, end)
        if colon < 0:
            return True  # field-less line: ignored
        name = buf[start:colon]
        value_start = colon + 1
        if value_start < end and buf[value_start] == 0x20:
            value_start += 1
        # Decode only the value span — one view, no intermediate copies.
        if name == b"event":
            self._event = str(memoryview(buf)[value_start:end], "utf-8", "replace")
        elif name == b"data":
            size = end - value_start
            if self._data_bytes + size > MAX_EVENT_BYTES:
                return False
            self._data_bytes += size
            self._data.append(str(memoryview(buf)[value_start:end], "utf-8", "replace"))
        # id / retry / unknown fields: ignored
        return True


def classify_turn_event(ev: SseEvent) -> m.TurnStreamEvent | MunariumError | None:
    """Classify one SSE event from the turn stream into the item it carries.

    ``None`` = skip (undecodable progress, unknown event names — both are
    forward-compat, never fatal). An :class:`MunariumError` is the stream's
    terminal error — the ``error`` event carries the same problem+json body
    the unary route would have returned, decoded through the one slug
    registry — and a :class:`models.TurnResult` is the terminal success.
    "Terminal" is derived from the item type by the machine, not returned.
    """
    if ev.event == "progress":
        try:
            return m.TurnProgress.model_validate_json(ev.data)
        except ValueError:
            return None
    if ev.event == "done":
        try:
            return m.TurnResult.model_validate_json(ev.data)
        except ValueError as e:
            return UnexpectedError(f"undecodable SSE done event: {e}")
    if ev.event == "error":
        try:
            body = json.loads(ev.data)
        except ValueError as e:
            return UnexpectedError(f"undecodable SSE error event: {e}")
        # error_from_problem handles a non-dict body itself; the status is
        # only needed as the HTTP-status fallback it records.
        status = body.get("status") if isinstance(body, dict) else None
        return error_from_problem(status if isinstance(status, int) else 500, body)
    return None  # unnamed/unknown events: ignored (forward-compat)


class TurnEventMachine:
    """The one turn-stream reducer both transports drive: feed raw byte
    chunks, collect the items to yield, and surface the typed terminal
    error. The invariants live here — exactly one terminal item, everything
    after it dropped — so the sync and async loops cannot drift.

    The caller's loop, per chunk: yield everything ``feed`` returned, THEN
    raise ``error`` if set, THEN stop when ``terminal`` — that ordering
    keeps progress events that arrived in the same chunk as a terminal
    error visible before the raise (the Rust stream's queue semantics).
    """

    def __init__(self) -> None:
        self._parser = SseParser()
        #: True once the terminal done/error event was seen — the caller
        #: must stop reading (nothing rides after the terminal event).
        self.terminal = False
        #: The typed terminal error (an ``error`` event or overflow), to be
        #: raised AFTER the items ``feed`` returned alongside it.
        self.error: MunariumError | None = None

    def feed(self, chunk: bytes) -> list[m.TurnStreamEvent]:
        """Feed one chunk; returns the items to yield, in order."""
        try:
            events = self._parser.push(chunk)
        except SseOverflow:
            self.terminal = True
            self.error = UnexpectedError(
                f"SSE peer exceeded the {MAX_EVENT_BYTES // (1024 * 1024)} MiB "
                "event buffer without completing an event"
            )
            return []
        out: list[m.TurnStreamEvent] = []
        for ev in events:
            if self.terminal:
                break  # nothing rides after the terminal event
            item = classify_turn_event(ev)
            if item is None:
                continue
            if isinstance(item, MunariumError):
                self.error = item
                self.terminal = True
            else:
                out.append(item)
                self.terminal = isinstance(item, m.TurnResult)
        return out
