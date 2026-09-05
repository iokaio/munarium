// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

/** The same idempotency key was replayed with a DIFFERENT request body. */
public final class IdempotencyMismatchException extends MunariumException {
    public IdempotencyMismatchException(String detail) {
        super("idempotency-mismatch", false, detail);
    }
}
