# Munarium concepts

This is the mental model behind the Munarium client libraries: what the API guarantees,
independent of any one language. It describes **observable behavior** — what a program sees
when it calls the API — never how the server produces it. For "how do I call this in my
language," see the per-language guides in [`../guides/`](../guides/); this set explains why
the calls are shaped the way they are.

Three rules hold across every page here, and they are why this set reads differently from a
typical vendor tutorial:

- **Observable behavior only.** No implementation detail, no measured benchmark, no corpus
  content, no roadmap. If a page needs one of those to make its point, the point belongs in
  Munarium Server's own documentation instead.
- **Adapted, not duplicated.** Each page is written fresh from the public contract
  (`../../contract/`) and the per-language guides; it doesn't restate their code samples.
- **The same license as the code.** This directory ships under Apache-2.0, exactly like the
  client source it documents.

## The pages

| Page | What it explains |
|---|---|
| [The fact ledger](fact-ledger.md) | Claims, updates and corrections; how supersession is resolved; what a version lineage is; point-in-time pins. |
| [Sessions and turns](sessions-and-turns.md) | What a retrieval turn returns, what it costs, and the streaming turn's stage sequence. |
| [Runbooks and access](runbooks-and-access.md) | Runbooks and shapes as the unit of access; collections; model tiers and `model_override`. |
| [Capability tokens](capability-tokens.md) | The two credential kinds, access level and compartments, the uid contract, and revocation. |
| [Evidence](evidence.md) | `[evidence/<id>#<row>]` citations, the manifest, the five kinds of evidence a turn can cite, and why sealing has no client-side call. |
| [Conformance as specification](conformance-as-spec.md) | The seven wire scenarios every client implements, read as the executable definition of correct behavior. |
| [Compatibility and errors](compatibility-and-errors.md) | How client and server versions relate, and how every client decodes a failure the same way. |

## Trying it

The public contract bundle (`../../contract/`) and the offline test suites in each language
directory are the parts of Munarium anyone can run without a server: build, run the unit tests,
read the conformance scenarios. To see a live server answer real questions, request access to
the gated demo — see the root [`README.md`](../../README.md#trying-it). There is no public
evaluation server image.
