// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

public final class ShapeViolationException extends MunariumException {
    private final String shapeRef;

    public ShapeViolationException(String shapeRef, String detail) {
        super("shape-violation", false, detail);
        this.shapeRef = shapeRef;
    }

    public String shapeRef() {
        return shapeRef;
    }
}
