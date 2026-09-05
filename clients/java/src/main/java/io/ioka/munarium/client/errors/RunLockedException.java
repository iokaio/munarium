// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

/**
 * Another run holds this runbook's run lock (409 / gRPC ABORTED with reason
 * {@code run-locked}). The server rejected the request BEFORE executing
 * anything, and the lock clears when the holding run finishes — retryable in
 * YOUR OWN pacing, like {@link RateLimitedException}, and for the same
 * reason deliberately NOT transient: a run lock is held for a whole run
 * (minutes), so sub-second auto-retry would be futile churn that masks the
 * typed signal.
 */
public final class RunLockedException extends MunariumException {
    public RunLockedException(String detail) {
        super("run-locked", false, detail);
    }
}
