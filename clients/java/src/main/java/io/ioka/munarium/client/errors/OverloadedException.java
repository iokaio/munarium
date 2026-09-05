// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

/**
 * Load-shed / graceful drain (503 / {@code overloaded}) — transient, retried
 * automatically on read paths, and the ONE outcome that is command-retry
 * safe (the server shed the request before executing it).
 */
public final class OverloadedException extends MunariumException {
    public OverloadedException(String detail) {
        super("overloaded", true, detail);
    }
}
