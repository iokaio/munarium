// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.rest;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.ioka.munarium.client.MunariumClientOptions;
import io.ioka.munarium.client.model.Json;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.http.HttpRequest.BodyPublisher;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Flow;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.Test;

class RestStreamingTest {
    @Test
    void streamingJsonBodyIsEquivalentAndDoesNotPublishOneGiantBuffer() throws Exception {
        String large = "a".repeat(200_000);
        Map<String, Object> value = Map.of(
                "files", List.of(Map.of(
                        "filename", "quotes-\"-and-unicode-ø.md",
                        "content_base64", large)));
        BodyPublisher publisher = RestTransport.streamingJsonBody(value);
        var bytes = new ByteArrayOutputStream();
        var maximumChunk = new AtomicInteger();
        var failure = new AtomicReference<Throwable>();
        var finished = new CountDownLatch(1);

        publisher.subscribe(new Flow.Subscriber<>() {
            @Override
            public void onSubscribe(Flow.Subscription subscription) {
                subscription.request(Long.MAX_VALUE);
            }

            @Override
            public void onNext(ByteBuffer item) {
                maximumChunk.accumulateAndGet(item.remaining(), Math::max);
                byte[] chunk = new byte[item.remaining()];
                item.get(chunk);
                bytes.writeBytes(chunk);
            }

            @Override
            public void onError(Throwable error) {
                failure.set(error);
                finished.countDown();
            }

            @Override
            public void onComplete() {
                finished.countDown();
            }
        });

        assertTrue(finished.await(5, TimeUnit.SECONDS), "publisher did not finish");
        assertNull(failure.get());
        assertEquals(Json.MAPPER.valueToTree(value), Json.MAPPER.readTree(bytes.toByteArray()));
        assertEquals(-1, publisher.contentLength(), "streamed bodies have unknown length");
        assertTrue(maximumChunk.get() < bytes.size(), "must not publish one request-sized buffer");
    }

    @Test
    void slowProgressConsumerDoesNotTripTheWireIdleWatchdog() {
        byte[] progress = ("event: progress\n"
                + "data: {\"stage\":\"retrieval\",\"hits\":1}\n\n")
                .getBytes(StandardCharsets.UTF_8);
        byte[] done = ("event: done\n"
                + "data: {\"session_id\":\"s-1\",\"ordinal\":1,"
                + "\"collections_searched\":[],\"skipped\":[],\"hits\":[],"
                + "\"envelopes\":[],\"completion\":null}\n\n")
                .getBytes(StandardCharsets.UTF_8);
        var source = new StepInputStream(progress, done);
        var callbackRan = new AtomicBoolean(false);

        try (var transport = new RestTransport(
                MunariumClientOptions.of("http://127.0.0.1:1").withToken("t").withUid("u"))) {
            var result = transport.readTurnStream(source, event -> {
                callbackRan.set(true);
                try {
                    Thread.sleep(100); // five times the test watchdog budget
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                    throw new AssertionError(e);
                }
            }, Duration.ofMillis(20));
            assertEquals("s-1", result.sessionId());
        }

        assertTrue(callbackRan.get());
        assertFalse(source.closedDuringCallback.get(),
                "application callback time must not count as wire idle time");
    }

    /** One SSE frame per read, and close records whether it happened before
     * the second read (which is exactly how the old watchdog failed). */
    private static final class StepInputStream extends InputStream {
        private final byte[][] steps;
        private int index;
        private boolean closed;
        private final AtomicBoolean closedDuringCallback = new AtomicBoolean(false);

        StepInputStream(byte[]... steps) {
            this.steps = steps;
        }

        @Override
        public int read() throws IOException {
            byte[] one = new byte[1];
            int n = read(one, 0, 1);
            return n < 0 ? -1 : Byte.toUnsignedInt(one[0]);
        }

        @Override
        public int read(byte[] target, int offset, int length) throws IOException {
            if (closed) {
                throw new IOException("closed by watchdog");
            }
            if (index >= steps.length) {
                return -1;
            }
            byte[] step = steps[index++];
            int count = Math.min(length, step.length);
            System.arraycopy(step, 0, target, offset, count);
            return count;
        }

        @Override
        public void close() {
            if (index == 1) {
                closedDuringCallback.set(true);
            }
            closed = true;
        }
    }
}
