// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import com.sun.net.httpserver.HttpServer;
import java.io.IOException;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CopyOnWriteArrayList;

/**
 * A stand-in Matrix: a JDK {@code HttpServer} on an ephemeral port, driven by
 * a handler the test writes inline.
 *
 * <p>A stub over a mocking library on purpose. These tests assert the exact
 * BYTES the service would have sent, and a fixture library would put its own
 * opinions about JSON between the test and the wire — which is the one thing
 * these tests exist to leave nothing standing between. It also keeps the
 * dependency list at one entry.
 */
final class StubMatrix implements AutoCloseable {

    /**
     * One request as the stub saw it.
     *
     * <p>{@code path} is the RAW path — percent-escapes intact. The decoded
     * form would hide the whole point of encoding an asset name: a name
     * carrying a '/' decodes back into a path that looks like a different
     * route, so a test reading the decoded path cannot tell the two apart.
     */
    record Request(String method, String path, String query, Map<String, String> headers, String body) {}

    /** One canned response. */
    record Reply(int status, String contentType, String body) {
        static Reply json(int status, String body) {
            return new Reply(status, "application/json", body);
        }

        static Reply problem(int status, String body) {
            return new Reply(status, "application/problem+json", body);
        }

        static Reply yaml(int status, String body) {
            return new Reply(status, "text/yaml; charset=utf-8", body);
        }
    }

    @FunctionalInterface
    interface Handler {
        Reply handle(Request request);
    }

    private final HttpServer server;
    private final List<Request> seen = new CopyOnWriteArrayList<>();

    StubMatrix(Handler handler) throws IOException {
        server = HttpServer.create(new InetSocketAddress(InetAddress.getLoopbackAddress(), 0), 0);
        server.createContext("/", exchange -> {
            byte[] requestBody = exchange.getRequestBody().readAllBytes();
            var headers = new java.util.LinkedHashMap<String, String>();
            exchange.getRequestHeaders()
                    .forEach((k, v) -> headers.put(k.toLowerCase(java.util.Locale.ROOT), String.join(",", v)));
            Request request = new Request(
                    exchange.getRequestMethod(),
                    exchange.getRequestURI().getRawPath(),
                    exchange.getRequestURI().getQuery(),
                    headers,
                    new String(requestBody, StandardCharsets.UTF_8));
            seen.add(request);
            Reply reply = handler.handle(request);
            byte[] out = reply.body().getBytes(StandardCharsets.UTF_8);
            exchange.getResponseHeaders().set("content-type", reply.contentType());
            exchange.sendResponseHeaders(reply.status(), out.length == 0 ? -1 : out.length);
            try (OutputStream os = exchange.getResponseBody()) {
                os.write(out);
            }
        });
        server.start();
    }

    String url() {
        return "http://" + server.getAddress().getAddress().getHostAddress()
                + ":" + server.getAddress().getPort();
    }

    MatrixClient client() {
        return MatrixClient.of(url(), "t");
    }

    /** Every request the stub received, in order. */
    List<Request> seen() {
        return List.copyOf(seen);
    }

    /** Just the paths, which is what most of these tests are asserting about. */
    List<String> paths() {
        return seen.stream().map(Request::path).toList();
    }

    @Override
    public void close() {
        server.stop(0);
    }

    /**
     * A port with nothing behind it: bound to claim it, then released. Used to
     * make a connection genuinely fail rather than hang, which is the only way
     * to test the transport-failure path honestly.
     */
    static String deadEndpoint() throws IOException {
        try (ServerSocket socket = new ServerSocket(0, 0, InetAddress.getLoopbackAddress())) {
            return "http://" + socket.getInetAddress().getHostAddress() + ":" + socket.getLocalPort();
        }
    }
}
