# SPDX-License-Identifier: Apache-2.0
"""Validate live analyses against retained raw outcomes and frozen workloads."""

import math
from pathlib import Path

import frozen_eval as ev
import p13_live as live


def validate_measurements(manifest, measured):
    definitions = manifest["latency_workloads"]
    ev.require(set(measured) == set(definitions), "missing_latency_path")
    for name, definition in definitions.items():
        runs = measured[name]
        ev.require(
            len(runs) == manifest["runner_settings"]["repetitions"],
            "missing_latency_repetition",
        )
        for number, run in enumerate(runs):
            ev.require(run["repetition"] == number, "repetition_identity_mismatch")
            ev.require(
                run["workload_id"] == ev.digest(definition),
                "workload_identity_mismatch",
            )
            samples = run["samples"]
            ev.require(
                ev.ids(samples) == [str(i) for i in range(definition["sample_count"])],
                "latency_sample_identity",
            )
            elapsed = run["elapsed_ms"]
            ev.require(
                elapsed is None
                or (
                    type(elapsed) in (float, int)
                    and math.isfinite(elapsed)
                    and elapsed > 0
                ),
                "invalid_elapsed_time",
            )
            for sample in samples:
                ev.require(
                    sample["outcome"]
                    in ("passed", "failed", "harness_error", "unavailable"),
                    "invalid_latency_outcome",
                )
                duration = sample["duration_ms"]
                ev.require(
                    duration is None
                    or (
                        type(duration) in (int, float)
                        and math.isfinite(duration)
                        and duration >= 0
                    ),
                    "invalid_duration",
                )
                ev.require(
                    sample["outcome"] != "passed" or duration is not None,
                    "missing_duration",
                )
                for field in ("stages_ms", "memory_bytes", "candidate_work", "reason"):
                    ev.require(field in sample, "missing_sample_" + field)
                if sample["outcome"] == "passed":
                    evidence = sample["evidence"]
                    index = int(sample["id"])
                    if name == "retrieval":
                        ev.require(
                            bool(evidence.get("envelope", {}).get("index_version"))
                            and any(
                                hit["source_path"] == f"latency/{index % 200}"
                                for hit in evidence["hits"]
                            ),
                            "false_retrieval_success",
                        )
                    elif name == "ledger_append":
                        ev.require(
                            evidence["claim"]["status"] == "accepted"
                            and evidence["claim"]["value"] == str(index),
                            "false_append_success",
                        )
                    else:
                        ev.require(
                            evidence["polls"] > 0
                            and evidence["duration_ms"] == duration,
                            "false_readiness_success",
                        )


def check_reports(root):
    directory = Path(root) / "server/conformance/results"
    findings = []
    records = {
        record["id"]: record
        for record in (ev.read(p) for p in directory.glob("sha256-*.json"))
    }
    for path in directory.glob("analysis-*.json"):
        try:
            report = ev.read(path)
            ev.envelope(report, "live_analysis")
            manifest, raw, grading = [
                records[report[field]]
                for field in ("manifest_id", "raw_id", "grading_id")
            ]
            ev.require(
                raw["manifest_id"] == manifest["id"] and grading["raw_id"] == raw["id"],
                "analysis_reference_mismatch",
            )
            ev.require(
                report["source"] == manifest["source"] == raw["source"],
                "analysis_source_mismatch",
            )
            validate_measurements(manifest, raw["latency_repetitions"])
            if manifest["source"].get("scripts/p13_live.py") == ev.source_identity(
                Path(live.__file__)
            ):
                ev.require(
                    report["governance"] == live.paired_summary(manifest, raw, grading),
                    "governance_analysis_mismatch",
                )
                ev.require(
                    report["calibration"]
                    == live.calibration(manifest, raw["latency_repetitions"]),
                    "latency_analysis_mismatch",
                )
        except (ValueError, TypeError, KeyError, OSError) as exc:
            findings.append(f"{path.name}: {exc}")
    return findings


if __name__ == "__main__":
    errors = check_reports(ev.ROOT)
    for error in errors:
        print(error)
    print(f"Live analysis validation: {len(errors)} finding(s)")
    raise SystemExit(bool(errors))
