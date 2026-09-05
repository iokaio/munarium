// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import java.util.List;

/**
 * The verdict from {@code POST /v1/assets/validate}.
 *
 * <p>{@code valid} is Matrix's own answer and is NOT "the findings list is
 * empty": a handful of codes are advisory
 * ({@code limits.above-inline-seal}, {@code mapping.authority-inert},
 * {@code authorization.classes-ignored}) and an asset carrying only those is
 * valid. Deriving the verdict from the list length here would give a different
 * answer than the service that enforces it — which is the drift this client
 * refuses to introduce by declining to hold its own copy of the rules.
 */
public record Validation(boolean valid, List<ValidationFinding> findings) {
    public Validation {
        findings = findings == null ? List.of() : List.copyOf(findings);
    }
}
