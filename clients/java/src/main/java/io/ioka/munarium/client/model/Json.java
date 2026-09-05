// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.databind.DeserializationFeature;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.PropertyNamingStrategies;

/**
 * The one shared Jackson mapper. Wire casing is the server's
 * {@code munarium-api-types} snake_case, mapped from camelCase record
 * components by the naming strategy (exceptions pin themselves with
 * {@code @JsonProperty}); unknown fields are ignored so the client stays
 * forward-compatible with additive server fields; {@code null} members are
 * omitted on serialization (matching the DTOs' skip-if-none posture).
 */
public final class Json {
    public static final ObjectMapper MAPPER = new ObjectMapper()
            .setPropertyNamingStrategy(PropertyNamingStrategies.SNAKE_CASE)
            .configure(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES, false)
            .setSerializationInclusion(JsonInclude.Include.NON_NULL);

    private Json() {}
}
