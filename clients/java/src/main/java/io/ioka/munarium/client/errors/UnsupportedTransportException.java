// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

/**
 * The operation has no RPC/route on the transport this client was built
 * with (e.g. the reports plane, streaming turns, and bulk upload sessions
 * are REST-only) — honest and typed, never a silent drop.
 */
public final class UnsupportedTransportException extends MunariumException {
    public UnsupportedTransportException(String detail) {
        super(null, false, detail);
    }
}
