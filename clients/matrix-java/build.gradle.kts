// SPDX-License-Identifier: Apache-2.0
// Official Java client for Munarium Matrix — the structured-evidence plane.
// Published to Maven Central at release time (Apache-2.0); usable as a
// composite build (`includeBuild`) against a checkout.
//
// ONE dependency: Jackson databind. REST rides java.net.http, which ships with
// the JDK, and the tests drive a `com.sun.net.httpserver` stub rather than
// pulling in a mock-server library — a client whose dependency list is one
// line cannot break a consumer's build over a transitive conflict.
//
// There are deliberately NO protobuf or gRPC dependencies here, and no protobuf
// plugin. Matrix's gRPC plane serves `MatrixQuery/Execute` alone and that call
// is service-to-service: the munarium-server makes it while answering a turn,
// carrying a session's authorization snapshot an application does not hold.
// Generating stubs for it would put ~15 MB of transitive netty on every
// consumer's classpath to expose a call none of them may make. If Matrix ever
// grows a second RPC that an application is entitled to, this file grows a
// transport — not a second client.
//
// Bytecode targets Java 21 (LTS: records, virtual threads) while building on
// any newer JDK via `--release`.

plugins {
    `java-library`
    `maven-publish`
}

group = "io.ioka.munarium"
version = "1.0.0"

repositories {
    mavenCentral()
}

val jacksonVersion = "2.19.0"
val junitVersion = "5.12.2"

dependencies {
    // `api`, not `implementation`: JsonNode appears in the public surface for
    // the reads whose shape is genuinely open (introspect, the journal), so a
    // consumer must be able to name the type.
    api("com.fasterxml.jackson.core:jackson-databind:$jacksonVersion")

    testImplementation("org.junit.jupiter:junit-jupiter:$junitVersion")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

java {
    sourceCompatibility = JavaVersion.VERSION_21
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 21
    options.encoding = "UTF-8"
    // No generated sources compile in this unit, so -Werror is affordable
    // here in a way it is not in the server client.
    options.compilerArgs.addAll(listOf("-Xlint:all,-processing,-this-escape", "-Werror", "-parameters"))
}

tasks.test {
    useJUnitPlatform()
    // `skipped` and the standard streams are both on so the live tier's
    // out-loud skip actually reaches the console. A skip that prints nothing
    // is indistinguishable from a pass.
    testLogging {
        events("passed", "failed", "skipped")
        showStandardStreams = true
    }
}

// Apache packaging convention: LICENSE and NOTICE travel inside EVERY jar a
// release ships, the main jar and the -sources and -javadoc jars Central
// requires (declared below), under META-INF, read from the clients root (one
// copy of each; check_license.py verifies every copy under clients/ is
// byte-identical), and each manifest names the license. `withType<Jar>` is
// what reaches all three: `tasks.jar` alone left the secondary jars with a
// bare manifest. Same shape as java/build.gradle.kts; change both together.
tasks.withType<Jar>().configureEach {
    from(files(rootDir.parentFile.resolve("LICENSE"), rootDir.parentFile.resolve("NOTICE"))) {
        into("META-INF")
    }
    manifest {
        attributes(
            "Implementation-Title" to "munarium-matrix-client",
            "Implementation-Version" to version,
            "Implementation-Vendor" to "Ioka LLC",
            "Bundle-License" to "Apache-2.0",
        )
    }
}

// Maven Central requires name, description, url, licenses, developers and scm
// in the POM, plus -sources and -javadoc jars. No repository is configured and
// no signing plugin is applied: the signing key and the target repository are
// release-time credentials, not repository content.
java {
    withSourcesJar()
    withJavadocJar()
}

publishing {
    publications {
        create<MavenPublication>("maven") {
            from(components["java"])
            pom {
                name = "munarium-matrix-client"
                description = "Official Java client for Munarium Matrix: the structured-evidence plane."
                url = "https://github.com/iokaio/munarium"
                licenses {
                    license {
                        name = "Apache-2.0"
                        url = "https://www.apache.org/licenses/LICENSE-2.0"
                    }
                }
                developers {
                    developer {
                        id = "ioka"
                        name = "Ioka LLC"
                        url = "https://github.com/iokaio"
                    }
                }
                scm {
                    connection = "scm:git:https://github.com/iokaio/munarium.git"
                    developerConnection = "scm:git:ssh://git@github.com/iokaio/munarium.git"
                    url = "https://github.com/iokaio/munarium"
                }
            }
        }
    }
}

// Javadoc for the -javadoc jar. `-missing` is off: these are records whose
// component names ARE the documentation, and demanding @param for each one
// would add noise, not meaning. Every other doclint check stays on, so a
// malformed tag or a broken @link still fails the build.
tasks.withType<Javadoc>().configureEach {
    (options as StandardJavadocDocletOptions).apply {
        addStringOption("Xdoclint:all,-missing", "-quiet")
    }
}
