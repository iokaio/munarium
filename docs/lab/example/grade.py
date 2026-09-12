#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Grade a Munarium Server against an answer key. Standard library only.

    py grade.py --base-url U --mgmt-token M --key answer-key.json [--complete]
                [--model-override provider[/model]] [--out results.json]
                [--ledger --rw-token T]

For every case in the key the grader mints a capability token for the
case's caller (level and compartments), opens a fresh session on the key's
runbook reference, sends one turn, and applies deterministic checks:

  evidence grades (never need a model)
    cite_any        at least one listed path appears in hits[].source_path
    cite_all        every listed path appears in hits[].source_path
    must_not_cite   no hit path starts with a listed prefix; a violation is
                    a hard fail (access leakage), whatever else passed
    conflict        cite_all / cite_any as above for the two sides' sources

  answer grades (need --complete and a configured provider)
    contains_all    every listed substring appears in completion.text
    conflict.sides  every side has at least one of its terms in the text
    insufficient    the text declares insufficiency; a confident answer fails

An answer grade that could not run is reported as n/a, never as a pass.

Exit codes: 0 every grade that ran passed; 1 any grade failed or any hard
fail; 2 answer grades were requested and at least one turn returned no
completion.
"""
from __future__ import annotations

import argparse
import json
import pathlib
import sys
import urllib.error
import urllib.parse
import urllib.request

INSUFFICIENT_MARKERS = (
    "insufficient evidence", "does not establish", "cannot establish",
    "not enough evidence", "no evidence", "cannot answer", "not answerable",
    "do not have enough", "does not contain", "not found in the evidence",
)


def call(base: str, token: str, method: str, path: str, body=None, uid: str | None = None):
    headers = {"Authorization": "Bearer " + token}
    if uid:
        headers["X-Munarium-Uid"] = uid
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base.rstrip("/") + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=600) as resp:
            raw = resp.read()
    except urllib.error.HTTPError as err:
        return err.code, {"error": err.read().decode("utf-8", "replace")}
    return 200, (json.loads(raw) if raw else {})


def mint(base: str, mgmt_token: str, case: dict) -> str:
    body = {
        "uid": f"grader-{case['id']}",
        "access_level": int(case["caller"].get("level", 0)),
        "scopes": ["query"],
        "compartments": list(case["caller"].get("compartments", [])),
        "ttl_secs": 900,
    }
    status, answer = call(base, mgmt_token, "POST", "/v1/access-tokens", body, uid="lab-grader")
    if status != 200:
        sys.exit(f"could not mint a capability token for {case['id']}: {answer}")
    return answer["token"]


def run_turn(base: str, token: str, runbook: str, case: dict, complete: bool, override: dict | None):
    status, session = call(base, token, "POST", f"/v1/runbooks/{runbook}/sessions", {})
    if status != 200:
        return None, {"stage": "session", "status": status, "detail": session}
    body = {"query": case["ask"], "complete": complete}
    if override:
        body["model_override"] = override
    status, turn = call(base, token, "POST", f"/v1/sessions/{session['session_id']}/turns", body)
    if status != 200:
        return None, {"stage": "turn", "status": status, "detail": turn}
    turn["_permitted_collections"] = session.get("permitted_collections", [])
    return turn, None


def grade_case(case: dict, turn: dict, complete: bool) -> dict:
    expect = case["expect"]
    paths = [h.get("source_path", "") for h in turn.get("hits", [])]
    text = (turn.get("completion") or {}).get("text") or ""
    evidence: list[tuple[str, bool, str]] = []
    answer: list[tuple[str, bool | None, str]] = []
    hard_fail = False

    def has_any(wanted): return any(p in paths for p in wanted)
    def has_all(wanted): return all(p in paths for p in wanted)

    if "cite_any" in expect:
        evidence.append(("cite_any", has_any(expect["cite_any"]), ", ".join(expect["cite_any"])))
    if "cite_all" in expect:
        evidence.append(("cite_all", has_all(expect["cite_all"]), ", ".join(expect["cite_all"])))
    if "must_not_cite" in expect:
        leaked = [p for p in paths if any(p.startswith(pre) for pre in expect["must_not_cite"])]
        ok = not leaked
        hard_fail = hard_fail or not ok
        evidence.append(("must_not_cite", ok, ", ".join(leaked) if leaked else "no restricted path in hits"))
    if "conflict" in expect:
        conflict = expect["conflict"]
        if "cite_all" in conflict:
            evidence.append(("conflict.cite_all", has_all(conflict["cite_all"]), ", ".join(conflict["cite_all"])))
        if "cite_any" in conflict:
            evidence.append(("conflict.cite_any", has_any(conflict["cite_any"]), ", ".join(conflict["cite_any"])))

    lowered = text.lower()
    ran = complete and bool(text)
    if "contains_all" in expect:
        missing = [t for t in expect["contains_all"] if t.lower() not in lowered]
        answer.append(("contains_all", (not missing) if ran else None, ", ".join(missing) if missing else "all terms present"))
    if "conflict" in expect and "sides" in expect["conflict"]:
        sides = expect["conflict"]["sides"]
        absent = [side for side in sides if not any(t.lower() in lowered for t in side)]
        answer.append(("conflict.sides", (not absent) if ran else None, f"{len(sides) - len(absent)} of {len(sides)} sides present"))
    if expect.get("insufficient"):
        declared = any(m in lowered for m in INSUFFICIENT_MARKERS)
        answer.append(("insufficient", declared if ran else None, "declared" if declared else "no insufficiency marker in the text"))

    return {
        "id": case["id"],
        "ask": case["ask"],
        "family": case.get("family", ""),
        "caller": case["caller"],
        "evidence": evidence,
        "answer": answer,
        "hard_fail": hard_fail,
        "hits": paths,
        "permitted_collections": turn.get("_permitted_collections", []),
        "collections_searched": turn.get("collections_searched", []),
        "skipped": turn.get("skipped", []),
        "completion": turn.get("completion"),
        # Keep the wire response as well as the compact scorecard fields:
        # hit text, scores and index envelopes are needed to diagnose a run.
        "turn": {k: v for k, v in turn.items() if k != "_permitted_collections"},
    }


def verdict(checks, *, allow_none: bool) -> str:
    if not checks:
        return "-"
    values = [c[1] for c in checks]
    if any(v is None for v in values):
        return "n/a" if allow_none else "FAIL"
    return "PASS" if all(values) else "FAIL"


def grade_ledger(base: str, rw_token: str, key: dict) -> tuple[str, str]:
    """Run lab.py's ledger sequence and compare it with the key's expectations."""
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import lab  # noqa: E402

    server = lab.Server(base, rw_token, "lab-grader")
    spec = key["ledger"]
    vid = server.call("POST", "/v1/versions", {}, idempotent=True)["version_id"]
    first = server.call("POST", f"/v1/versions/{vid}/claims",
                        {"claim_type": "fact", "subject": spec["subject"], "key": spec["key"], "value": spec["first_value"]},
                        idempotent=True)
    second = server.call("POST", f"/v1/versions/{vid}/claims",
                         {"claim_type": "fact", "subject": spec["subject"], "key": spec["key"], "value": spec["conflicting_value"]},
                         idempotent=True)
    disputed = server.call("GET", f"/v1/versions/{vid}/facts", query={"statuses": "disputed"})
    rows = disputed["facts"]
    correction = server.call("POST", f"/v1/versions/{vid}/claims",
                {"claim_type": "correction", "subject": spec["subject"], "key": spec["key"],
                 "value": spec["conflicting_value"], "supersedes_id": first["claim"]["id"]},
                idempotent=True)
    head = server.call("GET", f"/v1/versions/{vid}/facts")
    head_rows = head["facts"]
    canon = [r["normalized_text"] for r in head_rows if r.get("status") == "accepted"]
    expected_canon = f"{spec['subject']}.{spec['key']}={spec['expect_canon_after_review']}"
    first_seq = first["claim"]["seq"]
    pinned = server.call("GET", f"/v1/versions/{vid}/facts", query={"as_of_seq": first_seq})
    old_canon = [r["normalized_text"] for r in pinned["facts"] if r.get("status") == "accepted"]
    expected_old = f"{spec['subject']}.{spec['key']}={spec['first_value']}"
    findings = server.call("GET", f"/v1/versions/{vid}/findings")["findings"]

    def is_conflict(finding):
        return finding.get("rule_id") == "gate.ledger-conflict" and finding.get("severity") == "block"

    checks = {
        "first claim accepted": first["claim"]["status"] == "accepted",
        "second claim disputed": second["claim"]["status"] == "disputed",
        "conflict finding returned": any(is_conflict(f) for f in second["findings"]),
        "disputed slice": (len(rows) == spec["expect_disputed_before_review"]
                           and any(r["id"] == second["claim"]["id"] and r["status"] == "disputed" for r in rows)),
        "correction accepted": correction["claim"]["status"] == "accepted",
        "canon after review": canon == [expected_canon],
        "point-in-time canon": (pinned["as_of_seq"] == first_seq and old_canon == [expected_old]),
        "conflict finding persisted": any(
            f["seq"] == second["claim"]["seq"] and is_conflict(f["finding"]) for f in findings
        ),
    }
    failed = [name for name, ok in checks.items() if not ok]
    detail = (f"second claim {second['claim']['status']}, {len(rows)} disputed before review, "
              f"canon after review {sorted(canon)}, as_of_seq={first_seq} {old_canon}, "
              f"persisted conflict {'ok' if checks['conflict finding persisted'] else 'FAIL'}")
    if failed:
        detail += "; failed: " + ", ".join(failed)
    return ("FAIL" if failed else "PASS"), detail


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base-url", default="http://127.0.0.1:8080")
    parser.add_argument("--mgmt-token", required=True, help="static mgmt token; mints one capability token per case")
    parser.add_argument("--rw-token", help="static rw token; needed only with --ledger")
    parser.add_argument("--key", default="answer-key.json")
    parser.add_argument("--complete", action="store_true", help="request a completion and apply the answer grades")
    parser.add_argument("--model-override", help="provider or provider/model, honored only if the runbook allows it")
    parser.add_argument("--ledger", action="store_true", help="also run and grade the memory-governance sequence")
    parser.add_argument("--out", default="results.json")
    args = parser.parse_args(argv)
    if args.ledger and not args.rw_token:
        parser.error("--ledger needs --rw-token")

    key = json.loads(pathlib.Path(args.key).read_text(encoding="utf-8"))
    runbook = key["runbook"]
    override = None
    if args.model_override:
        provider, _, model = args.model_override.partition("/")
        override = {"provider": provider}
        if model:
            override["model"] = model

    results = []
    exit_code = 0
    missing_completion = False
    print(f"runbook {runbook}   completion {'on' if args.complete else 'off'}   cases {len(key['cases'])}\n")
    print(f"{'case':5} {'caller':22} {'evidence':9} {'answer':7} {'hard':5} notes")
    for case in key["cases"]:
        token = mint(args.base_url, args.mgmt_token, case)
        turn, error = run_turn(args.base_url, token, runbook, case, args.complete, override)
        caller = f"L{case['caller'].get('level', 0)} {','.join(case['caller'].get('compartments', [])) or '-'}"
        if error:
            results.append({"id": case["id"], "error": error})
            exit_code = 1
            print(f"{case['id']:5} {caller:22} {'FAIL':9} {'FAIL':7} {'':5} {error['stage']} {error['status']}: {str(error['detail'])[:80]}")
            continue
        graded = grade_case(case, turn, args.complete)
        results.append(graded)
        if args.complete and not (turn.get("completion") or {}).get("text"):
            missing_completion = True
        ev = verdict(graded["evidence"], allow_none=False)
        an = verdict(graded["answer"], allow_none=True)
        hard = "FAIL" if graded["hard_fail"] else ""
        if ev == "FAIL" or an == "FAIL" or graded["hard_fail"]:
            exit_code = 1
        notes = "; ".join(f"{name}={'ok' if ok else ('n/a' if ok is None else 'FAIL')}" for name, ok, _ in graded["evidence"] + graded["answer"])
        print(f"{case['id']:5} {caller:22} {ev:9} {an:7} {hard:5} {notes}")

    if args.ledger:
        status, detail = grade_ledger(args.base_url, args.rw_token, key)
        results.append({"id": "ledger", "verdict": status, "detail": detail})
        if status != "PASS":
            exit_code = 1
        print(f"\nledger {status}: {detail}")

    pathlib.Path(args.out).write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    if missing_completion and exit_code == 0:
        exit_code = 2
    print(f"\nfull turn responses written to {args.out}; exit {exit_code}")
    return exit_code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
