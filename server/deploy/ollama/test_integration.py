# SPDX-License-Identifier: Apache-2.0
"""Live REST/gRPC Ollama acceptance on the isolated Compose Server.

Requires grpcio and the dependencies of clients/python. Creates a small fixture
corpus in the evaluation tenant. Results live in the ignored server/scratch tree.
"""

from __future__ import annotations

import argparse
import base64
import json
import math
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "clients/python/src"))
import grpc
from munarium_client._proto.mmp.v1 import provider_pb2 as pb
from munarium_client._proto.mmp.v1 import provider_pb2_grpc as rpc


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--http", default="http://127.0.0.1:28080")
    parser.add_argument("--grpc", default="127.0.0.1:25051")
    parser.add_argument("--provider-endpoint", default="http://ollama:11434")
    parser.add_argument(
        "--output", type=Path, default=ROOT / "server/scratch/ollama/integration.json"
    )
    parser.add_argument(
        "--verify-persisted",
        type=Path,
        help="Verify a previous run after Server recreation, without reapplying configs",
    )
    args = parser.parse_args()
    headers = {
        "Authorization": "Bearer ollama-evaluation-token",
        "X-Munarium-Uid": "evaluator",
    }
    metadata = (
        ("authorization", "Bearer ollama-evaluation-token"),
        ("munarium-uid", "evaluator"),
    )
    results: dict = {"started": time.time(), "checks": []}

    def http(path, body=None, *, yaml=False, expected=200, other=False):
        hs = dict(headers)
        if other:
            hs["Authorization"] = "Bearer ollama-other-token"
        data = None
        if body is not None:
            hs["Content-Type"] = "text/yaml" if yaml else "application/json"
            hs["Idempotency-Key"] = str(uuid.uuid4())
            data = (body if yaml else json.dumps(body)).encode()
        request = urllib.request.Request(args.http + path, data=data, headers=hs)
        try:
            with urllib.request.urlopen(request, timeout=180) as response:
                status, raw = response.status, response.read()
        except urllib.error.HTTPError as error:
            status, raw = error.code, error.read()
        assert status == expected, (path, status, raw.decode()[:800])
        if not raw:
            return None
        if path == "/readyz":
            return raw.decode()
        return json.loads(raw)

    def passed(name):
        results["checks"].append(name)
        print(f"PASS {name}", flush=True)

    try:
        assert http("/version")["version"] == "1.1.0"
        http("/readyz")
        if args.verify_persisted:
            prior = json.loads(args.verify_persisted.read_text())
            assert prior["status"] == "passed"
            assert http("/v1/providers/example-ollama/health")["healthy"]
            facts = http(f"/v1/versions/{prior['versionId']}/facts")["facts"]
            assert any(f["subject"] == "invocation" for f in facts), facts
            turn = http(
                f"/v1/sessions/{prior['sessionId']}/turns",
                {"query": "Who is the Cedar observatory curator?", "complete": True},
            )
            assert (
                turn["hits"]
                and turn["envelopes"]
                and "Mira Chen" in turn["completion"]["text"]
            ), turn
            passed(
                "provider reload, invocation facts, session/index persistence and grounded answer after recreation"
            )
            results["groundedTurn"] = turn
            results["status"] = "passed"
            return
        config = (
            (ROOT / "server/runbooks/providers/example-ollama.yaml")
            .read_text()
            .replace("http://ollama:11434", args.provider_endpoint)
        )
        http("/v1/providers", config, yaml=True)
        inventory = http("/v1/providers")["providers"]
        assert next(p for p in inventory if p["name"] == "example-ollama")[
            "credential_ok"
        ]
        assert http("/v1/providers/example-ollama/health")["healthy"]
        http("/v1/providers/example-ollama/health", expected=404, other=True)
        passed(
            "REST registration, credential-free inventory, model health, tenant isolation"
        )

        version = http("/v1/versions", {})["version_id"]
        prompt = "The project code is MAPLE. What is the project code? Reply with only the code."
        completion = http(
            "/v1/providers/example-ollama/complete",
            {
                "prompt": prompt,
                "max_tokens": 64,
                "temperature": 0,
                "version_id": version,
            },
        )
        assert completion["text"].strip() == "MAPLE", completion
        assert completion["provider"] == "ollama" and completion["input_tokens"] > 0
        assert completion["output_tokens"] > 0 and completion["invocation_event_id"]
        selected = http(
            "/v1/providers/default/complete",
            {
                "provider": "ollama",
                "tier": "fast",
                "prompt": prompt,
                "max_tokens": 64,
                "temperature": 0,
            },
        )
        assert selected["text"].strip() == "MAPLE" and selected["model"] == "qwen3:1.7b"
        http("/v1/providers/default/complete", {"prompt": prompt}, expected=502)
        http(
            "/v1/providers/default/complete",
            {"provider": "ollama", "tier": "frontier", "prompt": prompt},
            expected=400,
        )
        passed(
            "REST completion, explicit family/tier selection, tokens and invocation provenance"
        )

        with grpc.insecure_channel(args.grpc) as channel:
            client = rpc.ProviderServiceStub(channel)
            applied = client.ApplyProviderConfig(
                pb.ApplyProviderConfigRequest(yaml=config),
                metadata=metadata,
                timeout=30,
            )
            assert applied.config_name == "example-ollama"
            assert client.ProviderHealth(
                pb.ProviderHealthRequest(config_name="example-ollama"),
                metadata=metadata,
                timeout=30,
            ).healthy
            answer = client.Complete(
                pb.CompleteRequest(
                    config_name="example-ollama",
                    prompt=prompt,
                    max_tokens=64,
                    version_id=version,
                ),
                metadata=metadata,
                timeout=180,
            )
            assert (
                answer.text.strip() == "MAPLE"
                and answer.provider == "ollama"
                and answer.invocation_event_id
            )
            try:
                client.Complete(
                    pb.CompleteRequest(
                        config_name="example-ollama",
                        prompt=prompt,
                        tools_json='[{"function":{"name":"test"}}]',
                    ),
                    metadata=metadata,
                    timeout=30,
                )
                raise AssertionError("tools must be rejected")
            except grpc.RpcError as error:
                assert error.code() == grpc.StatusCode.INVALID_ARGUMENT, error
            inputs = [
                f"MAPLE verification {uuid.uuid4()}",
                "The observatory curator is Mira Chen.",
            ]
            embedded = http(
                "/v1/providers/example-ollama/embed",
                {"inputs": inputs, "version_id": version},
            )
            assert (
                len(embedded["vectors"]) == 2
                and embedded["dimensions"] == 384
                and not embedded["cache_hit"]
            )
            assert embedded["invocation_event_id"]
            cached = client.Embed(
                pb.EmbedRequest(
                    config_name="example-ollama", inputs=inputs, version_id=version
                ),
                metadata=metadata,
                timeout=180,
            )
            assert (
                cached.cache_hit
                and cached.dimensions == 384
                and len(cached.vectors) == 2
            )
            assert all(
                math.isclose(a, b, rel_tol=1e-6, abs_tol=1e-8)
                for a, b in zip(cached.vectors[0].values, embedded["vectors"][0])
            )
            passed(
                "gRPC registration/health/completion, tool rejection, REST-to-gRPC embedding cache"
            )

        missing = http(
            "/v1/providers/example-ollama/complete",
            {"model": "munarium-missing-model:test", "prompt": prompt},
            expected=502,
        )
        results["missingModelError"] = missing
        truncated = http(
            "/v1/providers/example-ollama/complete",
            {
                "prompt": "Count from one to twenty, one number per line.",
                "max_tokens": 1,
                "temperature": 0,
            },
        )
        assert truncated["stop_reason"] == "length", truncated
        passed("missing-model error and token-limit truncation")

        unavailable = config.replace(
            "name: example-ollama", "name: ollama-unavailable"
        ).replace(args.provider_endpoint, "http://127.0.0.1:1")
        http("/v1/providers", unavailable, yaml=True)
        http(
            "/v1/providers/ollama-unavailable/complete",
            {"prompt": prompt},
            expected=502,
        )
        limited = config.replace(
            "name: example-ollama", "name: ollama-limited"
        ).replace("rpm: 60", "rpm: 1")
        http("/v1/providers", limited, yaml=True)
        http(
            "/v1/providers/ollama-limited/complete",
            {"prompt": prompt, "max_tokens": 64},
        )
        http(
            "/v1/providers/ollama-limited/complete",
            {"prompt": prompt, "max_tokens": 64},
            expected=429,
        )
        passed("unreachable endpoint and configured request budget")

        name = "ollama-smoke-" + uuid.uuid4().hex[:8]
        shape = f"""apiVersion: munarium.ioka.io/v1
kind: Shape
metadata: {{ name: {name}, version: 1 }}
spec:
  chunking: {{ strategy: para@1, max_chars: 800 }}
  indexing: {{ rrf_k: 60, candidate_n: 50 }}
"""
        runbook = f"""apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: {{ name: {name}, version: 1 }}
spec:
  sources: {{ prefix: '{name}/' }}
  collections:
    - name: {name}
      shape: {name}@1
      accessLevel: 0
      sources: {{ filenamePrefix: '{name}/', mediaTypes: [text/plain] }}
  retrieval: {{ topK: 4, rrfK: 60, candidateN: 50 }}
  models:
    default: {{ provider: example-ollama, tier: fast }}
    tasks:
      completion: {{ provider: example-ollama, tier: fast }}
  completion:
    contextCharBudget: 4000
    maxTokens: 256
    promptTemplate: |
      Answer only from the evidence. Give the curator's name in one short sentence.
      Evidence: {{context}}
      Question: {{query}}
  steps:
    - resolveSources: {{}}
    - buildIndex: {{}}
    - verify: {{}}
    - cutover: {{ approval: required }}
    - retireOld: {{ keep_versions: 2 }}
"""
        http("/v1/shapes", shape, yaml=True)
        validation = http("/v1/runbooks/validate", runbook, yaml=True)
        assert validation["valid"], validation
        http("/v1/runbooks", runbook, yaml=True)
        documents = [
            (
                "observatory.txt",
                "The Cedar observatory curator is Mira Chen. The observatory opens at 09:00.",
            ),
            (
                "library.txt",
                "The Cedar library librarian is Luis Vega. The library opens at 10:00.",
            ),
        ]
        for filename, text in documents:
            http(
                "/v1/ingest",
                {
                    "filename": f"{name}/{filename}",
                    "media_type": "text/plain",
                    "content_base64": base64.b64encode(text.encode()).decode(),
                },
            )
        run = http(f"/v1/runbooks/{name}/runs", {})
        status = http(f"/v1/runs/{run['run_id']}")
        assert status["state"] == "awaiting_approval", status
        pending = [s for s in status["steps"] if s["state"] == "awaiting_approval"]
        assert len(pending) == 1, status
        # This approves only the script's two synthetic documents in its unique collection.
        http(f"/v1/runs/{run['run_id']}/steps/{pending[0]['ordinal']}/approve", {})
        assert http(f"/v1/runs/{run['run_id']}")["state"] == "done"
        session = http(f"/v1/runbooks/{name}@1/sessions", {})
        turn = http(
            f"/v1/sessions/{session['session_id']}/turns",
            {"query": "Who is the Cedar observatory curator?", "complete": True},
        )
        assert any(
            h["source_path"] == f"{name}/observatory.txt" for h in turn["hits"]
        ), turn
        assert turn["envelopes"] and "Mira Chen" in turn["completion"]["text"], turn
        assert turn["completion"]["model"] == "qwen3:1.7b", turn
        results["groundedTurn"] = turn
        results["versionId"] = version
        results["sessionId"] = session["session_id"]
        passed(
            "synthetic corpus ingestion, local index build/cutover, provenance and Ollama grounded answer"
        )
        results["status"] = "passed"
    except Exception as error:
        results["status"] = "failed"
        results["error"] = str(error)
        raise
    finally:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(results, indent=2) + "\n")
        print(f"Evidence: {args.output}", flush=True)


if __name__ == "__main__":
    main()
