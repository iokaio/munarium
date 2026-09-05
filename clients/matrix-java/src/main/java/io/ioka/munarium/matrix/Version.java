// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

/** {@code GET /version} — what this Matrix is, and whether its server agrees. */
public record Version(
        String version,
        String contractVersion,
        String role,
        String serverVersion,
        String targetServerVersion,
        String serverCompatibility,
        Long uptimeSeconds) {

    /**
     * Matrix and the server it seals into must agree on the contract.
     *
     * <p>{@code exact} is the only state in which an evidence id minted here is
     * certain to resolve there — which is what a citation like
     * {@code [evidence/<id>#r0003]} depends on. Every other value
     * ({@code minor_behind}, {@code unknown}, absent) is a maybe, and a maybe
     * about whether a citation resolves is a no.
     */
    public boolean lockstepOk() {
        return "exact".equals(serverCompatibility);
    }
}
