#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""The mechanical ground rules, in one place because two copies disagreed.

Ground rules 1 and 3 (`../docs/architecture.md`) are checked by grepping a
`cargo tree` and the migration files. Until 2026-08-30 they were checked
TWICE — once in `test.ps1` for the laptop and once in `matrix-ci.yml` for CI —
with three differences between the copies, and one of them made CI red for
three pushes while the laptop stayed green:

  * `cargo tree` defaults to the HOST target. `openssl-probe` is
    `cfg(unix)`, so a Windows laptop never saw it and Ubuntu always did.
  * the openssl rule matched the PREFIX `openssl`, so `openssl-probe` —
    which is `rustls-native-certs`' CA-path finder, links nothing, and is in
    the graph BECAUSE the tree is rustls-only — read as "openssl entered the
    graph".
  * the migration rule banned a retype on the laptop and not in CI.

So: one implementation, one target, one answer everywhere. The target is the
one that SHIPS (`x86_64-unknown-linux-musl`, the Dockerfile's), because the
graph worth ruling on is the deployed graph, and pinning it is what stops a
laptop and a runner from disagreeing again.

Exit 1 on any violation, naming it. Stdlib only.
"""
from __future__ import annotations

import pathlib
import json
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]

# The graph that ships. Not the host's: see the docstring.
TARGET = "x86_64-unknown-linux-musl"

# Ground rule 1: matrix/ never depends on a server/ crate. The official Rust
# client path-depends on three of them, so this also catches "just use the
# official client".
SERVER_CRATES = [
    "munarium-core",
    "munarium-api-types",
    "munarium-proto",
    "munarium-shapes",
    "munarium-runbooks",
]

# The kernel stays pure — no runtime, no driver — which is what keeps evidence
# identity testable in milliseconds.
CORE_BANNED = ["sqlx", "reqwest", "axum", "tokio", "object_store"]

# Munarium Matrix Enterprise. These adapters reach analytics platforms an
# enterprise buys and administers separately; they are a separate product and
# their crates are not in this repository. They reach a runtime through
# `adapters::AdapterRegistry`, never through a patch to `runtime::open_adapter`.
#
# This used to be checked by grepping a `--no-default-features` dependency tree
# for crate names that cannot resolve here at all, which is a check that can
# never fail. It is now checked at the source level, where a mistake would
# actually be made.
ENTERPRISE_ADAPTERS = [
    "munarium-matrix-adapter-databricks",
    "munarium-matrix-adapter-snowflake",
    "munarium-matrix-adapter-bigquery",
    "munarium-matrix-adapter-cube",
    "munarium-matrix-adapter-dbt",
]

# The adapters a Munarium Matrix core build ships -- stated positively, so the
# rule is falsifiable in both directions: an Enterprise adapter appearing here
# fails, and a core adapter going missing fails too.
CORE_ADAPTERS = {
    "munarium-matrix-adapter-landing",
    "munarium-matrix-adapter-postgres",
    "munarium-matrix-adapter-mysql",
    "munarium-matrix-adapter-sqlserver",
}

# Cargo features this workspace is allowed to declare. Empty, deliberately:
# every feature it declared was empty, gated code needing absent crates, and
# was never built by CI. A feature added without a CI job that builds it is the
# defect this list exists to prevent -- add the job, then add the name here.
ALLOWED_FEATURES: set[str] = set()

# Ground rule 3: rustls only. Named exactly, never by prefix.
TLS_BANNED = [
    "openssl",
    "openssl-sys",
    "openssl-macros",
    "native-tls",
    "hyper-tls",
    "tokio-native-tls",
    "tokio-openssl",
]

# In the graph BECAUSE the tree is rustls-only. `openssl-probe` reads the
# platform's CA bundle path for `rustls-native-certs`; it links no C and
# depends on nothing. Listing it here is the difference between a rule and a
# prefix match that cannot tell the two apart.
TLS_ALLOWED = {"openssl-probe": "rustls-native-certs' CA-path finder; links nothing"}


def tree(args: list[str]) -> list[str]:
    out = subprocess.run(
        ["cargo", "tree", "--edges", "normal", "--prefix", "none", "--target", TARGET, *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        print(out.stdout)
        print(out.stderr, file=sys.stderr)
        sys.exit(f"cargo tree failed: {' '.join(args)}")
    # "name v1.2.3 (path)" -> "name"
    return sorted({line.split()[0] for line in out.stdout.splitlines() if line.strip()})


def enterprise_reference_failures() -> list[str]:
    """No Rust source or manifest in this tree may name an Enterprise adapter."""
    failures: list[str] = []
    for path in list(ROOT.rglob("*.rs")) + list(ROOT.rglob("Cargo.toml")):
        if "target" in path.parts:
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for crate in ENTERPRISE_ADAPTERS:
            for spelling in (crate, crate.replace("-", "_")):
                if spelling in text:
                    failures.append(
                        f"{path.relative_to(ROOT)} names {crate}, which is Munarium Matrix "
                        "Enterprise and is not in this repository. Adapters register through "
                        "adapters::AdapterRegistry."
                    )
                    break
    return failures


def core_adapter_failures(workspace: list[str]) -> list[str]:
    """The shipping adapter set is exactly CORE_ADAPTERS -- no more, no less."""
    found = {c for c in workspace if c.startswith("munarium-matrix-adapter-")}
    failures = []
    for extra in sorted(found - CORE_ADAPTERS):
        failures.append(
            f"{extra} is in the workspace graph but is not one of the four core adapters"
        )
    for missing in sorted(CORE_ADAPTERS - found):
        failures.append(
            f"{missing} is a core adapter but is missing from the workspace graph"
        )
    return failures


def unexercised_feature_failures() -> list[str]:
    """Every Cargo feature declared by a workspace member must be allowlisted."""
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    meta = json.loads(out.stdout)
    failures = []
    for pkg in meta["packages"]:
        for feature in pkg.get("features", {}):
            if feature == "default":
                continue
            if feature not in ALLOWED_FEATURES:
                failures.append(
                    f"{pkg['name']} declares feature '{feature}', which no CI job builds. "
                    "Add a job that builds it, then add the name to ALLOWED_FEATURES "
                    "in scripts/boundaries.py."
                )
    return failures


def main() -> int:
    failures: list[str] = []

    workspace = tree(["--workspace"])
    core = tree(["-p", "munarium-matrix-core"])

    for crate in SERVER_CRATES:
        if crate in workspace:
            failures.append(f"matrix/ must not depend on the server crate '{crate}' (ground rule 1)")

    for crate in CORE_BANNED:
        if crate in core:
            failures.append(f"munarium-matrix-core must not depend on {crate}")

    for crate in TLS_BANNED:
        if crate in workspace:
            failures.append(f"{crate} entered the graph; rustls only (ground rule 3)")

    # The open/Enterprise line, made mechanical -- and checked where a mistake
    # would actually be made: in the source and the manifests.
    failures.extend(enterprise_reference_failures())

    # The same line stated positively: exactly these adapters, no more, no less.
    failures.extend(core_adapter_failures(workspace))

    # No feature this workspace declares may go unbuilt by CI. This is the rule
    # whose absence let an empty `enterprise-adapters` feature gate ~2,300 lines
    # that no gate ever compiled.
    failures.extend(unexercised_feature_failures())

    # A migration that drops, retypes or renames is how an operator loses data
    # during a rolling deploy. Crude on purpose: a rule that cannot be argued
    # with.
    bad_sql = re.compile(
        r"\b(drop\s+table|drop\s+column|alter\s+column\s+\w+\s+type|rename\s+to)\b",
        re.IGNORECASE,
    )
    for path in sorted((ROOT / "src/munarium-matrix-store/migrations").glob("*.sql")):
        for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if bad_sql.search(line):
                failures.append(f"non-additive migration: {path.name}:{n} {line.strip()}")

    for f in failures:
        print(f"::error::{f}" if "CI" in __import__("os").environ else f"FAIL: {f}")
    if failures:
        return 1

    allowed = ", ".join(f"{k} ({v})" for k, v in TLS_ALLOWED.items() if k in workspace)
    print(
        f"boundaries: {len(workspace)} crates in the shipping graph ({TARGET}): "
        f"no server crate, core is pure, rustls only, migrations additive; "
        f"exactly the {len(CORE_ADAPTERS)} core adapters, no Enterprise adapter named "
        f"anywhere in source or manifests, no unexercised Cargo feature"
        + (f"; allowed beside rustls: {allowed}" if allowed else "")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
