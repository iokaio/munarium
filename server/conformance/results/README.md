# Public evaluation results

This index retains sanitized P13 records. The
[record schema](records.schema.json) defines manifests, raw outcomes and grading
receipts; [the guide](../../docs/guides/frozen-evaluation.md) explains production,
validation, regrading and measured-result markers.

Records named `sha256-*.json` are immutable by convention and checked by content
identity. Append revisions and diagnostic reruns; retain original raw evidence,
failed rows and old scores. Do not copy private datasets or ignored run residue
into this index. The committed pilot uses only the public fictional fixture.

The offline pilot is a recording/grader control, with no Server, live provider,
governance-benefit comparison or product latency claim. Its token usage is unknown.

## Scripted pilot 001

Five planned rows completed and passed the lexical/evidence checks across four
fictional histories. All five rows retain unknown usage. This result qualifies
the producer → persisted evidence → grader → receipt path only.
<!-- measured-result: sha256:0cc3332afd571582467df50a72556ffa52b496785521130e04ce60cb67b14bce source: sha256:09b3ee2c2ede003112199782f306f7852b6006ae5abaa240dffe8ecdf2b615e2 -->

| Record | Immutable artifact |
|---|---|
| Frozen manifest | [Manifest](sha256-0a1f85e9584852268814288e29179886fe77d44b8b7139ca456048a67c84a1e0.json) |
| Raw answers/evidence | [Raw outcomes](sha256-2d16e13f302d0892a79951cd6a0cd6edf22a9691c46e8b3b71b7b276d247b185.json) |
| Original grading receipt | [Revision 1](sha256-ecf8f8da33ea111256a7010fc03ff9638a2031c88a4faebe3c914d74d1a18794.json) |
| Formatting/lint regrade | [Revision 2](sha256-71b4a91d80581e1a104933104b6963b60db21361dba48a97540c8c5644921d89.json) |
| Current grading receipt | [Revision 3](sha256-0cc3332afd571582467df50a72556ffa52b496785521130e04ce60cb67b14bce.json) |

The manifest was frozen on disk before this diagnostic pilot; it was not a
committed preregistration or a qualifying held-out campaign. Run ID:
`p13-scripted-pilot-001`. No tokens, private data or machine resource names were
used. The Python version and hashes in the manifest identify the local producer.
Revision 2 regrades the same raw bytes after source formatting and lint fixes;
it retains the original result and records the updated scorer/evaluator hashes.
Revision 3 retains those records and regrades the same raw bytes after fixing
malformed/whitespace completion handling and failure precedence in the CLI.
To execute the current producer, freeze a new manifest; replaying the original
manifest against changed producer source correctly fails its identity check.

## Local validation

- `py -m unittest discover -s scripts -p 'test_*.py'`: 24 tests passed,
  including positive/negative grader, producer and documentation-marker controls.
- `cargo test --manifest-path server/Cargo.toml -p munarium-server docs_coverage`:
  all six documentation tests passed.
- `py scripts/frozen_eval.py verify server/conformance/results`: all retained
  records validate. The JSON Schema was also checked with `jsonschema` locally;
  production scripts require only the standard library.
- `py scripts/docs_linkcheck.py`, `py check_license.py`, and
  `py clients/check_compatibility.py`: passed.
- Ruff lint passed for all changed Python files; formatting passed for the three
  new scripts. `git diff --check`: passed.
- `py scripts/private_material_scan.py`: failed with 350 existing findings in
  local scratch files and nested snapshots. The unchanged scanner, scoped to
  the P13 diff, passed. No files or scanner exceptions were removed or changed
  to hide the full-scan findings.

No paid run, remote CI run, live Server campaign, calibrated product latency
measurement or D6 held-out qualification was performed.
