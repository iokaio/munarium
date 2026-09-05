// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import java.util.List;

/**
 * The promotion gates over time, newest first.
 *
 * <p>{@code passing} is how many of {@code runs} clear the current thresholds.
 * A ratio far from 0 or 1 is the signal that a threshold is doing real work;
 * 1.0 over a long series means it is not binding, and 0.0 means it is a wall.
 */
public record GateHistory(
        String mapping,
        double minIdentityPrecision,
        double minValueConformance,
        List<GateHistoryEntry> runs,
        int passing) {
    public GateHistory {
        runs = runs == null ? List.of() : List.copyOf(runs);
    }
}
