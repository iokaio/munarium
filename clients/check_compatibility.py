#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""compatibility.json must match what each package manifest actually declares
-- otherwise the compatibility record documents a release that cannot happen.
Read, never regenerated: the manifests are the source of truth; this script is
the drift check between them and the record a human wrote by hand at release
time.

    py clients/check_compatibility.py

Four things are checked per client, not one:

  * `version`   -- against the manifest's version
  * `package`   -- against the manifest's package name / id / coordinate
  * `registry`  -- against the set of registries that manifest can target
  * publishable -- the manifest actually carries what the named registry
                   requires (a maven-publish publication with url/licenses/
                   developers/scm for Maven Central, and so on)

Checking only `version` is how compatibility.json came to promise a PyPI
package called `munarium-matrix-client` while the pyproject declared
`munarium-matrix`, and a Maven Central release from a build file whose own
first comment said it was never published to a registry. Both passed.

Exit 1 on any mismatch or missing entry. Stdlib only (the Directory.Build.props
read is a regex over XML text, not a full parser -- sufficient for one
<Version> element).
"""
from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def rust_version() -> str:
    d = tomllib.loads((ROOT / "rust/Cargo.toml").read_text(encoding="utf-8"))
    return d["workspace"]["package"]["version"]


def python_version() -> str:
    d = tomllib.loads((ROOT / "python/pyproject.toml").read_text(encoding="utf-8"))
    return d["project"]["version"]


# The Munarium Matrix clients. They speak to Matrix rather than to the Server,
# so their row names a Matrix range. They can sit in either of two places:
# `clients/matrix-<lang>` beside the Server's four, or `matrix/clients/<lang>`.
# `matrix_path` resolves whichever exists, so one script serves both layouts.
def matrix_path(lang: str, name: str) -> Path:
    here = ROOT / f"matrix-{lang}" / name
    return here if here.is_file() else ROOT.parent / "matrix" / "clients" / lang / name
def matrix_python_version() -> str:
    d = tomllib.loads(matrix_path("python", "pyproject.toml").read_text(encoding="utf-8"))
    return d["project"]["version"]


def matrix_dotnet_version() -> str:
    text = matrix_path("dotnet", "Directory.Build.props").read_text(encoding="utf-8")
    m = re.search(r"<Version>([^<]+)</Version>", text)
    if not m:
        raise SystemExit("matrix-dotnet/Directory.Build.props: no <Version> element found")
    return m.group(1)


def matrix_java_version() -> str:
    text = matrix_path("java", "build.gradle.kts").read_text(encoding="utf-8")
    m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    if not m:
        raise SystemExit('matrix-java/build.gradle.kts: no top-level version = "..." found')
    return m.group(1)


def dotnet_version() -> str:
    text = (ROOT / "dotnet/Directory.Build.props").read_text(encoding="utf-8")
    m = re.search(r"<Version>([^<]+)</Version>", text)
    if not m:
        raise SystemExit("dotnet/Directory.Build.props: no <Version> element found")
    return m.group(1)


def java_version() -> str:
    text = (ROOT / "java/build.gradle.kts").read_text(encoding="utf-8")
    m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    if not m:
        raise SystemExit("java/build.gradle.kts: no top-level version = \"...\" found")
    return m.group(1)


# --------------------------------------------------------------- package ids


def rust_package() -> str:
    d = tomllib.loads((ROOT / "rust/munarium-client/Cargo.toml").read_text(encoding="utf-8"))
    return d["package"]["name"]


def python_package() -> str:
    d = tomllib.loads((ROOT / "python/pyproject.toml").read_text(encoding="utf-8"))
    return d["project"]["name"]


def matrix_python_package() -> str:
    d = tomllib.loads(matrix_path("python", "pyproject.toml").read_text(encoding="utf-8"))
    return d["project"]["name"]


def _dotnet_package(props: Path, project_glob: str) -> str:
    """NuGet id: an explicit <PackageId>, else the .csproj file name."""
    text = props.read_text(encoding="utf-8")
    m = re.search(r"<PackageId>([^<]+)</PackageId>", text)
    if m:
        return m.group(1)
    projects = sorted(props.parent.glob(project_glob))
    if not projects:
        raise SystemExit(f"{props}: no <PackageId> and no project matching {project_glob}")
    return projects[0].stem


def dotnet_package() -> str:
    return _dotnet_package(ROOT / "dotnet/Directory.Build.props", "src/*/*.csproj")


def matrix_dotnet_package() -> str:
    return _dotnet_package(matrix_path("dotnet", "Directory.Build.props"), "src/*/*.csproj")


def _gradle_coordinate(path: Path) -> str:
    text = path.read_text(encoding="utf-8")
    group = re.search(r'^group\s*=\s*"([^"]+)"', text, re.M)
    name = re.search(r'^\s*name\s*=\s*"([^"]+)"', text, re.M)
    if not group or not name:
        raise SystemExit(f"{path}: need a top-level group and a POM name to form a coordinate")
    return f"{group.group(1)}:{name.group(1)}"


def java_package() -> str:
    return _gradle_coordinate(ROOT / "java/build.gradle.kts")


def matrix_java_package() -> str:
    return _gradle_coordinate(matrix_path("java", "build.gradle.kts"))


# ------------------------------------------------------------- publishability

# What each registry requires of a manifest before a release can even be
# attempted. Each entry is (needle, why) checked against the build file text.
MAVEN_CENTRAL_REQUIRES = [
    ("`maven-publish`", "the maven-publish plugin"),
    ("MavenPublication", "a publication"),
    ("licenses {", "a <licenses> block"),
    ("developers {", "a <developers> block"),
    ("scm {", "an <scm> block"),
    ("url =", "a project url"),
    ("withSourcesJar()", "a -sources jar"),
    ("withJavadocJar()", "a -javadoc jar"),
]


def maven_publishable(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8")
    return [why for needle, why in MAVEN_CENTRAL_REQUIRES if needle not in text]


def registry_problems(lang: str, entry: dict) -> list[str]:
    """Can this manifest target the registry compatibility.json names?"""
    registry = entry.get("registry")
    if registry is None:
        return [f"{lang}: no registry named in compatibility.json"]
    expected = {
        "rust": "crates.io", "python": "PyPI", "dotnet": "NuGet", "java": "Maven Central",
        "matrix-python": "PyPI", "matrix-dotnet": "NuGet", "matrix-java": "Maven Central",
    }[lang]
    if registry != expected:
        return [f"{lang}: registry {registry!r} is not this ecosystem's ({expected!r})"]
    if registry != "Maven Central":
        return []
    path = (ROOT / "java/build.gradle.kts") if lang == "java" \
        else matrix_path("java", "build.gradle.kts")
    missing = maven_publishable(path)
    if missing:
        return [f"{lang}: compatibility.json promises Maven Central but "
                f"{path.name} is missing {', '.join(missing)}"]
    return []


READERS = {
    "rust": (rust_version, rust_package),
    "python": (python_version, python_package),
    "dotnet": (dotnet_version, dotnet_package),
    "java": (java_version, java_package),
    "matrix-python": (matrix_python_version, matrix_python_package),
    "matrix-dotnet": (matrix_dotnet_version, matrix_dotnet_package),
    "matrix-java": (matrix_java_version, matrix_java_package),
}


def main() -> int:
    record = json.loads((ROOT / "compatibility.json").read_text(encoding="utf-8"))
    bad: list[str] = []

    # Every entry in the record needs a reader, and every reader an entry --
    # otherwise a client can be added to one and forgotten in the other.
    for lang in record["clients"]:
        if lang not in READERS:
            bad.append(f"{lang}: in compatibility.json but this script cannot read its manifest")

    for lang, (read_version, read_package) in READERS.items():
        entry = record["clients"].get(lang)
        if entry is None:
            bad.append(f"{lang}: no entry in compatibility.json")
            continue

        actual = read_version()
        if entry["version"] != actual:
            bad.append(
                f"{lang}: compatibility.json says version {entry['version']!r}, "
                f"the manifest says {actual!r}"
            )

        declared = read_package()
        if entry.get("package") != declared:
            bad.append(
                f"{lang}: compatibility.json says package {entry.get('package')!r}, "
                f"the manifest declares {declared!r}"
            )

        bad.extend(registry_problems(lang, entry))

        ranges = entry.get("supported_server") or entry.get("supported_matrix")
        if not ranges:
            bad.append(f"{lang}: names neither supported_server nor supported_matrix")

    if bad:
        for line in bad:
            print(line)
        print(f"check_compatibility: {len(bad)} problem(s) -- "
              "fix the manifest or the record, whichever is wrong")
        return 1
    print(f"check_compatibility: {len(READERS)} client(s) -- version, package id, "
          "registry and publishability all agree with compatibility.json -- ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
