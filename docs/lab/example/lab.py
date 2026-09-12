#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Two helpers for the worked example, standard library only.

    py lab.py ingest      --base-url U --rw-token T [--corpus corpus] [--out ingest-manifest.json]
    py lab.py extraction  --base-url U --rw-token T [--manifest ingest-manifest.json]
    py lab.py ledger      --base-url U --rw-token T [--key answer-key.json]

`ingest` uploads every file under the corpus directory through
POST /v1/ingest, keeping the path relative to the corpus directory as the
document's filename (the filename is the identity and the binding key).
It is the fallback when `mmctl bulk upload` cannot read the mounted
directory, and it records what landed in a manifest.

`extraction` reads each uploaded source back and reports its extraction
status. Run it after the index build; `empty` means a document contributed
no chunks.

`ledger` runs the memory-governance sequence from docs/lab/04-memory-governance.md
and prints every response: a fact is accepted, a conflicting fact is
recorded as disputed with a gate finding, the disputed slice is listed, a
correction supersedes, and the point-in-time read returns the old value.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import pathlib
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid

MEDIA_TYPES = {".md": "text/markdown", ".txt": "text/plain", ".json": "application/json"}


class Server:
    def __init__(self, base_url: str, token: str, uid: str) -> None:
        self.base = base_url.rstrip("/")
        self.token = token
        self.uid = uid

    def call(self, method: str, path: str, body=None, *, idempotent: bool = False, query=None):
        url = self.base + path
        if query:
            url += "?" + urllib.parse.urlencode(query)
        data = None
        headers = {"Authorization": "Bearer " + self.token, "X-Munarium-Uid": self.uid}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if idempotent:
            headers["Idempotency-Key"] = str(uuid.uuid4())
        req = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=600) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as err:
            detail = err.read().decode("utf-8", "replace")
            sys.exit(f"{method} {path} -> {err.code}: {detail}")
        return json.loads(raw) if raw else {}


def cmd_ingest(args: argparse.Namespace) -> int:
    server = Server(args.base_url, args.rw_token, args.uid)
    root = pathlib.Path(args.corpus)
    files = sorted(p for p in root.rglob("*") if p.is_file())
    if not files:
        sys.exit(f"no files under {root}")
    manifest = []
    for path in files:
        rel = path.relative_to(root).as_posix()
        raw = path.read_bytes()
        media = MEDIA_TYPES.get(path.suffix.lower(), "application/octet-stream")
        body = {
            "filename": rel,
            "media_type": media,
            "content_base64": base64.b64encode(raw).decode("ascii"),
        }
        answer = server.call("POST", "/v1/ingest", body)
        row = {
            "filename": rel,
            "sha256": hashlib.sha256(raw).hexdigest(),
            "bytes": len(raw),
            "source_id": answer.get("source_id"),
            "existed": answer.get("existed"),
            "bound_to": answer.get("bound_to", []),
        }
        manifest.append(row)
        flag = "existed" if row["existed"] else "new"
        print(f"{rel:55} {flag:8} bound_to={','.join(row['bound_to']) or '-'}")
        if not row["bound_to"]:
            print(f"  warning: {rel} bound to no collection; check the runbook prefixes")
    pathlib.Path(args.out).write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"\n{len(manifest)} documents; manifest written to {args.out}")
    return 0


def cmd_extraction(args: argparse.Namespace) -> int:
    server = Server(args.base_url, args.rw_token, args.uid)
    manifest = json.loads(pathlib.Path(args.manifest).read_text(encoding="utf-8"))
    problems = 0
    for row in manifest:
        source = server.call("GET", f"/v1/sources/{row['source_id']}")
        status = source.get("extraction_status")
        method = source.get("extraction_method")
        media = source.get("media_type", "")
        if status is None:
            # Server 1.1.1 reports no extraction fields for a source whose
            # bytes are already text; the record shows only what was stored.
            shown = "text" if media.startswith("text/") else "unreported"
            if not media.startswith("text/"):
                problems += 1
        else:
            shown = f"{status} ({method})"
            if status != "ok":
                problems += 1
        print(f"{row['filename']:55} {media:16} {shown}")
    print(f"\n{problems} document(s) whose extraction is not known to be ok")
    return 1 if problems else 0


def cmd_ledger(args: argparse.Namespace) -> int:
    server = Server(args.base_url, args.rw_token, args.uid)
    key = json.loads(pathlib.Path(args.key).read_text(encoding="utf-8"))["ledger"]
    subject, fact_key = key["subject"], key["key"]

    print("1. Create a lineage")
    version = server.call("POST", "/v1/versions", {}, idempotent=True)
    vid = version["version_id"]
    print(f"   version_id {vid}")

    print(f"2. Propose the ticket's reading: {subject}.{fact_key}={key['first_value']}")
    first = server.call(
        "POST", f"/v1/versions/{vid}/claims",
        {"claim_type": "fact", "subject": subject, "key": fact_key, "value": key["first_value"]},
        idempotent=True,
    )
    print(f"   seq {first['claim']['seq']} status {first['claim']['status']} findings {len(first['findings'])}")
    first_id = first["claim"]["id"]

    print(f"3. Propose the bulletin's reading: {subject}.{fact_key}={key['conflicting_value']}")
    second = server.call(
        "POST", f"/v1/versions/{vid}/claims",
        {"claim_type": "fact", "subject": subject, "key": fact_key, "value": key["conflicting_value"]},
        idempotent=True,
    )
    print(f"   seq {second['claim']['seq']} status {second['claim']['status']}")
    for finding in second["findings"]:
        print(f"   finding {finding['rule_id']} [{finding['severity']}]: {finding['message']}")

    print("4. The review slice: every disputed fact")
    disputed = server.call("GET", f"/v1/versions/{vid}/facts", query={"statuses": "disputed"})
    rows = disputed.get("facts", disputed if isinstance(disputed, list) else [])
    for row in rows:
        print(f"   seq {row['seq']} {row['normalized_text']} ({row['status']})")

    print(f"5. A reviewer reads both documents and corrects canon to {key['conflicting_value']}")
    correction = server.call(
        "POST", f"/v1/versions/{vid}/claims",
        {"claim_type": "correction", "subject": subject, "key": fact_key,
         "value": key["conflicting_value"], "supersedes_id": first_id},
        idempotent=True,
    )
    print(f"   seq {correction['claim']['seq']} status {correction['claim']['status']} findings {len(correction['findings'])}")

    print("6. Canon now, and canon as it stood at seq 1")
    head = server.call("GET", f"/v1/versions/{vid}/facts")
    pinned = server.call("GET", f"/v1/versions/{vid}/facts", query={"as_of_seq": 1})
    head_rows = head.get("facts", head if isinstance(head, list) else [])
    pinned_rows = pinned.get("facts", pinned if isinstance(pinned, list) else [])
    for row in head_rows:
        print(f"   head:      seq {row['seq']} {row['normalized_text']} ({row['status']})")
    for row in pinned_rows:
        print(f"   as_of_seq=1: seq {row['seq']} {row['normalized_text']} ({row['status']})")

    print("7. The persisted findings for this lineage")
    findings = server.call("GET", f"/v1/versions/{vid}/findings")
    items = findings.get("findings", findings if isinstance(findings, list) else [])
    for item in items:
        finding = item.get("finding", item)
        print(f"   seq {item.get('seq', '?')} {finding.get('rule_id')} [{finding.get('severity')}]: {finding.get('message')}")

    result = {
        "version_id": vid,
        "disputed_before_review": len(rows),
        "canon_after_review": [r["normalized_text"] for r in head_rows],
        "as_of_seq_1": [r["normalized_text"] for r in pinned_rows],
    }
    pathlib.Path(args.out).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(f"\nrecorded to {args.out}")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    def common(p: argparse.ArgumentParser) -> None:
        p.add_argument("--base-url", default="http://127.0.0.1:8080")
        p.add_argument("--rw-token", required=True, help="the static rw token")
        p.add_argument("--uid", default="lab-operator")

    p_ingest = sub.add_parser("ingest")
    common(p_ingest)
    p_ingest.add_argument("--corpus", default="corpus")
    p_ingest.add_argument("--out", default="ingest-manifest.json")
    p_ingest.set_defaults(func=cmd_ingest)

    p_ext = sub.add_parser("extraction")
    common(p_ext)
    p_ext.add_argument("--manifest", default="ingest-manifest.json")
    p_ext.set_defaults(func=cmd_extraction)

    p_ledger = sub.add_parser("ledger")
    common(p_ledger)
    p_ledger.add_argument("--key", default="answer-key.json")
    p_ledger.add_argument("--out", default="ledger-results.json")
    p_ledger.set_defaults(func=cmd_ledger)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
