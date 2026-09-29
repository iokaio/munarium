# Contracts

Three directories. Two are cross-tree — **inbound**: a contract owned by another tree,
copied here so the server can build, test and validate against it **without a crate
dependency on that tree**; **outbound**: the server's own interface, cut into a bundle
that the public client libraries vendor — and one is **internal**: a contract this
tree owns and its own tests read, kept beside the other two because its bytes are
load-bearing in the same way.

| Directory | Direction | Source of truth | Owner |
|---|---|---|---|
| [`matrix/`](matrix/) | inbound | [`iokaio/munarium-matrix/contract`](https://github.com/iokaio/munarium-matrix/tree/main/contract), pinned by the vendored `contract.lock` | Munarium Matrix |
| [`mmp/`](mmp/) | outbound | this tree (`proto/`, `docs/api/`, two crates, `conformance/SCENARIOS.md`) | Munarium Server |
| [`datastore/`](datastore/) | internal | this directory: the `artifact@1` schemas, the Python reference implementation and the identity vectors | Munarium Server (`munarium-datastore`) |

## The internal contract: `datastore/`

The Munarium Datastore artifact contract — `artifact@1` canonicalization, the three
canonical documents (build spec, artifact plan, manifest), and the fifteen identity
vectors that prove the logical/physical split on concrete files. It is not a boundary
between trees: `munarium-datastore` and `munarium-server` share this workspace. It is a
contract because `artifact_id` is a content hash, so the encoding rules are load-bearing
forever; `datastore/README.md` says why. Two things read it: `munarium-datastore`'s
`contract_vectors` test, which must reproduce every vector's hash byte for byte, and
`datastore/validate_examples.py`, the self-consistency gate (29 checks). It lives here so
that nothing this workspace tests reads outside `server/`; the lexical-compatibility
oracle lives at `src/munarium-datastore/tests/fixtures/lexical-compat/`, beside the parity
test, for the same reason.

## The outbound bundle: `mmp/`

`mmp/publish.py` cuts the **public MMP contract bundle**: the ten protos, `openapi.json`, `errors.md`, the `munarium-api-types` and
`munarium-proto` crates lifted out of the workspace with concrete dependency versions
and Apache-2.0 headers, and `conformance/SCENARIOS.md`, plus `VERSION`, a sha256
`contract.lock`, and the bundle's own `README.md`, `LICENSE` (Apache-2.0, verbatim from
apache.org) and `NOTICE`, all of which live beside the publisher here. Every text file
is written UTF-8, LF, no BOM, so a cut is the same bytes on every platform; CI proves
it with `publish.py --self-test`, and `--check <dir>` tells a vendored copy whether it
still matches what this tree cuts. Source of truth stays here; the bundle is never
hand-edited. The client libraries under [`clients/`](../../clients/) consume this bundle;
cut one with `py server/contract/mmp/publish.py --out <dir>`, and prove a vendored copy
still matches what this tree cuts with `--check <dir>`.

## The rule

`server/contract/matrix/` is a **cut** of the standalone Matrix repository's
`contract/`, made by its `contract/publish.py`: every contract file verbatim (UTF-8, LF, no
BOM, whatever the checkout's line endings), plus a `contract.lock` naming the contract
version, the source commit, a sha256 per file and a digest over the sorted list. Not a
subset, not a reformat, not a hand edit. A recursive diff requires both repositories
in one workspace. Two
proofs are used instead:

- **where the standalone Matrix checkout exists** — this repository's CI checks out
  an audited, pinned `iokaio/munarium-matrix` revision, while local gates find it beside
  this repository or through `MUNARIUM_MATRIX_ROOT`; the copy must equal a fresh cut,
  source commit ignored;
- **everywhere** — `munarium-api-types`' `matrix_contract` test verifies every vendored
  file against `contract.lock` and refuses anything unlisted, with no sibling at all.

Both are byte comparisons, deliberately: a console redirect on Windows adds a UTF-8 BOM
and produces drift that is invisible when you read two files side by side. The
publisher's normalization is what makes the bytes the same on every platform, and
`server/contract/**` is pinned `eol=lf` so a checkout cannot undo it.

## Changing the contract

Edit `contract/` in `iokaio/munarium-matrix` and move `VERSION` according to the
compatibility rule in the
[Matrix contract documentation](https://github.com/iokaio/munarium-matrix/blob/main/contract/README.md)
(major = wire break, minor = additive, patch = examples and prose). Then re-cut this
copy in a coordinated Server pull request. The standalone repository owns the source;
the pinned Matrix revision in `server-ci.yml` advances with that re-vendoring change.

To re-cut the copy:

```bash
rm -rf server/contract/matrix && py ../munarium-matrix/contract/publish.py --out server/contract/matrix
py ../munarium-matrix/contract/publish.py --check server/contract/matrix
```

## Why a copy and not a shared crate

Ground rule 1: Matrix never depends on a Server crate and Server never depends on a
Matrix crate. A shared crate would be a dependency edge, and the edge is
what the rule exists to prevent — Matrix ships as its own image on its own
release cadence, and a shared crate would couple them at build time in exactly
the way that makes independent deployment a fiction. A vendored copy plus a
drift check buys the same safety with no edge.

## What reads this at build time

Nothing yet, but keep the constraint in mind: `server/.dockerignore` must not
exclude `contract/`. If a crate ever `include_str!`s a schema from here, an
excluded directory breaks the image build outright — which is how a stray
`runbooks/` exclusion was once found, as 26 "couldn't read" errors.
Note that `.dockerignore` does exclude `**/*.md`, so the vendored `README.md`
is absent from the build context; embed a `.json` schema, never the prose.
