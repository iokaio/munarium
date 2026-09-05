// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.util.Arrays;
import java.util.Locale;
import java.util.Set;
import java.util.TreeSet;
import java.util.stream.Collectors;
import org.junit.jupiter.api.Test;

/**
 * The design decisions, asserted rather than described. A rule that lives only
 * in a README is a rule that is one well-meaning pull request from being gone.
 */
class SurfaceTest {

    @Test
    void noMethodOnThisClientSealsEvidence() {
        // An SDK that could seal would invite an application to assert
        // provenance it cannot vouch for: a manifest is a statement about work
        // the SEALER did. Sealing is Matrix's own act, and evidence is READ
        // through the server's client.
        for (Class<?> type : new Class<?>[] {MatrixClient.class, AsyncMatrixClient.class}) {
            Set<String> offending = publicMethodNames(type).stream()
                    .filter(n -> n.contains("seal") || n.contains("evidence"))
                    .collect(Collectors.toCollection(TreeSet::new));
            assertTrue(offending.isEmpty(), type.getSimpleName() + " grew " + offending);
        }
    }

    @Test
    void noMethodOnThisClientTakesSql() {
        // Queries are pre-declared contracts and views, executed by name.
        // Nothing on this surface takes a statement, and a method named for
        // one would be the first crack.
        for (Class<?> type : new Class<?>[] {MatrixClient.class, AsyncMatrixClient.class}) {
            Set<String> offending = publicMethodNames(type).stream()
                    .filter(n -> n.contains("sql") || n.contains("query") || n.contains("statement"))
                    .collect(Collectors.toCollection(TreeSet::new));
            assertTrue(offending.isEmpty(), type.getSimpleName() + " grew " + offending);
        }
    }

    @Test
    void theAsyncTwinIsMethodForMethodTheSameSurface() {
        // A method on one twin and not the other is a trap for a caller
        // porting between them: it compiles the day they write it and fails at
        // the one call the other twin never grew. Asserting parity is the only
        // thing that keeps the docstring honest.
        Set<String> blocking = new TreeSet<>(declaredInstanceMethodNames(MatrixClient.class));
        Set<String> async = new TreeSet<>(declaredInstanceMethodNames(AsyncMatrixClient.class));
        // `close` is AutoCloseable's, and `blocking()` is the escape hatch to
        // the twin itself — neither is part of the Matrix surface.
        blocking.remove("close");
        async.remove("close");
        async.remove("blocking");
        assertEquals(blocking, async);
    }

    @Test
    void theRefusalClassesThatMeanRetryAreExactlyTwo() {
        // The closed vocabulary, pinned. `unavailable` and `exhausted` are
        // states of the world; the other four are statements about the request
        // or the assets, and repeating one changes nothing.
        String[] classes = {"not_covered", "unavailable", "denied", "incomplete", "invalid", "exhausted"};
        Set<String> retryable = Arrays.stream(classes)
                .filter(c -> new MatrixException("x", 500, c, "code", null).retryable())
                .collect(Collectors.toCollection(TreeSet::new));
        assertEquals(Set.of("exhausted", "unavailable"), retryable);
    }

    @Test
    void aFailureCarryingNoRefusalIsNotRetryable() {
        // A 404 for a missing asset carries no refusal object at all. Absent
        // is not "maybe": nothing about a name that does not exist improves by
        // asking again.
        assertTrue(!new MatrixException("not found", 404, null, null, null).retryable());
    }

    private static Set<String> publicMethodNames(Class<?> type) {
        return Arrays.stream(type.getMethods())
                .map(Method::getName)
                .map(n -> n.toLowerCase(Locale.ROOT))
                .collect(Collectors.toCollection(TreeSet::new));
    }

    private static Set<String> declaredInstanceMethodNames(Class<?> type) {
        return Arrays.stream(type.getDeclaredMethods())
                .filter(m -> Modifier.isPublic(m.getModifiers()))
                .filter(m -> !Modifier.isStatic(m.getModifiers()))
                .filter(m -> !m.isSynthetic())
                .map(Method::getName)
                .collect(Collectors.toCollection(TreeSet::new));
    }
}
