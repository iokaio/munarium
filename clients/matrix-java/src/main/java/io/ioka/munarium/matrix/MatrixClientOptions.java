// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import java.time.Duration;

/**
 * Connection options.
 *
 * <p>{@code endpoint} is Matrix's REST base URL ({@code http://host:8180}).
 * {@code token} is the bearer. {@code uid} is the acting operator: Matrix does
 * not require one, but the munarium-server's planes do, and sending it keeps a
 * single identity across BOTH journals when the same person drives both
 * services — which is the only way to read one story out of two audit trails.
 *
 * <p>{@code requestTimeout} defaults to 30 s: long enough for a verify that
 * runs a contract's whole suite against a cold warehouse, short enough that a
 * wedged call is not forever.
 */
public record MatrixClientOptions(
        String endpoint,
        String token,
        String uid,
        Duration connectTimeout,
        Duration requestTimeout) {

    public static MatrixClientOptions of(String endpoint) {
        return new MatrixClientOptions(
                endpoint, null, null, Duration.ofSeconds(5), Duration.ofSeconds(30));
    }

    public MatrixClientOptions withToken(String token) {
        return new MatrixClientOptions(endpoint, token, uid, connectTimeout, requestTimeout);
    }

    public MatrixClientOptions withUid(String uid) {
        return new MatrixClientOptions(endpoint, token, uid, connectTimeout, requestTimeout);
    }

    public MatrixClientOptions withRequestTimeout(Duration d) {
        return new MatrixClientOptions(endpoint, token, uid, connectTimeout, d);
    }
}
