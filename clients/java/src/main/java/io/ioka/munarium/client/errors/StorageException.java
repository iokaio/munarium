// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

public final class StorageException extends MunariumException {
    public StorageException(String detail) {
        super("storage-error", false, detail);
    }
}
