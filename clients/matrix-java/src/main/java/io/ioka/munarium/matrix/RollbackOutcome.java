// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

/**
 * What a rollback did. Every count is a claim about the LEDGER, and the ledger
 * is append-only: {@code superseded} claims were superseded by a correction
 * carrying the value that stood before, never deleted.
 *
 * <p>{@code skippedNoPrior} is the honest case a rollback cannot fix — the
 * mapping's claim had no predecessor, so there is nothing to restore to.
 * {@code disputed} is counted rather than dropped.
 */
public record RollbackOutcome(
        String mapping,
        String decisionId,
        long superseded,
        long skippedNoPrior,
        long alreadyRolledBack,
        long disputed) {}
