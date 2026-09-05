// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

public final class UnauthenticatedException extends MunariumException {
    public UnauthenticatedException(String detail) {
        super("unauthenticated", false, detail);
    }
}
