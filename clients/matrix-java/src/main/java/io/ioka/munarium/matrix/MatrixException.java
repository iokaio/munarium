// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import com.fasterxml.jackson.databind.JsonNode;
import java.io.IOException;

/**
 * A refusal, or a transport failure.
 *
 * <p>Matrix answers a refusal as RFC 9457 problem+json carrying a
 * {@code refusal} object with the CLASS and the CODE — the closed vocabulary
 * the whole system is built on. Both are surfaced as accessors rather than
 * flattened into the message, because a caller that must distinguish "not
 * covered" from "budget exhausted" should not be parsing prose to do it.
 *
 * <p>Unchecked on purpose. Every call on this client can refuse, so a checked
 * exception would put a {@code throws} clause on the whole surface and buy
 * nothing: there is no useful "handle it here" that a caller could be forced
 * into. The typed accessors are what make a refusal actionable.
 */
public final class MatrixException extends RuntimeException {

    private static final long serialVersionUID = 1L;

    private final Integer status;
    private final String code;
    private final String refusalClass;
    private final String detail;

    public MatrixException(String message, Integer status, String refusalClass, String code, String detail) {
        super(message);
        this.status = status;
        this.refusalClass = refusalClass;
        this.code = code;
        this.detail = detail;
    }

    /** HTTP status, or {@code null} when the request never got one. */
    public Integer status() {
        return status;
    }

    /**
     * The open refusal code — {@code budget_exceeded}, {@code not_covered},
     * {@code metric_view_changed}. What an operator reads.
     */
    public String code() {
        return code;
    }

    /**
     * The CLOSED refusal class: {@code not_covered}, {@code unavailable},
     * {@code denied}, {@code incomplete}, {@code invalid}, {@code exhausted}.
     * What a program switches on. {@code null} when the failure carried no
     * refusal at all (a 404 for a missing asset, say).
     */
    public String refusalClass() {
        return refusalClass;
    }

    /** The problem's {@code detail}, when it had one. */
    public String detail() {
        return detail;
    }

    /**
     * Whether retrying the SAME request could plausibly succeed.
     *
     * <p>{@code unavailable} and {@code exhausted} are states of the world;
     * every other class is a statement about the request or the assets, and
     * repeating it changes nothing. A caller that retries a {@code denied} is
     * hammering a door that is locked on purpose.
     */
    public boolean retryable() {
        return "unavailable".equals(refusalClass) || "exhausted".equals(refusalClass);
    }

    /**
     * Decode a non-2xx response.
     *
     * <p>The {@code refusal} member is read only when it is an OBJECT. On the
     * asset-validation path Matrix puts an ARRAY of findings there instead —
     * same key, different shape — and a decoder that assumed the object would
     * blow up on the single most common way to get a 422 out of this service.
     */
    public static MatrixException from(int status, byte[] body) {
        JsonNode root;
        try {
            root = MatrixJson.MAPPER.readTree(body);
        } catch (IOException e) {
            return new MatrixException("matrix answered " + status, status, null, null, null);
        }
        if (root == null || !root.isObject()) {
            return new MatrixException("matrix answered " + status, status, null, null, null);
        }
        JsonNode refusal = root.path("refusal");
        String refusalClass = refusal.isObject() ? refusal.path("class").asText(null) : null;
        String code = refusal.isObject() ? refusal.path("code").asText(null) : null;
        String detail = root.path("detail").asText(null);
        String title = root.path("title").asText(null);
        String message = firstNonBlank(detail, title, "matrix answered " + status);
        return new MatrixException(message, status, refusalClass, code, detail);
    }

    /**
     * A transport failure is {@code unavailable} — the one class that is
     * honestly true of it. Calling it anything else, or letting a raw
     * {@code IOException} escape, would make a caller write a second retry
     * rule for the case a refusal already describes.
     */
    public static MatrixException transportFailure(Exception cause) {
        MatrixException e = new MatrixException(
                String.valueOf(cause), null, "unavailable", null, null);
        e.initCause(cause);
        return e;
    }

    private static String firstNonBlank(String... candidates) {
        for (String c : candidates) {
            if (c != null && !c.isBlank()) {
                return c;
            }
        }
        return "";
    }
}
