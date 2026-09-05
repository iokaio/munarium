// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.Test;

/**
 * The live tier: a round trip against a REAL Matrix when
 * {@code MUNARIUM_MATRIX_TEST_URL} is set.
 *
 * <p>With it unset the test <b>says out loud that it skipped</b>. A skip that
 * prints nothing is indistinguishable from a pass, and that ambiguity is
 * exactly how Matrix's own Postgres conformance tier stayed vacuously green
 * for a whole phase.
 */
class LiveMatrixTest {

    private static final String URL_VAR = "MUNARIUM_MATRIX_TEST_URL";
    private static final String TOKEN_VAR = "MUNARIUM_MATRIX_TEST_TOKEN";

    @Test
    void liveVersionAndRegistryRoundTrip() {
        String url = System.getenv(URL_VAR);
        if (url == null || url.isBlank()) {
            System.out.println(
                    "SKIPPED OUT LOUD: set " + URL_VAR + " (and optionally " + TOKEN_VAR
                            + ") to run the Java Matrix client against a real Matrix.");
            Assumptions.abort("no " + URL_VAR + " — see the message above");
            return;
        }

        System.out.println("LIVE: " + URL_VAR + "=" + url);
        try (var mx = new MatrixClient(
                MatrixClientOptions.of(url).withToken(System.getenv(TOKEN_VAR)))) {
            Version version = mx.version();
            assertNotNull(version.version());
            assertFalse(version.version().isBlank());
            assertFalse(version.contractVersion().isBlank());
            System.out.println(
                    "LIVE: matrix " + version.version() + " role=" + version.role()
                            + " contract=" + version.contractVersion()
                            + " lockstep=" + version.lockstepOk());

            // The registry answers, and a listing is a list even when empty.
            assertNotNull(mx.listAssets("datasources"));
        }
    }
}
