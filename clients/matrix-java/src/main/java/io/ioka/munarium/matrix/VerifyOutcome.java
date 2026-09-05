// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import java.util.List;

/**
 * A contract's or a view's verified-question suite, run.
 *
 * <p>The CALL succeeding and the CONTRACT passing are different things: check
 * {@link #failed()}. {@code mxctl} exits 3 on a non-zero {@code failed} for
 * exactly this reason, so CI can tell a broken contract from a broken command.
 *
 * <p>{@code fingerprint} is populated for semantic views only: the definition
 * the questions ran under, which a later execute is held to.
 */
public record VerifyOutcome(
        String contract,
        int passed,
        int failed,
        String fingerprint,
        List<VerifiedQuestion> questions) {
    public VerifyOutcome {
        questions = questions == null ? List.of() : List.copyOf(questions);
    }
}
