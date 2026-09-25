# SPDX-License-Identifier: Apache-2.0
"""Versioned, content-addressed evaluation records; standard library, offline only.

CLI: frozen_eval.py verify DIRECTORY | grade MANIFEST RAW DIRECTORY
The grade command appends a new record; it never rewrites raw evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GRADER_PATH = ROOT / "docs/lab/example/grade.py"
spec = importlib.util.spec_from_file_location("example_grader", GRADER_PATH)
grader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(grader)
ID = re.compile(r"sha256:[0-9a-f]{64}\Z")
STATUSES = {"completed", "failed", "abstained", "interrupted", "unexecuted"}
KINDS = {"manifest", "raw", "grading"}


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def digest(value):
    return (
        "sha256:"
        + hashlib.sha256(
            json.dumps(
                value,
                sort_keys=True,
                separators=(",", ":"),
                ensure_ascii=False,
                allow_nan=False,
            ).encode("utf-8")
        ).hexdigest()
    )


def source_identity(path):
    # Portable across Git's CRLF/LF checkout conversion.
    return (
        "sha256:"
        + hashlib.sha256(path.read_bytes().replace(b"\r\n", b"\n")).hexdigest()
    )


def seal(kind, **fields):
    record = {"schema_version": 1, "kind": kind, **fields}
    record["id"] = digest(record)
    return record


def envelope(record, kind):
    require(isinstance(record, dict), "record_not_object")
    require(
        type(record.get("schema_version")) is int and record["schema_version"] == 1,
        "unsupported_schema",
    )
    require(record.get("kind") == kind, "record_kind_mismatch")
    require(
        record.get("id") == digest({k: v for k, v in record.items() if k != "id"}),
        "record_identity_mismatch",
    )


def read(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate_json_key")
            result[key] = value
        return result

    return json.loads(Path(path).read_text(encoding="utf-8"), object_pairs_hook=unique)


def write(directory, record):
    envelope(record, record["kind"])
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / (record["id"].replace(":", "-") + ".json")
    # Exclusive creation also refuses accidental overwrite with identical bytes.
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(record, stream, indent=2, ensure_ascii=False, allow_nan=False)
        stream.write("\n")
    return path


def ids(rows):
    require(isinstance(rows, list) and bool(rows), "empty_or_missing_cases")
    require(
        all(
            isinstance(r, dict) and isinstance(r.get("id"), str) and r["id"]
            for r in rows
        ),
        "invalid_case_id",
    )
    names = [r["id"] for r in rows]
    require(len(set(names)) == len(names), "duplicate_case_id")
    return names


def validate_manifest(manifest):
    envelope(manifest, "manifest")
    ids(manifest.get("cases"))
    for field in (
        "campaign",
        "revision",
        "build",
        "runbook",
        "retrieval",
        "model",
        "thresholds",
        "failure_treatment",
        "resource_limits",
        "source",
    ):
        require(bool(manifest.get(field)), "missing_" + field)
    for field in ("campaign", "revision"):
        require(isinstance(manifest[field], str), "invalid_" + field)
    for field in ("build", "runbook", "retrieval", "model", "resource_limits"):
        require(isinstance(manifest[field], dict), "invalid_" + field)
    source = manifest["source"]
    require(
        isinstance(source, dict)
        and source
        and all(
            isinstance(k, str) and isinstance(v, str) and ID.fullmatch(v)
            for k, v in source.items()
        ),
        "invalid_source_identity",
    )
    require(
        manifest.get("purpose") in ("pilot", "diagnostic", "qualification"),
        "invalid_purpose",
    )
    require(
        manifest.get("failure_treatment") == "retain_all", "invalid_failure_treatment"
    )
    require(
        isinstance(manifest["thresholds"], dict)
        and set(manifest["thresholds"]) == {"all_checks_pass"}
        and manifest["thresholds"]["all_checks_pass"] is True,
        "unsupported_thresholds",
    )
    for field in ("corpus_hash", "query_hash", "grader"):
        require(
            isinstance(manifest.get(field), str) and ID.fullmatch(manifest[field]),
            "invalid_" + field,
        )
    require(
        manifest["query_hash"]
        == digest([{"id": c["id"], "ask": c.get("ask")} for c in manifest["cases"]]),
        "query_hash_mismatch",
    )
    for case in manifest["cases"]:
        for field in ("history_id", "task_id", "ask", "caller", "expect"):
            require(bool(case.get(field)), "missing_case_" + field)
        for field in ("history_id", "task_id", "ask"):
            require(isinstance(case[field], str), "invalid_case_" + field)
        require(isinstance(case["caller"], dict), "invalid_caller")
        require(case.get("split") in ("pilot", "held_out"), "invalid_split")
        require(isinstance(case["expect"], dict), "invalid_expectation")
        allowed = {
            "cite_any",
            "cite_all",
            "must_not_cite",
            "conflict",
            "contains_all",
            "insufficient",
        }
        expect = case["expect"]
        require(set(expect) <= allowed, "unknown_expectation")
        require(
            bool(set(expect) & {"cite_any", "cite_all", "must_not_cite", "conflict"}),
            "missing_evidence_expectation",
        )
        for field in ("cite_any", "cite_all", "must_not_cite", "contains_all"):
            if field in expect:
                require(
                    isinstance(expect[field], list)
                    and bool(expect[field])
                    and all(isinstance(x, str) and x for x in expect[field]),
                    "invalid_" + field,
                )
        if "insufficient" in expect:
            require(expect["insufficient"] is True, "invalid_insufficient")
        if "conflict" in expect:
            conflict = expect["conflict"]
            require(
                isinstance(conflict, dict)
                and bool(conflict)
                and set(conflict) <= {"cite_any", "cite_all", "sides"},
                "invalid_conflict",
            )
            for field, values in conflict.items():
                require(
                    isinstance(values, list) and bool(values),
                    "invalid_conflict_" + field,
                )
                if field == "sides":
                    require(
                        all(
                            isinstance(side, list)
                            and bool(side)
                            and all(isinstance(x, str) and x for x in side)
                            for side in values
                        ),
                        "invalid_conflict_sides",
                    )
                else:
                    require(
                        all(isinstance(x, str) and x for x in values),
                        "invalid_conflict_" + field,
                    )
    assignments = {}
    tasks = set()
    for case in manifest["cases"]:
        history = case["history_id"]
        require(
            history not in assignments or assignments[history] == case["split"],
            "history_split_leakage",
        )
        assignments[history] = case["split"]
        task = (history, case["task_id"])
        require(task not in tasks, "duplicate_history_task")
        tasks.add(task)
    if manifest["purpose"] == "qualification":
        require(bool(manifest.get("decision_d6")), "missing_decision_d6")
        require(
            all(c["split"] == "held_out" for c in manifest["cases"]),
            "qualification_split",
        )


def validate_raw(manifest, raw):
    validate_manifest(manifest)
    envelope(raw, "raw")
    require(raw.get("manifest_id") == manifest["id"], "manifest_identity_mismatch")
    require(raw.get("source") == manifest["source"], "source_mismatch")
    actual = ids(raw.get("rows"))
    expected = ids(manifest["cases"])
    require(not set(actual) - set(expected), "extra_case_id")
    require(not set(expected) - set(actual), "missing_case_id")
    require(raw.get("purpose") == manifest["purpose"], "purpose_mismatch")
    require(isinstance(raw.get("run_id"), str) and raw["run_id"], "missing_run_id")
    if manifest["purpose"] == "qualification":
        commit = raw.get("frozen_commit")
        require(
            isinstance(commit, str) and re.fullmatch(r"[0-9a-f]{40}", commit),
            "missing_frozen_commit",
        )
        relative = (
            "server/conformance/results/" + manifest["id"].replace(":", "-") + ".json"
        )
        saved = subprocess.run(
            [
                "git",
                "-c",
                f"safe.directory={ROOT.as_posix()}",
                "-C",
                str(ROOT),
                "show",
                f"{commit}:{relative}",
            ],
            capture_output=True,
            check=False,
        )
        require(saved.returncode == 0, "frozen_manifest_not_committed")
        require(json.loads(saved.stdout) == manifest, "committed_manifest_mismatch")
    for row in raw["rows"]:
        require(row.get("status") in STATUSES, "invalid_outcome")
        require(
            isinstance(row.get("reason"), str) and row["reason"],
            "missing_outcome_reason",
        )
        require("usage" in row, "missing_usage_certainty")
        usage = row["usage"]
        require(
            usage is None
            or (
                isinstance(usage, dict)
                and set(usage) == {"input_tokens", "output_tokens"}
                and all(
                    v is None or (type(v) is int and v >= 0) for v in usage.values()
                )
            ),
            "invalid_usage",
        )
        if row["status"] in ("completed", "abstained"):
            require(isinstance(row.get("turn"), dict), "missing_turn")


def grade(manifest, raw, *, revision="1"):
    validate_raw(manifest, raw)
    require(isinstance(revision, str) and bool(revision), "invalid_grading_revision")
    rows = {r["id"]: r for r in raw["rows"]}
    scores = []
    for case in manifest["cases"]:
        row = rows[case["id"]]
        if row["status"] not in ("completed", "abstained"):
            scores.append(
                {
                    "id": case["id"],
                    "outcome": "failed" if row["status"] == "failed" else "not_run",
                    "reason": row["status"],
                    "checks": None,
                }
            )
            continue
        checked = grader.grade_case(case, row["turn"], True)
        checks = checked["evidence"] + checked["answer"]
        failed = checked["hard_fail"] or any(c[1] is False for c in checks)
        missing = any(c[1] is None for c in checks)
        outcome = "failed" if failed else "not_run" if missing else "passed"
        scores.append(
            {
                "id": case["id"],
                "outcome": outcome,
                "reason": "checks_failed"
                if failed
                else "missing_completion"
                if missing
                else "completed",
                "checks": checked,
            }
        )
    code = (
        1
        if any(s["outcome"] == "failed" for s in scores)
        else 3
        if any(s["outcome"] == "not_run" for s in scores)
        else 0
    )
    fields = {}
    if "latency" in raw:
        fields["latency"] = validate_latency(manifest, raw["latency"])
    return seal(
        "grading",
        manifest_id=manifest["id"],
        raw_id=raw["id"],
        source=raw["source"],
        grader=source_identity(GRADER_PATH),
        evaluator=source_identity(Path(__file__)),
        revision=revision,
        completed=all(s["outcome"] != "not_run" for s in scores),
        exit_code=code,
        scores=scores,
        unknown_usage_rows=sum(
            r["usage"] is None or any(v is None for v in r["usage"].values())
            for r in raw["rows"]
        ),
        **fields,
    )


def verify_bundle(records):
    by_id = {}
    runs = set()
    for record in records:
        require(
            isinstance(record, dict) and record.get("kind") in KINDS,
            "invalid_record_kind",
        )
        envelope(record, record["kind"])
        require(record["id"] not in by_id, "duplicate_result_id")
        by_id[record["id"]] = record
    for record in records:
        if record["kind"] == "manifest":
            validate_manifest(record)
            continue
        require(record.get("manifest_id") in by_id, "missing_manifest")
        manifest = by_id[record["manifest_id"]]
        if record["kind"] == "raw":
            validate_raw(manifest, record)
            run = (record["manifest_id"], record["run_id"])
            require(run not in runs, "duplicate_run_id")
            runs.add(run)
            continue
        require(record.get("raw_id") in by_id, "missing_raw")
        raw = by_id[record["raw_id"]]
        require(record.get("source") == manifest["source"], "source_mismatch")
        validate_raw(manifest, raw)
        require(
            isinstance(record.get("grader"), str) and ID.fullmatch(record["grader"]),
            "invalid_grader",
        )
        require(
            isinstance(record.get("evaluator"), str)
            and ID.fullmatch(record["evaluator"]),
            "invalid_evaluator",
        )
        require(
            ids(record.get("scores")) == ids(manifest["cases"]),
            "score_case_identity_mismatch",
        )
        outcomes = [s.get("outcome") for s in record["scores"]]
        require(
            all(o in ("passed", "failed", "not_run") for o in outcomes),
            "invalid_score_outcome",
        )
        code = 1 if "failed" in outcomes else 3 if "not_run" in outcomes else 0
        require(
            type(record.get("exit_code")) is int and record["exit_code"] == code,
            "grading_exit_mismatch",
        )
        require(
            record.get("completed") is ("not_run" not in outcomes),
            "grading_completion_mismatch",
        )
        # Regrading with a corrected scorer retains the original record. An old
        # implementation can be audited by its pinned source, but cannot back a
        # new measured claim until verified with that implementation or regraded.
        if record["grader"] == source_identity(GRADER_PATH) and record[
            "evaluator"
        ] == source_identity(Path(__file__)):
            require(
                record
                == json.loads(
                    json.dumps(grade(manifest, raw, revision=record["revision"]))
                ),
                "grading_mismatch",
            )
    return by_id


def check_claims(root):
    """Only explicit HTML markers opt a claim into the gate (including failures)."""
    root = Path(root)
    directory = root / "server/conformance/results"
    findings = []
    try:
        records = verify_bundle(
            [read(p) for p in sorted(directory.glob("sha256-*.json"))]
        )
    except (ValueError, TypeError, KeyError, OSError) as exc:
        return [f"evaluation records: {exc}"]
    files = list(root.glob("*.md"))
    for subdir in ("docs", "server/docs", "server/conformance/results"):
        files.extend((root / subdir).rglob("*.md"))
    for path in files:
        for line_number, line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), 1
        ):
            if "<!-- measured-result:" not in line:
                continue
            match = re.fullmatch(
                r"\s*<!-- measured-result: (sha256:[0-9a-f]{64}) source: (sha256:[0-9a-f]{64}) -->\s*",
                line,
            )
            reason = None
            if not match:
                reason = "malformed_result_marker"
            elif match[1] not in records:
                reason = "missing_result_id"
            else:
                result = records[match[1]]
                if result["kind"] != "grading":
                    reason = "result_schema_mismatch"
                elif digest(result["source"]) != match[2]:
                    reason = "result_source_mismatch"
                elif result.get("completed") is not True:
                    reason = "incomplete_result"
                elif result.get("grader") != source_identity(GRADER_PATH) or result.get(
                    "evaluator"
                ) != source_identity(Path(__file__)):
                    reason = "historical_grader_requires_revalidation"
            if reason:
                findings.append(f"{path.relative_to(root)}:{line_number}: {reason}")
    return findings


def latency_report(samples, *, elapsed_ms):
    """Nearest-rank percentiles over correct requests; all offered work retained."""
    require(isinstance(samples, list) and samples, "empty_latency_samples")
    require(
        type(elapsed_ms) in (int, float)
        and math.isfinite(elapsed_ms)
        and elapsed_ms > 0,
        "invalid_elapsed_time",
    )
    good = []
    for sample in samples:
        require(
            isinstance(sample, dict)
            and sample.get("outcome")
            in ("passed", "failed", "harness_error", "unavailable"),
            "invalid_latency_outcome",
        )
        duration = sample.get("duration_ms")
        require(
            duration is None
            or (
                type(duration) in (int, float)
                and math.isfinite(duration)
                and duration >= 0
            ),
            "invalid_duration",
        )
        if sample["outcome"] == "passed":
            require(duration is not None, "missing_duration")
            good.append(duration)
    good.sort()
    return {
        "offered": len(samples),
        "correct": len(good),
        "goodput_per_second": len(good) * 1000 / elapsed_ms,
        "p50_ms": good[math.ceil(len(good) * 0.50) - 1] if good else None,
        "p95_ms": good[math.ceil(len(good) * 0.95) - 1] if good else None,
        "min_ms": min(good) if good else None,
        "max_ms": max(good) if good else None,
        "outcomes": {
            k: sum(s["outcome"] == k for s in samples)
            for k in ("passed", "failed", "harness_error", "unavailable")
        },
    }


def validate_latency(manifest, measurements):
    """Reporting only: no latency threshold or automatic CI gate is inferred."""
    workloads = manifest.get("latency_workloads")
    require(
        isinstance(workloads, dict)
        and set(workloads) == {"retrieval", "ledger_append", "cold_readiness"},
        "missing_latency_workloads",
    )
    require(
        isinstance(measurements, dict) and set(measurements) == set(workloads),
        "latency_workload_mismatch",
    )
    reports = {}
    for name, definition in workloads.items():
        require(isinstance(definition, dict), "invalid_workload_definition")
        for field in (
            "build_profile",
            "hardware_class",
            "concurrency",
            "cache_state",
            "sample_count",
            "corpus_size",
            "eligible_fraction",
            "history_depth",
            "database_bytes",
            "artifact_bytes",
            "rerun_policy",
        ):
            require(
                field in definition and definition[field] is not None,
                "missing_workload_" + field,
            )
        for field in ("concurrency", "sample_count"):
            require(
                type(definition[field]) is int and definition[field] > 0,
                "invalid_workload_" + field,
            )
        measurement = measurements[name]
        require(
            isinstance(measurement, dict)
            and measurement.get("workload_id") == digest(definition),
            "workload_identity_mismatch",
        )
        samples = measurement.get("samples")
        require(
            isinstance(samples, list) and len(samples) == definition["sample_count"],
            "latency_sample_count_mismatch",
        )
        sample_ids = ids(samples)
        require(
            sample_ids == [str(i) for i in range(definition["sample_count"])],
            "latency_sample_identity",
        )
        for sample in samples:
            for field in ("stages_ms", "memory_bytes", "candidate_work"):
                require(field in sample, "missing_sample_" + field)
            require(
                isinstance(sample["stages_ms"], dict)
                and all(
                    type(v) in (int, float) and math.isfinite(v) and v >= 0
                    for v in sample["stages_ms"].values()
                ),
                "invalid_stage_time",
            )
            for field in ("memory_bytes", "candidate_work"):
                require(
                    sample[field] is None
                    or (type(sample[field]) is int and sample[field] >= 0),
                    "invalid_sample_" + field,
                )
        reports[name] = latency_report(
            samples, elapsed_ms=measurement.get("elapsed_ms")
        )
        reports[name]["workload_id"] = measurement["workload_id"]
        reports[name]["calibration"] = "report_only"
    return reports


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("directory", type=Path)
    scoring = commands.add_parser("grade")
    scoring.add_argument("manifest", type=Path)
    scoring.add_argument("raw", type=Path)
    scoring.add_argument("directory", type=Path)
    scoring.add_argument("--revision", default="1")
    args = parser.parse_args(argv)
    try:
        if args.command == "verify":
            records = [read(p) for p in sorted(args.directory.glob("sha256-*.json"))]
            require(bool(records), "empty_result_index")
            verify_bundle(records)
            print(f"Verified {len(records)} immutable records")
            return 0
        result = grade(read(args.manifest), read(args.raw), revision=args.revision)
        print(write(args.directory, result))
        return result["exit_code"]
    except (ValueError, TypeError, KeyError, OSError) as exc:
        print(f"Evaluation rejected: {exc}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
