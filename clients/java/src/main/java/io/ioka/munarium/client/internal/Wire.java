// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.internal;

import io.ioka.munarium.client.errors.InvalidInputException;
import io.ioka.munarium.client.errors.MunariumTransportException;
import java.util.List;
import java.util.concurrent.ThreadLocalRandom;

/**
 * Transport-NEUTRAL wire-contract helpers shared by the REST and gRPC
 * transports — pacing, caps, and client-side vocabulary guards live here so
 * the two transports cannot drift (the same posture as the Python client's
 * {@code _retry.py}/{@code _errors.py} and the Rust client's
 * {@code retry.rs}/{@code planes.rs}). Public only for cross-package
 * access; not part of the supported API surface.
 */
public final class Wire {
    /** The server's per-call file cap on batch ingest and bulk chunks. */
    public static final int MAX_FILES_PER_CHUNK = 500;

    /**
     * The promise statuses the server matches on. It FILTERS an
     * unrecognized value instead of erroring, so an unvalidated typo reads
     * as "no outstanding obligations" — reject it client-side, identically
     * on both transports.
     */
    public static final List<String> PROMISE_STATUSES =
            List.of("open", "fulfilled", "expired", "violated");

    private Wire() {}

    /** Reject an over-cap file list; {@code what} names the calling surface. */
    public static void checkChunkSize(String what, int n) {
        if (n == 0 || n > MAX_FILES_PER_CHUNK) {
            throw new InvalidInputException(
                    what + " must carry 1..=" + MAX_FILES_PER_CHUNK + " files (got " + n + ")");
        }
    }

    /** Reject a promise-status filter the server would silently drop. */
    public static void checkPromiseStatus(String status) {
        if (status != null && !PROMISE_STATUSES.contains(status)) {
            throw new InvalidInputException("unknown promise status '" + status + "' ("
                    + String.join(" | ", PROMISE_STATUSES) + ")");
        }
    }

    /** Jittered backoff: base 2^(n-1)*100ms, decorrelated, clamped [50ms, 2s]. */
    public static void sleepBackoff(int attempt) {
        long baseMs = 100L * (1L << Math.min(attempt - 1, 6));
        long ms = Math.clamp(
                ThreadLocalRandom.current().nextLong(baseMs / 2, baseMs * 3 / 2 + 1), 50, 2000);
        try {
            Thread.sleep(ms);
        } catch (InterruptedException ie) {
            Thread.currentThread().interrupt();
            throw new MunariumTransportException("interrupted during retry backoff", false);
        }
    }
}
