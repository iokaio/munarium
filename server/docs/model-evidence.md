<!-- SPDX-License-Identifier: Apache-2.0 -->
# Model evidence is data

Answer composition, session completion, evidence hierarchy layers, and corrective
completion retries use a shared model-only JSON envelope. Every envelope identifies
its `source_role`, `historical_pin`, and `citation_id`, and states
`execution_authority: false` and `approval_authority: false`. Content and metadata
are JSON values; quotes, newlines, section titles, or field names inside a document
cannot become sibling envelope fields. This representation changes model input,
not public DTOs, source IDs, or response citation IDs.

The pin describes the view actually served: a verified collection/index and
watermark for document retrieval, a sealed artifact identity for structured
evidence, or a runbook-selected memory version for facts. Fact layers currently
have no sequence pin, so `as_of_seq` is explicitly null. Unknown pins are null;
the renderer does not manufacture historical guarantees. Document envelopes keep
each layer's original collection association, including when a shared source has
the same chunk ID in two indexes.

Answer passages retain request-local `p1`, `p2`, etc. identifiers, which the
server maps back to the caller's opaque IDs after validating the response.
Session document citations retain `collection/chunk_id`; sealed table rows retain
the sealer's row IDs. Quotes and references still pass through the existing
validators. Rendering a legitimate quote does not confer authority on its text.

Session prompt templates are substituted once. A literal `{query}` in a source
or `{context}` in a question cannot trigger another substitution inside an
already encoded envelope. Context budgets include JSON framing. Hierarchy
truncation keeps complete envelopes and complete row/hit/fact prefixes; a shortened
table is marked `TRUNCATED`. A single item whose envelope cannot fit is dropped.
Layers that require a complete result continue to be preserved or dropped whole.
The core composer's additive model renderer is used for hierarchy facts; the
public brief's text, hash, and existing budget calculation remain unchanged.

The fictional adversarial fixtures exercise field impersonation, changed pins,
publication requests, access elevation, and approval bypass. Scripted completions
may repeat those requests as prose with a valid quote; the tests then verify that
publication policy, collection access, active index, vocabulary, and a waiting
approval gate remain unchanged. Separate requests with the same query capability
are rejected at the real mutation and approval handlers on REST and native gRPC.
Malformed source provenance, unknown citation IDs, altered quotes, and revoked
access retain their rejection tests.

These are deterministic framing, provenance, and authorization checks. They do
not measure model answer quality or prove general prompt-injection immunity.
There is no model tool executor, and model output is never an approval credential.
