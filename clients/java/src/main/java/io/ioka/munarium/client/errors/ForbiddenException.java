// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

public final class ForbiddenException extends MunariumException {
    public ForbiddenException(String detail) {
        super("forbidden", false, detail);
    }
}
