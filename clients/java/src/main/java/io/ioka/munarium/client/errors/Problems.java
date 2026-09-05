// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

import com.fasterxml.jackson.databind.JsonNode;
import io.ioka.munarium.client.model.Json;
import io.ioka.munarium.client.model.Ledger.GateFinding;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * The ONE error-construction path, fed by both transports: REST hands it a
 * decoded problem+json body, gRPC hands it the {@code google.rpc.ErrorInfo}
 * reason + metadata (the member NAMES are identical on both transports —
 * {@code server/docs/api/errors.md}).
 */
public final class Problems {
    private Problems() {}

    /** Decode a REST problem+json body into the typed exception. */
    public static MunariumException fromProblemJson(int status, JsonNode body, Duration retryAfter) {
        if (body == null || !body.isObject()) {
            return new UnexpectedServerException("non-problem error body (HTTP " + status + ")", status);
        }
        String type = body.path("type").asText("");
        String slug = type.substring(type.lastIndexOf('/') + 1);
        String detail = body.path("detail").asText("");
        return fromParts(slug, detail, new JsonExt(body), status, retryAfter);
    }

    /** Decode gRPC ErrorInfo metadata (same member names, string values). */
    public static MunariumException fromGrpcInfo(
            String reason, String detail, Map<String, String> metadata) {
        return fromParts(reason, detail, new MapExt(metadata), null, null);
    }

    private static MunariumException fromParts(
            String slug, String detail, Ext ext, Integer status, Duration retryAfter) {
        return switch (slug) {
            case "head-conflict" ->
                    new HeadConflictException(ext.asLong("expected"), ext.asLong("actual"), detail);
            case "policy-rejection" -> {
                List<GateFinding> findings = ext.findings();
                long total = ext.has("findings_total") ? ext.asLong("findings_total") : findings.size();
                yield new PolicyRejectionException(findings, total, ext.asBool("findings_truncated"), detail);
            }
            case "shape-violation" -> new ShapeViolationException(ext.asText("shape_ref"), detail);
            case "idempotency-mismatch" -> new IdempotencyMismatchException(detail);
            case "not-found" -> new NotFoundException(
                    ext.has("kind") ? ext.asText("kind") : "resource", ext.asText("id"), detail);
            case "invalid-input" -> new InvalidInputException(detail);
            case "unauthenticated" -> new UnauthenticatedException(detail);
            case "forbidden" -> new ForbiddenException(detail);
            case "rate-limited" -> new RateLimitedException(detail, retryAfter);
            case "overloaded" -> new OverloadedException(detail);
            case "storage-error" -> new StorageException(detail);
            case "provider-error" -> new ProviderException(detail);
            // platform identity/lifecycle slugs — mapped to existing kinds by
            // status class so re-auth/permission branching keeps working
            // (token lifecycle -> unauthenticated so a caller can refresh;
            // runbook-removed is a 410 surfaced as not-found;
            // session-not-open / authoring-draft-invalid follow the
            // removal-not-confirmed 409 precedent).
            case "uid-required", "removal-not-confirmed", "session-not-open",
                    "authoring-draft-invalid" -> new InvalidInputException(detail);
            case "token-expired", "token-revoked" -> new UnauthenticatedException(detail);
            case "uid-mismatch", "scope-missing", "override-not-allowed" ->
                    new ForbiddenException(detail);
            case "runbook-removed" -> new NotFoundException(
                    ext.has("kind") ? ext.asText("kind") : "runbook", ext.asText("id"), detail);
            // Its RETRYABILITY is semantic (rejected pre-execution; the lock
            // clears when the holding run finishes) — its own typed kind,
            // deliberately NOT transient.
            case "run-locked" -> new RunLockedException(detail);
            default -> new UnexpectedServerException(detail, status);
        };
    }

    private sealed interface Ext permits JsonExt, MapExt {
        boolean has(String key);

        long asLong(String key);

        boolean asBool(String key);

        String asText(String key);

        List<GateFinding> findings();
    }

    private record JsonExt(JsonNode body) implements Ext {
        @Override
        public boolean has(String key) {
            return body.hasNonNull(key);
        }

        @Override
        public long asLong(String key) {
            return body.path(key).asLong(0);
        }

        @Override
        public boolean asBool(String key) {
            return body.path(key).asBoolean(false);
        }

        @Override
        public String asText(String key) {
            return body.path(key).asText("");
        }

        @Override
        public List<GateFinding> findings() {
            return parseFindings(body.path("gate_findings"));
        }
    }

    private record MapExt(Map<String, String> md) implements Ext {
        @Override
        public boolean has(String key) {
            return md.containsKey(key);
        }

        @Override
        public long asLong(String key) {
            try {
                return Long.parseLong(md.getOrDefault(key, "0"));
            } catch (NumberFormatException e) {
                return 0;
            }
        }

        @Override
        public boolean asBool(String key) {
            return "true".equals(md.get(key));
        }

        @Override
        public String asText(String key) {
            return md.getOrDefault(key, "");
        }

        @Override
        public List<GateFinding> findings() {
            String raw = md.get("gate_findings");
            if (raw == null) {
                return List.of();
            }
            try {
                return parseFindings(Json.MAPPER.readTree(raw));
            } catch (Exception e) { // a bad finding never masks the error
                return List.of();
            }
        }
    }

    private static List<GateFinding> parseFindings(JsonNode raw) {
        if (raw == null || !raw.isArray()) {
            return List.of();
        }
        List<GateFinding> out = new ArrayList<>();
        for (JsonNode item : raw) {
            try {
                out.add(Json.MAPPER.treeToValue(item, GateFinding.class));
            } catch (Exception e) { // a bad finding never masks the error
                continue;
            }
        }
        return out;
    }
}
