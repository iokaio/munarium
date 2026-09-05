// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

public final class InvalidInputException extends MunariumException {
    public InvalidInputException(String detail) {
        super("invalid-input", false, detail);
    }
}
