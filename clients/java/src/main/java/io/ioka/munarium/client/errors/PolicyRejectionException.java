// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

import io.ioka.munarium.client.model.Ledger.GateFinding;
import java.util.List;

/**
 * Block-severity gate findings on a non-claim path. NOTE: a gated
 * {@code proposeClaim}/{@code appendEvents} does NOT throw — the claim is
 * recorded {@code disputed} and returned with findings (success, invariant
 * #1). On gRPC the findings list is size-capped by the server to fit the
 * HTTP/2 trailer: {@link #findingsTotal()} is the real count and
 * {@link #findingsTruncated()} marks a capped list.
 */
public final class PolicyRejectionException extends MunariumException {
    private final List<GateFinding> findings;
    private final long findingsTotal;
    private final boolean findingsTruncated;

    public PolicyRejectionException(
            List<GateFinding> findings, long findingsTotal, boolean findingsTruncated, String detail) {
        super("policy-rejection", false,
                messageOr(detail, "policy rejection: " + findings.size() + " finding(s)"));
        this.findings = List.copyOf(findings);
        this.findingsTotal = findingsTotal;
        this.findingsTruncated = findingsTruncated;
    }

    public List<GateFinding> findings() {
        return findings;
    }

    public long findingsTotal() {
        return findingsTotal;
    }

    public boolean findingsTruncated() {
        return findingsTruncated;
    }
}
