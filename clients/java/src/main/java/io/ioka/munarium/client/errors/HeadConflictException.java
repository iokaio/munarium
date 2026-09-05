// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

/**
 * Optimistic {@code expected_head} mismatch — normal and retryable: re-read
 * head, re-decide, retry (or use the built-in write loop). {@code actual == 0}
 * means the transport carried no structured seqs (e.g. an intermediary
 * stripped them) — re-read the head yourself.
 */
public final class HeadConflictException extends MunariumException {
    private final long expected;
    private final long actual;

    public HeadConflictException(long expected, long actual, String detail) {
        super("head-conflict", false,
                messageOr(detail, "head conflict: expected seq " + expected + ", actual " + actual));
        this.expected = expected;
        this.actual = actual;
    }

    public long expected() {
        return expected;
    }

    public long actual() {
        return actual;
    }
}
