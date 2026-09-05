// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.conformance;

import io.ioka.munarium.client.MunariumClient;
import io.ioka.munarium.client.MunariumClientOptions;
import org.junit.jupiter.api.Assumptions;

/**
 * Live-server configuration for the conformance suite. Tests skip cleanly
 * (JUnit assumptions) when the env vars are unset, so `gradlew test` stays
 * offline and `gradlew conformanceTest` is the explicit live gate:
 *
 * <pre>
 * # dev box (port 8080 is owned by another project; CI uses 8080/50051):
 * MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_GRPC_URL=127.0.0.1:15051 \
 * MUNARIUM_TOKEN=devtoken MUNARIUM_MGMT_TOKEN=devmgmt ./gradlew conformanceTest
 * </pre>
 */
final class Env {
    static final String REST_URL = System.getenv("MUNARIUM_REST_URL");
    static final String GRPC_URL = System.getenv("MUNARIUM_GRPC_URL");
    static final String TOKEN = System.getenv("MUNARIUM_TOKEN");
    static final String MGMT_TOKEN = System.getenv("MUNARIUM_MGMT_TOKEN");

    private Env() {}

    static MunariumClient rest(String token, String uid) {
        Assumptions.assumeTrue(REST_URL != null, "MUNARIUM_REST_URL unset — live suite skipped");
        return MunariumClient.rest(MunariumClientOptions.of(REST_URL).withToken(token).withUid(uid));
    }

    static MunariumClient grpc(String token, String uid) {
        Assumptions.assumeTrue(GRPC_URL != null, "MUNARIUM_GRPC_URL unset — live suite skipped");
        return MunariumClient.grpc(MunariumClientOptions.of(GRPC_URL).withToken(token).withUid(uid));
    }

    static MunariumClient forTransport(String transport) {
        String token = TOKEN != null ? TOKEN : "devtoken";
        return "grpc".equals(transport) ? grpc(token, "conformance") : rest(token, "conformance");
    }

    static String requireMgmt() {
        Assumptions.assumeTrue(
                MGMT_TOKEN != null, "MUNARIUM_MGMT_TOKEN unset — platform smokes skipped");
        return MGMT_TOKEN;
    }

    /** Unique-per-run value (nanos hex) for content that must be fresh. */
    static String nonce() {
        return Long.toHexString(System.nanoTime());
    }
}
