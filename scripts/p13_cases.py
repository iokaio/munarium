# SPDX-License-Identifier: Apache-2.0
"""Fictional event histories, independent answer keys and declared baseline policies."""

import json

ARMS = (
    "no_memory",
    "versioned_notes",
    "conventional_retrieval",
    "munarium",
    "governance_ablation",
)
FAMILIES = ("stable", "correction", "past", "conflict", "removed", "revoked")
REMOVED = "__removed__"


def fixture(split, per_family):
    histories, cases = [], []
    for family in FAMILIES:
        for i in range(per_family):
            hid = f"{split}-{family}-{i:03}"
            old, new = f"amber-{i:03}", f"violet-{i:03}"
            events = [{"id": hid + "/0", "at": 1, "kind": "fact", "value": old}]
            if family in ("correction", "past", "removed"):
                events.append(
                    {
                        "id": hid + "/1",
                        "at": 2,
                        "kind": "correction",
                        "value": REMOVED if family == "removed" else new,
                        "supersedes": events[0]["id"],
                    }
                )
            elif family == "conflict":
                events.append({"id": hid + "/1", "at": 2, "kind": "fact", "value": new})
            history = {
                "id": hid,
                "family": family,
                "events": events,
                "authorized_now": family != "revoked",
                "as_of": 1 if family in ("past", "revoked") else 2,
            }
            histories.append(history)
            # Expected observations are authored by scenario, never passed to a responder.
            expected = (
                [events[0]]
                if family in ("stable", "past")
                else events[-1:]
                if family == "correction"
                else events
                if family == "conflict"
                else []
            )
            expect = {"must_not_cite": ["forbidden/", hid + "/forbidden/"]}
            if expected:
                expect["must_not_cite"].extend(
                    e["id"] for e in events if e not in expected
                )
                expect["cite_all"] = [e["id"] for e in expected]
                expect["contains_all"] = [e["value"] for e in expected]
                if family == "conflict":
                    expect["contains_all"].append("conflict")
            else:
                expect["insufficient"] = True
                expect["must_not_cite"].append(hid + "/")
            for arm in ARMS:
                cases.append(
                    {
                        "id": hid + ":" + arm,
                        "history_id": hid,
                        "task_id": family + ":" + arm,
                        "split": split,
                        "ask": f"What is {hid}'s value at time {history['as_of']}?",
                        "caller": {"level": 0, "compartments": []},
                        "expect": expect,
                        "arm": arm,
                        "family": family,
                    }
                )
    return {"histories": histories, "cases": cases}


def resolve_notes(events, as_of):
    """A competent conventional policy: preserve timestamps, explicit supersession and conflicts."""
    eligible = [e for e in events if e["at"] <= as_of]
    superseded = {e["supersedes"] for e in eligible if "supersedes" in e}
    return [e for e in eligible if e["id"] not in superseded and e["value"] != REMOVED]


def respond(events, context_chars=4096):
    # Shared deterministic responder receives selected evidence only, no answer key.
    context = json.dumps(events, sort_keys=True)
    if len(context) > context_chars:
        raise ValueError("context_budget_exceeded")
    values = sorted({e["value"] for e in events})
    answer = (
        "insufficient evidence"
        if not values
        else ("conflict: " if len(values) > 1 else "") + " | ".join(values)
    )
    return {
        "hits": [{"source_path": e["id"], "text": e["value"]} for e in events],
        "completion": {"text": answer},
        "context_chars": len(context),
    }
