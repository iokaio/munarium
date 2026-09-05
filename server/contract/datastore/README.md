# The Munarium Datastore artifact contract

The normative shape and identity rules for a Munarium search artifact. Three
canonical documents, one canonicalization, two identifiers.

Unlike [matrix/contract/](../../../matrix/contract/README.md), this is **not** a
cross-tree boundary — `munarium-datastore` and `munarium-server` live in the
same workspace (it sits beside the tests that read it). It is a contract for a different reason: `artifact_id` is a
content hash, so the encoding rules are load-bearing forever. An artifact sealed
today must still verify against a reader built in two years, and a canonicalizer
that drifts by one byte silently renames every artifact ever sealed.

## What is here

| File | Normative content |
|---|---|
| `VERSION` | The contract version. One line, semver, no `v` prefix. |
| `canonicalization.schema.json` | `artifact@1` — how a document is encoded before hashing. Not a message: a machine-readable specification, with the rules as `const` values so a drift is a test failure rather than a discussion. |
| `build-spec.schema.json` | The logical indexed corpus. Its hash is `index_version_id`. |
| `artifact-plan.schema.json` | One physical realization. Its hash is `artifact_plan_sha256`. |
| `manifest.schema.json` | The content-pure manifest. Its hash **is** `artifact_id`. |
| `examples/` | One valid document per schema. |
| `vectors/identity-vectors.json` | 15 identity vectors and 5 refusal cases. **The executable half of the contract.** |
| `canonicalize.py` | The reference implementation of `artifact@1`. |
| `gen_vectors.py` | Regenerates the vectors from the examples. |
| `validate_examples.py` | The gate. Exit 0 means the committed contract is self-consistent. |

## The invariant

One sentence, and the reason the split exists:

> Changing a build timestamp, builder or attempt moves **neither** identifier.
> Changing an engine, revision or envelope format moves **`artifact_id` only**.
> Changing a source, chunker, extractor, analyzer contract or embedder moves the
> **logical `index_version_id`**.

The middle line is the one that pays for the whole design: it is what lets an
engine upgrade be a binding change rather than a reindex, and what lets a
session's pin survive a promotion.

`vectors/identity-vectors.json` proves it on concrete documents — thirteen
mutations, each asserting which identifier moved. A Rust implementation must
reproduce every hash byte for byte. Where the two disagree, **the vectors are the
contract** and both implementations are suspect until one is shown to violate the
schema.

## Two things that are deliberately absent from the manifest

**Non-content metadata.** No `built_at`, `builder`, `attempt_id` or `hostname`.
They live in the catalog and attempt rows. Purity is not tidiness: it is what
makes two byte-identical rebuilds converge on one catalog row by rule, instead of
colliding.

**Authority.** No `tenant_id`, no `index_version_id`. An identical corpus in two
tenants legitimately produces one `artifact_id`; isolation lives in the catalog
key and in the runtime `ArtifactCacheKey { isolation_domain, logical_version_id,
artifact_id }`. Putting either in the manifest would make a content hash pretend
to be an authorization boundary — and something that looks like a boundary but
is not is worse than no boundary at all.

Both absences are enforced by `additionalProperties: false` and tested as
refusal cases, because a schema silently loses its negative space the moment
someone relaxes that keyword.

## Why floats are forbidden

`artifact@1` is RFC 8785 (JCS) with one restriction: no floating-point numbers.
JCS number canonicalization is ES6 `Number::toString`, and it is the part
implementations get wrong — shortest round-trip formatting, exponent thresholds,
negative zero. Nothing in these documents needs a float: dimensions, byte
lengths, counts and positions are integers, and a genuine ratio is carried as a
decimal **string** at a declared scale, exactly as `canon@1` does. Removing a
whole class of implementation divergence is worth more than the convenience it
costs.

## Running the gate

```bash
cd server/contract/datastore
python validate_examples.py     # 29 checks; exit 0 = self-consistent
python gen_vectors.py           # regenerate after an intentional change
```

Requires `jsonschema`. `gen_vectors.py` asserts each invariant as it generates,
so a change that breaks one fails at generation rather than producing vectors
that agree with a mistake.

## Compatibility rule

Semver over the **meaning**, not the file bytes:

- **patch** — examples, descriptions or comments change. No consumer action.
- **minor** — a new optional field, or a new value in an open enum. A consumer
  MUST ignore fields it does not know.
- **major** — anything that changes an existing hash. This renames every
  artifact ever sealed, so it is a migration with a rebuild plan, not a version
  bump. `artifact@1` is expected to outlive several envelope `format_version`s.

Note the layering: the envelope `format_version` may move within `artifact@1`,
because it describes what the files are, not how the document is hashed.
