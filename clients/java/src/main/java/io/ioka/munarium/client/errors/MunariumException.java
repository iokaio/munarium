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
    // ClearView R11 retry guidance. Set once by Problems before the exception
    // is thrown; not constructor arguments, so every subclass constructor
    // keeps its published signature.
    private Boolean retryable;
    private String retryAction;

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

    /**
     * ClearView R11: the server's {@code retryable} member (REST problem+json,
     * or the gRPC {@code ErrorInfo} metadata key of the same name). {@code true}
     * only when the server has evidence the request was rejected before
     * execution; {@code null} when the server sent no guidance (every v1.4
     * server, and every slug outside the guidance table).
     *
     * <p>Informational: it does not change this client's automatic retry
     * behavior, and session turns stay send-once regardless.
     */
    public Boolean retryable() {
        return retryable;
    }

    /**
     * ClearView R11: {@code repeat}, {@code retry_later}, {@code reread} or
     * {@code inspect_outcome}; {@code null} when absent. An absent or
     * unrecognized action authorizes no automated replay.
     */
    public String retryAction() {
        return retryAction;
    }

    /** Attach the decoded R11 members; called only by {@link Problems}. */
    final MunariumException withRetryGuidance(Boolean retryable, String retryAction) {
        this.retryable = retryable;
        this.retryAction = retryAction;
        return this;
    }
}
