// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import java.util.List;

/** Capability-token management (mgmt role). */
public final class Tokens {
    private Tokens() {}

    /** Mint request. {@code ttlSecs} null = server default (24 h ceiling). */
    public record IssueTokenRequest(
            String uid,
            int accessLevel,
            List<String> compartments,
            List<String> scopes,
            List<String> runbookRefs,
            Long ttlSecs) {

        public static IssueTokenRequest of(String uid, int accessLevel, List<String> scopes) {
            return new IssueTokenRequest(uid, accessLevel, List.of(), scopes, null, null);
        }

        public IssueTokenRequest withCompartments(List<String> c) {
            return new IssueTokenRequest(uid, accessLevel, List.copyOf(c), scopes, runbookRefs, ttlSecs);
        }
    }

    /** The signed JWT — returned ONCE, never persisted server-side. */
    public record TokenGrant(String token, String jti, String expiresAt) {}

    /** One issued token in the audit view (never the token material). */
    public record TokenInfo(
            String jti,
            String uid,
            int accessLevel,
            List<String> compartments,
            List<String> scopes,
            List<String> runbookRefs,
            String issuedBy,
            String issuedAt,
            String expiresAt,
            String revokedAt) {}

    public record TokenPage(List<TokenInfo> tokens) {}

    public record RevokeResult(String jti, boolean revoked, boolean revocationCheckEnabled) {}
}
