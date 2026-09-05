// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

/**
 * One completed run's gate values, with the verdict the CURRENT thresholds
 * would give it.
 *
 * <p>{@code wouldPass} is computed against the thresholds in force now, not
 * the ones in force when the run happened. Lowering a threshold and re-reading
 * this series shows exactly which past runs the change would have admitted,
 * before anything is promoted under the new number.
 *
 * <p>The margins are SIGNED distances. A small positive number is a near-miss
 * worth knowing about, and it is invisible in a pass/fail column.
 */
public record GateHistoryEntry(
        String runId,
        String state,
        String endedAt,
        long observations,
        long ambiguous,
        long nonconforming,
        double identityPrecision,
        double valueConformance,
        double identityMargin,
        double valueMargin,
        boolean wouldPass) {}
