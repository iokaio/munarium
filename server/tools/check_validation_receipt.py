# SPDX-License-Identifier: Apache-2.0
"""Validate a local receipt against the run/profile/source the caller expected."""

import argparse
import json
from pathlib import Path


def validate(receipt, *, run_id, profile, source_sha256, required_steps):
    if not isinstance(receipt, dict) or receipt.get("schema_version") != 1:
        raise ValueError("unsupported_schema")
    for key, expected in (("run_id", run_id), ("profile", profile)):
        if receipt.get(key) != expected:
            raise ValueError(f"unexpected_{key}")
    if receipt.get("completed") is not True or not receipt.get("ended_at"):
        raise ValueError("incomplete_receipt")
    for key in ("source_before", "source_after"):
        if not isinstance(receipt.get(key), dict) or receipt[key].get("sha256") != source_sha256:
            raise ValueError("source_mismatch")
    if receipt.get("source_changed") is not False:
        raise ValueError("source_changed")
    required = receipt.get("required_steps")
    steps = receipt.get("steps")
    if not isinstance(required, list) or not required or any(not isinstance(x, str) or not x for x in required):
        raise ValueError("empty_or_invalid_profile")
    if len(set(required)) != len(required) or not isinstance(steps, list):
        raise ValueError("duplicate_or_missing_steps")
    if required != required_steps:
        raise ValueError("required_step_identity_mismatch")
    ids = [s.get("id") for s in steps if isinstance(s, dict)]
    if ids != required:
        raise ValueError("step_identity_mismatch")
    for step in steps:
        if step.get("outcome") not in ("passed", "failed", "not_run") or not step.get("reason"):
            raise ValueError("invalid_outcome")
        commands = step.get("commands")
        if not isinstance(commands, list):
            raise ValueError("invalid_commands")
        if step["outcome"] == "passed" and (
            not step.get("ended_at")
            or step["reason"] != "completed"
            or any(c.get("exit_code") not in c.get("accepted_exit_codes", [0]) for c in commands)
        ):
            raise ValueError("invalid_pass")
    code = 1 if any(s["outcome"] == "failed" for s in steps) else 3 if any(s["outcome"] == "not_run" for s in steps) else 0
    if receipt.get("reason") in ("cleanup_failed", "runner_error", "source_identity_unavailable"):
        code = 1
    if code == 0 and any(r.get("cleanup") != "completed" for r in receipt.get("resources", [])):
        raise ValueError("cleanup_incomplete")
    if type(receipt.get("exit_code")) is not int or receipt["exit_code"] != code:
        raise ValueError("exit_code_mismatch")
    return code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--source-sha256", required=True)
    parser.add_argument("--required-step", action="append", required=True)
    args = parser.parse_args()
    try:
        receipt = json.loads(args.receipt.read_text(encoding="utf-8-sig"))
        return validate(receipt, run_id=args.run_id, profile=args.profile, source_sha256=args.source_sha256, required_steps=args.required_step)
    except (OSError, ValueError, TypeError, KeyError) as exc:
        print(f"Receipt rejected: {exc}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
