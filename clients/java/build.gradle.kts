// SPDX-License-Identifier: Apache-2.0
//
// Official Java client for munarium-server — one plane surface, two transports
// (REST + gRPC), sync and async twins, typed errors keyed on the
// problem-slug registry. Maven Central publication is a release-time step, so
// the `maven-publish` block below declares the POM's license and nothing else
// about where it goes.
//
// Build posture mirrors the .NET client: gRPC stubs are generated at BUILD
// time from the normative protos under ../../server/proto (all ten
// files, session.proto and admin.proto included), so there is no committed
// generated code and no drift check to run. Bytecode targets Java 21 (LTS:
// records, sealed types, virtual threads) while building on any newer JDK via
// `--release`; CI builds on temurin 21.

plugins {
    `java-library`
    `maven-publish`
    id("com.google.protobuf") version "0.10.0"
}

group = "io.ioka.munarium"
version = "1.0.0"

repositories {
    mavenCentral()
}

val grpcVersion = "1.73.0"
val protobufVersion = "4.31.1"
val jacksonVersion = "2.19.0"
val junitVersion = "5.12.2"

dependencies {
    // REST rides java.net.http (zero-dep); JSON is Jackson over records.
    api("com.fasterxml.jackson.core:jackson-databind:$jacksonVersion")
    // gRPC: netty-shaded transport, protobuf marshalling, common protos for
    // google.rpc.ErrorInfo decoding out of grpc-status-details-bin. All
    // `implementation`: no pb/grpc type appears in the public API (consumers
    // construct via MunariumClient.grpc), so they stay off consumers' compile
    // classpaths. (Splitting a REST-only artifact to drop them from the
    // runtime classpath too is future work, documented in clientplan.)
    implementation("io.grpc:grpc-netty-shaded:$grpcVersion")
    implementation("io.grpc:grpc-protobuf:$grpcVersion")
    implementation("io.grpc:grpc-stub:$grpcVersion")
    implementation("com.google.protobuf:protobuf-java:$protobufVersion")
    implementation("com.google.api.grpc:proto-google-common-protos:2.72.0")

    // grpc-java's generated stubs carry @javax.annotation.Generated.
    compileOnly("org.apache.tomcat:annotations-api:6.0.53")

    testImplementation("org.junit.jupiter:junit-jupiter:$junitVersion")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

java {
    sourceCompatibility = JavaVersion.VERSION_21
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 21
    options.encoding = "UTF-8"
    // No -Werror: the protoc/grpc GENERATED sources compile in the same
    // unit and trip lint categories our code never would. Our own code is
    // kept warning-clean by review + the lint output below.
    options.compilerArgs.add("-Xlint:all,-processing,-serial,-this-escape")
}

protobuf {
    protoc {
        artifact = "com.google.protobuf:protoc:$protobufVersion"
    }
    plugins {
        create("grpc") {
            artifact = "io.grpc:protoc-gen-grpc-java:$grpcVersion"
        }
    }
    generateProtoTasks {
        all().forEach { task ->
            task.plugins {
                create("grpc")
            }
        }
    }
}

sourceSets {
    main {
        proto {
            // The normative MMP protos (server/proto, the tree that owns them; cut from the
            // server tree by server/contract/mmp/publish.py and drift-checked in
            // CI) — the same zero-drift posture as the .NET client.
            srcDir("../../server/proto")
        }
    }
    create("conformanceTest") {
        compileClasspath += sourceSets.main.get().output
        runtimeClasspath += sourceSets.main.get().output
    }
}

val conformanceTestImplementation = configurations.getByName("conformanceTestImplementation") {
    extendsFrom(configurations.testImplementation.get())
}
configurations.named("conformanceTestRuntimeOnly") {
    extendsFrom(configurations.testRuntimeOnly.get())
}

tasks.test {
    useJUnitPlatform()
    testLogging { events("failed", "skipped") }
}

// Apache packaging convention: LICENSE and NOTICE travel inside EVERY jar a
// release ships, the main jar and the -sources and -javadoc jars Central
// requires (declared below), under META-INF, read from the repository root
// (one copy of each; check_license.py verifies every copy under clients/ is
// byte-identical), and each manifest names the license. `withType<Jar>` is
// what reaches all three: `tasks.jar` alone left the secondary jars with a
// bare manifest, and CI's artifact check reads every jar under build/libs.
// `rootDir` is this Gradle project; its parent is the clients root, here and
// in the public repository alike.
tasks.withType<Jar>().configureEach {
    from(files(rootDir.parentFile.resolve("LICENSE"), rootDir.parentFile.resolve("NOTICE"))) {
        into("META-INF")
    }
    manifest {
        attributes(
            "Implementation-Title" to "munarium-client",
            "Implementation-Version" to version,
            "Implementation-Vendor" to "Ioka LLC",
            "Bundle-License" to "Apache-2.0",
        )
    }
}

// Maven Central requires name, description, url, licenses, developers and scm
// in the POM, plus -sources and -javadoc jars. All of that is declared here so
// the artifact a release builds is the artifact Central accepts. No repository
// is configured and no signing plugin is applied: the signing key and the
// target repository are release-time credentials, not repository content.
java {
    withSourcesJar()
    withJavadocJar()
}

publishing {
    publications {
        create<MavenPublication>("maven") {
            from(components["java"])
            pom {
                name = "munarium-client"
                description = "Official Java client for Munarium Server: one plane surface, two transports (REST + gRPC), sync and async, typed errors keyed on the problem-slug registry."
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

// Live conformance against a running pg-backed server. NOT part of `build`:
// run explicitly with MUNARIUM_REST_URL / MUNARIUM_GRPC_URL / MUNARIUM_TOKEN /
// MUNARIUM_MGMT_TOKEN set (tests skip cleanly when unset).
val conformanceTest = tasks.register<Test>("conformanceTest") {
    description = "Runs the conformance scenarios + platform smokes against a live server."
    group = "verification"
    testClassesDirs = sourceSets["conformanceTest"].output.classesDirs
    classpath = sourceSets["conformanceTest"].runtimeClasspath
    useJUnitPlatform()
    testLogging { events("passed", "failed", "skipped") }
    outputs.upToDateWhen { false } // always re-run against the live server
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
