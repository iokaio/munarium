// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

/** One reconcile pass as the store recorded it. {@code state} is
 * {@code running} | {@code ok} | {@code refused}. */
public record MappingRun(
        String runId,
        String state,
        long observations,
        long discrepancies,
        long ambiguous,
        long findingsFiled,
        long proposals,
        String endedAt) {}
