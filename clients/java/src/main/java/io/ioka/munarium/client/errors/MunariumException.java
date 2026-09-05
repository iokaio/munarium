// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

/**
 * Root of every error this client raises — one typed surface keyed on the
 * problem-slug registry ({@code server/docs/api/errors.md}). REST decodes
 * {@code application/problem+json}; gRPC decodes the
 * {@code google.rpc.ErrorInfo} detail in {@code grpc-status-details-bin}.
 * No English message text is ever parsed.
 */
public class MunariumException extends RuntimeException {
    private final String slug;
    private final boolean transientFailure;

    protected MunariumException(String slug, boolean transientFailure, String message) {
        super(message);
        this.slug = slug;
        this.transientFailure = transientFailure;
    }

    /** Detail when present, else the computed fallback (one defaulting rule
     * for every subclass — blank counts as absent). */
    protected static String messageOr(String detail, String fallback) {
        return detail == null || detail.isBlank() ? fallback : detail;
    }

    /** Registry slug for this error, or {@code null} when it maps to none. */
    public String slug() {
        return slug;
    }

    /**
     * Retrying the SAME request is safe and may succeed (reads auto-retry
     * these). Head conflicts are retryable too but need a REBUILT request —
     * see the write-loop helper.
     */
    public boolean isTransient() {
        return transientFailure;
    }
}
