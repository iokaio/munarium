// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

/**
 * Where a mapping stands, whether or not it is promoted.
 *
 * <p>The gate measurements live in {@link PromotionGates} because that is
 * where Matrix puts them — nested under {@code gates}, not at the top level.
 * Reading them from the top level yields {@code null} forever and looks like
 * "this mapping has never been measured", which is a different and much
 * calmer-sounding thing than "the client is looking in the wrong place".
 *
 * <p>{@code latestRun} is the most recent pass in whatever state, including
 * {@code running} and {@code refused}. Without it an operator who has just
 * queued a pass cannot learn that it refused except by reading the journal.
 */
public record PromotionStatus(
        String mapping,
        String mode,
        boolean promoted,
        Integer promotedVersion,
        String decisionId,
        String promotedAt,
        PromotionGates gates,
        int authorityScopes,
        MappingRun latestRun) {

    /** Convenience: the measurement, or {@code null} when nothing has run. */
    public Double identityPrecision() {
        return gates == null ? null : gates.identityPrecision();
    }

    /** Convenience: the measurement, or {@code null} when nothing has run. */
    public Double valueConformance() {
        return gates == null ? null : gates.valueConformance();
    }
}
