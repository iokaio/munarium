# SPDX-License-Identifier: Apache-2.0
"""Local v1.4 compatibility harness: containers, seed, replay and checks.

Evidence class ``local``. Everything runs on loopback ports in disposable
containers whose names start with the run prefix; ``down`` removes exactly
those. Raw transcripts and logs go to an ignored directory (default
``server/scratch/compat/<run>``) because they contain test tokens.

Typical sequence (from ``server/``)::

    py tools/compat/compat_local.py up        # network, PostgreSQL, stub, v1.4.0 on compat_base
    py tools/compat/compat_local.py seed      # v1.4 seed through REST
    py tools/compat/compat_local.py fork      # copy compat_base -> compat_cand, start the candidate
    py tools/compat/compat_local.py replay    # COMPAT-T1 REST replay through both, diff
    py tools/compat/compat_local.py mmctl     # COMPAT-T2 CLI cross-runs
    py tools/compat/compat_local.py console   # COMPAT-T5 console checks
    py tools/compat/compat_local.py t4        # COMPAT-T4 default-off checks and arrival counts
    py tools/compat/compat_local.py t3        # COMPAT-T3 rollback negative control
    py tools/compat/compat_local.py down

Images default to the published v1.4.0 index digest and a local candidate tag;
override with ``--baseline-image`` / ``--candidate-image``.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.cookiejar
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import compat_wire as cw  # noqa: E402

HERE = Path(__file__).resolve().parent
SERVER = HERE.parent.parent
BASELINE_IMAGE = "iokaio/munarium@sha256:800a7b19cd3e2b82a3fb8f0b33e86e3f21fc3ea71ac12a81d6a9a18a816f556a"
CANDIDATE_IMAGE = "munarium-compat-candidate:local"
PG_IMAGE = "pgvector/pgvector:pg16@sha256:ccc6e83d6e35e931dc7c5def2022729d5a6c370318d099181995567ff1fb4d6b"
STUB_IMAGE = "python:3.12-alpine"
PG_PASSWORD = "compat-local-only"
TOKEN_SECRET = "compat-local-hs256-test-key-0001"
TOKENS = {
    "rw": "compat-rw",
    "ro": "compat-ro",
    "mgmt": "compat-mgmt",
    "conf-rw": "conf-rw",
    "conf-mgmt": "conf-mgmt",
    "other": "compat-other-rw",
}
STATIC_TOKENS = ",".join(
    [
        "compat-rw:compat:rw",
        "compat-ro:compat:ro",
        "compat-mgmt:compat:mgmt",
        "compat-other-rw:compat-other:rw",
        "conf-rw:conformance:rw",
        "conf-mgmt:conformance:mgmt",
    ]
)
PORTS = {"base": (28080, 28051, 28090), "cand": (29080, 29051, 29090)}


def minimal_pdf(lines: list[str]) -> bytes:
    """A one-page PDF with a Type1 Helvetica text stream and a correct xref."""
    esc = [ln.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)") for ln in lines]
    text = "BT /F1 12 Tf 72 720 Td " + " 0 -16 Td ".join(f"({ln}) Tj" for ln in esc) + " ET"
    stream = text.encode("latin-1")
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n" + stream + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, obj in enumerate(objects, start=1):
        offsets.append(len(out))
        out += f"{i} 0 obj\n".encode() + obj + b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objects) + 1}\n".encode() + b"0000000000 65535 f \n"
    for off in offsets:
        out += f"{off:010d} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return bytes(out)


PDF_LINES = [
    "ExampleWorks handbook.",
    "Refund requests are reviewed by the refunds desk within five days.",
    "Hardware returns are handled by the logistics team.",
]


# --------------------------------------------------------------------------
# docker helpers
# --------------------------------------------------------------------------


def sh(args: list[str], check: bool = True, capture: bool = True, timeout: float = 600) -> subprocess.CompletedProcess[str]:
    res = subprocess.run(args, capture_output=capture, text=True, encoding="utf-8", errors="replace", timeout=timeout)
    if check and res.returncode != 0:
        raise RuntimeError(f"{' '.join(args)} -> {res.returncode}\n{res.stderr}")
    return res


class Run:
    def __init__(self, args: argparse.Namespace) -> None:
        self.prefix = args.prefix
        self.out = Path(args.out or SERVER / "scratch" / "compat" / args.prefix)
        self.out.mkdir(parents=True, exist_ok=True)
        self.baseline_image = args.baseline_image
        self.candidate_image = args.candidate_image
        self.state_path = self.out / "state.json"
        self.client_python = args.client_python
        self.v14_proto = args.v14_proto
        self.state: dict[str, Any] = cw.load_json(str(self.state_path)) if self.state_path.exists() else {}

    def save(self) -> None:
        cw.dump_json(str(self.state_path), self.state)

    def name(self, part: str) -> str:
        return f"{self.prefix}-{part}"

    def url(self, which: str) -> str:
        return f"http://127.0.0.1:{PORTS[which][0]}"

    def psql(self, sql: str, db: str = "postgres") -> str:
        res = sh(["docker", "exec", self.name("pg"), "psql", "-U", "munarium", "-d", db, "-At", "-c", sql])
        return res.stdout.strip()

    def server_env(self, db: str, instance: str) -> list[str]:
        env = {
            "MUNARIUM_STORE": "postgres",
            "MUNARIUM_SOURCE_STORE": "pg",
            "MUNARIUM_DATABASE_URL": f"postgres://munarium:{PG_PASSWORD}@{self.name('pg')}:5432/{db}",
            "MUNARIUM_AUTH_MODE": "static",
            "MUNARIUM_STATIC_TOKENS": STATIC_TOKENS,
            "MUNARIUM_TOKEN_SECRET": TOKEN_SECRET,
            "MUNARIUM_LOG": "info",
            "MUNARIUM_INSTANCE_ID": instance,
        }
        out: list[str] = []
        for k, v in env.items():
            out += ["-e", f"{k}={v}"]
        return out

    def start_server(self, which: str, image: str, db: str) -> None:
        name = self.name(which)
        sh(["docker", "rm", "-f", name], check=False)
        http, grpc, ops = PORTS[which]
        sh(
            ["docker", "run", "-d", "--name", name, "--network", self.name("net"),
             "-p", f"127.0.0.1:{http}:8080", "-p", f"127.0.0.1:{grpc}:50051", "-p", f"127.0.0.1:{ops}:9090"]
            + self.server_env(db, f"compat-{which}")
            + [image]
        )
        self.wait_ready(which)

    def wait_ready(self, which: str, timeout: float = 120) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                with urllib.request.urlopen(self.url(which) + "/readyz", timeout=5) as r:
                    if r.status == 200:
                        return
            except (urllib.error.URLError, ConnectionError, OSError):
                pass
            time.sleep(1)
        logs = sh(["docker", "logs", "--tail", "40", self.name(which)], check=False)
        raise RuntimeError(f"{which} not ready:\n{logs.stdout}\n{logs.stderr}")

    def image_id(self, image: str) -> dict[str, Any]:
        res = sh(["docker", "image", "inspect", image, "--format", "{{json .}}"], check=False)
        if res.returncode != 0:
            return {"image": image, "error": res.stderr.strip()}
        info = json.loads(res.stdout)
        return {
            "image": image,
            "id": info.get("Id"),
            "repo_digests": info.get("RepoDigests"),
            "labels": (info.get("Config") or {}).get("Labels"),
        }


# --------------------------------------------------------------------------
# steps
# --------------------------------------------------------------------------


def step_up(run: Run) -> None:
    net, pg, stub = run.name("net"), run.name("pg"), run.name("stub")
    for img in (PG_IMAGE, STUB_IMAGE, run.baseline_image):
        sh(["docker", "pull", img], timeout=900)
    sh(["docker", "network", "create", net])
    sh(["docker", "volume", "create", run.name("pgdata")])
    sh(["docker", "run", "-d", "--name", pg, "--network", net,
        "-e", "POSTGRES_USER=munarium", "-e", f"POSTGRES_PASSWORD={PG_PASSWORD}", "-e", "POSTGRES_DB=postgres",
        "-v", f"{run.name('pgdata')}:/var/lib/postgresql/data", PG_IMAGE])
    for _ in range(60):
        if sh(["docker", "exec", pg, "pg_isready", "-U", "munarium"], check=False).returncode == 0:
            break
        time.sleep(1)
    time.sleep(2)
    run.psql("CREATE DATABASE compat_base")
    stub_dir = run.out / "stub"
    stub_dir.mkdir(exist_ok=True)
    sh(["docker", "run", "-d", "--name", stub, "--network", net,
        "-v", f"{HERE}:/tool:ro", "-v", f"{stub_dir.resolve()}:/data",
        STUB_IMAGE, "python", "/tool/count_stub.py", "--port", "11434", "--log", "/data/arrivals.jsonl"])
    run.start_server("base", run.baseline_image, "compat_base")
    run.state["images"] = {
        "baseline": run.image_id(run.baseline_image),
        "candidate": run.image_id(run.candidate_image),
        "postgres": run.image_id(PG_IMAGE),
        "stub": run.image_id(STUB_IMAGE),
    }
    run.state["created"] = {
        "containers": [pg, stub, run.name("base")],
        "network": net,
        "volumes": [run.name("pgdata")],
    }
    run.save()
    print("up: baseline ready at", run.url("base"))


def rest(run: Run, which: str) -> cw.RestTransport:
    return cw.RestTransport(run.url(which), TOKENS)


def step_seed(run: Run) -> None:
    seed = cw.load_json(str(HERE / "cases" / "seed-v14.json"))
    variables: dict[str, Any] = {
        "stub_url": f"http://{run.name('stub')}:11434",
        "pdf_b64": base64.b64encode(minimal_pdf(PDF_LINES)).decode(),
    }
    transcript = cw.run_cases(seed["cases"], rest(run, "base"), variables, print)
    cw.dump_json(str(run.out / "seed-transcript.json"), transcript)
    bad = cw.status_expectations(transcript) + [
        {"case": e["id"], "problem": e.get("skipped") or e.get("transport_error") or e.get("capture_missing")}
        for e in transcript
        if "skipped" in e or "transport_error" in e or "capture_missing" in e
    ]
    stable = set()
    for e in transcript:
        if "response" in e:
            cw.collect_ids(e["response"].get("json"), stable)
    variables.pop("pdf_b64")
    run.state["seed"] = {"variables": variables, "stable_ids": sorted(stable), "problems": bad}
    run.save()
    print(json.dumps(bad, indent=2) if bad else "seed: all expectations met")


def step_reset(run: Run) -> None:
    """Drop both replay databases and restart v1.4.0 on an empty compat_base."""
    for which in ("base", "cand"):
        sh(["docker", "rm", "-f", run.name(which)], check=False)
    for db in ("compat_cand", "compat_base"):
        run.psql(f"DROP DATABASE IF EXISTS {db}")
    run.psql("CREATE DATABASE compat_base")
    stub_log = run.out / "stub" / "arrivals.jsonl"
    if stub_log.exists():
        stub_log.write_text("", encoding="utf-8")
    run.start_server("base", run.baseline_image, "compat_base")
    run.state.pop("seed", None)
    run.save()
    print("reset: empty compat_base, baseline ready")


def stub_lines(run: Run) -> list[str]:
    path = run.out / "stub" / "arrivals.jsonl"
    return path.read_text(encoding="utf-8").splitlines() if path.exists() else []


def step_fork(run: Run) -> None:
    run.state["stub_lines_at_seed"] = len(stub_lines(run))
    sh(["docker", "stop", run.name("base")])
    run.psql("CREATE DATABASE compat_cand TEMPLATE compat_base")
    run.start_server("base", run.baseline_image, "compat_base")
    run.start_server("cand", run.candidate_image, "compat_cand")
    if run.name("cand") not in run.state["created"]["containers"]:
        run.state["created"]["containers"].append(run.name("cand"))
    run.state["images"]["candidate"] = run.image_id(run.candidate_image)
    run.state["migrations"] = {
        db: run.psql("SELECT string_agg(version::text, ',' ORDER BY version) FROM _sqlx_migrations", db)
        for db in ("compat_base", "compat_cand")
    }
    run.save()
    print("fork: candidate ready at", run.url("cand"), run.state["migrations"])


def compare(run: Run, label: str, base: list[dict[str, Any]], cand: list[dict[str, Any]]) -> dict[str, Any]:
    comp = cw.Comparator(stable_ids=set(run.state["seed"]["stable_ids"]))
    findings = comp.compare_transcripts(base, cand)
    summary = cw.summarize(findings)
    summary["cases"] = len(base)
    summary["status_expectation_misses"] = {
        "baseline": cw.status_expectations(base),
        "candidate": cw.status_expectations(cand),
    }
    cw.dump_json(str(run.out / f"{label}-diff.json"), summary)
    print(f"{label}: cases={len(base)} counts={summary['counts']} passed={summary['passed']}")
    return summary


def step_replay(run: Run) -> None:
    cases = cw.load_json(str(HERE / "cases" / "replay-v14.json"))["cases"]
    results = {}
    for which in ("base", "cand"):
        variables = dict(run.state["seed"]["variables"])
        variables["run"] = "rest"
        variables["pdf_b64"] = base64.b64encode(minimal_pdf(PDF_LINES)).decode()
        results[which] = cw.run_cases(cases, rest(run, which), variables, lambda m, w=which: print(f"[{w}] {m}"))
        cw.dump_json(str(run.out / f"replay-rest-{which}.json"), results[which])
    compare(run, "replay-rest", results["base"], results["cand"])
    surface(run)
    python = run.client_python
    if not python or not Path(python).exists():
        print("replay: no released-client Python; bridge and typed gRPC not run")
        return
    pdf_file = run.out / "pdf.b64"
    pdf_file.write_text(base64.b64encode(minimal_pdf(PDF_LINES)).decode(), encoding="ascii")
    proto = run.v14_proto
    for mode in ("bridge", "typed"):
        out = {}
        for which in ("base", "cand"):
            path = run.out / f"replay-{mode}-{which}.json"
            args = [python, "-I", str(HERE / "grpc_driver.py"), "--target", f"127.0.0.1:{PORTS[which][1]}",
                    "--state", str(run.state_path), "--out", str(path), "--mode", mode]
            if mode == "bridge":
                args += ["--cases", str(HERE / "cases" / "replay-v14.json"), "--proto", proto, "--pdf-b64-file", str(pdf_file)]
            res = sh(args, check=False, timeout=1800)
            (run.out / f"replay-{mode}-{which}.log").write_text(res.stdout + res.stderr, encoding="utf-8")
            out[which] = cw.load_json(str(path)) if path.exists() else []
        compare(run, f"replay-{mode}", out["base"], out["cand"])


def step_diff(run: Run) -> None:
    """Recompute every replay comparison from the saved transcripts."""
    for mode in ("rest", "bridge", "typed"):
        paths = [run.out / f"replay-{mode}-{w}.json" for w in ("base", "cand")]
        if all(p.exists() for p in paths):
            compare(run, f"replay-{mode}", cw.load_json(str(paths[0])), cw.load_json(str(paths[1])))


def openapi_surface(doc: Any) -> dict[str, Any]:
    """Paths, methods and declared response codes of an OpenAPI document."""
    out: dict[str, Any] = {}
    for path, item in (doc or {}).get("paths", {}).items():
        for method, op in item.items():
            if method in ("get", "post", "put", "patch", "delete"):
                out[f"{method.upper()} {path}"] = sorted((op.get("responses") or {}).keys())
    return out


def surface(run: Run) -> None:
    docs = {}
    for which in ("base", "cand"):
        with urllib.request.urlopen(run.url(which) + "/openapi.json", timeout=30) as r:
            docs[which] = openapi_surface(json.loads(r.read()))
    base, cand = docs["base"], docs["cand"]
    report = {
        "baseline_operations": len(base),
        "candidate_operations": len(cand),
        "removed": sorted(set(base) - set(cand)),
        "added": sorted(set(cand) - set(base)),
        "response_codes_changed": {k: [base[k], cand[k]] for k in sorted(set(base) & set(cand)) if base[k] != cand[k]},
    }
    cw.dump_json(str(run.out / "openapi-surface.json"), report)
    print("openapi:", {k: (len(v) if isinstance(v, (list, dict)) else v) for k, v in report.items()})


# ---------------------------------------------------------------- mmctl


MMCTL_COMMANDS: list[tuple[str, list[str], str]] = [
    ("usage", [], "rw"),
    ("unknown-verb", ["frobnicate"], "rw"),
    ("runbook-list", ["runbook", "list"], "rw"),
    ("runbook-info", ["runbook", "info", "starter"], "rw"),
    ("runbook-info-pinned", ["runbook", "info", "starter@1"], "rw"),
    ("runbook-info-missing", ["runbook", "info", "no-such-runbook"], "rw"),
    ("get-run", ["get", "run", "${run_id}"], "rw"),
    ("runbook-validate", ["runbook", "validate", "-f", "/cases/mmctl-runbook.yaml"], "rw"),
    ("apply-shape", ["apply", "-f", "/cases/mmctl-shape.yaml"], "rw"),
    ("apply-unknown-kind", ["apply", "-f", "/cases/mmctl-unknown.yaml"], "rw"),
    ("token-issue", ["token", "issue", "cli-user", "1", "query"], "mgmt"),
    ("token-issue-rw-refused", ["token", "issue", "cli-user", "1", "query"], "rw"),
    ("token-issue-bad-level", ["token", "issue", "cli-user", "x", "query"], "mgmt"),
    ("datastore-jobs-list", ["datastore", "jobs", "list"], "mgmt"),
    ("datastore-rollout-get", ["datastore", "rollout", "get", "collection", "${coll_text}"], "mgmt"),
    ("bulk-status-missing", ["bulk", "status", "00000000-0000-0000-0000-000000000000"], "rw"),
    ("author-patterns", ["author", "patterns"], "rw"),
    ("no-token", ["runbook", "list"], "none"),
    ("matrix-version-no-peer", ["matrix", "version"], "rw"),
    ("matrix-journal-no-peer", ["matrix", "journal", "--limit", "1"], "rw"),
]


def run_mmctl(run: Run, cli_image: str, target: str, argv: list[str], token: str) -> dict[str, Any]:
    env = ["-e", f"MUNARIUMCTL_URL=http://{run.name(target)}:8080", "-e", "MUNARIUMCTL_UID=compat-cli",
           "-e", f"MUNARIUMCTL_MATRIX_URL=http://{run.name('nomatrix')}:8180"]
    if token != "none":
        env += ["-e", f"MUNARIUMCTL_TOKEN={TOKENS[token]}"]
    res = sh(["docker", "run", "--rm", "--network", run.name("net"), "-v", f"{HERE / 'cases'}:/cases:ro",
              "--entrypoint", "/mmctl"] + env + [cli_image] + argv, check=False, timeout=300)
    out: dict[str, Any] = {"exit": res.returncode, "stdout_sha256": hashlib.sha256(res.stdout.encode()).hexdigest()}
    try:
        out["json"] = json.loads(res.stdout) if res.stdout.strip() else None
    except ValueError:
        out["stdout"] = res.stdout
    out["stderr"] = redact(res.stderr)
    return out


def redact(text: str) -> str:
    for tok in TOKENS.values():
        text = text.replace(tok, "<token>")
    return re.sub(r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*", "<jwt>", text)


def step_mmctl(run: Run) -> None:
    variables = run.state["seed"]["variables"]
    matrix = {}
    combos = [("v14-cli", run.baseline_image, "base"), ("v14-cli", run.baseline_image, "cand"),
              ("cand-cli", run.candidate_image, "cand"), ("cand-cli", run.candidate_image, "base")]
    for cli, image, target in combos:
        key = f"{cli}->{target}"
        matrix[key] = {}
        for cid, argv, token in MMCTL_COMMANDS:
            resolved = [str(cw.substitute(a, variables)) for a in argv]
            matrix[key][cid] = run_mmctl(run, image, target, resolved, token)
            print(key, cid, matrix[key][cid]["exit"])
    cw.dump_json(str(run.out / "mmctl-matrix.json"), matrix)
    # Pairwise comparisons: the same CLI against both servers, and both CLIs against v1.4.
    report = {}
    for label, left, right in [("v14-cli: base vs cand", "v14-cli->base", "v14-cli->cand"),
                               ("cand-cli vs v14-cli against base", "v14-cli->base", "cand-cli->base"),
                               ("cand-cli vs v14-cli against cand", "v14-cli->cand", "cand-cli->cand")]:
        comp = cw.Comparator(stable_ids=set(run.state["seed"]["stable_ids"]))
        rows = []
        for cid, _argv, _tok in MMCTL_COMMANDS:
            a, b = matrix[left][cid], matrix[right][cid]
            comp.findings = []
            if a["exit"] != b["exit"]:
                comp.findings.append(cw.Finding(cid, "exit", "status", a["exit"], b["exit"]))
            comp.compare_value(cid, "/json", a.get("json"), b.get("json"))
            if "stdout" in a or "stdout" in b:
                if a.get("stdout") != b.get("stdout"):
                    comp.findings.append(cw.Finding(cid, "/stdout", "value_changed", a.get("stdout"), b.get("stdout")))
            if _strip_volatile(a["stderr"]) != _strip_volatile(b["stderr"]):
                comp.findings.append(cw.Finding(cid, "/stderr", "informational", a["stderr"], b["stderr"]))
            rows.append({"command": cid, "exit": [a["exit"], b["exit"]], "findings": [f.as_dict() for f in comp.findings]})
        report[label] = rows
    cw.dump_json(str(run.out / "mmctl-diff.json"), report)
    for label, rows in report.items():
        failing = [r for r in rows if any(f["kind"] not in ("added", "informational") for f in r["findings"])]
        print(label, "failing:", [r["command"] for r in failing])


def _strip_volatile(text: str) -> str:
    text = re.sub(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "<uuid>", text)
    return re.sub(r"\d{4}-\d{2}-\d{2}T[0-9:.]+Z?", "<ts>", text)


# ---------------------------------------------------------------- client suites (COMPAT-T2)


def client_suites(run: Run) -> list[dict[str, Any]]:
    """Each SDK conformance suite, released (published 1.4.0 packages, see
    released_clients.py) and candidate (this checkout's client source)."""
    scratch = SERVER / "scratch" / "compat"
    clients = SERVER.parent / "clients"
    win = os.name == "nt"
    exe = ".exe" if win else ""
    gradlew = "gradlew.bat" if win else "gradlew"
    venv_bin = "Scripts" if win else "bin"
    py14 = scratch / "venv-py14" / venv_bin / f"python{exe}"
    pycand = scratch / "venv-cand" / venv_bin / f"python{exe}"
    pytest = ["-m", "pytest", "conformance", "-q", "-rs", "-p", "no:cacheprovider"]
    rust_args = ["--token", "conf-rw", "--mgmt-token", "conf-mgmt", "--smoke"]
    return [
        {"sdk": "python", "build": "released", "cwd": scratch / "released" / "python", "cmd": [str(py14)] + pytest, "grpc": "bare"},
        {"sdk": "python", "build": "candidate", "cwd": clients / "python", "cmd": [str(pycand)] + pytest, "grpc": "bare"},
        {"sdk": "rust", "build": "released", "cwd": scratch / "released" / "rust",
         "cmd": [str(scratch / "released" / "rust" / "target" / "debug" / f"munarium-client-conformance{exe}")], "args": rust_args},
        {"sdk": "rust", "build": "candidate", "cwd": clients / "rust",
         "cmd": [str(clients / "rust" / "target" / "debug" / f"munarium-client-conformance{exe}")], "args": rust_args},
        {"sdk": "dotnet", "build": "released", "cwd": scratch / "released" / "dotnet" / "Conformance", "cmd": ["dotnet", "test", "--no-build", "-v", "q"], "grpc": "http"},
        {"sdk": "dotnet", "build": "candidate", "cwd": clients / "dotnet", "cmd": ["dotnet", "test", "tests/Ioka.Munarium.Client.Conformance", "--no-build", "-v", "q"], "grpc": "http"},
        {"sdk": "java", "build": "released", "cwd": scratch / "released" / "java",
         "cmd": [str(scratch / "released" / "java" / gradlew), "--no-daemon", "-q", "conformanceTest"], "grpc": "bare", "junit": True},
        {"sdk": "java", "build": "candidate", "cwd": clients / "java",
         "cmd": [str(clients / "java" / gradlew), "--no-daemon", "-q", "conformanceTest"], "grpc": "bare", "junit": True},
    ]


def junit_summary(directory: Path) -> dict[str, Any]:
    import xml.etree.ElementTree as ET

    total = {"tests": 0, "failures": 0, "errors": 0, "skipped": 0}
    for f in sorted(directory.glob("*.xml")):
        root = ET.parse(f).getroot()
        for k in total:
            total[k] += int(root.get(k, 0))
    return total


def summary_line(sdk: str, text: str) -> str:
    patterns = {
        "python": r"^\d+ (passed|failed).*$|^=+ .*(passed|failed).* =+$",
        "rust": r"^.*(scenarios|smokes).*failed.*$",
        "dotnet": r"^(Passed|Failed)!.*$",
    }
    if sdk not in patterns:
        return ""
    lines = [ln.strip() for ln in text.splitlines() if re.search(patterns[sdk], ln.strip())]
    return " | ".join(lines[-4:])


def step_clients(run: Run) -> None:
    results = []
    for target in ("cand", "base"):
        http, grpc_port, _ = PORTS[target]
        for suite in client_suites(run):
            env = dict(os.environ, MUNARIUM_REST_URL=f"http://127.0.0.1:{http}", MUNARIUM_TOKEN="conf-rw", MUNARIUM_MGMT_TOKEN="conf-mgmt",
                       MUNARIUM_GRPC_URL=(f"http://127.0.0.1:{grpc_port}" if suite.get("grpc") == "http" else f"127.0.0.1:{grpc_port}"))
            cmd = list(suite["cmd"])
            if suite["sdk"] == "rust":
                cmd += ["--rest", f"http://127.0.0.1:{http}", "--grpc", f"http://127.0.0.1:{grpc_port}"] + suite["args"]
            if not Path(cmd[0]).exists() and suite["sdk"] in ("python", "rust"):
                results.append({"target": target, "sdk": suite["sdk"], "build": suite["build"], "outcome": "not_run", "reason": f"missing {cmd[0]}"})
                continue
            started = time.time()
            res = subprocess.run(cmd, cwd=suite["cwd"], env=env, capture_output=True, text=True, encoding="utf-8",
                                 errors="replace", timeout=3600, shell=(os.name == "nt" and suite["sdk"] == "java"))
            log = run.out / f"t2-{suite['sdk']}-{suite['build']}-{target}.log"
            log.write_text(redact(res.stdout + res.stderr), encoding="utf-8")
            row = {"target": target, "sdk": suite["sdk"], "build": suite["build"], "exit": res.returncode,
                   "seconds": round(time.time() - started, 1), "summary": summary_line(suite["sdk"], res.stdout + res.stderr)}
            if suite.get("junit"):
                row["junit"] = junit_summary(Path(suite["cwd"]) / "build" / "test-results" / "conformanceTest")
                row["summary"] = json.dumps(row["junit"])
            row["outcome"] = "passed" if res.returncode == 0 else "failed"
            results.append(row)
            print(target, suite["sdk"], suite["build"], row["exit"], row.get("summary") or row.get("junit"))
    cw.dump_json(str(run.out / "t2-clients.json"), results)
    # Which catalog operations each binary serves over the native bridge (v1.4 export and checkout catalogs).
    v14_catalog = Path(run.v14_proto).parents[4] / "clients" / "server-api.json"
    for which in ("base", "cand"):
        sh([run.client_python, "-I", str(HERE / "grpc_driver.py"), "--mode", "catalog", "--state", "-",
            "--target", f"127.0.0.1:{PORTS[which][1]}", "--out", str(run.out / f"catalog-{which}.json"),
            "--catalog", str(v14_catalog), "--catalog", str(SERVER.parent / "clients" / "server-api.json")], check=False)


# ---------------------------------------------------------------- console


class Browser:
    def __init__(self, base: str) -> None:
        self.base = base
        self.jar = http.cookiejar.CookieJar()

        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *a: Any, **k: Any) -> None:
                return None

        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar), NoRedirect)

    def request(self, method: str, path: str, form: dict[str, str] | None = None, headers: dict[str, str] | None = None) -> dict[str, Any]:
        data = urllib.parse.urlencode(form).encode() if form is not None else None
        req = urllib.request.Request(self.base + path, data=data, method=method, headers=headers or {})
        if data is not None:
            req.add_header("Content-Type", "application/x-www-form-urlencoded")
        try:
            with self.opener.open(req, timeout=60) as r:
                body, status, hdrs = r.read(), r.status, r.headers
        except urllib.error.HTTPError as e:
            body, status, hdrs = e.read(), e.code, e.headers
        text = body.decode("utf-8", "replace")
        return {
            "status": status,
            "location": hdrs.get("Location"),
            "set_cookie": bool(hdrs.get("Set-Cookie")),
            "text": text,
        }


CONSOLE_PAGES = [
    "/admin", "/admin/traffic", "/admin/endpoints", "/admin/usage", "/admin/health",
    "/admin/runbooks", "/admin/runbooks/starter", "/admin/shapes/starter-documents@1",
    "/admin/runs/${run_id}", "/admin/providers", "/admin/collections", "/admin/collections/${coll_text}",
    "/admin/storage", "/admin/sessions", "/admin/sessions/${session_id}", "/admin/tokens",
    "/admin/audit", "/admin/findings", "/admin/matrix", "/admin/login",
]
CSRF_RE = re.compile(r'name="_csrf" value="([0-9a-f]+)"')


def page_facts(resp: dict[str, Any]) -> dict[str, Any]:
    text = resp["text"]
    title = re.search(r"<title>(.*?)</title>", text, re.S)
    return {
        "status": resp["status"],
        "location": resp["location"],
        "title": title.group(1).strip() if title else None,
        "has_csrf_field": 'name="_csrf"' in text,
        "view_only_badge": "view-only</span>" in text,
        "forms": sorted(set(re.findall(r'<form[^>]*action="([^"]+)"', text))),
        "bytes": len(text),
    }


def console_run(run: Run, which: str) -> dict[str, Any]:
    v = run.state["seed"]["variables"]
    base = run.url(which)
    out: dict[str, Any] = {}
    anon = Browser(base)
    out["anonymous"] = {p: page_facts(anon.request("GET", str(cw.substitute(p, v)))) for p in ["/admin", "/admin/tokens"]}
    bearer = Browser(base)
    auth = {"Authorization": f"Bearer {TOKENS['mgmt']}"}
    out["bearer_pages"] = {p: page_facts(bearer.request("GET", str(cw.substitute(p, v)), headers=auth)) for p in CONSOLE_PAGES}
    out["rw_bearer_admin"] = page_facts(bearer.request("GET", "/admin", headers={"Authorization": f"Bearer {TOKENS['rw']}"}))
    view_only = dict(auth, **{"x-munarium-admin-view-only": "true"})
    out["view_only_pages"] = {p: page_facts(bearer.request("GET", str(cw.substitute(p, v)), headers=view_only))
                              for p in ["/admin/tokens", "/admin/runs/${run_id}"]}
    # login (bookmark flow): wrong token, rw token, mgmt token, then cookie-only browsing
    b = Browser(base)
    out["login_bad"] = page_facts(b.request("POST", "/admin/login", form={"token": "not-a-token"}))
    out["login_rw"] = page_facts(b.request("POST", "/admin/login", form={"token": TOKENS["rw"]}))
    login = b.request("POST", "/admin/login", form={"token": TOKENS["mgmt"]})
    out["login_mgmt"] = page_facts(login)
    out["cookie_names"] = sorted(c.name for c in b.jar)
    out["cookie_pages"] = {p: page_facts(b.request("GET", str(cw.substitute(p, v)))) for p in ["/admin", "/admin/tokens", "/admin/runbooks"]}
    tokens_page = b.request("GET", "/admin/tokens")
    m = CSRF_RE.search(tokens_page["text"])
    out["issue_form_on_tokens_page"] = bool(m)
    if not m:  # ClearView U8 moved the issue form to GET /admin/tokens/issue
        issue_page = b.request("GET", "/admin/tokens/issue")
        out["issue_page_get"] = page_facts(issue_page)
        m = CSRF_RE.search(issue_page["text"])
    csrf = m.group(1) if m else ""
    out["csrf_present"] = bool(csrf)
    # CSRF: issue without token, with wrong token, with the page's token
    issue = {"uid": "console-user", "access_level": "1", "scope_query": "1"}
    out["issue_no_csrf"] = page_facts(b.request("POST", "/admin/tokens/issue", form=issue))
    out["issue_bad_csrf"] = page_facts(b.request("POST", "/admin/tokens/issue", form=dict(issue, _csrf="0" * 64)))
    issued = b.request("POST", "/admin/tokens/issue", form=dict(issue, _csrf=csrf))
    out["issue_good_csrf"] = page_facts(issued)
    out["issue_good_csrf"]["minted_jwt"] = bool(re.search(r"eyJ[A-Za-z0-9_-]+\.", issued["text"]))
    # Revoke: v1.4 revokes on POST; ClearView U4 answers a review page (known delta).
    minted = rest_call(run, which, "POST", "/v1/access-tokens", "mgmt",
                       {"uid": "console-revoke", "access_level": 0, "scopes": ["query"], "compartments": []})
    jti = (minted.get("json") or {}).get("jti", v["revoke_jti"])
    rv = b.request("POST", f"/admin/tokens/{jti}/revoke", form={"_csrf": csrf})
    out["revoke_post"] = page_facts(rv)
    out["revoke_post"]["review_page"] = "confirm" in rv["text"].lower() and rv["status"] == 200
    out["revoke_state_after_post"] = token_state(run, which, jti)
    if out["revoke_post"]["review_page"]:
        fields = confirm_form(rv["text"])
        out["revoke_confirm_form"] = sorted(fields.get("fields", {})) if fields else None
        if fields:
            done = b.request("POST", fields["action"], form=dict(fields["fields"], confirm=jti))
            out["revoke_confirm"] = page_facts(done)
            out["revoke_state_after_confirm"] = token_state(run, which, jti)
    rv_bad = b.request("POST", f"/admin/tokens/{jti}/revoke", form={"_csrf": "0" * 64})
    out["revoke_bad_csrf"] = page_facts(rv_bad)
    # Approve a pending cutover: v1.4 approves on POST; ClearView answers a review page.
    pending = start_pending_run(run, which)
    out["approve_pending_run"] = {k: pending.get(k) for k in ("state", "found_step")}
    if pending.get("ordinal") is not None:
        url = f"/admin/runs/{pending['run_id']}/steps/{pending['ordinal']}/approve"
        ap = b.request("POST", url, form={"_csrf": csrf, "rw_token": TOKENS["rw"]})
        out["approve_post"] = page_facts(ap)
        out["approve_post"]["review_page"] = ap["status"] == 200 and "confirm" in ap["text"].lower()
        out["approve_step_after_post"] = step_state(run, which, pending["run_id"], pending["ordinal"])
        ap_bad = b.request("POST", url, form={"_csrf": "0" * 64, "rw_token": TOKENS["rw"]})
        out["approve_bad_csrf"] = page_facts(ap_bad)
        if out["approve_post"]["review_page"]:
            fields = confirm_form(ap["text"])
            out["approve_confirm_form"] = sorted(fields.get("fields", {})) if fields else None
            if fields:
                done = b.request("POST", fields["action"], form=dict(fields["fields"], confirm=pending["run_id"], rw_token=TOKENS["rw"]))
                out["approve_confirm"] = page_facts(done)
                out["approve_step_after_confirm"] = step_state(run, which, pending["run_id"], pending["ordinal"])
    # A completed step cannot be approved again through the console on either binary.
    ap_done = b.request("POST", f"/admin/runs/{v['run_id']}/steps/{v['cutover_text']}/approve", form={"_csrf": csrf, "rw_token": TOKENS["rw"]})
    out["approve_post_done_step"] = page_facts(ap_done)
    out["done_step_after_post"] = step_state(run, which, v["run_id"], v["cutover_text"])
    # The supported automation path for the same actions (REST) is unchanged.
    out["api_revoke"] = rest_call(run, which, "POST", f"/v1/access-tokens/{jti}/revoke", "mgmt")
    out["revoke_state_after_api"] = token_state(run, which, jti)
    return out


FORM_RE = re.compile(r'<form[^>]*action="([^"]+/confirm)"[^>]*>(.*?)</form>', re.S)
HIDDEN_RE = re.compile(r'<input[^>]*type="hidden"[^>]*name="([^"]+)"[^>]*value="([^"]*)"')


def confirm_form(html: str) -> dict[str, Any] | None:
    """The action and hidden fields of a review page's confirmation form."""
    m = FORM_RE.search(html)
    if not m:
        return None
    fields = {name: value.replace("&amp;", "&") for name, value in HIDDEN_RE.findall(m.group(2))}
    return {"action": m.group(1).replace("&amp;", "&"), "fields": fields}


def start_pending_run(run: Run, which: str) -> dict[str, Any]:
    started = rest_call(run, which, "POST", "/v1/runbooks/starter/runs", "rw")
    run_id = (started.get("json") or {}).get("run_id")
    out: dict[str, Any] = {"run_id": run_id}
    for _ in range(120):
        r = rest_call(run, which, "GET", f"/v1/runs/{run_id}", "rw")
        body = r.get("json") or {}
        if body.get("state") in ("awaiting_approval", "done", "failed"):
            break
        time.sleep(1)
    out["state"] = body.get("state")
    for step in body.get("steps", []):
        if step.get("state") == "awaiting_approval":
            out["ordinal"], out["found_step"] = step["ordinal"], step["name"]
            break
    return out


def step_state(run: Run, which: str, run_id: str, ordinal: Any) -> Any:
    body = rest_call(run, which, "GET", f"/v1/runs/{run_id}", "rw").get("json") or {}
    for step in body.get("steps", []):
        if str(step.get("ordinal")) == str(ordinal):
            return step.get("state")
    return None


def token_state(run: Run, which: str, jti: str) -> Any:
    r = rest_call(run, which, "GET", "/v1/access-tokens", "mgmt")
    for t in (r.get("json") or {}).get("tokens", []) if isinstance(r.get("json"), dict) else []:
        if t.get("jti") == jti:
            return {k: t.get(k) for k in ("revoked", "revoked_at", "status") if k in t}
    return {"listed": False, "status": r["status"]}


def rest_call(run: Run, which: str, method: str, path: str, token: str, body: Any = None) -> dict[str, Any]:
    t = rest(run, which)
    req = {"method": method, "path": path, "query": {}, "headers": {}, "token": token, "uid": "compat-user",
           "content_type": "application/json" if body is not None else None,
           "body": json.dumps(body).encode() if body is not None else None}
    return t.call(req)


def step_console(run: Run) -> None:
    res = {which: console_run(run, which) for which in ("base", "cand")}
    cw.dump_json(str(run.out / "console.json"), res)
    comp = cw.Comparator()
    comp.compare_value("console", "", _console_view(res["base"]), _console_view(res["cand"]))
    summary = cw.summarize(comp.findings)
    cw.dump_json(str(run.out / "console-diff.json"), summary)
    print("console:", summary["counts"])
    for f in summary["failing"]:
        print("  ", f["path"], f["baseline"], "->", f["candidate"])


def _console_view(r: dict[str, Any]) -> dict[str, Any]:
    """Drop byte counts (pages gain navigation entries) before comparison."""
    def strip(x: Any) -> Any:
        if isinstance(x, dict):
            return {k: strip(v) for k, v in x.items() if k not in ("bytes",)}
        return x
    return strip(r)


# ---------------------------------------------------------------- COMPAT-T4


NEW_TABLES = {
    "mcp": ["mcp_invocations", "mcp_operations", "mcp_token_attempts", "mcp_bound_violations"],
    "inference": None,  # discovered by prefix
    "artifact damage": ["artifact_integrity_pins", "artifact_integrity_candidates", "artifact_integrity_incidents",
                        "artifact_repair_attempts", "artifact_integrity_observations"],
    "console sessions": ["console_sessions"],
}


def step_t4(run: Run) -> None:
    out: dict[str, Any] = {}
    tables = run.psql("SELECT string_agg(tablename, ',' ORDER BY tablename) FROM pg_tables WHERE schemaname='public'", "compat_cand").split(",")
    base_tables = set(run.psql("SELECT string_agg(tablename, ',' ORDER BY tablename) FROM pg_tables WHERE schemaname='public'", "compat_base").split(","))
    new = [t for t in tables if t not in base_tables]
    out["new_tables_rowcounts"] = {t: int(run.psql(f'SELECT count(*) FROM "{t}"', "compat_cand")) for t in new}
    for which in ("base", "cand"):
        r = {}
        for method, path in (("GET", "/mcp"), ("POST", "/mcp"), ("GET", "/.well-known/mcp")):
            resp = rest_call(run, which, method, path, "rw", {} if method == "POST" else None)
            r[f"{method} {path}"] = resp["status"]
        out[f"mcp_routes_{which}"] = r
        logs = sh(["docker", "logs", run.name(which)], check=False)
        text = logs.stdout + logs.stderr
        out[f"startup_feature_lines_{which}"] = sorted(
            {ln.strip()[:200] for ln in text.splitlines()
             if re.search(r"(?i)mcp|inference|isolated|artifact damage|console session|tenant_fair|control reserve|fair", ln)}
        )
        out[f"outbound_errors_{which}"] = sorted({ln.strip()[:200] for ln in text.splitlines() if re.search(r"(?i)connect(ion)? (refused|error)|dns|resolve", ln)})
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{PORTS[which][2]}/metrics", timeout=10) as m:
                metrics = m.read().decode()
        except (urllib.error.URLError, OSError) as e:
            metrics = f"error: {e}"
        cw_lines = [ln for ln in metrics.splitlines() if ln and not ln.startswith("#")]
        out[f"metric_names_{which}"] = sorted({ln.split("{")[0].split(" ")[0] for ln in cw_lines})
        out[f"provider_metrics_{which}"] = [ln for ln in cw_lines if ln.startswith(("munarium_provider_calls", "munarium_provider_tokens"))
                                            or "inference" in ln.split("{")[0] or "mcp" in ln.split("{")[0]]
        out[f"admission_metrics_{which}"] = [ln for ln in cw_lines if ln.startswith("munarium_admission_slots")]
    all_lines = stub_lines(run)
    out["stub_arrivals_during_seed"] = all_lines[: run.state.get("stub_lines_at_seed", 0)]
    arrivals = all_lines[run.state.get("stub_lines_at_seed", 0):]
    ips = {}
    for which in ("base", "cand"):
        res = sh(["docker", "inspect", run.name(which), "--format", "{{json .NetworkSettings.Networks}}"], check=False)
        try:
            ips[json.loads(res.stdout)[run.name("net")]["IPAddress"]] = which
        except (ValueError, KeyError):
            pass
    tally = {}
    for line in arrivals:
        rec = json.loads(line)
        who = ips.get(rec["client"], rec["client"])
        key = f"{rec['method']} {rec['path']}"
        tally.setdefault(who, {})[key] = tally.setdefault(who, {}).get(key, 0) + 1
    out["stub_arrivals_by_server"] = tally
    out["stub_arrivals_total"] = len(arrivals)
    out["candidate_ip_map"] = ips
    out["index_identity_and_ordering"] = index_identity(run)
    # Queue a build job with no builder configured: v1.4 leaves it pending; the candidate's
    # tenant-turn trigger records the tenant but the default FIFO claim never reads it.
    images = {"base": run.baseline_image, "cand": run.candidate_image}
    coll = run.state["seed"]["variables"]["coll_text"]
    for which in ("base", "cand"):
        out[f"enqueue_{which}"] = run_mmctl(run, images[which], which, ["datastore", "jobs", "enqueue", "backfill", coll], "rw")["exit"]
    time.sleep(10)
    for which, db in (("base", "compat_base"), ("cand", "compat_cand")):
        out[f"jobs_after_enqueue_{which}"] = run.psql("SELECT string_agg(kind || ':' || state || ':' || attempts, ',') FROM index_build_jobs", db)
    out["tenant_turns_cand"] = run.psql(
        "SELECT string_agg(tenant_id || ':' || coalesce(last_served_seq::text, 'never'), ',') FROM index_build_tenant_turns", "compat_cand")
    cw.dump_json(str(run.out / "t4.json"), out)
    print(json.dumps({k: v for k, v in out.items() if not k.startswith("metric_names")}, indent=2)[:6000])


def find_lists(value: Any, key: str) -> list[Any]:
    """Every list stored under ``key`` anywhere in a JSON value, in document order."""
    found: list[Any] = []
    if isinstance(value, dict):
        for k, v in value.items():
            if k == key and isinstance(v, list):
                found.append(v)
            else:
                found.extend(find_lists(v, key))
    elif isinstance(value, list):
        for v in value:
            found.extend(find_lists(v, key))
    return found


ORDERING_CASES = ["retrieval-search-text", "retrieval-search-pdf", "retrieval-search-pinned-index", "session-turn",
                  "session-turn-topk", "rebuild-search-text", "rebuild-search-pdf", "rebuild-search-old-index"]


def index_identity(run: Run) -> dict[str, Any]:
    """Index identities and hit ordering from the REST replay (COMPAT-T4)."""
    views = {}
    for which in ("base", "cand"):
        path = run.out / f"replay-rest-{which}.json"
        if not path.exists():
            return {"error": "run the replay step first"}
        by_id = {e["id"]: e for e in cw.load_json(str(path))}

        def active(cid: str) -> dict[str, Any]:
            body = (by_id.get(cid, {}).get("response") or {}).get("json") or {}
            return {c["name"]: c.get("active_index") for c in body.get("collections", [])}

        def hits(cid: str) -> list[Any]:
            body = (by_id.get(cid, {}).get("response") or {}).get("json")
            lists = find_lists(body, "hits")
            return [[h.get("chunk_id"), h.get("score")] for h in (lists[0] if lists else [])]

        views[which] = {
            "seeded_active_index": active("collections-list"),
            "rebuilt_active_index": active("rebuild-collections"),
            "hit_order": {cid: hits(cid) for cid in ORDERING_CASES},
        }
    b, c = views["base"], views["cand"]
    return {
        "views": views,
        "seeded_index_ids_equal": b["seeded_active_index"] == c["seeded_active_index"],
        "rebuilt_index_ids_equal": b["rebuilt_active_index"] == c["rebuilt_active_index"],
        "hit_order_equal": {cid: b["hit_order"][cid] == c["hit_order"][cid] for cid in ORDERING_CASES},
    }


# ---------------------------------------------------------------- COMPAT-T3


def step_t3(run: Run) -> None:
    """Start the v1.4.0 binary against the candidate-migrated database."""
    sh(["docker", "stop", run.name("cand")], check=False)
    name = run.name("t3")
    sh(["docker", "rm", "-f", name], check=False)
    sh(["docker", "run", "-d", "--name", name, "--network", run.name("net")]
       + run.server_env("compat_cand", "compat-t3") + [run.baseline_image])
    time.sleep(15)
    insp = json.loads(sh(["docker", "inspect", name, "--format", "{{json .State}}"]).stdout)
    logs = sh(["docker", "logs", name], check=False)
    text = re.sub(r"\x1b\[[0-9;]*m", "", redact(logs.stdout + logs.stderr))
    out = {
        "state": {k: insp.get(k) for k in ("Status", "Running", "ExitCode", "Error")},
        "log_tail": text.splitlines()[-15:],
        "version_missing": "VersionMissing" in text or "was previously applied but is missing" in text,
        "migrations_in_db": run.psql("SELECT string_agg(version::text, ',' ORDER BY version) FROM _sqlx_migrations", "compat_cand"),
    }
    sh(["docker", "rm", "-f", name], check=False)
    sh(["docker", "start", run.name("cand")], check=False)
    run.wait_ready("cand")
    cw.dump_json(str(run.out / "t3.json"), out)
    print(json.dumps(out, indent=2))


def step_down(run: Run) -> None:
    created = run.state.get("created", {})
    for c in created.get("containers", []) + [run.name("t3")]:
        sh(["docker", "rm", "-f", "-v", c], check=False)
    if created.get("network"):
        sh(["docker", "network", "rm", created["network"]], check=False)
    for v in created.get("volumes", []):
        sh(["docker", "volume", "rm", v], check=False)
    left = sh(["docker", "ps", "-a", "--filter", f"name={run.prefix}-", "--format", "{{.Names}}"], check=False).stdout.split()
    print("down: remaining containers with prefix:", left)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("step", choices=["up", "reset", "seed", "fork", "replay", "diff", "mmctl", "clients", "console", "t4", "t3", "down"])
    ap.add_argument("--prefix", default="mcompat")
    ap.add_argument("--out")
    ap.add_argument("--baseline-image", default=BASELINE_IMAGE)
    ap.add_argument("--client-python", default=str(SERVER / "scratch" / "compat" / "venv-py14" / "Scripts" / "python.exe"),
                    help="Python with the published munarium-client 1.4.0 installed (for gRPC replay)")
    ap.add_argument("--v14-proto", default=str(SERVER / "scratch" / "compat" / "src-v14" / "server" / "proto" / "mmp" / "v1" / "server_api.proto"),
                    help="server_api.proto exported from v1.4.0, used to route native-bridge calls")
    ap.add_argument("--candidate-image", default=os.environ.get("MUNARIUM_COMPAT_CANDIDATE", CANDIDATE_IMAGE))
    args = ap.parse_args()
    run = Run(args)
    {"up": step_up, "reset": step_reset, "seed": step_seed, "fork": step_fork, "replay": step_replay, "diff": step_diff, "mmctl": step_mmctl, "clients": step_clients,
     "console": step_console, "t4": step_t4, "t3": step_t3, "down": step_down}[args.step](run)


if __name__ == "__main__":
    main()
