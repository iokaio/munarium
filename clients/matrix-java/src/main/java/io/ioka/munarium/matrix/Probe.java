// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

/**
 * Reachability at probe time.
 *
 * <p>A refusal is an ANSWER here, not an exception: an unreachable source
 * comes back {@code reachable: false} with a typed reason, because "I asked
 * and it is down" is a successful probe. {@code breaker} is the circuit
 * breaker's state — {@code closed} | {@code open} | {@code half_open} |
 * {@code unknown}.
 */
public record Probe(
        String source, boolean reachable, Long latencyMs, String breaker, String detail) {}
