// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import java.util.List;

/**
 * One verified question's outcome.
 *
 * <p>{@code logicalResultHash} is the canon@1 identity of the result — the
 * hash over the canonical encoding, not over rendered text. Two runs that
 * agree on it returned the same answer whatever the formatting did.
 */
public record VerifiedQuestion(
        String question, boolean ok, Integer rows, String logicalResultHash, List<String> failures) {
    public VerifiedQuestion {
        failures = failures == null ? List.of() : List.copyOf(failures);
    }
}
