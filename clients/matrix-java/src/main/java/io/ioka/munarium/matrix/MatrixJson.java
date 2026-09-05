// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.databind.DeserializationFeature;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.PropertyNamingStrategies;

/**
 * The one shared Jackson mapper. Matrix's wire casing is snake_case, mapped
 * from camelCase record components by the naming strategy; unknown fields are
 * ignored, because Matrix adds response fields inside a contract MAJOR and a
 * client that threw on one would turn an additive change into an outage.
 */
public final class MatrixJson {
    public static final ObjectMapper MAPPER = new ObjectMapper()
            .setPropertyNamingStrategy(PropertyNamingStrategies.SNAKE_CASE)
            .configure(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES, false)
            .setSerializationInclusion(JsonInclude.Include.NON_NULL);

    private MatrixJson() {}
}
