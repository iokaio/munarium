# SPDX-License-Identifier: Apache-2.0
"""Replay and comparison logic for the local v1.4 compatibility harness.

Standard library only. The functions here do not start containers; they run a
case file against one Server endpoint through a transport and compare two
recorded transcripts. ``compat_local.py`` orchestrates the containers and
``grpc_driver.py`` supplies the gRPC transports.

Comparison rules (COMPAT-T1):

* Status codes and media types must match exactly.
* JSON bodies are compared member by member. A member present on one side only
  is ``added`` (candidate only) or ``removed`` (baseline only). Added members
  are reported for review, because the wire policy tolerates them; removed
  members fail. A change of JSON kind (for example ``null`` to ``[]``, or an
  absent member to ``null``) fails, which preserves the absent, null and empty
  distinctions.
* Generated identifiers are compared by meaning. A UUID that appears in the
  seed manifest must be byte-identical. Any other UUID is bound to its
  counterpart the first time it appears; every later occurrence must respect
  that one-to-one binding across the whole transcript.
* Timestamps and capability tokens must keep their format; their values are
  volatile. Numbers under latency-style keys are volatile.
* The problem+json ``detail`` text is informational: a wording change is
  reported but does not fail, while ``type``, ``title`` and ``status`` must match.
* A case marked ``exact`` must also have byte-identical bodies.
"""

from __future__ import annotations

import base64
import hashlib
import json
import re
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Callable, Iterable

UUID_RE = re.compile(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
TS_RE = re.compile(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}(:?\d{2})?)?$")
# Server identifiers: a lowercase kind prefix and a hex body (memv-, claim-, col-, ses-, tok-, idx-, ...).
PREFIXED_ID_RE = re.compile(r"^[a-z][a-z0-9]*(?:-[a-z0-9]+)*-[0-9a-f]{12,64}$")
JWT_RE = re.compile(r"^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*$")
VOLATILE_NUMBER_KEYS = re.compile(
    r"(^|_)(ms|millis|elapsed|latency|duration|took|uptime|secs_ago|age_secs|exp|iat|nbf)$"
)
VAR_RE = re.compile(r"\$\{([A-Za-z0-9_.-]+)\}")


# --------------------------------------------------------------------------
# templating and capture
# --------------------------------------------------------------------------


def substitute(value: Any, variables: dict[str, Any]) -> Any:
    """Replace ``${name}`` in every string of a JSON-like value.

    A string that is exactly one placeholder takes the variable's value with
    its JSON type, so a captured number stays a number."""
    if isinstance(value, str):
        whole = VAR_RE.fullmatch(value)
        if whole:
            return _lookup(variables, whole.group(1))
        return VAR_RE.sub(lambda m: str(_lookup(variables, m.group(1))), value)
    if isinstance(value, list):
        return [substitute(v, variables) for v in value]
    if isinstance(value, dict):
        return {k: substitute(v, variables) for k, v in value.items()}
    return value


def _lookup(variables: dict[str, Any], name: str) -> Any:
    if name not in variables:
        raise KeyError(f"undefined replay variable ${{{name}}}")
    return variables[name]


def json_pointer(doc: Any, pointer: str) -> Any:
    """RFC 6901 lookup; raises KeyError when the path is absent."""
    if pointer in ("", "/"):
        return doc
    cur = doc
    for raw in pointer.lstrip("/").split("/"):
        part = raw.replace("~1", "/").replace("~0", "~")
        if isinstance(cur, list):
            cur = cur[int(part)]
        elif isinstance(cur, dict):
            if part not in cur:
                raise KeyError(pointer)
            cur = cur[part]
        else:
            raise KeyError(pointer)
    return cur


def capture(case: dict[str, Any], response: dict[str, Any], variables: dict[str, Any]) -> list[str]:
    """Apply a case's ``capture`` map. Returns the names that were missing."""
    missing = []
    body = response.get("json")
    if body is None and response.get("events"):
        body = {"events": response["events"]}
    for name, pointer in case.get("capture", {}).items():
        try:
            variables[name] = json_pointer(body, pointer)
        except (KeyError, IndexError, ValueError, TypeError):
            missing.append(name)
    for name, rule in case.get("capture_where", {}).items():
        try:
            items = json_pointer(body, rule["array"])
            hits = [i for i in items if all(i.get(k) == v for k, v in rule["match"].items())]
            variables[name] = hits[0][rule["field"]]
        except (KeyError, IndexError, ValueError, TypeError, AttributeError):
            missing.append(name)
    return missing


# --------------------------------------------------------------------------
# path templates and request rendering
# --------------------------------------------------------------------------


def render_path(template: str, params: dict[str, Any]) -> str:
    def repl(m: re.Match[str]) -> str:
        key = m.group(1)
        if key not in params:
            raise KeyError(f"missing path parameter {key}")
        return urllib.request.quote(str(params[key]), safe="@:")

    return re.sub(r"\{([A-Za-z0-9_]+)\}", repl, template)


def render_request(case: dict[str, Any], variables: dict[str, Any]) -> dict[str, Any]:
    """Resolve a case into a transport-neutral request."""
    params = substitute(case.get("params", {}), variables)
    query = substitute(case.get("query", {}), variables)
    headers = substitute(case.get("headers", {}), variables)
    body: bytes | None = None
    content_type = case.get("content_type")
    if "json" in case:
        body = json.dumps(substitute(case["json"], variables), separators=(",", ":")).encode()
        content_type = content_type or "application/json"
    elif "text" in case:
        body = str(substitute(case["text"], variables)).encode()
    elif "form" in case:
        form = substitute(case["form"], variables)
        body = urllib.parse.urlencode(form).encode()
        content_type = content_type or "application/x-www-form-urlencoded"
    elif "base64" in case:
        body = base64.b64decode(substitute(case["base64"], variables))
    return {
        "method": case.get("method", "GET"),
        "template": case["path"],
        "params": {k: str(v) for k, v in params.items()},
        "path": render_path(case["path"], params),
        "query": {k: str(v) for k, v in query.items()},
        "headers": headers,
        "body": body,
        "content_type": content_type,
        "token": substitute(case.get("token", "rw"), variables),
        "uid": substitute(case.get("uid", "compat-user"), variables),
        "stream": bool(case.get("stream")),
    }


# --------------------------------------------------------------------------
# response decoding
# --------------------------------------------------------------------------


def parse_sse(text: str) -> list[dict[str, Any]]:
    """Split an event stream into ``{"event", "data"}`` records.

    ``data`` is decoded as JSON when it parses; comment lines are dropped."""
    events: list[dict[str, Any]] = []
    name, data = None, []
    for line in text.replace("\r\n", "\n").split("\n"):
        if line == "":
            if name is not None or data:
                payload = "\n".join(data)
                try:
                    decoded: Any = json.loads(payload)
                except ValueError:
                    decoded = payload
                events.append({"event": name or "message", "data": decoded})
            name, data = None, []
            continue
        if line.startswith(":"):
            continue
        key, _, val = line.partition(":")
        val = val[1:] if val.startswith(" ") else val
        if key == "event":
            name = val
        elif key == "data":
            data.append(val)
    if name is not None or data:
        payload = "\n".join(data)
        try:
            decoded = json.loads(payload)
        except ValueError:
            decoded = payload
        events.append({"event": name or "message", "data": decoded})
    return events


def decode_body(status: int, content_type: str, raw: bytes) -> dict[str, Any]:
    media = (content_type or "").split(";")[0].strip().lower()
    out: dict[str, Any] = {
        "status": status,
        "media_type": media,
        "bytes_len": len(raw),
        "sha256": hashlib.sha256(raw).hexdigest(),
    }
    if media == "text/event-stream":
        out["events"] = parse_sse(raw.decode("utf-8", "replace"))
    elif media.endswith("json"):
        try:
            out["json"] = json.loads(raw.decode("utf-8")) if raw else None
        except ValueError:
            out["text"] = raw.decode("utf-8", "replace")
    else:
        out["text"] = raw.decode("utf-8", "replace")
    return out


# --------------------------------------------------------------------------
# REST transport and the case runner
# --------------------------------------------------------------------------


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args: Any, **kwargs: Any) -> None:
        return None


@dataclass
class RestTransport:
    base_url: str
    tokens: dict[str, str]
    timeout: float = 120.0
    name: str = "rest"

    def call(self, req: dict[str, Any]) -> dict[str, Any]:
        url = self.base_url.rstrip("/") + req["path"]
        if req["query"]:
            url += "?" + urllib.parse.urlencode(req["query"])
        headers = {"Accept": "*/*"}
        token = req["token"]
        if token and token != "none":
            headers["Authorization"] = f"Bearer {self.tokens.get(token, token)}"
        if req["uid"]:
            headers["X-Munarium-Uid"] = req["uid"]
        if req["content_type"]:
            headers["Content-Type"] = req["content_type"]
        headers.update({k: str(v) for k, v in req["headers"].items()})
        request = urllib.request.Request(url, data=req["body"], method=req["method"], headers=headers)
        opener = urllib.request.build_opener(_NoRedirect)
        try:
            with opener.open(request, timeout=self.timeout) as resp:
                raw = resp.read()
                status, ctype, hdrs = resp.status, resp.headers.get("Content-Type", ""), resp.headers
        except urllib.error.HTTPError as err:
            raw = err.read()
            status, ctype, hdrs = err.code, err.headers.get("Content-Type", ""), err.headers
        out = decode_body(status, ctype, raw)
        loc = hdrs.get("Location")
        if loc:
            out["location"] = loc
        return out


def _poll_done(resp: dict[str, Any], poll: dict[str, Any]) -> bool:
    try:
        return json_pointer(resp.get("json"), poll["pointer"]) in poll["until"]
    except (KeyError, IndexError, ValueError, TypeError):
        return False


def run_cases(
    cases: Iterable[dict[str, Any]],
    transport: Any,
    variables: dict[str, Any],
    log: Callable[[str], None] = lambda _m: None,
) -> list[dict[str, Any]]:
    """Run cases in order, capturing variables. Never raises for a server
    response; a case whose template needs a missing variable is recorded as
    ``skipped`` with the reason, so one failure cannot hide later cases."""
    transcript = []
    for case in cases:
        if transport.name in case.get("skip_transports", []):
            continue
        entry: dict[str, Any] = {"id": case["id"], "kind": case.get("kind", "success")}
        for key in ("exact", "volatile", "surface"):
            if key in case:
                entry[key] = case[key]
        try:
            req = render_request(case, variables)
        except KeyError as err:
            entry["skipped"] = str(err)
            transcript.append(entry)
            log(f"{case['id']}: skipped ({err})")
            continue
        entry["request"] = {"method": req["method"], "path": req["path"], "template": req["template"], "token": case.get("token", "rw")}
        try:
            resp = transport.call(req)
            poll = case.get("poll")
            polls = 0
            while poll and polls < int(poll.get("max", 120)) and not _poll_done(resp, poll):
                time.sleep(float(poll.get("interval", 1.0)))
                resp = transport.call(req)
                polls += 1
        except Exception as err:  # transport failure is evidence, not a crash
            entry["transport_error"] = f"{type(err).__name__}: {err}"
            transcript.append(entry)
            log(f"{case['id']}: transport error {err}")
            continue
        entry["response"] = resp
        expected = case.get("expect_status")
        if expected is not None and transport.name == "rest":
            entry["expected_status"] = expected
            entry["status_ok"] = resp["status"] in (expected if isinstance(expected, list) else [expected])
        missing = capture(case, resp, variables)
        if missing:
            entry["capture_missing"] = missing
        log(f"{case['id']}: {resp['status']}")
        transcript.append(entry)
    return transcript


# --------------------------------------------------------------------------
# comparison
# --------------------------------------------------------------------------


@dataclass
class Finding:
    case: str
    path: str
    kind: str  # removed | added | kind_changed | value_changed | status | media_type | bytes | id_binding | format | informational | length | skipped
    baseline: Any = None
    candidate: Any = None

    def failing(self) -> bool:
        return self.kind not in ("added", "informational")

    def as_dict(self) -> dict[str, Any]:
        return {
            "case": self.case,
            "path": self.path,
            "kind": self.kind,
            "baseline": _short(self.baseline),
            "candidate": _short(self.candidate),
        }


def _short(v: Any) -> Any:
    s = json.dumps(v) if not isinstance(v, str) else v
    return s if len(s) <= 160 else s[:157] + "..."


def is_id(v: str) -> bool:
    return bool(UUID_RE.match(v) or PREFIXED_ID_RE.match(v))


def json_kind(v: Any) -> str:
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, (int, float)):
        return "number"
    if isinstance(v, str):
        return "string"
    if isinstance(v, list):
        return "empty-array" if not v else "array"
    if isinstance(v, dict):
        return "empty-object" if not v else "object"
    return type(v).__name__


@dataclass
class Comparator:
    stable_ids: set[str] = field(default_factory=set)
    forward: dict[str, str] = field(default_factory=dict)
    backward: dict[str, str] = field(default_factory=dict)
    findings: list[Finding] = field(default_factory=list)
    informational_keys: tuple[str, ...] = ("detail", "details")
    volatile: tuple[str, ...] = ()

    def compare_transcripts(self, base: list[dict[str, Any]], cand: list[dict[str, Any]]) -> list[Finding]:
        cand_by_id = {c["id"]: c for c in cand}
        for b in base:
            c = cand_by_id.get(b["id"])
            if c is None:
                self.findings.append(Finding(b["id"], "", "removed", "case", None))
                continue
            self.compare_case(b, c)
        base_ids = {b["id"] for b in base}
        for c in cand:
            if c["id"] not in base_ids:
                self.findings.append(Finding(c["id"], "", "added", None, "case"))
        return self.findings

    def compare_case(self, b: dict[str, Any], c: dict[str, Any], exact: bool | None = None) -> None:
        cid = b["id"]
        for side_name, side in (("baseline", b), ("candidate", c)):
            if "skipped" in side or "transport_error" in side:
                self.findings.append(
                    Finding(cid, side_name, "skipped", b.get("skipped") or b.get("transport_error"), c.get("skipped") or c.get("transport_error"))
                )
                return
        rb, rc = b["response"], c["response"]
        if rb["status"] != rc["status"]:
            self.findings.append(Finding(cid, "status", "status", rb["status"], rc["status"]))
        if rb["media_type"] != rc["media_type"]:
            self.findings.append(Finding(cid, "media_type", "media_type", rb["media_type"], rc["media_type"]))
        if exact if exact is not None else b.get("exact"):
            if rb["sha256"] != rc["sha256"]:
                self.findings.append(Finding(cid, "body", "bytes", rb["sha256"], rc["sha256"]))
        problem = rb["media_type"] == "application/problem+json"
        self.volatile = tuple(b.get("volatile", ()))
        problem = problem or rb["media_type"] == "grpc-error"
        for key in ("json", "events", "text"):
            if key in rb or key in rc:
                if key == "text":
                    self._compare_text(cid, rb.get("text"), rc.get("text"))
                elif key == "events":
                    self.compare_value(cid, "/events", order_concurrent(rb.get(key)), order_concurrent(rc.get(key)), problem)
                else:
                    self.compare_value(cid, f"/{key}", rb.get(key), rc.get(key), problem)

    def _compare_text(self, cid: str, b: str | None, c: str | None) -> None:
        if b == c:
            return
        self.findings.append(Finding(cid, "/text", "value_changed", b, c))

    def compare_value(self, cid: str, path: str, b: Any, c: Any, problem: bool = False) -> None:
        if any(path == v or path.startswith(v + "/") for v in self.volatile):
            if json_kind(b) != json_kind(c) and not (isinstance(b, (list, dict)) and isinstance(c, (list, dict))):
                self.findings.append(Finding(cid, path, "kind_changed", json_kind(b), json_kind(c)))
            return
        if isinstance(b, dict) and isinstance(c, dict):
            for k in sorted(set(b) | set(c)):
                sub = f"{path}/{k}"
                if k not in c:
                    self.findings.append(Finding(cid, sub, "removed", b[k], None))
                elif k not in b:
                    self.findings.append(Finding(cid, sub, "added", None, c[k]))
                elif problem and k in self.informational_keys and b[k] != c[k]:
                    self.findings.append(Finding(cid, sub, "informational", b[k], c[k]))
                else:
                    self.compare_value(cid, sub, b[k], c[k], problem)
            return
        if isinstance(b, list) and isinstance(c, list):
            if len(b) != len(c):
                self.findings.append(Finding(cid, path, "length", len(b), len(c)))
            for i, (x, y) in enumerate(zip(b, c)):
                self.compare_value(cid, f"{path}/{i}", x, y, problem)
            return
        kb, kc = json_kind(b), json_kind(c)
        if kb != kc:
            self.findings.append(Finding(cid, path, "kind_changed", kb, kc))
            return
        if isinstance(b, str):
            self._compare_string(cid, path, b, c)
        elif isinstance(b, (int, float)) and not isinstance(b, bool):
            key = path.rsplit("/", 1)[-1]
            if b != c and not VOLATILE_NUMBER_KEYS.search(key):
                self.findings.append(Finding(cid, path, "value_changed", b, c))
        elif b != c:
            self.findings.append(Finding(cid, path, "value_changed", b, c))

    def _compare_string(self, cid: str, path: str, b: str, c: str) -> None:
        if is_id(b) or is_id(c):
            if not (is_id(b) and is_id(c)):
                self.findings.append(Finding(cid, path, "format", b, c))
                return
            b, c = b.lower(), c.lower()
            if b in self.stable_ids or c in self.stable_ids:
                if b != c:
                    self.findings.append(Finding(cid, path, "value_changed", b, c))
                return
            fb, bc = self.forward.get(b), self.backward.get(c)
            if fb is None and bc is None:
                self.forward[b], self.backward[c] = c, b
            elif fb != c or bc != b:
                self.findings.append(Finding(cid, path, "id_binding", f"{b} -> {fb}", f"{c} <- {bc}"))
            return
        if TS_RE.match(b) or TS_RE.match(c):
            if not (TS_RE.match(b) and TS_RE.match(c)):
                self.findings.append(Finding(cid, path, "format", b, c))
            return
        if JWT_RE.match(b) or JWT_RE.match(c):
            if not (JWT_RE.match(b) and JWT_RE.match(c)):
                self.findings.append(Finding(cid, path, "format", "<jwt>", c[:12]))
            return
        if b != c:
            self.findings.append(Finding(cid, path, "value_changed", b, c))


def order_concurrent(events: Any) -> Any:
    """Sort each run of consecutive per-collection ``retrieval`` progress events
    by collection. A turn retrieves its collections concurrently and emits each
    event when that collection finishes, so their relative order differs between
    two runs of the SAME binary; the multiset and everything after the run are
    still compared in order."""
    if not isinstance(events, list):
        return events
    out: list[Any] = []
    run: list[Any] = []

    def is_retrieval(e: Any) -> bool:
        return isinstance(e, dict) and e.get("event") == "progress" and isinstance(e.get("data"), dict) and e["data"].get("stage") == "retrieval"

    for e in events + [None]:
        if e is not None and is_retrieval(e):
            run.append(e)
            continue
        out.extend(sorted(run, key=lambda x: json.dumps(x, sort_keys=True)))
        run = []
        if e is not None:
            out.append(e)
    return out


def summarize(findings: list[Finding]) -> dict[str, Any]:
    counts: dict[str, int] = {}
    for f in findings:
        counts[f.kind] = counts.get(f.kind, 0) + 1
    failing = [f.as_dict() for f in findings if f.failing()]
    return {
        "counts": counts,
        "failing": failing,
        "added": [f.as_dict() for f in findings if f.kind == "added"],
        "informational": [f.as_dict() for f in findings if f.kind == "informational"],
        "passed": not failing,
    }


def status_expectations(transcript: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Cases whose observed status differs from the case file's expectation."""
    return [
        {"case": e["id"], "expected": e["expected_status"], "observed": e["response"]["status"]}
        for e in transcript
        if "status_ok" in e and not e["status_ok"]
    ]


def collect_ids(value: Any, out: set[str] | None = None) -> set[str]:
    """Every UUID string inside a JSON value (used to build the seed manifest)."""
    out = set() if out is None else out
    if isinstance(value, str) and is_id(value):
        out.add(value.lower())
    elif isinstance(value, list):
        for v in value:
            collect_ids(v, out)
    elif isinstance(value, dict):
        for v in value.values():
            collect_ids(v, out)
    return out


def load_proto_routes(proto_text: str) -> dict[tuple[str, str], tuple[str, bool]]:
    """Map ``(METHOD, /path/{param})`` to ``(rpc name, server-streaming)``
    from the ``// METHOD /path`` comments in ``server_api.proto``."""
    routes: dict[tuple[str, str], tuple[str, bool]] = {}
    pending: tuple[str, str] | None = None
    for line in proto_text.splitlines():
        s = line.strip()
        m = re.match(r"^//\s*(GET|POST|PUT|PATCH|DELETE)\s+(\S+)\s*$", s)
        if m:
            pending = (m.group(1), m.group(2))
            continue
        r = re.match(r"^rpc\s+(\w+)\s*\(\s*ServerApiRequest\s*\)\s*returns\s*\(\s*(stream\s+)?ServerApiResponse\s*\)", s)
        if r and pending:
            routes[pending] = (r.group(1), bool(r.group(2)))
            pending = None
    return routes


def load_json(path: str) -> Any:
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def dump_json(path: str, value: Any) -> None:
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        json.dump(value, fh, indent=2, sort_keys=True)
        fh.write("\n")
