// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import java.util.List;

/**
 * Registration, NOT connectivity.
 *
 * <p>Every row here reports {@code reachable: false} with a detail saying so:
 * probing every source on a health call would make a health endpoint an
 * outbound-traffic amplifier. {@link MatrixClient#probe(String)} is the
 * deliberate per-source check.
 */
public record HealthData(boolean healthy, List<Probe> sources) {
    public HealthData {
        sources = sources == null ? List.of() : List.copyOf(sources);
    }
}
