// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

public final class NotFoundException extends MunariumException {
    private final String kind;
    private final String id;

    public NotFoundException(String kind, String id, String detail) {
        super("not-found", false, messageOr(detail, "not found: " + kind + " " + id));
        this.kind = kind;
        this.id = id;
    }

    public String kind() {
        return kind;
    }

    public String id() {
        return id;
    }
}
