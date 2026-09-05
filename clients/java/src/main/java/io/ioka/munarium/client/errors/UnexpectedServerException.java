// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

/** An error response that did not match the problem registry. */
public final class UnexpectedServerException extends MunariumException {
    private final Integer status;

    public UnexpectedServerException(String detail, Integer status) {
        // 5xx gateway statuses are transient for the read-retry class.
        super(null, status != null && status >= 502 && status <= 504, detail);
        this.status = status;
    }

    public Integer status() {
        return status;
    }
}
