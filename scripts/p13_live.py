# SPDX-License-Identifier: Apache-2.0
"""Local live P13 campaigns: freeze first, then execute the immutable manifest.

No external service, provider key or paid model is accepted by this runner.
"""

import argparse
import base64
import concurrent.futures
import hashlib
import json
import math
import os
import platform
import random
import subprocess
import sys
import time
from pathlib import Path

import frozen_eval as ev
import p13_cases as cases
from p13_local import CallError, LocalRig, binary_hash

ROOT = ev.ROOT
MAX_DATABASE_BYTES = 512 * 1024 * 1024


def sources():
    paths = [
        Path(__file__),
        ROOT / "scripts/p13_cases.py",
        ROOT / "scripts/p13_local.py",
        Path(ev.__file__),
        ev.GRADER_PATH,
        ROOT / "server/Cargo.lock",
    ]
    # Hash Rust, migrations and protocol inputs, including uncommitted changes.
    files = sorted(
        p
        for p in (ROOT / "server/src").rglob("*")
        if p.suffix in (".rs", ".toml", ".proto", ".sql")
    )
    files.extend(sorted((ROOT / "server/proto").rglob("*.proto")))
    identity = {p.relative_to(ROOT).as_posix(): ev.source_identity(p) for p in paths}
    identity["server/build-input-tree"] = ev.digest(
        {p.relative_to(ROOT).as_posix(): ev.source_identity(p) for p in files}
    )
    identity["server/Cargo.toml"] = ev.source_identity(ROOT / "server/Cargo.toml")
    return identity


def freeze(binary, phase, per_family, samples, starts, repetitions, baseline_raw=None):
    ev.require(
        Path(binary).resolve().parent.name == "release", "release_binary_required"
    )
    ev.require(
        per_family > 0 and samples >= 5 and starts >= 3 and repetitions >= 3,
        "insufficient_sampling_plan",
    )
    split = "held_out" if phase == "qualification" else "pilot"
    fixture = cases.fixture(split, per_family)
    workload = {
        "build_profile": "release",
        "hardware_class": f"{platform.system()}-{platform.machine()}-local-shared",
        "concurrency": 1,
        "cache_state": "warm-process-warm-database",
        "sample_count": samples,
        "corpus_size": 200,
        "eligible_fraction": 1.0,
        "history_depth": 0,
        "database_bytes": MAX_DATABASE_BYTES,
        "artifact_bytes": 0,
        "database_bytes_semantics": "upper_bound; exact pg_database_size in raw metadata",
        "rerun_policy": "three-or-more-fixed-repetitions; retain_all; no_pass_seeking",
        "repetitions": repetitions,
    }
    workloads = {
        "retrieval": workload,
        "ledger_append": {
            **workload,
            "concurrency": 4,
            "corpus_size": 0,
            "history_depth": 32,
            "history_growth": "each of four worker histories grows by assigned appends",
        },
        "cold_readiness": {
            **workload,
            "sample_count": starts,
            "corpus_size": 200,
            "cache_state": "new-server-process; warm-database-and-OS-cache",
            "boundary": "process_spawn_to_ops_readyz_200; 20ms_polling",
        },
    }
    settings = {
        "per_family": per_family,
        "samples": samples,
        "starts": starts,
        "repetitions": repetitions,
    }
    manifest = ev.seal(
        "manifest",
        campaign="p13-local-live-v1",
        revision="1",
        purpose=phase,
        source=sources(),
        build={
            "binary_sha256": binary_hash(binary),
            "profile": "release",
            "python": platform.python_version(),
            "system": platform.system(),
            "architecture": platform.machine(),
            "processor": platform.processor(),
            "logical_cpus": os.cpu_count(),
            "database_image": subprocess.check_output(
                [
                    "docker",
                    "image",
                    "inspect",
                    "pgvector/pgvector:pg16",
                    "--format",
                    "{{.Id}}",
                ],
                text=True,
            ).strip(),
            "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        },
        corpus_hash=ev.digest(fixture["histories"]),
        query_hash=ev.digest(
            [{"id": c["id"], "ask": c["ask"]} for c in fixture["cases"]]
        ),
        grader=ev.source_identity(ev.GRADER_PATH),
        cases=fixture["cases"],
        runbook={
            "identity": "p13-live@1",
            "steps": ["resolveSources", "buildIndex", "verify", "cutover"],
        },
        retrieval={"engine": "postgres", "top_k": 10, "context_chars": 4096},
        model={"identity": "deterministic-evidence-renderer-v1", "paid_calls": 0},
        thresholds={"all_checks_pass": True},
        failure_treatment="retain_all",
        resource_limits={
            "paid_calls": 0,
            "max_http_calls": 10000,
            "max_wall_seconds": 900,
            "max_database_bytes": MAX_DATABASE_BYTES,
            "context_chars": 4096,
        },
        decision_d6={
            "accepted": "user instruction in session",
            "population": "fictional fact histories; six families",
            "minimum_supported_correctness": 0.95,
            "maximum_unauthorized_disclosures": 0,
            "minimum_paired_improvement": 0.05,
            "comparison_arm": "conventional_retrieval",
            "uncertainty": "paired history bootstrap 95%; 10000 resamples; seed 13013",
            "model_quality_qualified": False,
        },
        latency_workloads=workloads,
        runner_settings=settings,
    )
    if baseline_raw is not None:
        raw = ev.read(baseline_raw)
        ev.envelope(raw, "raw")
        baseline_manifest = ev.read(
            Path(baseline_raw).parent / (raw["manifest_id"].replace(":", "-") + ".json")
        )
        ev.validate_raw(baseline_manifest, raw)
        ev.require(
            baseline_manifest["latency_workloads"] == workloads,
            "baseline_workload_mismatch",
        )
        for field in ("binary_sha256", "processor", "logical_cpus", "database_image"):
            ev.require(
                baseline_manifest["build"][field] == manifest["build"][field],
                "baseline_environment_mismatch",
            )
        ev.require(
            baseline_manifest["runner_settings"] == settings,
            "baseline_population_mismatch",
        )
        baseline = calibration(baseline_manifest, raw["latency_repetitions"])
        ev.require(
            len(baseline) == 3 and all(v["complete"] for v in baseline.values()),
            "incomplete_calibration_baseline",
        )
        manifest["latency_policy"] = {
            "baseline_raw_id": raw["id"],
            "absolute_p95_ms": {
                k: v["proposed_absolute_p95_ms"] for k, v in baseline.items()
            },
            "baseline_p95_ms": {k: v["p95_max_ms"] for k, v in baseline.items()},
            "relative_limit": 0.20,
            "consecutive_repetitions": 2,
            "scope": "local reporting thresholds only; no release SLO or required CI gate",
            "baseline_update": "new reviewed manifest; retain all previous baselines and confirmations",
        }
        manifest = ev.seal(
            "manifest",
            **{
                k: v
                for k, v in manifest.items()
                if k not in ("id", "kind", "schema_version")
            },
        )
    if phase == "qualification":
        ev.require(
            baseline_raw is not None, "qualification_requires_calibration_baseline"
        )
    return manifest


def apply_fixture(rig, fixture):
    client = rig.client
    shape = {
        "apiVersion": "munarium.ioka.io/v1",
        "kind": "Shape",
        "metadata": {"name": "p13-docs", "version": 1},
        "spec": {
            "chunking": {"strategy": "para@1", "max_chars": 4096},
            "indexing": {"rrf_k": 60, "candidate_n": 50},
        },
    }
    client.call("POST", "/v1/shapes", json.dumps(shape))
    collections = [
        {
            "name": h["id"],
            "shape": "p13-docs@1",
            "accessLevel": 0,
            "sources": {"filenamePrefix": h["id"] + "/", "mediaTypes": ["text/plain"]},
        }
        for h in fixture["histories"]
    ]
    collections.append(
        {
            "name": "p13-latency",
            "shape": "p13-docs@1",
            "accessLevel": 0,
            "sources": {"filenamePrefix": "latency/", "mediaTypes": ["text/plain"]},
        }
    )
    runbook = {
        "apiVersion": "munarium.ioka.io/v1",
        "kind": "Runbook",
        "metadata": {"name": "p13-live", "version": 1},
        "spec": {
            "sources": {"container": "sources", "prefix": ""},
            "collections": collections,
            "retrieval": {"topK": 10, "candidateN": 50, "rrfK": 60},
            "steps": [
                {"resolveSources": {}},
                {"buildIndex": {}},
                {"verify": {}},
                {"cutover": {"approval": "required"}},
            ],
        },
    }
    client.call("POST", "/v1/runbooks", json.dumps(runbook))
    states = {}
    before = time.perf_counter()
    for history in fixture["histories"]:
        vid = client.call("POST", "/v1/versions", {})["version_id"]
        mapping, responses = {}, []
        for event in history["events"]:
            body = {
                "claim_type": event["kind"],
                "subject": history["id"],
                "key": "value",
                "value": event["value"],
                "evidence": {"source_path": event["id"], "event_at": event["at"]},
            }
            if "supersedes" in event:
                body["supersedes_id"] = mapping[event["supersedes"]]
            response = client.call("POST", f"/v1/versions/{vid}/claims", body)
            mapping[event["id"]] = response["claim"]["id"]
            responses.append(response)
            client.call(
                "POST",
                "/v1/ingest",
                {
                    "filename": event["id"],
                    "media_type": "text/plain",
                    "content_base64": base64.b64encode(
                        json.dumps(event).encode()
                    ).decode(),
                },
            )
        note_started = time.perf_counter()
        note_versions = {
            str(event["at"]): cases.resolve_notes(history["events"], event["at"])
            for event in history["events"]
        }
        states[history["id"]] = {
            "version": vid,
            "claims": responses,
            "note_versions": note_versions,
            "notes_update_ms": (time.perf_counter() - note_started) * 1000,
            "notes_bytes": len(json.dumps(note_versions).encode()),
        }
    for i in range(200):
        text = f"p13needle{i:04} fictional retrieval document {i} with distinct evidence value {i}."
        client.call(
            "POST",
            "/v1/ingest",
            {
                "filename": f"latency/{i}",
                "media_type": "text/plain",
                "content_base64": base64.b64encode(text.encode()).decode(),
            },
        )
    result = client.call("POST", "/v1/runbooks/p13-live@1/runs")
    deadline = time.monotonic() + 180
    while result["state"] != "done":
        ev.require(result["state"] != "failed", "fixture_index_build_failed")
        ev.require(time.monotonic() < deadline, "fixture_index_build_timeout")
        status = client.call("GET", f"/v1/runs/{result['run_id']}")
        waiting = [s for s in status["steps"] if s["state"] == "awaiting_approval"]
        if waiting:
            result = client.call(
                "POST",
                f"/v1/runs/{result['run_id']}/steps/{waiting[0]['ordinal']}/approve",
            )
        else:
            result = status
            time.sleep(0.02)
    token = client.call(
        "POST",
        "/v1/access-tokens",
        {
            "uid": client.uid,
            "access_level": 0,
            "compartments": [],
            "scopes": ["query"],
            "ttl_secs": 1800,
        },
        token=rig.mgmt,
    )["token"]
    for history in fixture["histories"]:
        if not history["authorized_now"]:
            # First prove the same token had access, then revoke via current collection clearance.
            prior = client.call(
                "POST",
                "/v1/search",
                {
                    "query": history["id"],
                    "top_k": 10,
                    "filter": {"collections": [history["id"]]},
                },
                token=token,
            )
            ev.require(bool(prior["hits"]), "revocation_control_never_authorized")
            states[history["id"]]["index_version"] = prior["envelope"]["index_version"]
            client.call(
                "POST",
                "/v1/collections",
                {"name": history["id"], "shape_ref": "p13-docs@1", "access_level": 1},
            )
    return (
        states,
        token,
        {
            "duration_ms": (time.perf_counter() - before) * 1000,
            "calls": client.calls,
            "event_count": sum(len(h["events"]) for h in fixture["histories"]),
            "storage_bytes": len(json.dumps(fixture["histories"]).encode()),
            "allocation": "shared fixture setup; includes actual ledger append and index maintenance",
            "notes_update_ms": sum(s["notes_update_ms"] for s in states.values()),
            "notes_bytes": sum(s["notes_bytes"] for s in states.values()),
        },
    )


def query_arm(client, token, history, arm, state):
    start = time.perf_counter()
    try:
        search = client.call(
            "POST",
            "/v1/search",
            {
                "query": history["id"],
                "top_k": 10,
                "filter": {"collections": [history["id"]]},
                "index_version": state.get("index_version"),
            },
            token=token,
        )
        permitted = True
    except CallError as exc:
        if exc.status != 404 or history["authorized_now"]:
            raise
        search, permitted = {"hits": [], "authorization_status": 404}, False
    search_ms = (time.perf_counter() - start) * 1000
    if permitted and not history["authorized_now"]:
        # Fail before disallowed evidence can enter the responder context.
        return {
            "hits": search["hits"],
            "completion": {"text": "insufficient evidence"},
        }, {
            "unauthorized_disclosures": len(search["hits"]),
            "authorization_bypass": True,
            "search": search,
            "selected_events": [],
        }
    expected_sources = {e["id"]: e for e in history["events"]}
    eligible = []
    for hit in search["hits"]:
        event = json.loads(hit["text"])
        ev.require(
            event == expected_sources.get(hit["source_path"]),
            "retrieved_evidence_mismatch",
        )
        expected_hash = hashlib.sha256(json.dumps(event).encode()).hexdigest()
        ev.require(
            hit.get("source_content_hash") == expected_hash,
            "retrieved_source_hash_mismatch",
        )
        if event["at"] <= history["as_of"]:
            eligible.append(event)
    ledger = None
    if arm == "no_memory" or not permitted:
        selected = []
    elif arm == "versioned_notes":
        pin = max(
            int(at) for at in state["note_versions"] if int(at) <= history["as_of"]
        )
        selected = state["note_versions"][str(pin)]
    elif arm == "conventional_retrieval":
        selected = cases.resolve_notes(eligible, history["as_of"])
    elif arm == "governance_ablation":
        selected = [e for e in eligible if e["value"] != cases.REMOVED]
    else:
        ledger = client.call(
            "GET",
            f"/v1/versions/{state['version']}/facts?as_of_seq={history['as_of']}&statuses=accepted,disputed",
        )
        active = {
            f.get("evidence", {}).get("source_path")
            for f in ledger["facts"]
            if f["value"] != cases.REMOVED
        }
        selected = [e for e in eligible if e["id"] in active]
    turn = cases.respond(selected)
    return turn, {
        "search": search,
        "ledger": ledger,
        "selected_events": selected,
        "search_ms": search_ms,
        "unauthorized_disclosures": 0,
        "current_authorization": permitted,
        "requested_index_version": state.get("index_version"),
        "maintenance_events": 0 if arm == "no_memory" else len(history["events"]),
    }


def sample_call(function, sample_id, rig):
    before = time.perf_counter()
    try:
        valid, evidence = function()
        outcome, reason = (
            ("passed", "correct") if valid else ("failed", "incorrect_response")
        )
    except (OSError, ValueError, KeyError, RuntimeError) as exc:
        outcome, reason, evidence = "harness_error", type(exc).__name__, None
    duration = (time.perf_counter() - before) * 1000
    return {
        "id": str(sample_id),
        "outcome": outcome,
        "reason": reason,
        "duration_ms": duration,
        "stages_ms": {"client_roundtrip_and_validation": duration},
        "memory_bytes": rig.memory_bytes(),
        "candidate_work": None,
        "evidence": evidence,
    }


def measurement_plan(manifest):
    return {
        name: [
            {
                "repetition": r,
                "workload_id": ev.digest(definition),
                "elapsed_ms": None,
                "samples": [
                    {
                        "id": str(i),
                        "outcome": "unavailable",
                        "reason": "unexecuted",
                        "duration_ms": None,
                        "stages_ms": {},
                        "memory_bytes": None,
                        "candidate_work": None,
                    }
                    for i in range(definition["sample_count"])
                ],
            }
            for r in range(manifest["runner_settings"]["repetitions"])
        ]
        for name, definition in manifest["latency_workloads"].items()
    }


def measure_latency(rig, manifest, token, measured):
    settings = manifest["runner_settings"]
    for name, definition in manifest["latency_workloads"].items():
        for repetition in range(settings["repetitions"]):
            run_record = measured[name][repetition]
            samples = run_record["samples"]
            print(
                f"Measuring {name}, repetition {repetition + 1}/{settings['repetitions']}",
                flush=True,
            )
            versions = []
            if name == "ledger_append":
                for _ in range(4):
                    vid = rig.client.call("POST", "/v1/versions", {})["version_id"]
                    versions.append(vid)
                    # Preseed each worker's lineage outside the timed interval.
                    rig.client.call(
                        "POST",
                        f"/v1/versions/{vid}/events",
                        {
                            "claims": [
                                {
                                    "claim_type": "fact",
                                    "subject": f"seed-{j}",
                                    "key": "value",
                                    "value": str(j),
                                }
                                for j in range(32)
                            ]
                        },
                    )
            before = time.perf_counter()
            if name == "retrieval":

                def retrieval(i):
                    value = i % 200
                    response = rig.client.call(
                        "POST",
                        "/v1/search",
                        {
                            "query": f"p13needle{value:04}",
                            "top_k": 10,
                            "filter": {"collections": ["p13-latency"]},
                        },
                        token=token,
                    )
                    return bool(
                        response.get("envelope", {}).get("index_version")
                    ) and any(
                        h["source_path"] == f"latency/{value}" for h in response["hits"]
                    ), response

                for i in range(definition["sample_count"]):
                    samples[i] = sample_call(lambda i=i: retrieval(i), i, rig)
            elif name == "ledger_append":

                def worker(
                    worker_id, definition=definition, versions=versions, samples=samples
                ):
                    result = []
                    for i in range(worker_id, definition["sample_count"], 4):

                        def append(i=i):
                            response = rig.client.call(
                                "POST",
                                f"/v1/versions/{versions[worker_id]}/claims",
                                {
                                    "claim_type": "fact",
                                    "subject": f"sample-{i}",
                                    "key": "value",
                                    "value": str(i),
                                },
                            )
                            return response["claim"][
                                "status"
                            ] == "accepted" and response["claim"]["value"] == str(
                                i
                            ), response

                        samples[i] = sample_call(append, i, rig)
                        result.append(samples[i])
                    return result

                with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                    list(pool.map(worker, range(4)))
            else:
                for i in range(definition["sample_count"]):
                    rig.stop()
                    try:
                        startup = rig.start()
                        samples[i] = {
                            "id": str(i),
                            "outcome": "passed",
                            "reason": "readyz_200",
                            "duration_ms": startup["duration_ms"],
                            "stages_ms": {"spawn_to_ready": startup["duration_ms"]},
                            "memory_bytes": rig.memory_bytes(),
                            "candidate_work": None,
                            "evidence": startup,
                        }
                    except (OSError, RuntimeError) as exc:
                        samples[i] = {
                            "id": str(i),
                            "outcome": "harness_error",
                            "reason": type(exc).__name__,
                            "duration_ms": None,
                            "stages_ms": {},
                            "memory_bytes": None,
                            "candidate_work": None,
                        }
            run_record["elapsed_ms"] = (time.perf_counter() - before) * 1000
            run_record["database_bytes"] = rig.database_bytes()
    return measured


def paired_summary(manifest, raw, grading):
    score = {s["id"]: s for s in grading["scores"]}
    rows = {r["id"]: r for r in raw["rows"]}
    per_arm = {}
    for arm in cases.ARMS:
        selected = [c for c in manifest["cases"] if c["arm"] == arm]
        per_arm[arm] = {
            "planned": len(selected),
            "correct": sum(score[c["id"]]["outcome"] == "passed" for c in selected),
            "unauthorized_disclosures": sum(
                rows[c["id"]].get("observations", {}).get("unauthorized_disclosures", 0)
                for c in selected
            ),
            "abstentions": sum(
                "insufficient evidence"
                in rows[c["id"]].get("turn", {}).get("completion", {}).get("text", "")
                for c in selected
            ),
            "incomplete": sum(score[c["id"]]["outcome"] == "not_run" for c in selected),
            "authorization_bypasses": sum(
                bool(rows[c["id"]].get("observations", {}).get("authorization_bypass"))
                for c in selected
            ),
            "stale_or_conflicting": sum(
                any(
                    check[0] == "must_not_cite" and check[1] is False
                    for check in (score[c["id"]].get("checks") or {}).get(
                        "evidence", []
                    )
                )
                for c in selected
                if c["family"] != "revoked"
            ),
            "citation_failures": sum(
                any(
                    check[0] in ("cite_all", "cite_any", "evidence_shape")
                    and check[1] is False
                    for check in (score[c["id"]].get("checks") or {}).get(
                        "evidence", []
                    )
                )
                for c in selected
            ),
        }
    histories = sorted({c["history_id"] for c in manifest["cases"]})
    differences = [
        int(score[h + ":munarium"]["outcome"] == "passed")
        - int(score[h + ":conventional_retrieval"]["outcome"] == "passed")
        for h in histories
    ]
    rng = random.Random(13013)
    bootstrap = sorted(
        sum(rng.choices(differences, k=len(differences))) / len(differences)
        for _ in range(10000)
    )
    delta = sum(differences) / len(differences)
    lower, upper = bootstrap[249], bootstrap[9749]
    current = per_arm["munarium"]
    d6 = manifest["decision_d6"]
    mandatory = (
        all(
            a["unauthorized_disclosures"] == 0 and a["authorization_bypasses"] == 0
            for a in per_arm.values()
        )
        and current["correct"] / current["planned"]
        >= d6["minimum_supported_correctness"]
    )
    incomplete = any(
        r["status"] in ("failed", "interrupted", "unexecuted") for r in raw["rows"]
    )
    verdict = (
        "incomplete"
        if incomplete
        else "rejected"
        if not mandatory or upper < d6["minimum_paired_improvement"]
        else "qualified"
        if lower >= d6["minimum_paired_improvement"]
        else "inconclusive"
    )
    if manifest["purpose"] != "qualification":
        verdict = "diagnostic_only"
    return {
        "d6_verdict": verdict,
        "per_arm": per_arm,
        "independent_histories": len(histories),
        "paired_difference": delta,
        "paired_bootstrap_95": [lower, upper],
        "model_quality_qualified": False,
        "paired_history_differences": differences,
    }


def calibration(manifest, measurements):
    report = {}
    for name, runs in measurements.items():
        summaries = [
            ev.latency_report(r["samples"], elapsed_ms=r["elapsed_ms"])
            if r["elapsed_ms"] is not None
            else {
                "offered": len(r["samples"]),
                "correct": sum(s["outcome"] == "passed" for s in r["samples"]),
                "p95_ms": None,
                "goodput_per_second": None,
                "measurement": "interrupted_or_unexecuted",
            }
            for r in runs
        ]
        p95 = [s["p95_ms"] for s in summaries if s["p95_ms"] is not None]
        complete = all(
            s["correct"] == s["offered"] and s["p95_ms"] is not None for s in summaries
        )
        report[name] = {
            "repetitions": summaries,
            "p95_min_ms": min(p95) if p95 else None,
            "p95_max_ms": max(p95) if p95 else None,
            "complete": complete,
            "baseline_workload_id": ev.digest(manifest["latency_workloads"][name]),
            "proposed_absolute_p95_ms": math.ceil(max(p95) * 1.5)
            if p95 and complete
            else None,
            "proposed_relative_regression": 0.20,
            "confirmation": "two consecutive complete runs; retain both",
            "required_ci_gate": False,
            "scope": "local shared workstation; proposals require baseline review",
        }
        policy = manifest.get("latency_policy")
        if policy is not None:
            absolute = policy["absolute_p95_ms"][name]
            relative = policy["baseline_p95_ms"][name] * (1 + policy["relative_limit"])
            breaches = [
                s["p95_ms"] is not None and s["p95_ms"] > relative for s in summaries
            ]
            width = policy["consecutive_repetitions"]
            sustained = any(
                all(breaches[i : i + width]) for i in range(len(breaches) - width + 1)
            )
            report[name]["frozen_budget"] = {
                "absolute_p95_ms": absolute,
                "relative_p95_ms": relative,
                "absolute_breach": any(v > absolute for v in p95),
                "sustained_relative_breach": sustained,
                "verdict": "incomplete"
                if not complete
                else "regressed"
                if any(v > absolute for v in p95) or sustained
                else "within_local_budget",
            }
    return report


def run(manifest, binary, directory, run_id, frozen_commit=None):
    ev.validate_manifest(manifest)
    ev.require(manifest["source"] == sources(), "live_source_changed")
    ev.require(
        manifest["build"]["binary_sha256"] == binary_hash(binary), "live_binary_changed"
    )
    settings = manifest["runner_settings"]
    fixture = cases.fixture(
        "held_out" if manifest["purpose"] == "qualification" else "pilot",
        settings["per_family"],
    )
    ev.require(
        ev.digest(fixture["histories"]) == manifest["corpus_hash"],
        "live_corpus_changed",
    )
    rows = [
        {
            "id": c["id"],
            "status": "unexecuted",
            "reason": "not_started",
            "usage": {"input_tokens": 0, "output_tokens": 0},
        }
        for c in manifest["cases"]
    ]
    raw_fields = {
        "manifest_id": manifest["id"],
        "source": manifest["source"],
        "purpose": manifest["purpose"],
        "run_id": run_id,
        "rows": rows,
        "frozen_commit": frozen_commit,
    }
    ev.validate_raw(
        manifest, ev.seal("raw", **raw_fields)
    )  # Commit proof precedes any qualification execution.
    directory = Path(directory)
    journal = directory / (run_id + ".journal.jsonl")
    directory.mkdir(parents=True, exist_ok=True)
    deadline = time.monotonic() + manifest["resource_limits"]["max_wall_seconds"]
    owned_dir = ROOT / "server/scratch" / ("p13-" + run_id)
    rig = LocalRig(binary, owned_dir)
    rig.client.deadline = deadline
    rig.client.limit = manifest["resource_limits"]["max_http_calls"]
    metadata, measurements = {}, measurement_plan(manifest)
    with journal.open("x", encoding="utf-8") as log:
        log.write(
            json.dumps({"manifest_id": manifest["id"], "planned_rows": rows}) + "\n"
        )
        log.flush()
        os.fsync(log.fileno())
        try:
            with rig:
                ev.require(
                    rig.image_id == manifest["build"]["database_image"],
                    "database_image_changed",
                )
                print(
                    "Preparing isolated fictional corpus and live ledger histories",
                    flush=True,
                )
                states, token, maintenance = apply_fixture(rig, fixture)
                metadata = {
                    "database_image": rig.image_id,
                    "database_bytes_before": rig.database_bytes(),
                    "maintenance": maintenance,
                    "build": manifest["build"],
                    "host_class": manifest["latency_workloads"]["retrieval"][
                        "hardware_class"
                    ],
                }
                ev.require(
                    metadata["database_bytes_before"] <= MAX_DATABASE_BYTES,
                    "database_budget_exceeded",
                )
                histories = {h["id"]: h for h in fixture["histories"]}
                for row, case in zip(rows, manifest["cases"]):
                    ev.require(time.monotonic() < deadline, "wall_time_limit_reached")
                    before = time.perf_counter()
                    try:
                        turn, observations = query_arm(
                            rig.client,
                            token,
                            histories[case["history_id"]],
                            case["arm"],
                            states[case["history_id"]],
                        )
                        row.update(
                            status="completed",
                            reason="live_response",
                            turn=turn,
                            observations=observations,
                        )
                    except (OSError, RuntimeError, ValueError, KeyError) as exc:
                        row.update(status="failed", reason=type(exc).__name__)
                    row["duration_ms"] = (time.perf_counter() - before) * 1000
                    log.write(json.dumps(row) + "\n")
                    log.flush()
                    os.fsync(log.fileno())
                measure_latency(rig, manifest, token, measurements)
                metadata["database_bytes_after"] = rig.database_bytes()
                ev.require(
                    metadata["database_bytes_after"] <= MAX_DATABASE_BYTES,
                    "database_budget_exceeded",
                )
                ev.require(
                    manifest["source"] == sources()
                    and manifest["build"]["binary_sha256"] == binary_hash(binary),
                    "source_changed_during_run",
                )
        except (KeyboardInterrupt, OSError, RuntimeError, ValueError, KeyError) as exc:
            metadata["runner_error"] = type(exc).__name__
            for row in rows:
                if row["status"] == "unexecuted":
                    row["reason"] = (
                        "runner_interrupted"
                        if isinstance(exc, KeyboardInterrupt)
                        else "runner_failed"
                    )
        metadata["cleanup_completed"] = (
            rig.process is None and not rig.container_created
        )
    raw = ev.seal(
        "raw", **raw_fields, metadata=metadata, latency_repetitions=measurements
    )
    raw_path = ev.write(directory, raw)
    grading = ev.grade(manifest, ev.read(raw_path))
    ev.write(directory, grading)
    report = ev.seal(
        "live_analysis",
        manifest_id=manifest["id"],
        raw_id=raw["id"],
        grading_id=grading["id"],
        source=manifest["source"],
        governance=paired_summary(manifest, raw, grading),
        calibration=calibration(manifest, measurements),
    )
    path = directory / ("analysis-" + report["id"].split(":")[1] + ".json")
    with path.open("x", encoding="utf-8") as stream:
        json.dump(report, stream, indent=2)
        stream.write("\n")
    print(
        json.dumps(
            {
                "raw": raw["id"],
                "grading": grading["id"],
                "analysis": report["id"],
                "governance": report["governance"],
                "cleanup_completed": metadata["cleanup_completed"],
            }
        ),
        flush=True,
    )
    if metadata.get("runner_error") or not metadata["cleanup_completed"]:
        return 3
    if len(report["calibration"]) != 3 or not all(
        v["complete"] for v in report["calibration"].values()
    ):
        return 3
    verdict = report["governance"]["d6_verdict"]
    return (
        1
        if verdict == "rejected"
        else 3
        if verdict in ("incomplete", "inconclusive")
        else 0
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    freezing = commands.add_parser("freeze")
    freezing.add_argument("--phase", choices=("pilot", "qualification"), required=True)
    freezing.add_argument("--per-family", type=int, default=2)
    freezing.add_argument("--samples", type=int, default=60)
    freezing.add_argument("--starts", type=int, default=5)
    freezing.add_argument("--repetitions", type=int, default=3)
    executing = commands.add_parser("run")
    executing.add_argument("--manifest", type=Path, required=True)
    executing.add_argument("--run-id", required=True)
    executing.add_argument("--frozen-commit")
    freezing.add_argument("--baseline-raw", type=Path)
    for command in (freezing, executing):
        command.add_argument("--binary", type=Path, required=True)
        command.add_argument(
            "--directory", type=Path, default=ROOT / "server/conformance/results"
        )
    args = parser.parse_args(argv)
    try:
        if args.command == "freeze":
            print(
                ev.write(
                    args.directory,
                    freeze(
                        args.binary,
                        args.phase,
                        args.per_family,
                        args.samples,
                        args.starts,
                        args.repetitions,
                        args.baseline_raw,
                    ),
                )
            )
            return 0
        ev.require(
            args.run_id and all(c.isalnum() or c in "-_" for c in args.run_id),
            "invalid_run_id",
        )
        return run(
            ev.read(args.manifest),
            args.binary,
            args.directory,
            args.run_id,
            args.frozen_commit,
        )
    except (ValueError, OSError, KeyError, RuntimeError) as exc:
        print(f"Live evaluation rejected: {type(exc).__name__}: {exc}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
