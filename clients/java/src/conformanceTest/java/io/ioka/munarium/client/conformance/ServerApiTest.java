// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.conformance;

import com.fasterxml.jackson.databind.ObjectMapper;
import io.ioka.munarium.client.Munarium;
import io.ioka.munarium.client.MunariumClientOptions;
import io.ioka.munarium.client.ServerApiClient;
import io.ioka.munarium.client.ServerApiTransport.ApiRequest;
import io.ioka.munarium.client.errors.InvalidInputException;
import io.ioka.munarium.client.errors.NotFoundException;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;
import static org.junit.jupiter.api.Assertions.*;

class ServerApiTest {
    @ParameterizedTest
    @ValueSource(booleans = {false, true})
    void vocabularyAcrossTransports(boolean grpc) throws Exception {
        var endpoint = grpc ? Env.GRPC_URL : Env.REST_URL;
        Assumptions.assumeTrue(endpoint != null, "live Server endpoint unset");
        if (!endpoint.contains("://")) endpoint = "http://" + endpoint;
        var options = MunariumClientOptions.of(endpoint).withToken(Env.TOKEN == null ? "devtoken" : Env.TOKEN).withUid("api-java-conformance");
        var json = new ObjectMapper();
        try (var api = new ServerApiClient(options, grpc)) {
            assertEquals(Munarium.TARGET_SERVER_VERSION, api.versionInfoAsync(null).get().json().get("version").asText());
            var shape = "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: {name: api-java-docs, version: 1}\nspec:\n  fact:\n    schema: {type: object}\n";
            api.applyShape(new ApiRequest(Map.of(), List.of(), shape.getBytes(StandardCharsets.UTF_8), "text/yaml", Map.of(), null));
            var name = "api-java-" + UUID.randomUUID().toString();
            var collection = api.createCollection(ApiRequest.json(json.valueToTree(Map.of("name", name, "shape_ref", "api-java-docs@1", "access_level", 0, "compartments", List.of())), Map.of())).json();
            var path = Map.of("id", collection.get("id").asText());
            var first = api.getCollectionVocabulary(ApiRequest.empty().withPath("id", path.get("id"))).json();
            var body = json.createObjectNode().put("revision", first.get("revision").asLong()).put("enabled", true).put("auto_generate", false).putNull("sampling");
            body.putArray("groups").addArray().add("purchase order").add("procurement request");
            var input = ApiRequest.json(body, path);
            var saved = api.replaceCollectionVocabulary(input).json();
            assertTrue(saved.get("sampling").isNull());
            assertThrows(InvalidInputException.class, () -> api.replaceCollectionVocabulary(input));
            var patch = json.createObjectNode().put("revision", saved.get("revision").asLong()).put("enabled", false);
            assertFalse(api.updateCollectionVocabularyAsync(ApiRequest.json(patch, path)).get().json().get("enabled").asBoolean());
            assertThrows(NotFoundException.class, () -> api.getSource(ApiRequest.empty().withPath("source_id", "src-absent")));
        }
    }
}
