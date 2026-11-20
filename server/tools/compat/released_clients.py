# SPDX-License-Identifier: Apache-2.0
"""Build disposable conformance projects that use the PUBLISHED 1.4.0 clients.

The in-tree conformance suites compile against the checkout's client source.
For COMPAT-T2 the released packages must be under test instead, so this script
copies each suite from an export of the ``clients-v1.4.0`` source tree and
rewrites only its dependency on the client:

* Rust: ``munarium-client = "=1.4.0"`` and ``munarium-api-types = "=1.4.0"``
  from crates.io instead of workspace paths.
* .NET: a ``PackageReference`` to ``Ioka.Munarium.Client`` 1.4.0 instead of
  the ``ProjectReference``.
* Java: a ``conformanceTest`` source set depending on
  ``io.ioka.munarium:munarium-client:1.4.0`` from Maven Central.
* Python: the suite directory alone, so its ``../src`` path insertion finds
  nothing and the installed wheel is imported.

Usage::

    git archive 25012c22087fbaa8ce29ddd68d2880d6e9499b1c clients | tar -x -C <export>
    python released_clients.py --export <export>/clients --dest <scratch dir>

The output belongs in an ignored scratch directory; nothing here is tracked.
"""

from __future__ import annotations

import argparse
import re
import shutil
from pathlib import Path

VERSION = "1.4.0"


def rust_manifest(version: str = VERSION) -> str:
    return f"""[package]
name = "munarium-client-conformance"
version = "{version}"
edition = "2021"
license = "Apache-2.0"
publish = false

[[bin]]
name = "munarium-client-conformance"
path = "src/main.rs"

[dependencies]
munarium-client = "={version}"
munarium-api-types = {{ version = "={version}", features = ["proto"] }}
tokio = {{ version = "1", features = ["rt-multi-thread", "macros", "time", "sync"] }}
serde_json = "1"
futures-util = "0.3"
sha2 = "0.10"
hex = "0.4"
base64 = "0.22"

[workspace]
"""


def patch_csproj(text: str, version: str = VERSION) -> str:
    """Replace the client ProjectReference with the published package."""
    pattern = re.compile(r'<ProjectReference Include="[^"]*Ioka\.Munarium\.Client\.csproj"\s*/>')
    if not pattern.search(text):
        raise ValueError("client ProjectReference not found")
    return pattern.sub(f'<PackageReference Include="Ioka.Munarium.Client" Version="{version}" />', text)


def java_build(version: str = VERSION) -> str:
    return f"""plugins {{
    java
}}

repositories {{
    mavenCentral()
}}

sourceSets {{
    create("conformanceTest")
}}

dependencies {{
    "conformanceTestImplementation"("io.ioka.munarium:munarium-client:{version}")
    "conformanceTestImplementation"("com.fasterxml.jackson.core:jackson-databind:2.19.0")
    "conformanceTestImplementation"("org.junit.jupiter:junit-jupiter:5.12.2")
    "conformanceTestImplementation"("org.junit.jupiter:junit-jupiter-params:5.12.2")
    // The published POM scopes gRPC as runtime; the suite's ServerApiClient(options,
    // ManagedChannel) overload needs it at compile time, as the in-tree build has it.
    "conformanceTestImplementation"("io.grpc:grpc-api:1.73.0")
    "conformanceTestRuntimeOnly"("org.junit.platform:junit-platform-launcher")
}}

tasks.withType<JavaCompile>().configureEach {{
    options.release = 21
    options.encoding = "UTF-8"
}}

tasks.register<Test>("conformanceTest") {{
    testClassesDirs = sourceSets["conformanceTest"].output.classesDirs
    classpath = sourceSets["conformanceTest"].runtimeClasspath
    useJUnitPlatform()
    testLogging {{ events("passed", "failed", "skipped") }}
    outputs.upToDateWhen {{ false }}
}}
"""


def prepare(export: Path, dest: Path) -> None:
    if dest.exists():
        shutil.rmtree(dest)
    # Rust
    rust = dest / "rust"
    shutil.copytree(export / "rust" / "munarium-client-conformance" / "src", rust / "src")
    (rust / "Cargo.toml").write_text(rust_manifest(), encoding="utf-8")
    shutil.copy(export / "rust" / "rust-toolchain.toml", rust / "rust-toolchain.toml")
    # .NET
    net = dest / "dotnet" / "Conformance"
    shutil.copytree(export / "dotnet" / "tests" / "Ioka.Munarium.Client.Conformance", net)
    shutil.copy(export / "dotnet" / "Directory.Build.props", dest / "dotnet" / "Directory.Build.props")
    csproj = net / "Ioka.Munarium.Client.Conformance.csproj"
    csproj.write_text(patch_csproj(csproj.read_text(encoding="utf-8")), encoding="utf-8")
    # Java
    java = dest / "java"
    shutil.copytree(export / "java" / "src" / "conformanceTest", java / "src" / "conformanceTest")
    shutil.copytree(export / "java" / "gradle", java / "gradle")
    for name in ("gradlew", "gradlew.bat"):
        shutil.copy(export / "java" / name, java / name)
    (java / "settings.gradle.kts").write_text('rootProject.name = "released-client-conformance"\n', encoding="utf-8")
    (java / "build.gradle.kts").write_text(java_build(), encoding="utf-8")
    # Python
    shutil.copytree(export / "python" / "conformance", dest / "python" / "conformance")
    # The API-catalog conformance test reads clients/server-api.json two levels up.
    shutil.copy(export / "server-api.json", dest / "server-api.json")
    (dest / "python" / "pytest.ini").write_text("[pytest]\nasyncio_mode = auto\n", encoding="utf-8")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--export", required=True, type=Path)
    ap.add_argument("--dest", required=True, type=Path)
    args = ap.parse_args()
    prepare(args.export, args.dest)
    print("prepared", args.dest)


if __name__ == "__main__":
    main()
