// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

/**
 * Where a mapping's latest pass stands against the promotion gates.
 *
 * <p>The thresholds travel WITH the measurements, and that is the point: a
 * bare 0.94 says nothing until you know whether the bar is 0.90 or 0.95, and a
 * client that showed only the measurement would be inviting the reader to
 * remember a number from somewhere else.
 */
public record PromotionGates(
        Double identityPrecision,
        Double valueConformance,
        Double minIdentityPrecision,
        Double minValueConformance,
        Long observations,
        String runId) {}
