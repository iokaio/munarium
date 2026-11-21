# SPDX-License-Identifier: Apache-2.0
"""I3 retrieval-quality evaluation: documentation and source code, four arms each.

CLI:
  i3_eval.py freeze --binary SERVER --output DIR [--purpose pilot] [--thresholds FILE] ...
  i3_eval.py run --manifest MANIFEST --binary SERVER --lens-url URL [--lens-binary LENS]
  i3_eval.py verify DIRECTORY

`freeze` writes a content-addressed manifest before anything runs: source and
binary identity, each family's corpus file list and hashes, the query set and
judgments, the arms, k values, the serving profile (models, bundles,
instructions, serving parameters, hardware) and the thresholds, or an explicit
statement that none were declared. `run` executes that manifest against an
owned, loopback-only PostgreSQL container and a child release Server, through
the real Server APIs, using an already running inference endpoint. It writes
raw outcomes, one receipt per family and the I3-T1 two-family verdict. `verify`
recomputes every receipt and verdict from the manifest and raw record.

Standard library only. The answer keys (the fixture, this runner, its tests and
the guide that describes it) are refused as corpus inputs.
"""

from __future__ import annotations

import argparse
import base64
import fnmatch
import json
import math
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from datetime import datetime, timezone
from pathlib import Path

import frozen_eval as ev
from p13_live import host_identity, sources as server_sources
from p13_local import CallError, Client, binary_hash, unused_port

ROOT = ev.ROOT
require = ev.require
FIXTURE = "scripts/i3_pilot_fixture.json"
# Answer keys and the files that describe them never enter a corpus upload.
ANSWER_KEYS = (
    FIXTURE,
    "scripts/i3_eval.py",
    "scripts/test_i3_eval.py",
    "server/docs/guides/frozen-evaluation.md",
)
FAMILIES = ("docs", "code")
ARMS = ("hash", "embedder", "reranker", "combined")
# Execution order: the reranker arm runs before any semantic index exists, and
# the combined arm reuses the embedder arm's index (same semantic space).
ARM_ORDER = ("hash", "reranker", "embedder", "combined")
ARM_STAGES = {
    "hash": (),
    "embedder": ("embedding",),
    "reranker": ("reranking",),
    "combined": ("embedding", "reranking"),
}
PG_IMAGE = (
    "pgvector/pgvector:pg16"
    "@sha256:ccc6e83d6e35e931dc7c5def2022729d5a6c370318d099181995567ff1fb4d6b"
)
PURPOSES = ("pilot", "diagnostic", "qualification")
RESTRICTED_MARKER = "I3-RESTRICTED-CANARY"
KIND_MANIFEST, KIND_RAW, KIND_RECEIPT, KIND_VERDICT = (
    "i3_manifest",
    "i3_raw",
    "i3_receipt",
    "i3_verdict",
)
THRESHOLD_FIELDS = (
    "k",
    "min_recall",
    "min_ndcg",
    "max_p95_ms",
    "max_failures",
    "max_unauthorized_disclosures",
)
CPU_P95_LIMIT_MS = {"embedder": 5_000, "reranker": 45_000, "combined": 45_000}
QUALIFICATION_THRESHOLDS = {
    "k": 10,
    "min_recall": 0.90,
    "min_ndcg": 0.85,
    "min_ndcg_gain_over_hash": 0.03,
    "max_p95_ms": CPU_P95_LIMIT_MS["embedder"],
    "max_failures": 0,
    "max_unauthorized_disclosures": 0,
}
LENS_BUNDLES = {
    "repository": "iokaio/munarium-lens-models",
    "revision": "cb8d6d3f4da7b0f5eacdbde4a61b5000f9d57cb1",
    "embedding": {
        "name": "qwen3-embedding-0.6b-f32-1024d",
        "manifest_sha256": "1606785dd4138863591094eeff51e165384c1933981400eebb2085cb58f34b95",
    },
    "reranking": {
        "name": "qwen3-reranker-0.6b-f32",
        "manifest_sha256": "303d88c2d49ad84a11ca392d22163d64aa7890c0ec4289f296bb9fe44a956f36",
    },
}
QWEN3_QUERY_INSTRUCTION = (
    "Instruct: Given a web search query, retrieve relevant passages that answer the query\n"
    "Query:"
)
QWEN3_RERANK_INSTRUCTION = (
    "Given a web search query, retrieve relevant passages that answer the query"
)
EXIT = {"passed": 0, "supported": 0, "failed": 1, "incomplete": 3}


def now():
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def log(message):
    print(f"[i3 {time.strftime('%H:%M:%S')}] {message}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------------------
# metrics
# ---------------------------------------------------------------------------


def ranked_files(hits, prefix):
    """Distinct repository paths in first-occurrence order (file-level ranking)."""
    seen, out = set(), []
    for hit in hits:
        path = hit["source_path"]
        if not path.startswith(prefix):
            path = "\0" + path  # never matches a judgment
        else:
            path = path[len(prefix) :]
        if path not in seen:
            seen.add(path)
            out.append(path)
    return out


def recall_at_k(ranked, relevant, k):
    require(type(k) is int and k > 0, "invalid_k")
    require(isinstance(relevant, dict) and relevant, "empty_judgments")
    found = sum(1 for path in ranked[:k] if relevant.get(path, 0) > 0)
    return found / sum(1 for grade in relevant.values() if grade > 0)


def ndcg_at_k(ranked, relevant, k):
    """Graded nDCG with gain 2^grade - 1 and a log2(rank + 1) discount."""
    require(type(k) is int and k > 0, "invalid_k")
    require(
        isinstance(relevant, dict)
        and relevant
        and all(type(g) is int and g >= 0 for g in relevant.values()),
        "invalid_judgments",
    )
    dcg = sum(
        (2 ** relevant.get(path, 0) - 1) / math.log2(rank + 2)
        for rank, path in enumerate(ranked[:k])
    )
    ideal = sorted(relevant.values(), reverse=True)[:k]
    idcg = sum((2**g - 1) / math.log2(rank + 2) for rank, g in enumerate(ideal))
    require(idcg > 0, "no_relevant_judgment")
    return dcg / idcg


def nearest_rank(values, quantile):
    """Nearest-rank percentile, the same rule as frozen_eval.latency_report."""
    require(0 < quantile <= 1, "invalid_quantile")
    ordered = sorted(values)
    return ordered[math.ceil(len(ordered) * quantile) - 1] if ordered else None


# ---------------------------------------------------------------------------
# corpus selection and answer-key exclusion
# ---------------------------------------------------------------------------


def glob_match(path, pattern):
    """Segment-wise glob: `*` stays within a segment, `**` spans segments."""

    def match(parts, pats):
        if not pats:
            return not parts
        if pats[0] == "**":
            return any(match(parts[i:], pats[1:]) for i in range(len(parts) + 1))
        return (
            bool(parts)
            and fnmatch.fnmatchcase(parts[0], pats[0])
            and match(parts[1:], pats[1:])
        )

    return match(path.split("/"), pattern.split("/"))


def assert_no_answer_keys(paths):
    for path in paths:
        require(
            path not in ANSWER_KEYS and not Path(path).name.startswith("i3_"),
            "answer_key_in_corpus:" + path,
        )


def tracked_files(root=ROOT):
    """Tracked files only: ignored local material can never be selected."""
    out = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        capture_output=True,
        check=True,
    ).stdout.decode("utf-8")
    return sorted(p for p in out.split("\0") if p)


def select_corpus(spec, tracked):
    files = sorted(
        p
        for p in tracked
        if any(glob_match(p, g) for g in spec["include"])
        and not any(glob_match(p, g) for g in spec.get("exclude", []))
    )
    require(bool(files), "empty_corpus")
    assert_no_answer_keys(files)
    return files


def corpus_bytes(path, root=ROOT):
    # The same CRLF normalization as frozen_eval.source_identity, so the
    # uploaded bytes are the hashed bytes on every checkout.
    return (Path(root) / path).read_bytes().replace(b"\r\n", b"\n")


def corpus_entries(files, root=ROOT):
    return [
        {
            "path": p,
            "sha256": ev.source_identity(Path(root) / p),
            "bytes": len(corpus_bytes(p, root)),
        }
        for p in files
    ]


def upload_plan(manifest, family):
    """Every (filename, path, restricted) the run uploads, after the key check."""
    spec = manifest["families"][family]
    paths = [e["path"] for e in spec["files"]]
    assert_no_answer_keys(paths)
    require(manifest.get("fixture") not in paths, "answer_key_in_corpus:fixture")
    require(set(spec["restricted"]) <= set(paths), "restricted_not_in_corpus")
    plan = [(f"{family}/{p}", p, False) for p in paths]
    plan += [(f"{family}-restricted/{p}", p, True) for p in spec["restricted"]]
    for _, path, _ in plan:
        assert_no_answer_keys([path])
    return plan


# ---------------------------------------------------------------------------
# manifest
# ---------------------------------------------------------------------------


def total_memory_bytes():
    if os.name == "nt":
        import ctypes

        class Status(ctypes.Structure):
            _fields_ = [
                ("dwLength", ctypes.c_ulong),
                ("dwMemoryLoad", ctypes.c_ulong),
                ("ullTotalPhys", ctypes.c_ulonglong),
                ("ullAvailPhys", ctypes.c_ulonglong),
                ("ullTotalPageFile", ctypes.c_ulonglong),
                ("ullAvailPageFile", ctypes.c_ulonglong),
                ("ullTotalVirtual", ctypes.c_ulonglong),
                ("ullAvailVirtual", ctypes.c_ulonglong),
                ("sullAvailExtendedVirtual", ctypes.c_ulonglong),
            ]

        status = Status()
        status.dwLength = ctypes.sizeof(status)
        if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(status)):
            return int(status.ullTotalPhys)
        return None
    try:
        return os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    except (ValueError, OSError, AttributeError):
        return None


def hardware_identity():
    # Memory is rounded to GiB: the exact byte count varies with firmware.
    memory = total_memory_bytes()
    return {
        **host_identity(),
        "memory_gib": round(memory / 2**30) if memory else None,
    }


def source_identity_map(fixture=FIXTURE):
    server = server_sources()
    paths = [
        "scripts/i3_eval.py",
        "scripts/frozen_eval.py",
        "scripts/p13_local.py",
        "scripts/p13_live.py",
        fixture,
    ]
    identity = {p: ev.source_identity(ROOT / p) for p in paths}
    for key in ("server/build-input-tree", "server/Cargo.lock", "server/Cargo.toml"):
        identity[key] = server[key]
    return identity


def git_commit():
    result = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=False,
    )
    return result.stdout.strip() if result.returncode == 0 else None


def validate_thresholds(thresholds, k_values):
    """None means none were declared; otherwise one complete block per family."""
    if thresholds is None:
        return
    require(isinstance(thresholds, dict), "invalid_thresholds")
    require(set(thresholds) == set(FAMILIES), "thresholds_must_cover_each_family")
    for family, block in thresholds.items():
        require(isinstance(block, dict), "invalid_thresholds:" + family)
        for field in THRESHOLD_FIELDS:
            require(field in block, f"missing_threshold:{family}.{field}")
        require(block["k"] in k_values, f"threshold_k_not_frozen:{family}")
        for field in ("min_recall", "min_ndcg"):
            value = block[field]
            require(
                type(value) in (int, float) and 0 <= value <= 1,
                f"invalid_threshold:{family}.{field}",
            )
        require(
            type(block["max_p95_ms"]) in (int, float) and block["max_p95_ms"] > 0,
            f"invalid_threshold:{family}.max_p95_ms",
        )
        require(
            type(block["max_failures"]) is int and block["max_failures"] >= 0,
            f"invalid_threshold:{family}.max_failures",
        )
        # Authorization is never a tunable threshold.
        require(
            block["max_unauthorized_disclosures"] == 0,
            f"unauthorized_disclosures_must_be_zero:{family}",
        )
        gain = block.get("min_ndcg_gain_over_hash")
        require(
            gain is None or (type(gain) in (int, float) and math.isfinite(gain)),
            f"invalid_threshold:{family}.min_ndcg_gain_over_hash",
        )
        extra = set(block) - set(THRESHOLD_FIELDS) - {"min_ndcg_gain_over_hash"}
        require(not extra, f"unknown_threshold_field:{family}")


def validate_manifest(manifest):
    ev.envelope(manifest, KIND_MANIFEST)
    require(manifest.get("revision") in ("1", "2"), "unsupported_manifest_revision")
    require(manifest.get("purpose") in PURPOSES, "invalid_purpose")
    require(tuple(manifest.get("arms", ())) == ARMS, "arms_must_be_the_four_i3_arms")
    require(manifest.get("candidate_arm") in ARMS, "invalid_candidate_arm")
    ks = manifest.get("k_values")
    require(
        isinstance(ks, list) and ks and all(type(k) is int and k > 0 for k in ks),
        "invalid_k_values",
    )
    require(manifest.get("primary_k") in ks, "primary_k_not_frozen")
    require(set(manifest.get("families", {})) == set(FAMILIES), "family_set_mismatch")
    for family in FAMILIES:
        spec = manifest["families"][family]
        paths = [e["path"] for e in spec["files"]]
        require(paths == sorted(set(paths)), "unsorted_or_duplicate_corpus:" + family)
        assert_no_answer_keys(paths)
        require(manifest.get("fixture") not in paths, "answer_key_in_corpus:fixture")
        require(spec["corpus_hash"] == ev.digest(spec["files"]), "corpus_hash_mismatch")
        require(set(spec["restricted"]) <= set(paths), "restricted_not_in_corpus")
    cases = manifest.get("cases")
    ev.ids(cases)
    require(manifest.get("cases_hash") == ev.digest(cases), "cases_hash_mismatch")
    for case in cases:
        require(case.get("family") in FAMILIES, "case_family_invalid")
        files = {e["path"] for e in manifest["families"][case["family"]]["files"]}
        require(
            isinstance(case.get("relevant"), dict)
            and case["relevant"]
            and set(case["relevant"]) <= files,
            "judgment_outside_corpus:" + case["id"],
        )
        require(
            all(type(g) is int and g in (1, 2) for g in case["relevant"].values()),
            "invalid_grade:" + case["id"],
        )
    for family in FAMILIES:
        auth = manifest["families"][family]["authorization_queries"]
        require(
            auth and set(auth) <= {c["id"] for c in cases if c["family"] == family},
            "authorization_queries_invalid:" + family,
        )
    profile = manifest.get("profile")
    require(isinstance(profile, dict), "missing_profile")
    require(manifest.get("profile_id") == ev.digest(profile), "profile_id_mismatch")
    validate_thresholds(manifest.get("thresholds"), ks)
    if manifest["purpose"] == "qualification":
        require(
            manifest.get("thresholds") is not None, "qualification_needs_thresholds"
        )
        if manifest["revision"] == "2":
            require(manifest.get("fixture") != FIXTURE, "qualification_needs_held_out_fixture")
            require(
                manifest.get("thresholds")
                == {family: QUALIFICATION_THRESHOLDS for family in FAMILIES},
                "qualification_thresholds_differ_from_frozen_profile",
            )
    if manifest["revision"] == "2":
        require(manifest["candidate_arm"] == "embedder", "candidate_must_be_embedding_only")
        require(manifest.get("cpu_p95_limit_ms") == CPU_P95_LIMIT_MS, "invalid_cpu_p95_limits")
    for field in ("source", "build", "resource_limits", "postgres_image"):
        require(bool(manifest.get(field)), "missing_" + field)


def build_profile(args):
    lens = {
        "deployment": "native-release" if args.lens_binary else "image",
        "binary_sha256": binary_hash(args.lens_binary) if args.lens_binary else None,
        "image": args.lens_image,
        "source_commit": args.lens_source_commit,
        "features": "qwen3,fetch",
        "bundles": LENS_BUNDLES,
        "settings": dict(sorted(s.split("=", 1) for s in args.lens_setting)),
    }
    return {
        "embedding": {
            "model": LENS_BUNDLES["embedding"]["name"],
            "precision": "f32",
            "dimensions": 1024,
            "query_instruction": QWEN3_QUERY_INSTRUCTION,
            "passage_instruction": None,
            "policy": "require",
        },
        "rerank": {
            "model": LENS_BUNDLES["reranking"]["name"],
            "precision": "f32",
            "instruction": QWEN3_RERANK_INSTRUCTION,
            "max_candidates": args.max_candidates,
            "max_document_bytes": 8192,
            "policy": "require",
        },
        "timeout_ms": args.timeout_ms,
        "endpoint": {
            "max_batch": args.max_batch,
            "max_request_bytes": 65536,
            "max_attempts": 1,
        },
        "index": {
            "chunking": {"strategy": "para@1", "max_chars": args.max_chars},
            "indexing": {"rrf_k": 60, "candidate_n": 50},
            "semantic_vector_leg": "exact scan of one index version",
        },
        "top_k": args.top_k,
        "lens": lens,
        "hardware": hardware_identity(),
        "server_concurrency": {"MUNARIUM_INFERENCE_CONCURRENCY": "2"},
    }


def freeze(args):
    fixture_path = Path(args.fixture).as_posix()
    fixture = ev.read(ROOT / fixture_path)
    tracked = tracked_files()
    families = {}
    for family in FAMILIES:
        spec = fixture["families"][family]
        files = select_corpus(spec, tracked)
        require(fixture_path not in files, "answer_key_in_corpus:" + fixture_path)
        entries = corpus_entries(files)
        families[family] = {
            "label": spec["label"],
            "include": spec["include"],
            "exclude": spec["exclude"],
            "files": entries,
            "corpus_hash": ev.digest(entries),
            "corpus_bytes": sum(e["bytes"] for e in entries),
            "restricted": spec["restricted"],
            "authorization_queries": spec["authorization_queries"],
        }
    cases = fixture["queries"]
    thresholds = ev.read(args.thresholds) if args.thresholds else None
    if args.purpose == "qualification" and thresholds is None:
        thresholds = {family: QUALIFICATION_THRESHOLDS for family in FAMILIES}
    profile = build_profile(args)
    manifest = ev.seal(
        KIND_MANIFEST,
        campaign="i3-retrieval-quality",
        revision="2",
        purpose=args.purpose,
        frozen_at=now(),
        git_commit=git_commit(),
        source=source_identity_map(fixture_path),
        build={
            "server_binary_sha256": binary_hash(args.binary),
            "server_binary_name": Path(args.binary).name,
            "cargo_profile": "release",
        },
        postgres_image=PG_IMAGE,
        families=families,
        cases=cases,
        cases_hash=ev.digest(cases),
        fixture=fixture_path,
        fixture_sha256=ev.source_identity(ROOT / fixture_path),
        arms=list(ARMS),
        candidate_arm="embedder",
        cpu_p95_limit_ms=CPU_P95_LIMIT_MS,
        k_values=[5, 10],
        primary_k=10,
        profile=profile,
        profile_id=ev.digest(profile),
        thresholds=thresholds,
        thresholds_status="declared" if thresholds else "none_declared",
        failure_treatment=(
            "A failed or unexecuted query scores zero in the planned-query means; "
            "failures are counted per arm; latency percentiles use successful queries only."
        ),
        resource_limits={
            "max_wall_seconds": args.max_wall_seconds,
            "max_http_calls": 60000,
            "http_timeout_seconds": 180,
        },
    )
    validate_manifest(manifest)
    path = ev.write(args.output, manifest)
    print(path)
    for family in FAMILIES:
        spec = families[family]
        log(
            f"{family}: {len(spec['files'])} files, {spec['corpus_bytes']} bytes, "
            f"{sum(c['family'] == family for c in cases)} queries"
        )
    return 0


# ---------------------------------------------------------------------------
# the owned rig
# ---------------------------------------------------------------------------


class I3Client(Client):
    """p13_local.Client, keeping a fixed-vocabulary problem code on failure."""

    def call(self, method, path, body=None, *, token=None, headers=None, timeout=60):
        with self.lock:
            if self.calls >= self.limit:
                raise RuntimeError("request_limit_reached")
            if time.monotonic() >= self.deadline:
                raise RuntimeError("wall_time_limit_reached")
            self.calls += 1
        request_headers = {
            "Authorization": "Bearer " + (token or self.token),
            "X-Munarium-Uid": self.uid,
        }
        if method in ("POST", "PUT"):
            request_headers["Idempotency-Key"] = str(uuid.uuid4())
        if isinstance(body, str):
            data = body.encode()
            request_headers["Content-Type"] = "text/yaml"
        else:
            data = None if body is None else json.dumps(body).encode()
            request_headers["Content-Type"] = "application/json"
        request_headers.update(headers or {})
        request = urllib.request.Request(
            self.base + path, data=data, method=method, headers=request_headers
        )
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                raw = response.read()
                return json.loads(raw) if raw else {}
        except urllib.error.HTTPError as exc:
            error = CallError(exc.code)
            error.problem = problem_code(exc.read())
            raise error from None


def problem_code(body):
    """The problem type's last path segment, only if it is a plain slug."""
    try:
        value = json.loads(body)
        kind = str(value.get("type") or value.get("code") or "")
    except (ValueError, AttributeError):
        return None
    slug = kind.rstrip("/").rsplit("/", 1)[-1]
    ok = slug and len(slug) <= 64 and all(c.isalnum() or c in "-_." for c in slug)
    return slug if ok else None


class I3Rig:
    """Owns one --rm PostgreSQL container and one Server child; adopts nothing."""

    def __init__(self, binary, directory, http_timeout):
        self.binary = Path(binary).resolve()
        self.directory = Path(directory).resolve()
        self.name = "cv-i3-" + uuid.uuid4().hex[:12]
        self.pg_port, self.http_port, self.ops_port = [unused_port() for _ in range(3)]
        self.rw, self.mgmt, self.secret = [uuid.uuid4().hex for _ in range(3)]
        self.process = None
        self.container_created = False
        self.log = None
        self.cleanup = {"container": self.name, "container_removed": None}
        self.environment = {
            k: v for k, v in os.environ.items() if not k.startswith("MUNARIUM_")
        }
        self.settings = {
            "MUNARIUM_STORE": "postgres",
            "MUNARIUM_SOURCE_STORE": "pg",
            "MUNARIUM_GRPC_ADDR": "disabled",
            "MUNARIUM_AUTH_MODE": "static",
            "MUNARIUM_REQUIRE_UID": "true",
            "MUNARIUM_LOG": "warn",
            "MUNARIUM_DB_MAX_CONNS": "8",
            "MUNARIUM_RETRIEVAL_MODE": "postgres",
            "MUNARIUM_INFERENCE": "enabled",
            "MUNARIUM_INFERENCE_CONCURRENCY": "2",
            "MUNARIUM_DATASTORE_BUILDER": "enabled",
            "MUNARIUM_DATASTORE_BUILDER_POLL_MS": "500",
        }
        self.environment.update(self.settings)
        self.environment.update(
            {
                "MUNARIUM_DATABASE_URL": f"postgres://i3:i3-local-only@127.0.0.1:{self.pg_port}/i3",
                "MUNARIUM_HTTP_ADDR": f"127.0.0.1:{self.http_port}",
                "MUNARIUM_OPS_ADDR": f"127.0.0.1:{self.ops_port}",
                "MUNARIUM_TOKEN_SECRET": self.secret,
                "MUNARIUM_STATIC_TOKENS": f"{self.rw}:i3:rw,{self.mgmt}:i3:mgmt",
            }
        )
        self.client = I3Client(
            f"http://127.0.0.1:{self.http_port}", self.rw, uid="i3-operator"
        )
        self.http_timeout = http_timeout

    def docker(self, *args, timeout=120):
        result = subprocess.run(
            ["docker", *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        if result.returncode:
            # CLI output can carry environment values; keep it out of records.
            raise RuntimeError("docker_command_failed:" + args[0])
        return result.stdout.strip()

    def __enter__(self):
        self.directory.mkdir(parents=True, exist_ok=False)
        try:
            self.image_id = self.docker(
                "image", "inspect", PG_IMAGE, "--format", "{{.Id}}"
            )
            self.docker(
                "run",
                "--rm",
                "--detach",
                "--name",
                self.name,
                "--label",
                "munarium.i3=owned",
                "--publish",
                f"127.0.0.1:{self.pg_port}:5432",
                "--env",
                "POSTGRES_USER=i3",
                "--env",
                "POSTGRES_PASSWORD=i3-local-only",
                "--env",
                "POSTGRES_DB=i3",
                PG_IMAGE,
            )
            self.container_created = True
            deadline = time.monotonic() + 90
            while True:
                probe = subprocess.run(
                    [
                        "docker",
                        "exec",
                        self.name,
                        "pg_isready",
                        "-h",
                        "127.0.0.1",
                        "-U",
                        "i3",
                    ],
                    capture_output=True,
                    timeout=10,
                    check=False,
                )
                if probe.returncode == 0:
                    break
                require(time.monotonic() < deadline, "postgres_readiness_timeout")
                time.sleep(0.25)
            self.start()
            return self
        except BaseException:
            self.close()
            raise

    def start(self):
        self.log = (self.directory / "server.log").open("xb")
        self.process = subprocess.Popen(
            [str(self.binary)],
            cwd=self.directory,
            env=self.environment,
            stdout=self.log,
            stderr=self.log,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            require(self.process.poll() is None, "server_exited_before_readiness")
            try:
                with urllib.request.urlopen(
                    f"http://127.0.0.1:{self.ops_port}/readyz", timeout=1
                ) as response:
                    if response.status == 200:
                        return
            except (OSError, urllib.error.URLError):
                time.sleep(0.05)
        raise RuntimeError("server_readiness_timeout")

    def database_bytes(self):
        return int(
            self.docker(
                "exec",
                self.name,
                "psql",
                "-U",
                "i3",
                "-d",
                "i3",
                "-Atc",
                "SELECT pg_database_size(current_database())",
            )
        )

    def close(self):
        try:
            if self.process is not None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=30)
                self.process = None
            if self.log is not None:
                self.log.close()
                self.log = None
        finally:
            if self.container_created:
                # --rm removes the container and its anonymous volume on stop.
                self.docker("stop", "--time", "10", self.name, timeout=180)
                self.container_created = False
                listed = self.docker(
                    "ps", "-a", "--filter", f"name=^{self.name}$", "-q"
                )
                self.cleanup["container_removed"] = listed == ""

    def __exit__(self, *unused):
        self.close()


# ---------------------------------------------------------------------------
# the run
# ---------------------------------------------------------------------------


def lens_identity(url):
    """Content-free readiness facts from the running endpoint."""
    with urllib.request.urlopen(url.rstrip("/") + "/readyz", timeout=10) as response:
        ready = json.loads(response.read())
    stages = {
        s["stage"]: {
            "model": s.get("model"),
            "backend": s.get("backend"),
            "retrieval_model": s.get("retrieval_model"),
            "dimensions": s.get("dimensions"),
            "manifest_sha256": s.get("manifest_sha256"),
            "max_input_tokens": (s.get("limits") or {}).get("max_input_tokens"),
        }
        for s in ready.get("stages", [])
    }
    return {"status": ready.get("status"), "stages": stages}


def check_lens(manifest, observed, lens_binary):
    lens = manifest["profile"]["lens"]
    require(observed["status"] == "ready", "lens_not_ready")
    for stage in ("embedding", "reranking"):
        pin = lens["bundles"][stage]
        got = observed["stages"].get(stage) or {}
        require(got.get("model") == pin["name"], f"lens_model_mismatch:{stage}")
        require(
            got.get("manifest_sha256") == pin["manifest_sha256"],
            f"lens_bundle_mismatch:{stage}",
        )
        require(
            got.get("backend") == "qwen3" and got.get("retrieval_model") is True,
            f"lens_backend_mismatch:{stage}",
        )
        wanted = lens["settings"].get("LENS_MAX_INPUT_TOKENS")
        if wanted is not None:
            require(
                str(got.get("max_input_tokens")) == wanted,
                f"lens_limit_mismatch:{stage}",
            )
    if lens["binary_sha256"]:
        require(lens_binary is not None, "lens_binary_required")
        require(
            binary_hash(lens_binary) == lens["binary_sha256"], "lens_binary_changed"
        )


def check_inputs(manifest, binary):
    validate_manifest(manifest)
    fixture = manifest["fixture"]
    require(manifest["source"] == source_identity_map(fixture), "source_changed")
    require(
        manifest["fixture_sha256"] == ev.source_identity(ROOT / fixture),
        "fixture_changed",
    )
    require(
        manifest["build"]["server_binary_sha256"] == binary_hash(binary),
        "binary_changed",
    )
    require(manifest["profile"]["hardware"] == hardware_identity(), "hardware_mismatch")
    tracked = set(tracked_files())
    for family in FAMILIES:
        for entry in manifest["families"][family]["files"]:
            require(entry["path"] in tracked, "corpus_file_untracked:" + entry["path"])
        require(
            corpus_entries([e["path"] for e in manifest["families"][family]["files"]])
            == manifest["families"][family]["files"],
            "corpus_changed:" + family,
        )
    if manifest["purpose"] == "qualification":
        commit = manifest.get("git_commit")
        require(bool(commit), "qualification_needs_commit")


def collection_names(family):
    return f"i3-{family}", f"i3-{family}-restricted"


def setup_corpus(rig, manifest, record):
    client = rig.client
    profile = manifest["profile"]
    shape = {
        "apiVersion": "munarium.ioka.io/v1",
        "kind": "Shape",
        "metadata": {"name": "i3-corpus", "version": 1},
        "spec": {
            "chunking": profile["index"]["chunking"],
            "indexing": profile["index"]["indexing"],
        },
    }
    client.call("POST", "/v1/shapes", json.dumps(shape))
    collections = []
    for family in FAMILIES:
        public, restricted = collection_names(family)
        for name, level, prefix in (
            (public, 0, f"{family}/"),
            (restricted, 1, f"{family}-restricted/"),
        ):
            collections.append(
                {
                    "name": name,
                    "shape": "i3-corpus@1",
                    "accessLevel": level,
                    "sources": {"filenamePrefix": prefix, "mediaTypes": ["text/plain"]},
                }
            )
    runbook = {
        "apiVersion": "munarium.ioka.io/v1",
        "kind": "Runbook",
        "metadata": {"name": "i3-eval", "version": 1},
        "spec": {
            "sources": {"container": "sources", "prefix": ""},
            "collections": collections,
            "retrieval": {"topK": profile["top_k"], "candidateN": 50, "rrfK": 60},
            "steps": [
                {"resolveSources": {}},
                {"buildIndex": {}},
                {"verify": {}},
                {"cutover": {"approval": "required"}},
            ],
        },
    }
    client.call("POST", "/v1/runbooks", json.dumps(runbook))
    started = time.perf_counter()
    uploads = {}
    for family in FAMILIES:
        count = 0
        for filename, path, restricted in upload_plan(manifest, family):
            data = corpus_bytes(path)
            if restricted:
                data = f"{RESTRICTED_MARKER} {family}\n\n".encode() + data
            client.call(
                "POST",
                "/v1/ingest",
                {
                    "filename": filename,
                    "media_type": "text/plain",
                    "content_base64": base64.b64encode(data).decode(),
                },
            )
            count += 1
        uploads[family] = count
    record["setup"]["uploads"] = uploads
    record["setup"]["ingest_seconds"] = round(time.perf_counter() - started, 3)
    started = time.perf_counter()
    result = client.call("POST", "/v1/runbooks/i3-eval@1/runs")
    deadline = time.monotonic() + 1800
    while result["state"] != "done":
        require(result["state"] != "failed", "hash_index_build_failed")
        require(time.monotonic() < deadline, "hash_index_build_timeout")
        status = client.call("GET", f"/v1/runs/{result['run_id']}")
        waiting = [s for s in status["steps"] if s["state"] == "awaiting_approval"]
        if waiting:
            result = client.call(
                "POST",
                f"/v1/runs/{result['run_id']}/steps/{waiting[0]['ordinal']}/approve",
            )
        else:
            result = status
            time.sleep(0.5)
    record["setup"]["hash_build_seconds"] = round(time.perf_counter() - started, 3)
    listed = client.call("GET", "/v1/collections")["collections"]
    ids = {c["name"]: c for c in listed}
    record["setup"]["collections"] = {
        name: {
            "access_level": ids[name]["access_level"],
            "source_count": ids[name]["source_count"],
            "hash_index": ids[name].get("active_index"),
        }
        for family in FAMILIES
        for name in collection_names(family)
    }
    return {name: ids[name]["id"] for name in ids}


def mint(rig, uid, level):
    return rig.client.call(
        "POST",
        "/v1/access-tokens",
        {
            "uid": uid,
            "access_level": level,
            "compartments": [],
            "scopes": ["query"],
            "ttl_secs": 86400,
        },
        token=rig.mgmt,
    )["token"]


def provider_yaml(manifest, lens_url):
    profile = manifest["profile"]
    endpoint = profile["endpoint"]
    return (
        "apiVersion: munarium.ioka.io/v1\n"
        "kind: ProviderConfig\n"
        "metadata: {name: lens}\n"
        "spec:\n"
        "  provider: openai-compatible\n"
        f"  endpoint: {lens_url.rstrip('/')}/v1\n"
        "  models:\n"
        f"    embed: [{profile['embedding']['model']}]\n"
        f"    rerank: [{profile['rerank']['model']}]\n"
        f"  inference: {{embedDimensions: {profile['embedding']['dimensions']}, "
        f"maxBatch: {endpoint['max_batch']}, maxRequestBytes: {endpoint['max_request_bytes']}, "
        f"maxAttempts: {endpoint['max_attempts']}}}\n"
        "  budgets: {dailyTotalTokens: 50000000}\n"
    )


def binding_body(manifest, arm, revision):
    profile = manifest["profile"]
    body = {
        "expected_revision": revision,
        "provider_config": "lens",
        "timeout_ms": profile["timeout_ms"],
    }
    if "embedding" in ARM_STAGES[arm]:
        e = profile["embedding"]
        body["embedding"] = {
            "model": e["model"],
            "query_instruction": e["query_instruction"],
            "policy": e["policy"],
        }
        if e["passage_instruction"] is not None:
            body["embedding"]["passage_instruction"] = e["passage_instruction"]
    if "reranking" in ARM_STAGES[arm]:
        r = profile["rerank"]
        body["rerank"] = {
            "model": r["model"],
            "max_candidates": r["max_candidates"],
            "max_document_bytes": r["max_document_bytes"],
            "instruction": r["instruction"],
            "policy": r["policy"],
        }
    return body


def binding_snapshot(dto):
    return {
        "state": dto.get("state"),
        "revision": dto.get("revision"),
        "semantic_index_id": dto.get("semantic_index_id"),
        "semantic_chunks": dto.get("semantic_chunks"),
        "semantic_vectors": dto.get("semantic_vectors"),
        "active_index_id": dto.get("active_index_id"),
        "prior_index_id": dto.get("prior_index_id"),
        "stages": [
            {k: s.get(k) for k in ("stage", "state", "reason", "identity", "policy")}
            for s in dto.get("stages", [])
        ],
        "usage": dto.get("usage", []),
    }


def failure_of(exc):
    if isinstance(exc, CallError):
        return {
            "class": "http_error",
            "status": exc.status,
            "problem": getattr(exc, "problem", None),
        }
    if isinstance(exc, (TimeoutError, urllib.error.URLError, OSError)):
        return {
            "class": "transport_error",
            "status": None,
            "problem": type(exc).__name__,
        }
    return {"class": "harness_error", "status": None, "problem": str(exc)[:80]}


def build_semantic(rig, manifest, collection_id, journal):
    client = rig.client
    started = time.perf_counter()
    job = client.call(
        "POST",
        "/v1/index-build-jobs",
        {
            "kind": "semantic",
            "collection_id": collection_id,
            "max_chars": manifest["profile"]["index"]["chunking"]["max_chars"],
        },
    )
    deadline = time.monotonic() + manifest["resource_limits"]["max_wall_seconds"]
    last = None
    while job["state"] not in ("succeeded", "failed", "cancelled", "superseded"):
        require(time.monotonic() < deadline, "semantic_build_timeout")
        time.sleep(2)
        job = client.call("GET", f"/v1/index-build-jobs/{job['job_id']}")
        if job["state"] != last:
            journal(
                {
                    "event": "semantic_job",
                    "collection": collection_id,
                    "state": job["state"],
                }
            )
            last = job["state"]
    return {
        "state": job["state"],
        "seconds": round(time.perf_counter() - started, 3),
        "result": job.get("result"),
        "error_code": job.get("error_code"),
    }


def configure_arm(rig, manifest, arm, ids, family, record, journal):
    """Bind, build when needed, and enroll both collections of the family."""
    client, mgmt = rig.client, rig.mgmt
    steps = []
    for name in collection_names(family):
        cid = ids[name]
        current = client.call("GET", f"/v1/collections/{cid}/inference", token=mgmt)
        bound = client.call(
            "PUT",
            f"/v1/collections/{cid}/inference",
            binding_body(manifest, arm, current.get("revision", 0)),
            token=mgmt,
        )
        steps.append({"collection": name, "step": "bind", **binding_snapshot(bound)})
        if "embedding" in ARM_STAGES[arm] and not bound.get("semantic_index_id"):
            log(f"{family}/{arm}: semantic build of {name}")
            build = build_semantic(rig, manifest, cid, journal)
            build["usage"] = client.call(
                "GET", f"/v1/collections/{cid}/inference", token=mgmt
            ).get("usage", [])
            record["builds"][name] = build
            require(build["state"] == "succeeded", "semantic_build_" + build["state"])
            require(
                bool((build["result"] or {}).get("complete")),
                "semantic_build_incomplete",
            )
        current = client.call("GET", f"/v1/collections/{cid}/inference", token=mgmt)
        enrolled = client.call(
            "POST",
            f"/v1/collections/{cid}/inference/enroll",
            {"expected_revision": current["revision"]},
            token=mgmt,
        )
        steps.append(
            {"collection": name, "step": "enroll", **binding_snapshot(enrolled)}
        )
        require(enrolled.get("state") == "enrolled", "enroll_refused")
    return steps


def rollback_family(rig, ids, family):
    client, mgmt = rig.client, rig.mgmt
    steps = []
    for name in collection_names(family):
        cid = ids[name]
        current = client.call("GET", f"/v1/collections/{cid}/inference", token=mgmt)
        if current.get("state") != "enrolled":
            continue
        done = client.call(
            "POST",
            f"/v1/collections/{cid}/inference/rollback",
            {"expected_revision": current["revision"]},
            token=mgmt,
        )
        steps.append({"collection": name, "step": "rollback", **binding_snapshot(done)})
    return steps


def search(client, token, query, collection, top_k, timeout, index_version=None):
    started = time.perf_counter()
    body = {
        "query": query,
        "top_k": top_k,
        "filter": {"collections": [collection]},
        "disclose_execution": True,
    }
    if index_version is not None:
        body["index_version"] = index_version
    response = client.call(
        "POST",
        "/v1/search",
        body,
        token=token,
        timeout=timeout,
    )
    return response, (time.perf_counter() - started) * 1000


def hit_rows(hits):
    return [
        {
            "rank": i,
            "source_path": h["source_path"],
            "chunk_id": h["chunk_id"],
            "source_content_hash": h.get("source_content_hash"),
            "score": h.get("score"),
            "lexical_rank": h.get("lexical_rank"),
            "vector_rank": h.get("vector_rank"),
            "restricted_marker": RESTRICTED_MARKER in (h.get("text") or ""),
        }
        for i, h in enumerate(hits)
    ]


def replay_difference(before, after):
    """How a post-rollback hash read differs from the original hash read."""
    if before is None or before.get("status") != "ok" or after.get("status") != "ok":
        return "not_comparable"
    if before.get("index_version") != after.get("index_version"):
        return "index_version_changed"
    if before["hits"] == after["hits"]:
        return "identical"
    ids = [h["chunk_id"] for h in before["hits"]]
    again = [h["chunk_id"] for h in after["hits"]]
    if ids == again:
        return "scores_changed"
    if sorted(ids) == sorted(again):
        return "order_changed"
    return "hits_changed"


def replay_summary(replay):
    if replay is None or "differences" not in replay:
        return replay  # absent, or a count-only record from an earlier runner
    counts = {}
    if "baseline_queries" in replay:
        before = {r["id"]: r for r in replay["baseline_queries"]}
        expected = {r["id"]: replay_difference(before.get(r["id"]), r)
                    for r in replay["queries"]}
        require(expected == replay["differences"], "rollback_replay_difference_mismatch")
    for kind in replay["differences"].values():
        counts[kind] = counts.get(kind, 0) + 1
    return {"queries": len(replay["queries"]), "differences": counts}


def stage_rows(response):
    return [
        {
            k: s.get(k)
            for k in (
                "stage",
                "applied",
                "state",
                "reason",
                "fallback",
                "identity",
                "scored",
                "policy",
            )
        }
        for s in ((response.get("execution") or {}).get("stages") or [])
    ]


def stage_problem(arm, stages):
    """None when the receipts show exactly the arm's stages served."""
    by_stage = {s["stage"]: s for s in stages}
    if set(by_stage) != set(ARM_STAGES[arm]):
        return "stage_set_mismatch"
    for stage in ARM_STAGES[arm]:
        s = by_stage[stage]
        if not s["applied"] and not (
            stage == "reranking" and s["state"] == "not_applicable"
        ):
            return "stage_not_applied"
    return None


def run_queries(rig, manifest, arm, family, ids, tokens, journal, hash_pin=None):
    profile = manifest["profile"]
    timeout = manifest["resource_limits"]["http_timeout_seconds"]
    public, restricted = collection_names(family)
    rows = []
    for case in [c for c in manifest["cases"] if c["family"] == family]:
        row = {"id": case["id"], "status": "unexecuted"}
        try:
            response, ms = search(
                rig.client,
                tokens["reader"],
                case["query"],
                public,
                profile["top_k"],
                timeout,
                hash_pin,
            )
            stages = stage_rows(response)
            problem = stage_problem(arm, stages)
            envelope = response.get("envelope") or {}
            if hash_pin is not None and envelope.get("index_version") != hash_pin:
                problem = "hash_pin_mismatch"
            row.update(
                status="failed" if problem else "ok",
                failure={"class": problem} if problem else None,
                latency_ms=round(ms, 3),
                hits=hit_rows(response.get("hits", [])),
                stages=stages,
                index_version=envelope.get("index_version"),
                provider_fingerprint=envelope.get("provider_fingerprint"),
            )
        except Exception as exc:  # noqa: BLE001 - every failure is retained
            if isinstance(exc, RuntimeError) and str(exc) in (
                "request_limit_reached",
                "wall_time_limit_reached",
            ):
                raise
            row.update(status="failed", failure=failure_of(exc))
        rows.append(row)
        journal(
            {
                "event": "query",
                "family": family,
                "arm": arm,
                "id": case["id"],
                "status": row["status"],
                "latency_ms": row.get("latency_ms"),
            }
        )
    checks = []
    for qid in manifest["families"][family]["authorization_queries"]:
        query = next(c["query"] for c in manifest["cases"] if c["id"] == qid)
        check = {"id": qid}
        try:
            denied = search(
                rig.client,
                tokens["reader"],
                query,
                restricted,
                profile["top_k"],
                timeout,
            )[0]
            hits = denied.get("hits", [])
            check["unauthorized"] = {
                "outcome": "disclosed" if hits else "allowed_empty",
                "status": 200,
                "hits": len(hits),
            }
        except CallError as exc:
            check["unauthorized"] = {
                "outcome": "denied" if exc.status in (403, 404) else "error",
                "status": exc.status,
                "hits": 0,
            }
        except Exception as exc:  # noqa: BLE001
            check["unauthorized"] = {
                "outcome": "error",
                "status": None,
                "hits": 0,
                "failure": failure_of(exc),
            }
        try:
            allowed, _ = search(
                rig.client,
                tokens["cleared"],
                query,
                restricted,
                profile["top_k"],
                timeout,
            )
            hits = allowed.get("hits", [])
            control_problem = stage_problem(arm, stage_rows(allowed))
            ok = control_problem is None and bool(hits) and all(
                h["source_path"].startswith(f"{family}-restricted/") for h in hits
            )
            check["control"] = {
                "outcome": "passed" if ok else "failed",
                "hits": len(hits),
                "stage_problem": control_problem,
            }
        except Exception as exc:  # noqa: BLE001
            check["control"] = {
                "outcome": "failed",
                "hits": 0,
                "failure": failure_of(exc),
            }
        checks.append(check)
    return rows, checks


def execute(manifest, binary, lens_url, lens_binary, directory, run_id):
    check_inputs(manifest, binary)
    observed_lens = lens_identity(lens_url)
    check_lens(manifest, observed_lens, lens_binary)
    directory = Path(directory)
    scratch = directory / f"run-{run_id}"
    record = {
        "manifest_id": manifest["id"],
        "run_id": run_id,
        "purpose": manifest["purpose"],
        "started_at": now(),
        "environment": {
            "hardware": hardware_identity(),
            "lens": observed_lens,
            "server_binary_sha256": binary_hash(binary),
            "postgres_image": PG_IMAGE,
            "server_settings": None,
        },
        "setup": {},
        "families": {
            family: {
                "builds": {},
                "arms": {
                    arm: {
                        "availability": "unexecuted",
                        "reason": "not_started",
                        "config": [],
                        "queries": [],
                        "authorization": [],
                    }
                    for arm in ARMS
                },
                "rollback_replay": None,
            }
            for family in FAMILIES
        },
        "run_error": None,
    }
    journal_path = directory / f"{run_id}.journal.jsonl"
    journal_stream = journal_path.open("x", encoding="utf-8")

    def journal(event):
        journal_stream.write(json.dumps({"t": round(time.time(), 3), **event}) + "\n")
        journal_stream.flush()
        os.fsync(journal_stream.fileno())

    wall = time.perf_counter()
    rig = I3Rig(binary, scratch, manifest["resource_limits"]["http_timeout_seconds"])
    rig.client.deadline = (
        time.monotonic() + manifest["resource_limits"]["max_wall_seconds"]
    )
    rig.client.limit = manifest["resource_limits"]["max_http_calls"]
    record["environment"]["server_settings"] = rig.settings
    try:
        with rig:
            ids = setup_corpus(rig, manifest, record)
            log("corpus ingested and hash indexes built")
            tokens = {
                "reader": mint(rig, "i3-operator", 0),
                "cleared": mint(rig, "i3-operator", 1),
            }
            rig.client.call("POST", "/v1/providers", provider_yaml(manifest, lens_url))
            probe = rig.client.call(
                "POST", "/v1/providers/lens/inference-probe", token=rig.mgmt
            )
            record["setup"]["probe"] = {
                k: probe.get(k)
                for k in (
                    "passed",
                    "embed_model",
                    "embed_dimensions",
                    "rerank_model",
                    "checks",
                )
            }
            for family in FAMILIES:
                fam = record["families"][family]
                exact_baseline = None
                if manifest["revision"] == "2":
                    public, _ = collection_names(family)
                    pin = record["setup"]["collections"][public]["hash_index"]
                    require(bool(pin), "missing_hash_index_pin")
                    exact_baseline, _ = run_queries(
                        rig, manifest, "hash", family, ids, tokens, journal, hash_pin=pin
                    )
                for arm in ARM_ORDER:
                    slot = fam["arms"][arm]
                    log(f"{family}/{arm}: configuring")
                    try:
                        if arm != "hash":
                            require(probe.get("passed") is True, "probe_failed")
                            slot["config"] = configure_arm(
                                rig, manifest, arm, ids, family, fam, journal
                            )
                        slot["availability"], slot["reason"] = "available", None
                    except Exception as exc:  # noqa: BLE001 - recorded as unavailable
                        slot["availability"] = "unavailable"
                        slot["reason"] = failure_of(exc)
                        log(f"{family}/{arm}: unavailable {slot['reason']}")
                    if slot["availability"] == "available":
                        log(f"{family}/{arm}: querying")
                        slot["queries"], slot["authorization"] = run_queries(
                            rig, manifest, arm, family, ids, tokens, journal
                        )
                    if arm != "hash":
                        slot["config"] += rollback_family(rig, ids, family)
                    journal({"event": "arm_done", "family": family, "arm": arm})
                replay, _ = run_queries(
                    rig, manifest, "hash", family, ids, tokens, journal
                )
                reference = (exact_baseline if exact_baseline is not None
                             else fam["arms"]["hash"]["queries"])
                baseline = {r["id"]: r for r in reference}
                fam["rollback_replay"] = {
                    "queries": replay,
                    "differences": {
                        r["id"]: replay_difference(baseline.get(r["id"]), r)
                        for r in replay
                    },
                }
                if exact_baseline is not None:
                    fam["rollback_replay"]["baseline_queries"] = exact_baseline
            record["setup"]["database_bytes"] = rig.database_bytes()
    except Exception as exc:  # noqa: BLE001 - the run is retained as incomplete
        record["run_error"] = failure_of(exc)
        log(f"run error: {record['run_error']}")
    finally:
        journal_stream.close()
    record["cleanup"] = rig.cleanup
    record["http_calls"] = rig.client.calls
    record["finished_at"] = now()
    record["wall_seconds"] = round(time.perf_counter() - wall, 3)
    raw = ev.seal(KIND_RAW, **record)
    paths = [ev.write(directory, raw)]
    receipts = family_receipts(manifest, raw)
    for receipt in receipts:
        paths.append(ev.write(directory, receipt))
    verdict = two_family_verdict(manifest, raw, receipts)
    paths.append(ev.write(directory, verdict))
    for p in paths:
        print(p)
    for receipt in receipts:
        log(f"{receipt['family']}: {receipt['verdict']} {receipt['reasons']}")
    log(f"I3-T1: {verdict['verdict']} {verdict['reasons']}")
    return EXIT[verdict["verdict"]]


# ---------------------------------------------------------------------------
# receipts and verdicts (pure functions of manifest + raw)
# ---------------------------------------------------------------------------


def summarize_arm(manifest, family, slot):
    cases = [c for c in manifest["cases"] if c["family"] == family]
    planned = len(cases)
    by_id = {r["id"]: r for r in slot.get("queries", [])}
    ks = manifest["k_values"]
    prefix = f"{family}/"
    sums = {f"{m}@{k}": 0.0 for k in ks for m in ("recall", "ndcg")}
    ok_sums = dict(sums)
    succeeded, failures, classes, disclosures = 0, 0, {}, 0
    samples = []
    for case in cases:
        row = by_id.get(case["id"])
        if row is None or row["status"] == "unexecuted":
            continue
        for hit in row.get("hits") or []:
            if (
                hit["source_path"].startswith(f"{family}-restricted/")
                or hit["restricted_marker"]
            ):
                disclosures += 1
        if row["status"] != "ok":
            failures += 1
            key = (row.get("failure") or {}).get("class", "unknown")
            classes[key] = classes.get(key, 0) + 1
            samples.append({"outcome": "failed", "duration_ms": row.get("latency_ms")})
            continue
        succeeded += 1
        samples.append({"outcome": "passed", "duration_ms": row["latency_ms"]})
        ranked = ranked_files(row["hits"], prefix)
        for k in ks:
            for name, fn in (("recall", recall_at_k), ("ndcg", ndcg_at_k)):
                value = fn(ranked, case["relevant"], k)
                sums[f"{name}@{k}"] += value
                ok_sums[f"{name}@{k}"] += value
    executed = succeeded + failures
    auth = slot.get("authorization", [])
    for check in auth:
        disclosures += check.get("unauthorized", {}).get("hits", 0)
    summary = {
        "availability": slot.get("availability"),
        "reason": slot.get("reason"),
        "planned": planned,
        "executed": executed,
        "succeeded": succeeded,
        "failures": failures,
        "failure_classes": classes,
        "metrics": {k: round(v / planned, 6) for k, v in sums.items()},
        "metrics_successful_only": (
            {k: round(v / succeeded, 6) for k, v in ok_sums.items()}
            if succeeded
            else None
        ),
        "latency": (
            ev.latency_report(
                samples,
                elapsed_ms=max(1e-3, sum(s["duration_ms"] or 0 for s in samples)),
            )
            if samples
            else None
        ),
        "authorization": {
            "checks": len(auth),
            "planned": len(manifest["families"][family]["authorization_queries"]),
            "denied": sum(
                c.get("unauthorized", {}).get("outcome") == "denied" for c in auth
            ),
            "not_enforced": sum(
                c.get("unauthorized", {}).get("outcome")
                in ("disclosed", "allowed_empty")
                for c in auth
            ),
            "errors": sum(
                c.get("unauthorized", {}).get("outcome") == "error" for c in auth
            ),
            "controls_passed": sum(
                c.get("control", {}).get("outcome") == "passed" for c in auth
            ),
            "unauthorized_disclosures": disclosures,
        },
    }
    return summary


def family_verdict(thresholds, arms, candidate):
    """I3-T2: a missed threshold fails; missing evidence leaves it incomplete."""
    failed, incomplete = [], []
    for arm in ARMS:
        s = arms.get(arm)
        if s is None or s["availability"] != "available":
            incomplete.append(f"arm_unavailable:{arm}")
            continue
        if s["executed"] < s["planned"]:
            incomplete.append(f"unexecuted_queries:{arm}")
        a = s["authorization"]
        if a["unauthorized_disclosures"] > 0:
            failed.append(f"unauthorized_disclosure:{arm}")
        if a["not_enforced"] > 0:
            failed.append(f"authorization_not_enforced:{arm}")
        if (
            a["checks"] < a["planned"]
            or a["errors"] > 0
            or a["controls_passed"] < a["planned"]
        ):
            incomplete.append(f"authorization_evidence_incomplete:{arm}")
    if thresholds is None:
        incomplete.append("no_thresholds_declared")
    else:
        s = arms.get(candidate)
        if s is not None and s["availability"] == "available":
            k = thresholds["k"]
            if s["metrics"][f"recall@{k}"] < thresholds["min_recall"]:
                failed.append(f"recall@{k}_below_threshold")
            if s["metrics"][f"ndcg@{k}"] < thresholds["min_ndcg"]:
                failed.append(f"ndcg@{k}_below_threshold")
            p95 = (s["latency"] or {}).get("p95_ms")
            if p95 is None:
                incomplete.append("no_latency_samples")
            elif p95 > thresholds["max_p95_ms"]:
                failed.append("p95_above_threshold")
            if s["failures"] > thresholds["max_failures"]:
                failed.append("failures_above_threshold")
            gain = thresholds.get("min_ndcg_gain_over_hash")
            base = arms.get("hash")
            if gain is not None:
                if base is None or base["availability"] != "available":
                    incomplete.append("no_hash_baseline_for_gain")
                elif s["metrics"][f"ndcg@{k}"] - base["metrics"][f"ndcg@{k}"] < gain:
                    failed.append(f"ndcg@{k}_gain_over_hash_below_threshold")
    verdict = "failed" if failed else "incomplete" if incomplete else "passed"
    return verdict, failed + incomplete


def family_receipts(manifest, raw):
    out = []
    for family in FAMILIES:
        fam = raw["families"][family]
        arms = {arm: summarize_arm(manifest, family, fam["arms"][arm]) for arm in ARMS}
        for arm, limit in manifest.get("cpu_p95_limit_ms", {}).items():
            p95 = (arms[arm]["latency"] or {}).get("p95_ms")
            arms[arm]["cpu_p95_limit_ms"] = limit
            arms[arm]["cpu_latency_status"] = (
                "incomplete" if p95 is None else "passed" if p95 <= limit else "failed"
            )
        thresholds = (manifest.get("thresholds") or {}).get(family)
        verdict, reasons = family_verdict(thresholds, arms, manifest["candidate_arm"])
        if raw.get("run_error") is not None:
            if verdict == "passed":
                verdict = "incomplete"
            reasons = reasons + ["run_error"]
        out.append(
            ev.seal(
                KIND_RECEIPT,
                manifest_id=manifest["id"],
                raw_id=raw["id"],
                family=family,
                purpose=manifest["purpose"],
                profile_id=manifest["profile_id"],
                candidate_arm=manifest["candidate_arm"],
                k_values=manifest["k_values"],
                arms=arms,
                builds=fam["builds"],
                rollback_replay=replay_summary(fam["rollback_replay"]),
                thresholds=thresholds,
                thresholds_status="declared" if thresholds else "none_declared",
                verdict=verdict,
                reasons=reasons,
                exit_code=EXIT[verdict],
            )
        )
    return out


def two_family_verdict(manifest, raw, receipts):
    """I3-T1: both families must pass for the same profile; no pooling."""
    reasons, families = [], {}
    for r in receipts:
        require(r.get("kind") == KIND_RECEIPT, "not_a_family_receipt")
        require(r.get("family") in FAMILIES, "pooled_or_unknown_family_receipt")
        require(r["family"] not in families, "duplicate_family_receipt")
        families[r["family"]] = r
    for family in FAMILIES:
        if family not in families:
            reasons.append(f"missing_family_receipt:{family}")
    receipts_ok = [families[f] for f in FAMILIES if f in families]
    if len({r["profile_id"] for r in receipts_ok}) > 1:
        reasons.append("profile_mismatch")
    if any(r["manifest_id"] != manifest["id"] for r in receipts_ok):
        reasons.append("manifest_mismatch")
    states = {
        f: families[f]["verdict"] if f in families else "incomplete" for f in FAMILIES
    }
    if "failed" in states.values():
        verdict = "failed"
    elif reasons or "incomplete" in states.values():
        verdict = "incomplete"
    else:
        verdict = "supported"
    if manifest["purpose"] != "qualification" and verdict == "supported":
        # A pilot or diagnostic run never qualifies a profile.
        verdict, reasons = (
            "incomplete",
            reasons + [f"purpose_{manifest['purpose']}_cannot_qualify"],
        )
    return ev.seal(
        KIND_VERDICT,
        manifest_id=manifest["id"],
        raw_id=raw["id"],
        purpose=manifest["purpose"],
        profile_id=manifest["profile_id"],
        receipts={f: families[f]["id"] for f in families},
        families=states,
        claim_both_families=verdict == "supported",
        verdict=verdict,
        reasons=reasons,
        exit_code=EXIT[verdict],
    )


def verify_directory(directory):
    records = [ev.read(p) for p in sorted(Path(directory).glob("sha256-*.json"))]
    require(bool(records), "empty_result_directory")
    by_id = {}
    for record in records:
        kind = record.get("kind")
        require(
            kind in (KIND_MANIFEST, KIND_RAW, KIND_RECEIPT, KIND_VERDICT),
            "unknown_kind",
        )
        ev.envelope(record, kind)
        by_id[record["id"]] = record
    code = 0
    raws = [r for r in records if r["kind"] == KIND_RAW]
    for manifest in [r for r in records if r["kind"] == KIND_MANIFEST]:
        validate_manifest(manifest)
    for raw in raws:
        require(raw["manifest_id"] in by_id, "missing_manifest")
        manifest = by_id[raw["manifest_id"]]
        expected = family_receipts(manifest, raw)
        stored = [
            r for r in records if r["kind"] == KIND_RECEIPT and r["raw_id"] == raw["id"]
        ]
        require(
            sorted(r["id"] for r in stored) == sorted(r["id"] for r in expected),
            "receipt_mismatch",
        )
        verdict = two_family_verdict(manifest, raw, expected)
        stored_verdicts = [
            r for r in records if r["kind"] == KIND_VERDICT and r["raw_id"] == raw["id"]
        ]
        require(
            [v["id"] for v in stored_verdicts] == [verdict["id"]], "verdict_mismatch"
        )
        code = max(code, verdict["exit_code"])
    for record in records:
        if record["kind"] in (KIND_RECEIPT, KIND_VERDICT):
            require(record["raw_id"] in by_id, "missing_raw")
    print(f"Verified {len(records)} I3 records")
    return code


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def check_output_ignored(directory):
    """Run output stays in an ignored scratch path unless deliberately published."""
    probe = Path(directory).resolve() / "probe.json"
    result = subprocess.run(
        ["git", "-C", str(ROOT), "check-ignore", "-q", str(probe)], check=False
    )
    require(result.returncode == 0, "output_directory_not_ignored")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    f = commands.add_parser("freeze")
    f.add_argument("--binary", required=True, type=Path)
    f.add_argument("--output", required=True, type=Path)
    f.add_argument("--purpose", choices=PURPOSES, default="pilot")
    f.add_argument("--thresholds", type=Path)
    f.add_argument(
        "--fixture",
        default=FIXTURE,
        help="repository-relative query and judgment file (an answer key)",
    )
    f.add_argument("--lens-binary", type=Path)
    f.add_argument("--lens-image")
    f.add_argument("--lens-source-commit", required=True)
    f.add_argument(
        "--lens-setting",
        action="append",
        default=[],
        help="NAME=VALUE of a Lens serving setting the run depends on",
    )
    f.add_argument("--max-chars", type=int, default=800)
    f.add_argument("--top-k", type=int, default=10)
    f.add_argument("--max-candidates", type=int, default=10)
    f.add_argument("--max-batch", type=int, default=8)
    f.add_argument("--timeout-ms", type=int, default=60000)
    f.add_argument("--max-wall-seconds", type=int, default=6 * 3600)
    r = commands.add_parser("run")
    r.add_argument("--manifest", required=True, type=Path)
    r.add_argument("--binary", required=True, type=Path)
    r.add_argument("--lens-url", required=True)
    r.add_argument("--lens-binary", type=Path)
    r.add_argument("--output", type=Path)
    r.add_argument("--run-id", default=None)
    v = commands.add_parser("verify")
    v.add_argument("directory", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "freeze":
            require(
                bool(args.lens_binary) != bool(args.lens_image), "lens_binary_xor_image"
            )
            check_output_ignored(args.output)
            return freeze(args)
        if args.command == "run":
            output = args.output or args.manifest.parent
            check_output_ignored(output)
            run_id = args.run_id or ("i3-" + uuid.uuid4().hex[:8])
            return execute(
                ev.read(args.manifest),
                args.binary,
                args.lens_url,
                args.lens_binary,
                output,
                run_id,
            )
        return verify_directory(args.directory)
    except (ValueError, TypeError, KeyError, OSError, RuntimeError) as exc:
        print(f"I3 evaluation rejected: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
