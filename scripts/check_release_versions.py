#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Check source versions and published-image defaults without registry access.

Cargo owns the source version. server/release-versions.json records the last
published image: installation defaults stay there during release preparation.
Update that record and the installation defaults only with publication evidence.
"""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INSTALL_DOCS = (
    "README.md",
    "server/CONTAINER.md",
    "server/docs/guides/getting-started.md",
    "server/deploy/helm/munarium/README.md",
)
IMAGE_PIN = re.compile(r"(?:iokaio/munarium:|image\.tag=)([a-zA-Z0-9_.-]+)")


def check(root: Path) -> list[str]:
    errors: list[str] = []

    def read(path: str) -> str:
        return (root / path).read_text(encoding="utf-8")

    def one(path: str, pattern: str) -> str:
        matches = re.findall(pattern, read(path), re.MULTILINE)
        if len(matches) != 1:
            raise ValueError(f"{path}: expected exactly one version declaration")
        return matches[0]

    def equal(label: str, actual: str, expected: str) -> None:
        if actual != expected:
            errors.append(f"{label}: {actual!r}; expected {expected!r}")

    try:
        source = tomllib.loads(read("server/Cargo.toml"))["workspace"]["package"][
            "version"
        ]
        published = json.loads(read("server/release-versions.json"))["published_image"]
        for label, version in (("workspace", source), ("published_image", published)):
            if not isinstance(version, str) or not re.fullmatch(
                r"\d+\.\d+\.\d+", version
            ):
                raise ValueError(
                    f"{label}: expected a stable major.minor.patch version"
                )
        if tuple(map(int, published.split("."))) > tuple(map(int, source.split("."))):
            errors.append(
                "server/release-versions.json: published image is newer than source"
            )

        equal(
            "server/Dockerfile BUILD_VERSION",
            one("server/Dockerfile", r'^ARG BUILD_VERSION="([^"]+)"$'),
            source,
        )
        for path, key in (
            ("server/docs/api/openapi.json", ("info", "version")),
            ("clients/server-api.json", ("server_version",)),
            ("clients/compatibility.json", ("target_server",)),
        ):
            value = json.loads(read(path))
            for part in key:
                value = value[part]
            equal(path, value, source)

        equal(
            "server/deploy/helm/munarium/Chart.yaml appVersion",
            one("server/deploy/helm/munarium/Chart.yaml", r'^appVersion: "([^"]+)"$'),
            published,
        )
        equal(
            "server/deploy/helm/munarium/values.yaml image.tag",
            one(
                "server/deploy/helm/munarium/values.yaml",
                r'^image:\n(?:(?:[ \t]+[^\n]*|)\n)*?  tag: "([^"]+)"$',
            ),
            published,
        )
        equal(
            "server/CONTAINER.md publication record",
            one("server/CONTAINER.md", r"^### Last published image: (\d+\.\d+\.\d+)$"),
            published,
        )
        for path in INSTALL_DOCS:
            # A source-build command creates a candidate; it is not an install.
            pins = [
                pin
                for line in read(path).splitlines()
                if "--tag iokaio/munarium:" not in line
                for pin in IMAGE_PIN.findall(line)
            ]
            if not pins:
                errors.append(f"{path}: missing published image installation pin")
            for pin in sorted(set(pins)):
                equal(f"{path} installation pin", pin, published)
    except (OSError, KeyError, ValueError, TypeError) as error:
        errors.append(f"release inputs: {error}")
    return errors


def main() -> int:
    errors = check(ROOT)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("check_release_versions: source artifacts and published-image defaults agree")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
