// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.rest;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;

/** Port of the Rust client's sse.rs tests — same grammar, same guarantees. */
class SseParserTest {
    private static List<SseParser.Event> push(SseParser p, String wire) {
        byte[] bytes = wire.getBytes(StandardCharsets.UTF_8);
        return p.push(bytes, bytes.length);
    }

    @Test
    void parsesNamedEventsAndKeepalives() {
        var p = new SseParser();
        var evs = push(p, ": keep-alive\n\nevent: progress\ndata: {\"stage\":\"merge\"}\n\n");
        assertEquals(1, evs.size());
        assertEquals("progress", evs.get(0).event());
        assertEquals("{\"stage\":\"merge\"}", evs.get(0).data());
    }

    @Test
    void survivesArbitraryChunkBoundaries() {
        // The transport may split anywhere — byte-by-byte is the adversarial
        // version of that, CRLF spans included.
        byte[] wire = "event: progress\r\ndata: {\"n\":1}\r\n\r\nevent: done\ndata: {}\n\n"
                .getBytes(StandardCharsets.UTF_8);
        var p = new SseParser();
        var evs = new ArrayList<SseParser.Event>();
        byte[] one = new byte[1];
        for (byte b : wire) {
            one[0] = b;
            evs.addAll(p.push(one, 1));
        }
        assertEquals(2, evs.size());
        assertEquals("progress", evs.get(0).event());
        assertEquals("{\"n\":1}", evs.get(0).data());
        assertEquals("done", evs.get(1).event());
    }

    @Test
    void multiLineDataJoinsWithNewlines() {
        var p = new SseParser();
        var evs = push(p, "data: a\ndata: b\n\n");
        assertEquals("a\nb", evs.get(0).data());
        assertEquals("", evs.get(0).event(), "default event name is empty");
    }

    @Test
    void eventWithoutDataIsDroppedNotDispatched() {
        var p = new SseParser();
        assertTrue(push(p, "event: progress\n\n").isEmpty());
        // ...and the stale name does not leak into the next event.
        var evs = push(p, "data: x\n\n");
        assertEquals("", evs.get(0).event());
    }

    @Test
    void overflowNeverDropsEventsCompletedInTheSameChunk() {
        // A terminal done followed by oversized trailing bytes in ONE chunk:
        // the done is delivered; the overflow surfaces on the NEXT push.
        var p = new SseParser();
        byte[] head = "event: done\ndata: {}\n\n".getBytes(StandardCharsets.UTF_8);
        byte[] chunk = new byte[head.length + SseParser.MAX_EVENT_BYTES + 1];
        System.arraycopy(head, 0, chunk, 0, head.length);
        java.util.Arrays.fill(chunk, head.length, chunk.length, (byte) 'x');
        var evs = p.push(chunk, chunk.length);
        assertEquals(1, evs.size());
        assertEquals("done", evs.get(0).event());
        assertThrows(SseParser.Overflow.class, () -> p.push(new byte[] {'m'}, 1));
    }

    @Test
    void unknownFieldsAndNoColonLinesAreIgnored() {
        var p = new SseParser();
        var evs = push(p, "id: 7\nretry: 100\nnonsense\ndata: ok\n\n");
        assertEquals(1, evs.size());
        assertEquals("ok", evs.get(0).data());
    }

    @Test
    void aNeverendingEventOverflowsInsteadOfGrowingForever() {
        // No newline at all: the pending line buffer hits the cap.
        var p = new SseParser();
        byte[] chunk = new byte[1024 * 1024];
        java.util.Arrays.fill(chunk, (byte) 'x');
        assertThrows(SseParser.Overflow.class, () -> {
            for (int i = 0; i < 20; i++) {
                p.push(chunk, chunk.length);
            }
        });

        // Data lines that never dispatch (no blank line) also count.
        var p2 = new SseParser();
        StringBuilder line = new StringBuilder("data: ");
        line.append("y".repeat(1024 * 1024)).append('\n');
        byte[] bytes = line.toString().getBytes(StandardCharsets.UTF_8);
        assertThrows(SseParser.Overflow.class, () -> {
            for (int i = 0; i < 20; i++) {
                p2.push(bytes, bytes.length);
            }
        });
    }
}
