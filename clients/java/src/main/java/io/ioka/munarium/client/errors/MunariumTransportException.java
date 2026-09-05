// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

/**
 * Connection-level failure (DNS, refused, reset, timeout). Transient —
 * reads auto-retry it. {@link #mayHaveReachedServer()} is the command
 * replay-safety pivot: the server records an idempotency key only AFTER a
 * command finishes, so a command that fails with {@code true} here is NOT
 * auto-retried (a retry could overtake an in-flight attempt and execute it
 * twice); {@code false} means the request provably never left.
 */
public final class MunariumTransportException extends MunariumException {
    private final boolean mayHaveReachedServer;

    public MunariumTransportException(String detail, boolean mayHaveReachedServer) {
        super(null, true, detail);
        this.mayHaveReachedServer = mayHaveReachedServer;
    }

    public boolean mayHaveReachedServer() {
        return mayHaveReachedServer;
    }
}
