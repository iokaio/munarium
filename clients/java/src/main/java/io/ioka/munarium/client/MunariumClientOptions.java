// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client;

import java.time.Duration;

/**
 * Connection + behavior options, shared by both transports.
 *
 * <p>{@code endpoint}: REST base URL ({@code http://host:8080}) or gRPC
 * target ({@code host:50051}, plaintext exactly when the scheme is
 * {@code http://} or absent-with-loopback intent — pass {@code https://}
 * for TLS). {@code token}: bearer — a static token or a capability JWT;
 * {@code null} only works against {@code MUNARIUM_AUTH_MODE=disabled}.
 * {@code uid}: the acting end-user id (uid contract), sent as
 * {@code X-Munarium-Uid} (REST) / {@code munarium-uid} metadata (gRPC) on every
 * request — required by servers at the secure default
 * ({@code MUNARIUM_REQUIRE_UID=true}); with a capability JWT it must equal
 * the token's {@code sub}.
 *
 * <p>{@code readRetries}: extra attempts for READS on transient failures.
 * Commands re-send the SAME idempotency key, and only when the request
 * provably never reached the server (a connect-phase failure) or the server
 * shed it before executing — the server records an idempotency key AFTER a
 * command completes, so re-sending a possibly-delivered command could
 * execute it twice. On gRPC no transport failure is provably undelivered,
 * so only the typed pre-execution shed re-sends there.
 */
public record MunariumClientOptions(
        String endpoint,
        String token,
        String uid,
        Duration connectTimeout,
        Duration requestTimeout,
        int readRetries) {

    public static MunariumClientOptions of(String endpoint) {
        return new MunariumClientOptions(
                endpoint, null, null, Duration.ofSeconds(5), Duration.ofSeconds(30), 2);
    }

    public MunariumClientOptions withToken(String token) {
        return new MunariumClientOptions(endpoint, token, uid, connectTimeout, requestTimeout, readRetries);
    }

    /** Set the acting end-user id (the uid contract). */
    public MunariumClientOptions withUid(String uid) {
        return new MunariumClientOptions(endpoint, token, uid, connectTimeout, requestTimeout, readRetries);
    }

    public MunariumClientOptions withRequestTimeout(Duration d) {
        return new MunariumClientOptions(endpoint, token, uid, connectTimeout, d, readRetries);
    }

    public MunariumClientOptions withReadRetries(int n) {
        return new MunariumClientOptions(endpoint, token, uid, connectTimeout, requestTimeout, n);
    }
}
