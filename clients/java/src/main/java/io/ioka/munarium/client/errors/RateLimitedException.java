// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

import java.time.Duration;
import java.util.Optional;

/**
 * Per-tenant limits or provider budget exhausted. Deliberately NOT
 * auto-retried — honor {@link #retryAfter()} in your own pacing (the server
 * does not emit Retry-After today; the hint is read opportunistically).
 */
public final class RateLimitedException extends MunariumException {
    private final Duration retryAfter;

    public RateLimitedException(String detail, Duration retryAfter) {
        super("rate-limited", false, detail);
        this.retryAfter = retryAfter;
    }

    public Optional<Duration> retryAfter() {
        return Optional.ofNullable(retryAfter);
    }
}
