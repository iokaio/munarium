// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import java.util.List;

/**
 * The result of applying one asset.
 *
 * <p>{@code unchanged} is a byte-identical re-apply: ordinary GitOps, not an
 * error. The same version with DIFFERENT bytes is refused, because a version
 * is provenance — sealed evidence cites {@code name@version}, and letting one
 * mean two things would make every citation to it ambiguous.
 *
 * <p>{@code findings} carries the advisory findings a successful apply can
 * still return. They are worth surfacing precisely because they did not stop
 * the apply: nothing else will mention them again.
 */
public record ApplyOutcome(
        String assetRef, String kind, boolean unchanged, List<ValidationFinding> findings) {
    public ApplyOutcome {
        findings = findings == null ? List.of() : List.copyOf(findings);
    }
}
