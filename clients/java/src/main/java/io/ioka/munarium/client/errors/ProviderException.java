// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

public final class ProviderException extends MunariumException {
    public ProviderException(String detail) {
        super("provider-error", false, detail);
    }
}
