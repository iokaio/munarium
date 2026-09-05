// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

/** Meta routes outside the ten-plane surface. */
public final class Meta {
    private Meta() {}

    /**
     * {@code GET /version} — the served name + workspace version,
     * unauthenticated. Compare against
     * {@link io.ioka.munarium.client.Munarium#TARGET_SERVER_VERSION} to catch a
     * stale deploy early.
     */
    public record ServerVersion(String name, String version) {}
}
