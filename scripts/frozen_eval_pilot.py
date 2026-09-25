# SPDX-License-Identifier: Apache-2.0
"""Zero-cost scripted-provider plumbing pilot. No Server or model quality claim.

Freeze first: py scripts/frozen_eval_pilot.py freeze DIRECTORY
Then run:    py scripts/frozen_eval_pilot.py run MANIFEST DIRECTORY --run-id NAME
"""

import argparse
import copy
import platform
import sys
import time
from pathlib import Path

import frozen_eval as ev

FIXTURE = ev.ROOT / "server/conformance/evaluation/pilot.json"


def freeze():
    fixture = ev.read(FIXTURE)
    paths = [Path(__file__), Path(ev.__file__), ev.GRADER_PATH, FIXTURE]
    source = {p.relative_to(ev.ROOT).as_posix(): ev.source_identity(p) for p in paths}
    cases = fixture["cases"]
    return ev.seal(
        "manifest",
        campaign="scripted-evidence-pilot",
        revision="1",
        purpose="pilot",
        source=source,
        build={
            "runtime": "python-stdlib",
            "implementation": platform.python_implementation(),
            "version": platform.python_version(),
        },
        corpus_hash=ev.digest(fixture["histories"]),
        query_hash=ev.digest([{"id": c["id"], "ask": c["ask"]} for c in cases]),
        grader=ev.source_identity(ev.GRADER_PATH),
        cases=cases,
        runbook={"identity": "scripted-provider-v1", "server_execution": False},
        retrieval={"mode": "scripted-authorized-evidence", "context_chars": 4096},
        model={"identity": "scripted-v1", "temperature": 0, "network": False},
        thresholds={"all_checks_pass": True},
        failure_treatment="retain_all",
        resource_limits={
            "paid_calls": 0,
            "max_rows": len(cases),
            "context_chars": 4096,
        },
    )


def produce(manifest, directory, run_id, provider=None):
    ev.validate_manifest(manifest)
    expected = freeze()
    ev.require(manifest == expected, "pilot_source_or_fixture_changed")
    fixture = ev.read(FIXTURE)
    responses = fixture["scripted_responses"]
    provider = provider or (lambda case: copy.deepcopy(responses[case["id"]]))
    rows = []
    interrupted = False
    start = time.perf_counter()
    for case in manifest["cases"]:
        row = {
            "id": case["id"],
            "status": "unexecuted",
            "reason": "previous_interruption",
            "usage": None,
            "turn": None,
        }
        if not interrupted:
            before = time.perf_counter()
            try:
                row["turn"] = provider(case)
                row.update(status="completed", reason="scripted_response", usage=None)
            except KeyboardInterrupt:
                row.update(status="interrupted", reason="producer_interrupted")
                interrupted = True
            except (OSError, RuntimeError, ValueError, KeyError, TypeError) as exc:
                # Do not copy exception messages that could contain provider secrets.
                row.update(
                    status="failed", reason="provider_error:" + type(exc).__name__
                )
            row["duration_ms"] = (time.perf_counter() - before) * 1000
        rows.append(row)
    raw = ev.seal(
        "raw",
        manifest_id=manifest["id"],
        run_id=run_id,
        purpose=manifest["purpose"],
        source=manifest["source"],
        rows=rows,
        elapsed_ms=(time.perf_counter() - start) * 1000,
        timing_scope="scripted_provider_only; not product latency",
    )
    # Persist and read back before grading; this is the actual producer boundary.
    path = ev.write(directory, raw)
    persisted = ev.read(path)
    result = ev.grade(manifest, persisted)
    ev.write(directory, result)
    return raw, result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    freezing = commands.add_parser("freeze")
    freezing.add_argument("directory", type=Path)
    running = commands.add_parser("run")
    running.add_argument("manifest", type=Path)
    running.add_argument("directory", type=Path)
    running.add_argument("--run-id", required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "freeze":
            print(ev.write(args.directory, freeze()))
            return 0
        _, result = produce(ev.read(args.manifest), args.directory, args.run_id)
        print(f"{result['id']} exit={result['exit_code']}; scripted pilot only")
        return result["exit_code"]
    except (ValueError, OSError) as exc:
        print(f"Pilot rejected: {exc}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
