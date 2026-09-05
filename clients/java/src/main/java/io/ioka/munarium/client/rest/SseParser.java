// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.rest;

import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * A minimal incremental Server-Sent-Events parser for the streaming turn
 * plane. Hand-rolled by design — the server's emitter is simple (named
 * events, single-line JSON data, comment keep-alives) and a dependency
 * would be heavier than the format.
 *
 * <p>PURE: feed byte chunks as they arrive (boundaries may fall anywhere,
 * including mid-line and mid-UTF-8-codepoint) and it yields complete
 * events. Handles {@code event:}/{@code data:} fields, multi-line data
 * accumulation, {@code \n}/{@code \r\n}/{@code \r} line endings, comment
 * keep-alives, and unknown fields ({@code id:}, {@code retry:}) ignored.
 *
 * <p>Retention is bounded: past {@link #MAX_EVENT_BYTES} of buffered bytes
 * without a completed event, {@link Overflow} is thrown — a misbehaving
 * peer must not grow client memory toward an OOM kill.
 */
final class SseParser {
    /** Generous — a terminal done event carries a whole TurnResponse. */
    static final int MAX_EVENT_BYTES = 16 * 1024 * 1024;

    record Event(String event, String data) {}

    static final class Overflow extends RuntimeException {
        Overflow() {
            super("SSE peer exceeded the event buffer without completing an event");
        }
    }

    private final ByteArrayOutputStream buf = new ByteArrayOutputStream();
    private final List<String> data = new ArrayList<>();
    private int dataBytes;
    private String event = "";
    private boolean sawCr;
    /** The cap was exceeded on an earlier push: events completed in that
     * push were still delivered (a done beside oversized trailing bytes is a
     * real result), and the NEXT push reports the overflow. */
    private boolean poisoned;

    /** Feed one chunk; returns every event completed by it, in order. */
    List<Event> push(byte[] chunk, int len) {
        if (poisoned) {
            throw new Overflow();
        }
        List<Event> out = new ArrayList<>();
        for (int i = 0; i < len; i++) {
            byte b = chunk[i];
            if (b == '\n' && sawCr) {
                sawCr = false; // CRLF: LF consumed
            } else if (b == '\r' || b == '\n') {
                sawCr = b == '\r';
                line(buf.toByteArray(), out);
                buf.reset();
            } else {
                sawCr = false;
                buf.write(b);
            }
        }
        if (buf.size() + dataBytes > MAX_EVENT_BYTES) {
            poisoned = true;
            if (out.isEmpty()) {
                throw new Overflow();
            }
        }
        return out;
    }

    private void line(byte[] raw, List<Event> out) {
        if (raw.length == 0) {
            // Blank line = dispatch. An event with no data lines is dropped
            // per the SSE spec (comment keep-alives are free); the stale
            // event name must not leak into the next event.
            if (!data.isEmpty()) {
                out.add(new Event(event, String.join("\n", data)));
                data.clear();
            }
            event = "";
            dataBytes = 0;
            return;
        }
        if (raw[0] == ':') {
            return; // comment / keep-alive
        }
        // The retention cap counts BYTES (raw line length — a slight
        // overcount including the field prefix, fine for a bound): counting
        // String.length() chars would let multi-byte payloads exceed the
        // documented cap ~3-4x before tripping.
        String text = new String(raw, StandardCharsets.UTF_8);
        int colon = text.indexOf(':');
        String field = colon < 0 ? text : text.substring(0, colon);
        String value = colon < 0 ? "" : text.substring(colon + 1);
        if (value.startsWith(" ")) {
            value = value.substring(1);
        }
        switch (field) {
            case "event" -> event = value;
            case "data" -> {
                dataBytes += raw.length;
                data.add(value);
            }
            default -> {
                // id / retry / unknown fields: ignored
            }
        }
    }
}
