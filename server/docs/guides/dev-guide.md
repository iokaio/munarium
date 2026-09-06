# Munarium Server: Developers Guide

## About this guide

This guide is part of Munarium Server and is licensed with it under the Apache
License 2.0. Copy it, quote it and adapt it freely; the only thing the license
does not grant is the use of the Munarium and Ioka names, which
TRADEMARK.md explains.

It documents version 1.0. What 1.0 commits to — the wire contract, the
`MUNARIUM_*` configuration contract, and additive-only migrations — is stable
under semantic versioning. Internal APIs and crate boundaries are not, and this
guide says so where it teaches them.

## Preface

### What this book is

This guide teaches you to develop **with** Munarium in both senses of the
phrase. The rest of `server/docs/` tells you what the system is
([architecture.md](../architecture.md)), what it exposes
([api/rest.md](../api/rest.md), [api/grpc.md](../api/grpc.md)), and how to
operate it (`README.md`).

None of it teaches a developer how to *work*. It does not explain how to get
a change built, tested, and through CI without finding old problems again. It
does not tell you which of twenty crates owns the code you need to change or
why an obvious dependency might break the Docker build. It also does not show
how to turn a corpus of a hundred thousand documents into an AI research
assistant with auditable answers. Nor does it explain what your UI should show
when the platform marks a fact as *disputed*. That is this document's job.

A developer's guide for a system like this one has to serve two people who
rarely read the same books. The first is the engineer who will change the
platform. This work may include adding an endpoint, porting a kernel feature,
writing a migration, or tracing a bug through the gate pipeline. The second
is the engineer who will *use* the platform to build something for people who
have never heard of it. Examples include a chat assistant for a due-diligence
data room or a research tool for forty years of regulatory filings. Another
example is a support copilot that knows which release note replaced a given
knowledge-base article. Both engineers need depth, but about different
things. So this book is two books sharing a spine.

- **Part I: Developing Munarium itself.** You are changing code under
  `server/`. Part I covers the development loop: build, the five-tier test
  ladder, conformance, and CI. It also provides a set of how-do-I recipes,
  each anchored to a canonical existing example you can copy rather than a
  pattern you must imagine. It ends with the gaps ledger: the honest list of
  what is folklore, missing, or half-built, so you inherit our known
  unknowns instead of rediscovering them.
- **Part II: Developing AI-enabled applications on Munarium.** You have a
  corpus and users. You will probably never touch Rust, and you should not
  have to. A new corpus application is configuration, including shapes,
  runbooks, collections, and tokens, plus your own UX. Part II covers
  application patterns, and it has an unusual property for an
  architecture-patterns text: **every pattern was worked through against a
  real corpus before it was written down.** Part II describes what each one
  is strongest at and what to design against, the way Part I cites file and
  line.
- **Measure your own.** The numbers that matter for your application are the
  ones you measure yourself. Before a shape or a runbook reaches production,
  give it a bench: a disposable server, a slice of the real corpus, and a
  graded answer key, so that the shape's key discipline and the runbook's
  retrieval dials are tuned against your documents rather than inherited from
  someone else's. §21 step 8 shows the smallest form of that practice, and
  §19's "where it lives today" verdicts say what each pattern still asks you
  to measure.

### Where this system came from, and why it matters to you

You will understand a hundred small decisions in this codebase faster if you
know the origin story, because the origin story is unusual: **the semantics
shipped before the server did.**

The memory semantics came first, and the server is their second
implementation. An append-only fact ledger with supersession and
deterministic gates that catch contradictions at write time; point-in-time
snapshot reads; a context composer that works within token budgets; promise
tracking; and drift detection between memory epochs — each of these was
settled as a behaviour, with a test that pinned it, before any of it was
written in Rust.

That ordering is why the protos are normative rather than descriptive, and
why Part II can describe what a pattern is strongest at and what its failure
mode is: those are properties of corpus applications that were established
before this codebase existed, not conclusions drawn from it.

munarium-server is the production implementation of those semantics. It is a
single static Rust binary (~30 MB, distroless) that speaks REST and gRPC. A
shared conformance suite proves parity between them. PostgreSQL is the system
of record. Document bytes remain in whichever object store you already run,
and model calls go directly to your provider accounts under your keys. The
compatibility contract between the two worlds is the **Munarium Protocol**
in the proto files under [proto/mmp/v1/](../../proto/mmp/v1/). The two worlds
never share code by design. As for the name you will type ten thousand times,
**Munarium** is an invented word for a place where memory is kept — an echo of
Muninn, Odin's raven of memory, whose loss Odin feared more than the loss of
thought. This system is built by people who agree with him.

### How to read this book

If you want a first working corpus application before reading the full book,
start with [Getting started](getting-started.md). It follows the published
Docker image through ingestion, index approval and a verified retrieval turn.

Nobody reads a developer's guide cover to cover, and this one is not designed
for it. Suggested paths:

- **New platform engineer, first week:** Preface → Introduction → §1–§3
  (orientation, setup, the development loop) → §7 (testing and conformance).
  Then recipes from §6 as your first tickets demand them. Skim §13 (gaps)
  early. It is short and may save you an afternoon.
- **Platform engineer, reviewing or designing:** §4–§5 (workspace and server
  internals) → §9–§10 (dependencies, CI, deployment) → §12 (conventions,
  including the honesty rule your review should enforce).
- **Application architect:** Introduction → §14–§15 (what a governed
  application is and its anatomy) → §19 (the patterns catalog, to find the
  precedent closest to your corpus) → §20 (platform integration).
- **Application engineer building the thing:** §15–§18 in order, then the
  worked tutorial in §21, keeping [api/rest.md](../api/rest.md) and the
  [errors registry](../api/errors.md) open in the next tab.
- **Application engineer deploying the published Server:** start with
  [§10's Docker Hub walkthrough](#deploy-the-published-docker-hub-image).
  It needs Docker Desktop and PowerShell, and uses the released binaries
  directly. Return to §2–§3 when you need to change or build the Server.
- **Anyone about to add a dependency, a migration, or an endpoint:** the
  matching recipe in §6, *before* you start. The recipes exist because each
  one encodes at least one mistake that has already been made.
- **Anyone about to take a shape or a runbook to production:** §16 (corpus
  and clearance design) → §19 (the pattern closest to your corpus, and what
  it still asks you to measure) → §21 step 8 (grade against an answer key
  before you ship). Stand up a lab for that measurement before the first
  production index build; the numbers in this book calibrate yours, they do
  not replace them.

### The three commitments

Three commitments govern how this guide is written, and you should hold it to
them ruthlessly.

**1. Every claim is verified.** Counts, commands, file paths, request bodies,
and response shapes were checked against the tree at the version stated
below. The code examples were *executed*, not composed. This is not a
stylistic flourish. It is the difference between documentation and fiction
with syntax highlighting. (The discipline caught two errors in this very
chapter's examples before publication: a required field the draft omitted,
and an enum variant that did not exist.) If you find a claim you cannot
reproduce, that is a bug in the guide. File it, fix it, and do not bend your
mental model around it.

**2. Gaps are stated, not papered over.** Things that are missing, folklore,
or half-built are marked **[gap]** inline and collected in §13. This document
never describes an aspiration as a feature. The codebase itself follows this
rule. Unshipped gRPC methods answer `UNIMPLEMENTED` rather than faking
success, the deploy job fails loudly rather than green-skipping when
unconfigured, and its documentation cannot hold a lower standard than its
error handling.

**3. The guide tracks the workspace version.** A change that invalidates a
claim here must update this document in the same change; the README
environment table and API docs follow the same rule. Captured transcripts
name the build they ran on, and several were captured during development
before 1.0 — they are kept as captures rather than re-typed against a version
they did not run on. Line-number anchors such as `rest.rs:145` point at the
version the caption names; the symbol names beside them are the durable
pointer when lines drift.

### Chapter status

The guide is complete. Every chapter was written and verified against the
version stated above, and its examples ran on live rigs. The table remains as
the completion record.

| Chapter | Status |
|---|---|
| Preface + Introduction | **written** |
| Part I §1 | **written** |
| Part I §2 | **written** |
| Part I §3 | **written** |
| Part I §4 | **written** |
| Part I §5 | **written** |
| Part I §6 | **written** |
| Part I §7 | **written** |
| Part I §8 | **written** |
| Part I §9 | **written** |
| Part I §10 | **written** |
| Part I §11 | **written** |
| Part I §12 | **written** |
| Part I §13 | **written** |
| Part II §14 | **written** |
| Part II §15 | **written** |
| Part II §16 | **written** |
| Part II §17 | **written** |
| Part II §18 | **written** |
| Part II §19 | **written** |
| Part II §20 | **written** |
| Part II §21 | **written** |
| Appendices A–F | **written** |

## Introduction

### The problem this system exists to solve

Every AI application that lives longer than a demo eventually meets the same
three failures, usually in production, usually on a Friday.

The first is the **ungrounded answer**. Your assistant cites a document. A
user opens it, but the quoted sentence is not there. Sometimes the
model invented it. Sometimes the model paraphrased something real into
something false. Sometimes the citation was real last month, against an index
that has since been rebuilt, and nobody can reconstruct what the assistant
actually saw. Whatever the cause, the user has learned that citations from
your product mean nothing. That lesson does not wash out.

The second is the **silent contradiction**. Two documents in your corpus may
disagree. Examples include a stale knowledge-base article and the release
note that replaced it, or two versions of a contract. An org chart may also
lag behind reality. A
system with no notion of contradiction answers from whichever fragment
retrieval happened to rank first, confidently, differently on different days.

The problem gets worse if your system *extracts* knowledge by building facts
over time from many sources. The newest write silently wins. An error added on
Tuesday can then overwrite a truth established over months.

The third is the **unauditable history**. A decision was made in March on the
assistant's advice. It is now August. A regulator, a litigator, or your own
postmortem asks what the system knew in March. The honest answer is that we
cannot say. The index has been rebuilt, and the prompt template has changed.
The conversation logs record what was said but not what was *known*.

Most AI memory today is one of two things: a vector database (fast lookup,
no notion of truth, contradiction, or time) or an ever-growing prompt
(expensive, unauditable, lossy). Neither has anything to say about these
failures, because neither treats them as its job. Munarium exists because
they are somebody's job. In regulated and high-stakes fields such as finance,
legal work, healthcare, IP, and security, they are the difference between an AI
system you can deploy and one you cannot.

### What Munarium is and is not

Munarium is a **governed-memory service**: a containerized platform where
an AI system's facts, documents, and retrieval indexes live under rules that
the platform enforces. Those rules do not depend on prompt engineering, good
model behavior, or a policy document.

The service is self-hosted in your perimeter with your Postgres and object
store. It is bring-your-own-key. Model calls go from your environment to your
provider accounts, so the vendor is never in the data path. The service is
also protocol-defined. The MMP spec under
[proto/mmp/v1/](../../proto/mmp/v1/) is normative. REST and gRPC provide two
transports for one behavior, and a conformance suite proves they do not drift
apart.

It is equally important to say what it is not. It is **not a vector database
with extra steps**. Retrieval is one subsystem among several. The ledger,
gates, and pins that make Munarium different have no match in a vector
store. It is **not a model host**. It never runs inference itself. It composes
context, routes calls to your accounts, and records provenance.

It is also **not an identity provider**. User authentication, including SSO,
OIDC, and MFA, belongs to the API-management layer in front of it. This is an
explicit design choice, as [security-posture.md](../security-posture.md)
explains. The mesh uses tenant-scoped service tokens and short-lived
capability tokens that your gateway creates for each user.

### The five invariants

Five invariants define the system. They are not features on a roadmap. The
conformance suite checks these properties against every storage backend and
both wire transports. Every chapter of this guide depends on at least one of
them. Learn them here, and many later design choices will feel natural rather
than arbitrary.

**1. The ledger is append-only.** Memory is a sequence of events. Nothing is
ever updated in place or deleted. A correction is a *new*
record that names what it supersedes, and the superseded record remains
readable forever underneath it. The practical consequence for your code: there
is no `UPDATE` anywhere in the write path, and any design that wants one is
wrong at the spec level, not the implementation level. The practical
consequence for your users: a tamper-evident audit trail exists **by
construction**, not as a logging feature that someone might have disabled.

**2. One sequence per lineage.** Every write to a memory lineage receives the
next value of a single monotonic sequence. Facts, anchors, promises, and
counters all draw from the same counting domain. This may sound like minor
bookkeeping. It is the key decision that makes point-in-time reconstruction
*exact*. Because there is one clock, "the state
as of seq 41" is a complete, unambiguous description of everything, not a
best-effort join across five tables with five notions of time.

**3. Pins bound everything.** Any read of facts, open promises, counters,
anchors, or the composed context brief can be pinned `as_of` a sequence
number, and one pin governs all of them together. A promise fulfilled after
the pin reads back as still open. Digests are deterministically *rebuilt* at
the pin rather than served from storage, because a stored digest reflects
the head, not the past. This is the invariant that answers the March
question: the context your application composed for a model call last
quarter can be recomposed byte-for-byte today, content hash and all.

**4. The model proposes; the mesh disposes.** Governance is part of the
write path, not an API a caller can skip. Every proposed fact passes through
deterministic gates. These check consistency against locked anchors and
conflicts against existing canon. They also check chronology, leakage, and
similarity. A claim that fails a block-severity gate is not dropped or
accepted. It is recorded
**`disputed`**, with machine-readable findings naming the rule that fired and
the evidence. Nothing is lost, and nothing unvetted becomes truth. Most
important for application UX, *disputed is a success response*. Your code
checks a status instead of catching an exception. Your UI shows a review
queue instead of an error toast.

**5. Retrieval answers carry provenance.** Hybrid search combines lexical
full-text and vector similarity through reciprocal rank. It runs over
content-addressed
sources, and every result is wrapped in a provenance envelope: the source
document hashes, the index version that produced the result, the ledger
watermark that index reflects. Indexes are versioned and **immutable once
built**. A rebuild creates a new version and requires an approved cutover.
Old versions keep resolving. The cited answer your product gave in March is
verifiable in August against the exact index that produced it.

### The shape of the system

A quick tour of the machine you are about to work on or with, from the
outside in. (Part I §4–§5 does this again at implementation depth.)

**Three API planes, one behavior.** HTTP REST runs on :8080 with JSON,
problem+json errors, and OpenAPI docs at `/docs`. Direct gRPC runs on :50051
with tonic, reflection, and a health service. An ops listener runs on :9090
for `/healthz` and `/readyz`. Deployed environments put a gateway on :443 in
front. Both data planes convert requests into the same internal service
layer, which is why the conformance suite can run identical scenarios over
both wires and call it proof rather than testing.

**A kernel that owns the rules.** The governance semantics, including the
ledger, supersession, gates, pins, composer, promises, and counters, live in one crate
(`munarium-core`) that is forbidden by CI from depending on the web framework,
the database driver, or any HTTP client. Storage, retrieval, model providers,
and document stores plug in behind traits. This is why the same kernel
semantics run in-process in tests and against Postgres in production.

**Storage in two tiers.** PostgreSQL is the system of record: the partitioned
ledger, claims projection, collections, sessions, and interactions. CI
enforces an additive-only migration policy, so upgrades never rewrite
history. Document *bytes* live separately behind a source-store seam with
six backends: Azure Blob, S3 (or any S3-compatible), GCS, local filesystem,
Postgres itself, or memory. One environment variable selects the backend.
Cloud credentials come from each platform's ambient identity, and recorded
URIs never contain a credential.

**Retrieval as versioned infrastructure.** At index time, the system extracts
DOCX and PDF text layers locally. It can also use optional OCR or a paid
document-intelligence service for scans that local tools cannot read. The
system chunks and embeds documents per a declarative *shape*. It then indexes them into
*collections*. Each has an access level and optional compartments, so
retrieval is filtered by clearance before ranking, not after.

**Models as a gateway, never a dependency.** The provider gateway speaks
Anthropic, OpenAI, and OpenRouter dialects. It resolves your keys from your
secret store at call time. A fast/capable/frontier tier (three since
2026-09-01) routes work without hard-coded model names. `/healthai` uses one
authenticated call to probe every configured family, and `GET /v1/providers`
(2026-08-23) discloses the concrete model each config's tiers resolve to
without spending a token — the free counterpart to that paid probe. The
per-call output ceilings every such call carries are one replaceable
object since 2026-09-02 — `MUNARIUM_MAX_TOKENS_*` on the container,
`GET`/`POST /v1/max-tokens` per tenant, a runbook's own `maxTokens` above
both; [docs/tokenbudgets.md](../tokenbudgets.md). A deterministic local
embedder ships so retrieval works keyless out of the box.

**Identity as a chain of custody.** Every `/v1` request carries the acting
end-user id (`X-Munarium-Uid`). The interaction capture records who asked what
and redacts responses that contain credentials. Capability tokens limit a
user to a clearance level, compartments, and scopes. They also have a hard
TTL ceiling. The reports API turns that record into usage, audit, and cost
views.

### First contact: a governed write, start to finish

The fastest way to understand the mesh is to watch it govern something. The
following sequence runs against the dev profile on a fresh clone. Every
command and response below ran against v0.1.2. The responses are real and are
abridged only where marked with `…`:

```powershell
cd server
docker compose up --build        # postgres + munarium-server on :8080
```

For the published 1.0.0 image, use
[the Docker Hub walkthrough](#deploy-the-published-docker-hub-image) instead
of this source build. It uses the same development tenant and token, with
HTTP on **18080** by default; substitute that port in the examples below.

**Step 1: every call carries three things.** The first is a bearer token. The
compose profile ships `devtoken`, which maps to tenant `dev-tenant` with the
`rw` role. The second is a **uid**, which names the end user you represent. It
is required by default and always enters the audit trail. Core commands also
need an **idempotency key**:

```bash
curl -s http://localhost:8080/v1/versions \
  -H "Authorization: Bearer devtoken" \
  -H "X-Munarium-Uid: user-1" \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: $(uuidgen)" \
  -d '{}'
# -> { "version_id": "memv-d4fbbdcf0b9e4a08ad02bc87861c30f2" }
```

That `memv-…` id names a **memory version**. It is the root of a lineage that
will collect facts. Capture it because everything below writes into it.

Leave off the uid and you get the guide's most common first-request failure:
a typed `400` problem, not a mystery. Notice the shape. It has a stable `type` slug
your code can switch on, and a `detail` that tells a human exactly what to
do. Every error in this system looks like this:

```json
{ "type": "https://munarium.ioka.io/problems/uid-required", "title": "uid required",
  "status": 400,
  "detail": "X-Munarium-Uid header (REST) / munarium-uid metadata (gRPC) is required on every /v1 request" }
```

Why three headers on every write? The token answers *may this tenant do
this*. The uid answers *which human asked*, and it supports interaction
capture and usage reports. The idempotency key makes the write safe to retry
across a network you do not trust. If you replay the same key and body, the
server returns the recorded response instead of running the write twice.

**Step 2: an accepted fact and what the response teaches.** Propose a
simple fact:

```bash
V=memv-d4fbbdcf0b9e4a08ad02bc87861c30f2   # yours will differ
curl -s http://localhost:8080/v1/versions/$V/claims \
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1" \
  -H "Content-Type: application/json" -H "Idempotency-Key: $(uuidgen)" \
  -d '{ "claim_type": "fact", "subject": "hero", "key": "eyes", "value": "blue" }'
```

```json
{ "claim": { "id": "claim-37ad149ef72b4534a140048eed127b69",
             "claim_type": "fact", "subject": "hero", "key": "eyes", "value": "blue",
             "normalized_text": "hero.eyes=blue",
             "status": "accepted", "provenance": "witnessed", "seq": 1,
             "version_id": "memv-…" },
  "findings": [], "head_seq": 1 }
```

Four fields carry the system's worldview. `normalized_text` shows the
canonical `subject.key=value` identity the gates reason over. `status:
"accepted"` means the claim passed every gate and is now canon. `provenance:
"witnessed"` records *how* this fact entered the world. Here it was directly
proposed; other provenances exist for backfilled and repaired facts. Finally,
`seq: 1` with
`head_seq: 1` is invariant #2 made visible: this write took position 1 on the
lineage's single clock, and the clock now reads 1.

**Step 3: disputed is success.** Now propose a *contradicting* fact. The
write does not return an error. It lands as `disputed` and carries the finding that
explains why. This is the write path working, not failing:

```bash
curl -s http://localhost:8080/v1/versions/$V/claims \
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1" \
  -H "Content-Type: application/json" -H "Idempotency-Key: $(uuidgen)" \
  -d '{ "claim_type": "fact", "subject": "hero", "key": "eyes", "value": "green" }'
```

```json
{ "claim": { "id": "claim-9909a936f2b84c39bbe24ed13c3560aa",
             "normalized_text": "hero.eyes=green",
             "status": "disputed", "seq": 2, … },
  "findings": [
    { "rule_id": "gate.ledger-conflict", "severity": "block",
      "message": "claim 'hero.eyes=green' conflicts with accepted canon 'hero.eyes=blue' (use a correction to supersede)",
      "detail": { "claim_key": "hero.eyes",
                  "canon_claim_id": "claim-37ad149ef72b4534a140048eed127b69",
                  "canon_seq": 1, "canon_value": "blue",
                  "proposed_value": "green" } } ],
  "head_seq": 2 }
```

Read the finding closely because your application will render thousands of
them. `rule_id` is the stable, dotted identifier of the gate that fired.
There are five always-on gates and one declaratively armed chronology gate.
Every finding names its rule. `severity: "block"` explains why the claim went
to `disputed` instead of being accepted with a warning.

The `detail` field provides machine-usable evidence. It names the exact canon
claim in conflict, its value, and the proposed value. A review UI can use this
information to show a human both sides. Note what did *not* happen: the write
was not rejected. The write consumed `seq: 2`, the disputed claim remains
queryable, and the HTTP status was 200. Governance outcomes are data, not
exceptions.

**Step 4: changing canon legitimately.** The finding's message named the
path: a **correction**. A correction is a new claim that supersedes a named
predecessor. It is the append-only answer to "the old value was wrong":

```bash
curl -s http://localhost:8080/v1/versions/$V/claims \
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1" \
  -H "Content-Type: application/json" -H "Idempotency-Key: $(uuidgen)" \
  -d '{ "claim_type": "correction", "subject": "hero", "key": "eyes",
        "value": "green", "supersedes_id": "claim-37ad149ef72b4534a140048eed127b69" }'
# -> { "claim": { "claim_type": "correction", "status": "accepted", "seq": 2,
#                 "supersedes_id": "claim-37ad…", … }, "findings": [], "head_seq": 2 }
```

(The transcript above ran in a fresh version where the correction landed at
seq 2; in your step-3 lineage it lands at seq 3.) The correction is
*accepted*. Superseding canon through the front door raises no conflict. A
current read now returns green:

```bash
curl -s "http://localhost:8080/v1/versions/$V/facts" \
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1"
# -> { "facts": [ { "claim_type": "correction", "value": "green",
#                   "supersedes_id": "claim-37ad…", "seq": 2, … } ],
#      "as_of_seq": 0, "head_seq": 2 }
```

The old fact was not deleted or overwritten. It is superseded, which is a
relationship, not an erasure. Which brings us to:

**Step 5: the pin.** Ask the same question *as of before the correction*:

```bash
curl -s "http://localhost:8080/v1/versions/$V/facts?as_of_seq=1" \
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1"
# -> { "facts": [ { "claim_type": "fact", "value": "blue", "seq": 1,
#                   "status": "accepted", … } ],
#      "as_of_seq": 1, "head_seq": 2 }
```

Blue. Not "blue, probably," but blue exactly, because supersession is resolved
*as of the pin*, on the one clock every write shares. The response tells you
both truths at once: `as_of_seq: 1` (the world you asked about) and
`head_seq: 2` (the world as it is now). This mechanism supports every "what
did we know when we decided?" answer the platform can give. It works the same
way for promises, counters, anchors, and digests.

**Step 6: the composed brief.** Finally, ask the mesh to do what your
application will do before a model call: compose canon into a
context brief:

```bash
curl -s "http://localhost:8080/v1/versions/$V/context" \
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1"
# -> { "sections": [ { "title": "Canon digest", "body": "…" },
#                    { "title": "Accepted facts", "body": "hero.eyes=green" } ],
#      "text": "## Canon digest\n…\n\n## Accepted facts\nhero.eyes=green",
#      "estimated_tokens": 19,
#      "content_hash": "c0aac42d3abef7b9a611ff73658614eb8d…", "as_of_seq": 0 }
```

Three things to notice even in a two-fact toy. The brief contains the
*accepted* worldview. The disputed green claim from step 3 would never have
entered it. It carries an `estimated_tokens` count because the composer is
budget-aware: under a token budget it degrades deterministically
(digest resolution first, oldest facts last) instead of truncating
arbitrarily. It also carries a `content_hash`. Pin the read, and the same hash
can be reproduced forever. This is how "recompose exactly what the model saw
in March" becomes a working feature.

**The same rules, through a client.** The official libraries for Rust,
Python, and .NET support both transports. They encode the contract so that
application code handles governance as data. The uid rides the constructor,
disputed is a status check, and the head-conflict retry loop is built in:

```python
from munarium_client import MunariumClient, ClientOptions

client = MunariumClient.rest(
    ClientOptions("http://localhost:8080", token="devtoken", uid="user-1"))
out = client.commands.propose_claim(v, subject="hero", key="eyes", value="green")
if out.is_disputed:
    for f in out.findings:
        print(f.rule_id, f.message)     # gate.ledger-conflict …
```

**What just happened.** In six curl commands you exercised four of the five
invariants: append-only supersession (step 4), the single sequence (visible
in every response), pins (step 5), and the model-proposes-mesh-disposes
write path (step 3). The fifth invariant, retrieval provenance, needs a
corpus. It gets one in Part II. Everything else in this book applies these
ideas at scale: identified and audited calls, governance outcomes as data,
and provenance you can keep.

### The standards this guide follows

Read these once. Every chapter assumes them. Each exists because its absence
has a specific, observed cost.

**Commands are PowerShell on a Windows dev box**, because that is what this
repo's development actually looks like. The platform is developed natively
on Windows and cross-compiled to musl only inside Docker. `bash`/`curl`
appears where a command is genuinely platform-neutral (and every REST example
is). `cd server` is assumed throughout. Long output is elided with `…`.
Elision marks are never part of real output. When a chapter must show both
shells it shows PowerShell first, because the scripts you will actually run
(`build.ps1`, `test.ps1`, `gates.ps1`, `localdeploy.ps1`) are PowerShell.

**Ports follow a two-world rule.** Examples against the compose dev profile
use the canonical **:8080**. Examples that boot a server from source use the
alternates (**18080/15051/19090**), because 8080 is routinely taken on real
dev machines, and 8443 is reserved by Windows. The repo learned this fact the
annoying way and encoded it into every script. Section 2 maps the full port
landscape, and Appendix B is the quick reference. Build this debugging habit
now: if an example fails to bind or connect, suspect the port before the code.

**Credentials in examples are the dev profile's throwaways**: `devtoken`,
tenant `dev-tenant`, and uid `user-1`. Examples never use anything else. Real
secrets appear only as *references*: an env-var name or a `file:` path, matching the
server's own `resolve_secret` seam, which exists so that no configuration
surface ever holds key material. The same rule binds this guide: a literal
secret in a documentation snippet is a defect of the same kind as a secret
in a log line, and gets fixed with the same urgency.

**Placeholders and chaining.** Angle brackets mark values you substitute
(`<run-id>`); shell variables mark values captured earlier in the same
walkthrough (`$V`). Every multi-step example is written so the steps
actually chain. An output you will need later is captured at the moment it
first appears, because a reader pasting commands in order should never hit
"where was I supposed to get that?"

**Responses are abridged, but fields are real.** JSON responses show the
load-bearing fields and elide the rest. Every field name shown exists in the
generated contract ([openapi.json](../api/openapi.json)). When this guide's
excerpt and that file disagree, the generated file wins, and the
disagreement is a guide bug. The same deference applies to
[errors.md](../api/errors.md) for anything about failures.

**Errors are handled by slug, never by message text.** Every failure shown
in this guide is a problem+json body whose `type` slug is the stable
contract. Examples include `uid-required`, `head-conflict`,
`idempotency-mismatch`, and `gate.ledger-conflict` in a finding's `rule_id`.
Message wording may improve, but slugs cannot change. Code in this guide
switches on slugs, and code you write should too. The client libraries already
do.

**File anchors cite the version they were read at.** `rest.rs:145` means line
145 in the build the surrounding passage names. Line numbers drift with every
edit. The symbol names beside them (`with_idempotency`) are
the durable pointer. When you find a drifted anchor, the symbol finds the
new line, and updating the anchor is a welcome one-line fix.

**Every example was executed before it was written down.** This standard
guides all the others, and it is not decorative. While writing this chapter,
it caught a required field the draft omitted
(`claim_type`) and an enum variant that did not exist (`"claim"` for
`"fact"`). The guide's examples are contract, not illustration. The
conformance harness applies the same rule to the platform itself: assert
the semantics, not the status codes.

### Where to go from here

If you are here to change the platform, continue straight into Part I: §1
orients you in the codebase's vocabulary, §2 gets your machine ready, and §3
teaches the loop you will live in. If you are here to build on the platform,
skip to Part II: §14 reframes what you just saw in first contact as
application architecture, and §19's patterns catalog will show you the
already-measured shape closest to the thing you are building. Either way,
keep two tabs open beside this book: the [REST guide](../api/rest.md) and
the [errors registry](../api/errors.md). This guide teaches you to think in
the system's terms. Those two references contain the full vocabulary.

---

# Part I: Developing Munarium itself

## 1. Orientation: what you are working on

Before your first change, you need three things that no amount of grepping
will give you in the right order. First, you need the system's origin story
because it explains the code's strangest design choices. Second, you need its
core doctrines: the five invariants, plane parity, and the kernel boundary.
These are the rules your reviewer will apply. Third, you need the map that
shows which document answers each kind of question. It will keep you from
asking the code about matters the docs have already settled.

This chapter provides all three. It contains almost no procedure. Section 2
gets your machine working, and §3 teaches the development loop. Everything
after this chapter assumes you have read it. Reviews also go faster for
people who have.

### What this system is

munarium-server is the production implementation of the Munarium
governed-memory service. It is one Cargo workspace under `server/` with
twenty crates and the `mmp-conformance` harness. You can count them in the
root `Cargo.toml` `members` list, from `munarium-proto` through `munarium-cli`, plus
`conformance`.

The workspace compiles into one static binary that speaks on three planes.
REST uses :8080, direct gRPC uses :50051, and the ops listener uses :9090.
PostgreSQL is the system of record for the ledger and its projections.
Document bytes live behind a source-store seam with six backends. Model calls
go through a BYOK provider gateway to your own accounts.

The workspace is library-first by design ([architecture.md §3](../architecture.md)):
the server binary is a thin shell over crates that form the canonical SDK.
Most code you will change therefore lives in a library crate. Most of what
`munarium-server` itself does is wiring.

You are not working on a system that was designed in Rust. You are working on
the *second implementation* of semantics that were settled before this
codebase existed, as the Preface tells. The ledger, the gates, the pins, the
composer, and the promise registry were each established as behaviours first;
`munarium-core` is a faithful port of them, as its first doc header states.
When server behavior seems arbitrary, the corpus problem that made it
necessary is usually the explanation.

The two implementations connect through a *specification*, never shared code.
The Munarium Protocol lives in ten files under `proto/mmp/v1/`. Learn
their names because each maps to a service surface you will meet again in §5:

```powershell
Get-ChildItem proto/mmp/v1 -Name
```

```text
admin.proto
command.proto
common.proto
ingest.proto
ledger.proto
provider.proto
query.proto
retrieval.proto
runbook.proto
```

Why use a spec instead of a shared library? The two sides cannot share one.
The first implementation used Python and SQLite by design. The server uses Rust and Postgres by
design. A system based on "import the same module" would force one side to
weaken its reason for existing.

Instead, the proto files are normative, and both implementations follow them.
The conformance suite, not the compiler, catches drift. You will see this
pattern at every boundary in the codebase. The same assertions run against
both sides to prove agreement. Agreement is not assumed just because both
sides call the same function. This pattern forms the parity doctrine below.

![System context: the memory semantics were settled first, proto/mmp/v1 carries them as the normative contract, munarium-server implements them, and the client libraries speak MMP over either wire](images/ch1-system-context.svg)

### The five invariants as a developer's checklist

The Introduction presented the five invariants as a reader's mental model.
Here they return in their working clothes: they are the review criteria for
every change to this tree, and their authoritative statement in code is
the doc header of the kernel crate,
[`src/munarium-core/src/lib.rs:1`](../../src/munarium-core/src/lib.rs) at v0.1.2, abridged to the invariants:

```rust
//! Semantic invariants carried over verbatim (the conformance suite enforces
//! them against every storage backend):
//!
//! - The ledger is append-only. A correction is a NEW claim naming
//!   `supersedes_id`; nothing is ever updated in place.
//! - `seq` is monotonic across a version lineage (one counting domain), and
//!   every store stamps from it, so ONE `as_of_seq` pin bounds facts, anchors,
//!   promises, counters, and entities together.
//! - `slice_facts` resolves supersession AS OF the pin: the superseded-set is
//!   itself filtered by `seq <= as_of_seq`, so a claim superseded later still
//!   reads as current at the pin.
//! - Gate-blocked claims are recorded `disputed`, never dropped.
//! - Digests are deterministically REBUILT under a pin, never served stored.
```

Note the phrase *the conformance suite enforces them*. These are not goals in
a comment. They are properties backed by executable checks, and §7 walks
through the scenarios. When you review your own diff or someone else's, each
invariant becomes a question:

1. **Append-only.** Does this change write over history? An `UPDATE` or
   `DELETE` touching ledger rows, or any code path that mutates a claim's
   value in place fails review. The fix is always a new record naming
   `supersedes_id`. If your design "needs" an update, the design is what
   needs updating.
2. **One sequence per lineage.** Does anything new keep its own clock? A new
   store, table, or feature that stamps rows from its own counter or from
   wall-clock time has broken exact point-in-time reconstruction.
   Everything stamps from the lineage `seq`, one counting domain, so that a
   single number is a complete description of a moment.
3. **Pins bound everything.** Does every new read path accept and honor
   `as_of_seq`? The subtle half of this invariant is in the header's third
   bullet: the *superseded-set itself* is filtered by the pin, so a claim
   superseded after the pin still reads as current at it. A read path that
   resolves supersession at head and then filters by seq gets the wrong
   answer and will pass casual testing.
4. **Disputed, never dropped.** Does any path discard a claim that failed a
   gate? Does it instead turn the result into an error the caller must catch? A
   block-severity finding routes the claim to `disputed` status, recorded
   with its findings, sequence consumed. Governance outcomes are data.
5. **Digests rebuilt under a pin.** Does anything serve stored digest text
   on a pinned read? Stored digests reflect the head; under a pin they are
   deterministically rebuilt from the pinned facts. A cache "optimization"
   here is a correctness bug.

When a change and an invariant conflict, the invariant wins. If you believe
you found an exception, discuss it first in the proto spec and conformance
suite, not in an implementation PR.

### The plane-parity doctrine

The mesh exposes the same behavior over two wires. "The same" is a strong
claim. It means the same gate outcomes, supersession results, pin semantics,
and byte-comparable governance findings. The caller may speak JSON to axum or
protobuf to tonic. Most dual-protocol systems still drift because each
handler gains its own logic one bug fix at a time. This codebase uses a
structural solution, stated where it is enforced: the doc header of
[`src/munarium-server/src/service.rs:1`](../../src/munarium-server/src/service.rs)
at v0.1.2:

```rust
//! The shared command/query service layer. BOTH planes (REST handlers, tonic
//! impls) convert to these calls, so gate behavior, supersession, and pin
//! semantics cannot diverge between planes — the conformance suite asserts it.
//!
//! The command path IS the governance path: ProposeClaim/AppendEvents load
//! the snapshot, run the deterministic gates, and record block-flagged claims
//! as DISPUTED (never dropped).
```

You can watch the doctrine hold in thirty seconds. Both plane modules call
the same `service::` functions, and nothing else:

```powershell
git grep -n "service::" -- src/munarium-server/src/rest.rs src/munarium-server/src/grpc.rs
```

```text
src/munarium-server/src/grpc.rs:195:                let out = service::append_events(
src/munarium-server/src/grpc.rs:246:                let out = service::append_events(
src/munarium-server/src/grpc.rs:463:        let resp = service::get_claim(ctx.store.as_ref(), &inner.claim_id)
src/munarium-server/src/grpc.rs:654:        let d = service::compose_context(
src/munarium-server/src/rest.rs:229:        let out = service::append_events(
src/munarium-server/src/rest.rs:268:        let out = service::append_events(
src/munarium-server/src/rest.rs:397:        service::slice_facts(
src/munarium-server/src/rest.rs:430:    Ok(Json(service::get_claim(store.as_ref(), &claim_id).await?))
src/munarium-server/src/rest.rs:518:        service::compose_context(
```

Read one pair side by side. `rest.rs:229` and `grpc.rs:195` are the claim
write path. Both use the same function with the same storage handle, shape
registry, tenant, version, and claims. Each plane's job ends at translation.
It parses its wire format and enforces its transport contract. The contract
covers a uid header versus uid metadata and an idempotency key versus
idempotency metadata. The plane then converts to shared DTOs, calls
`service.rs`, and converts the result back. The governance decision happens
once below both planes. It covers the snapshot, gates, and disputed or
accepted status.

The doctrine has three practical consequences for your work:

- **To change behavior, change `service.rs` or below.** Both planes inherit
  it. You can test the change without booting either wire. The service layer
  takes `&dyn StorageBackend`, so the in-process
  conformance mode exercises it directly.
- **Logic appearing in a plane module is the review smell.** A conditional
  in `rest.rs` that affects *what* happens rather than *how it is said on
  this wire* is a parity bug waiting for its first divergent caller. The
  recipes in §6 (add-an-endpoint, add-an-RPC) both end at the same
  instruction: the handler calls the same `service.rs` function the other
  plane calls.
- **Parity is asserted, not assumed.** The conformance suite runs the
  *identical* scenario set over `--http` and `--grpc` against a live server
  (`test.ps1 -BlackBox` in §3's ladder, with more detail in §7). That is why the
  service-layer header can say "cannot diverge" without blushing: if it
  ever did, CI would say so before a user could.

Where parity is *incomplete*, some platform REST endpoints do not yet have gRPC
twins. The project states that gap clearly. Unshipped RPCs answer
`UNIMPLEMENTED`, and [api/grpc.md](../api/grpc.md) maintains the ledger of
gaps. That is the honesty rule (§12) applied to the doctrine itself.

![Plane parity: mmp-conformance runs one scenario set over both wires; rest.rs and grpc.rs each translate their transport and funnel into service.rs, which calls munarium-core](images/ch1-plane-parity.svg)

### The kernel boundary, machine-enforced

One more doctrine belongs in this orientation because it explains the crate
design before §4 tours it. The kernel stays *pure*. `munarium-core` owns the
gates, ledger, composer, pins, promises, counters, and other semantics. It
cannot depend on the web framework, database driver, or any HTTP client. A CI
step enforces this rule by walking the dependency tree and failing the build
([`.github/workflows/server-ci.yml:118`](../../../.github/workflows/server-ci.yml),
the `crate boundary check`). `munarium-core` must never depend on sqlx, axum,
tonic, reqwest, or utoipa.

You can watch the boundary hold in a few seconds. `cargo tree` is read-only
and fast. CI walks the full dependency tree, so a banned crate cannot hide
behind another dependency. `--depth 1` shows the direct dependencies that
keep it out:

```powershell
cargo tree -p munarium-core -e normal --depth 1
```

```text
munarium-core v0.1.2 (…\munarium\server\src\munarium-core)
├── async-trait v0.1.92 (proc-macro)
├── chrono v0.4.45
├── hex v0.4.3
├── regex v1.13.1
├── serde v1.0.229
├── serde_json v1.0.151
├── sha2 v0.10.9
├── thiserror v2.0.20
└── uuid v1.24.0
```

There are nine direct dependencies, and none is a framework. This design
supports the testing model. The kernel sees the outside world only through
its trait seams. These include the `storage`, `retrieval`, `provider`,
`sources`, and `docintel` modules among the fifteen declared by `lib.rs`.
The same semantics can run in-process against `munarium-store-mem` in a unit
test or against Postgres in production. This design also made them portable
from Python.

If you want to use `reqwest` inside a kernel module, the boundary tells you
that the code belongs in an adapter crate behind a trait. Section 4 has the
boundary table, and since 2026-08-17 every rule in it is machine-checked:
the boundary step greps `munarium-core` and `munarium-access` for the banned
frameworks and asserts `munarium-providers` carries no storage crate (the
review-only rows were §13's entry 1, now closed).

### The map of everything

Learn the top-level `server/` tree so its directory names stop being noise.
`src/` holds the twenty crates, and `proto/mmp/v1/` holds the spec.
`conformance/` holds the harness, which is a workspace member rather than an
afterthought. `docs/` is where you are now. `deploy/` holds the Helm chart, the
example AKS Terraform module, and the Envoy gateway configs. `runbooks/`
holds the committed shapes and example runbook applications used in Part II.

The root also has four PowerShell scripts: `build.ps1`, `test.ps1`,
`localdeploy.ps1`, and `gates.ps1`. Sections 2, 3 and 10 show how to use
them; Appendix C is the inventory.

Documentation has a map of its own and one rule. The map is
[docs/README.md](../README.md), which indexes everything under `server/docs/`
in four groups: design and record, API reference, guides, and operations. The
rule appears in its first paragraph: **"If you add a document, add it
here. An unlisted doc is an unread doc."** The rule exists because this
tree already tried the alternative. Documents found only through `ls` and
folklore might as well not exist. Treat the index row as
part of the definition of done for any new doc.

Two entries in that index are **generated**:
[api/openapi.json](../api/openapi.json) (from
`cargo run -p munarium-server -- openapi`) and
[api/grpc-reference.md](../api/grpc-reference.md) (from `gen-grpc-docs`). They
are never hand-edited. CI checks both against your checkout for drift, so a
hand edit does not survive the first pipeline run.

For a new platform developer, the following reading order works. Each
document answers a different kind of question. Reading them out of order can
lead you to ask the wrong one:

1. **This guide, §1–§3** covers vocabulary, doctrine, and the loop. You are
   nearly done with the first of those.
2. **[architecture.md](../architecture.md)** contains the normative GA design:
   the four scaling layers, the crate plan, the deployment profiles, the
   design tenets ("rebuild, don't migrate"; "no proprietary service in the
   path"). Read it before making any *structural* decision. It is the
   document that tells you where a new thing belongs.
3. **[security-posture.md](../security-posture.md)** explains the trust model
   and answers the question every newcomer asks in week one: *why is there no
   OIDC in the server?* The omission is deliberate. munarium-server is not an
   identity provider. The enterprise API-management layer in front of it
   authenticates humans. The mesh governs what an authenticated caller may
   touch, including tenancy, capability tokens, access-filtered retrieval,
   and the uid-attributed audit. Read it before wiring the server into any
   environment that serves more than one human,
   and before proposing an auth feature that the posture has already
   ruled out of scope.

Beside those three sit the working references you will keep open in tabs
rather than read linearly: [api/rest.md](../api/rest.md),
[api/grpc.md](../api/grpc.md), and the errors registry
[api/errors.md](../api/errors.md). Deployment documentation deliberately
lives with the deployment code (`deploy/helm/munarium/README.md`,
`deploy/terraform/example-aks/README.md`), not under `docs/`; the index
links across.

### What "done" means here

Orientation ends with the standard your changes must meet. It is stricter
than the standard in most codebases, so you should know it before review. A
change to `server/` is done when *all* of the following are true in the same
change, not the same week:

- **The CI gates pass locally.** `build.ps1 -Lint` runs the exact CI pair:
  `fmt --check` and clippy with warnings denied. Section 3 shows the
  full local mirror (`gates.ps1` runs the identical gate list CI runs,
  including the drift checks and the boundary grep; §10 walks it).
- **Tests exist at the right tier.** The five-tier ladder (§3, §7 in
  depth): bare `cargo test --workspace` for logic, `-Postgres` when
  storage is involved, and `-BlackBox` when either wire's behavior changed.
  That tier *is* the parity check. Use `-Platform` for the platform surface.
  A behavior change without a test at the tier that can see it is not
  done.
- **Conformance still proves the invariants.** If your change touches
  kernel semantics, the scenario set (§7) either still passes or has been
  extended. Recipe 9 in §6 shows how a new scenario automatically runs
  against every backend and both wires.
- **The documentation moved with the code.** A new env var lands in the
  README table in the same change. A new endpoint regenerates
  `openapi.json` and updates the route map. A new error slug is
  registered in [api/errors.md](../api/errors.md), and a new doc gets its
  index row. A change that invalidates a claim in *this guide* also updates
  this guide. The Preface's third commitment is a workspace rule, not a
  request. CI enforces the generated-file half of this, while reviewers
  enforce the rest.
- **Gaps are stated, not implied.** If you shipped part of a surface, the
  unshipped part answers `UNIMPLEMENTED`, and the gap is written down
  in [api/grpc.md](../api/grpc.md)'s parity ledger, in §13's gaps ledger,
  or as an inline **[gap]** here. "Never fake" is the house style (§12),
  and it applies to documentation exactly as it applies to endpoints.

If that list sounds heavy, notice what it buys: the guide you are reading
can promise that every command was executed and every claim is checkable.
That is possible because the tree holds every change to the same standard.
You inherit that trust, along with the duty to maintain it.

Everything above was vocabulary and doctrine. None of it required a working
toolchain. Section 2 covers stable Rust on native Windows and explains why
you must never link musl locally. It also covers Docker for Postgres and the
port setup that makes each local example bind to 18080 instead of 8080. A
five-command sequence takes you from a fresh clone to a passing test suite, a
green conformance run, and a live `/healthz`. Have Docker Desktop running
before you turn the page.

## 2. Getting set up (Windows-first, by design)

§1 closed by telling you to have Docker Desktop running. This chapter puts
that instruction to use. By its end, your machine will have compiled the whole
workspace. You will have watched the conformance suite prove the five
invariants in process, started the compose Postgres, and booted a server you
built from source. You will also have answered its `/healthz` with your own
curl.

Everything below was run against v0.1.2 on a Windows 11 dev box, in the order
shown. The outputs are real. Read two sections *before* you need them: the port
landscape and the gotcha almanac at the end. Every entry in both cost somebody
an afternoon.

### Windows-first is a decision, not an accident

Most Rust server projects treat Windows development as something between
tolerated and theoretical. The CI is Linux, the Dockerfile is the real build,
and the Windows instructions were last tested by whoever wrote them. This
tree inverts that pattern. The platform is **developed natively on Windows**.
The edit-build-test loop is plain `cargo` against the MSVC toolchain. The Linux
artifact exists in exactly one place: the musl cross-compile inside the Docker
builder stage. The README states the rule when you might first be tempted to
break it (README:57):

> From source on Windows (native; never link musl locally; the Linux binary
> is built in Docker/CI)

"Never link musl locally" is not superstition. Linking
`x86_64-unknown-linux-musl` from a Windows host means assembling a cross C
toolchain and convincing every `-sys` crate in the dependency graph to use
it. That is an afternoon of tedious work to produce a binary you cannot run
anyway. The Docker builder already does it in a repeatable way. CI does it on
every push, and the development loop does not need the Linux binary. When you
want the image, `build.ps1 -Image` (§3) asks Docker to make one.

What makes the native loop *painless*, rather than merely permitted, is a
dependency policy decision you will meet in full in §9: **rustls everywhere,
OpenSSL banned** (`deny.toml` enforces the ban; the README's boundary-rules
paragraph states it). There is no C cryptography library to build, which
means no vcpkg, no perl, no `OPENSSL_DIR`, and no special environment on
Windows or in the musl link. The whole workspace is pure Rust down to the TLS
stack. That choice is why `cargo test --workspace` on a fresh Windows clone
just works.

It also gives you an early review rule. A new dependency that brings in a C
library or OpenSSL breaks two builds at once: your native loop and the static
Linux link. Section 9 explains the full argument.

### The toolchain

The workspace pins its toolchain declaratively. The whole file
(`rust-toolchain.toml`):

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
targets = ["x86_64-unknown-linux-musl"]
```

Three settings reflect three decisions. `channel = "stable"` means there are
no nightly features anywhere in the tree. Rustup reads this file and selects
or installs the right toolchain the first time you run a `cargo` command in
the workspace. Therefore, "install the toolchain" is not a separate setup
step.

The `components` pair defines the CI lint gate. `build.ps1 -Lint` runs
`fmt --check` plus clippy with warnings denied (§3). Pinning the components
here means a fresh machine has them before the first lint.

The musl entry in `targets` has a different purpose. It tells rustup to keep
the *target's standard library* installed. It also makes every environment
agree on which Linux triple the product ships. However, **only the Docker
builder links it**.

The verified versions on the box this chapter was executed on:

```powershell
cargo --version
docker --version
py --version
```

```text
cargo 1.97.1 (c980f4866 2026-06-30)
Docker version 29.5.2, build 79eb04c
Python 3.13.12
```

Anything reasonably current works. The README's stated floor is Rust ≥
1.89. The `stable` toolchain will always be newer than that.
The full kit for a platform developer:

- **Rust via rustup.** If the machine has nothing:
  `winget install Rustlang.Rustup`. `test.ps1` shows that exact command when
  it cannot find cargo. Note how the script works (`test.ps1:25-27`). It
  resolves `cargo` from `PATH` but falls back to
  `$env:USERPROFILE\.cargo\bin\cargo.exe`. This lets the test ladder work even
  in a shell whose profile never loaded the cargo environment. The scripts
  are written for the machine you have, not the one the docs imagine.
- **Docker Desktop.** Postgres, MinIO, the Envoy gateway profile, and the
  image build all live in compose. Have it running before the ladder below.
  The compose CLI errors are vague when the daemon is down.
- **The `py` launcher.** The drift checks are scripts that verify the
  generated OpenAPI and gRPC reference match your checkout (§3, §10). They shell
  out to `py`. Any Python 3 via python.org or winget provides it; the
  version above is just what this box had.
- **Optional, in honesty order:** `cargo-deny` is the license and ban
  auditor. `gates.ps1` warns if it is absent, and CI still enforces it
  on push/PR, so you can defer it. Use `grpcurl` to check the direct gRPC
  plane by hand. Reflection is enabled, so it works without proto files.
  `terraform` matters only if you edit the example AKS module under
  `deploy/terraform/`; CI's `terraform` job runs `fmt` and `validate` on
  it (§10). No part of the dev loop needs it.

That is the whole list. No database install (compose owns Postgres), no
protoc (vendored by `munarium-proto`'s build script, §4), no Node, no OpenSSL.

### The port landscape

Read this section before your first mystery failure. The cause is usually a
port. The Introduction's standards stated the two-world rule. Here is the
whole map, followed by the reasons.

![The port landscape: canonical ports 443, 8080, 50051 and 9090; from-source alternates 18080, 15051, 19090 and 18443; test tiers on 18080/15051/19090 for black-box and 18081/19091 for the platform surface, with Postgres on host 5433 and MinIO on 9000/9001. A script reaps only a stale munarium-server on the alternate ports and refuses to touch anything else](images/ch2-port-map.svg)

The **canonical world** is what the server considers its own defaults and
what compose publishes: REST on 8080, direct gRPC on 50051, ops on 9090,
and a gateway on 443 in deployed environments. Every compose example in
this guide and every deployed environment uses these.

The **alternate world** exists because canonical ports often fail on a real
dev machine. The repo records that lesson so people do not have to learn it
again. `localdeploy.ps1` explains why in its header
(localdeploy.ps1:12-13):

> Default host ports avoid this machine's conflicts (8080 in use, 8443
> Windows-reserved). Override with -HttpPort/-GrpcPort/-OpsPort/-GatewayPort.

Two distinct failure modes hide in that sentence. **8080 is routinely
taken** because many local daemons use it by default. That failure is at
least clear: *address already in use*.

**8443, the compose gateway profile's default host port, is
Windows-reserved** on many machines. Hyper-V/WinNAT blocks whole port ranges
from user binding, and 8443 often falls inside one. The result is a confusing
*access denied* error on a port with no listener. Run `netsh interface ipv4
show excludedportrange protocol=tcp` to see your machine's ranges.

Everything booted from source or by the local scripts therefore uses the
**+10000 alternates**: 18080 (REST), 15051 (gRPC), 19090 (ops), and 18443
(gateway). The test tiers follow the same convention. Black-box conformance
uses 18080/15051/19090. The platform tier runs beside it on 18081/19091,
and the cluster tier's two instances take 18082/19092 and 18083/19093
(test.ps1's header is the port ledger), so every tier can coexist.

Backing services get the same treatment. Compose publishes Postgres on host
**5433** and maps it to the container's 5432. This prevents a collision with
a native Postgres install on 5432. That is why every connection string in
the scripts reads `localhost:5433`. MinIO takes 9000 for its S3 API and 9001
for its console when you enable `--profile s3`.

The server, gateway, and MinIO host ports are all variables in
`docker-compose.yml`, such as `${MUNARIUM_HOST_HTTP:-8080}`. A machine with
different conflicts can remap them with environment variables instead of
editing the file. `localdeploy.ps1` sets those variables from its parameters.
Postgres's `5433:5432` is the one hardcoded mapping. It exists to avoid the
conflict, so it never needed a setting.

One port policy deserves its own paragraph because it explains the project's
view of automation. The alternate ports belong to the repo's scripts, and
`gates.ps1` enforces that claim (gates.ps1:88-107). Before a black-box
run, the script checks ports 18080/15051/19090. It handles a listener by
*identity*, not by port.

If the owning process is named `munarium-server`, it is **by definition a stale
test instance**. A gate, the clients-conformance recipe, or an interrupted
run left it behind. The script removes it instead of making a person do so.
If the process is anything else, the script throws this message with your
machine's own pid and process name:

```text
port 18080 is already in use by pid 4242 (some-other-app) — not an
munarium-server, so not stopping it for you
```

Automation may clean up after itself; it may never clean up after *you*.
When you write a script that needs a port, copy this pattern, not a bare
`Stop-Process`.

All of this is why the Introduction told you to suspect the port before
the code. This one-liner answers "who owns it" and is the same command
`gates.ps1` uses:

```powershell
Get-NetTCPConnection -State Listen -LocalPort 18080 |
  ForEach-Object { Get-Process -Id $_.OwningProcess }
```

Appendix B carries the full table as a quick reference, including the
handful of ports this chapter had no reason to mention.

### First success, five commands

Now follow the ladder from a fresh clone to a proven working machine. It has
five commands, and each one checks a different layer: the compiler, kernel
semantics, container plumbing, storage integration, and a live server on the
wire. Every output below is real.

**1. The workspace test suite.** All offline: no network, no database, no
keys. The first run does a full compile and takes several minutes. After
that, the suite itself takes seconds.

```powershell
cd server
cargo test --workspace 2>&1 | Select-String 'test result'
```

```text
test result: ok. 44 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
…
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

This tree has sixty `test result` lines, one per test harness (unit and
integration binaries, plus one per crate's doc-test harness, even an empty
one). They total **720 passed, 0 failed**. Two details will help you read that output.
The many `0 passed` lines are normal. Every crate gets a doc-test harness,
even if it has no doc-tests. Binary targets get empty harnesses too.

The suite passing without a database is also expected. Tests that need
external state, such as Postgres, MinIO, or provider keys, skip when their
gate environment variable is unset. Section 3 names this practice, and §7
examines it. A red bare run is *always* a real failure.

**2. The conformance suite, in process.** This is the five invariants (§1)
as an executable checklist, run against the in-memory backend with no
server and no wire:

```powershell
cargo run -p mmp-conformance -- --in-process
```

```text
MMP conformance — in-process (munarium-store-mem)
--------------------------------------------------------
  PASS  ledger.append-head-conflict
  PASS  ledger.supersession-pin
  PASS  pins.one-pin-bounds-all-stores
  PASS  gates.block-records-disputed
  PASS  composer.budget-degradation
  PASS  digests.rebuilt-under-pin
  PASS  gates.chronology-certain-only
  PASS ledger.origin-round-trips
--------------------------------------------------------
8 passed, 0 failed
```

Compare the scenario names with §1's checklist. The mapping is one-to-one:
append-only with head conflicts, supersession resolved at the pin, one pin
bounding every store, disputed-never-dropped, budget-aware composition,
digests rebuilt under a pin, and the chronology gate firing only on certain
violations. Later, the same scenario set will run against Postgres and over
both wire planes (§7). The assertions stay the same while the base changes.
That is the key idea.

**3. Postgres, from compose.** The first command that needs Docker Desktop:

```powershell
docker compose up -d postgres
docker exec server-postgres-1 pg_isready -U munarium
```

```text
 Container server-postgres-1  Running
/var/run/postgresql:5432 - accepting connections
```

That is the `pgvector/pgvector:pg16` image, published on host port 5433 as
described above. Compose polls its `pg_isready` healthcheck for you.
`test.ps1` waits on that same healthcheck instead of sleeping and hoping.
Nothing has connected to it yet. It is simply *available*.

**4. The Postgres-backed tier.** With the container healthy, the second
rung of the test ladder is one flag:

```powershell
.\test.ps1 -Postgres
```

This chapter stops at the command on purpose. Section 3 covers the five-tier
ladder. It explains what each tier adds, how `MUNARIUM_TEST_DATABASE_URL` is set
only around the test run, what `-BlackBox` and `-Platform` boot, and where
their server logs land. The transcript belongs there. Run the command now if
you like. It starts the compose Postgres if you skipped step 3, and it is
idempotent.

**5. A server you built, answering on the wire.** Last rung: boot
`munarium-server` from source and probe it. Per the landscape, a from-source
boot uses the alternate ports. For a first boot, use the smallest possible
configuration: in-memory storage with full governance semantics, no database,
no persistence, gRPC off, and static dev auth.

```powershell
$env:MUNARIUM_HTTP_ADDR = '127.0.0.1:18080'
$env:MUNARIUM_GRPC_ADDR = 'disabled'            # the literal 'disabled' turns the listener off
$env:MUNARIUM_OPS_ADDR  = '127.0.0.1:19090'
$env:MUNARIUM_STORE = 'memory'
$env:MUNARIUM_AUTH_MODE = 'static'
$env:MUNARIUM_STATIC_TOKENS = 'devtoken:dev-tenant:rw'
cargo run -p munarium-server
```

From a second terminal, the two probes that matter:

```powershell
curl.exe http://127.0.0.1:18080/healthz
curl.exe http://127.0.0.1:18080/version
```

```text
{"ok":true}
{"name":"munarium-server","planes":{"grpc_direct":"50051","grpc_gateway":"443 via gateway","rest":"8080"},"version":"0.1.2"}
```

The ops listener answers the same probe on its own plane.
`Invoke-RestMethod http://127.0.0.1:19090/healthz` returns `ok`. This matters
because deployed environments expose *only* the ops plane to health probes.

Read the `/version` body carefully. The `planes` map states the **canonical
contract ports**, not the addresses you happened to bind. It documents the
protocol surface instead of echoing your environment. This difference will
save you a confused minute when you first see `"rest":"8080"` from a server
listening on 18080.

At this point you have a governed-memory server made from your own
checkout. The Introduction's six-step first-contact walkthrough works
against it word for word if you substitute `127.0.0.1:18080` for
`localhost:8080`. The `memory` store runs the same gates and ledger, but it
forgets on exit.

When you later want a *persistent* from-source server, set
`MUNARIUM_STORE=postgres` with the compose URL. Also set
`MUNARIUM_SOURCE_STORE=pg` explicitly. Under the postgres store, the source
store defaults to `az` and fails closed without a storage account. That
default is correct for deployments but wrong for a laptop. The black-box
gate in `gates.ps1` sets it for this reason (gates.ps1:110-114).

Then clean up after yourself as the scripts would. Press `Ctrl-C` to stop the
server. If you started it detached, stop it by identity. Check that the
process name is `munarium-server` before you run `Stop-Process`, as required by
the reaping rule. Then clear the environment:

```powershell
'MUNARIUM_HTTP_ADDR','MUNARIUM_GRPC_ADDR','MUNARIUM_OPS_ADDR',
'MUNARIUM_STORE','MUNARIUM_AUTH_MODE','MUNARIUM_STATIC_TOKENS' |
  ForEach-Object { Remove-Item "Env:$_" -ErrorAction SilentlyContinue }
```

The cleanup is not just politeness. `MUNARIUM_*` variables left in a long-lived
shell often cause problems later. A forgotten `MUNARIUM_STORE=memory` can make
a "Postgres-backed" experiment pass against the wrong backend. The test
scripts limit their variables to each run for this reason. Your interactive
shell should do the same.

Five commands, five layers proven: the toolchain compiles the workspace,
the kernel honors the invariants, Docker and Postgres are plumbed, the
storage tier is one flag away, and the server binary binds, serves, and
identifies itself.

### The gotcha almanac

Four entries follow one shape: symptom, cause, and fix. They live in this
chapter because each one appears during *setup and first use*, not deep in
feature work.

**PowerShell splits `-flag=value` arguments at dots.** Symptom: terraform
fails with `Too many command line arguments. Did you mean to use -chdir?`
on a command that looks perfectly well-formed. Mechanism: PowerShell's
argument-mode tokenizer splits an unquoted bareword at a leading `..` or an
interior `.` segment. As a result, `-var-file=../my.tfvars`
reaches terraform as **two** arguments: `-var-file=` and
`../my.tfvars`. The stray halves become positional arguments. The fix:
**quote the whole argument**.

```powershell
terraform "-chdir=deploy/terraform/example-aks" plan `
    "-var-file=$tfvars" '-out=../example.tfplan'
```

The rule applies to every native executable. Always quote a `-flag=value`
argument when its value contains dots or path separators; `-out=` and
`-var-file=` are the two terraform flags that bite first. Quoting is the
lasting fix, and it keeps you in the shell used by the other scripts.

A related detail: if you run the az CLI from Git Bash, MSYS path conversion
changes `/subscriptions/...` arguments into
`C:/Program Files/Git/subscriptions/...`. Set `MSYS_NO_PATHCONV=1` to fix
it. You will not encounter this problem if you stay in PowerShell, which is
where every script in this tree keeps you.

**A new migration file that does not run (the sqlx stale-embed).** Symptom:
you added `src/munarium-store-pg/migrations/00NN_*.sql`, the server starts
cleanly, and your new table does not exist. Mechanism: `sqlx::migrate!`
embeds the migration set **at compile time** of `munarium-store-pg`. Adding a
file does not mark the crate as changed, so cargo reuses the old build with
the old embedded set. Fix: force the crate to recompile with
`cargo clean -p munarium-store-pg` or touch its `lib.rs`, and then rebuild. The
full migration discipline, including this gotcha's place in it, is recipe
§6.3.

**You edited a shipped migration, and now nothing becomes ready.** Symptom:
the server refuses to start against an existing database with `migration N
was previously applied but has been modified`. Migrations run on connect,
so the new process never becomes healthy. In a deployment, the new pods
never pass readiness and the old ones keep serving (§10).

Mechanism: sqlx records a checksum for each applied migration. Editing an
applied file in place breaks the checksum on every database that already
ran it. Recovery for your local compose database is `docker compose down
-v`, which drops the `pgdata` volume; for any other database the recovery
is the same in kind — drop and recreate it, or restore it from before the
edit — because there is no way to make a database un-see a file it applied.
The answer that avoids the recovery is permanent: add a migration and
never edit one. Section 6.3
explains this rule. Its worked example (`0015`) is a comment-only migration
created to demonstrate the never-edit rule.

**Piped builds that die mid-compile (SIGPIPE).** Symptom: a long
`cargo build ... | head` in a bash shell stops partway with no error, or a
background build simply vanishes.

Mechanism: when `head` exits after ten lines, the pipe closes. The build's
next write gets SIGPIPE, which kills it without a message in the middle of
compilation. This one comes from the plan's dated log too. It was learned
while tailing deploy builds.

Fix: in bash, redirect to a file and inspect the file. PowerShell pipelines
do not have this failure mode because the downstream cmdlet drains its input.
That is why this chapter could safely run
`cargo test --workspace 2>&1 | Select-String 'test result'` and why the
repo's own scripts pipe freely: they are PowerShell.

### Where you stand

Your machine now compiles the workspace, proves the invariants in process,
runs the compose Postgres, and boots a server that answers with its own
version. What you do not yet have is a *rhythm*. You need to know which
command to run after each kind of change and what each tier of `test.ps1`
buys and costs. You also need to know how to run the full stack with
`localdeploy.ps1` and how to mirror CI's exact gates before pushing.
Section 3 describes that development loop.

## 3. The development loop

§2 left you with a working machine and no rhythm. This chapter gives you
that rhythm. It covers the two scripts you will run hundreds of times:
`build.ps1` and `test.ps1`. It also explains the five-tier test ladder and
what each rung buys and costs. You will learn the pattern used by every
externally gated test, the compose profiles that start services on demand,
and which rung to use after each kind of change.

Everything below was run against v0.1.2 while this chapter was written, in
the order shown. One run caught a real bug in the ladder itself. The bug is
now fixed, and you will see the incident at tier 4.

### The philosophy: cheapest signal first

The loop is a ladder, and the organizing rule is the one that orders any
scoring pipeline: **spend the cheap signal before the expensive one.** A type error should be caught by the
compiler, not by a black-box conformance run that took a minute to boot a
server. An in-process scenario should catch a gate-semantics regression in
milliseconds. Do not wait for CI to catch it twenty minutes after you push.

The tiers are ordered by required infrastructure. Tier 1 needs nothing.
Tier 2 needs the compose Postgres. Tier 3 boots a real server on the wire,
and tier 4 does both at once. Each tier exists because it can find a class
of bug that cheaper tiers cannot see. While you work, climb only as high as
your change requires. Before you push, climb the whole ladder once. CI runs
the same rungs (§10), and it is faster to find the problem on your machine.

Learn two properties of the ladder before the details. First, **it is
cumulative by construction**. Every call to `test.ps1` runs tier 1 before
any flagged tier. Tier 1 includes `cargo test --workspace` and in-process
conformance. Therefore, `-BlackBox` alone still proves the offline world
first because test.ps1:46-58 always runs. The flags add gates rather than
replace them. `-All` simply sets all three (test.ps1:23).

Second, **it fails at the first failing tier** with a non-zero exit
(test.ps1:12). This makes it safe to chain in scripts. A green final line,
`all requested test tiers OK`, certifies everything you asked for, not just
the last item printed.

### `build.ps1`: compile and the CI lint pair

The build script is deliberately thin. It has four switches over `cargo`, plus
the image build:

```powershell
.\build.ps1              # debug build of every crate
.\build.ps1 -Release     # optimized build
.\build.ps1 -Lint        # fmt --check + clippy -D warnings (what CI enforces)
.\build.ps1 -Image       # docker build of the distroless musl image
```

Make a habit of using `-Lint`. It runs **exactly the CI pair**:
`cargo fmt --all --check`, then
`cargo clippy --workspace --all-targets -- -D warnings`
(build.ps1:26-33). It then runs the workspace build. Executed for this
chapter:

```powershell
.\build.ps1 -Lint
```

```text
== cargo fmt --check
== cargo clippy -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.78s
== cargo build --workspace
   Compiling mmp-conformance v0.1.2 (…\munarium\server\conformance)
   Compiling munarium-proto v0.1.2 (…\munarium\server\src\munarium-proto)
   Compiling munarium-server v0.1.2 (…\munarium\server\src\munarium-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.60s
build OK
```

A clean lint is *silent*. No news is the pass. Warnings are denied, so a
lint failure is loud and names the line. The pair is byte-for-byte the same
as CI's pair. Therefore, a clean local `-Lint` means the lint gate cannot be
what CI rejects.

Run it before every push. Use plain `.\build.ps1`, or just `cargo build`,
during the edit loop. The lint pair costs a full clippy pass, which is too
much between every edit. `-Image` asks Docker for the musl → distroless
production image. This is the only place the Linux target is linked, as §2
explains. `-ImageTag` names the image. The daily loop needs neither switch.

### `test.ps1`: the five-tier ladder

The whole contract is in the script's header, and it is worth reproducing
as the table you will keep in your head (four tiers through v0.1.2; the
`-Cluster` tier joined 2026-08-17 with the N-replica work):

| Tier | Flag | Needs | What it adds | Reach for it when |
|---|---|---|---|---|
| 1 | *(none)* | nothing; offline | `cargo test --workspace` + in-process conformance (munarium-store-mem) | constantly; after any change |
| 2 | `-Postgres` | Docker (compose pg on :5433) | un-skips the pg-gated tests; conformance against `munarium-store-pg` | storage, SQL, migrations, retrieval-pg |
| 3 | `-BlackBox` | a built server (no Docker) | live server on 18080/15051/19090; conformance over **both planes** for the parity check | wire behavior: handlers, DTOs, auth, transport contracts |
| 4 | `-Platform` | Docker (compose pg) | pg-backed live server on 18081/19091; the five platform scenarios | uid contract, capability tokens, runbook v2, sessions, ingest, removal, reports |
| 5 | `-Cluster` | Docker (compose pg) | TWO pg-backed servers on 18082/18083 sharing one database + tenant; the five N-replica scenarios | registry convergence, cross-instance idempotency, seq interleaving, the run advisory lock ([ops/clustering.md](../ops/clustering.md)) |
| All | `-All` | all of the above | tiers 1–5 in order | before every push |

![The five-tier test ladder: offline unit tests plus in-process conformance by default, then -Postgres adds real storage, -BlackBox adds both wire planes, -Platform adds the pg-backed platform surface, and -Cluster runs two servers over one database. Gated satellites keyed by environment variables skip loudly rather than passing silently](images/ch3-test-ladder.svg)

Warm (after the first compile), each tier is seconds to about a minute;
`-All` is a few minutes end to end. The tiers in turn, each with the real
output from this chapter's runs.

**Tier 1: bare.** `.\test.ps1` with no flags runs the workspace suite
and then the in-process conformance mode. No Docker, no network, no keys
are needed. This is the rung you live on:

```text
== cargo test --workspace
     Running unittests src\lib.rs (...\munarium_core-....exe)
running 44 tests
test gates::tests::ledger_gate_blocks_plain_conflict_exempts_supersession ... ok
test ledger::tests::supersession_respects_pin ... ok
…
test result: ok. 44 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
…
     Running tests\pg_integration.rs (...\pg_integration-....exe)
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
…
== conformance: in-process (munarium-store-mem)
MMP conformance — in-process (munarium-store-mem)
--------------------------------------------------------
  PASS  ledger.append-head-conflict
  … (all eight)
--------------------------------------------------------
8 passed, 0 failed

all requested test tiers OK
```

The full run is the sixty harnesses summing to **720 passed, 0 failed**
that §2 showed, plus the eight invariant scenarios §2 read against §1's
checklist. Note the
`pg_integration` line finishing in `0.00s` with no database anywhere:
that is a *vacuous pass*. The test returned early because its gate
environment variable is unset. A section below explains this pattern so
you can read `ok` correctly on this rung.

**Tier 2: `-Postgres`, real storage.** The flag does three things.
First, `Ensure-Postgres` starts the compose Postgres if needed and waits
on the container's own `pg_isready` healthcheck rather than sleeping and
hoping (test.ps1:31-44; 90-second deadline, last-10-lines log dump on
failure).

Second, it sets `MUNARIUM_TEST_DATABASE_URL` to the compose URL *only around*
the workspace test run. Copy this detail into your own scripts. A
`try/finally` removes the variable even on failure (test.ps1:47-54). The
gated tests run once, and the variable does not remain in next week's shell.
This enforces §2's cleanup advice. Third, it runs the same conformance
scenarios in `--postgres` mode against `munarium-store-pg`
(test.ps1:61-65). Executed:

```text
== ensuring compose postgres is up
== cargo test --workspace
…
test result: ok. 12 passed; 0 failed; … finished in 0.53s   (collections_integration)
…
test result: ok. 2 passed; 0 failed; … finished in 0.34s    (pg_integration)
…
== conformance: postgres (munarium-store-pg)
MMP conformance — postgres (munarium-store-pg)
--------------------------------------------------------
  PASS  ledger.append-head-conflict
  …
  PASS  gates.chronology-certain-only
  PASS ledger.origin-round-trips
--------------------------------------------------------
8 passed, 0 failed
all requested test tiers OK
```

Two signs show that the tier did real work. The gated harnesses that
finished in `0.00s` on the bare rung now take real time. `pg_integration`
went from 0.00s → 0.34s because
`concurrent_appends_serialize_on_lineage_head` races four writers against
`lineage_heads` FOR UPDATE. `collections_integration` went from 0.00s →
0.53s while building real indexes.

The conformance report's label also changed. The same eight scenarios ran
on a different base, which is the main point. The pg conformance mode
creates a **fresh tenant per run** (conformance/src/main.rs:4). Reruns never
collide with data from the last one, so there is nothing to clean or reset.

Use this rung for changes to `munarium-store-pg`, `munarium-retrieval-pg`, or a
migration. If a new migration seems not to run here, see §2's sqlx
stale-embed warning. Recipe §6.3 gives the fix and the full practice.

**Tier 3: `-BlackBox`, both wires and the parity check.** This rung boots
what you have been testing *around*: a real `munarium-server` process. The
script builds the binary, then starts it on the alternate ports with the
memory store and a static token. The identity is random for each run:

```powershell
# test.ps1:73-82 (abridged)
$token = "bbtoken"
$tenant = "bb-$(Get-Random)"
$serverEnv = @{
    MUNARIUM_HTTP_ADDR     = '127.0.0.1:18080'
    MUNARIUM_GRPC_ADDR     = '127.0.0.1:15051'
    MUNARIUM_OPS_ADDR      = '127.0.0.1:19090'
    MUNARIUM_STORE         = 'memory'
    MUNARIUM_AUTH_MODE     = 'static'
    MUNARIUM_STATIC_TOKENS = "${token}:${tenant}:rw"
}
```

It then polls `/healthz` on 18080 for up to 30 seconds. Next, it runs the
conformance suite with **both** `--http` and `--grpc`. The harness's own doc
header defines this as the parity check: "Passing BOTH
--http and --grpc is the cross-plane parity check: the same scenario set
must go green on each plane" (conformance/src/main.rs:14-15). Executed:

```text
== black-box: starting server (memory store, 18080/15051)
2026-08-12T01:43:34Z  INFO munarium_server: starting config.store=Memory http=127.0.0.1:18080 grpc=Some("127.0.0.1:15051")
2026-08-12T01:43:34Z  INFO munarium_server: REST plane listening addr=127.0.0.1:18080
2026-08-12T01:43:34Z  INFO munarium_server: direct gRPC plane listening addr=127.0.0.1:15051
== conformance: REST + gRPC planes (parity)
MMP conformance — REST plane (http://127.0.0.1:18080)
  PASS  ledger.append-head-conflict
  … (all eight)
8 passed, 0 failed

MMP conformance — gRPC plane (http://127.0.0.1:15051)
  PASS  ledger.append-head-conflict
  … (all eight)
8 passed, 0 failed
all requested test tiers OK
```

The same eight scenarios run *twice*. One run goes through axum and JSON.
The other goes through tonic and protobuf against the same live process.
This turns §1's parity rule into an executable check.

Run this rung whenever you touch `rest.rs`, `grpc.rs`, the DTOs, or
anything related to auth. Tier 1 tests the service layer beneath both
planes. Only this tier can catch a translation bug in one plane's handler.
The server stops and the environment variables are removed in a `finally`
block (test.ps1:101-104), whether the run passes or fails.

If an interrupted run leaks the process, §2's reaping rule applies. The
scripts identify a stale `munarium-server` on these ports and clean it up on
the next run. This tier does *not* need Docker. The memory store gives it
full governance semantics with no backing services.

**Tier 4: `-Platform`, the platform surface.** The last rung combines the
previous two. It starts a *pg-backed* live server on the second port pair,
18081/19091, so it can coexist with a black-box run. gRPC is disabled. Two
static tokens divide the rw and mgmt roles. `MUNARIUM_TOKEN_SECRET` is set so
the server can mint capability tokens. `MUNARIUM_SOURCE_STORE=pg` is pinned so
document bytes stay in Postgres (test.ps1:116-130).

The tenant line applies the same practice used by the pg conformance mode
at the tier level (test.ps1:114-115):

```powershell
# Fresh tenant per run so scenarios never collide with earlier data.
$entTenant = "ent-$(Get-Random)"
```

Postgres persists between runs, so the *namespace* gets refreshed. No
scenario depends on or trips over data from a previous run. Copy this
pattern whenever you write a test against shared state.

The `MUNARIUM_SOURCE_STORE` pin has a story, and it is this guide's
first commitment doing its job. Executing `.\test.ps1 -All` for this
chapter's draft **failed at tier 4** in a clean environment. Tiers 1–3
passed, but the platform server never answered `/healthz`. Its log held
a three-line diagnosis:

```text
config error: source store is 'az' (the default under MUNARIUM_STORE=postgres):
set MUNARIUM_AZURE_STORAGE_ACCOUNT, or set MUNARIUM_SOURCE_STORE=pg to keep
document bytes in Postgres (local/CI posture)
```


This is §2's fail-closed default in a real case. The tier's environment
block was written before the source-store system landed. CI's platform
step pinned the variable when it added the same tier (server-ci.yml:90), but
the local script did not. The two copies of one tier drifted in opposite
directions.

The CI job exists because the tier once ran *only* locally. As a result, "a
run-breaking resolveSources bug shipped to main with CI green"
(server-ci.yml:82-84). Moving the tier to CI fixed that drift but introduced
this one. The fix is the pinned line now at
test.ps1:122-126, with a comment citing the CI mirror it matches.
Mirrors drift in both directions; §10 is about keeping them honest.

On the fixed script, the tier passes in a clean environment. The transcript
below is the fixed script's run, re-verified 10/10 for this revision:

```text
== platform: starting server (postgres store, 18081)
2026-08-12T01:51:25Z  INFO munarium_server: starting config.store=Postgres http=127.0.0.1:18081 grpc=None
2026-08-12T01:51:26Z  INFO munarium_server::state: source bytes store backend="pg"
2026-08-12T01:51:26Z  INFO munarium_server: REST plane listening addr=127.0.0.1:18081
== conformance: platform scenarios (uid/tokens/runbook-v2/sessions/ingest/removal/reports)
MMP conformance — platform surface (http://127.0.0.1:18081)
--------------------------------------------------------
  PASS platform.discrepancy-findings
  PASS  platform.uid-contract
  PASS  platform.role-partition
  PASS  platform.application-and-compartments
  PASS platform.evidence-hierarchy
  PASS  platform.removal-double-pass
  PASS  platform.reports-and-revoke
  PASS platform.authoring-lifecycle
  PASS platform.bulk-upload-lifecycle
  PASS platform.admin-dashboards-render
--------------------------------------------------------
10 passed, 0 failed
all requested test tiers OK
```

Ten scenarios cover the platform surface end to end: connector findings,
the uid contract, rw/mgmt role partitioning, runbook-v2 applications with
compartments, the evidence hierarchy, the two-pass removal flow, reports
plus token revocation, guided authoring, bulk upload, and the admin
dashboards. Section 7 explains the five this chapter walks through in
detail.

### Where the server logs land

Both live tiers redirect server stderr to a file in your temp directory.
The black-box server uses `$env:TEMP\munarium-test-server.log`
(test.ps1:84-85). The platform server uses
`$env:TEMP\munarium-ent-server.log` (test.ps1:132-133). On startup failure,
the script shows the last ten lines before it reports an error
(test.ps1:93-96, :141-144).

Two details about these files can save you time. On a *healthy* run, they
are **empty**. The server's tracing output goes to stdout and appears in the
test script's console. That is where the `INFO munarium_server: starting …`
lines above came from.

The files catch early, fatal stderr, such as a config error printed before
the process exits. That is what happened in the tier 4 incident above. The
console was empty where the startup lines should have appeared. The
three-line `config error: source store is 'az' …` diagnosis was in
`munarium-ent-server.log`. When a live tier dies before `/healthz` answers,
that file is the first place to look and is usually the last.

### The vacuous-skip pattern

Tier 1 promises that a red bare run is *always* a real failure. That promise
depends on one practice used by every test in the tree that needs
external state: **skip vacuously when your gating env var is unset.** The
pg integration tests state the contract in their doc header
(pg_integration.rs:1-4):

> Postgres integration tests. Skip (pass vacuously) when
> MUNARIUM_TEST_DATABASE_URL is unset, so plain `cargo test --workspace`
> stays green without a database

and the implementation is one idiomatic line at the top of each test
(pg_integration.rs:30):

```rust
let Some(url) = test_url() else { return };
```

There is no `#[ignore]`, which would require a flag, and no failure or
conditional compilation. The test passes without an assertion when its
world is absent. It runs fully when that world is present.

This pattern makes `ok` unclear by itself, which is why the tiers exist.
`-Postgres` promises that the pg-gated `ok` results were real. Use this
pattern when you write a test that needs external state. Name its
environment variable with the `MUNARIUM_TEST_*` pattern.

Three gated families exist at v0.1.2. You do not have to accept the pattern
on faith. Here are both halves running against the s3 family. Its doc header
is also the runnable recipe (s3_integration.rs:1-14).

**Half 1: the variable is unset.** The skip announces itself on stderr, so ask
cargo not to swallow it:

```powershell
cargo test -p munarium-store-objects --test s3_integration -- --nocapture
```

```text
running 1 test
skipping s3_integration_roundtrip: MUNARIUM_TEST_S3_ENDPOINT is not set
test s3_integration_roundtrip ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

You get `ok`, `0.00s`, and a clear explanation. Without `--nocapture`, you
get the same `ok` with no message. That is how this harness appeared in the
tier 1 transcript above.

**Half 2: the world is provided.** MinIO is one compose profile away. The
the test file's own doc comment is the recipe, reproduced and executed
here:

```powershell
docker compose --profile s3 up -d minio
docker compose exec minio mc alias set local http://127.0.0.1:9000 minioadmin minioadmin
docker compose exec minio mc mb --ignore-existing local/sources
$env:MUNARIUM_TEST_S3_ENDPOINT = 'http://127.0.0.1:9000'
cargo test -p munarium-store-objects --test s3_integration
```

```text
Added `local` successfully.
Bucket created successfully `local/sources`.
…
running 1 test
test s3_integration_roundtrip ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
```

The result is still `ok`, but now it includes a real S3 round trip against a
real bucket in `0.03s`. The timing is a clue again.

This run found one first-run detail. If compose must *pull* the MinIO image,
the container may not accept connections when the first `mc alias set`
checks it. A connection-refused error means you should retry the two `mc`
commands a few seconds later. Then leave things as you found them:

```powershell
docker compose stop minio
Remove-Item Env:MUNARIUM_TEST_S3_ENDPOINT
```

The three families, for the record:

- **`MUNARIUM_TEST_DATABASE_URL`** is the pg family:
  `src/munarium-store-pg/tests/pg_integration.rs` (the FOR-UPDATE
  concurrency race and the mem-agreement test) and
  `src/munarium-retrieval-pg/tests/collections_integration.rs` (twelve
  collection/indexing tests, same contract per its header). You rarely
  set this one by hand. `test.ps1 -Postgres` sets and unsets it for you.
- **`MUNARIUM_TEST_S3_ENDPOINT`** is the object-store family demonstrated
  above; companion vars (`MUNARIUM_TEST_S3_BUCKET`, `_REGION`, access keys)
  default to the MinIO throwaways (s3_integration.rs:31-36). No tier sets
  it: CI is network-free and never starts MinIO (docker-compose.yml:47,
  the `s3` profile is off by default), so this family is strictly
  on-demand.
- **`MUNARIUM_LIVE_PROVIDER_TESTS`** is the paid family. The contract
  tests in `src/munarium-providers/tests/contract.rs` run against an
  in-process mock (the four dialect/retry/fail-closed tests you saw
  pass in tier 1), and since 2026-08-17 the file also carries the live
  smokes its header always promised (§13's entry 5, now closed): one
  per family (`live_anthropic_smoke`, `live_openai_smoke`,
  `live_openrouter_smoke`), double-gated with the vacuous-skip pattern —
  first on `MUNARIUM_LIVE_PROVIDER_TESTS=1`, then on the family's
  conventional key var (`MUNARIUM_SECRET_ANTHROPIC` / `_OPENAI` /
  `_OPENROUTER`, the same names the deployed environments use). Each is
  one minimal fast-tier completion (openai adds one embedding) asserting
  transport shape only — non-empty text, usage accounting — never model
  output content. CI never sets the gate, so these never run there by
  construction. The `MUNARIUM_TEST_PROVIDER_KEY` env var you will also find
  there (contract.rs:91) is a *fake* credential the mock tests set to
  exercise the `CredentialRef::Env` resolution seam, not a gate.

### The compose profiles

`docker-compose.yml` is one file, three postures, selected by profile
(its header, docker-compose.yml:1-5, is the summary):

- **Default** (`docker compose up`) includes Postgres plus the containerized
  server: §2's dev profile, the Introduction's first-contact target. The
  server container sets `MUNARIUM_SOURCE_STORE: pg` explicitly with a
  comment explaining why: "Document bytes in Postgres, the offline
  fallback, so the dev profile needs no cloud account"
  (docker-compose.yml:32-36). This is the same pin tier 4 carries, learned the
  same way.
- **`--profile gateway`** adds the Envoy container. One gateway port
  (8443-style, remappable via `MUNARIUM_HOST_GATEWAY`) routing REST and
  gRPC-over-HTTP/2 by content type, the same shape deployed environments
  put on :443. Reach for it when you are debugging something that only
  reproduces *through* the gateway plane.
- **`--profile s3`** adds MinIO, as just demonstrated. It is off by default;
  "CI and
  the normal dev loop never start it" (docker-compose.yml:44-47).

Profiles keep the default posture minimal: what you did not ask for does
not start, does not take ports, and does not appear in `docker compose
ps`.

### A persistent stack for application development

For manual checks, corpus loading and client-library work, keep PostgreSQL
in a named volume and replace only the Server container between versions.
The public source Compose file supports this through its `pgdata` volume.
To use the released binaries without a Rust toolchain, follow
[§10's Docker Hub walkthrough](#deploy-the-published-docker-hub-image).
It includes a write/recreate/read check and a backup/restore drill.

Earlier revisions of this guide described a `localdeploy.ps1` operator
script here. That script is not shipped in the public Server tree. The
standalone Compose recipe in §10 is the public path; it requires no
private deployment scripts. In either path, readiness is the first check,
followed by an authenticated operation and a persistence check.

### Working smaller: one crate, one test, one mode

The ladder is the outer loop. The inner loop, while you iterate on one
thing, is narrower and faster:

```powershell
cargo test -p munarium-core                    # one crate's suite (< 1s warm)
cargo test -p munarium-core gates::            # one module's tests, by prefix
cargo test -p munarium-store-pg --test pg_integration   # one integration harness
cargo build -p munarium-server                 # compile just the binary path
```

The conformance harness selects a **mode**, not a scenario. Its modes are
`--in-process`, `--postgres <url>`, `--http`/`--grpc`, and `--platform`
(conformance/src/main.rs:1-15). There is no run-one-scenario flag. The seven
in-process scenarios finish in milliseconds, so there is no need for one
yet.

For a quick standalone invariant check during an `munarium-core` refactor, use
either command below. They prove the semantics without the whole workspace
suite:

```powershell
cargo run -q -p mmp-conformance -- --in-process
cargo test -p mmp-conformance               # same scenarios as a plain test
```

The second command works because the harness also ships its scenario set as
an ordinary integration test, `conformance/tests/in_process.rs`. As a
result, every `cargo test --workspace` proves the invariants again, whether
or not you ask for it.

The judgment call, then, in the form you will actually use it:

- **Editing kernel or library code?** `cargo test -p <crate>` while you
  iterate; bare `.\test.ps1` when the change settles.
- **Touching storage, SQL, or a migration?** Add `-Postgres`. Nothing
  cheaper can see your bug, and its gated tests are otherwise passing
  vacuously.
- **Touching either plane, DTOs, auth, or anything on the wire?**
  Use `-BlackBox`. Parity is asserted there and nowhere cheaper.
- **Touching the platform surface (uid, tokens, runbooks v2, sessions,
  ingest, removal, reports)?** Use `-Platform`. No cheaper rung touches
  that surface at all.
- **About to push?** `.\build.ps1 -Lint`, then `.\test.ps1 -All`. CI runs
  the same gates plus the drift and boundary checks; the full local
  mirror of *those* is `gates.ps1`, which belongs to §10's story.

### Where you stand

You now control the loop. Lint exactly as CI lints, climb as high as your
change requires, and trust `ok` because you know when it is empty. Prove the
stack with conformance instead of checking it by eye.

You still need a map of *where* to make a change. You need to know which of
the twenty crates owns the part you will touch and which boundaries you
must not cross. Section 4 provides that map.

## 4. Workspace tour: the crates and their boundaries

§3 ended with the question this chapter answers: which of the seventeen
crates owns the thing you are about to change? It also asked which
boundaries you must not cross to get there. The answer is teachable
because the workspace is not twenty crates in a bag. It is six layers
with one rule between them. Once you understand the layers, "where does
my change go?" usually answers itself.

This chapter walks through every crate using its own doc header as the
source. Each crate's `lib.rs` opens with a header that serves as the
crate's contract. This tour quotes those headers instead of paraphrasing
from memory. It then makes the boundary rules clear, including which ones
a machine enforces and which are still documented folklore. All anchors
are v0.1.2.

> ✎ **This chapter's census is stale as of the 2026-08-30 datastore merge,
> and says so rather than pretending otherwise** (re-measured 2026-08-31).
> The workspace is now **twenty** src crates / **twenty-one** members —
> `munarium-datastore` and `munarium-retrieval` joined after this tour was
> written. The layer rules the
> chapter teaches are unchanged (the two new crates ship with their own CI
> boundary gates: no Axum/tonic/SQLx in `munarium-datastore`'s graph, the
> storage backend named only at the composition root). The walk-through
> itself has not been re-executed against the new tree; re-executing it is
> the datastore program's documentation obligation, recorded here so the
> stale counts read as dated rather than current.

### How to read a seventeen-crate workspace

Start with what is actually there:

```powershell
Get-ChildItem src -Directory -Name
```

```text
munarium-access
munarium-api-types
munarium-authoring
munarium-azure-auth
munarium-cli
munarium-core
munarium-docintel-az
munarium-extract
munarium-proto
munarium-providers
munarium-retrieval-pg
munarium-runbooks
munarium-server
munarium-shapes
munarium-store-mem
munarium-store-objects
munarium-store-pg
```

These seventeen directories are in alphabetical order. That is the wrong
reading order because it puts the access-token crate first and the kernel
sixth. The eighteenth workspace member lives one directory up:
`conformance/` (the `mmp-conformance` package). It sits outside `src/`
because it is not part of the product. It is the harness that judges the
product.

The root `Cargo.toml` `members` list is closer to the truth. It is roughly
in dependency order, from `munarium-proto` through `munarium-cli`. However, the
layering makes the workspace easiest to understand:

**kernel → protocol → storage → retrieval/extraction → access/providers →
surface**, with the conformance harness standing beside the whole stack.

![The workspace by layer: twenty crates in five layers with dependencies flowing down to munarium-core, the upward direction forbidden, and mmp-conformance beside the stack running the same assertions in-process and over both wires](images/ch4-crate-map.svg)

The layering is not a diagram convention; it is a dependency fact you can
check. Ask Cargo who depends on the kernel:

```powershell
cargo tree --workspace -i munarium-core -e normal --depth 1
```

```text
munarium-core v0.1.2 (…\src\munarium-core)
├── munarium-api-types v0.1.2
├── munarium-azure-auth v0.1.2
├── munarium-docintel-az v0.1.2
├── munarium-extract v0.1.2
├── munarium-providers v0.1.2
├── munarium-retrieval-pg v0.1.2
├── munarium-server v0.1.2
├── munarium-shapes v0.1.2
├── munarium-store-mem v0.1.2
├── munarium-store-objects v0.1.2
├── munarium-store-pg v0.1.2
└── mmp-conformance v0.1.2
```

Twelve of the eighteen members sit directly on the kernel; the
thirteenth is the kernel itself. The five crates that remain are as
instructive as the twelve, because each absence is a design statement,
not an accident:

- **`munarium-proto`** depends only on prost, prost-types, and tonic. It is
  the wire, and the wire is not allowed to know the domain.
- **`munarium-access`** depends only on `jsonwebtoken`, serde, and thiserror
  because token mechanics are pure logic. They are testable with nothing else in the
  room.
- **`munarium-runbooks`** depends only on serde, serde_json, and
  `serde_yaml`. It parses and validates specs. The executor that *runs*
  them lives in the server.
- **`munarium-authoring`** (2026-08-19) depends on `munarium-shapes` and
  `munarium-runbooks` — never the kernel — plus serde and sha2. Guided
  authoring is pure document composition and validation; the drafts
  table, the BYOK assist call, and every route live in the server.
- **`munarium-cli`** depends only on blocking `reqwest`, `serde_json`,
  `serde_yaml`, `sha2`/`hex` (bundle hash verification), and `uuid`. The
  CLI's whole design claim is that the REST plane is sufficient, and its
  dependency list is the proof.

One more number needs an honest explanation. §1 showed the kernel's
*direct* dependencies (`--depth 1`: nine crates, no frameworks). However,
depth 1 is a view, not a check. A banned crate could hide behind an
intermediate dependency. The check walks the full transitive tree, just
as CI does. Run the transitive version yourself:

```powershell
cargo tree -p munarium-core -e normal --prefix none |
  ForEach-Object { ($_ -split ' ')[0] } | Sort-Object -Unique
```

At v0.1.2, that prints 33 unique crate names. The list includes
`munarium-core` itself and 32 transitive dependencies from the nine direct
ones. These include proc-macro plumbing, `chrono`'s platform crates, and
more. None of the 32 is sqlx, axum, tonic, reqwest, or utoipa. The
boundary table at the end of this chapter quotes the CI step that checks
this on every push.

Now the tour moves layer by layer, from the bottom up. Every other layer
is defined by its relationship to the kernel.

### The kernel: `munarium-core`

You met its doc header in §1. It lists the five invariants "carried over
verbatim (the conformance suite enforces them against every storage
backend)." This tour adds what §1 deliberately skipped: what is actually
*in* the crate. Fifteen modules appear in `lib.rs` under that header:

```text
chrono_gate  composer  counters  digests   docintel
error        gates     ledger    promises  provider
retrieval    similarity  sources  storage  types
```

Read that list as two groups. The first covers the semantics this crate
ports. It includes `ledger` (append, supersession,
and `resolve_slice`, the reference read that storage backends must match), `gates` and
`chrono_gate` and `similarity` (the deterministic write-path governance),
`composer` and `digests` (budget-aware context composition, pin-aware
rebuild), `promises` and `counters` (the obligation and budget stores),
and `types` and `error` (the domain vocabulary). `KernelError` carries
gate-finding vectors by design, because a policy rejection is payload, not
an anomaly.

The second group contains the trait seams: `storage`
(`StorageBackend`), `sources` (`SourceStore`), `retrieval`
(`RetrievalBackend`), `provider` (`ModelProvider`), `docintel`
(`DocumentIntelligence`). Every adapter crate in the layers above
implements exactly one of these, which is why the Cargo description can
end the way it does: "Pure: storage and retrieval live behind traits; no
HTTP, no SQL, no provider calls."

You touch this crate to change what the mesh *means*. Examples include a
gate's behavior, supersession semantics, composer degradation order, or
a new trait seam. Two rules apply to every such change. First is the
purity rule from §1. Code that needs `reqwest` or `sqlx` belongs in an
adapter crate behind a seam, and CI refuses the shortcut. Second is the
proof rule. A kernel semantic change is not done until the conformance
scenario set still passes or has been extended to cover it. See recipe 9
in §6 and the scenarios in §7. The header's claims are true only while
the harness keeps them true.

### The protocol pair: `munarium-proto` and `munarium-api-types`

Two crates own the two wire vocabularies, and each header is mostly a
prohibition.

**`munarium-proto`** is the generated MMP surface. Its header:

> The proto files under `server/proto/mmp/v1/` are the normative wire
> spec. These types are wire-only: axum handlers and tonic service impls
> convert to `munarium-api-types` / `munarium-core` domain types at the boundary
> and never let prost types travel further in.

Mechanically it is a `build.rs` running vendored protoc ("no system
protoc on Windows dev boxes or CI runners," so you never install anything),
`tonic::include_proto!`, and an embedded `FILE_DESCRIPTOR_SET` that does
double duty: it powers server reflection ("so `grpcurl` works without
local protos") and feeds the `gen-grpc-docs` binary that generates
[api/grpc-reference.md](../api/grpc-reference.md), which CI drift-checks.
You touch this crate when the *spec* changes. A proto edit is
additive-only, and recipe 2 in §6 walks the full process. The gotcha is
the prohibition itself: the compiler will happily let a prost type leak
into `service.rs`; only review stops it, a point the boundary table
returns to.

**`munarium-api-types`** is the REST vocabulary, and its header states the
rule this guide has already leaned on twice:

> Field names here are the OpenAPI truth: snake_case, dotted rule ids
> preserved as strings. Never derive ToSchema on prost types. Mirror
> here instead.

There is one struct per wire message and, since 2026-09-02, **no dependency on
`munarium-core`**: the explicit conversions to and from domain types live in
`munarium-api-conv` as the `Convert<T>` trait (`claim.convert()`,
`dtos.into_iter().map(convert)`), one impl per pair in each direction. They moved
out because the DTO crate ships in the public contract bundle, and they
became a trait rather than `From` impls
because the orphan rule keeps a `From` between two foreign types out of a third
crate. Behind the `proto` feature, a `wire` module holds the DTO↔prost
conversions. This lets the gRPC plane share the same
JSON-shaped types. This crate owns `Problem`, the RFC 9457 error
envelope with the mesh's extension members, whose registry is
[api/errors.md](../api/errors.md).

You touch it whenever an endpoint
gains a request or response field (recipe 1) or an error gains an
extension member (recipe 4). The gotcha is that JSON casing is decided
here, once, for everything. Renaming a field here is an API break. The
OpenAPI drift check will catch a missed regeneration, but only review can
catch a rename that should not have happened.

### Storage: `munarium-store-mem`, `munarium-store-pg`, `munarium-store-objects`

Three crates cover two seams. The first two implement `StorageBackend`,
the ledger's home. The third implements `SourceStore`, where document
bytes live. The workspace keeps both the reference implementation and
the production one. It runs the same conformance scenarios against both.
This is the workspace's second-favorite trick after plane parity, which
applies the same idea to a different boundary.

**`munarium-store-mem`** and **`munarium-store-pg`** implement one contract
twice on purpose. You rarely touch either one alone. When the
`StorageBackend` trait grows, both move in lockstep. The conformance suite
keeps that lockstep honest. The reference is store-mem, whose header states
the rule: seq allocation happens under one write
lock (the BEGIN IMMEDIATE equivalent), supersession resolution delegates
to the reference `ledger::resolve_slice`, and every store stamps seq
from the single lineage counting domain so one pin bounds everything.

It is the conformance target, a dev-profile option, and the backend
used by your bare `cargo test` runs. It also hosts `MemSourceStore` on
purpose because the object-store crate needs that backend to have zero
dependencies (see below). Production is store-pg. Its header states the
anti-drift mechanism. This pattern is worth quoting because you should
copy it whenever you add a second implementation of anything:

> Semantics contract: identical to `munarium-store-mem`. The conformance
> suite runs the same scenarios against both, and reads resolve through
> the SAME `munarium-core` reference functions (`ledger::resolve_slice`,
> `promises::status_as_of`), so the two backends cannot drift apart.

There are two layers of sameness: shared assertions and shared resolution
code. The backends differ in how they fetch rows, never in what those
rows mean. Store-pg adds the production write path. You should understand
this path before changing it.

A transaction takes `SELECT … FOR UPDATE` on the lineage root's
`lineage_heads` row. This is the Postgres version of SQLite's
`BEGIN IMMEDIATE`, and it serializes writers per lineage. The transaction
computes the head inside the lock, enforces `expected_head`, then inserts
the ledger event and claims-projection row together. Handles are
tenant-scoped at construction through `TenantScopedStore`. Every query
therefore carries the tenant predicate by construction.

Migrations are embedded
(`sqlx::migrate!("./migrations")`) and applied on connect, which is
recipe 3's territory along with its stale-embed gotcha. The header also
carries an honesty marker you should preserve when editing: reads
currently fetch the lineage's rows and resolve in Rust. This is "correct
at demo scale and provably agrees with the reference semantics." The
push-down into SQL is a deliberate deferral and is not implied to exist.

**`munarium-store-objects`** holds document bytes for four backends: S3,
Azure Blob, GCS, and the local filesystem. It uses one adapter over the
Apache Arrow `object_store` crate. This replaces a hand-rolled Azure REST crate.
Its header enumerates three contracts, "all test-enforced here":

> - **Recorded URIs never carry a credential.** The URI written to the
>   `sources` row is constructed from the bucket/endpoint topology alone,
>   never from a signed request.
> - **Azure stays byte-identical.** `backend_id()` remains `"az"` and the
>   recorded URI remains
>   `https://{account}.blob.core.windows.net/{container}/{…}`, so rows
>   written by `munarium-store-az` and rows written here are
>   indistinguishable.
> - **Credentials are ambient by default.** S3 walks the standard AWS
>   chain …; GCS walks `GOOGLE_APPLICATION_CREDENTIALS` then the metadata
>   server; Azure falls back to IMDS managed identity … unless a SAS is
>   supplied.

Note what the header also refuses: "The in-memory backend intentionally
does NOT live here." `MemSourceStore` stays in the zero-dep store-mem
crate, and `object_store::memory::InMemory` appears only in tests. You
touch this crate to add a fifth byte backend (recipe 6). The gotcha is the
second contract. Anything that changes a recorded URI's shape is not a
refactor. It is a data-compatibility break with rows already written.

### Retrieval and extraction: `munarium-retrieval-pg`, `munarium-extract`, `munarium-docintel-az`

Three crates take a document from raw bytes to ranked, citable evidence.
Two of them exist because of a specific, named failure that their headers
refuse to forget.

**`munarium-retrieval-pg`** implements the kernel's `RetrievalBackend` as
in-Postgres hybrid retrieval: tsvector lexical plus pgvector HNSW,
reciprocal-rank fused (k configurable per shape, default 60). Its header
is a dense statement of retrieval's identity rules. Three of them do the
most work. A source is identified by its *logical path*. The content hash
travels "as INTEGRITY, not identity, so the same bytes staged at two paths
are two independently bindable sources."

The pipeline is keyless by
default. The `local-hash@1` embedder is
deterministic 256-dim feature hashing, so retrieval works with no
provider account, and a tenant's BYOK embedder swaps in per index build,
named in the manifest either way. And the versioning rule that carries
invariant #5: "index_version = hash(shape_ref, chunker, embedder, sorted
source set); builds are side-by-side and immutable; cutover is the
`active` flip; old versions keep resolving so past envelopes stay
verifiable."

Internally, collections are LIST-partitioned with one
partition per collection, created as advisory-locked runtime DDL
(`collections.rs`, with `pg_advisory_xact_lock` around the partition
create), which is the detail to know before you touch anything that runs
DDL at request time. There is no numbered recipe for ranking changes;
the declarative knobs live in shapes (Part II §16), and changes to the
machinery itself start here.

**`munarium-datastore`** (2026-08-30) is the derived-index tier's engine
crate: immutable, content-verified search artifacts — Tantivy shards plus a
vector index, exact or DiskANN behind the off-by-default `vector-diskann`
feature — sealed from the server's committed chunks, hydrated to local disk,
and opened through an L0 open-shard cache. Its header states the boundary
CI enforces with a `cargo tree` grep (§10's datastore boundary step): no
Axum, tonic, SQLx, `munarium-core`, server config, auth or runbooks anywhere
in its graph, so the crate stays independently usable and PostgreSQL stays
the system of record. §8A is the server-side map of the plane it
implements.

**`munarium-retrieval`** is the coordinator the server calls instead of a
backend. It dispatches each scope to PostgreSQL or the datastore on the
rollout selector with **no fallback past selection**, owns the
engine-neutral fusion that replaced `merge_hits_weighted` (proven
bit-identical against the old implementation as oracle), the mirror build
(extraction → seal → publish → bind), and the readiness warmer's
serving-required set. A second CI grep (§10's retrieval boundary step) keeps the server
naming a storage backend only at its composition root, `state.rs`. §8A is
the operator's view of this pair.

**`munarium-extract`** turns bytes into indexable text, and its header
opens with the failure that created it: "Before this crate,
`build_collection_index` ran `String::from_utf8_lossy` over every source.
A PDF then chunked and embedded as replacement characters, so retrieval
returned confident noise." Two rules define the crate. First, it runs at
*index* time, not ingest time. Raw bytes stay pristine in the object
store. This "makes an extractor improvement a rebuild rather than a
re-upload, under the same 'rebuild, don't migrate' rule the rest of the
system follows."

Second, it uses only pure Rust. The header does not
hedge: "pdfium, poppler, mupdf and tesseract are not tradeoffs to weigh.
They cannot link at all," because the image is a static musl binary on
distroless. The crate-level `EXTRACTOR_SET_VERSION` ("bump when any
extractor changes its output for the same bytes") joins the index
identity, so improving an extractor correctly invalidates and rebuilds
(recipe 7). The `ocr` feature is off by default. OCR pulls model
weights and real compute into a build that is otherwise instant.

**`munarium-docintel-az`** is the escalation seam behind the kernel's
`DocumentIntelligence` trait, for "documents the local extractors cannot
read: scanned PDFs, TIFF faxes, and the case that motivated it, PDFs
whose page images are JBIG2 or CCITT encoded, for which no pure-Rust
decoder exists." Its posture is the point: "**It is off unless
configured.** Every call costs money per page and leaves the cluster,"
so nothing runs by default.

It is hand-rolled over the Analyze Document
REST surface on the workspace's existing rustls `reqwest`, with no Azure
SDK, as elsewhere. It pins `API_VERSION = "2024-11-30"` for a reason that
generalizes: "a hosted model's output is only reproducible against a
fixed version, so it goes in the fingerprint." If you ever wrap
another hosted model, copy that rule before you copy anything else.

### Access and providers: `munarium-access`, `munarium-providers`, `munarium-azure-auth`

Everything credential-shaped lives in these three crates, and they share
one posture: secrets are resolved at the moment of use, never stored.

**`munarium-access`** is deliberately small, and its header leads with what
it is not: "munarium-server is NOT the identity provider." The platform
API-management layer authenticates humans and exchanges its `mgmt` token
for "a short-lived, least-privilege JWT minted here." It uses HS256 with
one server-held secret and is verified locally, with "no JWKS, no OIDC,
no introspection." The one authorization primitive is level plus
compartments, described as "Bell-LaPadula 'simple security' with
categories." It also uses scopes (`query`, `ingest`) and a hard TTL
ceiling that issuance clamps to.

The header ends with a purity claim: "Like munarium-core, this crate must
never depend on sqlx/axum/tonic/reqwest. CI greps the dependency tree."
Since 2026-08-17 that sentence is true as written: the boundary step
greps `munarium-access` with the same banned list it applies to
`munarium-core`, in both mirrors (server-ci.yml and gates.ps1). It
spent v0.1.2 ahead of the workflow — the machine checked one crate, not
two — which was §13's entry 1, now closed.

**`munarium-providers`** is the BYOK gateway behind the kernel's
`ModelProvider` trait. It supports Anthropic Messages, OpenAI Chat +
Embeddings (base-URL override covers Azure OpenAI and vLLM), and
OpenRouter in the OpenAI dialect. The header's load-bearing sentence:
"Keys resolve
through the `SecretResolver` seam at call time and are never stored,
logged, or serialized." In a deployed environment, secrets from your
secret store land as env vars or CSI-mounted files, "so the `env` and `file`
resolvers ARE the vault path. Rotation is a vault operation invisible
to munarium." The crate owns bounded retry honoring `retry-after` and
per-config rpm/tpm token buckets. Invocation provenance is explicitly
*not* its job. It is "the server's job on top of these responses" (§5).

Recipe 5 adds an adapter, and its gotcha lives here: the tier-model
coverage test fails if you register a provider without a
`builtin_tier_model` entry. One documented rule rides this crate:
providers never depend on a storage crate. Its only workspace
dependency is `munarium-core`, and since 2026-08-17 the boundary step
asserts it — `cargo tree -p munarium-providers` must name no
`munarium-store-*` or `munarium-retrieval-*` crate (the inverted variant of
the purity grep; §13 entry 1, closed).

**`munarium-azure-auth`** exists because of a failure mode its header names
precisely: every Azure-backed crate needs the same token
lifecycle, and "a second subtly-different copy of that lifecycle is how
you get an outage at the 24-hour mark. One implementation, two consumers
(Blob, Document Intelligence), and any future Azure plane." It is one
GET against the platform identity endpoint (Container Apps and App
Service inject `IDENTITY_ENDPOINT` plus a rotating `IDENTITY_HEADER`;
VM and AKS workloads fall back to IMDS), cached until shortly before
expiry, with a fail-closed configuration rule quoted straight from its
error text: "IDENTITY_ENDPOINT and IDENTITY_HEADER must either both be
set or both be absent."

Keep one more fact in mind when local
tooling misbehaves: IMDS is link-local and unreachable off-Azure by
design. This is "why every consumer also offers a key/SAS path for
local tooling."

### The surface: `munarium-server`, `munarium-cli`, `munarium-shapes`, `munarium-runbooks`, `munarium-authoring`

**`munarium-server`** is the binary with "three listeners, one service
layer, graceful shutdown." It is the one crate in the workspace that may
know about everything because its job is wiring. Its work includes config
from env, state construction, and the REST and gRPC planes. Those planes
funnel into the shared `service.rs` you met in §1. It also owns the ops
listener and the `munarium-server openapi` command that prints the document
CI drift-checks.

It is deliberately a thin shell over the library crates (§1's
"library-first" note), but thin is relative. The platform surface still
puts much of its module mass here. §5 maps those modules and their sizes,
so you know where the weight is before you open the crate. Even its two
crate-level lint
allowances are doctrine in miniature: `result_large_err` because
"KernelError carries gate-finding vectors by design (policy rejections are
the payload, not an anomaly)"; `too_many_arguments` because the step and
invocation recorders take one argument per recorded dimension.

**`munarium-cli`** is `mmctl`, and its header is a design argument in
two lines: "Deliberately std-arg-parsed and thin: every operation is one
REST call, so anything mmctl does, CI can do with curl." There is no
subcommand framework, no gRPC, and no domain types. Apply, run, approve,
and token issue each use HTTP. The Cargo description adds the intended
consequence: "Wire `mmctl apply` into CI so runbook changes flow
through review like code." If you find yourself adding logic to the CLI
that is not a REST call plus formatting, you are building something the
server should expose instead.

**`munarium-shapes`** is the shape registry: "a shape is a versioned
declarative bundle" with three rules stacked in its header:
"validation at the command gate; additive versioning (old versions stay
resolvable because they are provenance); shapes are data deployed through
the API, and their publication is itself a ledger event" (the server appends
a `munarium-shapes.<name>=<version>@<hash>` claim in the tenant's system
lineage, so governance eats its own cooking). A shape violation is not
an HTTP 4xx improvisation; it is a policy rejection with citation
(`shape.schema-violation`), recorded like every other gate finding.
Validation results are cacheable by `(shape_ref, body_hash)`. Shapes
are data, not code. That is why adding one is recipe 8's YAML path,
not a Rust change.

**`munarium-runbooks`** parses and validates the two runbook spec
generations: v1 (`spec.shape`, the single-shape reindex pipeline) and
v2 (`spec.collections`, the retrieval application with declarative
source bindings and an optional RAG completion step). Its scope fence is
in the Cargo description and worth internalizing before you propose an
orchestration feature: "the executor stays a step machine. Fan-out
orchestration is Temporal's seam, not scope creep here." (The executor
itself lives in the server's `runbooks_api.rs`; this crate owns the
types, parsing, and deterministic validation with stable dotted finding
codes.)

It also carries the workspace's best-documented serde gotcha:
steps arrive as "raw single-key step maps (`- resolveSources: {}`);
converted by `parse_runbook` (serde_yaml 0.9 dropped single-key-map enum
syntax)." A new step kind is therefore *not* a serde enum variant. It goes
through `step_from_value`, or it will parse in your test YAML and fail
on every real runbook. Recipe 8 walks it.

**`munarium-authoring`** (2026-08-19) is the authoring half of what
`munarium-shapes` + `munarium-runbooks` enforce: the served §19 pattern catalog
(every `runbooks/{applications,shapes,pipelines}` sample embedded at
compile time, parse-verified by its own tests), the §16-ordered
interview, the deterministic answers→YAML materializer (which emits from
`serde_yaml::Value` trees and proves itself by re-parsing — a
`RunbookDoc` cannot be re-serialized faithfully because `steps` is
`#[serde(skip)]`), set-level cross-document validation (`set.*` codes:
unresolved shapes, additive-versioning preflight, sensitivity-inverting
prefix overlap, answer-key bindings), and the hash-manifested export
bundle `mmctl bundle apply` verifies before deploying. Pure like its
two siblings; drafts persistence (`authoring_drafts`, migration 0019),
the BYOK assist pass, and the `/v1/authoring/*` surface live in the
server's `authoring_api.rs`. (The `/admin/authoring` pages that shipped
beside it were removed 2026-08-27 when the dashboard became the operator
console — authoring is an API/CLI workflow whose deploy artifact is a
git-reviewed bundle, and a browser form for it served no real purpose.)

### The harness beside the workspace: `conformance`

The eighteenth member gets its own subsection because it is the
mechanism behind half the claims this guide has already made. Its header:

> MMP conformance scenarios, written against the `StorageBackend` trait
> so the SAME assertions run on the in-memory store, the Postgres
> store, and, through the client adapters, the running server's
> REST and gRPC planes (platform, with cross-plane parity diffing).
>
> The pin fixtures are the crown jewels: they encode the
> one-pin-bounds-everything semantic that defines the mesh.

That first sentence describes the whole architecture of trust in this
workspace. One scenario set is written once against the kernel's own
trait. It serves as both the storage-backend equivalence proof and the
cross-plane parity check. The `--http`/`--grpc` black-box modes drive
both planes with the same assertions and compare the outcomes. Extending it
is deliberately frictionless: `run_all`'s own comment says "Add here;
every backend runs the full set," and recipe 9 shows that one added
tuple runs your new scenario in all four modes with no further wiring.
§7 walks the scenarios themselves.

### The boundary rules: machine-enforced vs documented

Every rule this chapter has mentioned, in one table, with the honest
enforcement column. "CI" means a machine fails the build; "review"
means the rule is stated in a doc header and a human must catch the
violation.

| Rule | Stated where | Enforced by |
|---|---|---|
| `munarium-core` never depends on sqlx / axum / tonic / reqwest / utoipa | core's header; §1 | **CI**: transitive grep, quoted below |
| Migrations are additive-only (no destructive DDL, ever) | the CI step's own comment; recipe 3 | **CI**: DDL grep over `migrations/` |
| Generated API docs match the tree (`openapi.json`, `grpc-reference.md`) | §1's map | **CI**: drift checks |
| Storage backends agree with the reference semantics | store-pg header | **CI**: conformance, all modes |
| prost types never travel past the plane boundary | proto header | review only |
| JSON casing decided only in `munarium-api-types`; never `ToSchema` on prost | api-types header | review only (drift check sees symptoms, not the rule) |
| `munarium-access` holds the same purity rule as core | access header | **CI**: same grep, second crate (since 2026-08-17) |
| `munarium-providers` never depends on a storage crate | this chapter; architecture.md | **CI**: inverted grep — no `munarium-store-*`/`munarium-retrieval-*` in its tree (since 2026-08-17) |
| `munarium-api-types` depends on no server crate but `munarium-proto` (it ships in the public contract bundle) | api-types header; api-conv header | **CI**: inverted grep over its `--all-features` tree — the only `munarium-*` allowed are itself and `munarium-proto` (since 2026-09-02) |
| Recorded source URIs never carry credentials; Azure URIs stay byte-identical | store-objects header | crate tests |

The machine-enforced anchor, quoted in full because you should know
exactly what it does and does not check
([`server-ci.yml:207-235`](../../../.github/workflows/server-ci.yml);
mirrored step-for-step in gates.ps1):

```yaml
- name: crate boundary check
  # Mirrored in server/gates.ps1 'crate boundary check' — change
  # both in the same commit. Three machine-checked boundaries (dev-guide
  # §4 boundary table):
  #   munarium-core / munarium-access: never depend on sqlx/axum/tonic/reqwest/utoipa
  #   munarium-providers: never depends on a storage crate
  run: |
    for crate in munarium-core munarium-access; do
      deps=$(cargo tree -p "$crate" -e normal --prefix none | awk '{print $1}' | sort -u)
      for banned in sqlx axum tonic reqwest utoipa; do
        if echo "$deps" | grep -qx "$banned"; then
          echo "BOUNDARY VIOLATION: $crate depends on $banned"; exit 1
        fi
      done
    done
    deps=$(cargo tree -p munarium-providers -e normal --prefix none | awk '{print $1}' | sort -u)
    for banned in munarium-store-pg munarium-store-mem munarium-retrieval-pg; do
      if echo "$deps" | grep -qx "$banned"; then
        echo "BOUNDARY VIOLATION: munarium-providers depends on $banned"; exit 1
      fi
    done
    # munarium-api-types ships in the public contract bundle: it may depend on munarium-proto
    # and on nothing else of the workspace. The core <-> DTO conversions live
    # in munarium-api-conv.
    deps=$(cargo tree -p munarium-api-types --all-features -e normal --prefix none | awk '{print $1}' | sort -u)
    strays=$(echo "$deps" | grep -E '^munarium-' | grep -vx -E 'munarium-api-types|munarium-proto' || true)
    if [ -n "$strays" ]; then
      echo "BOUNDARY VIOLATION: munarium-api-types depends on a server crate:"; echo "$strays"; exit 1
    fi
```

Note the shape: full transitive tree, exact-name grep, four crates.
Until 2026-08-17 the step checked `munarium-core` alone and the last two
rows above read **review only [gap]** — that was §13's entry 1, closed
by pointing the same check at `munarium-access` (whose header already
promised it) and inverting it for `munarium-providers`.

### Where your change probably goes

Here is the layering in the form you will actually use. Find your task,
go to the crate, and open the recipe before you type:

| I need to… | Crate(s) | See |
|---|---|---|
| Add or change a REST endpoint | `munarium-server` (`rest.rs`, `openapi.rs`) + `munarium-api-types` | §6.1 |
| Add a gRPC RPC | `proto/mmp/v1/` + `munarium-proto` + `munarium-server` (`grpc.rs`) | §6.2 |
| Change the database schema | `munarium-store-pg/migrations/` | §6.3 |
| Add an error slug or extension member | `munarium-server` (`error.rs`) + `munarium-api-types` (`Problem`) | §6.4 |
| Add a model provider | `munarium-providers` | §6.5 |
| Add a document-bytes backend | `munarium-store-objects` + server config | §6.6 |
| Support a new file format at index time | `munarium-extract` | §6.7 |
| Add a shape, runbook, or step kind | `munarium-shapes` / `munarium-runbooks` / `runbooks_api.rs` | §6.8 |
| Change gate, ledger, pin, or composer semantics | `munarium-core` + a conformance scenario | §6.9, §7 |
| Add an env var | `munarium-server` (`config.rs`) + README table | §6.10 |
| Add a dependency, anywhere | read §9 first, then §6.11 | §9 |
| Change retrieval ranking or chunking machinery | `munarium-retrieval-pg` (declarative knobs: shapes, Part II §16) | §16 |
| Change token or permit logic | `munarium-access` (+ `tokens_api.rs` in the server) | §7 |

Two patterns in that table are worth naming because they also apply
elsewhere. Almost every behavior change lands in a *library* crate, while
`munarium-server` takes only the wiring. That is the library-first design
doing its job. Every row that touches the kernel or a wire also ends in a
proof obligation, such as a conformance scenario or drift-checked
regeneration. In this workspace, a boundary you cannot check is a
boundary you do not have.

You now hold the map: six layers, one direction of dependency, and an
honest list of which walls are concrete and which are paint. The map
deliberately flattened the one crate allowed to touch everything: the
server binary itself. It contains twenty-two modules for wiring,
middleware, the platform surface, and (since 2026-08-17) the
observability surface. §5 opens it up. It explains the
startup order, why each step is in place, where the module mass sits, and
how the config contract makes the whole system fail closed.



## 5. The server binary, end to end

§4 drew the workspace as six layers with one crate deliberately flattened:
`munarium-server`, the only member allowed to touch everything. This chapter
opens it up. As you read, test the claim implied by §4's table: the binary
is a *thin* surface over the kernel. It has twenty-two `mod` declarations
across twenty-three `.rs` files (the 2026-08-17 observability work added
`metrics`, `ops`, `charts`, and `dashboard`). Almost all of them provide
wiring, translation, and operator surfaces instead of behavior.

Behavior lives below in `munarium-core` and the storage crates. This is why
the conformance suite can prove that REST and gRPC are equivalent. Both
planes converge on the same 251-line service layer, with nowhere else for
semantics to hide.

Every transcript in this chapter was executed against v0.1.2 on 2026-08-11,
using the already-built debug binary (`target/debug/munarium-server.exe`) and
the §2 from-source alternate ports (18080 REST, 19090 ops, gRPC disabled).

### Where the mass is

> ✎ **Stale as of the 2026-08-30 datastore merge** (measured 2026-08-31):
> the crate is now ~29,000 non-blank lines across 40 `.rs` files with 35
> `mod` declarations — the table below is the 2026-08-27 tree, before the
> evidence plane (`evidence_*.rs`), the datastore modules
> (`datastore_*.rs`, `storage_api.rs`, `shadow_plane.rs`) and
> `dashboard/storage.rs` landed. `runbooks_api.rs` is still the largest
> (1,934). Re-run the one-liner below for the current answer; re-executing
> the prose is the datastore program's documentation obligation.

Run the census yourself. It is one line, and the answer changes where you
will expect to spend review time:

```powershell
$root = (Resolve-Path src/munarium-server/src).Path
Get-ChildItem -Recurse src/munarium-server/src -Filter *.rs |
  ForEach-Object { [pscustomobject]@{
    Name  = $_.FullName.Substring($root.Length + 1).Replace('\','/')
    Lines = (Get-Content $_.FullName | Measure-Object -Line).Lines } } |
  Sort-Object Lines -Descending
```

```text
Name                    Lines
----                    -----
runbooks_api.rs          1603
sessions_api.rs          1571
rest.rs                  1219
authoring_api.rs         1194
ingest_api.rs            1147
config.rs                1031
reports_api.rs            957
dashboard/inventory.rs    948
providers_api.rs          861
middleware.rs             803
state.rs                  791
dashboard/runbooks.rs     746
dashboard/mod.rs          686
grpc.rs                   656
grpc_data.rs                574
dashboard/monitoring.rs   515
charts.rs                 457
ops.rs                    425
error.rs                  383
grpc_platform.rs        373
metrics.rs                364
openapi.rs                312
service.rs                309
main.rs                   247
models.rs                 211
collections_api.rs        180
verification.rs           160
interactions.rs           119
chronology_api.rs         112
tokens_api.rs             104
```

(`Measure-Object -Line` counts non-blank lines; physical line counts run a
few percent higher. 19,058 non-blank lines total across the crate,
re-censused 2026-08-27 — the one-liner became recursive that day because
`dashboard.rs` became the `dashboard/` directory: `mod.rs` (auth, CSRF,
chrome, routes), `monitoring.rs`, `runbooks.rs`, `inventory.rs`. The
2026-08-17 census stood at 10,782; the growth since is the platform
surface — authoring, bulk uploads, chronology rules, streaming turns,
verification, and the operator console's control plane.)

Three numbers in that table teach more than the rest combined:

- **`runbooks_api.rs` at 1603 is the largest module in the binary**. It is larger
  than the whole REST plane. It is the runbook *executor*: the step machine
  that walks resolveSources → index → approve → cutover, with approval
  gates and idempotent step recording (and, since 2026-08-27, the
  control-plane reads the console's runbook hub renders). The
  `munarium-runbooks` crate owns parsing and validation (§4); the server
  owns execution, because execution needs everything: storage, retrieval,
  providers, and sessions. Only the server has everything.
- **`service.rs` at 309 is the punchline of the architecture.** The shared
  command/query layer both planes call is a quarter the size of the REST
  plane and a fifth of the gRPC one, because the kernel beneath it owns the
  actual rules. Its header states the contract: "BOTH planes (REST
  handlers, tonic impls) convert to these calls, so gate behavior,
  supersession, and pin semantics cannot diverge between planes. The
  conformance suite asserts it. The command path IS the governance path"
  (service.rs:1-7).
- **`main.rs` at 247 non-blank lines is one of the smallest files in the
  crate.** The entire startup story fits in one screenful per concern,
  which is why this chapter can walk all of it.

The rest of the code is the platform surface added milestone by
milestone. It includes `providers_api.rs` (BYOK gateway + `/healthai`),
`sessions_api.rs` (chat sessions and turns), `reports_api.rs`
(usage/audit/cost), `ingest_api.rs` (the batch front door), `grpc.rs` +
`grpc_data.rs` (tonic impls in two generations), `tokens_api.rs` +
`collections_api.rs` (capability tokens, collections). This chapter also
covers four cross-cutting spines in detail: `config.rs`,
`middleware.rs`, `error.rs`, `state.rs`.

### The startup order, and why each step is where it is

`main.rs` runs nine steps in a fixed order. The order is the design:
**everything that can be validated without side effects fails before
anything binds a port.** Exit code 2 means *your environment is wrong*.
Nothing was touched. Exit code 1 means *the environment was plausible but
the world said no*. A connection failed, or a pool could not be built.
This distinction is important for operators. §11's triage cookbook uses
it, and this section demonstrates both exits live.

![The startup sequence of main.rs: the openapi argv short-circuit exits 0; tracing init reads MUNARIUM_LOG; Config::from_env and the gRPC address parse both exit 2 on a config error; AppState::new exits 1 on a startup error; then the REST, direct gRPC and ops listeners bind, and an ops bind failure is fatal](images/ch5-startup-order.svg)

**Step 1: the `openapi` argv short-circuit (main.rs:38-44).** Before
tracing, before config, `munarium-server openapi` prints the OpenAPI document
and returns. It sits first *because* it must not require an environment: CI
regenerates `docs/api/openapi.json` from a checkout with no database, no
tokens, no env vars, and diffs it against the tree (recipe 1's closing
step). If the spec dump needed even `Config::from_env()` to pass, the drift
check would need a fake environment and would rot.

**Step 2: tracing from `MUNARIUM_LOG` (main.rs:46-51).** The subscriber
initializes before config parsing because step 3's *success* is announced
by a tracing line (`starting`, main.rs:60). If initialized any later, the
one line that proves config passed would be lost. Config *failure* does
not depend on tracing. It uses one plain `eprintln!`, so error reporting
owes tracing nothing. The filter uses tracing's env-filter syntax
(`munarium_server=debug,sqlx=warn`). A missing or malformed `MUNARIUM_LOG`
falls back to `info` instead of exiting. This is the one fail-open in the
whole sequence because logging detail is not worth dying over. §3 told you
where script-launched servers put this stream
(`$env:TEMP\munarium-test-server.log` and friends); a foreground boot writes
it to your terminal.

**Step 3: `Config::from_env()`, or exit 2 (main.rs:53-59).** The whole
environment contract is evaluated here, fail-closed, before any I/O.
Here is a demonstration against the built binary. Set a store that needs
a database, but provide no database:

```powershell
$env:MUNARIUM_STORE = 'postgres'
& .\target\debug\munarium-server.exe
```

```text
config error: MUNARIUM_STORE=postgres requires MUNARIUM_DATABASE_URL
EXITCODE: 2
```

One line on stderr, exit 2, no port bound, no log noise. And the contract
composes: fix that one variable and config still refuses to proceed on the
next missing prerequisite (the same run with `MUNARIUM_DATABASE_URL` set but
no token material exits 2 with `MUNARIUM_AUTH_MODE=static requires
MUNARIUM_STATIC_TOKENS or _FILE (token:tenant:role,...); use
MUNARIUM_AUTH_MODE=disabled to opt out`. Note that the message names the way out,
a house rule the config section below returns to).

**Step 4: the gRPC address parses before any bind (main.rs:62-74).** This
step looks redundant. Why not parse the address where the gRPC plane
starts, five steps later? The comment in the source gives the answer. It is quoted
in full because it is the crate's best one-paragraph lesson in startup
ordering:

> Validate the gRPC address BEFORE any listener binds: a bad
> MUNARIUM_GRPC_ADDR used to panic only after the REST plane was already up
> and logged as listening, leaving a half-started process. (main.rs:62-64)

A process that logs "REST plane listening" and then dies is worse than one
that dies at once. An orchestrator's log-based readiness check, a
colleague's quick `curl`, and your own eyes may disagree about whether it
started. The general rule is to *parse everything, then bind anything.* A
malformed address is a config error, so it exits 2, just as in step 3.

**Step 5: `AppState::new`, or exit 1 (main.rs:76-82).** Side effects begin
only now. They include the store registry, which is an in-memory map or a
real Postgres pool. `PgStore::connect` happens here. Other side effects
include the source-bytes store from
`build_source_store` (state.rs:146-212), the optional document-intelligence
provider, and the interaction writer task. Failures here are *startup*
errors. The environment parsed, but the world refused. This is shown with
a plausible-but-wrong database URL (and `MUNARIUM_SOURCE_STORE=pg` so the
source store does not fail closed first; see below):

```powershell
$env:MUNARIUM_STORE        = 'postgres'
$env:MUNARIUM_DATABASE_URL = 'postgres://munarium:wrong@localhost:5433/nope'
$env:MUNARIUM_SOURCE_STORE = 'pg'
$env:MUNARIUM_AUTH_MODE    = 'static'
$env:MUNARIUM_STATIC_TOKENS = 'devtoken:dev-tenant:rw'
& .\target\debug\munarium-server.exe
```

```text
2026-08-12T02:02:48.640355Z  INFO munarium_server: starting config.store=Postgres http=0.0.0.0:8080 grpc=Some("0.0.0.0:50051")
startup error: storage error: error returned from database: password authentication failed for user "munarium"
EXITCODE: 1
```

Read the two lines as a diagnosis. The `starting` line proves config
passed because step 3 logs it at main.rs:60, *after* `from_env` returns.
Whatever follows is not your env-var spelling. It is connectivity,
credentials, or migrations. This transcript shows the exit-2/exit-1
boundary doing its job. §11's triage cookbook builds its first decision
branch on this distinction.

**Steps 6–8: the three listeners, data planes first and ops last.** REST
binds first (main.rs:87-101) through `rest::router` over `MUNARIUM_HTTP_ADDR`,
serving `/v1`, `/docs`, `/openapi.json`, and health probes, h1 and h2c on
one port. The bind is awaited inline in `main`, so a REST bind failure
panics the process. This is honest because nothing else is up yet. A loud,
immediate death leaves no half-started ambiguity. The direct gRPC plane
follows (main.rs:104-156) unless `MUNARIUM_GRPC_ADDR=disabled`. The
tonic server stacks the health service, reflection (both mmp and health
descriptor sets), and six mmp services: command, query, ingest, retrieval,
runbook, provider. One builder call there earns its comment:

> Source chunks are whole-document slices; tonic's 4 MiB default would
> reject a large chunk mid-upload. (main.rs:120-121)

Therefore, the ingest service alone gets
`.max_decoding_message_size(rest::MAX_SOURCE_BYTES)`. The value is 256
MiB, the same ceiling used by the REST source routes (rest.rs:22).
Cross-plane parity covers more than semantics. Even the *limits* are
shared constants. A document that uploads over REST but fails over gRPC
would be a parity bug that the conformance suite must explain.

The ops listener comes last (main.rs:158-175), and its failure mode is
unique in the file:

```rust
} else {
    tracing::warn!(%addr, "ops port unavailable; continuing without it");
}
```

**Bind failure on the ops plane is a warn, not a fatal.** The asymmetry is
deliberate: the ops plane serves only liveness (`/healthz`, `/readyz`), and
a server that refuses to serve *data* because its *health port* was taken
would be optimizing for the monitoring system over the users. The data
planes are meant to die loudly; the ops plane is allowed to fail soft.

Since 2026-08-17 "meant to" IS enforced on the gRPC plane: the listener
binds INLINE before the spawn and before `direct gRPC plane listening` is
logged, then hands the bound socket to the task
(`serve_with_incoming_shutdown` over a `TcpListenerStream`). An occupied
port now panics loudly at startup — the same fail-loudly shape as REST.
Through v0.1.2 the bind happened inside the spawned task after the log
line, so two servers sharing a gRPC port produced a half-started process
that answered REST health checks under a false `listening` line (verified
live for this chapter; §13 entry 4, closed). Parse-time validation had
already closed the malformed-address route; the bind-before-spawn fix
closed the occupied-port route.

**Step 9: the drain.** `shutdown.await` waits for a stop signal — since
2026-08-17 that means Ctrl-C (SIGINT) **or SIGTERM** on unix, and Ctrl-C
or a console shutdown event on Windows. Until then only ctrl_c was
awaited, so Kubernetes and every other orchestrator that stops with SIGTERM never
triggered the drain and every rolling restart was a hard kill. The moment
the signal fires, a watcher flips `AppState::draining`, so BOTH planes'
`/readyz` answer 503 `draining` — load balancers stop routing here while
in-flight requests get up to `MUNARIUM_SHUTDOWN_GRACE_SECS` (default 20) to
finish. All listeners were spawned with graceful-shutdown futures watching
the same signal; the binary logs `shutdown signal received; draining` and
joins them under the grace timeout. The process exits even if a handler
gets stuck.

Here is an honesty note under the Preface's first commitment. These demos
ran under a non-interactive harness that cannot send a real console
Ctrl-C. The drain behavior is described from main.rs instead of captured.
The teardown below stopped the process by identity under §2's reaping
rule.

### A clean boot, annotated

The smallest configuration from §2 uses the alternate ports. It uses the
memory store, with full governance semantics and no persistence. gRPC is
off, and static dev auth is on:

```powershell
$env:MUNARIUM_HTTP_ADDR = '127.0.0.1:18080'
$env:MUNARIUM_GRPC_ADDR = 'disabled'
$env:MUNARIUM_OPS_ADDR  = '127.0.0.1:19090'
$env:MUNARIUM_STORE = 'memory'
$env:MUNARIUM_AUTH_MODE = 'static'
$env:MUNARIUM_STATIC_TOKENS = 'devtoken:dev-tenant:rw'
.\target\debug\munarium-server.exe
```

```text
2026-08-12T02:03:00.186815Z  INFO munarium_server: starting config.store=Memory http=127.0.0.1:18080 grpc=None
2026-08-12T02:03:00.187083Z  INFO munarium_server::state: source bytes store backend="mem"
2026-08-12T02:03:00.187126Z  INFO munarium_server::state: document intelligence disabled (MUNARIUM_DOCINTEL=none) — local extraction only
2026-08-12T02:03:00.189744Z  INFO munarium_server: REST plane listening addr=127.0.0.1:18080
2026-08-12T02:03:00.190105Z  INFO munarium_server: ops listening addr=127.0.0.1:19090
```

Each of these five lines confirms a startup step. `starting` means config
passed. It also shows the resolved store and addresses, which is the first
place to look when a server listens somewhere unexpected.

`source bytes store backend="mem"` means `build_source_store` resolved
(state.rs:266). In production, you want to see `backend="az"` here. This
line reveals a source store routed to the wrong place. The doc-intel line
states the *default* posture instead of leaving it silent.
Config.rs:391-397 explains why none is a complete configuration. The last
lines cover each listener that bound. There is no gRPC line because
`grpc=None` in the first line already explains why. The process began
serving in three milliseconds.

The ops plane answers on its own port:

```powershell
curl.exe http://127.0.0.1:19090/healthz
```

```text
ok
```

### The middleware spine: every /v1 request, instrumented

Between the router and every handler sits one axum middleware
(`middleware.rs::capture`, wired at rest.rs:1075-1080 as the router's
outermost layer). A tower twin handles gRPC. The header states the
contract (middleware.rs:1-16). REST buffers request and response bodies
and passes them through unchanged — with one exception since
2026-08-23: a `text/event-stream` response is never buffered; its body
is wrapped (`SseCapture`) and forwarded frame by frame, and the
interaction is recorded at end of stream from the handler's
`StreamOutcome` slot (§11 lists the rule beside the redaction rules; §17
shows the route that needed it and the buffered capture that exposed
it). It enforces the uid contract on `/v1`,
stamps a request id, opens the tracing span, and records the interaction.
Meta routes for health, version, docs, openapi, and healthai pass through
untouched.

**The request id, live.** Every response that reaches the capture span,
whether a success or failure, carries `x-munarium-request-id: req-<uuidv7>`
(middleware.rs:40-42, 137-141). Against the booted server, with the §2
dev token:

```powershell
curl.exe -si http://127.0.0.1:18080/v1/versions `
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1" `
  -H "Content-Type: application/json" -H "Idempotency-Key: ch5-demo-1" -d '{}'
```

```text
HTTP/1.1 200 OK
content-type: application/json
x-munarium-request-id: req-019ff3b5a41c7501bc903a43c9f76666
content-length: 54

{"version_id":"memv-b092812c614b48e58b3e216e11ba047c"}
```

The id is a time-ordered uuidv7, so sorted request ids show chronology.
The *middleware*, not the handler, stamps it. Errors therefore carry it
too, with one narrow exception. The middleware's own pre-span rejections
(uid-required, uid-mismatch, body-too-large) occur before the id is
created. §11's correlation section demonstrates this detail live.

The same call without `Content-Type: application/json`
(curl's `-d` defaults to form encoding) produced a problem+json 400
(`invalid-input`, via the `ProblemJson` extractor, rest.rs:24-41) with
`x-munarium-request-id: req-019ff3b586c870009a9ed819f8d87349` on the error
response. When a user reports a failure, the id in their error is the same
id in your tracing span (middleware.rs:124) and in the recorded
interaction row: one string correlates all three. §11 builds on this.

**The uid contract, in one place.** The middleware, not the handlers,
enforces `X-Munarium-Uid` (middleware.rs:71-89): missing uid with
`require_uid` (the default) is the typed `uid-required` 400 the
Introduction showed; a capability JWT's `sub` can stand in for the header
(the migration bridge), and a JWT whose `sub` *disagrees* with the
asserted header is rejected (`uid-mismatch`) before any handler runs.

Handlers downstream simply trust the `Uid` extension. The gRPC twin
(`GrpcCaptureLayer`, middleware.rs:175-310) mirrors the same arms over
`munarium-uid` metadata for `/mmp.v1.*` paths. Health and reflection are
exempt. Rejections use the same slugs carried as `ErrorInfo` details
(error.rs:4-10: clients never parse English).

**Interaction capture, and the 32 KiB cap.** After the handler responds,
the middleware creates an `InteractionRecord`. It includes tenant, uid,
request id, plane, method and path, status, latency, and optional context
for sessions, runbooks, or collections. Handlers attach this context with
response extensions. The middleware puts the record on a bounded channel
to one writer task without blocking (interactions.rs:69-114). If the
channel is full, it drops the record with a warning. Audit is best-effort
under overload *by design* because audit writes should not slow the data
plane.

Bodies are stored verbatim only when they are JSON and at most
`MUNARIUM_INTERACTION_BODY_MAX` bytes
(default 32768, config.rs:21-23). Above the cap, or for non-JSON data, the
row stores `{sha256, bytes_len}` instead (interactions.rs:12-27). A 200 MB
document upload then costs the audit table 100 bytes but remains
identifiable by content. Handlers whose responses carry secrets, such as
minted capability JWTs, set `redact_response`. The middleware stores a
redaction marker instead. This fulfills the "token material is NEVER
stored" contract (interactions.rs:49-59).

**The gRPC asymmetry is stated by the header itself:** "Proto bodies
are not captured. The REST plane is the full-body audit surface; gRPC
rows carry the envelope only" (middleware.rs:8-10). gRPC interactions
record method, uid, tenant, status, and latency. This provides attribution
without payload. If your compliance posture needs full-body capture, the REST
plane is the plane to put in front of users.

### config.rs is the env contract

At 806 lines, `config.rs` is the third-largest module, and nearly all of
it is one idea executed uniformly: **every environment variable is parsed
fail-closed, and every failure message names the cause AND the way out.**
The README's env table is the public face of this file, and the two are
maintained under one rule you will meet again as recipe 10. A new env var
lands in `Config`, the README table, and this guide's quick-ref *in the
same change*. This applies the Preface's third commitment to
configuration.

The exemplar is the `MUNARIUM_SOURCE_STORE=az` arm (config.rs:257-268),
worth quoting because it shows the reasoning, not just the rule:

> Fail closed, exactly like MUNARIUM_DATABASE_URL under MUNARIUM_STORE=postgres:
> a missing account must not degrade to a silent local-bytes fallback
> nobody notices until a restart. 'az' is usually the DEFAULT under
> MUNARIUM_STORE=postgres rather than something the operator typed, so the
> error must explain both how they got here and the way out. A bare
> "az requires an account" reads as nonsense on a laptop.

and the message it produces (whitespace collapsed): `source store is 'az'
(the default under MUNARIUM_STORE=postgres): set MUNARIUM_AZURE_STORAGE_ACCOUNT,
or set MUNARIUM_SOURCE_STORE=pg to keep document bytes in Postgres (local/CI
posture)`. Every arm of the file meets this standard. The S3 arm rejects
half a static credential rather than falling back to the ambient chain
("that fallback would sign with whatever identity the host happens to
have", config.rs:332-334).

The doc-intel arm defaults *off* because "a
system that quietly acquires [a paid, network-egressing dependency]
because a default said so is a system that surprises somebody with a bill
or a data-residency problem" (config.rs:391-397). When you add
configuration, imitate these arms. The §2 footnote about a laptop
`postgres` boot failing closed on a missing storage account is this
mechanism working as intended.

Secrets never appear inline. The one seam is `resolve_secret`
(config.rs:443-459): a reference is either an env-var *name* (a secret your
platform injects from its secret store as environment) or a `file:` path (a
Secrets Store CSI mount on Kubernetes); the function reads, trims, and rejects
empty. This is the same two-variant seam that the BYOK provider keys use.
Config fields hold *resolved* secret material or references, and the
recorded source URIs downstream never carry credentials (§4's
store-objects rule).

### The router is the map of the REST plane

`rest::router` has ninety-three `.route()` registrations, plus twenty-four
`/admin` console routes merged from `dashboard/mod.rs` (re-counted
2026-08-31 after the datastore merge and the Matrix S-packages landed the
storage and matrix surfaces; the 2026-08-27 recount said 72 + 22, and the
earlier "fifty-eight" predated authoring, bulk uploads, chronology
rules, and the streaming turn). Six are meta routes (`/healthz`,
`/readyz`, `/healthai`, `/version`, `/openapi.json`, `/docs`), and
eighty-seven are `/v1` paths.
Together, they read like a table of contents for the platform surface:
kernel commands and reads (versions, claims, events, promises, anchors,
counters, digests, head, facts, lineage, context), then shapes, sources,
ingest, indexes, search, providers, runbooks, sessions, access tokens,
reports (including the four 2026-08-17 dashboard views), and
collections. The `/admin` pages sit outside `/v1` and outside the
OpenAPI contract, like `/docs`.

When you need to know whether an operation exists on the REST plane and
which handler owns it, read the router instead of the docs. Then confirm that
the docs match. They are drift-checked, but the router is the truth from
which the check is derived.

Three registrations carry per-route body limits, and the two comments
beside them are the lesson (rest.rs:986-988, 1030-1032): axum's default
body cap is 2 MiB. That is correct for JSON commands but far too small for
document uploads. Therefore, `PUT /v1/sources` and the two ingest routes
layer `DefaultBodyLimit::max(MAX_SOURCE_BYTES)` (256 MiB). The batch comment
preserves the measured sting: base64 is 4/3 overhead, so under the
default "a whole BATCH was capped at ~1.5 MB of document bytes, which no
real corpus fits in." The pattern generalizes and recipe 1 encodes it:
default limits are per-route decisions, and a route that buffers bodies
documents its ceiling *at the registration site*, where the next reader
will actually see it.

The router ends by layering the capture middleware over the whole tree
(rest.rs:1075-1080). This lets you add a handler without also handling uid
enforcement, request ids, or audit. That is the crate's division of labor
in one line: handlers are thin
(auth → idempotency → `service.rs` → DTO, per rest.rs:1-2), and the
cross-cutting spine is nobody's per-route responsibility.

### The ops plane, honestly

What :9090 serves (since 2026-08-17; `ops.rs`) is the complete list:
`/healthz` (static ok), `/readyz` (the REAL store probe — the same
`AppState::store_ready` the REST twin calls, so the two planes cannot
disagree about readiness; until this change the ops readyz returned a
static `ok`, a lie to any orchestrator probe pointed at it), and
`/metrics` — Prometheus text exposition, hand-rolled in `metrics.rs`
with zero new crates. Captured from a fresh memory-store boot after two
meta requests:

```text
$ curl -s http://127.0.0.1:19094/metrics | head -9
# HELP munarium_build_info Build metadata; value is always 1.
# TYPE munarium_build_info gauge
munarium_build_info{version="0.1.2"} 1
# HELP munarium_interactions_queue_depth Interaction records waiting in the bounded writer channel.
# TYPE munarium_interactions_queue_depth gauge
munarium_interactions_queue_depth 0
# HELP munarium_http_requests_total Requests served, by plane, route template, method, and status class.
# TYPE munarium_http_requests_total counter
munarium_http_requests_total{plane="rest",route="/healthz",method="GET",status_class="2xx"} 2
```

The metric set: RED counters and latency histograms per plane and route
TEMPLATE (never the raw path — bounded cardinality), provider
call/latency/token counters, runbook step transitions, load-shed and
audit-writer counters, DB-pool and queue-depth gauges polled at render
time. Two rules in the module header are load-bearing: no tenant/uid
labels (per-tenant analytics belong to the interactions table and the
mgmt reports API), and no instance label (the scraper assigns `instance`
per target — the cluster-correct posture). There is still no OTel trace
export, by stated decision rather than gap: the exporter stack is a
dependency graph §9 would have to swallow, and nothing demands it yet
(architecture.md §12 records the posture; the old §13 entry 2 is
closed). §11 covers the rest of what you can observe (structured tracing
via `MUNARIUM_LOG`/`MUNARIUM_LOG_FORMAT=json`, request-id correlation,
interaction rows, the reports API, `/healthai`, and the `/admin`
dashboards).

Deployed environments probe the ops plane rather than
the data planes (§2's port landscape shows the topology), which is why
its bind-failure-is-a-warn asymmetry matters there: a platform health probe
hitting a warned-away ops listener is a deployment incident, not a crash.
The `starting` line and the missing `ops listening` line in the log make
the issue quick to diagnose.

After the demos above, teardown per §2: stop the process (by identity;
`Get-Process munarium-server` before `Stop-Process`, per the reaping rule),
clear every `MUNARIUM_*` variable, and verify 18080/19090 are free. All
three were done and verified for this chapter's runs.

You have now walked the binary end to end. You saw nine startup steps that
fail closed in the right order and three listeners with different failure
policies. You also saw a middleware spine that tracks every request, a
config module that treats error messages as documentation, and a router
that serves as its own map.

You are now ready for §6, the how-do-I recipes. There, "add an endpoint,"
"add an RPC," and "add an env var" become concrete diffs against the
exact files and seams named in this chapter.

## 6. How-do-I recipes (the heart of Part I)

This chapter is the reason the book exists. Its premise appears in the
Preface: **the recipes exist because each one encodes at least one mistake
that has already been made.** A recipe is not a tutorial for something you
could not figure out. It is the paved path around a known pothole. Examples
include a batch route silently capped at 1.5 MB and a blob store that used the
wrong metadata endpoint. Another example is a migration that compiled but
never ran.

Every recipe follows the same shape. It has numbered steps and a
**canonical existing example to copy**. Open that example instead of writing
from imagination. Each recipe also lists known gotchas and ends with a
checklist. The listed docs and tests must ship *in the same change*. This is
the Preface's third commitment, enforced by the CI gates in §10.

The recipes share one deep pattern, shown below and repeated at the end of
the chapter. A change in this codebase never affects only one file. The unit
of work includes the code, the registries that describe it, the generated
artifacts, and the tests that pin its behavior.

![One change, many surfaces: a single route, RPC, migration or slug connects to the rest.rs router, the grpc.rs implementations, openapi.rs, the generated API documents, the documentation registries, the README environment table, deny.toml, the conformance list and the crate tests](images/ch6-change-surface.svg)

This chapter uses two conventions. Line anchors refer to v0.1.2, as stated in
the Preface. The symbol names beside them remain useful when lines move.
Where a step produces output, the output shown is real. This applies to the
two regeneration commands, the migration listing, and the conformance
wrapper. The output was captured from this tree on 2026-08-11.

### Recipe 1: Add a REST endpoint

**Canonical examples:** `rest.rs::create_version` (rest.rs:194-210) for
the minimal shape; `rest.rs::propose_claim` (rest.rs:212-251) for one
with a documented error response. Read both before writing anything.

**Steps.**

1. **DTOs first, in munarium-api-types.** Request and response types live in
   the shared DTO crate, never inline in the server. The same types feed
   the OpenAPI document and the Rust client. JSON casing is decided here
   (§4's gotcha), and every new type derives `ToSchema` so utoipa can
   describe it.
2. **Write the handler, annotated.** The annotation *is* the API
   documentation source. The OpenAPI drift check derives from it:

   ```rust
   #[utoipa::path(post, path = "/v1/versions", request_body = dto::CreateVersionRequest,
       responses((status = 200, body = dto::CreateVersionResponse)), tag = "command")]
   async fn create_version(
       State(state): State<Arc<AppState>>,
       headers: HeaderMap,
       ProblemJson(req): ProblemJson<dto::CreateVersionRequest>,
   ) -> ApiResult<axum::response::Response> {
       let (ctx, store) = auth(&state, &headers).await?;
       let hash = idem_body_hash(&req)?;
       with_idempotency(&state, &ctx, &headers, hash, || async move {
           let id = store
               .create_version(req.parent_version_id.as_deref(), req.metadata.clone())
               .await?;
           Ok(json_value(&dto::CreateVersionResponse { version_id: id }))
       })
       .await
   }
   ```

   Note three things you get by copying rather than composing: the
   `ProblemJson` extractor (rest.rs:24-41), whose rejection is a
   problem+json `invalid-input` instead of axum's plain-text default; the
   `auth` helper doing token → tenant store resolution; and the error
   type `ApiResult`, which leaves the process as problem+json without any
   per-handler ceremony. When the endpoint can fail in a way worth
   documenting, declare it. `propose_claim` declares its 409:

   ```rust
   #[utoipa::path(post, path = "/v1/versions/{version_id}/claims",
       request_body = dto::ProposeClaimRequest,
       responses((status = 200, body = dto::ProposeClaimResponse),
                 (status = 409, body = dto::Problem)), tag = "command")]
   ```

3. **Commands go through `with_idempotency`** (rest.rs:145-175), with no
   exceptions. It enforces `require_rw` and requires the `Idempotency-Key`
   header. On a repeat, it replays a stored response. The following comment
   is worth learning because this code prevents cross-plane data corruption:

   ```rust
   // Plane-namespaced hash: REST stores JSON, gRPC stores hex-encoded proto
   // in the SAME table. Reusing one Idempotency-Key across planes must
   // surface as idempotency-mismatch, never decode the other plane's bytes.
   let body_hash = format!("rest:{body_hash}");
   ```

   Queries skip all of this: no idempotency, no rw requirement, and (by
   convention from §5) an `as_of_seq` parameter for point-in-time reads.
4. **Register the route in `rest::router`** (rest.rs:946-1082). The
   router is the map of the REST plane; a handler not registered here
   does not exist. If your handler buffers request bodies, decide its
   ceiling *at the registration site* with a per-route
   `DefaultBodyLimit` and a comment saying why. See the gotcha below.
5. **Register the path and any new DTOs in `openapi.rs`.** Two lists: the
   handler goes into `paths(...)` (openapi.rs:78-134), every new DTO into
   `components(schemas(...))` (openapi.rs:135-227). You do *not* declare
   auth or the uid header per route. The `SecurityAddon` and
   `UidHeaderAddon` modifiers (openapi.rs:14-68) apply bearer auth and
   the required `X-Munarium-Uid` parameter to every `/v1` path once,
   across all routes. Meta routes opt out with `security(())`.
6. **Regenerate the committed OpenAPI document.** From `server/`:

   ```
   cargo run -q -p munarium-server -- openapi > docs/api/openapi.json
   ```

   The following output proves the parsed-JSON comparison that CI runs
   (server-ci.yml:131-139), executed here on a clean checkout:

   ```
   drift: NONE
   paths: 93 schemas: 179
   ```

   On a clean tree the regeneration is a no-op, which is the property the
   whole scheme rests on: the file is *generated, never hand-edited*
   (§12), so after your change it differs by exactly your endpoint.
7. **Update the registries in the same change:** the route map in
   [docs/api/rest.md](../api/rest.md) (its "Route map" table), and, if
   the endpoint mints a new error slug, recipe 4's registry,
   [docs/api/errors.md](../api/errors.md).
8. **Tests at the tier that can see it** (§3): a service-layer unit test
   for the logic, and black-box coverage if the behavior is part of the
   platform surface. Recipe 9 shows where scenarios go.

**Gotchas.**

- **axum's default body cap is 2 MiB, and it bit the batch route.**
  Correct for JSON commands, absurd for uploads: under the default,
  base64's 4/3 overhead meant a whole ingest batch was capped at ~1.5 MB
  of document bytes, "which no real corpus fits in" (the comment at
  rest.rs:1030-1036 preserves the sting; `PUT /v1/sources` carries the
  same treatment at rest.rs:984-990, with `MAX_SOURCE_BYTES` = 256 MiB,
  rest.rs:22). Default limits are per-route decisions, documented where
  the next reader will look.
- **A handler missing from `openapi.rs` compiles, routes, and serves, but
  is invisible.** The generated document silently lacks it. Clients
  generated from the spec can't call it, and the deploy workflow's
  live-vs-repo path-count comparison (server-ci.yml:293-294) is the last
  tripwire. The router and the `paths(...)` list must move together.
- **Do not reach for `Json<T>`.** The `ProblemJson` wrapper exists
  because axum's default rejection is plain text, which breaks the
  errors.md contract that *every* error body is problem+json.

**Checklist:** openapi.json regenerated (diff shows only your endpoint) ·
rest.md route map row · errors.md row if a new slug · tests at the right
tier · one change.

### Recipe 2: Add a gRPC RPC

**Canonical example:** the `AppendEvents` pair. It includes the RPC in
[proto/mmp/v1/command.proto](../../proto/mmp/v1/command.proto) and the
tonic impl `grpc.rs::append_events` (grpc.rs:224-273), which is the REST
handler's twin down to the service call.

**Steps.**

1. **Edit the proto, additively.** MMP is the normative contract
   (Introduction), so the proto edit is the real API change. Follow
   protobuf's permanent rules. New RPCs and fields get fresh field numbers.
   Never renumber, reuse, or repurpose them. A new RPC joins its
   service block:

   ```proto
   service CommandService {
     rpc CreateVersion(CreateVersionRequest) returns (CreateVersionResponse);
     rpc ProposeClaim(ProposeClaimRequest) returns (ProposeClaimResponse);
     rpc AppendEvents(AppendEventsRequest) returns (AppendEventsResponse);
     ...
   }
   ```

2. **If you added a new `.proto` file, list it in
   `munarium-proto/build.rs`**. The file array (`"common"`, `"ledger"`,
   `"command"`, ... build.rs:8-21) is explicit. Each listed file also
   gets a `cargo:rerun-if-changed` line. A new file you forget to list is
   simply not compiled, and nothing warns.
3. **Write the tonic impl calling the SAME `service.rs` function REST
   calls.** This is the parity invariant made mechanical: `grpc.rs` (the
   kernel surface) and `grpc_data.rs` (shapes/ingest/retrieval) convert
   proto ↔ DTO at the boundary and delegate. The conversions live *once*
   in `munarium_api_types::wire`, shared with every Rust gRPC client
   (grpc.rs:123-127), if you find yourself writing a field-by-field
   mapping inside the server, stop. Put it there instead.
4. **Commands get the idempotency twin** in the same table but a different namespace
   (grpc.rs:90-121). Where REST stores JSON, gRPC stores hex-encoded
   proto bytes and replays by decoding:

   ```rust
   // Plane-namespaced hash — see rest::with_idempotency: one key reused
   // across planes is a mismatch, never a cross-format decode.
   let req_hash = format!("grpc:{req_hash}");
   ```

   The request hash is `request_hash(&inner.encode_to_vec())`. It uses the
   encoded proto and mirrors REST's serialized-DTO hash.
5. **Register the service in `main.rs`** if you added a whole new
   service; a new rpc on an existing service needs nothing there.
6. **Regenerate the gRPC reference.** No external `protoc-gen-doc`: the
   generator decodes the descriptor set already embedded in munarium-proto
   (with source comments) and emits byte-stable markdown:

   ```
   cargo run -q -p munarium-proto --bin gen-grpc-docs -- docs/api/grpc-reference.md
   ```

   Proven on this tree (generated to scratch, then diffed against the
   committed file, exactly as CI does at server-ci.yml:140-144):

   ```
   GRPC-DOCS DRIFT: NONE
   ```

7. **Update the parity ledger.** The "Plane parity notes" section of
   [docs/api/grpc.md](../api/grpc.md) records which REST surfaces have
   gRPC twins and which don't. If your change closes or opens a gap, the
   ledger says so in the same change.
8. **Run conformance with both planes.** `--http` plus `--grpc` *is*
   the cross-plane parity check (recipe 9, §7).

**Gotchas.**

- **The honesty rule is load-bearing here: unshipped RPCs answer
  `UNIMPLEMENTED`, never fake success.** The pattern has a worked example
  at the file level. `admin.proto` is declared but not served, and the
  generated reference says so in bold rather than pretending
  (gen-grpc-docs.rs renders it "**Reserved, declared, not served.**").
  A stub that returns `Ok` with empty fields is a parity bug you shipped
  on purpose.
- **Cross-plane key reuse.** The `grpc:`/`rest:` namespacing above exists
  because both planes share one idempotency table; without it, a client
  that reused a key across transports would get the *other plane's bytes
  decoded as its own format*. Keep the namespace prefix if you ever touch
  this code.
- **The generated reference is byte-stable by design** (LF endings, no
  BOM). Regenerate on Windows, and the diff is still clean. Do not
  "improve" its formatting by hand; CI diffs it verbatim.

**Checklist:** proto additive · build.rs list if a new file ·
grpc-reference.md regenerated · grpc.md parity ledger · conformance run
with `--http --grpc` · one change.

### Recipe 3: Add a migration

**Canonical examples:** `0001_tenants.sql` for the discipline header,
`0015_storage_backend_vocab.sql` for the never-edit rule made concrete.

**Steps.**

1. **Take the next number.** The real listing, captured from this tree
   (`Get-ChildItem src/munarium-store-pg/migrations -Name`, last three):

   ```
   0029_token_budgets.sql
   0030_index_version_deactivation.sql
   0031_max_tokens_budgets.sql
   ```

   Your file is `0032_<what_it_does>.sql`.
2. **Write additive-only DDL.** The very first migration opens with the
   contract:

   ```sql
   -- Additive-only migration discipline: new tables, nullable/defaulted columns,
   -- new partitions. Never destructive DDL against the ledger (CI-enforced).
   ```

   "CI-enforced" is literal: server-ci.yml:166-170 greps the migrations
   directory for `DROP TABLE`/`DROP COLUMN`/`ALTER TABLE ... DROP` and
   fails the build if any appears. No destructive DDL may *ever* enter
   the set.
3. **Force the crate to recompile. This is the full stale-embed gotcha.**
   This is the one §2 previewed and §3's triage table pointed here for,
   so here are the complete mechanics. `munarium-store-pg` embeds its
   migration set with `sqlx::migrate!("./migrations")` **at compile
   time**. Adding a `.sql` file does not touch Rust source. Cargo may see the
   crate as clean and reuse the previous build, which contains the old
   embedded set. The result is confusing. The server starts cleanly and logs
   nothing unusual, but your new table does not exist. Make the crate rebuild:

   ```
   cargo clean -p munarium-store-pg     # or: touch src/munarium-store-pg/src/lib.rs
   cargo build -p munarium-server
   ```

   Migrations run on connect, so the next boot applies your file.
4. **Never edit an applied migration.** sqlx records a checksum for each
   applied file. Editing a file breaks the checksum on every database that
   already ran it. The server then refuses to start with
   `migration N was previously applied but has been modified`. During a
   deployment, the new pods never become ready, and the old ones keep
   serving (§10). The worked example of doing it
   right is `0015`, a **comment-only migration** that exists precisely
   because the comment it wanted to change lived in an applied file:

   ```sql
   -- The storage_backend vocabulary widened when the object_store adapter
   -- (munarium-store-objects) replaced the hand-rolled Azure crate and added the
   -- S3 / GCS / local-filesystem backends. The column stays free TEXT; this
   -- comment is the one place the vocabulary is recorded in the schema.
   -- (0006 could not be edited in place: sqlx checksums applied migrations.)
   COMMENT ON COLUMN sources.storage_backend IS 'az | pg | mem | s3 | gcs | file';
   ```

   Even a *comment* change gets a new number. That is the rule stated at
   its sharpest.
5. **Recover when an in-place edit has already happened.** Such an edit
   strands every database that applied the old bytes. For your local compose
   database, `docker compose down -v` drops the `pgdata` volume and the next
   boot applies the set from zero; `gates.ps1` recreates its own database
   every run for the same reason (§10). For any shared database the recovery
   is the same in kind — drop and recreate, or restore from before the edit
   — which is why the answer is permanent: an additive migration, never an
   edit. That is the practice that `0015` rehearses.

**Gotchas.** Both of the big ones are above, because for migrations the
gotchas *are* the recipe: the stale embed (step 3) and the checksum trap
(step 4). One more from §8's territory: if your migration introduces or
widens a stored vocabulary such as `storage_backend`, the schema comment
is its registry. Update it the `0015` way.

**Checklist:** next number, additive-only · crate recompiled and the
change observed in a real database (`psql` on 5433, §8) · schema comment
if a vocabulary changed · the conformance pg mode still green (recipe 9)
· one change.

### Recipe 4: Add an error slug

**Canonical example:** [error.rs](../../src/munarium-server/src/error.rs).
The whole file implements the registry, and it has two distinct
paths depending on what kind of error you are adding.

**Path A: a new kernel error (`KernelError` variant).** This is rare because it means the
*protocol* gained a failure mode. Three matches must change together,
all in error.rs:

1. `slug()` (error.rs:174-188), the cross-transport key:
   `KernelError::HeadConflict { .. } => "head-conflict"` and friends.
2. `to_problem()` (error.rs:190-238), HTTP status + title, plus any
   extension members (`expected`/`actual` on head-conflict, `gate_findings`
   on policy-rejection, `kind`/`id` on not-found).
3. `to_status()` (error.rs:276-341), the tonic code plus the same
   extensions as gRPC metadata. The comment states the contract:

   ```rust
   // Metadata keys use the SAME names as the REST problem+json extension
   // members (expected, actual, gate_findings, shape_ref, kind, id) so a
   // client's error mapping is one table across both transports.
   ```

   A slug whose gRPC metadata invents new key names has broken the
   one-table promise every client is built on.

**Path B: a server-layer problem (`CustomError`).** This is the common case.
Examples include uid contract failures, token lifecycle issues, and policy
refusals. These are not kernel errors, but they live in the same registry
with their own slugs. Add a constructor. There are eight examples
(error.rs:56-141), and the shape is uniform:

```rust
pub fn scope_missing(scope: &str) -> Self {
    Self {
        slug: "scope-missing",
        status: StatusCode::FORBIDDEN,
        code: tonic::Code::PermissionDenied,
        title: "scope missing",
        detail: format!("the access token does not carry the '{scope}' scope"),
    }
}
```

`CustomError::to_problem` and `to_status` then do the transport mapping for
free. The slug rides as the problem `type` suffix on REST and as the
`ErrorInfo.reason` (domain `mmp.ioka.io`) on gRPC.

**Then, for either path:**

4. **New extension members go on `Problem` in munarium-api-types.** The
   DTO is shared with clients and the OpenAPI schema, so this triggers
   recipe 1's regeneration step (captured output there; same command).
5. **Register the row in [docs/api/errors.md](../api/errors.md).** Include the
   slug, HTTP status, gRPC code, meaning, and extensions. The registry is
   the client-facing contract. An unregistered slug is folklore.

**Gotchas.**

- **Clients key on slugs, never English.** `detail` strings are for
  humans and may be reworded freely; the slug and the extension-member
  names are frozen API. Review any diff that touches them accordingly.
- **gRPC error details ride an HTTP/2 trailer, and trailers have a size
  budget.** The `gate_findings` arm (error.rs:301-327) is the worked
  example. Peers commonly enforce 8–16 KiB header-list limits. The
  trailer carries base64 of the encoded Status (~4/3 inflation), so the
  raw findings JSON is budgeted at 4 KiB, with `findings_total` always
  carrying the real count and `findings_truncated` marking a capped
  list. If your new slug carries a list-shaped extension, copy that
  treatment. An unbounded extension is a working feature until the first
  large corpus produces a trailer the peer rejects.
- **Registering ahead of emitting is allowed, but silently emitting ahead
  of registering is not.** errors.md carries `overloaded` (503) marked
  "reserved: … not yet emitted": the honesty rule applied to the error
  registry.

**Checklist:** three matches (kernel) or one constructor (custom) ·
`Problem` extensions + OpenAPI regen if members changed · errors.md row ·
a test asserting slug + status (both transports if kernel) · one change.

### Recipe 5: Add a provider adapter

**Canonical example:** [munarium-providers/src/lib.rs](../../src/munarium-providers/src/lib.rs)
from end to end. It is one file. The three existing families are Anthropic,
OpenAI, and OpenRouter. OpenRouter is a thin variation of the OpenAI shape.
Together, they show every seam you must fill.

**Steps.**

1. **Implement `ModelProvider`** for the new family. It takes the
   endpoint override and a `CredentialRef`: *where the key lives, never
   the key itself* (lib.rs:184-189: env-var name or `file:` path, the
   same two-variant seam as recipe 10's secrets).
2. **Add the `build_provider` arm** (lib.rs:601-609):

   ```rust
   pub fn build_provider(doc: &ProviderConfigDoc) -> Box<dyn ModelProvider> {
       let endpoint = doc.spec.endpoint.as_deref();
       let cred = doc.spec.credential_ref.clone();
       match doc.spec.provider.as_str() {
           "anthropic" => Box::new(AnthropicProvider::new(endpoint, cred)),
           "openrouter" => Box::new(OpenAiProvider::openrouter(endpoint, cred)),
           _ => Box::new(OpenAiProvider::new(endpoint, cred)),
       }
   }
   ```

   And note what feeds it: `parse_provider_config` *rejects* unknown
   family names (the test at lib.rs:631-632 asserts that renaming
   `anthropic` to `watsonx` fails to parse). Your family must also join
   the parse allow-list, or no config document can ever name it.
3. **Fill the tier and default tables:** a `builtin_tier_model` entry
   for both tiers. See lib.rs:106-116; `("anthropic", ModelTier::Fast) =>
   Some("claude-haiku-4-5")` is the shape), a `default_env_var` entry
   (the conventional `MUNARIUM_SECRET_<FAMILY>` name your secret store surfaces),
   If the family should take part in the default-provider rule and
   `/healthai`, also add a slot in `DEFAULT_PROVIDER_PRIORITY` (lib.rs:102).
4. **Let the coverage test fail you honest.**
   `tier_and_default_tables_cover_all_families` (lib.rs:674-686) walks
   `DEFAULT_PROVIDER_PRIORITY` and asserts every family has both tier
   models, a default env var, and a synthesizable default config. Add
   the family to the priority list and forget a table, and the test
   names exactly what is missing. This is the "coverage test fails if you
   forget" rule from the chapter map. It has already caught this omission.
5. **`/healthai` comes along for free but must be verified live:** it
   probes the built-in default models of every family in
   `DEFAULT_PROVIDER_PRIORITY` (providers_api.rs:424-431), skipping
   families with no credential. One call uses real tokens. This is §11's BYOK
   diagnostic. Its free sibling, `GET /v1/providers` (2026-08-23;
   `list_providers`, providers_api.rs:654-686), walks the same priority
   list through `default_config_doc`, so a new family appears in the
   disclosure listing — name, `credential_ok`, and the concrete `fast` /
   `capable` models its tier table resolves to — the moment step 3's
   tables are filled. (Since 2026-09-01 the table has a third row,
   `frontier`, so the disclosure lists three models per config and
   `/healthai` probes nine; each probe's ceiling is the tenant's
   `healthai_probe` budget, [docs/tokenbudgets.md](../tokenbudgets.md).)
   Check it keyless: the family must show up with
   `credential_ok: false` and both tier models populated; a missing
   `builtin_tier_model` entry shows as an absent `fast`/`capable`
   member, which is the listing's honest way of saying the tier cannot
   resolve.
6. **Update the tier table** in [docs/api/rest.md](../api/rest.md)
   ("Provider selection: default rule, tiers, and /healthai") in the
   same change.

**Gotchas.**

- **Never persist or log key material.** Provider errors carry
  `endpoint_fingerprint`, a truncated sha-256 of the endpoint
  (lib.rs:596-598). This makes failures clear without ever
  writing a URL that might embed credentials. Copy that pattern.
- **Respect the tier resolution order** (lib.rs:151-182): explicit
  model > config tier override > built-in tier default > first
  configured model > built-in capable. `tier_resolution_order`
  (lib.rs:636-671) pins it. Extend that test rather than reasoning from
  memory.
- **Budgets (rpm/tpm) and retry-after honor are part of the adapter
  contract**, not optional politeness. The config doc's `budgets` block
  must mean something in your impl.

**Checklist:** parse allow-list + `build_provider` arm + tier/env/default
tables · coverage test green · rest.md tier table · contract tests
against recorded fixtures (the pattern) · one change.

### Recipe 6: Add a source-store backend

**Canonical example:** the S3 path from end to end. It is the newest
full-size backend and every seam is annotated: the config arm
(config.rs:295-361), the state factory arm (state.rs:173-189), and the
object_store constructor (munarium-store-objects). This recipe also owns
the freshest war story in the book.

**Steps.**

1. **Implement the byte store, almost certainly as a new
   `ObjectSourceStore` constructor in munarium-store-objects**, not a new
   crate. Every cloud and filesystem backend rides the `object_store`
   adapter (state.rs:142-145), and the crate's job is exactly to turn a
   validated config into a store plus a `backend_id` and a recorded-URI
   prefix that never carries credentials (§4's rule).
2. **Add the `SourceStoreConfig` variant and the `source_store_from_env`
   arm** (config.rs:249-389). Make it fail closed with a message
   that names both the cause and the way out. §5 quoted the `az` arm's
   reasoning; the S3 arm is the exemplar for credential handling:

   ```rust
   // Half a static credential is a misconfiguration, not a signal
   // to fall back to the ambient chain — that fallback would sign
   // with whatever identity the host happens to have.
   let access_key_id = std::env::var("MUNARIUM_S3_ACCESS_KEY_ID").ok();
   let secret_ref = std::env::var("MUNARIUM_S3_SECRET_KEY_REF").ok();
   let (access_key_id, secret_access_key) = match (access_key_id, secret_ref) {
       (Some(id), Some(reference)) => (Some(id), Some(resolve_secret(&reference)?)),
       (None, None) => (None, None),
       (Some(_), None) => return Err("MUNARIUM_S3_ACCESS_KEY_ID is set without \
           MUNARIUM_S3_SECRET_KEY_REF, static credentials need both, or neither \
           for the ambient AWS chain".to_string()),
       ...
   };
   ```

   Both-or-neither, secrets through `resolve_secret` (env name or
   `file:` path), with *deliberate* ambiguity nowhere. Even the region
   fallback explains itself (an S3-compatible endpoint implies a
   placeholder region; real AWS without one is a config bug,
   config.rs:301-316).
3. **Add the `state.rs::build_source_store` arm** (state.rs:146-212).
   This is mechanical: map the config variant to the constructor.
4. **Register the vocabulary, as a migration.** `storage_backend` is
   free TEXT whose vocabulary lives in one schema comment, and that
   comment sits in an applied file. A new backend id therefore means a new
   `00NN` comment-only migration in the `0015` mold (recipe 3), never an
   edit.
5. **Docs in the same change:** README env-table rows for every new
   `MUNARIUM_*` variable (README.md:93 onward) and a section in
   [docs/guides/source-stores.md](source-stores.md).
6. **Tests:** the S3 integration test's doc header is itself the
   runnable recipe (s3_integration.rs:1-14, per §4). It uses MinIO through the
   compose profile, no cloud account needed. Give your backend the
   equivalent.

**Gotchas.**

- **The 2026-08-11 IDENTITY_ENDPOINT incident** explains why this
  recipe says *copy the constructor, do not write one*. The Azure
  constructor carries the scar tissue in place
  (munarium-store-objects/src/lib.rs:185-191):

  ```rust
  // MUST be from_env(), not new(): Container Apps / App Service have no
  // classic IMDS at 169.254.169.254 — the platform injects
  // IDENTITY_ENDPOINT (+ IDENTITY_HEADER, read per-request by
  // object_store's credential provider) and from_env() is what picks
  // the endpoint up. With new() the managed-identity path black-holes
  // against link-local IMDS and every blob call times out (live
  // incident, 2026-08-11 dev smoke).
  ```

  The failure shape is the worst kind: config validates, the server
  boots, and health is green. The *first blob call* then hangs until timeout,
  in a deployed environment only, because a laptop never exercises
  managed identity. If your backend's SDK has a `from_env`-style
  constructor that picks up platform-injected credentials, use it. Also
  say why in a comment this explicit.
- **The feature combo is not negotiable.** object_store's stock cloud
  features hard-wire `aws-lc-rs`, which needs cmake. It is absent from the
  alpine musl builder. The working combination (`-base` features +
  `ring` + a process-level ring provider installed in the crate
  constructors. That is what `ensure_rustls_provider()` at the top of
  each constructor does. This setup is §9's case study. Read it there before
  touching munarium-store-objects' `Cargo.toml`, rather than rediscovering
  it against a red Docker build.

**Checklist:** constructor + config arm (fail-closed) + state arm ·
vocabulary migration · README env rows + source-stores.md · integration
test with a local emulator where one exists · one change.

### Recipe 7: Add an extractor

**Canonical example:** `PdfTextExtractor` (munarium-extract/src/pdf.rs). It shows
the trait implementation, registration, and dependency story in miniature.

**Steps.**

1. **Implement `TextExtractor`** (munarium-extract/src/lib.rs:128-132):

   ```rust
   pub trait TextExtractor: Send + Sync {
       fn media_types(&self) -> &[&str];
       fn extract(&self, bytes: &[u8]) -> Result<Extracted>;
       fn id(&self) -> &'static str;
   }
   ```

   `Extracted` carries text plus `PageSpan`s, which are byte ranges for each
   page or block. Downstream retrieval cites these provenance locations, so
   populate them honestly or not at all.
2. **Register it.** `ExtractorRegistry::new()` ships DOCX and PDF text
   layers. OCR arrives through `with_ocr` behind the `ocr` feature. Yours
   goes through `registry.register(Box::new(...))` (lib.rs:163-165).
3. **Understand what `version()` does for you** (lib.rs:167-173): every
   extractor id, sorted, joins the string that feeds **index identity**.
   This means "an extractor change forces a rebuild rather than serving stale
   chunks." Adding your extractor invalidates existing index versions by
   construction. That is correct and intended (§8's index-identity
   list). Bump `EXTRACTOR_SET_VERSION` when an *existing* extractor's
   behavior changes, since its id alone won't.

**Gotchas.**

- **Never fail the batch.** The registry's `extract` "never panick[s]
  and never fail[s] the caller: a bad document is recorded as `failed`
  and the build moves on" (lib.rs:175-178). Your `extract` may return
  `Err`, but if it can panic on hostile bytes you have broken the
  batch-ingest rule. Fuzz-shaped inputs are table stakes: this code
  parses caller-uploaded files at index time.
- **Pure Rust only.** The musl static build excludes every native
  engine outright. pdfium, poppler, and mupdf are all non-starters. The
  deny.toml ttf-parser block says so while accepting its own risk. If
  the format needs a native library, the answer is the document
  intelligence seam (`MUNARIUM_DOCINTEL`, a paid external service,
  default *off*), not a C dependency.
- **Must clear cargo-deny**, including its license allow-list, advisories,
  and bans.
  A new parsing crate is exactly the profile that trips advisories; if
  you must ignore one, recipe 11 → §9's rules: written justification
  with re-evaluate triggers, in deny.toml itself.

**Checklist:** trait impl + registration · unit tests incl. a corrupt
input recorded as `failed` · `cargo deny check` clean · docs touch in
[docs/guides/loading-corpora.md](loading-corpora.md) if the supported
formats list changes · one change.

### Recipe 8: Add a shape, a runbook, or a runbook step kind

The first two are **data, not code** (§4's rule). A shape is YAML that you
`POST /v1/shapes`, and a runbook is YAML that you `POST /v1/runbooks`. They
need no Rust or recompilation. Part II (§15) teaches how to author them.
What lands *here* in Part I is the machinery around them: validation
rules and step kinds.

**Canonical examples:** the `steps.cutover-before-verify` rule in
munarium-runbooks/src/validate.rs for a validation rule; the `Cutover`
step kind (with its approval field, `name()` arm, and two executor
arms) for a step kind.

**Adding a validation rule** (munarium-runbooks/src/validate.rs):

1. Emit findings through the one helper, with a **stable dotted code**.
   Codes are API surface (`ValidationFindingDto` crosses the wire from
   `POST /v1/runbooks/validate`), messages are prose:

   ```rust
   out.push(finding(
       Severity::Warn,
       "steps.cutover-before-verify",
       "cutover precedes verify — the index goes live before verification".into(),
       format!("spec.steps[{c}]"),
   ));
   ```

   Follow the severity rules in the existing set. `Error` blocks (`is_valid`),
   as in `steps.verify-before-build` and related rules. Use `Warn` for legal-but-suspect
   (above); `Info` for worth-knowing (`steps.cutover-unapproved`, no
   human in the loop).
2. The suggestion layer "layers on top of these findings; it never
   replaces them" (validate.rs:3). Do not move a deterministic check
   into the model-assisted layer.

**Adding a step kind** (munarium-runbooks/src/lib.rs + runbooks_api.rs):

1. Add a variant to `StepSpec` (lib.rs:283-301). It uses camelCase serialization, so
   the YAML name is decided by the variant name:

   ```rust
   pub enum StepSpec {
       ResolveSources {},
       BuildIndex {},      // side-by-side build; NEVER activates (cutover does that)
       Verify {},
       Cutover { approval: Option<String> },
       RetireOld { keep_versions: u32 },
   }
   ```

2. Add its `name()` arm (lib.rs:308-316). This string is used by the
   validator's ordering rules and the run reports.
3. **Add both executor arms in runbooks_api.rs:** the v2
   per-collection arm (~runbooks_api.rs:329) and the v1 arm
   (~runbooks_api.rs:389). The matches are exhaustive, so the compiler
   walks you to each site. This is a rare gotcha that defends itself.
4. Ordering/approval semantics belong in validate.rs as rules with
   codes (step 1 above), not in prose.

**Gotchas.**

- **The compiler defends the executor arms; only the samples test
  defends everything else.** `munarium-runbooks/tests/sample_runbooks.rs`
  walks every YAML under `server/runbooks/` and fails CI on a parse
  error or an Error-severity finding. Its header states the reason
  better than a style guide could:

  ```rust
  //! They are documentation people copy, so a broken one is worse than none —
  //! and the `sources.prefix-mismatch` rule only earns its keep if something
  //! actually runs it over the set.
  ```

  It also asserts `n >= 14`, so *deleting* samples is noticed too. A
  rule that no shipped sample exercises has never actually run. When you
  add a step kind or a rule, add or update the sample that makes it
  "earn its keep."

**Checklist:** rule code registered in the validate.rs set (grep-able,
stable) · both executor arms (compiler-enforced) · a shipped sample
exercising it · `POST /v1/runbooks/validate` round-trip test · one
change.

### Recipe 9: Add a conformance scenario

**Canonical examples:** any tuple in `conformance/src/lib.rs::run_all`
for the kernel; `platform.rs::uid_contract` for the black-box surface.
§7 covers what the existing twelve scenarios prove; this recipe is only
the mechanics of adding a thirteenth.

**Kernel scenario.**

1. Write `async fn my_scenario(store: &dyn StorageBackend) ->
   ScenarioResult` against the *trait*. That is the entire trick. One
   scenario body runs in-process, on Postgres, and over live REST and
   gRPC through the client adapters. The last two modes together form the
   cross-plane parity check.
2. Assert with the `expect!` macro (lib.rs:21-25). A failed condition
   returns a formatted message, so a red scenario names *what* diverged,
   not just that something did:

   ```rust
   macro_rules! expect {
       ($cond:expr, $($msg:tt)+) => {
           if !$cond { return Err(format!($($msg)+)); }
       };
   }
   ```

3. Add the tuple to `run_all` (lib.rs:27-28). The comment is the whole
   registration ceremony:

   ```rust
   /// All scenarios, name -> future. Add here; every backend runs the full set.
   pub async fn run_all(store: &dyn StorageBackend) -> Vec<(&'static str, ScenarioResult)> {
   ```

   That one line buys all four kernel run modes (conformance/src/main.rs:3-6):
   `--in-process`, `--postgres <url>` (fresh tenant per run),
   `--http <base> [--token T]`, and `--grpc <endpoint>`. No further wiring is needed.
   (The fifth mode, `--platform`, runs the separate suite below.)
4. The fast feedback loop is the cargo wrapper,
   `conformance/tests/in_process.rs`, which runs every scenario as one
   assertion against the in-memory backend. Captured from this tree:

   ```
   test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
   ```

   `cargo test -p mmp-conformance` takes 0.03 seconds. Run it before every push
   (CI runs it regardless, as tier two of §3's ladder).

**Platform scenario** (the platform surface): black-box REST only.
Collections need Postgres, and the concerns are cross-cutting (uid,
tokens, compartments). Write `async fn my_scenario(env: &PlatformEnv)
-> R` using the `send` helper (method, path, token, uid, JSON or YAML
body, platform.rs:39-58), add the tuple to `platform::run_all`
(platform.rs:693), and honor the environment contract in the module
header: a **fresh tenant**, rw + mgmt static tokens,
`MUNARIUM_TOKEN_SECRET` set, and **zero provider keys**. Nothing in the
platform suite may trigger a completion.

**Gotchas.**

- **Fresh-tenant discipline everywhere.** Scenarios assume they own
  their tenant's ledger. A scenario that would pass only on an empty
  store must create its own version lineage, not assert global state.
- **Write the scenario against the semantics, not a backend's
  incidental behavior.** If it passes in-process and fails on pg, you
  have found either a real conformance bug (good, that is the point) or
  an assumption about ordering/timing that the spec does not make.

**Checklist:** tuple in the right `run_all` · wrapper test green
locally · for behavior changes, follow §3's rule: the scenario that pins the
old behavior must fail before your fix and pass after · one change.

### Recipe 10: Add an env var

The shortest recipe with the strictest rule, met twice already (§2's
laptop-boot footnote, §5's config walkthrough): **a new env var lands in
`Config`, in the README table, and in this guide's quick-ref (Appendix
B), in the same change.**

**Canonical example:** the `file` arm of `source_store_from_env`
(config.rs:376-384), quoted in step 1. It has no convenient default and uses a
fail-closed message naming the way out.

**Steps.**

1. **Parse it in config.rs and fail closed.** Use a message that names the
   cause *and* the way out. §5 quoted the `az` arm's reasoning at
   length. The `file` arm shows the other half of the philosophy: the
   refusal of convenient defaults:

   ```rust
   // No default directory on purpose: a silent /tmp fallback is
   // precisely the local-bytes surprise the az arm refuses above.
   let root = std::env::var("MUNARIUM_FILE_ROOT").map_err(|_| {
       "MUNARIUM_SOURCE_STORE=file requires MUNARIUM_FILE_ROOT (the directory \
        document bytes live under)".to_string()
   })?;
   ```

   Enumerated values reject unknowns by name
   (`"MUNARIUM_SOURCE_STORE must be az|pg|mem|s3|gcs|file, got '{other}'"`);
   booleans reject anything but `true|false`/`1|0`; secrets go through
   `resolve_secret` (env-name or `file:` reference, config.rs:443-459)
   and never sit in a config field as inline material. Config errors
   exit 2, before any listener binds (§5's startup order).
2. **README table row** ("Configuration (env vars)", README.md:93
   onward): name, default, one-line meaning including what it fails
   closed against.
3. **Appendix B quick-ref row** in this guide.
4. **Add a config unit test.** Existing config.rs tests set and clear variables
   around each case. Follow them. They are also why config parsing is
   testable without booting anything).

**Gotcha:** the temptation is always the silent default. The config file's
arms exist to argue with you. The doc-intel arm defaults *off*
because "a system that quietly acquires [a paid, network-egressing
dependency] because a default said so is a system that surprises
somebody with a bill or a data-residency problem" (config.rs:391-397).
If a variable's default changes behavior that someone pays for, the default
is off. This includes money, egress, and storage residency.

**Checklist:** parse + fail-closed message · README row · Appendix B
row · unit test · one change. (CI cannot grep prose tables; reviewers
enforce this one.)

### Recipe 11: Add a dependency

This recipe is short because **§9 owns this topic**. Read all of §9 first.

**Canonical example:** the ttf-parser advisory ignore (deny.toml:82-96)
shows the written-justification pattern every exception follows. The shape
of the checklist you will find in §9:

1. Pin the version once, in the root `Cargo.toml`
   `[workspace.dependencies]`. Member crates reference it.
2. Set `default-features = false`, then add back only what you need.
   Prefer rustls and pure-Rust features. `openssl`, `openssl-sys`, and
   `native-tls` are hard-banned in deny.toml (deny.toml:99-106). The
   musl case study in §9 (the object_store/aws-lc-rs/ring story recipe 6
   cross-referenced) is what happens when a transitive default sneaks
   native code into the static build.
3. `cargo deny check` locally. License must be on the allow-list; a new
   **workspace crate** means a new `[[licenses.exceptions]]` block for
   its license marker. deny.toml:23-25 is the two-line example. If you
   forget it, the deny job tells you instead of the compiler.
4. An advisory ignore is never bare: it requires a written justification
   block *in deny.toml* with explicit re-evaluation triggers. The
   ttf-parser entry (deny.toml:82-96) is the example. It even names
   the condition that "flips this from acceptable to blocking."
5. The Docker/musl build is part of the test: a dependency that builds
   on your Windows box and dies in the alpine builder is not added yet.

**Checklist:** workspace pin · features minimized · deny clean (with any
exception/ignore written down where CI reads it) · musl build green ·
one change, and §9 for everything this summary compressed.

### The pattern of patterns

Look back across all eleven recipes. The same closing points appear in each
one. This is the real lesson of the chapter:

1. **Regenerate what is generated.** openapi.json and grpc-reference.md
   are outputs, never inputs. On a clean tree, regeneration is a no-op
   (both proven above: `drift: NONE`, `GRPC-DOCS DRIFT: NONE`). After
   your change, the output differs by exactly your change.
2. **Update the registry the same hour you update the code.** The route
   map, the slug table, the tier table, the env table, the parity
   ledger, the schema's vocabulary comment, and deny.toml's exceptions.
   Every subsystem here keeps a human-readable registry. A registry
   that lags its code is worse than none because it is *believed*.
3. **Extend the tests at the tier that can see the change** (§3, §7).
   Prefer registration points that make coverage automatic: one
   tuple in `run_all`, one walk of the samples directory, one coverage
   test over the provider tables.
4. **Ship it as one change.** Do not use "code now, docs in a follow-up."
   That follow-up is the lie the Preface's third commitment exists to
   forbid, and §12 makes it a review standard.

CI (§10) checks generated files for drift, scans migrations, runs the
conformance ladder, and gates the dependency graph. But CI is the
backstop, not the process. The process is the recipe, and the recipe is
short precisely because somebody already paid for the long version.


## 7. Testing and conformance in depth

Section 3 taught you to *run* the conformance suite. It is woven through all
four rungs of the ladder. You have already watched the same seven PASS lines
scroll by on four different substrates.

This chapter explains what those lines mean. It covers why the suite has its
current design and what each green scenario proves. It also explains what the
platform suite adds and requires from its environment. Finally, it shows
how the harness's black-box mode doubles as a development tool against any
running server. This chapter
does not teach the ladder again. It explains what the ladder is made of.
Every report in this chapter was executed
against this working tree (v0.1.2), per the Preface's first commitment.

### One scenario set, four execution contexts

The load-bearing decision is stated in the first four lines of
`conformance/src/lib.rs`, and it is worth reading as a design document:

> MMP conformance scenarios, written against the `StorageBackend` trait so
> the SAME assertions run on the in-memory store, the Postgres store
>, and, through the client adapters, the running server's REST and
> gRPC planes (platform, with cross-plane parity diffing). (lib.rs:1-4)

A scenario is an `async fn` that takes `&dyn StorageBackend` and returns
`Result<(), String>`. That trait is the kernel's storage contract (§4). A
scenario therefore states a rule about *semantics* without choosing a
*substrate*. The store object that the runner provides selects the substrate:

- **In-process**: `MemStore` from `munarium-store-mem`. No server, no Docker,
  no network. This is tier 1's conformance leg and the
  `cargo test -p mmp-conformance` wrapper.
- **Postgres**: `PgStore` from `munarium-store-pg`, connected to a real
  database with a fresh tenant per run. Tier 2's conformance leg.
- **REST black-box**: `RestClientStore`, the same trait, implemented over
  a *running server's* REST plane.
- **gRPC black-box**: `GrpcClientStore`, the same trait again, over the
  direct tonic plane.

The last two live in `conformance/src/clients.rs`, whose header contains
the sentence this whole architecture exists to earn:

> The SAME scenario set runs against both, which IS the cross-plane parity
> check. Any semantic divergence between planes fails a scenario on one
> side. (clients.rs:2-4)

Dwell on that "IS." Parity between REST and gRPC is not a separate test
suite that someone must keep in sync with the feature set. It is a
*consequence* of running one scenario set through two adapters. There is no
separate parity test to forget.

When a new kernel scenario lands in `run_all`, it becomes a parity check for
free because both planes must pass it. If the planes disagree about anything
the scenario touches, one goes red and names the difference. Section 1 calls
this the parity doctrine. This file makes the doctrine executable.

![One scenario set in five contexts: in-process over MemStore, in-process over PgStore with a fresh tenant, the REST and gRPC client adapters, and a fifth pg-backed platform context. The paired adapter run is the cross-plane parity check](images/ch7-conformance-contexts.svg)

Two details in the adapters deserve attention. First, the black-box
stores translate wire errors *back into the kernel's typed errors*. The
REST adapter maps problem+json slugs onto `KernelError` variants (reading
the head-conflict numbers from the problem's structured
`expected`/`actual` fields), and the gRPC adapter's `status_to_err` maps
tonic codes. `parse_head_conflict` (clients.rs:23-28) literally
re-parses the human-readable status message, `"head conflict: expected
seq X, actual Y"`, to recover the numbers the typed error carries,
because a tonic status has no structured fields to read.

As a result, the error taxonomy is *inside* the conformance surface. Rename
the `head-conflict` slug in `rest.rs` and `ledger.append-head-conflict`
fails in `--http` mode, because the adapter falls through to a generic
storage error instead of the `HeadConflict` the scenario matches on. The
contract is not "some 4xx happened." It is the exact slug and, on the
gRPC side, the exact detail grammar that §6's recipe 4 registers. Second, the adapters
stamp every call with the conformance uid (`CONFORMANCE_UID`,
clients.rs:17). The black-box modes run under the uid contract like
any other caller, so a regression in uid handling fails conformance before
it fails a user.

Assertions inside a scenario use the `expect!` macro, quoted in full in
recipe 9 of §6. The key property is that a failed
condition returns a *formatted message*, so a red scenario names what
diverged, `"claim superseded after the pin must read as current at the
pin (got [\"blue\"])"`, not merely that something did. Registration
is one tuple in `run_all`, whose doc comment is the entire ceremony:
"All scenarios, name -> future. Add here; every backend runs the full
set." (lib.rs:27).

### The eight kernel scenarios, annotated

Executed for this chapter, and annotated with what each PASS line is
actually claiming (§2 and §3 showed the unannotated originals):

```text
$ cargo run -q -p mmp-conformance -- --in-process
MMP conformance — in-process (munarium-store-mem)
--------------------------------------------------------
  PASS  ledger.append-head-conflict     # stale expected_head → typed conflict; head unmoved
  PASS  ledger.supersession-pin         # correction wins at head; pin still reads the original
  PASS  pins.one-pin-bounds-all-stores  # promises+anchors+counters all bounded by one as_of_seq
  PASS  gates.block-records-disputed    # blocked ≠ dropped: disputed recorded, canon untouched
  PASS  composer.budget-degradation     # budget met by degrading digests before trimming facts
  PASS  digests.rebuilt-under-pin       # stored head digests never served under a pin
  PASS  gates.chronology-certain-only   # "circa" draws no violation; certain lateness fires, with chain
  PASS ledger.origin-round-trips # a connector claim's origin survives on every backend
--------------------------------------------------------
8 passed, 0 failed
```

The same invocation with
`--postgres postgres://munarium:munarium-dev@localhost:5433/munarium` (the compose
Postgres, §2) was executed immediately after and produced the identical
seven-PASS report under the label `postgres (munarium-store-pg)`. It has the
same semantics on a different substrate, which is the point of the design.
The sections below explain what each scenario does and proves. They also show
which of the Introduction's five invariants it guards.

**`ledger.append-head-conflict`** (lib.rs:61-83). This scenario appends one
fact with `expected_head = Some(0)` and checks that it receives seq 1. It then
appends a second fact with the now-stale `expected_head = Some(0)`. The result
must be a typed `KernelError::HeadConflict`, and the head must remain *1*.

**Proves:** optimistic concurrency on the lineage clock is real. A stale
write is rejected rather than reordered, and it consumes no sequence number.
**Guards invariants 1 and 2:** the
append-only ledger admits no lost-update path, and the single sequence is
the thing the conflict is detected against. In `--http` mode this scenario
additionally proves the wire answer is the exact 409 `head-conflict`
problem slug. Nothing else maps back to the `HeadConflict` the scenario
matches on, per the adapter subtlety above. (That the problem also carries
usable expected/actual values is visible in §11's captured 409, not
asserted here.)

**`ledger.supersession-pin`** (lib.rs:85-140). This scenario writes
`hero.eyes=green` in a version, then adds a *correction* to `blue` in a
**child** version. It checks two things. The head read of the child sees one
`eyes` fact valued `blue`, proving that supersession crosses the lineage
boundary. A read pinned at seq 2, before the correction, sees `green` as
current *at the pin*. The second check can pass only if the superseded record
was never edited to get there. **Proves:**
supersession is resolved at read time, per pin, over an untouched history.
**Guards invariants 1 and 3:** a correction is a new record naming its
victim, and what "current" means is a function of where you pin.

**`pins.one-pin-bounds-all-stores`** (lib.rs:142-251). The file's own
header calls the pin fixtures "the crown jewels: they encode the
one-pin-bounds-everything semantic that defines the mesh," and this is the
jewel. It places five kinds of state on one clock: a fact (seq 1), promise
registration (seq 2), anchor lock (seq 3), counter (seq 4), another fact
(seq 5), and promise fulfillment (seq 6).

The scenario then checks three views. Pin 1 hides the promise, anchor, and
counter. Pin 2 shows the promise as **open**, even though it is fulfilled at
head, while the anchor and counter stay hidden. The unpinned head shows both
the anchor and the fulfilled promise.

**Proves:** invariant 2 makes invariant 3 possible. Every store draws from
the same sequence, so one number bounds them all
coherently, promises included. This is the scenario that answers the
Introduction's March question, and if you change anything near
`load_snapshot` or the seq-stamping of the non-ledger stores, this is the
line to watch.

**`gates.block-records-disputed`** (lib.rs:253-324). This scenario replays the
accept path in miniature. It seeds canon with `hero.eyes=green`, loads a
snapshot, and runs the gates against a candidate that proposes `eyes=blue`.
It checks that `gate.ledger-conflict` fires at `Severity::Block`. It then
does what the server's write path does and appends the blocked claim with
`status = Disputed`.

The accepted slice must still read `green`, while a disputed-status slice
must contain the recorded `blue` claim. **Proves:**
invariant 4 end to end. Nothing is lost, nothing unvetted becomes canon,
and *disputed is data, not an exception*, the property the Introduction
said reshapes application UX, here reduced to three `expect!`s.

**`composer.budget-degradation`** (lib.rs:326-365). This scenario writes
twenty facts across two scopes and composes the full brief. It then composes
again with a token budget just under the full size. It checks that the budget holds and
that the "Accepted facts" section still contains **all twenty** lines:
"digests must degrade BEFORE facts trim." **Proves:** the composer's
degradation order is a contract, not a heuristic. Under pressure, the brief
loses digest *resolution* first; it sheds a canonical fact only as the
last resort. **Guards the composer clause of invariant 3:** the brief is a
deterministic function of the snapshot and the budget, so the context your
application composed under pressure is exactly reproducible and
exactly predictable.

**`digests.rebuilt-under-pin`** (lib.rs:453-488). This scenario writes two
facts at seqs 1 and 2. It builds and *stores* the digest ladder at head, whose
rungs mention the seq-2 fact. It then loads a snapshot pinned at seq 1. No
tier-0 digest under the pin may contain the seq-2 content: "stored head
digests must never be served under a pin."

**Proves:** the Introduction's statement about digests. A stored digest
reflects the head, so pinned reads must *rebuild* digests from the
pinned facts rather than serving stale summaries that would leak the
future into the past. **Guards invariant 3** at its most easily botched
edge. It is possible to implement pins correctly for facts but still leak
post-pin content through a cached summary. This scenario
exists because that bug class is quiet.

**`gates.chronology-certain-only`** (lib.rs:367-451). This scenario seeds a
certain `filing_date` and an *uncertain* `birth_date` of `"circa 1950"`. It
then arms order and deadline rules. A candidate death date of 1940 *might*
conflict with a circa-1950 birth, so it must file **no order violation**. A
response date that is certainly beyond the 30-day deadline must fire
`gate.chronology-deadline`. Its detail must carry a `chain` of exactly two
events, which forms the full evidence path.

**Proves:** the gate fires only when interval algebra says a violation is
*certain*. When it fires, the finding is auditable rather than merely
red. **Guards invariant 4's** evidence half: findings name the rule and
carry the chain a reviewer can walk.

**`ledger.origin-round-trips`** (lib.rs:71-100). This scenario proposes a
fact carrying a `ClaimOrigin` — a connector's kind, source id, mapping
version, row key, event position, observation time, and evidence id — and
reads it back. Every field must survive byte for byte. **Proves:** origin is
a wire fact, not a documented convention. The same assertion runs against
mem, pg, and, through the client adapters, REST and gRPC, so "origin
round-trips" is checked on every backend rather than assumed from one.
**Guards** the connector provenance a proposed claim carries: a fact a
service files on a document's behalf must remain distinguishable from one a
human wrote, all the way to the row a reader resolves.

One honest note on the mapping: the eight kernel scenarios cover
invariants 1 through 4. **Invariant 5, retrieval provenance, has no
kernel scenario and cannot have one.** Collections require Postgres and a
real index build, which are outside the `StorageBackend` trait. Its
conformance home is the platform suite below (per-collection provenance
envelopes) plus the twelve `collections_integration` tests that §3's tier
2 un-skips. Stated so you do not go hunting through `lib.rs` for a
retrieval scenario that is deliberately somewhere else.

### Five of the ten platform scenarios, and their contract

The kernel scenarios test the memory machine. The platform suite
(`conformance/src/platform.rs`) tests the *platform around it*: the
platform surface of uids, roles, capability tokens, collections, sessions,
ingestion, removal, and reports. It uses only black-box REST. Its
environment contract appears in the module header, and every clause is
load-bearing:

> Runs against a live pg-backed server (collections need postgres). The
> caller supplies rw + mgmt static tokens for a FRESH tenant and the
> server must have MUNARIUM_TOKEN_SECRET configured. Zero provider keys
> required, nothing here triggers a completion. (platform.rs:5-8)

Postgres is required because collections and sessions live there. A fresh
tenant is required because the scenarios use fixed resource names
(`ent-support@1`, `entdocs@1`) and assume they own them. The token secret is
required because four of the five scenarios mint capability JWTs. Provider
keys must remain absent because a conformance gate that spent money would
stop being run. Every session turn in the suite uses retrieval only with the
keyless local embedder. Section 3's tier 4 satisfies this contract locally.
CI's platform step satisfies it
on 18081. Five of the ten, from reading the bodies — the ones that were part of the suite when this chapter was written, walked through in the depth the others get in their own doc comments:

- **`platform.uid-contract`** checks two identity failures. A `/v1` call
  with a valid token but no `X-Munarium-Uid` returns `400 uid-required`; the
  valid rw credential does not rescue it. A capability JWT minted for
  `uid-alice` but presented under uid `mallory` returns `403 uid-mismatch`.
  **Proves** the chain-of-custody floor. A missing identity outranks a valid
  credential, and a token
  cannot be borrowed across users.
- **`platform.role-partition`** checks both role boundaries. An rw token
  that tries to mint a capability token gets `403 forbidden`. A mgmt token
  that tries to write the ledger also gets `403 forbidden`. **Proves** the
  role split is a *partition*, not a hierarchy. Neither role contains the
  other, so a
  leaked ops credential cannot mint access and a leaked governance
  credential cannot forge memory.
- **`platform.application-and-compartments`** covers a full application
  lifecycle in one long scenario. It publishes a shape and validates a clean
  and invalid runbook. The invalid case uses `topK: 0`. It applies the
  runbook, ingests two files, and checks that filename matchers bind each file
  to its collection. It also confirms that a level-0 ingest token cannot
  write to the level-2 collection.

  The scenario runs five steps through **two** approval gates to `done`.
  Next, two capability tokens run **the same query** against the same
  runbook. The level-0 token can reach only the public collection. The
  level-2 token with `eng` can reach the secret collection. Each collection
  produces its own provenance envelope.

  A follow-on turn receives ordinal 2 on the same session. Alice cannot use
  Bob's session. A model override outside `allowOverrides` returns
  `403 override-not-allowed` before any provider spend. A query-scoped token
  that attempts ingest returns `403 scope-missing`. **Proves** invariant 5
  plus the entire access
  model: clearance filters retrieval before ranking, provenance arrives
  per collection, and every denial is a typed slug an application can
  switch on.
- **`platform.removal-double-pass`** confirms that removal takes two
  passes. Confirming a removal that was never requested returns
  `409 removal-not-confirmed`. Using the wrong `removal_id` returns `409`.
  The proper request-then-confirm flow succeeds. After that, sessions on the
  removed exact ref answer
  `410 runbook-removed`, the *bare name* still resolves to the live
  `@1`, and the removed version is hidden from the default listing but
  visible under `include_removed=true`. **Proves** removal is soft and
  double-pass, invariant 1 extended to the application surface: nothing
  is deleted; what changes is resolution.
- **`platform.reports-and-revoke`** checks reporting and revocation. An rw
  token gets `403` on reports. The mgmt usage report includes rows for
  `comp-alice` and `comp-bob`, the uids used by the previous scenario. The
  report is therefore checked against traffic from the suite rather than
  fixtures. The audit trail for `comp-bob` must not be empty. Revoking a
  new token must add `revoked_at` to the issuance audit. The response
  reports the server's `MUNARIUM_TOKEN_REVOCATION_CHECK` enforcement mode.
  **Proves** the governance surface watches the same traffic the data
  surface carries.


Five more joined the suite after this chapter was written, each with the
same depth of doc comment as the five above, in `platform.rs`:

- **`platform.discrepancy-findings`** — a service (Munarium Matrix, in
  production) files a warn-only finding on `POST /v1/versions/{id}/findings`
  with both evidence sides attached; a re-run of the same finding files
  nothing twice; a block-severity finding is refused; and the `findings`
  scope alone, with neither `query` nor `ingest`, is what authorizes it.
- **`platform.evidence-hierarchy`** — a turn that names a research
  profile reads governed layers in declared order, reports
  `supports_completeness: false` from a document layer rather than
  claiming a proof of completeness it cannot make, and, when a REQUIRED
  layer cannot be served, refuses the turn without naming that layer's
  sources in the refusal.
- **`platform.authoring-lifecycle`** — guided runbook authoring end to
  end and keyless: catalog to draft to answers to validate to assist
  (which must degrade to a note rather than fail, with zero provider
  keys) to a hash-verified export to apply, ending with the runbook and
  its collections existing — plus the refusal contract that a blank
  draft cannot export.
- **`platform.bulk-upload-lifecycle`** — a self-contained walk through
  bulk upload sessions: manifest diff, chunked upload with per-file sha
  verification, wholesale chunk-replay idempotency, finalize
  verification, and the zero-byte re-run.
- **`platform.admin-dashboards-render`** — the `/admin` dashboards
  render against a real pg-backed server rather than the memory store,
  which is the only tier that can catch a defect class where a handler
  panics instead of erroring; that is exactly how `/admin/storage` was
  first found dead (`SUM(bigint)` is `NUMERIC` in PostgreSQL, and every
  prior exercise of the page had run on the memory store).

Note the deliberate ordering dependency in that last pair:
`reports-and-revoke` asserts on uids that `application-and-compartments`
created. The platform suite is a *sequence*, not a set.
`platform::run_all` awaits each scenario in registration order while it
builds the result vector. The runner is also single-threaded, using
`#[tokio::main(flavor = "current_thread")]` at main.rs:47. Register a new scenario after the
traffic it depends on.

### Modes, flags, exit codes, tenants

The runner's doc header (conformance/src/main.rs:3-12) is the complete
CLI reference:

```text
--in-process              in-memory backend
--postgres <url>          munarium-store-pg, fresh tenant per run
--http <base> [--token T] black-box over the REST plane
--grpc <endpoint>         black-box over the direct gRPC plane
--platform <base> --rw-token T --mgmt-token M
                          platform REST scenarios … Needs a pg-backed server
                          with MUNARIUM_TOKEN_SECRET set and a FRESH tenant
                          behind the two tokens.
```

Modes combine freely. Any subset can run in one invocation. Each prints its
own labeled report, and failures are summed across all modes.
`--token` defaults to `devtoken`. Exit codes:

- **0** means every requested scenario passed.
- **1** means a scenario failed *or a substrate could not be reached*. A
  Postgres connect/migrate failure and a gRPC connect failure are each
  counted as a failure (main.rs:77-80, 94-97), not skipped. A conformance
  run that cannot reach its substrate is red, never vacuous. This is the
  opposite of §3's environment-gated integration tests. Those are optional
  extras, while this suite is the contract.
- **2** means a usage error: no mode was given, or `--platform` lacked
  `--rw-token`/`--mgmt-token`.

Fresh-tenant discipline appears at every level of the stack: the
`--postgres` mode mints `conf-<nanos>` per run (main.rs:71). Section 3's tier
3 boots its server with a random `bb-*` tenant, and tier 4 uses `ent-*`.
CI's steps use `ci-$GITHUB_RUN_ID` / `ent-$GITHUB_RUN_ID`. The
As a result, **no cleanup step exists because none is needed**. Postgres
persists, while the namespace refreshes. A scenario that would pass only on
an empty store is wrong. Recipe 9's first
gotcha says the same thing from the author's side.

Finally, the cargo wrapper: `conformance/tests/in_process.rs` runs every
kernel scenario as one assertion against `MemStore`. Plain
`cargo test --workspace`, and therefore tier 1, carries the suite
without anyone remembering to invoke the runner. It finishes in 0.03 s.

Recipe 9 in §6 explains how to add the thirteenth scenario. It covers the
`expect!` macro, the `run_all` tuple that enables all modes, and the
platform environment contract. Those details are not repeated here. The one
sentence worth repeating is its closing gotcha: write the scenario
against the semantics, not a backend's incidental behavior. If it passes
in-process and fails on pg, you have found either a real conformance bug
(good; that is the point) or an ordering assumption the spec does not
make.

### The harness as a development tool

The black-box mode is not only a CI step. `mmp-conformance` takes a base
URL and a token and runs the shared scenario set against whatever answers
there — a from-source binary, a compose stack, a cluster behind a gateway.
That makes it a three-second regression sweep after a change to REST
handlers, auth, or the problem taxonomy, needing nothing but a URL and a
token. The usage line is the whole CLI (conformance/src/main.rs):

```text
mmp-conformance --in-process | --postgres <url>
              | --http <base> [--grpc <endpoint>] [--token T]
              | --platform <base> --rw-token T --mgmt-token M
              | --cluster <baseA> --peer <baseB> [--token T]
```

Boot §2's rung-five server with the memory store and static `devtoken`.
Turn gRPC off, use alternate ports 18080/19090, and then point the harness
at it:

```powershell
# terminal 1 (the §2 recipe, binary pre-built)
$env:MUNARIUM_HTTP_ADDR = '127.0.0.1:18080'; $env:MUNARIUM_GRPC_ADDR = 'disabled'
$env:MUNARIUM_OPS_ADDR  = '127.0.0.1:19090'; $env:MUNARIUM_STORE = 'memory'
$env:MUNARIUM_AUTH_MODE = 'static'; $env:MUNARIUM_STATIC_TOKENS = 'devtoken:dev-tenant:rw'
.\target\debug\munarium-server.exe
# → REST plane listening addr=127.0.0.1:18080 / ops listening addr=127.0.0.1:19090

# terminal 2, from server/
cargo run -p mmp-conformance -- --http http://127.0.0.1:18080 --token devtoken
```

The report is the one §10's gate transcript shows for the REST plane:
`MMP conformance — REST plane (http://127.0.0.1:18080)`, one line per
scenario, and a `passed, 0 failed` tally. Read what that proves. Every
kernel scenario — an accepted claim, a conflicting claim **DISPUTED** with
`gate.ledger-conflict` named in the findings, canon holding the original
value, a pin bounding the slice, a stale `expected_head` answered with the
exact 409 `head-conflict` slug — passed against `MUNARIUM_STORE=memory`,
driven over plain HTTP by the REST adapter. That is the
storage-independence claim of the first half of this chapter, proven again
from the outside. Add `--grpc http://127.0.0.1:15051` when the direct gRPC
plane is on and both wires are checked in one run; the scheme is required,
because tonic accepts a scheme-less endpoint on Linux and rejects it on
Windows (gates.ps1:126-130).

The platform scenarios need more from their target: a Postgres-backed
store (collections do not exist on the memory store), an `rw` and a `mgmt`
static token, and `MUNARIUM_TOKEN_SECRET` set so capability JWTs can be
minted. Point `--platform` at the compose stack or at a server booted the
way tier 4 boots one (§3), never at a memory-mode target; the harness will
tell you which precondition is missing rather than pretend.

Two checks the harness does not make, and you should, because they compare
the *target* with *your checkout* rather than testing behavior:
`GET /version` against `[workspace.package].version` in `Cargo.toml`, and
the served `/openapi.json` path count against `docs/api/openapi.json`.
Against a from-source server they pass trivially. Against a deployment they
are the stale-deploy alarm: a package version can span two API contracts
between releases, so two different builds can report the same `/version`
while serving different surfaces. Run them *before* debugging "the server."
Three seconds tells you whether you are arguing with last week's image; §10
gives the two commands and the deployment rule they belong to.

Nothing here spends money. The harness never calls a provider, and the one
server path that can spend (`/healthai`, §11) is not in its scenario set.

Teardown, exactly as §2 taught and executed here: the server was stopped
by identity (process name checked before `Stop-Process`), the six
`MUNARIUM_*` variables removed, and ports 18080/19090 verified free. This avoids
surprises next week.

### What is tested where, the honest boundaries

The suite in this chapter covers the server in depth, but it is not the
whole testing story. Claiming otherwise would violate the
Preface's second commitment. The boundaries:

- **Client-library conformance lives in `clients/`, not here.** This
  suite proves the server's two planes agree with each other and with the
  spec. The client libraries
  carry their own harnesses and their own CI (`clients-ci.yml`) proving
  the Rust, Python,.NET, and Java libraries agree with the server over both
  transports. A client bug does not get a scenario in `lib.rs`.
- **Provider live smokes exist but never run here.** §3's gated-test
  section has the full story: since 2026-08-17 `munarium-providers`'
  contract tests carry one live smoke per family behind the double gate
  (`MUNARIUM_LIVE_PROVIDER_TESTS=1` plus the family's `MUNARIUM_SECRET_*`
  key). Nothing in the conformance suite or CI spends
  provider money — CI never sets the gate, and the one server path that
  can spend (`/healthai`) skips keyless, as the run above showed.
- **The gRPC half-start is closed at the source, not by a tier.** Through
  v0.1.2 a second server on an occupied gRPC port logged `listening`, then
  its spawned task panicked with `AddrInUse` while REST kept serving —
  and black-box conformance could catch it only if the port happened to be
  occupied during a run, which no tier arranges. The 2026-08-17 fix binds
  the listener inline before the spawn (§5's startup walk), so the failure
  is now a loud startup death no tier NEEDS to arrange (§13 entry 4,
  closed).
- **The platform suite's history is the argument for its CI step.** The
  step's comment in `server-ci.yml` (lines 80-84) is the honesty rule
  applied to CI itself: "The 2026-08-11 lesson: the platform suite
  existed only in test.ps1 -Platform, so a run-breaking resolveSources
  bug shipped to main with CI green. This step is the regression gate for
  the whole platform REST surface." A suite that runs only on the machines
  of people who remember it is a suite that will eventually miss the
  commit that matters. Section 10 maps where CI runs each mode. It runs
  in-process and pg in one step, then both black-box planes against pre-built
  binaries. The 60 ms bind-race lesson lives in that step's comment. It also
  runs platform mode on 18081. Section 3's ladder is the local mirror of
  all four.

That last mode is also the bridge. The `--postgres` run at the top of
this chapter proved that the storage *contract* holds on the real substrate:
eight scenarios, a fresh tenant, and no cleanup. Section 8 covers the substrate
itself: the additive-only migration discipline that keeps upgrade history
as append-only as the ledger it stores, how to open `psql` and inspect
what a conformance run actually wrote, why collections have no delete
API, and where document bytes really go.



## 8. Data and storage for developers

§7 ended on the observation that the pg conformance mode proves the storage
*contract*; this chapter is about the substrate the contract runs on. It is
deliberately not a DBA manual. You will not tune autovacuum here. It is
also not recipe §6.3, which covers the mechanics of adding a migration.
Instead, it gives you a mental model of where data lives and why.

This model comes from a real development database. Every query in this
chapter ran against the compose Postgres database already populated by
this tree's test tiers. Nothing below was composed from memory. The
transcripts are abridged but real, under the Preface's first commitment.

### The two-tier mental model

There are exactly two places durable data lives, and one row type that
bridges them.

**Tier one is PostgreSQL, the system of record.** Everything the platform
*asserts* is a Postgres row. This includes ledger events, memory versions,
the claims projection, index manifests, sessions, and the audit trail. The crate that
owns the kernel's share of it is `munarium-store-pg`, and its module header
([src/munarium-store-pg/src/lib.rs:1-23](../../src/munarium-store-pg/src/lib.rs))
is the storage contract in four paragraphs. The first is the one to
internalize: the semantics are "identical to `munarium-store-mem`. The
conformance suite runs the same scenarios against both, and reads resolve
through the SAME `munarium-core` reference functions (`ledger::resolve_slice`,
`promises::status_as_of`), so the two backends cannot drift apart." Postgres
is not where the *semantics* live; `munarium-core` is. Postgres is where the
rows live. The write path makes appending them safe.

`append_claim` opens a transaction and takes `SELECT ... FOR UPDATE` on
the lineage root's `lineage_heads` row. The header calls this the
Postgres equivalent of SQLite's `BEGIN IMMEDIATE`. One lock row
serializes all writers per lineage. The transaction computes the chain
head inside the lock, enforces `expected_head`, and inserts the ledger
event and claims-projection row together. When §3's `-Postgres` tier was
"actually racing four writers against `lineage_heads` FOR UPDATE," this
is the code it raced.

Two more facts from that header shape everything downstream. First is
tenancy. A `PgStore` handle is scoped to one tenant at construction, so
"every query carries the tenant predicate by construction." The demo uses
one database with a `tenant_id` column. Production uses a database per
tenant and cell. It swaps the pool router above the type, not the queries.

Second, migrations run **on connect**. `PgStore::connect` calls
`sqlx::migrate!("./migrations").run(&pool)` before it returns
(lib.rs:56-59), which is why a fresh compose database is ready without a
setup step, and why the checksum failure in §2's almanac manifests as a
server that never becomes ready.

**Tier two is the object store, and it holds bytes only.** Document content
never has a home in a Postgres column of the `sources` table, even in
the dev profile. Bytes go wherever `MUNARIUM_SOURCE_STORE` points: `az`, `s3`,
`gcs`, or `file` through the `munarium-store-objects` adapter, `pg` through a
dedicated `source_blobs` table, `mem` in-process. The platform's opinion
about *which* is operational, not architectural. Every backend implements
the same `SourceStore` trait and lays bytes out at the same logical path,
`{tenant}/{filename}`.

**The seam between the tiers is one row.** Every ingested document gets a
`sources` row recording `filename` (identity), `content_hash` (integrity),
`storage_backend` (which tier-two backend has the bytes), and `blob_uri`
(where, credential-free and test-enforced, per
[source-stores.md](source-stores.md)). The row is the bridge: Postgres
always knows where the bytes went without knowing how to get in.

![The two data tiers: PostgreSQL table families for the kernel, retrieval and platform surfaces beside the object store's six backends, bridged by the sources row's storage_backend and blob_uri columns. Index versions are immutable slabs and cutover moves one pointer](images/ch8-data-tiers.svg)

### Migrations are the schema's own ledger

The schema's history is now thirty-one files; the fifteen below are a
captured snapshot from when this chapter was written, kept because the
eras they group into (kernel, retrieval, configuration, the platform
wave) are still the right way to read the set, and reading fifteen files
that way teaches the discipline as well as reading thirty-one would. Files
0016 onward continue the same discipline one era further (identity and
gate findings, chronology rules, authoring, bulk uploads, evidence, the
datastore tier's artifact catalog and build jobs, token budgets) — read
the directory itself for the current, complete list
(`Get-ChildItem src/munarium-store-pg/migrations -Name`):

```
0001_tenants.sql
0002_ledger.sql
0003_claims.sql
0004_anchors_promises_counters_digests.sql
0005_idempotency.sql
0006_retrieval.sql
0007_shapes.sql
0008_providers.sql
0009_runbooks.sql
0010_identity_interactions.sql
0011_collections.sql
0012_runbook_lifecycle.sql
0013_sessions.sql
0014_interactions_tenant_created.sql
0015_storage_backend_vocab.sql
```

Era by era:

- **0001–0005 are the kernel.** Tenants; the ledger tier (`0002` creates
  `memory_versions` with the denormalized `lineage_root_id` "so the whole
  chain shares one seq-allocation lock row," the `lineage_heads` mutex
  table, and `ledger_events` itself, which is range-partitioned on a
  `tenant_seq` identity column "so indexes stay small and cold partitions
  archive trivially"); the claims projection (`0003`, whose header carries
  the projection philosophy in one line: "derived-but-transactional
  (written in the same transaction as the ledger event), regenerable from
  `ledger_events`. Follow *rebuild, don't migrate*"); the seq-stamped satellite
  stores (`0004`: "every store carries a seq stamp from the one lineage
  counting domain, so a single `as_of_seq` pin bounds facts, anchors,
  promises, and counters together," which expresses the pin invariant as DDL);
  and idempotency replay (`0005`).
- **0006 is retrieval**, the largest single file: `sources` (with the
  filename-is-identity, hash-is-integrity commentary §16 will lean on),
  `source_blobs`, `index_versions`, `index_chunks`, and
  `CREATE EXTENSION vector`. This is why compose runs
  `pgvector/pgvector:pg16` rather than stock Postgres.
- **0007–0009 are configuration**: published shapes ("a shape_ref, once
  published, never changes content... new content = new version"),
  provider configs (`0008`'s header is the BYOK posture in one sentence:
  "credentialRef ONLY. Key material never enters this table, the ledger,
  or any log line"), and runbooks with durable, resumable executions.
- **0010–0014 are the platform wave**: the token-issuance audit
  and interaction capture (`0010`: "Token material is NEVER stored. Only
  the jti and the claims needed for audit/reporting"), collections and the
  partitioned chunk store (`0011`, next section), runbook lifecycle
  (`0012`, whose removal design is soft-transition columns and whose header
  ends "No DDL here ever deletes"), sessions (`0013`: a session pins the
  runbook name@version and snapshots the token's access level at creation,
  "so a mid-session runbook upgrade or token change never changes what an
  ongoing conversation can see"), and one index (`0014`) whose header
  explains precisely which audit-page scan none of the `0010` indexes could
  serve.
- **0015 is the exemplar**: a comment-only migration, quoted in full in
  recipe §6.3, existing because the `storage_backend` vocabulary comment it
  wanted to update lived in an applied file and "0006 could not be edited
  in place: sqlx checksums applied migrations."

Read as a set, the files express a discipline the very first line of `0001`
states outright: "Additive-only migration discipline: new tables,
nullable/defaulted columns, new partitions. Never destructive DDL against
the ledger (CI-enforced)." The enforcement is a grep in `server-ci.yml`
(lines 125-129) that fails the build on `DROP TABLE`/`DROP COLUMN`/`ALTER
TABLE ... DROP` anywhere in the directory. The result is a pleasing
symmetry you should keep intact: **the schema's history is append-only,
exactly like the ledger it stores.** A migration is never edited, never
reordered, never deleted; corrections are new numbered files, the way a
ledger correction is a new event.

Even the applied set is itself a table, `_sqlx_migrations`, with checksums
and all. It reports every version applied so far, thirty-one today, and its
descriptions match the current filenames, fifteen more of which than the
listing above. When you want to know what
schema a database *actually* has, that table outranks any document,
including this one.

This discipline has several operational hazards. A compile-time stale
embed can keep a new file from running without warning. Editing an applied
file creates a checksum trap. The recovery path is to drop and
recreate the database (`docker compose down -v` for local compose). These are §2's almanac
entries, while recipe §6.3 gives the steps for doing it right. This
chapter adds only the reason. Those hazards are the *cost* of a schema
history you can trust, and the project pays it on purpose.

### Opening the database: a guided tour

The compose Postgres publishes on host port 5433 (§2's port map), so two
doors work: inside the container or from any host psql:

```powershell
docker compose exec postgres psql -U munarium munarium
# or, from the host (password munarium-dev, per docker-compose.yml):
psql "postgres://munarium:munarium-dev@localhost:5433/munarium"
```

Inspecting is always safe: Postgres MVCC means a psql reader never blocks
the server's writers, so browsing while a conformance run or a live server
is mid-flight costs nothing. Mutating is a different matter. The no-delete
discipline the API enforces on callers applies to you at the psql prompt
too, doubly so because dev databases accumulate history the test tiers
rely on comparing against.

`\dt` on this tree's development database returned 74 relations. Abridged
to the families (the full list is the three migration eras made visible,
plus partitions):

```
 public | claims                                               | table
 public | collection_chunks                                    | partitioned table
 public | collection_chunks_p_019ff2038edd78a1ae74ddadb7e35849 | table
   ... (45 collection_chunks partitions in this database) ...
 public | ledger_events                                        | partitioned table
 public | ledger_events_default                                | table
 public | ledger_events_p0                                     | table
 public | lineage_heads                                        | table
 public | memory_versions                                      | table
 public | sources                                              | table
 public | source_blobs                                         | table
 public | index_versions                                       | table
 public | sessions / session_turns / interactions / runbooks ...
 public | _sqlx_migrations                                     | table
```

Notice two things before any query. Both partitioned tables are visible
as themselves *plus* their partitions. `ledger_events` has its first
range partition `ledger_events_p0` and the catch-all
`ledger_events_default` (both created by `0002`, which also notes that
partition maintenance is an mmctl command), `collection_chunks` with one
LIST partition per collection ever created here. And this database is
*dirty on purpose*: the pg conformance mode creates a fresh tenant per run
and cleans nothing
(§7), so weeks of test tiers have left 51 tenants. These include `conf-*` conformance
tenants, `ent-*`/`local-m13` from the platform suite, single-purpose
tenants from targeted integration tests. That residue is exactly what makes
the following queries show something.

**First query: where are the lineages, and how long are they?**
`lineage_heads` is the smallest interesting table. It has one row per lineage
root, holding the lock and the current head seq (columns per `0002`:
`lineage_root_id`, `current_seq`):

```sql
SELECT tenant_id, lineage_root_id, current_seq
  FROM lineage_heads ORDER BY current_seq DESC LIMIT 5;
```
```
       tenant_id       |            lineage_root_id            | current_seq
-----------------------+---------------------------------------+-------------
 local-m13             | memv-652d29b8401c4c609cdc1ec7398752b5 |          20
 local-m13             | memv-22a3beca09b44d80a485ca0b6b3250fb |          20
 conf-18cad2974d32c0b8 | memv-0eff539d17d14f51b704a55b84076c44 |          20
 conf-18caeb3c49d84754 | memv-9ae4bb5bdd0146be8796dcdbad92a675 |          20
 conf-18caeb5e04abbfe0 | memv-b7a9be3f42a346b89b560e4cd906aa57 |          20
```

Every conformance run drove its lineage to seq 20. The scenario set and
shape are the same, but the tenants differ. This is determinism visible as
data.

**Second query: the claims projection, and what `status` means.** The
projection carries the row-per-claim view the ledger events imply
(`rebuild, don't migrate`), and its `status` column is where governance
becomes queryable:

```sql
SELECT status, count(*) FROM claims GROUP BY status;
```
```
  status  | count
----------+-------
 accepted |   207
 disputed |     6
```
```sql
SELECT tenant_id, subject, key, value, status
  FROM claims WHERE status = 'disputed' LIMIT 3;
```
```
       tenant_id       | subject | key  | value |  status
-----------------------+---------+------+-------+----------
 conf-18cad2974d32c0b8 | hero    | eyes | blue  | disputed
 local-m13             | hero    | eyes | blue  | disputed
 local-m13             | hero    | eyes | blue  | disputed
```

Those six disputed rows are the conformance suite's planted contradiction,
the hero's eyes from §7's scenario walk. It is stored just as a real
contradiction would be. The claim is not deleted or overwritten. It is
*marked*, with the full row set preserved for whatever judges it next.
The set includes `claim_type`, `provenance`, `supersedes_id`, `evidence`,
and `shape_ref`; see `\d claims`.

**Third query: the seam row.** The `sources` table on this database has
real rows because integration tests ingested documents:

```sql
SELECT source_id, filename, storage_backend, blob_uri, extraction_status
  FROM sources LIMIT 5;
```
```
      source_id       |   filename    | storage_backend |                          blob_uri                          | extraction_status
----------------------+---------------+-----------------+------------------------------------------------------------+-------------------
 src-4619301bfb4345e0 | kb/article.md | pg              | pg://source_blobs/update-18cad296aad1bb94/kb/article.md    |
 src-cc9f3d4558cb0949 | fda/scan.pdf  | pg              | pg://source_blobs/nodocintel-18cad296aad7b288/fda/scan.pdf | failed
 src-185bed78d4b3375b | fda/scan.pdf  | pg              | pg://source_blobs/difail-18cad296aad21698/fda/scan.pdf     | failed
 src-776724a95f9d1cc5 | fda/ok.md     | pg              | pg://source_blobs/difail-18cad296aad21698/fda/ok.md        | ok
 src-936102c26b259d8a | eng/launch.md | pg              | pg://source_blobs/ent-m13-local/eng/launch.md              | ok
```

One row tells the whole tier-two story: the backend is `pg` (compose
default), the URI is credential-free and encodes the logical path
(`{tenant}/{filename}` under the `pg://source_blobs/` scheme).
`extraction_status` is also honest. Scanned PDFs that could not yield a
text layer sit at `failed`. Their tenants, `nodocintel-*` and `difail-*`,
are the docintel failure-path tests. Markdown sits at `ok`, and the row
not yet indexed sits at NULL. This matches the vocabulary documented in
`0006`: `NULL | ok | empty | failed`.

A failed extraction is a recorded fact about a source, not an error that
blocked the batch. This makes recipe §6.7's never-fail-the-batch rule
visible as a column.

That is the pattern to internalize from all three queries: **in this
system, "look at the database" is a legitimate debugging move**, because
the schema was designed to be read. Comments in the migrations tell you
what columns mean; status vocabularies are short and documented; nothing is
serialized opaquely that could be a column. When an endpoint's behavior
surprises you, the row usually explains it faster than the log line.

### Collections: partitioned on purpose, undeletable on purpose

The 45 `collection_chunks_p_*` tables in the `\dt` output are the collections design showing through, and psql describes it better than
prose (`\d collection_chunks`, abridged):

```
              Partitioned table "public.collection_chunks"
      Column      |    Type     | Nullable |           Default
------------------+-------------+----------+------------------------------
 tenant_id        | text        | not null |
 collection_id    | text        | not null |
 index_version_id | text        | not null |
 chunk_id         | text        | not null |
 source_id        | text        | not null |
 source_hash      | text        | not null |
 ordinal          | integer     | not null |
 text             | text        | not null |
 ts               | tsvector    |          | generated always as (to_tsvector('english', text)) stored
 embedding        | vector(256) |          |
Partition key: LIST (collection_id)
Indexes:
    "collection_chunks_pkey" PRIMARY KEY (tenant_id, collection_id, index_version_id, chunk_id)
    "idx_collection_chunks_ts" gin (ts)
    "idx_collection_chunks_vec" hnsw (embedding vector_cosine_ops)
Number of partitions: 45 (Use \d+ to list them.)
```

Every line is a decision. `LIST (collection_id)` gives each collection its
own physical partition, created at collection-creation time by runtime DDL
under an advisory lock ([collections.rs:1-9](../../src/munarium-retrieval-pg/src/collections.rs)
because the lock serializes concurrent `CREATE TABLE ... PARTITION OF`.
Two requests therefore cannot race the catalog). The GIN and HNSW indexes
are declared on the *parent* and cascade to every partition. As a result,
"every collection gets its own ANN graph." Each graph is smaller and
better clustered than one global graph. A single-collection query also
prunes to one partition before it touches an index.

`source_hash` is denormalized into every chunk row. This lets the system
build the provenance envelope without a join back to `sources`. `0011`
also adds one constraint that psql will not show unless you ask: there is
deliberately **no DEFAULT partition**, so "an insert for an unknown
collection must fail loudly" instead of accumulating orphan rows. (Contrast
`ledger_events`, which *does* keep a `ledger_events_default` catch-all: a
`tenant_seq` beyond the current range partitions is a maintenance lag, but
an unknown `collection_id` is a bug.)

The partition is also the answer to a question every reviewer eventually
asks: *why is there no delete API for index data?* The policy line at the
top of [docs/ops/index-deletion-runbook.md](../ops/index-deletion-runbook.md)
is absolute: "no munarium-server API can delete index data anywhere, under
any role." The design reason follows it: "every collection owns one LIST
partition of `collection_chunks`... Detaching and dropping the partition
is an O(1) catalog operation with no long DELETE or vacuum debt. **The
application's queries physically cannot express it.**"

That last clause is the security property. Deletion is not a permission
the app declines to grant. It is an operation outside the app's SQL
vocabulary. What the API offers instead is soft: collection `status`
flips to `retired` (rows never deleted), runbook removal hides but
retains, and `retireOld` reclaims only *inactive* index versions' chunks
while keeping every manifest.

Physical removal requires a DBA at psql with a change ticket and the
runbook. The runbook also covers two details a developer should know. The
first is the table of what is deliberately *kept*: collection rows,
index-version manifests, sources, sessions, ledger events, and everything
else that provenance needs to keep resolving. The second is the "And the
blobs" section because dropping a partition removes
derived index data only. The document bytes live in tier two, are possibly
shared by other collections, and outlive everything Postgres-side unless a
legal purge explicitly extends to them.

### Source stores from the dev seat

Everything §8 has said about tier two so far is contract; here is the
developer's practical view. The compose dev profile pins
`MUNARIUM_SOURCE_STORE: pg` (docker-compose.yml:36), with the reasoning in
the adjacent comment: document bytes in Postgres is "the offline fallback,
so the dev profile needs no cloud account."

That is the same
zero-external-state principle as §3's tier design, applied to storage: a
fresh clone reaches a working ingest path with `docker compose up` and
nothing else. The implementation is `PgSourceStore` in the Postgres store crate
([sources.rs:1-12](../../src/munarium-store-pg/src/sources.rs); it moved out of the
retrieval crate because it is a storage backend, not a search one),
and its header records a subtle choice: bytes live in their own
`source_blobs` table rather than a column on `sources`, "so the object-store
seam does not depend on the metadata row's write ordering." Swap `pg` for
`az` and nothing about how `sources` rows are written changes.

The dev-seat map of the six backends:

- **`pg`** is the default you are already using. Bytes are in `source_blobs`,
  URIs like the `pg://source_blobs/...` rows in the query above. Fine for
  development corpora; the reason it is not the production posture is
  scale, stated bluntly in [source-stores.md](source-stores.md): "Postgres
  `BYTEA` was never going to carry the 580 MB insurance corpus or the
  514 MB LOC harvest. Documents belong in object storage, with Postgres
  holding the metadata row."
- **`mem`** is in-process for the memory-store mode and unit tests. Bytes
  vanish with the process, which is the point.
- **`s3` against MinIO** is the path to exercise real object-store code
  without a cloud account. §3 already executed it end to end (the
  `--profile s3` compose profile, the two `mc` commands, the gated
  integration test flipping from a 0.00 s vacuous skip to a 0.03 s real
  round trip), so it is not repeated here.
- **`az`, `gcs`, `file`** are production and air-gapped variants of the same
  `munarium-store-objects` adapter. Their env contracts, auth modes, and
  fail-closed behaviors are the backend matrix in
  [source-stores.md](source-stores.md), which is the authoritative
  operational companion; this chapter deliberately does not duplicate it.

What is *not* per-backend is the recorded-URI contract, and it is the one
rule to carry into any new backend (recipe §6.6): every `sources` row
records `storage_backend` plus a `blob_uri` that never contains a
credential. Tests enforce this rule, so a SAS token cannot leak into a row
that audit queries and API responses will surface for years. The
`storage_backend` vocabulary itself (`az | pg | mem | s3 | gcs | file`)
lives in exactly one place, the schema comment maintained by `0015`. Thus,
widening it means a new comment-only migration, never an edit, which is how
recipe §6.6's checklist and recipe §6.3's discipline meet.

### Index identity: what a version is, and what invalidates one

The last piece of the data model is the one that makes retrieval auditable:
index versions. The identity rule sits in the retrieval crate's header
([munarium-retrieval-pg/src/lib.rs:14-16](../../src/munarium-retrieval-pg/src/lib.rs)):

> index_version = hash(shape_ref, chunker, embedder, sorted source set);
> builds are side-by-side and immutable; cutover is the `active` flip; old
> versions keep resolving so past envelopes stay verifiable.

Unpack the hash inputs to get the complete list of what invalidates an
index. These changes *should* force a rebuild instead of silently serving
stale chunks:

- **the shape** (`shape_ref`): retrieval configuration is versioned data
  (`0007`: new content = new version), so a shape change is a new ref;
- **the chunker** is deterministic and versioned (`para@1`); a chunking
  change is a new version string, not an in-place tweak;
- **the embedder** uses `local-hash@1` keyless by default, or a tenant's BYOK
  embedding provider per build, named in the manifest either way;
- **the extractor set** is folded in via the registry's `version()` string
  (every extractor id, sorted, plus `EXTRACTOR_SET_VERSION`), which is why
  recipe §6.7 tells you that merely *adding* an extractor invalidates
  existing versions by construction, and that changing an existing
  extractor's behavior requires bumping `EXTRACTOR_SET_VERSION` because
  its id alone won't move the hash;
- **the source set** is sorted, so binding or retiring a source changes
  identity, but re-ingesting identical bytes does not (the deterministic
  chunker and embedder "reproduce identical chunk ids for identical
  bytes," which is also the runbook's no-archive rollback story).

On the wire and in the database this is the `index_versions` table: id,
manifest (JSONB naming every input), `active` flag, `built_at`. The same
development database again:

```sql
SELECT id, collection_id, shape_ref, active
  FROM index_versions ORDER BY built_at DESC LIMIT 3;
```
```
          id          |            collection_id             | shape_ref | active
----------------------+--------------------------------------+-----------+--------
 idx-d46efdc45a651701 | col-019ff3aae6217410835d41e0416b3929 | entdocs@1 | t
 idx-f6f359ed8a1882a9 | col-019ff3aae5d4706194bd20d337c44cf4 | entdocs@1 | t
 idx-49bc8d3f6d1df5b8 | col-019ff3a427a976e2a11edb9fbcce58ed | entdocs@1 | t
```

`0011` enforces the invariant the flip depends on with a partial unique
index: at most one `active` version per (tenant, collection). A build
writes a complete new version's chunks beside the old under the new
`index_version_id`. Cutover is then a single-row flag flip, and the old
version's rows and manifest *stay*. That retention is not laziness. It is
the third invariant doing its job.

A provenance envelope minted last March names the index version that
served it. Because versions are immutable and old ones keep resolving,
that envelope still verifies today against the manifest of the exact
index that answered. `retireOld` may reclaim an inactive version's chunk
rows for space. Even then, it keeps the manifest that verification needs.

For the platform developer, the practical result is a habit: **when you
change anything on the identity list, you have not broken the old indexes.
You have made new ones necessary.** Rebuilds are the retrieval
tier's migrations, and like migrations they are additive: side-by-side
build, verify, flip. The claims projection's motto, rebuild, don't
migrate, is also the whole data tier's motto.

One honest edge: the pg store's read path currently fetches a lineage's
rows and resolves slices in Rust rather than pushing resolution into SQL.
It is "correct at demo scale and provably agrees with the reference
semantics," per the lib.rs hardening note. The SQL push-down (and the
`sqlx::query!` + `.sqlx/` offline machinery that would ride with it) is
a deliberate deferral — the tree's posture is
runtime-checked query strings with conformance against both backends as
the drift net, stated as such since 2026-08-17 (the Dockerfile's old
`SQLX_OFFLINE=true`, which gated nothing, is gone; §13 entry 3, closed).

Everything in this chapter ultimately rests on three key dependency
choices: `sqlx` and its compile-time-embedded migrations, `pgvector`
bindings for the HNSW tier, and the `object_store` crate whose
feature flags nearly broke the musl build. Why those exact crates, what
was rejected, and the rules for adding the next one are §9's story.

## 8A. The datastore plane: derived indexes beside PostgreSQL

*(Added 2026-09-02, lettered like §21A–C so the numbering of the chapters
around it stays true. §8 is the system of record; this chapter is the tier
derived from it.)*

### What it is, in one paragraph

**Munarium Datastore** is a second retrieval engine that serves a scope —
a collection or a legacy shape — from immutable, content-verified artifacts
instead of from PostgreSQL's tsvector + pgvector tables. The artifacts are
built from the server's committed chunks, sealed with a manifest whose
sha256 is their identity, stored in object storage, hydrated to a local
disk tier (L1), and opened through an in-memory open-shard cache (L0).
PostgreSQL remains the system of record for everything — tenants, sources,
collections, build state, the active pointer, the rollout selector,
authorization, audit — and *"this is not a replacement database and never
becomes one"* is a design rule, not a slogan. Two consequences follow. A
lost or corrupt artifact is **rebuilt or rolled back to PostgreSQL, never
repaired**. And a datastore failure must never take PostgreSQL down with
it, so a replica that agreed to serve a scope from the datastore **leaves
the traffic pool** (`/readyz` false, 503 `datastore-unavailable` on the
request path) rather than silently answering from the other engine. The
measured reason to have it: on the baseline rig a warm search is
**p50 3.35 ms against PostgreSQL's 15.60** (4.7×; 6.7× at p95). The
rollback path has been used for real: two corpora went back to PostgreSQL
for a fusion defect (BM25 magnitudes compared as if they were ts_rank),
which is exactly what "rebuilt or rolled back, never repaired" is for.

The two crates are §4's `munarium-datastore` (the engine) and
`munarium-retrieval` (the coordinator). The crates' own tests are their
design record, and [ops/mmctl.md](../ops/mmctl.md) is the operator's
command reference for the plane.
This chapter is the server-side developer's map: the modes, the routes, the
lifecycle, the environment, and what a local server can and cannot show you.

### Modes and the selector

`MUNARIUM_RETRIEVAL_MODE` (README table; default `postgres`) names the
process's mode: `postgres` (unchanged behaviour — the coordinator wraps
`PgRetrieval` and forwards), `mirror` (build artifacts beside PostgreSQL,
serve nothing from them), `shadow` (serve from PostgreSQL, run a sampled
fraction of turns through the datastore candidate path for comparison —
`MUNARIUM_DATASTORE_SHADOW_SAMPLE_RATE`), and `datastore`. In `datastore`
mode each scope's engine is whatever its **rollout selector row** says
(`GET /v1/retrieval-rollout/{scope_kind}/{scope_id}`; `PUT
/v1/retrieval-rollout` to set it), and selecting `datastore` for a scope is
**gated on serving-required completeness** — every version a live session
could still be pinned to must have a verified artifact — while selecting
`postgres`, the rollback, is never gated. Anything but `postgres` needs the
Postgres store and `MUNARIUM_DATASTORE_LOCAL_ROOT`; an unknown mode logs and
falls back to `postgres`.

### The artifact lifecycle

Every operation names a **logical index version**; an `artifact_id` (the
manifest's sha256) appears only in answers, never as a parameter that grants
access — two tenants holding identical corpora legitimately hold identical
hashes, so a hash is content, not authority (`datastore_builds.rs`'s
header).

1. **Build.** Synchronously through `POST /v1/index-artifacts/backfill`
   (every serving-required version of a collection) or
   `POST /v1/index-artifacts/{index_version_id}/rebuild`, or durably
   through `POST /v1/index-build-jobs` — the request path writes a row and
   answers with a `bjob-…` id, and whichever process runs the builder loop
   (`MUNARIUM_DATASTORE_BUILDER=enabled`) claims it with `SKIP LOCKED`,
   re-offers a lapsed lease (`MUNARIUM_DATASTORE_JOB_LEASE_SECS`) up to an
   attempt ceiling, and completes it idempotently as the holder. Outcomes:
   `published`, `converged`, `already_built`, `deferred` (another node holds
   the build). A direct build seals exactly the engine the plan names; only
   the direct build decides exact-vs-approximate vectors, above
   `MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD` (4,096) when the binary
   carries `vector-diskann`.
2. **Verify.** `POST /v1/index-artifacts/{index_version_id}/verify` re-reads
   the stored bytes and re-checks the manifest and every component;
   `GET /v1/index-artifacts/{index_version_id}` shows the catalogued
   artifacts (`sealed | verified | failed | retired`) and the version's
   bindings with their generations.
3. **Bind.** `POST …/bind` puts a verified artifact into the `staged` or
   `shadow` slot, as a compare-and-swap on the slot's generation. The
   `serving` slot is deliberately not bindable here.
4. **Promote.** `POST …/promote` moves `staged` into `serving` as a CAS
   against **both** generations the caller read, behind a fleet gate that
   reads every node's heartbeat and staged-open residency — the §7.3 of the
   datastore plan, and the operation §13.5 entry 26's neighbour, the
   2026-08-31 readiness wedge (migration 0028), was found by.
5. **Activate.** `POST /v1/collections/{id}/activate-index` flips the
   collection's ACTIVE pointer as a CAS (`expected_active`); a mismatch
   answers `activated: false` with the pointer untouched, and a
   datastore-routed collection refuses a version with no verified
   `serving` binding. Build → promote → activate, in that order.
6. **Retire.** A version that stops being active stays serving-required for
   one **pin horizon** from the moment it was deactivated
   (`index_versions.deactivated_at`, migration 0030 — §13.5 entry 27 records
   why the anchor moved off `built_at`), then falls out of the required set;
   `MUNARIUM_DATASTORE_RETIRED_RETENTION` says how long its bytes stay on L1
   after that.

`mmctl datastore status|verify|rebuild|backfill|bind|promote|rollout|jobs`
([ops/mmctl.md](../ops/mmctl.md)) drives the same routes.

### Readiness

The §9.2 contract `datastore_serving.rs` exists to keep: **a replica with
any datastore-selected scope is not ready until its complete
serving-required set is hydrated, verified and openable** — and a replica
with none has no datastore readiness dependency at all. Warming is
asynchronous and bounded (`MUNARIUM_DATASTORE_STARTUP_HYDRATE_TIMEOUT_MS`,
`…_ROLLOUT_REFRESH_MS`); `/readyz` reads maintained state and does no I/O.
The sharp edge this bought: an unactivated in-flight build once
joined the required set, took a deployment's only replica out of ingress,
and with it the API its own promotion needed (migration 0028
`activated_at` closed it). `MUNARIUM_DATASTORE_L0_OPEN_SHARDS` is the other
number to know — a deployment with a hundred-odd granular collections
thrashes the default of 8, so size it to the collection count.

### The environment

All 23 `MUNARIUM_DATASTORE_*` variables, `MUNARIUM_RETRIEVAL_MODE` and
`MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID` are in the
[README's Configuration table](../../README.md#configuration-env-vars),
per Appendix A's rule that the env contract lives in one place. The ones a
developer meets first: `LOCAL_ROOT` (required for any mode but `postgres`;
`<root>/l2` and `<root>/staging` default the artifact and staging roots),
`ARTIFACT_STORE` (`file | az | s3 | gcs | pg`), the L1 watermarks, and
`BUILDER`.

### What a local server shows you, and what it cannot

The datastore routes need the Postgres store (the memory store answers 400
`invalid-input`: "build jobs require the postgres store"). The transcripts
below ran on 2026-09-02 against a throwaway server on the compose Postgres —
`MUNARIUM_STORE=postgres`, static tokens `guide-rw:tenant-guide:rw` and
`guide-mgmt:tenant-guide:mgmt`, no datastore configuration at all — which is
exactly the state a developer's first server is in, and enough to see the
job queue, the selector and the refusal shapes.

```text
$ curl -s http://127.0.0.1:18080/v1/index-build-jobs -H "Authorization: Bearer guide-rw" -H "X-Munarium-Uid: casey.ops"
{"jobs":[]}

$ curl -s -X POST http://127.0.0.1:18080/v1/index-build-jobs -H "Authorization: Bearer guide-rw" -H "X-Munarium-Uid: casey.ops" \
    -H "Content-Type: application/json" -d '{"kind":"backfill","collection_id":"col-does-not-exist"}'
{"job_id":"bjob-88602cabaa8949ed8ab6b531276d582e","kind":"backfill","scope_kind":"collection",
 "scope_id":"col-does-not-exist","state":"pending","attempts":0,"created_at":"2026-09-02 23:37:29.958917+00"}

$ curl -s -X POST http://127.0.0.1:18080/v1/index-build-jobs/bjob-88602cabaa8949ed8ab6b531276d582e/cancel \
    -H "Authorization: Bearer guide-rw" -H "X-Munarium-Uid: casey.ops"
{"ok":true}

$ curl -s http://127.0.0.1:18080/v1/index-build-jobs/bjob-88602cabaa8949ed8ab6b531276d582e \
    -H "Authorization: Bearer guide-rw" -H "X-Munarium-Uid: casey.ops"
{"job_id":"bjob-88602cabaa8949ed8ab6b531276d582e","kind":"backfill","scope_kind":"collection",
 "scope_id":"col-does-not-exist","state":"cancelled","attempts":0,"created_at":"2026-09-02 23:37:29.958917+00"}

$ curl -s http://127.0.0.1:18080/v1/retrieval-rollout/collection/col-does-not-exist \
    -H "Authorization: Bearer guide-rw" -H "X-Munarium-Uid: casey.ops"
{"type":"https://munarium.ioka.io/problems/not-found","title":"not found","status":404,
 "detail":"not found: rollout entry collection/col-does-not-exist","kind":"rollout entry","id":"collection/col-does-not-exist"}
```

Two things that transcript shows are worth an honest word each. **The
enqueue accepted a collection that does not exist**: the request path
validates the request's shape (`kind`, which id a kind needs), not the
target's existence, and it is the builder that fails the job at claim time.
A pending job for a phantom collection is harmless — it is cancelled above
— but a 404 at enqueue would be the kinder contract; `[gap]`, recorded
2026-09-02. And `GET /v1/index-artifacts/{index_version_id}` on the same
server answers 400 *"a mirror build needs MUNARIUM_DATASTORE_STAGING_ROOT
(or MUNARIUM_DATASTORE_LOCAL_ROOT …)"* even for a read — the status route
constructs the mirror before it looks anything up, so a server with no
datastore configuration cannot answer "what artifacts does this version
have?" (the honest answer is "none"); `[gap]`, recorded 2026-09-02.

What this chapter does **not** show, because a local server with no
artifact store cannot: a build, a verify, a bind, a promote, an activation,
or a `PUT /v1/retrieval-rollout` that selects `datastore` `[gap]`. The
evidence for those is the datastore tree's own record — 23 integration tests
against real PostgreSQL and real artifacts in Docker, and the engine-upgrade
promotion drill. Reproducing one build here needs a
collection with chunks, `MUNARIUM_DATASTORE_LOCAL_ROOT`, and
`MUNARIUM_DATASTORE_ARTIFACT_STORE=file`; the datastore guide's
"Operations" section is the walkthrough.

## 9. Dependencies and supply chain

§8 closed on three key dependency choices: sqlx, pgvector, and
object_store. It promised the story of why those exact crates, what was
rejected, and the rules for adding the next one. That story has a thesis,
and it is worth stating before any of the machinery: **every dependency is
a liability that must clear a gauntlet, and the gauntlet is physical, not
procedural.**

Four of its gates are policies enforced by a tool (`cargo deny`). Two are
facts about the build that no amount of arguing can waive. A crate
that links a C library does not get a design discussion. It gets a red
Docker build, because the binary is statically linked against musl inside
`rust:1-alpine`, and the only thing that builder installs beyond the Rust
toolchain is `musl-dev` (Dockerfile:21-22). Since 2026-08-25 the compile
step runs under BuildKit cache mounts (cargo registry, git checkouts,
`target/`) so a dev roll recompiles only what a commit touched instead of
every crate from zero; the binaries are copied out of the mount into a real
layer (`/out`) because a cache mount is not part of the image.

A crate that survives the link
still lands in `gcr.io/distroless/static-debian12:nonroot`. It has no shell,
package manager, or system libraries to discover at runtime. The image
must also meet the `< 30 MB` target in the Dockerfile's header comment. As of
2026-08-18 the built image measures 30,298,689 bytes (28.9 MiB / 30.3 SI
MB) — sitting exactly at the target boundary after the observability,
clustering, and gRPC-parity programs; the next many-megabyte crate pays
the full §9 checklist.

The server treats dependency restraint as an operational posture,
not as a goal by itself. Fewer crates mean fewer advisories to review,
fewer licenses to audit, and fewer upstream projects to monitor.

![The dependency gauntlet: a new crate passes six gates in sequence — the licence allow-list, advisories, the openssl and native-tls bans, unknown-source denial, the alpine musl static link, and the distroless size budget. Stock features fail at the musl gate where cmake enters the builder; the -base plus ring configuration clears all six](images/ch9-dependency-gauntlet.svg)

Two habits make the gauntlet survivable, and both are visible in the first
screenful of the root [Cargo.toml](../../Cargo.toml).

**Versions are pinned once, at the workspace root.** Every third-party
crate any member uses is declared in `[workspace.dependencies]`. There
are 33 entries as of v0.1.2. Member crates write `tokio = { workspace =
true }` rather than naming versions. The sqlx entry says why in passing:

> db (added at the milestone; declared here so versions stay pinned workspace-wide)

One declaration means one place to audit, one place to upgrade, and no
possibility of two member crates drifting onto different feature sets of
the same dependency and silently doubling it in the lockfile. (When a
second line of the *same* crate is genuinely required, it is taken
knowingly and documented. The reqwest story below is the one instance.)

**`default-features = false` is the default posture.** A Rust crate's
default features are tuned for the common case, and the common case
assumes a glibc host with a C toolchain. This workspace starts from
nothing and adds back only what it can name. `sqlx` uses `tls-rustls` and
only the runtime, driver, and type features needed by the store. `chrono`
is limited to `clock` and `serde`. `jsonschema` and `jsonwebtoken` have
defaults off, so the workspace chooses what enters the graph. The next two
sections explain `reqwest` and `object_store` in detail. This habit is
cheap when adding a crate and valuable later. A feature you never enabled
is an advisory you never have to review.

### rustls everywhere, openssl nowhere

The single most consequential dependency rule in the tree is that **no
code path, anywhere, links OpenSSL**. Every TLS surface (HTTP client,
gRPC, Postgres driver) rides rustls. The root Cargo.toml declares the rule
in three comments, each sitting on the dependency it governs:

> `# grpc (rustls only; zero openssl anywhere in the workspace)` (:35, on
> tonic)
>
> `# capability tokens (rustls-compatible; pulls ring, never
> openssl)` (:53, on jsonwebtoken)
>
> `# http client (rustls only)` (:66, on reqwest)

Comments are documentation; `deny.toml` makes the rule mechanical
(deny.toml:99-106):

```toml
[bans]
multiple-versions = "warn"
# rustls-everywhere rule: openssl must never enter the graph
deny = [
    { crate = "openssl" },
    { crate = "openssl-sys" },
    { crate = "native-tls" },
]
```

`native-tls` is on the list because it is the polite way openssl sneaks
in: a transitive dependency asks for "the platform TLS," and on Linux the
platform TLS is openssl. Banning all three means a new crate whose default
features pull any of them fails `cargo deny check` before it fails
anything else. And per the Preface's first commitment, the claim is
verified against this tree, not asserted. Here is the entire normal-dependency
graph of the workspace, grepped:

```powershell
PS> cargo tree --workspace -e normal | Select-String 'openssl|native-tls' |
>>   Measure-Object | Select-Object -ExpandProperty Count
0
```

(One heads-up before you reproduce that on Linux: when resolved for a Linux
target, the same grep surfaces `openssl-probe`, pulled in by
`rustls-native-certs` behind tonic's `tls-roots` feature to locate the
distro's CA-certificate bundle. It is pure Rust, probes file paths, and
links no OpenSSL; the name is the only OpenSSL thing about it, which is
why the bans list, which is about linkage, does not name it. The
transcript above is from the Windows dev box, where its cfg gate keeps it
out of the resolved graph entirely.)

Why hold this line so hard? The rationale is
one sentence: rustls everywhere, zero openssl "makes musl static build
and native Windows dev both painless." This provides two distinct wins.

For the **musl static link**, openssl means cross-compiling a C library
inside the alpine builder. That requires version pinning and
`OPENSSL_DIR` setup. It can also break when the base image changes. rustls
on the `ring` backend compiles as ordinary Rust with a small amount of
bundled assembly, so the static link works.

For **Windows dev**, openssl is the classic "works on the CI image, dies
on the laptop" dependency. §2 established that this tree is Windows-first.
Windows has no system openssl, so every developer would need a
vcpkg/perl/nasm toolchain before the first `cargo build`. rustls avoids
that cost on both platforms. The same tree therefore builds unchanged on
a Windows laptop and inside `rust:1-alpine`.

The rule has one subtle consequence you will meet in `deny.toml`'s license
list, flagged there so nobody panics: ring vendors code inherited from
BoringSSL, so its license *text* includes the OpenSSL license. The
allow-list therefore contains the `OpenSSL` license identifier for
ring's paperwork only, never for openssl linkage (which the bans above
make impossible). License-of-ring is not linkage-of-openssl; the comment
on deny.toml:17 exists so that distinction survives the next reviewer.

### The musl case study: object_store, aws-lc-rs, and the cmake wall

This is the book's best worked example of the gauntlet, because one
dependency, `object_store`, is the Apache Arrow crate behind §8's four cloud
byte backends. It hit the physical gates in exactly the way the policy gates
cannot catch, and the fix touched three files whose comments now tell the
story to anyone who reads them. Recipe 6 in §6 cross-references this
section rather than repeating it; here is the full account.

**The failure.** `object_store`'s stock cloud features (`aws`, `azure`,
`gcp`) enable its HTTP client with rustls's *default* crypto backend,
which, since rustls 0.23, is `aws-lc-rs`. aws-lc-rs is a fine library with
one disqualifying property for this tree: its `-sys` crate builds a C
cryptography library, and the build requires **cmake**. The Dockerfile's
builder stage is `rust:1-alpine` with `musl-dev` and nothing else. It leaves
out cmake on purpose. A builder that collects C toolchain packages becomes
harder to understand.

The obvious `cargo add object_store --features aws,azure,gcp` works on a
dev box that happens to have cmake but fails in the alpine builder. The
extract crate's package description states the rule: a native dependency
here "is not a tradeoff, it is a build failure."

**The fix, part one: the `-base` features plus `ring`.** `object_store`
publishes each cloud integration in a `-base` variant that leaves the TLS
provider choice to the consumer. The workspace pin
([Cargo.toml:68-72](../../Cargo.toml)) selects those and names the
provider explicitly, with the comment carrying the whole rationale:

```toml
# unified source-bytes backends (S3 / Azure Blob / GCS / local fs). The -base
# cloud features + `ring` keep aws-lc-rs (and its cmake requirement) out of the
# alpine musl builder; `reqwest` here is object_store's own 0.13 line,
# default-features off (rustls) — no openssl enters the graph.
object_store = { version = "0.14", default-features = false, features = ["fs", "aws-base", "azure-base", "gcp-base", "reqwest", "ring"] }
```

**The fix, part two: reqwest 0.13 with `rustls-no-provider`.** The
comment above flags the wrinkle: object_store's cloud clients ride reqwest
**0.13**, a separate major line from the workspace's own 0.12 pin. Simply
enabling that line's `rustls` feature would re-import the same problem.
reqwest 0.13's `rustls` feature hard-wires aws-lc-rs. The
`munarium-store-objects` crate therefore declares the 0.13 line itself, with
the workspace's most load-bearing dependency comment
([src/munarium-store-objects/Cargo.toml:12-19](../../src/munarium-store-objects/Cargo.toml)):

```toml
# object_store's cloud clients ride reqwest 0.13 (a separate line from the
# workspace's 0.12), whose `rustls` feature would hard-wire aws-lc-rs and its
# cmake requirement into the alpine musl build. `rustls-no-provider` +
# `rustls/ring` keeps the whole workspace on the one crypto backend it
# already links. If a future dependency ever enables rustls's aws-lc-rs
# feature alongside this, rustls demands an explicit process-level provider
# at runtime — treat that as a build-breaking event, not something to paper
# over here.
reqwest = { version = "0.13", default-features = false, features = ["rustls-no-provider", "http2"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "logging", "std", "tls12"] }
```

**The fix, part three: install the provider at runtime.** `rustls-no-
provider` means what it says. reqwest compiles against rustls without
choosing a crypto backend, and rustls will **panic at first use** unless a
process-level provider has been installed. The store crate owns that
obligation in
[src/munarium-store-objects/src/lib.rs:119-129](../../src/munarium-store-objects/src/lib.rs),
sitting directly above the constructors that depend on it:

```rust
/// reqwest 0.13's `rustls-no-provider` build path (chosen to keep aws-lc-rs
/// and its cmake requirement out of the musl build) panics unless a
/// process-level rustls crypto provider is installed. Install ring exactly
/// once; an Err from install_default means another component already
/// installed a provider, which is equally fine — one just has to exist.
fn ensure_rustls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
```

Every backend constructor (`s3`, `azure`, `gcs`) calls it first, so no
code path can reach a TLS handshake before a provider exists, and the
swallowed `Err` is deliberate: `install_default` failing means someone
else already installed one, and the invariant is "exactly one exists," not
"ours won." Note the last sentence of the Cargo.toml comment above. If a
future dependency ever enables rustls's aws-lc-rs feature alongside this,
that is a build-breaking event to fix at the dependency graph, not
something to paper over with a bigger constructor.

**The accepted cost, written in the lockfile.** The workspace now
carries two reqwest major lines: its own 0.12 (providers, azure-auth,
docintel, the CLI, the conformance client) and object_store's 0.13. Both are
rustls, both default-features-off. Verified against this tree:

```powershell
PS> Get-Content Cargo.lock | Select-String '^name = "reqwest"' -Context 0,1
> name = "reqwest"
  version = "0.12.28"
> name = "reqwest"
  version = "0.13.4"
```

Two copies of an HTTP client compile into the binary. The size cost is an
estimate, not a measurement. Nobody has built the tree both ways and
compared the images. The cost is likely a megabyte or two,
absorbed within the < 30 MB target. The duplication is also why `[bans]
multiple-versions` is `"warn"` rather than
`"deny"`: it is known, commented, and accepted, and a warning
keeps it visible without making the accepted case a permanent CI
exception. When object_store's line and the workspace's converge on one
major version, the duplicate disappears and the warning with it.

The final proof that the change worked, with ring in and aws-lc-rs out,
is one grep over the lockfile:

```powershell
PS> Get-Content Cargo.lock | Select-String '^name = "(ring|aws-lc-rs|aws-lc-sys)"' -Context 0,1
> name = "ring"
  version = "0.17.14"
```

One crypto backend serves everything. Tonic's TLS, sqlx's `tls-rustls`, both
reqwest lines, and jsonwebtoken's signatures all resolve to that single
`ring 0.17.14`.

### deny.toml, section by section

`cargo deny` is the policy half of the gauntlet, and
[deny.toml](../../deny.toml) is short enough to hold in your head. It has
four sections, each carrying its reasoning inline. The file opens with its
mission statement:

```toml
# cargo-deny gate — the standing defense against the relicensing wave (architecture.md §11).
# Any dependency that turns is replaced behind its seam.
```

"The relicensing wave" is the run of infrastructure projects that swapped
permissive licenses for source-available ones; the defense is an
allow-list (a license change to anything not on it turns the gate red on
the next `cargo update`) plus the architecture's seam discipline (§4),
which is what makes "replaced behind its seam" a credible threat rather
than a slogan.

**`[licenses]`** allows the standard permissive set: MIT and MIT-0,
Apache-2.0 (with the LLVM-exception variant), BSD-2/3, ISC, Zlib,
Unicode-3.0, MPL-2.0, and CDLA-Permissive-2.0. One entry needs
its comment:

```toml
"OpenSSL",  # ring's bundled license text; no openssl linkage exists in this workspace
```

That is the nuance from the rustls section: the `OpenSSL` *license
identifier* is allowed because ring's vendored BoringSSL heritage carries
that license text; openssl *the library* remains banned two sections
down. Copyleft beyond MPL's file-level terms is absent from the list on
purpose. Nothing in the GPL family can enter without this file changing.
Such a change requires a reviewed diff.

**`[[licenses.exceptions]]`** is one small block per workspace
crate, each allowing `Apache-2.0` for exactly that crate.
The comment above them explains the shape:

```toml
# Workspace crates are first-party and unpublished; cargo-deny treats each as
# an exception, named one at a time (the gate exists for THIRD-party license
# drift, architecture.md §11).
```

The per-crate form is deliberate: naming each workspace crate means the
allowance never silently covers a third-party crate that changes its license.
That is the exact event the gate exists to catch.

Recipe 11 (§6) also carries the practical rule: **adding a workspace crate
means adding one of these blocks.** If you forget it, the compiler stays
green. The cargo-deny CI job then fails with an unlicensed-crate error that
names your new crate. This can confuse a first-time contributor who does
not know the registry exists. It is this chapter's example of §6's
update-the-registry pattern.

**`[advisories]`** sets `yanked = "deny"`. A yanked crate version is
upstream saying "do not use this," and the gate agrees. The section holds
three ignores, each with the written justification this tree
requires. The
first is small and typical: RUSTSEC-2025-0134, rustls-pemfile archived
upstream, reaching the tree transitively through tonic/reqwest TLS
features. "Not a vulnerability; re-evaluate when tonic/reqwest drop the
dependency." The second is the exemplar, quoted in full because it is the
template every future ignore is held to (deny.toml:82-96):

```toml
# ttf-parser is unmaintained, with no safe upgrade available. It reaches
# us through lopdf -> pdf-extract, which is how PDFs get a text layer.
#
# ACCEPTED RISK, stated plainly: this is an UNMAINTAINED advisory, not a
# known vulnerability — but the crate parses embedded fonts out of
# caller-uploaded PDFs, so it is untrusted input, and nobody upstream is
# watching it. Accepted because the alternative is no PDF support at all
# (every native engine — pdfium, poppler, mupdf — is excluded outright by
# the static-musl/distroless build), and because extraction runs at index
# time on already-stored bytes rather than in the request path.
#
# Re-evaluate when: pdf-extract/lopdf drop it, a maintained fork appears,
# or ANY vulnerability advisory lands against it — the last of which flips
# this from acceptable to blocking.
"RUSTSEC-2026-0192",
```

A third ignore, added when the datastore crates joined the workspace, holds
the same five fields for `RUSTSEC-2024-0384` (`instant`, unmaintained,
arriving through tantivy's `measure_time` dependency): read verbatim, it
compiles to a four-line type alias on every target this workspace builds, and
the fix that removes it (`tantivy` 0.24+) is deliberately not taken yet
because it would invalidate the pinned lexical-parity measurements.

Read each entry as a form with five mandatory fields, because that is what it is:
*what* the advisory actually says, *how* the crate reaches the tree,
*why the risk is real anyway*, *why it is accepted regardless*, and *the
concrete triggers that reopen the decision*, including the one that
flips it from acceptable to blocking. This is the Preface's honesty rule
applied to supply chain. An ignore is never a suppression. It is a signed
decision with an expiry condition, stored where CI reads it so the decision and the
enforcement cannot drift apart. If you cannot fill in all five fields for
a new advisory, you do not have an accepted risk. You have a dependency
to replace.

**`[bans]`** you have seen (openssl, openssl-sys, native-tls;
multiple-versions warn). **`[sources]`** closes the file by denying
unknown registries and unknown git sources. Every crate comes from
crates.io, full stop. No `git = "https://..."` pin to somebody's fork, no
alternate registry, which means no dependency whose contents can change
under an unmoved version number.

The gate runs in three places, and you should use all of them. It runs
locally through `cargo deny check`; cargo-deny is a dev-machine install,
not a build dependency. It also runs in the `cargo-deny` CI job (§10)
through EmbarkStudios' cargo-deny-action against `server/Cargo.toml`.
Finally, it runs in
effect on every `cargo update`, since the next check reviews each moved
transitive version against a fresh advisory database. On this tree, the
healthy output is one line:

```powershell
PS> cargo deny check 2>&1 | Select-Object -Last 1
advisories ok, bans ok, licenses ok, sources ok
```

### The extract crate: dependency comments as house style

If deny.toml is the tree's dependency *policy*, the manifest of
[munarium-extract](../../src/munarium-extract/Cargo.toml) is its dependency
*prose style*. This is the file to imitate when you add anything. Its package
description closes by setting the stakes for the whole crate:

> Pure Rust only. The image is a static musl binary on distroless, so a
> native dependency is not a tradeoff, it is a build failure.

Then every dependency line answers, in a comment, the question a reviewer
would otherwise have to reconstruct: *why this crate, why these features,
why this version floor.* Four are worth reading closely.

**`zip`: why features are subtracted.** DOCX is a ZIP holding
`word/document.xml`, and the comment explains why the obvious declaration
would be wrong:

```toml
# `default-features = false` matters: zip's defaults pull bzip2, lzma, zstd
# and xz, all of which bind C libraries and would break the static musl link.
# The `deflate` feature is the pure-Rust path (it selects
# `flate2/rust_backend`); `deflate-zlib` and `deflate-zlib-ng` are the C ones
# and must never be selected here. DOCX only ever uses deflate anyway.
zip = { version = "2", default-features = false, features = ["deflate"] }
```

Note the last sentence: the constraint costs nothing, because the format
never needed the C codecs. Most `default-features = false` stories end
that way, which is why the habit is the default.

**`quick-xml`: a version floor that encodes an advisory.**

```toml
# >=0.41 is required, not preferred: earlier versions carry an unbounded
# allocation advisory on namespace declarations, and a DOCX is untrusted
# caller-supplied XML — precisely the input that advisory describes.
quick-xml = "0.41"
```

Without the comment, `"0.41"` looks like whatever was current at add-time,
and a well-meaning refactor could relax it. With it, the floor is
load-bearing and says so. It also names the *threat model match* (the
advisory describes exactly this crate's input) rather than just citing an
ID.

**`pdf-extract`: the floor with a war story.**

```toml
# PDF text layer. Pure Rust (wraps lopdf); no pdfium/poppler.
# >=0.12 pulls lopdf >=0.42, which fixes RUSTSEC-2026-0187: unbounded
# recursion on nested PDF objects aborted the PROCESS (SIGABRT) on a crafted
# upload. Anyone who can ingest a PDF could have killed the server.
pdf-extract = "0.12"
```

One sentence of consequence ("anyone who can ingest a PDF could have
killed the server") does more review-proofing than a page of policy.

**The OCR block: pinning against duplication.** The `ocr` feature's
dependencies show two more moves. ocrs is chosen with the alternatives
enumerated and rejected in place. Every Tesseract route binds C++ natives
or shells out, so "none can exist in a rust:alpine -> distroless/static
build." rten, the ocrs ML runtime, is pinned for a
reason that has nothing to do with APIs:

```toml
# Pinned to the version ocrs itself depends on: declaring a newer rten pulls
# a SECOND copy of the whole ML runtime into the binary, which this image
# budget cannot absorb. `rten_format` loads the .rten model files.
rten = { version = "0.24", optional = true, default-features = false, features = ["rten_format"] }
```

That is the dual-reqwest lesson inverted: there, a second copy of an HTTP
client was accepted at a megabyte or two; here, a second copy of an ML
runtime is refused because the budget cannot absorb it. Both decisions are
right, and both are *written down at the declaration site*. This is the
actual house rule. §12 states it generally ("comments state constraints
the code can't"); this manifest is what it looks like applied to a
dependency list. The whole feature is `default = []`, off unless asked
for. OCR "adds ~10 MB of model runtime to the image and is CPU-expensive,
so it is an explicit choice." This follows the same default-off-if-somebody-pays
philosophy as recipe 10's doc-intel arm.

### The add-a-dependency checklist

Recipe 11 in §6 compresses this chapter into five lines and points here;
this is the complete landing site. Work it from top to bottom. Each step
exists because skipping it has already burned somebody.

1. **Challenge the need.** The cheapest dependency is none: this tree
   serves HTTP docs pages from static strings and vendored nothing to do
   it. If the crate would be
   load-bearing for correctness (an async runtime, a parser for a hostile
   format, a crypto primitive), proceed; if it saves twenty lines, write
   the twenty lines.
2. **Vet before adding.** License on the allow-list? Maintained (when did
   it last release; who depends on it)? Pure Rust, or does `cargo tree`
   on a scratch project show a `-sys` crate, cmake, or a C toolchain
   anywhere in its subtree? A `-sys` in the subtree is a probable gate-5
   failure. Stop and look for the pure-Rust alternative first, the way
   the extract crate's comments show the search ending.
3. **Pin it once, at the root.** Add the version to
   `[workspace.dependencies]` in the root `Cargo.toml`; member crates
   take `{ workspace = true }`. A member-local version is reserved for
   the object_store situation: a genuinely separate line that carries a
   comment that says so.
4. **Start from `default-features = false`** and add back only features
   you can name a consumer for, preferring `rustls`/pure-Rust variants.
   If TLS is involved, the feature you want usually contains the string
   `rustls`, and never `native-tls`, which the bans will catch anyway.
5. **Write the comment.** At the declaration site, in the extract-crate
   style: why this crate, why these features, why this floor. If the
   version floor encodes an advisory or a build constraint, say which,
   and what input or gate makes it matter.
6. **Run `cargo deny check`.** All four gates must come back `ok`. If a
   license is off-list, that is a decision for the file's allow-list and
   a reviewed diff, not a local override. If an advisory fires and you
   believe it is acceptable, write the five-field justification block in
   deny.toml: what it is, how it reaches us, why the risk is real, why it
   is accepted anyway, and the re-evaluation triggers. Otherwise, replace the crate.
   No bare ignores, ever.
7. **If you are adding a workspace *crate*, add its
   `[[licenses.exceptions]]` block**. The per-crate exception makes the
   deny job, not the compiler, your first failure otherwise.
8. **Prove the musl build.** `docker build .` from `server/` (or
   `gates.ps1` plus an image build, §10). A dependency
   that compiles on your Windows box and dies in `rust:1-alpine` is not
   added yet. This is the gate the object_store story hit, and it is
   cheaper to hit it before review than after merge.
9. **Check the size delta.** The image target is < 30 MB and the current
   binary spends most of it. A crate that adds many megabytes needs
   either the rten treatment (align versions so nothing duplicates) or
   the OCR treatment (an off-by-default feature, priced in its comment).
10. **Respect the boundaries.** `munarium-core` takes no new dependencies
    without a conversation. CI greps its subtree for sqlx, axum, tonic,
    reqwest, and utoipa (§4's boundary table), and the honest shape for
    new capability is almost always a trait in core with the dependency
    in an adapter crate.
11. **Ship it as one change**. The declaration, comment, any deny.toml
    entries, and the lockfile move together, per §6's pattern of
    patterns. A dependency PR whose deny.toml diff arrives "in a
    follow-up" is the follow-up lie §12 forbids.

The checklist's enforcement lives one chapter ahead: the `cargo-deny` job
that runs gates 1–4 on every push, the image build that runs gates 5–6 on
release, and the rest of the pipeline that a green dependency change still
has to clear: fmt, clippy, the test ladder, conformance ×3, the drift
checks, and the boundary grep. §10 walks that pipeline job by job, and
then follows the artifact it produces down the path to production. That
artifact is an image whose name is its commit, and whose tag cannot hide a
dirty tree.

## 10. CI and the path to production

§3 taught you the test ladder as something you climb while you work: run
the cheapest rung that can see your change, climb higher as the change
settles. This chapter shows the same material from the other side. Instead
of tiers you choose, it covers **gates you cannot choose to skip**. It then
follows the artifact that clears those gates — the container image — to
the two deployment shapes this repository ships.

The cast is small. One workflow file,
[.github/workflows/server-ci.yml](../../../.github/workflows/server-ci.yml),
holds three jobs. One script, `gates.ps1`, runs the same gates from your
desk. One [Dockerfile](../../Dockerfile) creates the only output that
reaches a registry. One Helm chart
([deploy/helm/munarium](../../deploy/helm/munarium/README.md)) and one
illustrative Terraform module
([deploy/terraform/example-aks](../../deploy/terraform/example-aks/README.md))
are the cluster deployment path. Releases are built and published by Ioka;
the public Server image is now available as `docker.io/iokaio/munarium`.
This chapter covers consuming that image locally and selecting it for a
cluster, while the source-build and CI sections remain for contributors.
The gate transcript below comes from a run on this tree.

### One list of gates, three mirrors

The design principle first, because it explains every file layout choice
that follows: **there is one list of gates, and it is deliberately
mirrored in three places.**

1. **CI** (`server-ci.yml`, job `lint-test` + job `cargo-deny`) is the
   enforcement copy. It runs on every push to main and every PR that touches
   `server/**`. You cannot merge around it.
2. **The local gate runner** (`gates.ps1`) is the complete local copy: the
   same gate list against the same compose Postgres, deploying nothing.
   Its header says so in its first sentence — "the same gates server-ci's
   lint-test and cargo-deny jobs run, against the same pg container. This
   is the product's full local gate runner; it deploys nothing"
   (gates.ps1:2-4).
3. **The dev-loop pieces** (`test.ps1`, `build.ps1 -Lint`) are the
   ergonomic copies, sliced into §3's tiers so you can pay for exactly
   the rung your change needs while iterating.

One behavior has three copies, and copies can drift. That is why each one
has comments that name its siblings. Section 3 showed the main incident
from the inside. Tier 4 of `test.ps1` failed in a clean environment because
CI's copy had pinned `MUNARIUM_SOURCE_STORE`, but the local script had not. Two
copies of one tier had drifted in opposite directions (§3, "the
`MUNARIUM_SOURCE_STORE` pin has a story").

That incident set a rule. When you change a gate in one copy, change it in
all three in the same commit. Leave a comment at each site that names the
others. Every mirrored step in `gates.ps1` and `server-ci.yml` carries
that comment today; read a few and you will see the pattern.

### The workflow at a glance

`server-ci.yml` is the server's gate. It is path-scoped to `server/**`,
plus one sibling tree, `matrix/contract/**`, because `server/contract/matrix/`
is a byte-for-byte copy of it and a contract-only edit must trigger the
server half or the two copies drift silently. Three jobs:

- **`lint-test`** is the gate list. It runs on every push/PR.
- **`cargo-deny`** checks licenses, advisories, bans, and sources. It is also
  unconditional, as its own job so a licensing failure is legible at a
  glance rather than buried mid-log.
- **`terraform`** proves the shipped example module is a thing a reader can
  actually run: `fmt -check` and an offline `init` + `validate` over every
  `deploy/terraform/example-*/` directory. No credentials, no state, no
  deploy.

Nothing in the workflow publishes anything. A push or a PR is green or red,
and that is all.

### `lint-test`, step by step

The job boots a real `pgvector/pgvector:pg16` service container on host
port 5433. It uses the same image, credentials, and port as the local
compose file. A `pg_isready` health check runs before any step. The job also
exports `MUNARIUM_TEST_DATABASE_URL`, so pg-gated tests run just as they do in
§3's local tier 2 (server-ci.yml:35-49). One housekeeping step precedes
the gates: it deletes some 25 GB of toolchains the hosted runner ships and
this job never touches, because the feature build in step 4 once filled the
runner's disk mid-build (server-ci.yml:57-59).

Beyond checkout, toolchain, and cache setup, the job has nineteen gate
steps. Their order is a policy: cheap and general checks come first.
Expensive and specific checks come later. Documentation drift follows
behavior, and structural greps come last.

**1–2. `fmt` → `clippy`.** `cargo fmt --all --check` and
`cargo clippy --workspace --all-targets -- -D warnings`. Identical
invocations to `build.ps1 -Lint`. Lint locally, and CI cannot surprise
you with a formatting diff.

**3. `test`.** `cargo test --workspace`, with the service container
making the pg integration suites real rather than vacuously green (§3's
vacuity discussion; the same suite you will see in the local transcript
below). One test in this step gates documents rather than code and
deserves a name: `docs_coverage` in the server crate reads the served
OpenAPI document and asserts that every path appears — by shape, so `{id}`
covers `{version_id}` — in `docs/api/rest.md` and in this book's Appendix
F; that every problem slug the crate can emit appears in
`docs/api/errors.md`; and that every relative link under `docs/` resolves
to a file that exists. The audit that motivated it found thirteen
datastore-plane routes and one slug undocumented while both documents read
as complete, and twenty-one broken relative links. `docs/route-index.py`
regenerates Appendix F's table.

**4. The `vector-diskann` feature.** Clippy and the tests of
`munarium-datastore`, `munarium-retrieval` and `munarium-server` with the
feature ON. It is off by default in the workspace, so the workspace run
never sees it, and without this step a feature-gated regression would reach
the image, which builds with it on (below).

**5. Conformance, in-process + postgres.** The kernel scenarios (§7)
against `munarium-store-mem`, then against the service database. This is
the storage-parity half of the conformance story.

**6. Conformance, black-box.** The transport-parity half: a real server
on 18080 (REST) and 15051 (gRPC), pg-backed, probed over both wires. The
step's comment is one of the two recorded lessons this chapter exists to
pass on (server-ci.yml:82-86):

> Pre-build BOTH binaries before starting the server: `cargo run &` holds
> the build-dir lock while compiling, so a fixed sleep races the server's
> port bind (seen live: conformance failed 60ms before the listener came
> up). Run compiled binaries directly and poll /healthz.

Apply this lesson broadly: **anything that boots a server in automation
must build first, run the compiled binary, and poll for readiness**. Never
use `cargo run` with a fixed sleep. The failure is a 60-millisecond race
that you will lose about one run in twenty. A flaky gate is worse than no
gate because it teaches people to rerun failed checks.

Every script in this tree that boots a server follows the same
build-then-poll pattern. This includes `test.ps1`, `gates.ps1`, and the
compose health checks. Anything you add should follow it too.

**7. Conformance, platform.** A second server on 18081/19091 (gRPC
disabled, ops plane on its own port so it cannot collide with step 6's),
two static tokens (`rw` + `mgmt`), `MUNARIUM_TOKEN_SECRET` pinned, and the
full platform REST surface driven end to end. The comment is the second
recorded lesson, and the most expensive one (server-ci.yml:102-106):

> The platform suite once existed only in test.ps1 -Platform, so a
> run-breaking bug shipped with CI green. This step is the regression
> gate for the whole platform REST surface (uid/tokens/collections/runbook-v2/
> sessions/ingest/removal/reports).

Read that comment as a definition of what "CI green" may mean. A local-only
tier will be skipped under deadline, often when it matters most. A new test
surface is not done until CI runs it. The platform tier existed and
worked, but it caught nothing because nothing forced it to run.

**8. Conformance, cluster.** Two instances of the same binary against the
same database (18082/18083, gRPC disabled, `MUNARIUM_REGISTRY_TTL_SECS=1`,
`MUNARIUM_REPLICA_COUNT=2`), running the N-replica scenarios: registry
convergence within the TTL, cross-instance idempotency, interleaved seq
allocation, and the run advisory lock. The step's comment names its two
mirrors (test.ps1 `-Cluster`, gates.ps1) per the mirror-drift rule — this
tier was born mirrored in all three, so the platform step's history
cannot repeat here.

**9. The trimmed authoring build.** `cargo test -p munarium-authoring
--no-default-features`. A tree that drops `runbooks/applications/` builds the
authoring catalog with `default = []`; this is that configuration, proven on
every push so a trimmed build does not fail on the compile-time embeds.

**10. The OpenAPI drift check.** It regenerates the spec from the binary
(`cargo run -p munarium-server -- openapi`) and asserts JSON equality
against the committed `docs/api/openapi.json`. This is the
docs-track-the-code commitment (Preface, commitment 3) turned into a
gate. You *cannot* change the REST surface without updating the committed
spec in the same push.

One Windows detail is worth knowing. The comparison loads the committed
file with `encoding='utf-8-sig'` (server-ci.yml:161) because a spec rebuilt
on Windows may have a BOM. The gate compares parsed JSON rather than bytes,
so CRLF and BOM noise cannot cause a false failure.

**11. The contract bundle is reproducible.** `contract/mmp/publish.py
--self-test`: two cuts of the public MMP contract bundle must be
byte-identical and every file must match the lock, or a vendored copy could
never be drift-checked against the tree.

**12. The gRPC reference drift check.** The same shape as step 10 for the
generated `docs/api/grpc-reference.md`.

**13. The Matrix contract drift check.** `server/contract/matrix/` is cut
from `matrix/contract/` by that tree's publisher, lock included. The step
self-tests the publisher, then checks the vendored copy against a fresh
cut; independently, `munarium-api-types`' `matrix_contract` test verifies
every vendored file against the lock. A copy plus checks, no crate edge
(§4's ground rule). Mirrored in `gates.ps1` and `matrix-ci.yml`.

**14. The license gate.** `check_license.py`: the workspace and every crate
declare Apache-2.0, `LICENSE` / `NOTICE` / `THIRD_PARTY_NOTICES.md` exist,
and every source file that can carry an SPDX line does (`--stamp` adds a
missing header).

**15. Third-party notices.** `tools/third_party_notices.py --check
--cargo-target x86_64-unknown-linux-musl`: `THIRD_PARTY_NOTICES.md` is
generated from the musl-resolved runtime graph — the graph the image
actually ships — and reviewed; a dependency change that is not regenerated
fails here (§9).

**16. Crate boundary check.** `cargo tree` piped through greps for the
documented boundaries (§4's boundary table, server-ci.yml:207-235):
`munarium-core` and `munarium-access` must carry none of the five banned
crates (sqlx, axum, tonic, reqwest, utoipa); `munarium-providers` must carry
no storage crate; and `munarium-api-types`, which ships in the public
contract bundle, may depend on nothing of the workspace but
`munarium-proto`. Purity is a build-time property here, not a review
convention.

**17. Retrieval boundary check.** Cargo has no notion of "a dependency only
the composition root may name", so this one is a source-level grep
(server-ci.yml:236-252): `munarium-server` may reference
`munarium_retrieval_pg` only in `state.rs`, where `AppState` constructs the
backend. One `use munarium_retrieval_pg::` in a new handler and PostgreSQL is
the real interface again — §4's `munarium-retrieval` paragraph is the rule
this enforces.

**18. Datastore boundary check.** A `cargo tree` grep over
`munarium-datastore` (server-ci.yml:253-269) for Axum, tonic, SQLx,
`munarium-core`, the server config, auth and runbooks: none may appear, so
the crate stays independently usable (§8A).

**19. Additive-only migrations check.** A regex over
`src/munarium-store-pg/migrations/` for `DROP TABLE`, `DROP COLUMN`, or
`ALTER TABLE … DROP` (server-ci.yml:270-275). Destructive DDL cannot enter
the migration set. This enforces §8's append-only storage policy with the
cheapest possible tool: a grep. It is also what makes rolling an image back
safe, as the deployment half of this chapter explains.

### The `cargo-deny` job

This needs only one paragraph because §9 owns the content. The job is a
dozen lines: checkout plus `EmbarkStudios/cargo-deny-action` pointed at
`server/Cargo.toml` with `--all-features` (server-ci.yml:277-290), so the
optional `vector-diskann` dependency graph is inside the checked surface
rather than left unaudited. It runs the same four gates as a local `cargo
deny check`: `licenses`, `advisories`, `bans`, and `sources`. Both use the
same committed `deny.toml`.

Section 9 explains what gives those gates meaning. It covers the allow-list
policy, per-workspace-crate license exceptions, the five-field advisory
reason format, and the no-bare-ignores rule. Section 10 adds only placement.
The separate job lets you tell "a dependency changed its license" from
"your code broke" at the top of the Actions page.

### The `terraform` job

The shipped Terraform example must be a thing a reader can actually run:
every input declared, every reference resolvable. The job runs `terraform
fmt -check -recursive` over `deploy/terraform/`, then for every
`example-*/` directory an `init -backend=false -input=false` followed by
`validate` (server-ci.yml:292-309). `-backend=false` is what keeps it
offline: no credentials, no state, no cloud account, so the job runs on
every push like the others.

Be exact about what this proves. The module parses, its references resolve,
and its provider constraints are satisfiable. It does not prove the module
converges on a real subscription; the module's own README says "authored and
syntax-checked, not applied end to end", and this job is precisely the check
that sentence describes. Expect a shakedown pass on a first apply.

### The local mirror: `gates.ps1`

Now the second mirror, and the centerpiece. `gates.ps1` opens by declaring
exactly what it is (gates.ps1:2-4): "the munarium-server local gate set, the
same gates server-ci's lint-test and cargo-deny jobs run, against the same
pg container. This is the product's full local gate runner; it deploys
nothing." Here is a run on this tree, shortened to the step banners. Each
`Step` block is "the lint-test job, verbatim gates":

```text
PS server>.\gates.ps1

==> postgres test container (docker compose, :5433)
  test database munarium_ci recreated (clean migration run)

==> fmt
==> clippy
==> test (includes pg integration)
    ... 720 tests across the workspace, pg suites un-skipped ...

==> conformance (in-process + postgres)
MMP conformance — in-process (munarium-store-mem)
8 passed, 0 failed
MMP conformance — postgres (munarium-store-pg)
8 passed, 0 failed

==> conformance (black-box, both planes, pg-backed)
MMP conformance — REST plane (http://127.0.0.1:18080)
8 passed, 0 failed
MMP conformance — gRPC plane (http://127.0.0.1:15051)
8 passed, 0 failed

==> conformance (platform platform surface, pg-backed)
MMP conformance — platform surface (http://127.0.0.1:18081)
10 passed, 0 failed

==> conformance (cluster, two instances, pg-backed)
MMP conformance — cluster (http://127.0.0.1:18082 + http://127.0.0.1:18083)
5 passed, 0 failed

==> openapi drift check
==> contract bundle self-test
==> matrix contract drift check
==> license gate (manifests, SPDX headers, license texts)
==> third-party notices current for the shipping crate graph
==> grpc-reference drift check
==> crate boundary check (core + access purity; providers vs storage)
==> retrieval boundary check (server names the coordinator, not the backend)
==> datastore boundary check (the crate stays independently usable)
==> additive-only migrations check
==> cargo deny (licenses/advisories)
advisories ok, bans ok, licenses ok, sources ok

ALL LOCAL GATES PASSED
```

A run that recompiles the workspace after a source change takes a few
minutes (3 m 49 s for this one); a warm tree runs the list in about a
minute, and a cooler incremental cache doubles that. The first run after a
toolchain update or `cargo clean` must compile the full workspace, so allow
several minutes for that run. A one-to-two-minute warm run keeps the
policy realistic. People will follow the advice to "run the CI mirror
before you push" at that price.

Three details of the transcript deserve their paragraphs.

**The disposable database.** The first banner's second line says
`test database munarium_ci recreated`. This differs on purpose from the way
your *dev-loop* database is handled. The header explains why
(gates.ps1:11-15):

> The pg gates execute against a DISPOSABLE database recreated every run: CI
> gets a clean schema for free from a fresh container, but a dev box keeps
> its pgdata volume for weeks, and a long-lived database accumulates
> whatever migration history it happened to see. Reusing it would make the
> gate depend on this machine's past rather than on the code under test.

That last clause gives the full theory of gate design in nine words: **a
gate must depend on the code under test and nothing else.** CI achieves this
with temporary containers. A laptop uses a temporary *database* inside a
long-lived container.

The same reasoning explains why every run uses `DROP DATABASE … WITH
(FORCE)` and `CREATE DATABASE`. It also explains why an in-place edit to a
shipped migration never breaks the gates. Such an edit makes sqlx reject a
database that already applied the old checksum, but it affects only a
long-lived database. Sections 2 and 8 cover the recovery.

**The Windows differences.** The mirror matches behavior rather than text.
Each place where the script differs from the YAML has a comment that gives
the reason.

Before binding, it removes a *stale munarium-server*, and only a
munarium-server, from its test ports. Unlike a fresh runner, a dev box may
still have a process from an interrupted run (gates.ps1:88-107; see §2's
port map and reaping rule). The script also includes the `http://` scheme
on the gRPC probe endpoint. Tonic accepts an endpoint without a scheme on
Linux but rejects it on Windows (gates.ps1:126-130, verified live).
Finally, it pins `MUNARIUM_SOURCE_STORE=pg`, the variable from §3's drift
incident, "exactly like the compose file does" (gates.ps1:110-114).

**The soft gate.** `cargo deny` runs if installed and warns if it is absent
(gates.ps1:421-427). This is the only gate in the list that is optional
locally. The difference is deliberate and clear. A missing local tool
becomes a warning *because CI still enforces it on every push*. No gate is
silently skipped where it is required.

### The image: what `build.ps1 -Image` produces

The [Dockerfile](../../Dockerfile) builds `munarium-server` and `mmctl`
with the locked Cargo dependencies and Rust 1.98.0. The builder and final
base images are pinned by digest. `tonistiigi/xx`, clang and lld support
cross-compilation to `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl`; the final verification checks both binaries
for static linkage. BuildKit caches the Cargo registry and git checkouts,
with separate target caches for AMD64 and ARM64. The `FEATURES` argument
defaults to `vector-diskann`.

The final distroless image contains `/munarium-server`, `/mmctl`, CA roots
and license notices under `/usr/share/licenses/munarium/`. It has no shell
or package manager. `docker exec ... sh` and an in-container `curl`
healthcheck therefore cannot work: probe from the host or another service,
and invoke `/mmctl` directly. Port 8080 serves REST and the dashboard,
50051 serves direct gRPC, and 9090 serves operations/metrics. `EXPOSE`
documents those ports; it does not publish them to the host.

The image runs as distroless `nonroot`, UID/GID **65532**. Persistent
directories mounted for filesystem sources or derived indexes must be
writable by that identity. Kubernetes deployments should set numeric
`runAsUser` and `runAsGroup` values so `runAsNonRoot` can verify the user.
The Dockerfile's size comment is a build target, not a measured size for
every release or a substitute for checking the published platform images.

The build needs no database. Every sqlx query is a runtime-checked string,
so the Dockerfile sets no `SQLX_OFFLINE` and carries no `.sqlx/` data;
conformance against both store backends is the query-drift net (§13 entry
3).

### Naming the image you deploy

Before deploying anything, settle the question every deployment shares:
what is the artifact called? Three rules, each of which exists because the
obvious answer lies.

**The Cargo version is not a build identity.** `/version` reports
`CARGO_PKG_VERSION`, which is the workspace version (`1.0.0` as this is
written). Between releases, many commits share that number, so two
different images can answer `/version` identically while serving different
API surfaces. Tag images by commit — `sha-<shortsha>` from a clean tree —
and tag a dirty tree so it can never masquerade as a reproducible
per-commit image (`local-<shortsha>-dirty-<timestamp>`, or any scheme
that puts the word *dirty* in the name).

**Select a release, then record its digest.** On Docker Hub, `1.0.0` is
immutable; `1.0` and `latest` may advance. Candidate tags such as
`1.0.0-rc.1` are evaluation builds. The additional `sha-<full-source-sha>`
tag identifies the released source revision. Pin deployments to the image
index digest (`repository@sha256:…`) so the same declaration selects the
same build on both AMD64 and ARM64. A locally built image can have a
different index digest even at the same source commit, because build
provenance is part of the index. Do not substitute it for the published
artifact while retaining the published artifact's verification claims.

**Check the contract, not just the pulse.** After a roll, compare the
served `/openapi.json` path count with the committed
`docs/api/openapi.json` from the commit you deployed. Against a from-source
server it passes trivially; against a deployment it is the stale-deploy
alarm, and it catches exactly the case the first rule describes — one
package version spanning two API contracts. Two commands, one from each
side:

```bash
curl -s https://<host>/openapi.json | python3 -c "import json,sys; print(len(json.load(sys.stdin)['paths']))"
python3 -c "import json; print(len(json.load(open('docs/api/openapi.json', encoding='utf-8-sig'))['paths']))"
```

The two numbers must agree. This is CI's step 10 extended past the
registry to the running host.

### Deploy the published Docker Hub image

This walkthrough uses **Server 1.0.0**, published at
[iokaio/munarium](https://hub.docker.com/r/iokaio/munarium). The image contains
the Server and its CLI; PostgreSQL, Matrix and your application UI are
separate services. Pulling it needs no Docker Hub login. Application
authentication is still required once it is running.

#### Choose the artifact and the storage layout

Use PowerShell 7.3 or later and Docker Desktop with Linux containers.
You need no Rust compiler, source checkout, cloud account or model key for
the checks below. Docker selects AMD64 or ARM64 from the multi-platform
index. Keep its digest in the deployment definition rather than pinning
only one architecture's child manifest:

```powershell
$image = 'docker.io/iokaio/munarium@sha256:9f5cd5dec2f52cef26aabce625ace1390164e4930c93b5cc0d2177806b498d4c'
docker pull $image
if ($LASTEXITCODE -ne 0) { throw 'Server image pull failed' }
docker buildx imagetools inspect $image
if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect the published index' }
```

The equivalent human-readable tag is `iokaio/munarium:1.0.0`. Release notes
record source commit `154b9c4f33b13a9f58a2229f578c0d7644410449` and provide
[the Cosign verification command](https://github.com/iokaio/munarium/releases/tag/v1.0.0).
The signature binds the index digest to its CI signing identity. SBOM and
provenance attestations are attached to the index; the Rust SBOM inventory
also includes build and development dependencies, not just linked runtime
code. Neither the signature nor the SBOM proves your own configuration is
correct: that is what the deployment checks establish.

There are three different things to persist:

| Data | Configuration | Persistence in this walkthrough |
|---|---|---|
| Ledger, metadata, runbooks, sessions and provider configuration | `MUNARIUM_STORE=postgres`, `MUNARIUM_DATABASE_URL` | PostgreSQL's `pgdata` volume |
| Raw ingested document bytes | `MUNARIUM_SOURCE_STORE` | `pg`, in the same PostgreSQL backup as the ledger |
| Optional derived search artifacts and local cache | `MUNARIUM_DATASTORE_*` and `MUNARIUM_RETRIEVAL_MODE` | Not enabled; ordinary PostgreSQL retrieval needs no Server data volume |

`MUNARIUM_STORE=postgres` does **not** imply `MUNARIUM_SOURCE_STORE=pg`.
With a PostgreSQL ledger, the source-store default is `az`, which needs
Azure configuration. Set `pg` explicitly for a self-contained installation.
Conversely, `MUNARIUM_STORE=memory` and `MUNARIUM_SOURCE_STORE=mem` lose
their data with the process; mounting an otherwise unused directory on that
container does not make memory storage persistent.

#### Create a standalone Compose project

Choose an empty directory outside the source checkout. The example uses
HTTP **18080** and gRPC **15051**, leaving the usual development ports free.
Change the host ports in `.env` if they are occupied. Keep the same directory
and project name when redeploying so Compose reuses the same named volume.

```powershell
$ErrorActionPreference = 'Stop'
if (Test-Path .env) { throw 'Use an empty deployment directory; preserve existing secrets' }
$dbPassword = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
$tokenSecret = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
@"
MUNARIUM_IMAGE=$image
POSTGRES_PASSWORD=$dbPassword
MUNARIUM_TOKEN_SECRET=$tokenSecret
MUNARIUM_HOST_HTTP=18080
MUNARIUM_HOST_GRPC=15051
"@ | Set-Content .env -Encoding utf8
$dbPassword = $null
$tokenSecret = $null
```

The generated hexadecimal password needs no URI escaping. `.env` is a
private deployment file: restrict access and keep it out of Git. Compose
interpolates it into the configuration; the Server itself does not read
Compose's `.env`. Values supplied by the shell can override `.env`, so
avoid stale `MUNARIUM_*` or `POSTGRES_PASSWORD` shell variables when following
the example. `docker compose config` prints resolved secrets unless you use
`--quiet`; do not attach that expanded output to an issue.

Save this as `compose.yaml` in the same directory:

```yaml
name: munarium-hub
services:
  postgres:
    image: pgvector/pgvector:pg16@sha256:ccc6e83d6e35e931dc7c5def2022729d5a6c370318d099181995567ff1fb4d6b
    restart: unless-stopped
    environment:
      POSTGRES_USER: munarium
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:?Set POSTGRES_PASSWORD in .env}
      POSTGRES_DB: munarium
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U munarium -d munarium"]
      interval: 3s
      timeout: 3s
      retries: 20
  server:
    image: ${MUNARIUM_IMAGE:?Set MUNARIUM_IMAGE in .env}
    restart: unless-stopped
    depends_on:
      postgres:
        condition: service_healthy
    environment:
      MUNARIUM_STORE: postgres
      MUNARIUM_DATABASE_URL: "postgres://munarium:${POSTGRES_PASSWORD:?Set POSTGRES_PASSWORD in .env}@postgres:5432/munarium"
      MUNARIUM_SOURCE_STORE: pg
      MUNARIUM_RETRIEVAL_MODE: postgres
      MUNARIUM_AUTH_MODE: static
      MUNARIUM_STATIC_TOKENS: devtoken:dev-tenant:rw
      MUNARIUM_TOKEN_SECRET: ${MUNARIUM_TOKEN_SECRET:?Set MUNARIUM_TOKEN_SECRET in .env}
    ports:
      - "127.0.0.1:${MUNARIUM_HOST_HTTP:-18080}:8080"
      - "127.0.0.1:${MUNARIUM_HOST_GRPC:-15051}:50051"
volumes:
  pgdata:
```

`devtoken` is this guide's **local-only** credential. Before exposing a
deployment, replace it with a private token registration and configure TLS
ingress. The token format is `token:tenant:role`; `rw`, `ro` and `mgmt` are
distinct roles. A `rw` token is enough for these examples. Token issuance and
management-only routes need `mgmt`; do not solve a role failure by disabling
authentication. `MUNARIUM_TOKEN_SECRET` is a separate signing key of at least
32 bytes and must remain stable across Server recreations.

Start the project and define a bounded readiness check:

```powershell
docker compose config --quiet
if ($LASTEXITCODE -ne 0) { throw 'Invalid Compose configuration' }
docker compose pull
if ($LASTEXITCODE -ne 0) { throw 'Image pull failed' }
docker compose up -d
if ($LASTEXITCODE -ne 0) { throw 'Compose startup failed' }
$httpPort = (docker compose port server 8080).Trim().Split(':')[-1]
if ($LASTEXITCODE -ne 0) { throw 'Cannot discover the HTTP port' }
$base = "http://127.0.0.1:$httpPort"
function Wait-MunariumReady {
    $deadline = [DateTime]::UtcNow.AddMinutes(2)
    do {
        try {
            $response = Invoke-WebRequest "$base/readyz" -TimeoutSec 5
            if ($response.StatusCode -eq 200) { return }
        } catch { }
        Start-Sleep -Seconds 2
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Server did not become ready; inspect docker compose logs server postgres'
}
Wait-MunariumReady
```

The PostgreSQL role created by this local example can run the embedded
migrations, including `CREATE EXTENSION IF NOT EXISTS vector`. The image
supplies the extension files. A healthy PostgreSQL container means the
database accepts connections; `/readyz` establishes that the Server finished
its own startup. Open `$base/admin` and `$base/docs` in your browser.

#### Verify identity, access and a persisted write

Check the running container's requested image and its actual image ID.
Then read the version from the process. These are complementary checks:
the package version alone is not a build identity.

```powershell
$serverId = (docker compose ps -q server).Trim()
if ($LASTEXITCODE -ne 0 -or -not $serverId) { throw 'Server container is missing' }
docker inspect --format '{{.Config.Image}} {{.Image}}' $serverId
if ($LASTEXITCODE -ne 0) { throw 'Container inspection failed' }
Invoke-RestMethod "$base/version"
$headers = @{ Authorization='Bearer devtoken'; 'X-Munarium-Uid'='user-1' }
$writeHeaders = $headers.Clone()
$writeHeaders['Idempotency-Key'] = [guid]::NewGuid().ToString()
$version = Invoke-RestMethod "$base/v1/versions" -Method Post -Headers $writeHeaders -ContentType application/json -Body '{}'
$versionId = $version.version_id
$writeHeaders['Idempotency-Key'] = [guid]::NewGuid().ToString()
$claim = @{claim_type='fact';subject='deployment';key='storage';value='postgres'} | ConvertTo-Json
Invoke-RestMethod "$base/v1/versions/$versionId/claims" -Method Post -Headers $writeHeaders -ContentType application/json -Body $claim
docker compose exec -T -e MUNARIUMCTL_TOKEN=devtoken -e MUNARIUMCTL_UID=user-1 server /mmctl runbook list
if ($LASTEXITCODE -ne 0) { throw 'Authenticated CLI request failed' }
```

A fresh database has no applied runbooks; an empty successful list is
expected. A running Server image does not preload your shapes, corpora or
provider keys. Apply your reviewed configuration with `/mmctl` as described
in [the CLI guide](../ops/mmctl.md), then ingest your own documents. Files
passed to a CLI running inside a container must be mounted or copied into
that container; a host path is not automatically a container path.

Force a Server replacement, then check the fact survived. This exercises
the database-backed write path without making any model calls:

```powershell
docker compose up -d --no-deps --force-recreate server
if ($LASTEXITCODE -ne 0) { throw 'Server recreation failed' }
Wait-MunariumReady
$facts = Invoke-RestMethod "$base/v1/versions/$versionId/facts" -Headers $headers
if (-not @($facts.facts | Where-Object { $_.subject -eq 'deployment' -and $_.key -eq 'storage' -and $_.value -eq 'postgres' }).Count) {
    throw 'The persisted fact was not recovered after recreation'
}
```

Keep `$versionId` in your verification record for future rolls. Repeat this
with a representative document and runbook before accepting an application
deployment. Direct gRPC is published separately on 15051; a REST check does
not validate a proxy's gRPC routing. Run your client's gRPC acceptance suite
against that endpoint, using HTTP/2 and the same tenant/uid contract.

#### Back up and rehearse a restore

`docker compose down` keeps the named `munarium-hub_pgdata` volume.
`docker compose down -v` deletes it. Recreating the Server does neither.
A retained Docker volume is not an independent backup and does not protect
you from deleting the Docker Desktop data disk.

For a local logical backup, stop Server writes and have PostgreSQL write a
custom-format dump to a file. Copy the file out with Docker instead of
piping binary output through PowerShell text handling:

```powershell
docker compose stop server
if ($LASTEXITCODE -ne 0) { throw 'Could not stop Server writes' }
docker compose exec -T postgres pg_dump -U munarium -d munarium -Fc -f /tmp/munarium.dump
if ($LASTEXITCODE -ne 0) { throw 'Database backup failed' }
New-Item -ItemType Directory -Force backups | Out-Null
docker compose cp postgres:/tmp/munarium.dump ./backups/munarium.dump
if ($LASTEXITCODE -ne 0) { throw 'Backup copy failed' }
Get-FileHash ./backups/munarium.dump -Algorithm SHA256
docker compose start server
if ($LASTEXITCODE -ne 0) { throw 'Server restart failed' }
Wait-MunariumReady
```

Rehearse restoration into a **new** database. The following drill refuses
to create a database that already exists; it never drops the live database.
The restore destination here is on the same local PostgreSQL service, so
it proves the dump can be read and applied, not recovery from machine loss:

```powershell
docker compose exec -T postgres createdb -U munarium munarium_restore_check
if ($LASTEXITCODE -ne 0) { throw 'Restore destination must be a new database' }
docker compose cp ./backups/munarium.dump postgres:/tmp/munarium-restore.dump
if ($LASTEXITCODE -ne 0) { throw 'Restore input copy failed' }
docker compose exec -T postgres pg_restore -U munarium -d munarium_restore_check --no-owner --no-privileges --exit-on-error /tmp/munarium-restore.dump
if ($LASTEXITCODE -ne 0) { throw 'Restore drill failed' }
docker compose exec -T postgres psql -U munarium -d munarium_restore_check -v ON_ERROR_STOP=1 -c 'SELECT count(*) AS restored_events FROM ledger_events;'
if ($LASTEXITCODE -ne 0) { throw 'Restored ledger could not be read' }
```

The walkthrough wrote an event, so the restored count must be nonzero. A
production recovery drill also starts a Server against the restored database
and checks known facts, source bytes and an application run. Keep the dump
outside the Docker host and handle it as private application data. With
`MUNARIUM_SOURCE_STORE=pg`, it includes document bytes; with `file`, `az`,
`s3` or `gcs`, back up or retain those stores separately. See
[backup and restore](../ops/backup-restore.md) for PITR and recovery limits.

#### Use an existing PostgreSQL service

Remove the Compose `postgres` service, the Server's `depends_on` entry and
the `pgdata` declaration. Set the Server environment entry to
`MUNARIUM_DATABASE_URL: ${MUNARIUM_DATABASE_URL:?Set the database URI}`
and put the full URI in `.env`, preserving `MUNARIUM_STORE=postgres` and
your explicit `MUNARIUM_SOURCE_STORE` choice. PostgreSQL must already have
the database and pgvector extension files available. The application role
needs schema migration privileges; a database administrator can enable
`vector` first if extension creation is restricted.

For Docker Desktop, a PostgreSQL instance on the Windows host is reachable
through [`host.docker.internal`](https://docs.docker.com/desktop/features/networking/networking-how-tos/#connect-a-container-to-a-service-on-the-host).
For example, use `postgres://user:password@host.docker.internal:5432/munarium`.
Inside the Server container, `localhost` names the Server container itself.
With Compose-managed PostgreSQL, `postgres:5432` names the database service;
it does not use a host-side port mapping. Permit the connection in PostgreSQL's
listener, host authentication rules and firewall. Percent-encode reserved URI
characters in passwords, or use a generated hexadecimal password.

For a remote database, configure verified TLS, for example
`?sslmode=verify-full`. If the CA is not in the image's trust store, mount its
certificate read-only and add `sslrootcert=/path/in/container/ca.pem` to the
URI. The hostname must match the certificate. Store connection credentials
in your deployment platform's secret facilities; do not put them into an
image layer. `POSTGRES_PASSWORD` on the database container initializes a
new database volume only: changing it in `.env` does not rotate an existing
database role's password.

#### Move document bytes or enable derived indexes

For a filesystem source store, set `MUNARIUM_SOURCE_STORE=file` and
`MUNARIUM_FILE_ROOT=/data/sources`, and mount persistent storage at
`/data/sources`. Prepare that directory for UID/GID 65532 before starting
the Server; a new root-owned mount can otherwise fail with permission denied.
Persisting source bytes there supplements PostgreSQL rather than replacing
the ledger. A multi-replica deployment needs a shared, consistently mounted
filesystem or an object store; separate per-container disks do not share bytes.

Azure Blob (`az`), S3 (`s3`) and Google Cloud Storage (`gcs`) use the same
source-store interface. Configure the account/bucket and credentials from
[source-stores.md](source-stores.md). Credential *references* such as
`MUNARIUM_S3_SECRET_KEY_REF` name an environment variable or secret file;
they are not the access-key value itself. Changing a backend variable does
not copy existing objects to a new location. Plan and verify any existing
document migration, and retain the original bytes until it is complete.

Derived retrieval is a separate choice. Keep `MUNARIUM_RETRIEVAL_MODE=postgres`
for the tested setup above. If you enable `mirror`, `shadow` or `datastore`,
retain PostgreSQL and configure a writable `MUNARIUM_DATASTORE_LOCAL_ROOT`.
A local artifact store additionally uses `MUNARIUM_DATASTORE_ARTIFACT_STORE=file`
and `MUNARIUM_DATASTORE_ARTIFACT_ROOT`; staging uses
`MUNARIUM_DATASTORE_STAGING_ROOT`. Make each mounted path writable by 65532
and keep durable artifacts outside the replaceable container layer.
`MUNARIUM_DATASTORE_BUILDER=enabled` runs the build-job worker; it does not
create a collection or select an active index on its own. Follow §8A's
build, validation and rollout procedure before selecting datastore serving.
Setting an environment variable alone is not an index cutover.

#### Upgrade, roll back and diagnose

Before an upgrade, save the current image digest and configuration, take a
database backup, and review the target release's migration and configuration
notes. Change only `MUNARIUM_IMAGE` in `.env` to the verified target digest,
then run `docker compose pull server` followed by
`docker compose up -d --no-deps server`. Recheck identity, readiness and the
saved fact/application checks. Use the previous digest and the same commands
for a compatible rollback. Retain the database volume and signing secret in
both directions; do not rerun the initial secret-generation step. Schema
compatibility must be checked for the particular release, even when migrations
are additive. This procedure replaces one local Server container and can
briefly interrupt clients; it is not a rolling high-availability deployment.

| Symptom | Check and correction |
|---|---|
| `no matching manifest` or a Windows image-platform error | Docker Desktop must use Linux containers; the release supports AMD64 and ARM64. |
| Port allocation fails | Change only the host ports in `.env`; keep container ports 8080 and 50051. |
| PostgreSQL is healthy but Server exits | Read `docker compose logs server`; check database URI, migration permissions and pgvector availability. |
| Startup asks for an Azure storage account | Set `MUNARIUM_SOURCE_STORE=pg` for the local recipe, or configure the intended Azure backend. |
| Password authentication fails after editing `.env` | An existing PostgreSQL volume still has its original role password. Reconcile the role and URI; do not delete the volume to repair credentials. |
| Static auth fails or `/v1` reports `uid-required` | Send the configured bearer token and `X-Munarium-Uid`; use a management token only for management routes. |
| Filesystem sources or datastore hydration report permission denied | Inspect mount ownership and grant UID 65532 the required access. The image has no shell for an in-container repair. |
| `/healthz` works but the app does not | Check `/readyz`, actual container identity, authenticated operations and the app's applied runbooks/provider configuration. |

### The deployment path: the chart and the module

What ships is a ladder with three rungs, and most readers should not start
at the top.

| | What it gives you | Where |
|---|---|---|
| 1 | A persistent Server on a laptop | The Docker Hub Compose walkthrough above; `docker compose up --build` from `server/` when developing the source |
| 2 | A real cluster install, no cloud account | `helm install` from [deploy/helm/munarium](../../deploy/helm/munarium/README.md) against kind or minikube |
| 3 | A cloud deployment with managed identity and blob storage | [deploy/terraform/example-aks](../../deploy/terraform/example-aks/README.md), which consumes the chart |

Rung 1 is the whole evaluation. Nothing on rungs 2 and 3 is needed to
decide whether Munarium does what you want.

To select the public image in the existing Helm chart, set
`image.repository=docker.io/iokaio/munarium` and `image.tag=1.0.0` in your
reviewed values file. The current template renders `repository:tag` and has
no separate `image.digest` value; supplying a digest as the repository would
produce an invalid reference with an extra tag. Use the immutable version
tag with this chart, and verify the running pod's `imageID` against the
appropriate architecture manifest of the published index. The chart still
requires an explicit repository even though older comments in its values
file predate the public image. Its CNPG, gateway, identity and secret setup
requirements are unchanged; a published image does not install those services.

**The Helm chart.** One release is one CloudNativePG Postgres cell, the
`munarium-server` deployment, and all three API planes: REST, the Envoy
gateway (Gateway API CRDs + Envoy Gateway, `gateway.enabled`), and a
LoadBalancer straight to the direct gRPC port (`directGrpc.enabled`). The
database URL is not a value; it comes from the CNPG-generated app secret,
wired by the deployment template. The readiness probe is the ops plane's
real `/readyz` (§5), which is why §5's bind-failure asymmetry on that
listener matters in a cluster. The chart's status is stated in its README
and repeated here because it is the honest version: its first install ran
on kind and verified end to end (§13 entry 8); the workload-identity token
exchange and the gateway plane remain unexercised because they need a real
cloud cluster.

The chart deliberately wires **no `MUNARIUM_TOKEN_SECRET`** and **no
`MUNARIUM_SECRET_*`** provider key. Those are secrets, and the chart has no
opinion about where your secrets live. Inject them from your secret store —
a Key Vault or Secrets Manager reference surfaced as an environment
variable, a Secrets Store CSI mount read through the `file:` form of the
secret seam (§5's `resolve_secret`), or a Kubernetes Secret patched into the
deployment's env, which is the workaround the chart README spells out until
an `extraEnv` hook lands. The `staticTokens` default is a pair of demo
literals; [security-posture.md](../security-posture.md) explains why they
must not survive contact with production.

**The example AKS module.** It builds a small cluster, installs
CloudNativePG and Envoy Gateway, creates a user-assigned managed identity
federated to the `munarium` ServiceAccount and a storage account with no
access keys (the workload identity is the only credential), and installs
the chart. Nothing is assumed to exist except a subscription you can write
to; there are no `data` sources reaching for resources someone else made.
It is authored and syntax-checked (the `terraform` job above), not applied
end to end, and its README says so before it says anything else. The parts
most likely to need changing for a real deployment are listed there too —
node sizes, storage replication, the cluster SKU, a private endpoint — as
the smallest choices that let the example be read in one sitting rather
than as recommendations.

The consolidated operator procedure for both shapes — gate, build and push,
install or upgrade, verify the rollout, roll back, back up — is
[ops/deployment-runbook.md](../ops/deployment-runbook.md). This chapter
does not repeat it; the rest of this section is the reasoning an operator
can apply to any platform.

### Rolling forward and back, safely

Five rules, each learned the expensive way on some platform and none
specific to one.

**Configuration before image.** Environment variables, secrets, identities
and roles live in your infrastructure definition, not in the image roll. A
new image may require a newly declared variable, secret or role, so check
and apply that definition *before* the image rolls. The other order
strands an unhealthy image behind a config change that failed after it.

**Never trust the stable hostname.** Every orchestrator keeps the last
healthy replica serving while a new one repeatedly crashes, and a probe of
`https://…/healthz` right after a roll may therefore validate *last week's*
image. Verify the new pods by identity first (which image digest is
running), by readiness second, and only then probe the shared ingress. The
deployment runbook's §4 is titled "Verify the rollout, not the hostname"
for this reason.

**Check the contract, not just the pulse.** The path-count comparison
above, run against the rolled host, closes the loop from the commit that
started the deploy to the surface on the wire.

**Rehearse the rollback against the database you will retain.** Migrations
run on connect (§8), and the additive-only policy preserves existing schema
objects. That supports image rollback, but it does not replace checking the
particular versions' migration validation, configuration and data compatibility.
Test the previous image against a restored copy of the upgraded database
before relying on that rollback path. Never edit an applied migration or
delete migration records to force startup; recipe 3 in §6 explains the
checksum contract. A database restore is a separate recovery operation with
its own possible loss of writes after the restore point.

**Destruction is its own flow.** A deploy that would destroy
infrastructure — a storage account, a database — must refuse to proceed
and hand you to an explicit teardown (`terraform destroy`, or deleting the
resource group the example module created). Never let a config repair
become data loss because it ran under the same command as an image roll.

### What runs when

The whole system, one table:

| Event | lint-test | cargo-deny | terraform | What you get |
|---|---|---|---|---|
| Push to main / PR touching `server/**` or `matrix/contract/**` | ✅ | ✅ | ✅ | Green or red; nothing published, ever |
| `workflow_dispatch` | ✅ | ✅ | ✅ | The same gates, run by hand |
| `gates.ps1` (local) | mirror¹ | mirror² | n/a | Local confidence; nothing deployed |
| `build.ps1 -Image` (local) | n/a | n/a | n/a | A local image you name, tag and push yourself |
| `helm upgrade` / `terraform apply` (operator) | n/a | n/a | n/a | Your cluster runs the image you named; verify per the rules above |
| Published Server release | n/a | n/a | n/a | Ioka publishes a signed multi-platform `iokaio/munarium` image, SBOM/provenance and immutable version tag |

¹ the full lint-test list including the platform and cluster
conformance steps (mirrored across all three copies).
² only if `cargo-deny` is installed; warns and defers to CI otherwise.

Read the table's pattern once more because it sums up the chapter. The
rows that *prove* run everywhere in the same automatic way. The rows that
*publish* run only when a person pulls a lever, and nothing in this
repository pulls one for you. For your own builds, retain the gate results
and source identity beside the image. For published releases, start from
the released digest and verification metadata, then validate your actual
deployment configuration and application behavior.

### Where you stand

You can now trace a change from `git push` to a deployable image. You know
which gates see it, what each copy runs, and why those copies cite each
other. You know how the image is built, how to name it so the name cannot
lie, and how to verify a roll on any platform: identity, readiness,
contract, in that order.

You still need to know what to do when that path, or the running server it
creates, fails. You need to read the startup log's plane-by-plane messages,
follow a request id through middleware, understand exit codes, and diagnose
the ten failures that most people meet. Section 11 is that triage guide.

## 11. Debugging and observability

§5 walked the machinery: nine startup steps, the middleware spine, the
request id, and the interaction writer. This chapter explains how to use
them at 2 a.m. The server may not start. A request may fail after startup,
or every check may be green while a user reports a problem.

This chapter is written for people doing that work. Every diagnostic below
ran against a real server using the §2 memory-mode recipe and the alternate
source ports. Every failure transcript is a real capture, not a created
example. When §5 already explains a mechanism, this chapter links back to
it. When the answer is "you cannot observe that yet," this chapter says so
and points to §13.

### The observability you have

The list is short, and you should know it well. You have structured tracing
with an environment filter, five startup lines that prove the boot, a
request id on responses, and an interaction row for each `/v1` call. You
also have one authenticated endpoint that probes your model providers.
There is no metrics endpoint or OTel support. The end of this chapter states
that limit clearly. Tonight, these five tools are what you have.

**The startup lines are your first diagnostic.** §5 annotated them line
by line; here is the habit. A healthy memory-mode boot is five lines:

```text
2026-08-12T03:00:58.488724Z  INFO munarium_server: starting config.store=Memory http=127.0.0.1:18080 grpc=None
2026-08-12T03:00:58.489010Z  INFO munarium_server::state: source bytes store backend="mem"
2026-08-12T03:00:58.489054Z  INFO munarium_server::state: document intelligence disabled (MUNARIUM_DOCINTEL=none) — local extraction only
2026-08-12T03:00:58.492538Z  INFO munarium_server: REST plane listening addr=127.0.0.1:18080
2026-08-12T03:00:58.494749Z  INFO munarium_server: ops listening addr=127.0.0.1:19090
```

Read the lines as a checklist, not a greeting. `starting` proves that the
config parsed and shows the *resolved* store and addresses. Check it first
when the server listens somewhere you did not expect.

`backend="mem"` is where a source store sent to the wrong place reveals
itself. In production, you expect `backend="az"`. The blob-failure entry
below starts with this line. Each plane that binds then prints one line.

A *missing* line is also useful. If `ops listening` is absent, the ops port
was taken, so the server warned and continued (main.rs:172-174). In a
deployed environment, health probes then fail against an otherwise working
server. See §5's ops-plane section. If `direct gRPC plane listening` is
absent and the first line says `grpc=None`, the listener is off by config,
not broken.

**`MUNARIUM_LOG` is tracing's env filter.** The subscriber initializes
before anything else can fail (main.rs:46-51). It reads `MUNARIUM_LOG` in
the standard `target=level` comma syntax: `munarium_server=debug`,
`munarium_server=debug,sqlx=warn`, or just `debug` for everything. The
default is `info`. A malformed filter falls back to `info` rather than
exiting. This is the one planned fail-open in startup because log detail is
not worth stopping the server (§5 step 2).

The filter reaches every crate in the binary, which gives it much of its
value. `munarium_server` limits its log volume. At v0.1.2, it has exactly one
debug-level site. You can leave `munarium_server=debug` on at low cost. Open
the dependency tree, such as `sqlx`, `tower`, or `object_store`, only for a
short and planned check. Here is the same §2 boot with one extra variable:

```powershell
$env:MUNARIUM_LOG = 'munarium_server=debug'
.\target\debug\munarium-server.exe
```

The boot lines are identical to the five above. The difference appears
after requests begin. Here is the output after two `POST /v1/versions`
calls:

```text
2026-08-12T03:02:23.226988Z DEBUG munarium_server::interactions: interaction (memory store; not persisted) uid=user-1 method=POST /v1/versions
2026-08-12T03:02:23.266198Z DEBUG munarium_server::interactions: interaction (memory store; not persisted) uid=user-2 method=POST /v1/versions
```

That one debug site (interactions.rs:81) is more useful than it looks:
the memory store has no interactions table, so the audit trail moves to the
log. As a result, `munarium_server=debug` gives any memory-mode server a free
per-request access log with uid and method. On a Postgres-backed server, the
same records go to the database, and the debug log stays quiet.

**Where the log actually is.** A foreground boot writes to your
terminal. Script-launched servers split their streams. Review §3's "Where
the server logs land" section before you need it. The live test tiers
redirect stderr to
`$env:TEMP\munarium-test-server.log` (black-box, test.ps1:84-85) and
`$env:TEMP\munarium-ent-server.log` (platform, test.ps1:132-133), while
tracing's stdout streams into the script's console output.

On a healthy run, those files are **empty**. They catch early fatal stderr
from config errors. That is why "the console went quiet where the startup
lines should be" should lead you to inspect that file.

In deployed environments, the container streams go to the platform's log
store. The startup checklist is the same wherever those five lines appear.

### Correlation: one string across three records

Every request that reaches the handler stack gets a request id. It starts
with `req-` and adds a uuidv7 created by the middleware
(middleware.rs:118). Sorted ids therefore follow time order. The same
string goes three places:
the `x-munarium-request-id` response header (middleware.rs:137-141), the
`http` tracing span that wraps the handler (middleware.rs:124), and the
interaction row (§5's middleware-spine section walked the mechanism).
This creates a useful 2 a.m. habit. When a user sends you a failing
response, use the header value as a grep key for both the log and the audit
trail. It matters most for *errors*. Here is a real failing request with its
headers:

```text
HTTP/1.1 409 Conflict
content-type: application/problem+json
x-munarium-request-id: req-019ff3eae556766291244204186f57e1
```

Running this chapter's examples revealed one important detail. The
middleware's own rejections happen before it creates the id. These are
`uid-required`, `uid-mismatch`, and the body-too-large 413. The uid checks
at middleware.rs:71-89 and the body-size check at 101-116 all run before
line 118. Those three responses therefore have **no** request id. The
cookbook below includes a capture. Every request that passes the uid
contract gets the header, whether it succeeds or fails. Account for this
exception if your alerting depends on the header.

**The interaction capture is the flight recorder.** Every `/v1` REST
request becomes a row: tenant, uid, request id, method and path, HTTP
status, latency, plus whatever domain context the handler attached
(session, runbook, collections) and the token's `jti`. This is the chain of
custody §Introduction promised. Bodies are captured under three
redaction rules you should be able to recite before you rely on the
trail (§5 covered the mechanism; this is the operator's summary):

- **The cap.** Request and response bodies are stored verbatim only
  when they are JSON and at most `MUNARIUM_INTERACTION_BODY_MAX` bytes,
  default 32 KiB (config.rs:233). Above the cap, or non-JSON, the row
  stores `{sha256, bytes_len}` instead (interactions.rs:12-27): a
  200 MB upload costs the audit table ~100 bytes yet stays
  content-addressably identifiable.
- **Secrets are never stored.** Handlers whose responses carry secret
  material (minted capability JWTs) set `redact_response`, and the row
  stores a marker instead of the body (interactions.rs:49-64).
- **gRPC rows are envelope-only.** They store method, uid, tenant, status,
  and latency, but never proto bodies (middleware.rs:8-10). If your compliance posture
  needs full-body capture, the REST plane is the plane you put in front
  of users.
- **Streams are recorded at end of stream, never buffered** (2026-08-23).
  A `text/event-stream` response passes through the capture middleware
  frame by frame; a body wrapper (`SseCapture`, middleware.rs) records the
  row when the stream ends — the request body under the cap as usual, the
  response as `{streamed: true, content_type, bytes_len}` — and the
  handler's `StreamOutcome` slot supplies the session/runbook attribution
  and the status the stream's *terminal event* carried (an SSE `error`
  is a 4xx/5xx row even though the HTTP status was 200). Today the one
  streaming route is the session turn (§17).

Two operational details complete the picture. Under overload, capture is
best-effort *by design*. The record travels through a bounded channel to one
writer task. When the channel fills, the system drops the record with a
warning instead of slowing the data plane (interactions.rs:5-8, 69-73). An
audit row matters, but it is not worth blocking a write path.

The reports API provides the query surface for the trail. Use `GET
/v1/reports/audit` with the mgmt role. "What did user X do at 14:32" is one
filtered call, not a SQL query.

### /healthai: the BYOK diagnostic in one call

The provider gateway has several failure points: three families, three tiers
(fast, capable, and — since 2026-09-01 — frontier), and keys resolved from
the environment at call time. Checking them one at a time is difficult, so
the server has one endpoint that probes all of them. `GET /healthai` fires
nine parallel probes: three provider families times three tiers. Each one
requests a minimal completion from the built-in default model with a
30-second timeout (providers_api.rs:429-509).

A missing credential is not an error. Its checks report `skipped` with a
`detail` that names the exact environment variable needed to enable them.
This makes the endpoint free to call on a keyless server. It can therefore
be shown here without spending anyone's tokens. This is the real response
from the chapter's memory-mode boot:

```powershell
curl.exe -s http://127.0.0.1:18080/healthai -H "Authorization: Bearer devtoken"
```

```json
{ "healthy": false,
  "checks": [
    { "provider": "anthropic", "tier": "fast", "model": "claude-haiku-4-5",
      "ok": false, "skipped": true, "latency_ms": null,
      "detail": "credential env var 'MUNARIUM_SECRET_ANTHROPIC' is not set" },
    { "provider": "anthropic", "tier": "capable", "model": "claude-sonnet-5",
      "ok": false, "skipped": true, "latency_ms": null,
      "detail": "credential env var 'MUNARIUM_SECRET_ANTHROPIC' is not set" },
    { "provider": "anthropic", "tier": "frontier", "model": "claude-fable-5-1",
      "ok": false, "skipped": true, "latency_ms": null,
      "detail": "credential env var 'MUNARIUM_SECRET_ANTHROPIC' is not set" },
    { "provider": "openai", "tier": "fast", "model": "gpt-5.4-mini",
      "ok": false, "skipped": true, … },
    … ]
}
```

Read the shape once and the diagnostic story writes itself. Each check
names the family, tier, and exact model that the tier resolves to today. A
skipped check also names the exact `MUNARIUM_SECRET_*` variable to set.

`healthy` is true only when at least one family was probed *and* every
probe passed (providers_api.rs:506-508). If all checks skip, the result is
`healthy: false`. A BYOK gateway with no usable credentials is unconfigured,
not healthy.

On a keyed server, the same call answers "is it my key, my network, or my
model name?" in one step. For each family and tier, a check returns `ok`
with latency and stop reason, a provider error shortened to 300 characters,
or `timed out after 30s`.

The probe also handles one known trap. A reasoning model can spend its
whole completion budget on hidden reasoning and return empty text. The
probe allows 512 tokens (256 until 2026-09-02, when every per-call budget
was doubled) and treats an empty completion as a failure. Its
detail includes the stop reason (providers_api.rs:461-478).

Two warnings, both encoded in the route's own doc comment
(providers_api.rs:606-608). The call is **authenticated**. Any role works,
but a token is required, because on a keyed server every call spends
real provider tokens and must not be drive-by reachable like
`/healthz` (the keyless capture above still needed `devtoken`; without
it, 401). And for the same reason it belongs in your runbook as a
*diagnostic*, not in a monitor loop. Nine completions per poll on
every configured family is a bill — the frontier probe prices its ~256
tokens at the top of each family — and §20's integration guidance
treats it accordingly.

### GET /v1/providers: the free disclosure counterpart (2026-08-23)

The paid probe answers "do my keys work?" A different question comes up
far more often in an application: "which model will this call actually
use?" — asked *before* a user's turn, by a UI that wants to label its
model selector truthfully, and asked without spending anything to find
out. `GET /v1/providers` (`list_providers`, providers_api.rs:654-686) is
that answer. It lists every applied `ProviderConfig` for the tenant plus
the synthesized env-backed defaults, and for each one resolves the
`fast`, `capable` and `frontier` tiers through the same
`resolve_complete_model` the turn path uses — so what it reports is what
a turn would get, not a copy of the tier table. Zero provider calls, zero tokens. `credentialRef` is
never echoed; the listing carries only `credential_ok`, whether the
reference currently resolves.

Executed on this chapter's keyless memory-mode boot, after applying an
OpenRouter provider config named `demo-openrouter` — the shape of the
committed `runbooks/providers/example-openrouter.yaml` — with
`POST /v1/providers`:

```powershell
curl.exe -s http://127.0.0.1:18080/v1/providers `
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1"
```

```json
{ "providers": [
    { "name": "demo-openrouter", "provider": "openrouter", "source": "applied",
      "credential_ok": false,
      "fast": "deepseek/deepseek-v4-flash", "capable": "z-ai/glm-5.2",
      "frontier": "z-ai/glm-5.3" },
    { "name": "default-anthropic", "provider": "anthropic", "source": "default",
      "credential_ok": false,
      "fast": "claude-haiku-4-5", "capable": "claude-sonnet-5",
      "frontier": "claude-fable-5-1" },
    { "name": "default-openai", "provider": "openai", "source": "default",
      "credential_ok": false,
      "fast": "gpt-5.4-mini", "capable": "gpt-5.4",
      "frontier": "gpt-5.6-sol" },
    { "name": "default-openrouter", "provider": "openrouter", "source": "default",
      "credential_ok": false,
      "fast": "deepseek/deepseek-v4-flash", "capable": "z-ai/glm-5.2",
      "frontier": "z-ai/glm-5.3" } ] }
```

Applied configs come first, name-sorted, then the three defaults in
`DEFAULT_PROVIDER_PRIORITY` order. The applied config resolves to the same
models as `default-openrouter` because it exists to let a runbook's
`allowOverrides` *name* a family, not to change credentials or models —
the example file's header comment says so, and the listing proves it. A
`fast`/`capable`/`frontier` member that is simply absent means the tier
does not resolve for that config (the helper swallows the resolution
error and reports nothing rather than a guess).

Two differences from `/healthai` decide which one you call. This route is
**safe to poll and safe to cache**: a UI can mirror it behind its own
endpoint with a short TTL and label the chat toolbar
with the exact model the next question will use (§17). And it is
REST-only — there is no `ProviderService` twin for the listing, and
`/healthai` is REST-only for the same reason ([api/rest.md](../api/rest.md)
carries the parity statement). `credential_ok: false` here and `skipped`
there name the same condition; the difference is that this one cost you
nothing to learn.

### The triage cookbook

This is the heart of the chapter: symptom → meaning → first move for the
failures most people meet. The tree below shows the whole section in one
picture. The entries that follow add real transcripts.

![Triage: a server that will not start branches to exit 2 for a config error, exit 1 for a startup error, or a migration checksum mismatch that is recovered by recreating the database. A failed request branches by status: 400 uid-required or invalid-input, 401 unauthenticated, 403 forbidden by level or compartment, 404 unknown asset, 409 head conflict, and 422 gate refusal](images/ch11-triage-tree.svg)

**The server won't start: read the exit code first.** The triple was
demonstrated live in §5's startup-order section. This chapter does not
repeat those captures. The summaries here build on them.

**Exit 2** prints `config error: …` on stderr with *no* `starting` log line.
Your environment is wrong, and nothing was touched. The message names the
variable and the way out, so follow it. Section 5 step 3 captures this
example: `MUNARIUM_STORE=postgres requires MUNARIUM_DATABASE_URL`.

**Exit 1** prints the `starting` line and then `startup error: …`. The config
parsed, but the outside world refused. A database may reject login, a pool
may fail to build, or a source store may fail closed. The `starting` line
is the key to this diagnosis. Its presence clears your environment-variable
spelling and points to connectivity or credentials. Section 5 step 5
captures `password authentication failed for user "munarium"`.

The third case is **`migration N was previously applied but has been
modified`**. Someone edited a shipped migration, so sqlx's checksum now
fails on every database that already applied it. The server refuses to
become ready. In a deployment, the new pods never become healthy.

This case cannot be shown safely because it requires damage to migration
history. Section 2's gotcha almanac gives the recovery: `docker compose
down -v` for the local compose database, and drop-and-recreate or a restore
for any other. The answer that avoids it is always "an additive migration,
never an edit" (recipe 3).

**400 `uid-required`: the #1 first-request failure.** Captured against
the chapter's server, headers and all:

```powershell
curl.exe -si http://127.0.0.1:18080/v1/versions `
  -H "Authorization: Bearer devtoken" -H "Content-Type: application/json" `
  -H "Idempotency-Key: ch11-uid-demo" -d '{}'
```

```text
HTTP/1.1 400 Bad Request
content-type: application/problem+json
content-length: 188

{"type":"https://munarium.ioka.io/problems/uid-required","title":"uid required","status":400,"detail":"X-Munarium-Uid header (REST) / munarium-uid metadata (gRPC) is required on every /v1 request"}
```

The first move is in the detail: send `X-Munarium-Uid`. The Introduction's
first-contact walkthrough covers this failure in full. It explains why the
uid exists and how the capability-JWT bridge lets the token's `sub` stand
in.

Notice what the transcript leaves out: there is no
`x-munarium-request-id` header. This is the pre-span exception described in
the correlation section above. If your first request returns
`invalid-input` instead, check `Content-Type` before the body. Curl's `-d`
uses form encoding by default, and §5's middleware section captures that
exact 400.

**409 `head-conflict`: normal, not a bug.** This is an optimistic
concurrency result. You sent `expected_head` with the head you read, but the
lineage moved forward. Here it is triggered on purpose against a lineage
whose head is 1:

```powershell
curl.exe -si http://127.0.0.1:18080/v1/versions/$V/claims `
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1" `
  -H "Content-Type: application/json" -H "Idempotency-Key: ch11-c2" `
  -d '{"claim_type":"fact","subject":"hero","key":"hair","value":"red","expected_head":999}'
```

```text
HTTP/1.1 409 Conflict
content-type: application/problem+json
x-munarium-request-id: req-019ff3eae556766291244204186f57e1
content-length: 182

{"type":"https://munarium.ioka.io/problems/head-conflict","title":"optimistic head conflict","status":409,"detail":"head conflict: expected seq 999, actual 1","expected":999,"actual":1}
```

The `expected`/`actual` extension members are machine-usable (and ride
gRPC as identically-named `ErrorInfo` metadata, error.rs:292-300). The
first move is a loop, not a fix. Read the head again and decide whether your
write still makes sense after the change. Retry with the new head and a
fresh idempotency key. The retry is a *new decision*, not a redelivery.

The client libraries implement this as `propose_claim_with_retry`. The
write-loop guide
([clients/docs/guides/write-loop.md](../../../clients/docs/guides/write-loop.md))
is the reference. Treating 409 as an error that should page someone is the
most common way to misread this system.

**422 `idempotency-mismatch` vs replay: a pair shown as one.** The
idempotency contract has two sides, and you must understand them together.
The Idempotency section in rest.md is the contract. Here it is in action.
First, record a command under a key:

```powershell
curl.exe -s http://127.0.0.1:18080/v1/versions/$V/claims `
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1" `
  -H "Content-Type: application/json" -H "Idempotency-Key: ch11-k1" `
  -d '{"claim_type":"fact","subject":"hero","key":"hair","value":"red"}'
# -> { "claim": { "id": "claim-847e5dc192a14e11a09ce95cc9e7a6fa",
#                 "value": "red", "seq": 2, … }, "head_seq": 2 }
```

**Face one: the same key with a *different* body is rejected**, because
an idempotency key names a command, not a slot to reuse:

```text
HTTP/1.1 422 Unprocessable Entity
content-type: application/problem+json
x-munarium-request-id: req-019ff3eb0e8877a089f55e713285d499
content-length: 196

{"type":"https://munarium.ioka.io/problems/idempotency-mismatch","title":"idempotency key replayed with a different request","status":422,"detail":"idempotency key replayed with a different request"}
```

(That was the identical call with `"value":"black"`.) The same 422 occurs
when a key is reused *across planes*. A REST command's key replayed on its
gRPC twin is a mismatch, not a cross-format replay. Records are grouped by
transport (rest.md:51-53, and the full scope table at
[errors.md#idempotency-scope](../api/errors.md#idempotency-scope)).
First move on an unexpected mismatch: you are reusing a key you meant
to create again. Common causes are a copied transcript or a client that
caches keys across retries of *different* commands.

**Face two: the same key with the *same* body is the safety working.**
Repeat the original call byte-for-byte:

```text
{"claim":{"claim_type":"fact","id":"claim-847e5dc192a14e11a09ce95cc9e7a6fa","key":"hair", … "seq":2, …},"findings":[],"head_seq":2}
```

The recorded response comes back with the same claim id and `seq: 2`. A
`GET …/head` confirms `{"head_seq":2}`, so nothing ran twice. This is why
the write loop can safely retry a timed-out command.

Keep one limit from the retry contract in mind (rest.md:172-176). Keys are
recorded **after** the command finishes, so there is no reservation while a
command is running. A client that sends the command again while the first
delivery may still be running can execute it twice. The official clients
retry commands only after connection-phase failures and clear load-shed
responses. Follow their pattern.

**Blob-path failures: boots green, first blob call hangs.** Section 6 recipe
6 owns this incident. In a deployed Azure environment, a server can pass
config and health checks but still time out on its *first* blob operation.

The reason: Azure Container Apps and App Service have no classic IMDS at
169.254.169.254. The platform injects `IDENTITY_ENDPOINT` and a rotating
`IDENTITY_HEADER`. Only a `from_env()`-style constructor reads them. Any
other setup sends the managed-identity path to link-local IMDS, where every
blob call times out. The notes at
munarium-store-objects/src/lib.rs:185-191 record this lesson.

The pattern is clear: laptop fine, deploy fine, health green, and the first
upload hangs for about its timeout. First, read the `source bytes store
backend=` startup line to confirm which backend was built. Then see recipe
6's warning. Remember the general lesson: a laptop never uses managed
identity, so this failure stays hidden until deployment.

### What you have since 2026-08-17, and what you still do not

The observability surface grew three pieces (closing §13's entry 2):

- **`GET /metrics` on the ops plane (:9090)** — Prometheus text
  exposition, hand-rolled with zero new crates. RED counters and latency
  histograms per plane and route template, provider call/latency/token
  counters, runbook step transitions, load-shed and audit-writer
  counters, pool/queue gauges. No tenant/uid/instance labels by rule
  (metrics.rs's header explains both). §5's "ops plane, honestly" carries
  the captured exposition.
- **Four dashboard report views** — `GET /v1/reports/timeseries`,
  `/endpoints`, `/runbooks`, `/sessions` (mgmt role): bucketed ad-hoc
  aggregates over the interactions/sessions/runbook tables, which means
  they aggregate across every instance sharing the database. The audit
  trail also gained keyset pagination (`before`/`next_before`).
- **The `/admin` operator console** — fourteen server-rendered HTML pages
  (overview, traffic, endpoints, usage, providers, runbooks, collections,
  storage, sessions, tokens, audit, findings, matrix, health — the storage
  and matrix pages joined after this list was first written and were
  missing from it until 2026-08-31) plus per-object viewers
  (runbook, shape, chronology rules, run, collection, session) on the REST
  plane: inline SVG, zero JavaScript, mgmt bearer or the `/admin/login`
  cookie flow. The monitoring half shipped 2026-08-17; the control-plane
  half — the inventory of what is deployed, each object's applied YAML,
  and three actions (token issue/revoke on the mgmt credential; runbook
  gate approval, which asks for the rw credential per submission because
  it is an rw operation) — replaced the short-lived authoring pages on
  2026-08-27 (§21A). The health page reads process state and the
  effective non-secret configuration only and never calls the paid
  `/healthai`.

What you still do not have, by stated decision rather than gap: OTel
trace export. The exporter stack is a dependency graph §9 would have to
swallow, and the tracing logs + `/metrics` + reports API cover today's
operations; architecture.md §12 records the posture and what would
reopen it. The interaction rows remain your per-user, per-call record —
queryable and attributable, now paired with a real scrape target.

One Docker build detail to know: all sqlx queries are runtime-checked
strings — there is no `.sqlx/` offline data and no prepare step, by
stated posture rather than omission since 2026-08-17 (the Dockerfile's
old `SQLX_OFFLINE=true`, which gated nothing, was removed; §13 entry 3,
closed). Do not look for a prepare step that rebuilds offline data; it
does not exist. The metrics gap above is tracked in §13's ledger. Check
that list before you assume something else is missing rather than
undocumented.

### Where you stand

You can now boot a server and read its five-line proof of startup. You can
increase log detail with `MUNARIUM_LOG` and know which debug site is useful.
You can follow one request id from a user's error through the span to the
interaction row. You can also test all six provider probes in one
authenticated call.

You can now triage the known failure list. It includes the three startup
failures by exit code, the uid contract, the 409 loop, the paired meanings
of 422, and the blob path that fails only in production.

A shared discipline shapes all of this. Error slugs stay stable, messages
name the way out, and gaps are stated instead of hidden. This is not unique
to this chapter. It is a house style enforced in review. Section 12 covers
that style and its central honesty rule.

## 12. Conventions, style, and the honesty rule

Every chapter so far has cited conventions in passing: §6's "update the
registry the same hour," §9's dependency comments, §10's fail-loudly rule,
§11's error messages that name the way out. This chapter collects them,
because they are not separate habits. They are one rule wearing different
clothes, and the rule is the product.

Munarium sells auditability. The Introduction's third founding failure
is a system that cannot say what it knew in March. The whole architecture
exists so the answer to "what did the system know and why did it say that"
can always be rebuilt. Its parts include the append-only ledger,
disputed-not-dropped gates, pinned reads, and interaction records.

A codebase that makes this promise cannot have false documentation,
outdated comments, vague error messages, or stub endpoints that fake
success. **A doc that lies breaks the same contract as a gate that lies.**
A user who finds a false feature claim in the README learns the same lesson
as a user who finds a made-up citation. Outputs from this system cannot be
trusted without a separate check. As the Introduction notes, that lesson
does not fade.

So the conventions below are not style preferences, and review should not
treat them as optional polish. They apply the Preface's honesty rules to
code and docs: verify claims, state gaps, and track versions. This closes a
loop. The book follows the codebase's rules because they were always the
same rules.

![The honesty rule at five layers: gates record a blocked claim as disputed rather than dropping it; the API answers UNIMPLEMENTED rather than faking an RPC; CI treats a server that never became ready as an error rather than a skip; the docs state gaps in a ledger; and a conformance scenario that cannot run says SKIPPED, loudly](images/ch12-honesty-stack.svg)

### 12.1 Docs voice: why-first, measured, ledgered

Open any document under `server/docs/` and three properties repeat.

**Why-first.** The docs explain the reason before (or instead of) the
mechanism, because the mechanism is in the code and the reason is not.
[ops/mmctl.md](../ops/mmctl.md) opens not with a flag list but with
the design argument: "every operation is exactly one REST call, so
anything mmctl does, CI can do with `curl`. That is the point." The
README's environment table rarely states a bare default. `MUNARIUM_DOCINTEL`
is "**Off by default because it is paid and egresses**";
`MUNARIUM_FILE_ROOT` is required because there is "no silent temp-dir
default."

When you add a row, endpoint description, or guide section, lead with the
reason. A reader who knows why can rediscover how. The reverse is not true.

**Measured facts only.** Numbers in these docs were counted, not
estimated. The whitepaper's index entry carries its own enforcement
clause: its status table "is measured from shipping code; keep it true"
(docs/README.md:13). The row's image size, §8's table counts, §9's
binary budget all reproduce from the tree. If you cannot reproduce a
number, do not write it. If you can, explain how you got it, as this guide
does with its commands.

**Honesty ledgers.** Where something is missing, the docs say so in a
structured place rather than trailing off. §13 of this guide is the
canonical one; the example AKS module's README states what has and has
not been run before it says anything else; [api/grpc.md](../api/grpc.md) advertises "honest
transport gaps" in its one-line index description; the Helm chart's
README opens with a dated status ("first install validated 2026-08-18",
naming what remains unexercised) — it opened "authored, deployed
nowhere yet" until that install ran.
A ledger entry costs one line. The alternative is a reader finding the gap
at 2 a.m. That costs trust which may never return.

Two mechanical rules keep the voice honest at scale.

**The generated-files rule.** Two documents under `docs/api/` are
outputs, never inputs: [openapi.json](../api/openapi.json) (from
`cargo run -p munarium-server -- openapi`) and
[grpc-reference.md](../api/grpc-reference.md) (from `gen-grpc-docs`).
Their index entries say **Generated … never hand-edit**
(docs/README.md:23-24), and CI makes the rule mechanical with two drift
gates (server-ci.yml:131-144): the OpenAPI gate regenerates the spec and
asserts parsed-JSON equality; the gRPC gate regenerates the reference and
diffs it. A hand edit, even one that looks correct, fails the next push
with a message naming the regeneration command. §12.2 below executes
both gates against this checkout.

**The index rule.** [docs/README.md](../README.md) opens with it: "If
you add a document, add it here; an unlisted doc is an unread doc."
Before that index existed, the docs were "findable only by `ls` and
folklore" (its own words). The rule generalizes: every subsystem here
keeps a human-readable registry. Examples include the route map, slug
table, environment table, and deny.toml's exceptions. Section 6 explains
why a lagging registry is worse than none: it is *believed*.

### 12.2 The drift gates, executed

Run for this chapter, on this checkout, exactly as CI runs them
(regeneration to a scratch path; the committed files untouched):

```text
cargo run -q -p munarium-server -- openapi   → parsed-JSON compare:
drift: NONE
paths: 93 schemas: 179

cargo run -q -p munarium-proto --bin gen-grpc-docs → diff:
GRPC-DOCS DRIFT: NONE
```

One honest footnote, in the spirit of the Preface's parenthetical: the
first OpenAPI comparison reported drift. Every em-dash character in the
document differed. The tree was clean, but the *harness* was wrong.
Windows Python decoded the new spec as cp1252 while reading the committed
file as UTF-8. The two copies therefore disagreed byte by byte.

Reading with an explicit `encoding='utf-8'` removed the first problem. The
gRPC diff also needed normalized line endings on a CRLF-autocrlf checkout.
After those fixes, both gates reported no drift.

There are two related lessons. A drift gate requires *you* to prove that
reported drift is real before acting. CI also runs these checks on Linux,
where neither problem exists. That is why the committed comparison lives
in server-ci.yml instead of relying on folklore.

### 12.3 Code commentary: comments state constraints the code can't

The house style for comments fits in one clause: *comments state constraints
the code can't*. The model is the body-limit
comment in the router (rest.rs:1030-1032):

```rust
// Same 256 MiB ceiling as PUT /v1/sources. Without this layer axum's
// 2 MiB default governs, and base64 is 4/3 — so a whole BATCH was
// capped at ~1.5 MB of document bytes, which no real corpus fits in.
.route(
    "/v1/ingest",
    post(crate::ingest_api::ingest_file).layer(DefaultBodyLimit::max(MAX_SOURCE_BYTES)),
)
```

Read what those three lines carry that the code cannot. First is the
*coupling*: this ceiling must track the one on `PUT /v1/sources`. The
constant `MAX_SOURCE_BYTES` at rest.rs:22 enforces it today. The comment
says the match is intentional, not a coincidence.

Next is the *counterfactual*. Delete the layer, and nothing fails loudly.
Axum's 2 MiB default takes over in silence. The *arithmetic consequence* is
that base64's 4/3 expansion leaves about 1.5 MB for document bytes. The
*stakes* are clear: no real corpus fits.

At the deletion site, a future refactorer can see exactly what will break
if they remove the "redundant" layer. The comment also explains why no test
may catch the break at once.

That is the standard. A comment earns its place by recording something
invisible in the code. It may state a cross-file invariant, a rejected
choice, a production-only effect, or a version floor tied to a security
notice.

Section 9 showed this style in a dependency list. Every line in the
munarium-extract Cargo.toml answers *why this crate, why these features, why
this floor*. In §9's words, "this manifest is what it looks like applied
to a dependency list." The kernel's module headers state their contracts
in the same way.
`munarium-core/src/gates.rs:8`: "Blocked claims are recorded `disputed`,
never dropped." This is the five-layer figure's bottom layer: the
honesty rule written where the gate lives.

And the negative space: do not comment what the code already says.
`// increment the counter` above `count += 1` is noise. It trains readers to
skip comments, so they may miss the body-limit comment when it matters. If
a comment repeats the line below it, delete one of them. If a comment no
longer matches the code beside it, that is not a minor issue. By this
chapter's logic, it is a false document at the smallest scale. Review treats
it as a bug.

### 12.4 Fail-closed error style: name the cause AND the escape hatch

§5 established the mechanics (config errors exit 2 before a listener
binds); §11 showed the triage value. Here is the *authoring* standard,
shown through the `az` source-store arm
(config.rs:257-268; the error string is one 230-character line in the
file, wrapped here with `\` for the page):

```rust
// Fail closed, exactly like MUNARIUM_DATABASE_URL under
// MUNARIUM_STORE=postgres: a missing account must not degrade to a
// silent local-bytes fallback nobody notices until a restart.
// 'az' is usually the DEFAULT under MUNARIUM_STORE=postgres rather
// than something the operator typed, so the error must explain
// both how they got here and the way out — a bare
// "az requires an account" reads as nonsense on a laptop.
let account = std::env::var("MUNARIUM_AZURE_STORAGE_ACCOUNT").map_err(|_| {
    "source store is 'az' (the default under MUNARIUM_STORE=postgres): set \
     MUNARIUM_AZURE_STORAGE_ACCOUNT, or set MUNARIUM_SOURCE_STORE=pg to keep \
     document bytes in Postgres (local/CI posture)"
        .to_string()
})?;
```

The message has three mandatory parts, and the comment above it explains
why each matters.

**The cause, including how you got here** is "source store is 'az' (the
default under MUNARIUM_STORE=postgres)." This matters because the operator on
a laptop never typed `az`. An error about a setting you never set reads as
nonsense.

**The fix** is to set `MUNARIUM_AZURE_STORAGE_ACCOUNT`.

**The escape hatch** is to set `MUNARIUM_SOURCE_STORE=pg` instead. This is the
valid local/CI setup. Naming it means the laptop user does not have to
create a storage account to run tests.

The comment also states the alternative that the code refuses: a silent
local-bytes fallback that "nobody notices until a restart." Section 10's
"never trust the stable hostname" rule is the same lesson. A system that
quietly does something other than what was asked is worse than one that
stops.

Two enforcement notes. First, the message *text* is under test:
`az_without_an_account_fails_closed` (config.rs:497-506) asserts the
error "must name the pg escape hatch, or a laptop/CI launch reads it as
nonsense." The escape hatch is a tested contract, not a courtesy. When you
write a fail-closed arm, write its matching test. The s3/gcs/file arms each
have one (config.rs:584, 696, 716). The file arm's test comment closes the
loop: "a silent /tmp fallback is the exact failure mode the az arm's
fail-closed comment warns about."

Second, the **never-inline corollary**: `resolve_secret`
(config.rs:443-459). Every credential in config is a *reference*, such as an
environment-variable name or `file:/path` for a CSI mount. It is never the
secret itself. The resolver fails closed on an unset reference
("credential env var 'X' is not set") and on an empty value. The README
environment table says it in
table voice: "`file:/path`; never the secret inline"
(`MUNARIUM_S3_SECRET_KEY_REF`, README.md:125). An error message that names
a missing credential by *reference* can be pasted into a ticket; one
that risks embedding a secret cannot. Honest errors must also be
shareable errors.

### 12.5 "Never fake": the trio

The Preface's second commitment quotes the codebase as precedent:
"unshipped gRPC methods answer `UNIMPLEMENTED` rather than faking
success, the deploy job fails loudly rather than green-skipping when
unconfigured." Here
are the three verified instances behind that sentence. Copy this pattern
whenever you are tempted to stub.

**UNIMPLEMENTED beats a stub.** The gRPC module opens with the rule
in its header (grpc_data.rs:1-4): RunbookService shipped its shape half,
and "the executor RPCs answer UNIMPLEMENTED until the milestone. **Never fake**."
A gRPC client probing the surface gets a truthful, machine-readable
answer it can branch on; a stub returning fabricated success would
poison every integration built against it. If your milestone ships half
a service, ship the honest half and `UNIMPLEMENTED` for the rest.

**Fail-loudly beats green-skip.** A gate that cannot do its job must
fail and say exactly what to set, never skip and report green: CI's
black-box step errors with `server never became ready` rather than letting
a conformance run against nothing pass, and `gates.ps1` throws on a port it
cannot claim rather than testing whatever answers there (§10). Note the one
deliberate contrast: `cargo-deny` may *warn* when absent locally, because
CI still enforces it on every push, so nothing is skipped where it is
required. The rule is not "always fail." It is "never let absence of work
look like work."

**Exit 2 beats pretending to parse.** `mmctl` documents its command
list with the claim "Everything below is the complete surface; there
are no hidden flags" (ops/mmctl.md:24), and an unknown command gets
the full usage text on stderr and exit 2 (ops/mmctl.md:75; the
`_ =>` arm at munarium-cli/src/main.rs:219-233). No guessing, no partial
match, and no silent success. The docs' claim of completeness is itself an
honesty artifact. It turns "is there an undocumented flag for this?" from
a search into a claim that can be tested.

The general test: when a component cannot do its job, does the caller
*find out, immediately, with the fix named*? Gates answer with
`disputed` plus findings; RPCs with `UNIMPLEMENTED`; config with exit 2
and an escape hatch; CI with a red X naming the variables. Anything
that would answer with fabricated green fails review here.

### 12.6 History is annotated, never rewritten

This guide's own captures show the rule. §15's and §21's turn transcripts
carry a fused score of `0.0328`, which the merge no longer produces —
§13.5 entry 17 changed the arithmetic — and they are left as captured,
with a note at each site saying what changed. §3's test-ladder figure was
drawn with four tiers, and the caption beside it says the fifth is in the
table and not yet in the figure. The tempting edit in each case would be
to patch the number or redraw the figure. That would make the docs
*currently* accurate and *historically* false: the captured run would then
claim a number it never produced. The annotation preserves both truths
and, as a bonus, records *why* the number moved.

This is not a documentation nicety; it is the ledger philosophy applied
to prose. The kernel never edits an accepted fact. It supersedes it
with a new one, keeping the old value readable at its pin. §8 shows the
same principle compiled into SQL: a shipped migration "is never edited,
never reordered, never deleted" because sqlx checksums applied
migrations, and even a comment fix arrives as a new comment-only
migration (0015, §8's exemplar). Facts, schema, docs: append and
annotate, never rewrite. When you must change a recorded measurement, a
stated decision, or a published claim, write the new value *beside* the
old with the date or version that separates them.

### 12.7 Docs move in the same change

The Preface's third commitment names its own enforcement examples: "the
same rule the README env table and the API docs live under." The API
docs half is mechanical (§12.2's drift gates). The env-table half is
practiced, and you can verify it by inspection rather than git history.
Every arm in `source_store_from_env` (config.rs:249-441) has a row in the
README table (README.md:112-128), down to shared language.

The README's `MUNARIUM_AZURE_STORAGE_ACCOUNT` row says "fails
closed, like `MUNARIUM_DATABASE_URL` under `MUNARIUM_STORE=postgres`," which
is the config.rs comment's own analogy; the `MUNARIUM_FILE_ROOT` row's "no
silent temp-dir default" is the file-arm test's assertion. v0.1.2's
whole feature was new config arms (s3/gcs/file via `object_store`), and
the table has all of them: the docs moved with the code because they
were the same change.

§6 already made this a recipe step ("update the registries in the same
change") and named the anti-pattern: "not 'code now, docs in a follow-up.'
The follow-up is the lie the Preface's third commitment exists to forbid."

Section 12 makes this the review standard. A PR that changes an environment
variable, endpoint, error slug, or dependency is incomplete without an
update to the matching registry. "I'll document it after" asks reviewers to
merge a known lie. Section 6's change-surface figure is the reviewer's
checklist. Every spoke touched by the change must appear in the diff.

### 12.8 Versioning and release: one number, asserted everywhere

The version story is deliberately small enough to hold in your head.

**One source of truth.** `[workspace.package] version` in the root
`server/Cargo.toml` (line 29; `1.0.0` as this is written). Every crate
takes `version.workspace = true`; `/version` reports
`CARGO_PKG_VERSION`; the OpenAPI `info.version` carries it.

**The tag must agree.** A release is a `server-v<version>` tag. Releases
are cut by Ioka outside this repository, and the first thing a release
procedure must do is assert tag == workspace version and refuse to build
otherwise, because `/version` reads `CARGO_PKG_VERSION` and a tag that
disagrees with `Cargo.toml` ships an image whose reported identity is
wrong. Section 10 states the same rule from the deployment side: a package
version is not a build identity, so name images by commit and pin them by
digest. An identity that cannot lie is worth a failed build.

**Clients move in lockstep.** Each client pins itself to the server
version explicitly with a `TARGET_SERVER_VERSION` constant, and a server
bump moves all eight sites. These are the four package versions
(`clients/rust/Cargo.toml:7`, `clients/python/pyproject.toml:8`,
`clients/dotnet/Directory.Build.props:11`, `clients/java/build.gradle.kts:25`)
and the four constants (rust `munarium-client/src/lib.rs:61`, python
`munarium_client/_options.py:9`,.NET `MunariumClient.cs:69` (spelled
`TargetServerVersion` in C#), and Java `Munarium.java:7` (spelled
`TARGET_SERVER_VERSION`, same as Rust)). All eight read `1.0.0` today. This
was checked for this chapter because an unchecked lockstep claim is exactly
the kind that drifts.

**This guide is a ninth site.** A change that invalidates a claim here
commits to updating this document in the same change. When you bump the
workspace version with a behavior change, this document is on your change
surface too.

### 12.9 Commit style: what CONTRIBUTING.md requires

Every commit carries a Developer Certificate of Origin sign-off
(`git commit -s`), checked on every pull request by `dco.yml`; an unsigned
commit fails that check before anything else runs. Beyond the sign-off,
message format is free-form, lowercase, and brief — there is no Conventional
Commits scheme, ticket-number template, or commit-lint hook, and this guide
will not invent a policy the tree does not enforce.

The project relies on the *content* gates around each commit, not its
message, for history. These include CI's full ladder on every push to main
and drift checks that put docs in the same commit as code. The dated notes
in this guide and its sibling documents carry the detail that other
projects put in commit bodies.

If the project adopts a message format, record that decision here. Until
then, the honest statement is simple: messages are informal, and the record
lives in CI, migrations, and the docs.

### 12.10 The bridge to §13

Every practice in this chapter produces the same artifact when it
encounters something missing: a *written, findable statement of the
gap*. The `[gap]` markers across §1–§11, fail-loudly messages that name
missing config, UNIMPLEMENTED answers, and dated status lines that name
what remains unexercised all lead to one place.

That place is §13, the known-gaps ledger. It stays current and appears last
as the honesty rule's standing output. Read it as you would a gate finding.
It is not an embarrassment. It is the system telling the truth about
itself.

## 13. Known gaps ledger (kept current, deliberately last)

### 13.1 Why a gaps ledger is a feature

This chapter is the Preface's second commitment in its final form. §12
closed with an observation. Every honesty practice that meets something
missing produces the same artifact: a written, findable statement of the
gap. All those statements lead to one place.

This is that place. It is the last chapter of Part I because it covers what
most systems refuse to write. A stated gap can be scheduled, estimated, and
closed. A hidden gap gets found again at 2 a.m., often after someone trusted
the missing part.

Check this ledger *before* you decide that something is missing rather than
undocumented. If you find a gap that is neither here nor marked **[gap]**
inline, the guide violates commitment 1. File it and fix it.

Every entry follows one skeleton, so the ledger stays greppable and a
closed entry is easy to audit. The fields are **What's missing**,
**Evidence** (file:line or the chapter that measured it), **Impact** (who
hits it and how), **The shape of the fix** (enough to open a ticket, not a
design doc), and **Status** (open | folklore | closed | retired). Every
entry also names the chapter with the full discussion. The ledger is an
index, not a replacement for that discussion.

The register is deliberately undated. Each entry states the gap or the
decision as it stands in this tree; when it moved, and through which
commits, is version control's job, not this chapter's. Source comments
cite entries by number (`dev-guide §13 entry 12`, say), so **numbers are
stable identifiers, not positions**: an entry keeps its number when it
closes, and an entry whose subject has left this repository keeps its
number with a one-line retirement note rather than disappearing.

### 13.2 Open

No numbered entry is open. That is the goal state, and also a snapshot:
the next gap found gets the next number and lands here. Three sub-items
closed *with* their entries stay visible where they are tracked rather
than here:

- The Helm chart's workload-identity exchange and gateway plane are
  unexercised. The chart README's status says so; both need a real cloud
  cluster, which the kind install that closed entry 8 could not provide.
- Fetch-on-cite re-retrieval is not ported into turn verification. The
  corrective round re-serves what the turn already served; extending it
  is the documented next step (verification.rs header; entry 10).
- The streaming turn (`POST /v1/sessions/{id}/turns/stream`) and the
  provider listing (`GET /v1/providers`) are REST-only. `session.proto`
  has no server-streaming `Turn` and `ProviderService` no List (entry 16).

### 13.3 Folklore

Empty, and that is the goal state for this section. Folklore is knowledge
that lives only in heads or in this guide; when a future entry lands here,
the fix is always the same — encode it where the mistake would be made,
then move the entry down. §2's gotcha almanac is where setup-time gotchas
live in the meantime, and each of its entries names the file that now
carries the rule.

### 13.4 Sharp edges, documented

Two judgment calls belong on this board without being gaps.

The first: **the server has no blend backend.** Nothing in
`/v1/search` knows the ledger exists. Serving canonical facts beside
document retrieval requires two application-layer calls:
`GET /v1/versions/{id}/context` plus `POST /v1/search`, assembled into
one prompt. Section 18 documents the pattern: serving canon up front, ahead
of the retrieval residual, is the part that decides the answer. The two-call
form reproduces that order.

This is a design choice, not a missing feature. Retrieval remains a pure
document surface with a clear provenance story. Under §14's division of
labor, the prompt belongs to the application.

This choice appears here because the first person to look for a `ledger`
member in the search filter may think it was forgotten. If the composition
becomes a burden across applications, discuss a convenience surface as a
product change. It is not a bug report.

The second: the middleware's **pre-span rejections carry no request
id**: `uid-required`, `uid-mismatch`, and body-too-large reject before
the id is created. Those responses have no `x-munarium-request-id` header.
Section 5's correlation section states this exception, and §11 shows it
live by reading what a transcript leaves out.

This is a documented detail, not a missing feature. By design, these
rejections happen before the instrumented span exists. It appears here
because it *looks* like a gap when you first try to trace a uid-required
400 and find nothing to search.

If this causes a real problem, such as a support flow that must trace
pre-span rejections, move the entry to Open. The fix is small: create the id
first and reorder the middleware. Until then, documented is the right
status. This section keeps that difference visible.

### 13.5 Closed

The ledger records exits too. A closed gap that silently vanishes
teaches nobody anything. Entries are in number order; three unnumbered
items that never had time to be open follow them.

**Entry 1 — the crate boundary check covered one crate of three.** Closed:
the boundary step greps `munarium-access` with the same banned list as
`munarium-core`, asserts `munarium-providers` carries no
`munarium-store-*`/`munarium-retrieval-*` crate, and asserts
`munarium-api-types` depends on nothing of the workspace but
`munarium-proto`, in both mirrors (server-ci.yml and gates.ps1, each
comment-citing the other). §4's boundary table reads **CI** in every row
and quotes the check in full.

**Entry 2 — no `/metrics`, no OTel export.** Closed one way and resolved
the other. The ops plane serves `/metrics` in Prometheus text format —
hand-rolled exposition, zero new crates, RED/provider/runbook/queue metrics
with hard cardinality rules (no tenant/uid/instance labels) — plus a real
`/readyz` sharing the REST plane's store probe; four `/v1/reports/*`
dashboard views and the `/admin` HTML dashboards ride the same surface.
Trace export is a stated non-goal until a real backend demands it
(architecture.md §12 records the posture). §5's "ops plane, honestly"
carries the captured exposition; §11 the full inventory.

**Entry 3 — `SQLX_OFFLINE=true` was a no-op.** Closed by re-scoping: the
dead variable is gone from the Dockerfile, whose header states the real
posture — runtime-checked query strings, with conformance against both
store backends as the query-drift net. The `sqlx::query!` + `.sqlx/`
machinery returns only if the SQL slice push-down lands, which is a
deliberate deferral; §8 and §11 state the posture.

**Entry 4 — gRPC half-start on an occupied port.** Closed with the
clustering lifecycle work, in the shape the entry proposed: bind the
listener inline before the spawn, log `listening` only after the bind
succeeds, hand the bound socket to the task (`serve_with_incoming_shutdown`).
An occupied gRPC port is a loud startup death, matching the REST plane. The
same change made `shutdown_signal` handle SIGTERM (orchestrators finally
trigger the drain) and flipped `/readyz` to 503 `draining` on the stop
signal. §5's startup walk tells the story.

**Entry 5 — provider live smokes: named in a comment, not implemented.**
Closed: contract.rs carries one live smoke per family
(`live_anthropic_smoke`, `live_openai_smoke`, `live_openrouter_smoke`),
double-gated vacuous-skip on `MUNARIUM_LIVE_PROVIDER_TESTS=1` plus the
family's `MUNARIUM_SECRET_*` key, asserting transport shape only. §3 tells
the full story; CI never sets the gate.

**Entry 6.** Retired: concerned deployment tooling that is not part of
this repository.

**Entry 7.** Retired: concerned deployment tooling that is not part of
this repository.

**Entry 8 — the Helm chart was authored, deployed nowhere.** Closed by
its first install, on a kind cluster with the CNPG operator and a locally
built image (gateway and direct-LB planes disabled, since kind has no
Gateway API CRDs and no load balancer). The cell came up 2/2 with pgvector,
both server replicas passed the real-Postgres readiness probe, and the
release verified end to end: a gated write accepted, read back under the
ro token, `munarium_build_info` on `/metrics`. Exactly as the entry
predicted, the install was the chart's integration test — it found two real
defects, fixed in the same change: the pinned cell image tag was
webhook-rejected as an invalid version tag (the default is now
`ghcr.io/cloudnative-pg/postgresql:16`, whose official operand image ships
pgvector), and `runAsNonRoot` failed against distroless's non-numeric
`nonroot` user (the pod securityContext now pins uid/gid 65532). Still
unexercised, and stated in the chart README: the workload-identity exchange
and the gateway plane, which need a real cloud cluster (§13.2).

**Entry 9 — gRPC parity gaps for the platform surface.** Closed: the
platform surface has gRPC twins, each calling the SAME op function as its
REST handler. `session.proto` serves CreateSession/Turn/GetSession/CloseSession
under the data-plane guard chain (capability JWT + `munarium-uid` metadata,
query scope + revocation); `AdminService` is served for
IssueAccessToken/ListAccessTokens/RevokeAccessToken (mgmt role;
tenant-lifecycle RPCs still answer `UNIMPLEMENTED` honestly);
RunbookService gained ListRunbooks/GetRunbookInfo/ValidateRunbook/
RequestRemoval/ConfirmRemoval; IngestService gained IngestFiles (native
bytes, per-item outcomes); RetrievalService gained the collections CRUD.
Verified live via grpcurl against the pg store — shape→collection→runbook→
ingest→session→turn→close end to end, close→turn refused
`FAILED_PRECONDITION` (mmp:session-not-open) — and the cross-plane
conformance run held. The live probe also caught a real regression in the
trailer-status wrapper (trailers-only error responses lost their END_STREAM
flag because the body wrapper did not delegate `is_end_stream()`; every gRPC
error read "server closed the stream without sending trailers" until the
delegation landed — middleware.rs documents it at the fix site). Reports and
`/admin` dashboards stay REST-only by design; [api/grpc.md](../api/grpc.md)
carries the parity statement. The two REST-only routes that remain are
entry 16's.

**Entry 10 — no verification-retry machinery in the session turn loop.**
Closed: runbooks may declare `completion.verification: {quotes, citations,
maxRetries}`; the turn loop then runs deterministic checks over data the
turn already holds — quoted spans ≥ 15 chars must resolve
whitespace-normalized in served hit text, bracketed `collection/chunk_id`
citations must name content served this turn — and grants up to
`maxRetries` (clamped to 2, default 1) corrective completions with the
violations *and the full served context* re-attached. The outcome rides
the response as `completion.verification` ({checks, retries,
first_pass_violations, violations}; non-empty final violations mean the
answer stands UNVERIFIED — the caller decides display). Honest delta from
the measured mechanism: fetch-on-cite re-retrieval is NOT ported — the
corrective round re-serves what the turn already served rather than
fetching cited-but-unserved documents — and extending it is the documented
next step if measurements demand it (verification.rs states this in its
header; §13.2). §17's client-side checklist remains valid — the block is
opt-in, and detection client-side is still free.

**Entry 11 — `sessions.state` had vocabulary but no API.** Closed in two
halves. `POST /v1/sessions/{id}/close` ends a session's lifecycle
idempotently (owner via capability JWT, or a static rw/mgmt token; `ro` is
refused — a close is a write), and `op_turn`'s refusal of a closed session
is the registered `session-not-open` slug (409, the actual state in the
detail). `MUNARIUM_SESSION_IDLE_TTL_SECS` (default 0 = off) arms a pg-mode
janitor in the clustering pattern — one UPDATE stamping sessions `expired`
when `COALESCE(last_turn_at, created_at)` ages past the TTL, jittered,
naturally concurrent across replicas. The refusal path enforces whatever
the column says, so the sweep is exactly the policy knob the close
predicted.

**Entry 12 — gate findings rode the write response, and only the write
response.** Closed: findings persist at write time (`gate_findings`,
migration 0017; both backends implement the `record_findings`/`findings`
store methods and a pg/mem agreement test holds them byte-identical) and
`GET /v1/versions/{id}/findings` serves them with severity/rule/pin filters
— one `as_of_seq` bounds this store like every other. Persistence is
best-effort RELATIVE TO THE WRITE by design (the claims are already
appended; failing the request would push clients into a re-appending
retry): the write response stays authoritative and a persistence failure is
a loud warn.

**Entry 13 — the chronology gate had no arming surface.** Closed:
`ChronologyRules` is a kind-sniffed declarative asset
(`POST /v1/chronology-rules`, `mmctl apply -f`) with a declarative
`chronology:` block vocabulary, and a memory version arms the sixth gate by
naming an asset in its creation metadata (`{"chronology_rules": "<name>"}`).
The write path runs `check_chronology` right after `run_gates` on BOTH
planes; certain violations join the findings stream (and the persisted
store, per entry 12). Arming against a missing asset fails loud —
misconfiguration never silently un-gates. `now` stays `None`
(deadline-absence checks need the assertion-date clock the server does not
carry — the same reason `as_of_date` stays rejected).

**Entry 14 — `find_overdue` was ported, tested, and never invoked.**
Closed: the existing promises route gained the overdue view —
`GET /v1/versions/{id}/promises?overdue_scope=<scope>` or `?final=true`
adds kernel-computed `gate.promise-unfulfilled` warn findings to the
response, evaluated over the full pinned slice before any status filter. No
new route; the path count did not move for this one.

**Entry 15.** Retired: concerned deployment tooling that is not part of
this repository. (The gotcha it tracked, MSYS path conversion when the az
CLI runs from Git Bash, is still taught in §2's almanac.)

**Entry 16 — the streaming turn plane was buffered by interaction
capture, so it did not stream.** Closed: the route was verified end to end
— contract, ordering, persistence — but the REST capture middleware read
every `/v1` response body to completion (`to_bytes(res_body, usize::MAX)`)
before forwarding it, the SSE body included, so the whole event sequence
reached the client in one burst at turn end. Found by the book's own
method: the executed capture in §17 showed `content-length: 204` on a
`text/event-stream` response, which a live stream cannot carry. Closed by
`SseCapture` (middleware.rs): when the handler's response is
`text/event-stream` the head goes out at once with its request id, the body
is forwarded frame by frame with no exact size hint (hyper: chunked), and
the interaction row + RED metrics are recorded at end of stream — the gRPC
plane's trailer-capture shape — with the handler's `StreamOutcome` slot
supplying session/runbook/collection attribution and the *terminal event's*
status, so an SSE `error` is recorded as its 4xx/5xx rather than the 200
the stream opened with. Re-executed: `transfer-encoding: chunked`, no
`content-length`, `status_class="4xx"` on the metric. Two tests hold it: a
gate-not-timer pass-through test (run against the buffered path
deliberately, it failed in 5.01 s) and a real-router route test. Still
REST-only — `session.proto` has no server-streaming `Turn` and
`ProviderService` no List — the same parity debt entry 9 retired for the
unary surface, open for these two routes and noted in §13.2.

**Entry 17 — the cross-collection turn merge starved relevant
collections.** Closed: `merge_hits` flattened per-collection hits and
sorted by their per-collection RRF scores — but those scores are
rank-derived, so every collection's rank-1 scored exactly `1/(rrf_k+1)`
whether or not the collection had anything to say. With permitted
collections ≥ `top_k` the merge degenerated into a rank-1 interleave: one
arbitrary document per collection, and the relevant collection's rank-2
mathematically unable to surface. Found live on a due-diligence runbook (a
clean-team L3 turn asking about change-of-control provisions was served
one commercial document out of 96 — and the more clearance a session
carried, the more collections it searched, the worse its context got). The
§16 passage explaining why RRF needs no score normalization was the tell
read backwards: ranks compare fairly *within* one collection's legs and
not at all *across* collections. Closed by global re-fusion: `rrf_fuse`
records each hit's raw leg measurements (`ts_rank`, cosine distance —
magnitude-comparable across collections sharing one shape and embedder) on
the `SearchHit` (`lexical_score`, `vector_distance`, additive on the REST
wire, absent on the proto plane), and `merge_hits` re-ranks the pooled
candidates globally per leg, fuses `Σ 1/(rrf_k + global rank)`, writes the
fused score onto each hit, and truncates. Query-time only — no reindex;
single-collection `/v1/search` ranking is unchanged. Two unit tests hold
it (a relevant collection's rank-2 with strong raw scores must beat an
irrelevant collection's rank-1; evidence-less legacy hits sort last). A
second, compounding defect closed with it: the lexical leg's
`plainto_tsquery` ANDs every query word, so a question-shaped query almost
never matches ALL terms, the leg comes back empty, and ranking falls to the
shallow hash-vector leg alone. Both lexical call sites now rewrite the
tsquery's `&` to `|` and let `ts_rank` order by matched-term density
(plainto emits only `&`, so the rewrite is total; empty queries still match
nothing). The sample runbooks raised `retrieval.topK` 10 → 20 — a
query-time knob, new runbook version, no rebuild. Captured turn transcripts
printed before this change show the old per-collection scores (e.g. §21's
`0.0328` = rank-1 in both legs); they are left as captured, with a note at
the site.

**Entry 18 — evidence-driven collection selection chose the collections
that USE a query's words over the ones ABOUT its subject.** Closed in the
engine, in several moves, each replayed on live pools before it shipped.
The problem: a question about an entity was answered with a refusal because
collections that *mention* the entity's words constantly (travel narratives
saying "Washington", "cities" and "visit") out-scored the collections
*about* the entity by term density, and a query-specific `collectionRoutes`
rule was rightly rejected in review as policy the engine should not carry.
The generic answer is `collectionSelection`: probe every permitted
collection with the original query and rank collections by phrase evidence
first (`select_collection_indices`: the fraction of a probe pool carrying
one of the query's own adjacent content-word pairs verbatim, then
three-strongest `ts_rank`, then three-nearest cosine, then name — a query
with no adjacent content words falls through to the density order
unchanged), blended as density × `(1 + phraseBoost × fraction)`, default 3,
so a dense collection still wins when no pool carries the phrase. Three
rules ride with it. **Selection spends the deep search; it never
excludes**: the unselected collections' probe pools stay in the merge as
their own stratum (one lexical and one vector ordering per query
formulation, RRF over ranks, `fusion.unselectedPoolWeight`), because a
probe's *original-query* `ts_rank` and the deep search's *expanded-query*
`ts_rank` are not comparable raw, and the response's `collections_searched`
lists every probed collection. **`retrieval.fusion` is weighted RRF**:
per-leg weights plus a collection-evidence leg that carries the selection
ranking into the merge as `weight / (rrfK + rank of the collection)`;
defaults reproduce the unweighted merge byte-for-byte. **A demotion can be
scoped**: `contentDemotions[].exceptCollections` exempts a catalog
collection whose metadata record *is* the content from a 1000× metadata
demotion that would otherwise exclude it from a 1,200-candidate merge — a
corpus-structure rule declared in the runbook, never a query rule. Two
visibility additions on the streaming plane, `selection` and `expansion`
progress events, let a caller see which collections a turn actually
searched and what the paid expansion call returned, and
`completion.contextCharBudget` replaced the 16,000-character engine default
that had been serving the model ten of a turn's twenty hits. The
`history-revolution` sample runbook declares all of these; the other sample
runbooks, which declare none, replay with only `retrieval`/`merge` stages
and the unweighted-merge score shape, unchanged. Unit tests hold the
blended selector, the strata, and the scoping on the measured shapes; §16
carries the discussion and [retrieval-sizing.md](retrieval-sizing.md) the
sizing rule.

**Entry 19 — a turn's first byte waited for the whole collection probe.**
Closed: the per-collection probe ran one shard at a time, so on a loaded
database the `selection` event — the first progress event, and therefore
the response's first byte — could arrive minutes into a turn and outlast an
ingress's first-byte limit, while an identical turn minutes earlier had
taken seconds. Closed by (1) bounded concurrent searching — the probe and
the deep search fan out with at most `retrieval.searchConcurrency` (default
4) in flight, results re-ordered to collection order so ranking stays
deterministic — and (2) a `probe` progress event per collection as each
probe completes, so the stream carries bytes from the first hundred
milliseconds and no turn can reach an ingress's first-byte limit however
slow the database is. What the change does *not* fix is the database's
capacity: a sequential probe at seconds per shard is a load signal, and
entry 20 is what it pointed at.

**Entry 20 — the lexical leg's cost under load.** Closed: after entry
17's OR semantics every lexical query ranked each chunk holding any query
word, and the substring demotion test then detoasted and lowered each of
those rows' full text — seconds per shard with two turns in flight. Four
closures, all measured on a local Postgres: (1) `contentDemotions[].match:
phrase` tests the marker as a word sequence against the tsvector the rank
already reads (`'text' <-> 'none' <-> 'metadata' <-> 'record'` matches the
catalog record and not prose that merely uses the words) — the text column
is no longer touched by the demotion; (2) `retrieval.minimumShouldMatch: 2`
excludes single-word rows before ranking through a GIN-indexable tsquery of
ANDed lexeme pairs built once per query formulation; (3) the vector leg sets
`hnsw.ef_search` to its candidate pool per query — the pgvector default of
40 had capped every ANN scan at forty rows however large `candidateN` was,
which is why the vector leg's global rank-1s were so often fragments — and
enables iterative scanning so the `index_version` filter stops consuming
that budget (a partition keeps the retiring version's chunks beside the
active one's); (4) every collection build ends with `VACUUM (ANALYZE)` on
its partition, because a freshly built collection was being planned on
stale or absent statistics and its first queries walked the GIN index's
unmerged pending list row by row. Two smaller fixes rode along: the lexical
rank computed `ts_rank` twice per row even when the blend weight was 0 or 1
and now short-circuits, and the two SQL paths (with and without demotion
rules) folded into one statement each with a rules CTE — an empty rule list
reproduces the prior rank byte-for-byte. Integration tests hold the
phrase-form demotion and the two-term prefilter on a real Postgres. Two
operational notes: size the server's connection pool for the fan-out (a
turn issues up to `searchConcurrency` searches at once, and a pool of 10
leaves no headroom on a small database), and the database behind the
lexical leg needs sustained cores, not burst credits. Recorded for later: a
partition holding the retiring index version beside the active one makes
both legs filter after the index scan — a `btree_gin` multicolumn GIN on
`(index_version_id, ts)` would restrict the lexical scan to the live
version, and a per-query `statement_timeout` would bound a runaway search
rather than let it hold a pooled connection; neither is needed while each
collection carries a single built version.

**Entry 21 — corpus-adaptive stop terms, and a build that inserted one
chunk at a time.** Closed, both halves measured on a local Postgres. Query
side: with OR semantics the candidate set for an entity question in that
entity's own shard was the whole shard — every chunk holds the entity's
name — and no static list can say which words are stop words *where*. A
build now records each index version's lexeme document frequencies with
`ts_stat` (every lexeme in ≥ 1% of its chunks, plus a sentinel row carrying
the chunk count; an index built before the table existed is measured once
on its first query, so nothing needs rebuilding; `retireOld` removes a
version's rows with its chunks). Under `retrieval.stopTermFraction`, a
query lexeme found in more than that share of a collection's chunks is
dropped from that collection's candidate predicate and kept in the rank;
the predicate is built per collection from the query's normalized lexemes
(computed once per formulation) together with the two-term rule, and is
never empty. The same review caught a flaw in entry 20's prefilter —
written as `(pf IS NULL OR ts @@ pf)`, which no GIN scan can drive — now
the candidate predicate itself, so the index skips the excluded rows
instead of fetching and discarding them. Build side: the index build issued
one `INSERT` per chunk — some 400,000 round trips for a large archive, each
also updating the partition's GIN and HNSW indexes on its own — and now
inserts 200 rows per statement through `unnest`, then vacuum-analyzes the
partition. Integration tests hold the stop-term exclusion, the pair rule,
and the batched build on a real Postgres.

**Entry 22 — connector claim origin and an external findings route.**
Closed: the first server-side package for Munarium Matrix. A claim may carry
an `origin` block — source, mapping version, row key, source position,
sealed evidence id — stored as JSONB (migration `0022_claim_origin`),
written into the `ledger_events` body so "rebuild, don't migrate" still
holds, and carried on both planes and all four clients; no gate reads it.
`POST /v1/versions/{id}/findings` lets a service file warn/info findings it
computed outside the gates, stamped at the current head, idempotent by
content rather than by header; a `block` is refused because this route is
not a gate. A third capability scope, `findings`, authorizes it, and
`rule_prefix=` on the findings read (and a picker on `/admin/findings`)
separates `gate.*` from `matrix.*`. The scenario that matters,
`ledger.origin-round-trips`, is in the shared set and therefore runs on
mem, pg, REST and gRPC — the cross-plane parity check. Two things it
taught: the issuance validator's scope set is closed (a good property,
found by the platform scenario refusing to mint), and migration numbers
are order — a number reserved for unshipped work belongs to nothing until
it ships.

**Entry 23 — `verifyDataViews` verified nothing.** Closed: the first
runbook with `dataViews` to run against a real Munarium Matrix failed the
step with two 401s, and then, after the credentials were fixed, passed it
while one of the two contracts' verified question was failing. Two defects
in one step, neither visible to its tests. (1) The request carried no
bearer and no uid; the execute path (`evidence_providers::MatrixProvider`)
sends both from `MUNARIUM_MATRIX_TOKEN`, and the verify request now does
the same. (2) A 200 was taken as a pass. Matrix's verify answers 200 with
per-question outcomes so a caller can see WHICH question moved; the step
read the status and not the body, so `failed: 1` was "verified". It now
reads the body and fails the step with the failing questions and their
failure strings — which is what surfaced that the contract's own
expectation was wrong (`rows: 3` written against a table its principal is
row-level-secured to one region of). Both fixes are in
`runbooks_api.rs::verify_data_views`. The crate has no HTTP mock, but it
has `axum` and a loopback interface, which is the only mock a request
header needs: `verify_data_views_against(base, token, views)` is the
step's body with its two inputs as parameters, and two tests drive it at a
stand-in Matrix on `127.0.0.1:0` — one asserts the `Authorization: Bearer`
and `X-Munarium-Uid` headers arrive, the other that a 200 with `failed: 1`
fails the step naming the question. §21C carries the discussion.

**Entry 24 — sample runbooks shipped on the retrieval defaults, and the
check that should have caught it said GREEN.** Closed as a rule rather
than a patch. The engine defaults — `topK: 10`, an unweighted merge, the
16,000-character context — are starting points, not sizing, and a runbook
that runs fourteen permitted collections at ten hits reproduces entry 17's
starvation shape runbook-side: refusals of answerable questions, more on
the capable tier than the fast one. That asymmetry is behavioural rather
than mechanical — nothing in the turn varies by tier, and a capable model
obeys "never generalise from a sample" where a fast one answers wrongly
from the same twelve sheets. Sized per corpus (`topK` 20–40,
`collectionSelection`, `fusion.vectorWeight 0.3`, `contextCharBudget`) and
measured on both tiers, the sizing fixed one of seven failing questions;
the rest were a tokenization gap (entry 25), enumeration wider than a
single turn (entry 26), and one collection flooding the enlarged
per-collection pool. Three lessons ride with it. A content check that
passes grounded refusals for citing what they checked, and misses "does not
contain", prints GREEN over wrong answers — assert the answer, not the
envelope. Runbooks live only in the `runbooks` table and are applied
through the API, so a runbook change does not ship with an image; apply
first, then roll anything that serves the same YAML. And a context budget
declared on a runbook is not binding until measured input tokens say so —
a "silently truncating" claim without that measurement is an upper bound,
not a finding. The rule that replaces per-runbook folklore is
[retrieval-sizing.md](retrieval-sizing.md).

**Entry 25 — the number-form tokenization gap, closed in the engine.**
Closed: a corpus writes `US4436097`; a person writes `4,436,097` or
`4436097`. Postgres' parser makes one token of the first and three of the
second, and every leg runs on those tokens — the lexical rank, the
bag-of-words vector, and `modelQueryExpansion`, which forbids numbers by
design — so entry 24 had recorded the two number-form questions as
unanswerable by any runbook knob. Closed generically, with no vocabulary in
the engine. Migration `0025_number_lexemes` adds `index_number_lexemes`, a
per-index-version derived table mapping each lexeme's trailing digit run to
the lexeme that carries it (`4436097 → us4436097`), populated from the
index's own recorded lexemes under the same lazy sentinel as
`frequent_lexemes`. At query time `number_query_digits` (collections.rs)
extracts the identifier-shaped numbers — a comma-grouped number of
five-plus joined digits that is not a decimal's integer part, a bare run of
five or more that is neither a decimal nor a `(19|20)……`-dated eight, a
letter-prefixed form's digit suffix — and `number_form_lexemes` returns the
corpus-observed forms over the ACTIVE index of each collection the SESSION
may search, so a collection a caller cannot search contributes no forms (a
lexeme leak is a smaller cousin of serving the document; asserted by a
test), capped at eight forms. The forms join the query for the selection
probe and the deep lexical/vector search (`effective_query`,
sessions_api.rs) — never routing, never the question the completion prompt
shows — and a query with no such number takes the old path with no extra
round trip. The operational bite: `sqlx::migrate!` expands its migration
set at compile time and nothing recompiles the store crate when only a
`.sql` file is added, so the migration reached neither the local gates nor
the first image built from the change until a source touch forced the
macro to re-expand — locally `cargo clean -p munarium-store-pg`, and in
Docker a cargo cache mount with the same staleness; the note sits at the
end of `munarium-store-pg/src/lib.rs` where the next migration author will
meet it (§2's almanac; recipe §6.3).

**Entry 26 — enumeration answered by the register, and the view that
asked at level 0.** Closed: entry 24's second class — "each quarter the
score was reduced", "the most recent postmortem", "what do our contracts
promise" — is answerable only by reading a whole set (26 constraint sheets,
40 postmortems, 30 SLA contracts), and set members tie on term density, so
no `topK` makes which twelve arrive anything but arbitrary. The structured
path is the answer (§21C): a register derived from the corpus's own sources
— never from answer keys — registered as a Matrix DataSource at the
collection's own class with a bounded QueryContract and verified questions,
bound into the runbook as a `dataViews` entry with a research profile whose
register layer is controlling and optional, with `preserveCompleteResult`,
so the turn cites sealed rows (`[evidence/<id>#CON-2023-Q2]`, "COMPLETE, 26
rows"). **The defect worth the entry: a `dataViews` entry that declares no
`accessLevel` or `compartments` asks at level 0, and every register turn
refuses `policy_denied`.** The provider asks Matrix at the INTERSECTION of
session and view (`evidence_providers.rs` — `min` of levels, compartment
overlap), no L2/L3 source class accepts level 0, and the refusal is
pre-journal on the Matrix side, so the Matrix journal shows *nothing*; the
evidence for this class of failure lives in the server's hierarchy
decision, not in Matrix. Declare the view's class explicitly. Two residuals
the register path does not touch, recorded here so nobody re-measures
them: a model that writes a fact into its narrative and omits it from its
headline list (model prose, not retrieval), and a vocabulary asymmetry
between question and corpus that pool sizing measurably does not move
(20 of 20 at `candidatePoolPerCollection` 100 *and* 40).

**Entry 27 — a review branch cut from stale `main` merged clean and could
not boot.** Closed: a review branch some thirty-five commits behind `main`
merged with no textual conflicts, every gate on its own tree green, and two
things the merge could not show. The branch added a migration numbered the
same as one `main` had gained meanwhile — sqlx applies by version, so the
merged tree collides on a fresh database and mismatches the checksum on an
existing one, refusing to start either way; the file is renumbered (it is
`0030` now), and the store-pg touch comment records why. And the branch's
stricter OpenAI decoding turned `content: null` into a provider error in
every case, which would have bypassed the truncation retry the frontier
tier's empty-text case depends on (§17 step 5); the decoder now keeps
`content: null` beside a truncation `finish_reason` as empty text, pinned by
`openai_decoder_pins_the_error_and_truncation_shapes`. The rule: re-check
the next free migration number after every merge from `main`, and run the
pg integration tier on the MERGED tree, not the branch.

**Entry 28 — reasoning models exhausting the turn ceiling: every per-call
budget doubled, then made a setting.** Closed: a reasoning-model tier
(`z-ai/glm-5.2`, the OpenRouter capable default) spent 1,024 + 4,096
output tokens on hidden reasoning over an advisory question and answered
with nothing — a ~10k-token prompt paid twice — the exact shape §17's
truncation retry and `completion.maxTokens` had guarded against for the
frontier tier on one corpus alone. Closed in two moves. First, every
per-call output ceiling was doubled (turn 1,024→2,048 with the 4× retry
intact; query expansion 128→256; the `/v1/complete` default 512→1,024; the
`/healthai` probe 256→512; the hierarchy classifier and intent tasks
16/240→32/480; the validation advisory 1,024→2,048; the authoring assist
4,096→8,192; the `history-revolution` sample runbook's explicit 2048→4096).
Second, the eight ceilings became ONE object, `MaxTokensBudgets`: seeded
from optional `MUNARIUM_MAX_TOKENS_<FIELD>` variables (a value that does not
parse or sits out of range refuses to boot), replaceable per tenant through
`GET`/`POST /v1/max-tokens` (REST-only; POST replaces the whole set — all
eight fields required, no partial update — persisted per tenant in
migration 0031 and served through a `ProviderRegistry`-style TTL cache,
`max_tokens_api.rs`), with a runbook's own `completion.maxTokens` /
`modelQueryExpansion.maxTokens` (the latter now optional in the grammar)
winning over all of it. All four clients carry the pair on their providers
plane. Reference: [docs/tokenbudgets.md](../tokenbudgets.md). The daily
spending caps (§20) were deliberately not touched: a cap on a day's spend
is not a per-call ceiling.

**Unnumbered — empty turn answers from reasoning-model tiers** (fixed the
day it was found, never an open entry): the turn completion's 1,024-token
`max_tokens` let a reasoning model spend the whole budget on hidden
reasoning and return an empty `completion.text` under an honest model
badge; §11's `/healthai` probe already guarded its own probes against
exactly this; the turn path had not. `op_turn` now pays one
truncation-aware retry at 4× when the verbatim stop reason is
`max_tokens`/`length` or the text is empty, the corrective verification
retries inherit the raised budget, and the response's token counts sum both
calls (sessions_api.rs). Recorded here because it is the kind of defect the
ledger exists to make visible: a constant that was right for one model
class and silently wrong for the next one the tier table pointed at. §17's
completion path and model economics carry the discussion; entry 28 is its
sequel.

**Unnumbered — the `test.ps1 -Platform` source-store pin** (found while
writing §3): executing `.\test.ps1 -All` failed at tier 4 on a clean
environment, because the tier's env block predated the source-store
subsystem. CI's platform step had pinned `MUNARIUM_SOURCE_STORE=pg` and
the local script had not; two mirrors of one tier drifted in opposite
directions. The fix is the pinned line in test.ps1's platform block
(test.ps1:133), with a comment citing the CI mirror it matches; §3 tells the
full story, and its tier-4 transcript is the fixed script's re-verified run.

**Unnumbered — the `GET /v1/reports/cost` Postgres 500** (found while
writing §20): the cost report against the compose Postgres drew `500
storage-error`. The rollup's `ORDER BY itok + otok` used its output aliases
inside an expression, a form Postgres resolves only as bare column
references, so the third report view had evidently never been exercised
against the Postgres store before the book called it. The fix repeats the
aggregate expressions in the `ORDER BY`, now at reports_api.rs:325-329 with
a comment recording the rule; §20 tells the full story, and its re-verified
transcript is a `200` with empty rows on a fresh tenant.

### 13.6 How to use this ledger

Four rules, all instances of disciplines you already know:

- **Add an entry in the same change that discovers a gap.** The same
  rule the registries live under (§6, §12): not "code now, ledger in a
  follow-up." The follow-up is the lie. If your change ships part of a
  surface, the unshipped part gets an entry (or an inline **[gap]**
  marker pointing here) in the same diff.
- **Remove an entry in the same change that closes it** by moving it
  to §13.5 with the closing cite, where it lives until the story is told
  well enough elsewhere (as the test.ps1 fix is told in §3) to prune.
  A ledger that only grows is a graveyard; the columns exist so entries
  can move.
- **Retire, never renumber.** Source comments cite entries by number. An
  entry whose subject has left this repository keeps its number and a
  one-line retirement note; a number is never reused.
- **Keep the skeleton.** What's missing / Evidence / Impact / Fix shape /
  Status. An entry without evidence is a rumor; an entry without a fix
  shape is a complaint. Neither belongs here.

One scope note: this is the whole book's ledger, not Part I's. As Part
II's chapters are written and measured, the gaps they surface join here
under the same rules. Entry 9 is already the pattern: a platform gap
stated where the application builders who will hit it can find it.
Entries 10 and 11 arrived exactly that way: §17's executed captures
surfaced both, and they were filed here in the same change. Entries
12–14 (and §13.4's blend judgment call) are §18's harvest, filed the
same way; entries 17–26 are §16's and §21C's.

This loops back to where the book began: the Preface's second commitment
promised that gaps are stated, not papered over, and named this chapter
as the place the statements collect. The commitment is only as good as
the ledger is current. This makes maintaining it part of the
definition of done for any change that touches what it describes.

### 13.7 Part I, closed out

Take stock of what you now hold. You can navigate twenty crates by their
boundary rules and know which rules a machine enforces (§1, §4). You can
move from a fresh clone to a passing suite and a live `/healthz` without
finding old problems again (§2).

You have the five-tier ladder and its gated satellites (§3). You know the
server's nine startup steps and three failure approaches (§5). You have a
recipe for each likely change, and each recipe is based on a past mistake
(§6). The conformance suite keeps two wire planes and every storage backend
honest (§7).

You also have the additive-only data rules (§8), dependency checks (§9),
path from push to a deployable image (§10), triage catalog (§11), and house
style that guides all of it (§12). Finally, you have this ledger. It is the
honest remainder, so you inherit our known unknowns instead of finding them
again.

Part II turns the book around. Its engineer will probably never read
`main.rs` and should not have to. Part II takes the properties that Part I
taught you to maintain and builds on them from the outside. These include
the append-only ledger, deterministic gates, pins, and parity proved by the
conformance suite.

You now know what that engineer never needs to know: which machine enforces
each promise. Thanks to this ledger, you also know where enforcement is
still one entry away. The patterns that follow were measured before they
were written down.
---

# Part II: Developing AI-enabled applications on Munarium

> The audience shifts. You have a corpus, such as a data room, case file,
> support knowledge base, or filing archive. You are building the application your
> users will actually touch: chat over the corpus, grounded Q&A, research
> assistance, red-flag review. Munarium is the governed substrate underneath:
> it holds the bytes, builds the indexes, filters retrieval by clearance,
> catches contradictions, and keeps the audit trail. **Your application owns the
> user experience; the substrate owns the truth discipline.** Every pattern
> below was built against a real corpus before it was written down.

## 14. What a governed AI application is

You have a corpus. A data room your analysts need to interrogate, a support
knowledge base your copilot should answer from, a filing archive your
research tool must not misquote. You are going to put a model in front of it,
and you are going to give the result to people who will make decisions based
on what it says. This chapter is about the difference between that
application built naively and that application built *governed*. It also covers
the one piece of architecture you must get right before any retrieval
quality, prompt craft, or model choice matters: who is asking, and how the
platform knows.

You do not need Part I to read this chapter or any that follows. Part I is
for engineers changing the platform; you are building *on* it, and
everything you touch is configuration and HTTP. Where a Part I chapter
offers useful depth (how the middleware enforces what this chapter
demonstrates, say), it is cited as optional. What this chapter does assume
is the Introduction, especially the first-contact walkthrough and the
five invariants. If you skipped it, read it now; it is the vocabulary
lesson, and this chapter is where the vocabulary starts earning rent.

### The three failure modes, from the builder's seat

The Introduction told you why the platform exists: ungrounded answers,
silent contradiction, unauditable history. That telling was about the
*system*. Here is the same list from the view of *your application*. Each
failure mode, governed, stops being a risk and becomes a feature you must
design UI for. This reframing is the working definition of a governed AI
application: **an application whose failure modes have been converted into
renderable states.**

**The ungrounded answer becomes a render decision.** In a naive RAG app, a
citation is decoration. It is a string the model emitted, and the user may
not be able to follow it. On the mesh, every retrieval answer arrives
wrapped in a provenance envelope with source hashes, an index version, and
a ledger watermark. The rule is clear: quotes must resolve
word for word in served text, and citations must name content that was
retrieved. The envelope makes these checks deterministic. A fuller loop
enforces it with verification retries; the server does not yet run those
inside a turn (**[gap]**; §17 proves it from code and gives the client-side
recipe).

What reaches your UI should be one of
two states: an answer whose citations resolve, or an honest
*insufficient-evidence* outcome. Your job is to render both. The second one
is not an error toast. Graded answer keys score
cited-or-insufficient as *correct* behavior, because an assistant that says
"the corpus does not establish this" is working, and an assistant that
invents a citation is broken in the way that ends products. §16 covers the
retrieval mechanics; §17 tells you exactly what to render per turn.

**The silent contradiction becomes a review queue.** Your application may
extract facts over time from sources that disagree. A naive pipeline lets
the newest write win, and
nobody ever knows there was a fight. On the mesh, the losing write is not
lost and the fight is not silent: a claim that contradicts accepted canon
lands as **`disputed`**, with a machine-readable finding naming the rule,
the canon value, and the proposed value (you watched this happen in the
Introduction's step 3). For a due-diligence app, that stream of findings *is
the product*. It is the red-flag queue a human reviews, with both sides of every
conflict already attached as evidence. Your application gains a workflow
surface it never had to build: triage, resolve-via-correction, or
waive-with-a-note. §18 develops this into the full canonical-memory
pattern.

**The unauditable history becomes a pin.** Sometime after launch, perhaps
sooner than expected in finance, legal, or healthcare, your
compliance team will ask some version of: *what did the assistant know when
it said that?* The naive app answers with the Introduction's shrug. On
the mesh you answer with two mechanisms that exist by construction. The
**pin**: any read, including facts, promises, and the composed context brief, can be
replayed `as_of` a sequence number, byte-for-byte, content hash and all.
And **interaction capture**: every `/v1` call is recorded with the uid that
made it, the token that carried it, and the request and response bodies.
which turns "who asked what, and what were they told" from a forensic
reconstruction into a `GET /v1/reports/audit` call. §20 builds the
compliance story on these primitives.

### The division of labor

Governed does not mean the mesh does everything. It means three parties do
exactly their own jobs, and the boundaries between them are load-bearing:

![The division of labour: your application owns the end user, the identity provider or API manager that forms the security boundary, and your UX. Munarium mints a capability JWT through POST /v1/access-tokens, serves governed data planes filtered by level and compartments, and provides the ledger, the gates, point-in-time pins and uid-attributed interactions](images/ch14-division-of-labor.svg)

**Your application** owns everything a user can see or decide: the chat and
review UX, the workflow around it, when to call a model at all, and **user
authentication**, which is the focus of this chapter. SSO, OIDC, MFA,
directories, sessions-with-the-human all live in your API-management layer
(APIM, Apigee, Kong, or a custom BFF), not in the mesh. Your layer
authenticates a human, decides what they are cleared to see, and asserts
both facts downstream.

**The mesh** owns the truth discipline: the document bytes and their
content-addressed identity, versioned indexes, retrieval filtered by
clearance *before* ranking, the append-only ledger with its gates and pins,
and the uid-attributed audit trail. It enforces what an
already-authenticated caller may touch; it never decides who a human is.

**Your model accounts** own inference. The mesh composes context and routes
completions through its provider gateway, but the keys are yours, resolved
at call time from your secret store, and the calls go from your environment
directly to Anthropic, OpenAI, or OpenRouter. **BYOK means the vendor is
never in the data path**. Ioka, the company behind the mesh, runs no
proxy, sees no prompt, and holds no key. It cannot become a data-processing
addendum problem. You keep a self-hosted perimeter,
your Postgres, your object store, your model bills.

The rest of this chapter walks the seam between the first two parties: the
identity chain. Every Part II pattern sits on top of it, and
because it is the piece most application teams get wrong by default
(usually by building a second, worse identity system inside the app).

### The identity chain, demonstrated end to end

Everything below was executed against v0.1.2 and the responses are real
(abridged only where marked). The rig is a from-source server on the
alternate ports (the Introduction's two-world rule: from-source examples
bind 18080/19090) with the compose Postgres behind it. Nothing here is
dev-rig-specific; the same sequence runs against the compose profile on
:8080 if you add the last two variables, and against any deployed
environment through its gateway.

Two things distinguish this boot from the Introduction's: the static token
list now carries **two roles**, and a **token secret** is configured, which
is what arms capability-token issuance:

```powershell
$env:MUNARIUM_HTTP_ADDR = '127.0.0.1:18080'
$env:MUNARIUM_GRPC_ADDR = 'disabled'
$env:MUNARIUM_OPS_ADDR  = '127.0.0.1:19090'
$env:MUNARIUM_STORE = 'postgres'
$env:MUNARIUM_DATABASE_URL = 'postgres://munarium:munarium-dev@localhost:5433/munarium'
$env:MUNARIUM_SOURCE_STORE = 'pg'
$env:MUNARIUM_AUTH_MODE = 'static'
$env:MUNARIUM_STATIC_TOKENS = 'devtoken:dev-tenant:rw,mgmttoken:dev-tenant:mgmt'
$env:MUNARIUM_TOKEN_SECRET = 'dev-guide-ch14-secret-32-bytes-min!!'   # ≥ 32 bytes
cargo run -p munarium-server
```

The two static tokens are deliberately *not* interchangeable, and the demo
will prove it: `rw` runs the control plane (shapes, runbooks, ledger
commands) but cannot mint tokens; `mgmt` mints tokens and reads reports but
cannot write the ledger. A leaked credential is bounded to its plane. In
production the `mgmt` token is held by exactly one party: your API-management
layer. The secret comes from your key vault, never a compose file.

**Step 1: the manager exchanges trust for a token.** Your API manager has
just authenticated `alice@example.com` (its problem, solved with its SSO and
its MFA) and consulted its directory: level 0, no compartments, may query
but not ingest, and only through the `field-support` application. It
exchanges its long-lived `mgmt` credential for a short-lived token that
*attenuates* that trust:

```bash
curl -s http://127.0.0.1:18080/v1/access-tokens \
  -H "Authorization: Bearer mgmttoken" -H "X-Munarium-Uid: api-manager" \
  -H "Content-Type: application/json" \
  -d '{ "uid": "alice@example.com", "access_level": 0,
        "scopes": ["query"], "runbook_refs": ["field-support"],
        "ttl_secs": 900 }'
```

```json
{ "token": "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhbGljZUBleGFtcGxlLmNvbSIsInRlbiI6…",
  "jti": "tok-019ff40821e57c138f95bb6fecbbd478",
  "expires_at": "2026-08-12T03:48:21Z" }
```

Three fields have three jobs. `token` is the credential itself. It is an
HS256 JWT that is never stored server-side. The manager forwards it as the
bearer on the user's calls.

`jti` is the token's identity for everything *about* it. This includes the
issuance audit row in the `access_tokens` table, which records who minted
it, for whom, and with what claims. It also supports per-request attribution
in interaction capture and serves as the revocation key if you enable the
deny-list. `expires_at` defines the token's full lifecycle. There is no
refresh. The manager mints a new token when its own session rules say the
user is still present.

Decode the payload (it is a standard JWT encoded with base64, not encrypted; the
*signature* is the security) and you are looking at the whole authorization
model of the data plane:

```json
{ "sub": "alice@example.com",
  "ten": "dev-tenant",
  "lvl": 0,
  "cmp": [],
  "scopes": ["query"],
  "rb": ["field-support"],
  "jti": "tok-019ff40821e57c138f95bb6fecbbd478",
  "iat": 1786505601,
  "exp": 1786506501 }
```

Field by field, because your integration will live on these:

- **`sub`** is the end-user id the manager authenticated. This is the claim
  that makes the token *personal*: it must match the `X-Munarium-Uid` header on
  every call made with the token, and it is what the audit trail records.
- **`ten`** is the tenant, taken from the minting `mgmt` token, not from the
  request. A manager cannot mint across tenants.
- **`lvl`** is hierarchical clearance. A collection at level *L* requires
  `lvl >= L`. Alice at 0 sees only public collections.
- **`cmp`** contains need-to-know compartment tags, the horizontal axis. A
  collection's tags must be a *subset* of the token's. Level plus
  compartments is the entire policy language. It is Bell-LaPadula "simple
  security" with categories, one comparison, no policy engine. §16 turns
  this into design guidance for modeling your org's clearances.
- **`scopes`** defines which data planes the token may use: `query` (sessions and
  turns) and/or `ingest` (file upload). Alice can ask; she cannot feed the
  corpus.
- **`rb`** is an optional runbook-name allowlist; absent means any runbook
  the level permits. Alice's token works only against `field-support`.
- **`jti`, `iat`, `exp`** define identity and lifetime. `exp − iat` here is the
  900 seconds we asked for.

**The TTL ceiling is a ceiling, not a suggestion.** Ask for a
999,999-second token and the server mints exactly 24 hours, silently
clamped (the cap is hard-coded in the server; the default when you send no
`ttl_secs` is `MUNARIUM_TOKEN_TTL_SECS`, itself defaulting to one hour):

```bash
# requested ttl_secs: 999999 (≈ 11.6 days), minted at 03:33:21Z:
{ "jti": "tok-019ff408222c711287eab69281782ac7",
  "expires_at": "2026-08-13T03:33:21Z" }        # exactly +24 h
```

Short lifetimes are the primary bound on a stolen token. Revocation is
optional (one deny-list lookup per verify, off by default), so `exp` is the
mechanism your threat model should lean on. Match the TTL to your manager's
session length, and prefer shorter.

**Step 2: the token at work, and the uid it carries.** For the token to
have something to do, the operator side of the house (the `rw` token)
publishes a minimal retrieval application. It has one shape and one runbook with a
public collection at level 0 and an engineering collection at level 2 behind
the `eng` compartment. §15 and §16 treat runbooks properly; here it is
scenery, so it is shown compressed:

```bash
curl -s -X POST http://127.0.0.1:18080/v1/shapes \
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: casey.ops" \
  --data-binary $'apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: docs, version: 1 }\nspec:\n  fact:\n    schema: { type: object }'
# -> { "shape_ref": "docs@1", … }

curl -s -X POST http://127.0.0.1:18080/v1/runbooks \
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: casey.ops" \
  --data-binary @- <<'EOF'
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: field-support, version: 1 }
spec:
  collections:
    - name: public-docs
      shape: docs@1
      accessLevel: 0
      sources: { filenamePrefix: "public/" }
    - name: internal-eng
      shape: docs@1
      accessLevel: 2
      compartments: [eng]
      sources: { filenamePrefix: "eng/" }
  retrieval: { topK: 8 }
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
EOF
# -> { "runbook_ref": "field-support@1" }
```

Now the user's turn. Your application, holding Alice's minted JWT, opens a
session against the runbook. Note what the request does *not* carry:

```bash
JWT=eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9…      # the minted token, in full
curl -s -X POST http://127.0.0.1:18080/v1/runbooks/field-support/sessions \
  -H "Authorization: Bearer $JWT"
```

```json
{ "session_id": "ses-019ff408baa87ee0b2281e1e359e7faa",
  "runbook_ref": "field-support@1",
  "permitted_collections": ["public-docs"] }
```

Two teachings in one response. First, there is **no `X-Munarium-Uid` header**
and the call succeeded anyway: when the bearer is a capability JWT, the
token's `sub` *is* the uid. A header there could only agree or be rejected,
so its absence is unambiguous, and the interaction record is attributed to
`alice@example.com` all the same. (With a *static* token, a missing header
is a `400 uid-required`, the Introduction's most common first-request
failure. Static tokens name a tenant, not a person, so the caller must
say who is asking.)

Second, look at `permitted_collections`: the runbook
has two collections. Alice's session has clearance snapshotted at
creation from `lvl: 0, cmp: []` and sees exactly one. Nothing about
`internal-eng` leaks, here or in any retrieval this session ever runs.
Same application, same runbook, different tokens, different worlds: that is
clearance-filtered retrieval, and §16–§17 build the whole UX on it.

**Step 3: the chain defends itself.** Now send the wrong header. This is
the stolen-token scenario, a JWT replayed under a different asserted
identity:

```bash
curl -s -X POST http://127.0.0.1:18080/v1/runbooks/field-support/sessions \
  -H "Authorization: Bearer $JWT" -H "X-Munarium-Uid: mallory@example.com"
```

```json
{ "type": "https://munarium.ioka.io/problems/uid-mismatch", "title": "uid mismatch",
  "status": 403,
  "detail": "asserted uid 'mallory@example.com' does not match the access token subject 'alice@example.com'" }
```

The cross-check runs in middleware on every `/v1` request, before any
handler (the gRPC plane enforces the identical contract on `munarium-uid`
metadata; Part I §5 shows the implementation if you want it). And the rest
of the chain's failure surface, each captured against the live server.
learn these by slug, because slugs are the stable contract and message
wording is not:

| You send | You get | Slug |
|---|---|---|
| No bearer token at all | `401` | `unauthenticated` |
| A garbage/tampered JWT | `401` | `unauthenticated` ("invalid token") |
| An expired JWT (uid header present) | `401` | `token-expired` ("request a new one from the management plane") |
| An expired JWT, **no** uid header | `400` | `uid-required`; see below |
| The `rw` token on `POST /v1/access-tokens` | `403` | `forbidden` ("role 'rw' cannot use the management plane (mgmt required)") |
| A `query`-only JWT on `POST /v1/ingest` | `403` | `scope-missing` ("the access token does not carry the 'ingest' scope") |
| The capability JWT on a kernel route (e.g. `GET /v1/versions/{id}/facts`) | `403` | `forbidden` ("access tokens are data-plane credentials; this endpoint requires a static token") |

Two rows deserve a note. The expired-token-without-header case surfaces as
`uid-required`, not `token-expired`: a token the server can no longer
verify can no longer supply your uid, so the uid contract fails first. Your
client's retry logic should treat `uid-required` on a JWT-bearing call as a
possible expiry, not only as a missing header. You can also always send the
header, which costs nothing and keeps the mismatch check active.

The last row shows the plane partition working as designed. Remember this
rule: **capability tokens are data-plane credentials.** They open
sessions, drive turns, search collections, and ingest files; they cannot
read or write the memory ledger directly, mint tokens, or touch the control
plane. Your application's server-side components use the static tokens for
operator work; your users' requests ride JWTs; the two never blur. This
also corrects a natural first assumption: a minted JWT is *not* a
lower-privilege general key to every route; it is a key to the user-facing
planes only.

**Why the server has no OIDC.** By now the shape is visible: nothing in
this chain involved the mesh authenticating a human. That is not a missing
feature. [security-posture.md](../security-posture.md) gives the full,
normative argument. The short version belongs here
because your integration plan depends on believing it. Your API-management
layer already owns the user directory, the MFA, the session semantics, and
the credential lifecycle.

If the mesh grew a second login system, you would
have two sources of identity truth to keep synchronized, two credential
stores to defend, and an audit trail that is ambiguous about who decided
what. So the server refuses the job: no login, no user store, no JWKS, no
federation, and no token introspection. Verification is a local HS256 check
against one server-held secret. The manager *asserts* identity (the uid,
and the level/compartments it bakes into the mint request); the mesh
*enforces data governance* for that asserted identity and records the
evidence.

The manager is the security boundary; if it is compromised, the
mesh's audit trail is your forensic record, not your prevention layer.
Residual risks and their bounds are listed honestly in the posture
document. Read it with your security team before production, alongside
§20's integration sketch.

### What "governed" buys you, concretely

The identity chain is the entry fee. Here is the whole return, mapped from
the concern you already have to the mechanism that answers it and the
chapter that teaches it:

| Your application concern | Mesh mechanism | Where |
|---|---|---|
| "Users must only see documents they're cleared for" | Collections with `accessLevel` + `compartments`; retrieval filtered by the token's `lvl`/`cmp` before ranking; clearance snapshotted per session | §16 |
| "Citations must actually resolve" | Provenance envelopes (source hashes, index version, watermark); verify-quotes / verify-citations checks (measured; client-side today, §17 **[gap]**); cited-or-insufficient as a first-class outcome | §16–§17 |
| "Two sources disagree. Surface it; don't guess" | Gates; `disputed` status with machine-readable findings; the red-flag review queue; corrections via supersession | §18 |
| "What did the system know when it answered?" | One-sequence pins (`as_of_seq`) over facts, promises, and composed briefs; immutable index versions that keep resolving | §18, §20 |
| "Who asked what, and what were they told?" | uid-attributed interaction capture on every call; `GET /v1/reports/usage\|audit\|cost`; `x-munarium-request-id` correlation | §20 |
| "Model spend must be governable" | BYOK to your accounts; fast/capable/frontier tier routing; runbook `models:` policy with `allowOverrides` default-closed; per-call output ceilings as one replaceable object (`/v1/max-tokens`, 2026-09-02) and daily token caps per provider config (2026-09-01) | §17, §20 |
| "A new corpus shouldn't mean new code" | Shapes + runbooks + collections: a corpus application is declarative configuration | §15 |

None of these rows is aspirational, which brings us to the last thing this
chapter owes you.

### The patterns are worked, not guessed

The Preface told the origin story: the platform's semantics were settled
before the server existed, so that story is not repeated here. What matters
for Part II is the *consequence*: every application pattern in the chapters
ahead was built and run against a real corpus before it was written down as
guidance.

When §19 tells you an open-ended "find everything" sweep misses what targeted
questions find, that is a result someone got, not a design opinion. §19 is the
full catalog, with one pattern per section. Each section gives the shape, what
it composes, what it still asks of you, and the committed runbook to copy.
Appendix E maps every committed sample runbook to the modelling decision it
demonstrates. When your corpus resembles one of those, you are not starting
from a blank page; you are starting from a worked precedent.

### Where this leaves you

A governed AI application, then: your API manager authenticates the human
and mints a scoped, short-lived capability token; every call your app makes
carries that token and the user's identity; the mesh filters retrieval by
clearance, converts contradictions into a review queue, wraps every answer
in provenance, and records the whole exchange against pins that can replay
it exactly. Your UX renders answers with citations that resolve,
insufficient-evidence outcomes as designed states, and disputes as work
items. Your compliance team gets their March question answered with a
query parameter.

What this chapter did *not* do is put real documents behind that identity
chain. The `field-support` runbook above never even built an index. That
is deliberate, and it is next. §15 explains the anatomy of a corpus
application: corpus in, shape, runbook, run, and sessions out. It uses the
committed due-diligence data room that serves as Part II's worked spine all
the way through the full tutorial in §21.

## 15. Anatomy of a corpus application

§14 ended on a deliberate emptiness: an identity chain with nothing behind
it. The `field-support` runbook never even built an index. This chapter
fills the space behind the chain. Your application may be chat over a data
room, grounded Q&A over filings, or a red-flag review queue. In each case, it
instantiates the same five-stage pipeline, and every stage is either a YAML
artifact you keep in git or an API call your deployment scripts make. That
is the claim §14's closing table made in one row ("a new corpus shouldn't
mean new code"), and this chapter is the proof: **a new corpus application
is configuration.** You will not write a line of Rust; neither did the
thirteen applications committed under `runbooks/applications/`.

Here are the five stages, with the artifact that defines each, who authors
it, and where it lives:

1. **Corpus.** Your documents, uploaded via `POST /v1/ingest` /
   `POST /v1/ingest/batch` (base64 bodies) or streamed raw via
   `PUT /v1/sources`. The bytes land in whatever object store the server is
   configured for; the row records where. You own the documents and the
   *path discipline*. A document's filename IS its identity, its
   blob path, and the string collections bind against
   ([loading-corpora.md](loading-corpora.md) is the definitive treatment;
   this chapter only demonstrates the consequences).
2. **Shape** (`kind: Shape`, published via `POST /v1/shapes`). How one
   *class* of document validates, chunks, and indexes. It also defines the fact
   vocabulary minted over it when canonical memory enters (§18). Authored
   by you, versioned in your repo. Twelve committed examples under
   [runbooks/shapes/](../../runbooks/shapes/), claim files to patent
   documents.
3. **Runbook application** (`kind: Runbook`, applied via
   `POST /v1/runbooks`). The application itself has named collections. Each
   with an access level, optional compartments, and a declarative
   `filenamePrefix` source binding. It also has retrieval knobs, model policy,
   and an optional RAG completion template. Authored by you, versioned in
   your repo. Thirteen committed examples under
   [runbooks/applications/](../../runbooks/applications/), one per measured
   corpus.
4. **Run** (`POST /v1/runbooks/{name}/runs`). The only stage that is
   server-side *state* rather than an artifact. The five-step pipeline,
   `resolveSources → buildIndex → verify → cutover → retireOld`, is executed
   once per collection, pausing at the approval-gated cutover for a human
   decision. You own the approval; the server owns the execution record.
5. **Sessions and search.** The data plane your UI talks to: filtered
   `POST /v1/search` for single-collection retrieval, sessions and turns
   (§17) for the multi-collection conversational plane. This is runtime, not git,
   but every answer carries the provenance envelope that makes it
   verifiable later.

![The stages of a corpus application: corpus files where the filename is the identity, then ingest into source rows and the object store, then shapes carrying chunk and index rules, then runbooks declaring collections with levels and prefixes, then the run through resolveSources, buildIndex, verify, an approval-gated cutover and retireOld, and finally sessions and search where every answer carries a provenance envelope](images/ch15-five-stages.svg)

Two stages produce YAML you review in pull requests; two are API calls your
deploy pipeline makes; one is a human approval. Nothing else. The rest of
this chapter runs that pipeline end to end at miniature scale. It then examines
the committed due-diligence application, Part II's worked reference, at
full scale.

### The pipeline in miniature, executed end to end

What follows was executed against v0.1.2 and the responses are real
(abridged only where marked). It is deliberately **not** the due-diligence
load. That is §21's tutorial, and it deserves the room. Here the corpus is
four markdown documents small enough to print in full, because at four
documents you can see the machinery: which call produced which artifact,
what the run did between `pending` and `done`, and exactly what arrives in
a search hit.

The rig is the same from-source posture as §14 (the two-world rule: 18080
for REST, ops on 19090, the compose Postgres on 5433), with two choices
worth naming. `MUNARIUM_SOURCE_STORE=pg` keeps document bytes in Postgres,
the zero-setup backend, fine for four documents; a real corpus points this
at blob storage instead ([source-stores.md](source-stores.md)). And the
tenant is **fresh**: `ch15-b7e2`, a name this walkthrough has never used,
so every id below was created by exactly the calls shown. This is the same
fresh-tenant discipline the test tiers use (Part I §3), and a habit worth
stealing for your own experiments. Re-running this section under a new
tenant name costs nothing and collides with nothing.

```powershell
$env:MUNARIUM_HTTP_ADDR = '127.0.0.1:18080'
$env:MUNARIUM_GRPC_ADDR = 'disabled'
$env:MUNARIUM_OPS_ADDR  = '127.0.0.1:19090'
$env:MUNARIUM_STORE = 'postgres'
$env:MUNARIUM_DATABASE_URL = 'postgres://munarium:munarium-dev@localhost:5433/munarium'
$env:MUNARIUM_SOURCE_STORE = 'pg'
$env:MUNARIUM_AUTH_MODE = 'static'
$env:MUNARIUM_STATIC_TOKENS = 'ch15tok:ch15-b7e2:rw'
cargo run -p munarium-server
```

One static `rw` token drives all five stages. That works because a static
`rw` credential maps to an *unrestricted* access context. It has the
highest level, every compartment, and both the `query` and `ingest` scopes. This is right for
an operator rig and exactly wrong for production, where ingest and query
ride scoped capability JWTs and the operator token never reaches end users.
§14 already made that argument; this chapter spends its budget elsewhere.

**Stage 1: the corpus goes in.** The corpus is a landlord's field file for
a fictional pier operator: two lease summaries, an insurance note, a vendor
note. Everything uploads under the `harbor/` prefix, with a sub-folder per
kind. That layout is a *decision*, not a convention, because the
prefix is what collections will bind. First one document, then two more in
a batch:

```bash
curl -s -X POST http://127.0.0.1:18080/v1/ingest \
  -H "Authorization: Bearer ch15tok" -H "X-Munarium-Uid: casey.ops" \
  -H "Content-Type: application/json" -d '{
    "filename": "harbor/leases/bluewater-pier.md",
    "media_type": "text/markdown",
    "content_base64": "'"$(base64 -w0 bluewater-pier.md)"'" }'
```

```json
{ "filename": "harbor/leases/bluewater-pier.md",
  "source_id": "src-27758de723e629c7",
  "sha256": "8d4458670605104d0658bca9d9743c6e40d7947c0c3082e16f62ec7766856f4d",
  "existed": false,
  "bound_to": [] }
```

Four fields, four teachings. `source_id` is stable and derived from the
*path*. Upload different bytes to this path later and the id stays, the
hash changes, and a rebuild is owed. `sha256` is the content address the
provenance envelope will carry in stage 5. `existed: false` says this
path+bytes pair is new. Replay the same request and you get
`existed: true`, because ingest is idempotent.

`bound_to` is empty for the
honest reason: ingest evaluates the declarative matchers of the runbooks
that exist *now*, and no runbook exists yet. Nothing is lost. When a
matching runbook is applied in stage 3, these documents will be claimed at
that moment. The empty list is your first sight of a rule that runs
through the whole platform: which collections hold a source is always
*derived* from runbook configuration, never bookkeeping you write. The
server re-derives it at every moment that could change the answer: at
ingest, at runbook apply, and again at the start of every run.

The batch route uses the same contract and returns per-file results. One
bad file never fails the batch. The route is `POST /v1/ingest/batch`, with
a limit of 500 files. This example is abridged; both remaining documents
came back with `"bound_to": []`. (For whole corpora there is a third,
higher-level surface layered on the same storage path — **bulk upload
sessions**, 2026-08-19: `POST /v1/ingest/bulk` opens a session from a
manifest of `{filename, sha256, bytes_len, media_type}`, the server diffs
it against `sources` and answers with the `needed` work list, chunks flow
through `POST /v1/ingest/bulk/{id}/chunk` with per-file hash verification,
and `.../complete` re-verifies everything before declaring the load done.
`mmctl bulk upload --dir <dir> --prefix <p/>` drives the whole flow;
the operational walkthrough lives in
[loading-corpora.md](loading-corpora.md).) Then the
question every operator asks first: *did it land, and where?*

```bash
curl -s http://127.0.0.1:18080/v1/sources/src-27758de723e629c7 \
  -H "Authorization: Bearer ch15tok" -H "X-Munarium-Uid: casey.ops"
```

```json
{ "source_id": "src-27758de723e629c7",
  "filename": "harbor/leases/bluewater-pier.md",
  "media_type": "text/markdown",
  "content_hash": "8d4458670605104d0658bca9d9743c6e40d7947c0c3082e16f62ec7766856f4d",
  "bytes_len": 260,
  "storage_backend": "pg",
  "blob_uri": "pg://source_blobs/ch15-b7e2/harbor/leases/bluewater-pier.md",
  "created_at": "2026-08-12 03:45:43.02468+00" }
```

`storage_backend` names where the bytes physically went; `blob_uri` is the
recorded, credential-free location. Notice that the tenant and logical
path are both *in* it. Filename is identity, identity is blob path.
(`extraction_status` is absent until an index build first touches the
source; it matters greatly for PDF corpora, as §16 explains.)

**Stage 2: the shape.** A shape governs a *class* of document: how it
chunks and indexes now, and what fact vocabulary is minted over it when
canonical memory arrives (§18). The simplest useful form, modeled on the
committed [dataroom-documents.yaml](../../runbooks/shapes/dataroom-documents.yaml):

```bash
curl -s -X POST http://127.0.0.1:18080/v1/shapes \
  -H "Authorization: Bearer ch15tok" -H "X-Munarium-Uid: casey.ops" \
  --data-binary @- <<'EOF'
apiVersion: munarium.ioka.io/v1
kind: Shape
metadata: { name: field-notes, version: 1 }
spec:
  fact:
    schema:
      type: object
      properties:
        subject: { type: string, pattern: "^[a-z][a-z0-9_]{0,63}$" }
        key: { type: string, pattern: "^[a-z][a-z0-9_:-]{0,63}$" }
        value: { type: string, minLength: 1 }
      required: [subject, key, value]
    supersession:
      identity: [subject, key]
  chunking: { max_chars: 800 }
EOF
# -> { "shape_ref": "field-notes@1",
#      "yaml_hash": "4aae18f82faf7068a4482a110ca15e694ad876db2e712db2d50909e56288b61f", … }
```

Even a retrieval-only application should not ship the overly broad
`schema: { type: object }` shown in §14. The committed shapes enforce two
hard-won rules. First, fold subjects with lowercase letters and underscores.
This makes two spellings of one entity collide instead of hiding a conflict.
Second, use **dot-free keys**. `subject.key` splits at the last dot, so a
dotted key silently steals from the subject. That lesson was expensive.
The dataroom shape's header comments tell the story, and
[runbooks/README.md](../../runbooks/README.md) codifies it.

`supersession.identity: [subject, key]` declares what makes two facts "the
same fact" for update-vs-conflict decisions. `chunking.max_chars` is the
one knob retrieval feels immediately. It decides the unit of retrieval
and is part of index identity. Change it and the next build is a new
index version. The response's `shape_ref` (`field-notes@1`) is the handle
collections bind, and `yaml_hash` pins exactly which text of the shape they
bind.

**Stage 3: the runbook application.** Now the application itself. One
collection is enough for the miniature; the reference application below
has thirteen. Validate first. It gives deterministic findings for free and
uses the same check CI runs on every committed sample. Then apply:

```yaml
# harbor-review.yaml
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: harbor-review, version: 1 }
spec:
  collections:
    - name: harbor-notes
      shape: field-notes@1
      accessLevel: 0
      sources: { filenamePrefix: "harbor/" }
  retrieval: { topK: 5 }
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
```

```bash
curl -s -X POST http://127.0.0.1:18080/v1/runbooks/validate \
  -H "Authorization: Bearer ch15tok" -H "X-Munarium-Uid: casey.ops" \
  --data-binary @harbor-review.yaml
# -> { "valid": true, "findings": [], "suggestions": [] }

curl -s -X POST http://127.0.0.1:18080/v1/runbooks \
  -H "Authorization: Bearer ch15tok" -H "X-Munarium-Uid: casey.ops" \
  --data-binary @harbor-review.yaml
# -> { "runbook_ref": "harbor-review@1" }
```

Applying does more than store the YAML: it materializes the collection and
immediately sweeps the *existing* sources through its matchers. The three
stage-1 documents were ingested when there was nothing to bind to. They are
claimed retroactively at this moment, so the runbook is fully inspectable
before its first run. The proof from the other direction is the *fourth*
document, ingested after the runbook exists:

```json
{ "filename": "harbor/notes/vendor-crane-service.md",
  "source_id": "src-a3b210bac2b30bb5",
  "existed": false,
  "bound_to": ["harbor-notes"] }
```

Same ingest call as stage 1, but now `bound_to` names the collection whose
`filenamePrefix: "harbor/"` matched. One matcher, two trigger points:
apply binds what has already arrived, ingest binds what arrives afterward
because upload order and apply order do not matter. (A third
trigger closes the loop in stage 4.) Matching is a literal `starts_with`,
no globs, which is why prefix design deserves forethought (§16 opens with
exactly this).

**Stage 4: the run, and the approval pause.** A run is where
configuration becomes infrastructure. Start it and read its state:

```bash
curl -s -X POST http://127.0.0.1:18080/v1/runbooks/harbor-review/runs \
  -H "Authorization: Bearer ch15tok" -H "X-Munarium-Uid: casey.ops"
# -> { "run_id": "run-019ff413ddf87c71ba985dcc5bafd5db", "state": "awaiting_approval" }
```

The response already says `awaiting_approval`: execution is synchronous up
to the first gate, so by the time the POST returns, three steps have
already run. `GET /v1/runs/{id}` shows the full picture:

```json
{ "run_id": "run-019ff413ddf87c71ba985dcc5bafd5db",
  "runbook_ref": "harbor-review@1",
  "state": "awaiting_approval",
  "steps": [
    { "ordinal": 0, "name": "resolveSources:harbor-notes", "state": "done",
      "detail": { "collection_id": "col-019ff413c0737361ac947bb334aad3a1",
                  "sources": 4, "missing_declared_hashes": [] } },
    { "ordinal": 1, "name": "buildIndex:harbor-notes", "state": "done",
      "detail": { "index_version": "idx-61f2173837f592c9", "watermark": 0 } },
    { "ordinal": 2, "name": "verify:harbor-notes", "state": "done",
      "detail": { "chunks": 4, "self_probe_hits": 1 } },
    { "ordinal": 3, "name": "cutover:harbor-notes", "state": "awaiting_approval",
      "detail": null },
    { "ordinal": 4, "name": "retireOld:harbor-notes", "state": "pending",
      "detail": null } ]
}
```

Read it as a story. Step names are `step:collection` because a run
executes the pipeline once *per collection*. With thirteen collections you
get thirteen `buildIndex:*` rows and thirteen independent approval gates.
`resolveSources` is the third matcher trigger promised in stage 3: it
re-runs the same binding sync before counting, so a run never builds from
bindings gone stale since the apply. It reports all **4** sources.
(`missing_declared_hashes` matters when a binding pins explicit content
hashes rather than a prefix; empty here.)

`buildIndex` built `idx-61f2173837f592c9` **side by side**. The new index
exists, is queryable by id, and serves nothing yet. Its `watermark: 0` is
honest because no memory lineage is attached. Run with `?version_id=` and the
watermark records that lineage's head seq at build time, the number the
provenance envelope uses to say *what the ledger knew when this index was
_built_; see §18.

`verify` counted chunks (four documents under 800 chars
each → four chunks) and ran a self-probe. This is a retrieval query drawn from
the index's own content that must come back non-empty, the cheap sanity
check that an index is not silently hollow. And then the run *stopped*.

A step state is one of `pending → running → awaiting_approval → done`, or
`failed` (a failed step fails the run; fix and start a new run because runs are
cheap, indexes are versioned, nothing is overwritten). The run's own state
mirrors the same vocabulary, and right now a human owns the next
transition. This is the same gate you saw §14's `field-support` runbook
declare and never reach: `cutover: { approval: required }` means **no
index serves traffic because a build succeeded**. It serves because a
person said so, and the run row records that they did. Approve it:

```bash
curl -s -X POST http://127.0.0.1:18080/v1/runs/run-019ff413ddf87c71ba985dcc5bafd5db/steps/3/approve \
  -H "Authorization: Bearer ch15tok" -H "X-Munarium-Uid: casey.ops"
# -> { "run_id": "run-019ff413ddf87c71ba985dcc5bafd5db", "state": "done" }
```

Approval resumes execution synchronously: cutover activated
`idx-61f2173837f592c9` (its `detail` records `"activated"`), `retireOld`
reclaimed nothing (`retired_chunk_rows: 0` because there is no older version
yet; on rebuild number three it starts collecting chunk rows beyond
`keep_versions: 2`, while retired versions' manifests stay resolvable so
old provenance envelopes remain verifiable), and the run is `done`. Every
transition records who started the run, which uid approved which ordinal, and when. It
is in the run record and the interaction audit (§20).

**Stage 5: the payoff exhibit.** The application now answers. Scoped
search against the collection:

```bash
curl -s -X POST http://127.0.0.1:18080/v1/search \
  -H "Authorization: Bearer ch15tok" -H "X-Munarium-Uid: casey.ops" \
  -H "Content-Type: application/json" -d '{
    "query": "when does the Bluewater Pier lease renew and at what rent",
    "top_k": 2,
    "filter": { "collections": ["harbor-notes"] } }'
```

```json
{ "hits": [
    { "chunk_id": "src-27758de723e629c7#0",
      "source_id": "src-27758de723e629c7",
      "source_path": "harbor/leases/bluewater-pier.md",
      "source_content_hash": "8d4458670605104d0658bca9d9743c6e40d7947c0c3082e16f62ec7766856f4d",
      "text": "# Bluewater Pier — lease summary\n\nLandlord: Bluewater Pier Holdings LLC. …\n\nThe lease renews on 2027-03-01 at $18,400 per month, …",
      "score": 0.0328, "lexical_rank": 1, "vector_rank": 1 },
    { "chunk_id": "src-a1562f070150fec4#0",
      "source_path": "harbor/notes/insurance-summary.md", "…": "…" } ],
  "envelope": {
    "chunk_ids": ["src-27758de723e629c7#0", "src-a1562f070150fec4#0"],
    "source_ids": ["src-27758de723e629c7", "src-a1562f070150fec4"],
    "source_paths": ["harbor/leases/bluewater-pier.md",
                     "harbor/notes/insurance-summary.md"],
    "source_content_hashes": ["2d331e1abc7c80c809fd9e1dcd2688aedd628c7c879763eb83d6570ee8dcd6f0",
                              "8d4458670605104d0658bca9d9743c6e40d7947c0c3082e16f62ec7766856f4d"],
    "index_version": "idx-61f2173837f592c9",
    "event_watermark": 0,
    "provider_fingerprint": "local/local-hash@1/256" }
}
```

The right document, top-ranked on both the lexical and vector legs of the
hybrid fusion is first. The half of the response your UI may ignore is the
half this platform exists for. The **provenance envelope** closes every
loop this chapter opened. `index_version` is the exact id that
`buildIndex` reported and the approval activated. The
`source_content_hashes` are the exact `sha256` values returned by ingest in
stage 1. `event_watermark` is the ledger pin reflected by the index.

`provider_fingerprint` names the embedder. Here it is `local-hash@1`, the
deterministic keyless local embedder. This is why the whole chapter ran
without a provider key. A BYOK embedder would be named here. Changing it
forces a rebuild because the embedder is part of index identity.

One reading note: the envelope's lists are each sorted and deduplicated
independently. Pair a path with its hash through the hit that cites it,
not by list position (which is why `bluewater-pier.md` leads
`source_paths` while its hash sits second in `source_content_hashes`). An
answer that cites `bluewater-pier.md` is therefore not citing a filename.
It cites *bytes with a hash, served by an index with a version, built
at a known ledger watermark, activated by a recorded human decision*. When
§17 wires model completions on top, this envelope is what makes the
model's citations checkable rather than decorative.

Two production notes before the miniature retires. Filtered search is a
**data-plane** read: it enforces the collection's level and compartments
against the caller (our unrestricted rw token clears everything; a real
user's JWT may not, and a collection they cannot clear 404s rather than
revealing it exists; see §16). One collection per search is the deliberate
limit: the single envelope stays truthful about a single index.
Multi-collection retrieval, with merged hits and one envelope *per collection*,
is the session plane, which is §17's subject.

That is the whole pipeline: four documents, two YAML artifacts, a handful
of API calls, one human approval, and a verifiable answer. Scale changes
the numbers, not the anatomy. (The same route sequence, told from the
operator's and API-manager's seats against the compose profile, including
the token-minting and reporting sides omitted here, is
[platform-features.md](platform-features.md), the walkthrough this
chapter complements.)

### The reference application, dissected

Part II's worked example is the **due-diligence** application:
[runbooks/applications/due-diligence.yaml](../../runbooks/applications/due-diligence.yaml)
over a synthetic M&A data room of markdown documents, foldered by functional
area. The corpus itself is not in this repository
([loading-corpora.md](loading-corpora.md) says what to put under each sample
runbook's prefixes), but the application is
complete as committed, which is why §21 uses it for the full tutorial. Here the
interest is anatomical: it is the same five stages as the miniature with
every dial actually turned. Read the file alongside this walk.

**The header is documentation with teeth.** A page of comments names what the
sample demonstrates, how it maps onto server collections, and the upload
convention (`northgate/<area>/…`). Every committed runbook does this. The file is the application's README.

**`spec.sources` declares where the documents live** (`container:
sources`, `prefix: "northgate/"`). It is checkable, not decorative:
`mmctl runbook validate` errors with `sources.prefix-mismatch` if any
collection binds a path outside it, and warns when a prefix does not end
in `/` (literal `starts_with`: `north` would also match
`northgate-archive/`). The miniature skipped this block; a real
application should not.

**Thirteen collections, drawn on real governance boundaries.** Each
data-room functional area is a collection binding one folder prefix, and
the levels and compartments encode who may see what. A buyer-side
associate, deal counsel, and an HR specialist get disjoint slices of the
same runbook:

| Level | Compartment | Collections (prefix under `northgate/`) |
|---|---|---|
| 0 | none | `dataroom-index` (`000_`, the room's index document) |
| 1 | none | corporate (`01_`), commercial (`05_`), real-estate (`10_`), insurance (`11_`), operations (`12_`) |
| 2 | `finance` | equity (`02_`), finance (`03_`), tax (`04_`) |
| 2 | `legal` | ip (`07_`), legal-compliance (`08_`) |
| 3 | `hr` | employment (`06_`) |
| 3 | `security` | privacy-security (`09_`) |

The pattern to copy is *restraint*: levels and compartments appear where
the corpus has genuinely different sensitivity (employment files, security
incidents), and stay level 0 where it does not. Runbooks whose corpora
are wholly public (history-revolution's LOC archives,
regulatory-compliance's CFR) are uniformly level 0 and accept the
validator's `collections.uniform-access` Info finding as the honest
model. [runbooks/README.md](../../runbooks/README.md) documents that
choice, and §16 turns the whole topic into design guidance.

**One shape, shared.** Every collection binds `dataroom-documents@1`, and
so do the sweep-coverage and sweep-v2 runbooks, because all three read the
same corpus through different retrieval policies. Shapes are shared, not
copied; the shape belongs to the *document class*, the runbook to the
*application*. Its spec is the miniature's two disciplines at production
strength (folded subjects, dot-free keys, `max_chars: 1200`,
`indexing: { rrf_k: 60, candidate_n: 120 }`).

**`retrieval:` and `models:` are application policy.**
`retrieval: { topK: 10, rrfK: 60, candidateN: 120 }` sets how much
context a turn assembles (§16 explains the knobs; app developers mostly
inherit them). When a corpus needs vocabulary or record-quality policy, keep
that policy declarative too:

```yaml
retrieval:
  topK: 10
  rrfK: 60
  candidateN: 120
  queryExpansionWeight: 0.7
  modelQueryExpansion:
    maxTerms: 12
    maxTokens: 256   # 128 until 2026-09-02
    required: false
  collectionSelection:
    maxCollections: 8
    probeCandidateN: 50
    candidatePoolPerCollection: 100
  contentDemotions:
    - contains: "metadata-only"
      lexicalMultiplier: 0.1
      vectorDistancePenalty: 0.5
```

`modelQueryExpansion` asks the runbook's `query_expansion` task model for a
bounded list of generic lexical variants. The engine prompt forbids answers,
new names, places, organizations, dates, numbers, and multiword phrases; the
parser independently enforces lowercase single tokens. `required: false`
falls back to the original query if that paid assist is unavailable.
`queryExpansionWeight` controls the variants' relative ranking influence (`0`
keeps ranking on the original query, `1` gives the expanded query all the
weight); candidate selection still sees every accepted term.

`collectionSelection` is a generic two-stage strategy for wide runbooks. A
bounded original-query probe (`probeCandidateN` per leg, the whole fused
pool returned) runs over every access-permitted collection, the collections
are ranked by the evidence their pools carry, and a deep candidate pool is
retrieved only from the best `maxCollections`. Selection spends the deep
search; it never excludes: the other collections' probe pools stay in the
global merge (they were retrieved already), ranked by the same evidence
order through the fusion's collection-evidence leg. The evidence is a
blend, and both halves were measured (§13.5 entry 18): the sum of the
pool's three strongest `ts_rank` scores (density) multiplied by
`1 + phraseBoost × phrase-fraction`, where the fraction is the share of the
pool whose text contains one of the query's own adjacent content-word
pairs verbatim (`george washington`, `tea party`; stop words never form a
phrase); the three smallest cosine distances are the fallback when the
lexical leg is empty, and collection name breaks ties. Term density cannot
tell a corpus that *uses* the query's words from one that is *about* them;
the query's own phrases can, and they carry no vocabulary of the engine's
— but phrase evidence must not *override* density when it is weak
everywhere (a query whose phrase is later coinage), which is why it
multiplies rather than sorts first: at the default `phraseBoost: 3` a pool
85% carrying the phrase counts 3.55×, one carrying it in 6% of hits 1.18×.
A query with no adjacent content words has a multiplier of 1 everywhere
and selection is density alone, the prior behavior; `phraseBoost: 0`
disables the signal. `candidatePoolPerCollection` prevents useful local rank 21+ hits
from being discarded before global RRF produces the final `topK`. Both
stages are visible on the streaming turn plane: a `selection` progress
event (`probed`, `selected`, the selected names) precedes the per-collection
`retrieval` events, and an `expansion` event reports the model-expansion
call's provider, model, accepted terms and tokens.

`retrieval.searchConcurrency` (default 4, 1..=16) is how many collections a
turn searches at once — the selection probe and the deep search alike.
Each in-flight search holds one pooled connection, so the bound must stay
under the server's pool. The probe also streams a `probe` progress event per
collection as it completes, so a turn's response carries bytes from its
first hundred milliseconds; before this, the first byte was the `selection`
event after the *whole* probe, and on a loaded database a many-shard probe at
~4.5 s per shard outlasted the ingress's first-byte timeout (§13.5 entry 19).

`completion.contextCharBudget` (default 16,000) sizes the served context.
It is the knob `topK` needs beside it: twenty 1,500-character hits are
~30,000 characters, and hits past the budget are retrieved and reported
but never reach the prompt.

`retrieval.fusion` weights the global merge: each leg contributes
`weight / (rrfK + global rank)` and the defaults
(`lexicalWeight: 1`, `vectorWeight: 1`, `collectionEvidenceWeight: 0`)
reproduce the unweighted merge. Lower `vectorWeight` on the built-in
bag-of-words embedder, whose global rank-1s are the shortest chunks
sharing any token (measured: fragments and town-name tables).
`collectionEvidenceWeight` adds a third leg fed by `collectionSelection`'s
ranking — every hit also scores `weight / (rrfK + rank of its collection)`
— so a collection the probe showed to be *about* the subject lends its
chunks a prior a collection merely *using* the words does not get; without
`collectionSelection` the leg has nothing to read and the validator warns.
The unselected collections' probe pools are ranked as their own stratum:
raw leg scores are comparable only within one query formulation (Postgres'
OR `ts_rank` shrinks as the term count grows — an original-query pool
scores ~0.2 where the same chunk under a nineteen-term expansion scores
~0.03), so the merge builds one lexical and one vector ordering per stratum
and RRF fuses ranks, never raw scores, across them. `unselectedPoolWeight`
(default 1) scales the probe stratum's contributions; at 1 a probe rank-1
counts like a deep rank-1 and the evidence leg arbitrates.

`contentDemotions.contains` is a
case-insensitive substring marker. Its lexical multiplier is applied before
the lexical `candidateN` limit, while its distance penalty reranks the bounded
vector candidate pool. If more than one demotion matches, the strongest
lexical and vector penalties win. `exceptCollections` scopes a rule out of
named collections (validated against `spec.collections`) — a corpus-structure
declaration for catalog collections, where the "metadata-only" record *is*
the content: history-revolution's map records ("Fort on Dorchester Point")
carry the marker because a map has no text, and with the rule applied there
`loc-maps` never entered a turn's selection or top 20 (measured 2026-08-25).
The exemption applies to the selection probe and the deep search alike.
`match` chooses how the marker is tested: `substring` (default, the exact
case-insensitive contract — but every candidate row's full text is
detoasted and lowered to test it) or `phrase` (the marker's words in
sequence in the chunk's tsvector, via `phraseto_tsquery`: stemmed,
punctuation-insensitive, and evaluated against the vector the rank already
needs — the single most expensive per-row term of the lexical leg under
load became free; §13.5 entry 20). Prefer `phrase` for markers of three or
more words; a one-word marker degrades to a bare lexeme match.

`retrieval.minimumShouldMatch` (1 or 2, default 1) is the lexical leg's
cost control. With OR semantics the leg ranks every chunk holding *any*
query word — most of a newspaper shard for "washington" — and under load
that scan is what a slow turn is made of. At 2, a GIN-indexable tsquery of
ANDed lexeme pairs (built once per query formulation, 171 pairs for a
nineteen-term expanded query) excludes the single-word rows before any rank
is computed; those rows sort last under density ranking regardless, so the
top of the pool is unchanged. The vector leg's search depth is not a knob:
the engine sets pgvector's `hnsw.ef_search` to the candidate pool (40 to
1,000) for each query — the default 40 silently capped every ANN scan at
forty rows whatever `candidateN` said — and enables iterative scanning
(pgvector ≥ 0.8) so the `index_version` filter no longer eats the budget
when a partition still holds the retiring version's chunks.

`retrieval.stopTermFraction` (0 = off, else 0.05..=0.9) is the other cost
control, and the one that adapts to the corpus. A build records each
index version's lexeme document frequencies (`ts_stat`, every lexeme in at
least 1% of the version's chunks; an index built before the table existed
is measured once, on its first query). At query time a lexeme found in more
than the fraction of a collection's chunks is dropped from *that
collection's* candidate predicate — it still counts toward the rank, it
just no longer makes every chunk holding it a candidate — so "washington"
is a stop word in a Washington letterbook shard and an ordinary term in a
newspaper shard, with no vocabulary configured anywhere. If every query
lexeme is frequent the full set is kept; the predicate is never empty.
Both knobs derive one predicate per collection from the query's normalized
lexemes, computed once per formulation (§13.5 entry 21). These are query-time rules and require a
runbook re-apply, not an index rebuild. Domain vocabulary and corpus markers
belong in the runbook; the Rust retrieval engine only implements these generic
instructions.

Number-form normalization (2026-08-30) is the third query-time rule, and
the only one without a knob: it is on for every runbook because it can
only add lexemes the permitted collections' own indexes hold. A query's
identifier-shaped numbers — `4,436,097`, a bare `4436097`, the digit
suffix of `US4436097`; never decimals, dates, years or short counts —
contribute the corpus-observed forms of those digits from a derived
per-index table (`index_number_lexemes`, migration 0025), and the
augmented query feeds the selection probe and the deep search while
routing and the completion prompt see the question exactly as asked. A
corpus with no letter-prefixed identifiers contributes nothing and pays
one indexed lookup. The story, the measured closure and the
`sqlx::migrate!` staleness it surfaced are §13.5 entry 25.

Static `queryExpansions` and explicit `collectionRoutes` remain available for
applications with reviewed domain policy, but they are not necessary for the
generic two-stage strategy above.

The `models:` block routes work by task. It puts `completion` on
the capable tier and `validation` / `query_expansion` on the fast tier. The
`allowOverrides: [default]` is the policy gate on callers requesting a
different model per turn (default when omitted: closed). Model policy is
decided in the runbook, in git, and in review, not in the client.

**The `completion:` template carries measured lessons.** The prompt is not
boilerplate: "a conflict between documents is a FINDING. Report every
side," "an amendment controls its base agreement," "if the context does
not answer, say so plainly." Each rule earns its place from a measured failure
mode (the enumerable-set rule in financial-advisory's template took its
one failing question from 0.25 to 1.0). Your corpus will accumulate its
own rules the same way; the template is where they live.

**The steps block is identical to the miniature's.** Five steps, approval
on cutover, `keep_versions: 2`. At this scale that means thirteen
build/verify/cutover sequences and thirteen recorded approvals per run,
the same anatomy, once per collection.

What the reference application does *not* contain is equally instructive:
no answer keys (the `seeded_findings.json` key never enters any binding
because a key inside the index is not a measurement), no credentials, no model
names (tiers resolve at call time), and no code.

### Corpus staging tiers, and what loading actually costs

The miniature ingested one kilobyte; the due-diligence sample corpus is
four megabytes; the largest sample corpora measured were half a gigabyte. Where your corpus falls
on that range decides your loading strategy, and
[loading-corpora.md](loading-corpora.md) is the operational guide. It covers the
per-runbook acquisition table (none of the sample corpora ship in this
repository; it says which are public datasets you can obtain and which are
not available), the binary-format realities, and the honest PDF
limits. The shape of it:

| Tier | What | Scale | Why it exists |
|---|---|---|---|
| 1 | A handful of documents | ~60 KB | prove every binding contract on a fresh clone |
| 2 | Corpora of a few megabytes (the due-diligence, financial-advisory and threat-intelligence samples were measured at this scale) | ~17 MB | realistic scale, trivial cost; Part II's teaching tier |
| 3 | Blob-necessary corpora (the insurance-claims and history-revolution samples ran at 580 MB and 514 MB) | ~1.1 GB | the scale that makes object storage necessary, not just correct |

At Tier 3, blob storage runs about $0.02/GB/month. The whole set at that
scale is cents. The real costs of loading live elsewhere: extraction (a
scanned-PDF corpus may need the paid document-intelligence escalation, so
budget before enabling, §16), and embedding at index time if you move off
the local embedder. Start at Tier 2 scale for design work; the anatomy is
identical at every tier.

### What you own, what you inherit

The division of labor per stage is worth pinning above your desk because
the "you own" column is the entire surface area of your application
outside its UI:

| Stage | You own | You inherit |
|---|---|---|
| Corpus | the documents, the path/prefix layout, keeping answer keys out | content addressing, idempotent ingest, blob placement, extraction |
| Shape | fact vocabulary, supersession identity, chunk size | validation at the gate, index identity, chunking execution |
| Runbook | collections + levels + compartments, prefix bindings, retrieval/model policy, the completion template | binding evaluation, the validator, versioning, soft removal |
| Run | pressing approve (and deciding who may) | side-by-side builds, verify, cutover atomicity, version retirement, the execution record |
| Sessions / search | queries, rendering answers + envelopes + insufficient-evidence states | clearance filtering, hybrid fusion, provenance envelopes, audit capture |

Everything in the right column is the same for every application on the
platform, which is what "the mesh owns the truth discipline" means in
practice. Everything in the left column is YAML, an HTTP call, or a
decision.

### Where this leaves you

You have now built a small but complete corpus application:
documents in under a deliberate prefix layout, a shape and a
runbook in git, an approval-gated run, and answers that arrive wearing
their provenance. The miniature made every choice the easy way: one
collection, level 0, default retrieval knobs, four documents that fit in
one chunk each. §16 is about making those choices *well* when the corpus
is real. These include prefix layout as access design, plus levels and
compartments that
model your organization, the retrieval mechanics you should understand
even though you will rarely tune them, and the extraction realities that
decide whether your documents are actually *in* the index you built. The
due-diligence application stays on the table throughout; §21 loads all
613 documents and takes it end to end.

## 16. Designing retrieval for a corpus

Section 15 built an application where every choice was easy: one collection,
level 0, default settings, and four documents that each fit in one chunk.
This chapter covers those choices for a real corpus. It orders them by how
hard they are to revise.

The prefix layout is almost fixed once you upload. The clearance model is
fixed once users depend on it. You mostly inherit the retrieval mechanics
and need to understand them. You must *check* extraction results, while you
can trust the index lifecycle. The design flows in that order, and so does
the chapter.

Everything below ran against v0.1.2 on the §15 rig. The source server used
ports 18080/19090, and compose Postgres used 5433. The run used a fresh
tenant, `ch16-7493`. Its central example captures one runbook, two clearances,
and disjoint knowledge.

### Prefix design is access design

Recall the miniature's one binding rule: `filenamePrefix` matching is a
literal `starts_with` over the ingest filename, with no globs or path parsing.
and the filename is simultaneously the document's identity, its blob path,
and the string collections bind
([loading-corpora.md](loading-corpora.md) is the operational treatment).
Now add the fact this chapter turns on: **levels and compartments attach to
collections, and collections bind prefixes.** Chain those together and the
conclusion is the section title. Your corpus's directory layout *is* your
collection topology. Your collection topology *is* your access-control
topology. The folder scheme you create before upload is not just a tidiness
choice. It is the governance document that filters every retrieval decision.
Design it like one.

The two committed reference topologies show the two shapes most corpora
take.

**Due-diligence: folders drawn on clearance boundaries.** The Northgate
data room ([due-diligence.yaml](../../runbooks/applications/due-diligence.yaml),
§15's dissection) uploads 613 documents under `northgate/<area>/…`, one
numbered folder per data-room functional area, one collection per folder.
Three design choices are worth copying. First, the folders follow the data
room's *own* functional areas. M&A practice already draws this structure
along access lines, so the model comes from the domain instead of being
invented.

Areas that differ only by topic share a level. Corporate,
commercial, real estate, insurance, and operations are all level 1. Areas
that differ by *audience* use compartments. Equity, finance, and tax are
level 2 behind `finance`. IP and legal compliance are level 2 behind `legal`.
They have the same seniority but separate need-to-know rules. Second, the numbered prefixes
(`northgate/03_finance/`) sort the tree in room order and guarantee no
prefix is accidentally a prefix of another. Third, the room's index document
lives alone in a level-0 collection. The one thing everyone may see is the
catalog of what exists.

**Support-knowledge: a prefix per source system.** The support world
([support-knowledge.yaml](../../runbooks/applications/support-knowledge.yaml))
scatters about 1,500 documents across ten source systems. These include
tickets, forums, KB articles, chat, email, bugs, release notes, postmortems,
DOCX policies, and PDF contracts. Each system has its own prefix and
collection. The reason is not that they are ten topics. In a real deployment,
they have ten owners and ten levels of sensitivity. Public-facing knowledge
(KB, forum, release notes) at level 0, tickets and the bug tracker at
level 1, chat/postmortems/mail at level 2, signed contracts at level 3
behind `legal`.

Two refinements the due-diligence topology did not need:
every binding here also declares `mediaTypes:`. This is required for the
`docvault/` DOCX policies and `contractstore/` PDF contracts, which bind by
content type as well as prefix. The mail archive declares *two*
compartments, `[support, legal]`. Under the subset rules below, this means a
caller must hold **both** to see it.

**What makes a prefix scheme good.** Distilled from those two and the other
eleven committed runbooks:

- **Boundaries follow governance, not topics.** Ask "who must *not* see
  this folder?" instead of "what is this folder about?" Folders that answer the
  first question identically can share a collection.
- **End prefixes in `/`.** `starts_with` on `northgate/03` would also match
  `northgate/030_other/`. The validator warns only for the runbook-level
  `spec.sources.prefix` (`sources.prefix-unterminated`). Per-collection
  `filenamePrefix` bindings get no such check, so that discipline is
  yours. The one committed exception is deliberate: due-diligence's index
  collection binds the filename stem `northgate/000_` to catch exactly the
  catalog file.
- **No bound prefix nests inside another** unless you intend a document to
  live in two collections (each binding is independent; overlap is legal
  and occasionally right, but it should be a decision, not an accident).
- **Declare `spec.sources.prefix`** so `mmctl runbook validate` errors
  (`sources.prefix-mismatch`) on any collection binding a path outside the
  runbook's declared tree. This check catches typos before they become
  invisible empty collections.
- **Leave room to grow.** A new sibling folder is cheap: one new collection
  in a new runbook version, one run. *Splitting* an existing folder into two
  sensitivities is not cheap, for the reason that follows.

**The immutability warning.** A prefix rename is not a rename. The path is
the identity: upload the same bytes under a new path and you have a *new*
source with a new `source_id`, bindable and retirable independently
(loading-corpora.md's third consequence). There is no move API or delete API.
Removing index data requires a deliberate manual DBA runbook
(a partition detach), not an endpoint. So restructuring a live corpus means
re-ingesting every affected document under the new layout, rebuilding, and
carrying the old sources as harmless but permanent residents of the old
paths. Ten minutes of layout design before the first upload is the cheapest
insurance this platform sells. Treat the prefix scheme like a database
schema: reviewed in the same pull request as the runbook that binds it.

### Levels and compartments: modeling your organization's clearances

The entire authorization model of the data plane is one function:
`AccessCtx::permits` in `munarium-access`
([src/munarium-access/src/lib.rs](../../src/munarium-access/src/lib.rs)):

```rust
pub fn permits(&self, level: i32, compartments: &[String]) -> bool {
    self.level >= level
        && (self.all_compartments
            || compartments
                .iter()
                .all(|c| self.compartments.iter().any(|have| have == c)))
}
```

In plain words: a caller sees a collection **iff** their level dominates the
collection's level *and* they hold every compartment tag the collection
carries. Level is the vertical axis. It is hierarchical clearance, starting
at 0 for public data, with strictly nested audiences. Compartments are the
horizontal axis. They are named need-to-know sets compared by subset and are
deliberately *not* nested.
This is Bell-LaPadula "simple security" with categories: one comparison, no
roles, no groups, no policy engine, no rule files to keep synchronized.
Where do a caller's `lvl` and `cmp` come from? From the capability token
your API manager minted (§14). The mesh never decides who is cleared for
what. It *enforces* the clearance your identity layer asserted at session
creation and on every retrieval.

Two subtleties in that function bite integrators, so name them now. First,
**level dominance does not clear a compartment**: a level-9 token without
the `finance` tag still cannot see a level-2 `finance` collection. The two
axes are independent, and the demonstration below executes exactly this
case. Second, `all_compartments` is how the control-plane static `rw` token
(and `MUNARIUM_AUTH_MODE=disabled`) sees everything: it clears the compartment
gate *unconditionally*. A capability JWT is always `all_compartments:
false`. An empty `cmp: []` clears nothing compartmented. Operator
convenience exists, but it is not reachable from a minted token.

Design guidance, distilled from the committed applications:

- **Use few levels.** No committed runbook needs more than four (0–3). A
  level is only right when the audiences are strictly nested. The rule "everyone
  cleared at N may see everything at N and below" must be literally true.
  The moment two audiences at the same seniority must not see each other's
  material, that is not a level 4 and a level 5; it is two compartments at
  one level (due-diligence's `finance` vs `legal`, both at 2).
- **A compartment is a need-to-know set, not a team.** Compartment-per-team
  reproduces your org chart in token claims and rots at the first reorg.
  Compartment-per-*data-sensitivity* (`hr`, `security`, `legal`) stays
  stable while teams move around it.
- **Multiple compartments on one collection mean AND.** Support-knowledge's
  mail archive (`[support, legal]`) is visible only to callers holding
  both. There is no OR; if you need "either support or legal", that is two
  collections over two prefixes.
- **Uniform level 0 is honest for public corpora.** History-revolution's
  LOC archives and regulatory-compliance's CFR are wholly public; their
  runbooks are uniformly level 0 and accept the validator's
  `collections.uniform-access` Info finding as correct modeling. Do not
  invent sensitivity your corpus does not have.
- **Keep the two vocabularies apart.** The runbook maps *data* to
  requirements; the API manager maps *people* to claims. They meet only
  inside `permits()`. The day your app hard-codes "managers are level 2"
  anywhere except the manager's minting logic, you have built the second
  identity system §14 warned about.

None of this is folklore: the conformance scenario
`platform.application-and-compartments`
([conformance/src/platform.rs](../../conformance/src/platform.rs)) runs
the full lifecycle, shape, runbook, matcher-routed ingest, per-collection
approvals. It then creates a level-0 and a level-2-plus-compartment session.
Both ask the same question. The scenario asserts disjoint result sets on both wire
transports. The behavior below is contract, not implementation detail.

### The two-clearance demonstration

The chapter's main example was reproduced live for the book. It uses the §15
rig, whose anatomy is explained there. Section 14 adds two roles: an `rw`
token for operator work and a `mgmt` token for capability-token minting.
`MUNARIUM_TOKEN_SECRET` enables minting. A fresh tenant ensures that the shown
calls created every id below:

```powershell
# env exactly as §15's miniature, except these two lines and the tenant:
$env:MUNARIUM_STATIC_TOKENS = 'ch16tok:ch16-7493:rw,ch16mgmt:ch16-7493:mgmt'
$env:MUNARIUM_TOKEN_SECRET = 'dev-guide-ch16-secret-32-bytes-min!!'
cargo run -p munarium-server
```

The corpus is four markdown documents about one corporate event, split
across two prefixes with a planted difference. Under `briefing/press/` is
what the world knows: an acquisition announcement whose key sentence is
"Financial terms were not disclosed," and a Q2 results release. Under
`briefing/finance/`, what the deal team knows: a valuation memo ("$84M cash
at close, plus a $9M earn-out") and a board financing memo. The runbook
turns that split into clearance (shape `briefing-notes@1` is §15's pattern,
elided):

```yaml
spec:
  sources: { container: sources, prefix: "briefing/" }
  collections:
    - name: press-public
      shape: briefing-notes@1
      accessLevel: 0
      sources: { filenamePrefix: "briefing/press/" }
    - name: finance-internal
      shape: briefing-notes@1
      accessLevel: 2
      compartments: [finance]
      sources: { filenamePrefix: "briefing/finance/" }
  retrieval: { topK: 5 }
  # steps: §15's five, verbatim — approval on cutover, keep_versions: 2
```

Apply, batch-ingest the four files (the matcher routes each to its
collection at ingest; the output is abridged), run, and approve **two** cutovers,
because the run executes the pipeline once per collection:

```json
{ "results": [
  { "filename": "briefing/press/meridian-announcement.md",
    "source_id": "src-1f963556386653f4", "bound_to": ["press-public"], "…": "…" },
  { "filename": "briefing/finance/meridian-valuation.md",
    "source_id": "src-90582b48c4f00655", "bound_to": ["finance-internal"], "…": "…" } ] }
```

```text
POST /v1/runbooks/ch16-briefing/runs  ->  run-019ff41e2ada7153bf8268e73f0b14f8
  2  buildIndex:press-public       done   { index_version: idx-a45848a2ff892dfc }
  3  buildIndex:finance-internal   done   { index_version: idx-5e3ada519dd3b2ff }
  6  cutover:press-public          approved by casey.ops
  7  cutover:finance-internal      approved by casey.ops
state: done
```

Now the exhibit. The `mgmt` plane mints two capability tokens through the
route from §14, so only the decoded claims are shown. Ana is a public-side
analyst, while Finn is on the deal team:

```json
{ "sub": "ana.analyst@example.com",  "ten": "ch16-7493",
  "lvl": 0, "cmp": [],          "scopes": ["query"],
  "jti": "tok-019ff41d4b707e30aa09be451065f7c1", "…": "…" }

{ "sub": "finn.finance@example.com", "ten": "ch16-7493",
  "lvl": 2, "cmp": ["finance"], "scopes": ["query"],
  "jti": "tok-019ff41d4ba373b281bee19bb3cc5fbd", "…": "…" }
```

Each opens a session against the **same runbook**. The sessions already
disagree about what exists because clearance is captured at creation:

```json
{ "session_id": "ses-019ff41e4fb07f20a8ded4564920c31b",
  "runbook_ref": "ch16-briefing@1",
  "permitted_collections": ["press-public"] }

{ "session_id": "ses-019ff41e4feb77b08a07bff85f66dfbb",
  "runbook_ref": "ch16-briefing@1",
  "permitted_collections": ["press-public", "finance-internal"] }
```

Then both sessions receive the same question:
`{"query": "What are the terms of the Meridian acquisition?"}`. Ana's turn:

```json
{ "session_id": "ses-019ff41e4fb07f20a8ded4564920c31b", "ordinal": 1,
  "collections_searched": ["press-public"],
  "hits": [
    { "collection": "press-public",
      "source_path": "briefing/press/meridian-announcement.md",
      "text": "…Financial terms were not disclosed.", "score": 0.0328, "…": "…" },
    { "collection": "press-public",
      "source_path": "briefing/press/q2-results.md",
      "text": "…the Meridian transaction remains on track…", "score": 0.0161, "…": "…" } ],
  "envelopes": [
    { "collection": "press-public",
      "envelope": { "index_version": "idx-a45848a2ff892dfc", "…": "…" } } ], "…": "…" }
```

Finn's turn:

```json
{ "session_id": "ses-019ff41e4feb77b08a07bff85f66dfbb", "ordinal": 1,
  "collections_searched": ["press-public", "finance-internal"],
  "hits": [
    { "collection": "press-public",
      "source_path": "briefing/press/meridian-announcement.md",
      "score": 0.0328, "…": "…" },
    { "collection": "finance-internal",
      "source_path": "briefing/finance/board-deal-terms.md",
      "text": "…The $84M consideration is funded $60M from the revolving
               credit facility…", "score": 0.0164, "…": "…" },
    { "collection": "press-public",
      "source_path": "briefing/press/q2-results.md", "score": 0.0161, "…": "…" },
    { "collection": "finance-internal",
      "source_path": "briefing/finance/meridian-valuation.md",
      "text": "…$84M cash at close, plus a $9M earn-out tied to FY2027
               EBITDA…", "score": 0.0161, "…": "…" } ],
  "envelopes": [
    { "collection": "press-public",
      "envelope": { "index_version": "idx-a45848a2ff892dfc", "…": "…" } },
    { "collection": "finance-internal",
      "envelope": { "index_version": "idx-5e3ada519dd3b2ff", "…": "…" } } ], "…": "…" }
```

![The two-clearance demonstration: one runbook with a level-0 public collection and a level-2 collection behind a finance compartment. The clearance filter evaluates permits() for each collection before ranking, so a level-0 token sees one collection and one envelope while a level-2 token with the finance compartment sees both](images/ch16-clearance-filter.svg)

Read the two responses side by side. The design rule becomes concrete:
**retrieval is filtered by clearance before ranking, not after.** A turn
first filters the runbook's collections through the session's saved clearance
by calling `permits` for each collection. It runs hybrid search inside each
permitted collection's partition. Only then does it merge the lists by score
([src/munarium-server/src/sessions_api.rs](../../src/munarium-server/src/sessions_api.rs),
[src/munarium-retrieval-pg/src/collections.rs](../../src/munarium-retrieval-pg/src/collections.rs)
`merge_hits`). The announcement has the *same* 0.0328 score in both turns.
Its score is computed within `press-public`. Changing clearance can reorder
the merged list, but it cannot change a hit's score.

Ana's two hits are not "Finn's four minus two redactions." The finance chunks
were never candidates. They were never scored, ranked, or trimmed from a
longer list. Her response says nothing about the name, document count, or
existence of `finance-internal`.

The envelopes come **one per collection**, each naming its own index
version, so every slice of the merged hit list is independently verifiable
(§15's single-envelope rule, scaled to the session plane).

Two corollaries, also executed. A third token at **level 2 with the wrong
compartment** (`cmp: ["legal"]`) opens a session and gets
`"permitted_collections": ["press-public"]`. Level dominance was
satisfied, but the subset check was not. The axes are independent. Ana
calling filtered `POST /v1/search` directly against `finance-internal`
gets:

```json
{ "type": "https://munarium.ioka.io/problems/not-found", "status": 404,
  "detail": "not found: collection finance-internal",
  "kind": "collection", "id": "finance-internal" }
```

A **404, not a 403**, is returned to an uncleared caller. For that caller, a
restricted collection does not exist, and the error channel will not confirm
otherwise. This is a
design decision, not an accident of routing: the search handler builds the
same `not-found` problem it would for a name that never existed:
"never reveal that a collection the caller cannot clear exists"
([src/munarium-server/src/rest.rs](../../src/munarium-server/src/rest.rs),
`rest.rs:850`; the `kind`/`id` extensions are the registry's documented
`not-found` shape, [errors.md](../api/errors.md)).

This design gives your application three benefits. First, you never
post-filter results in your code, where any bug could cause a *leak*. Second,
a model completing over a turn's hits (§17) can see only what the user may
see. Prompt injection cannot cross a clearance boundary that retrieval never
crossed. Third, `permitted_collections` at session creation gives the UI an
honest capability list. Show the scope the user *has* and never list what
they lack.

### The hybrid mechanics you inherit

You will rarely tune retrieval, but you will read its outputs and field its
quality questions, so you need the mechanics at working depth.

Every filtered `/v1/search` and session turn runs **two legs** over the
collection's chunks. The first is a lexical leg (Postgres full-text,
`tsvector`) and a vector leg (pgvector ANN over per-collection HNSW
indexes). Each leg returns up to `candidate_n` ranked candidates. The
two rankings are fused by **reciprocal rank fusion**: a chunk's score is
`Σ 1/(k + rank)` across the legs it appears in, `k = 60`
([src/munarium-retrieval-pg/src/lib.rs](../../src/munarium-retrieval-pg/src/lib.rs),
`rrf_fuse`). RRF is a deliberately simple choice. It compares *ranks* rather
than raw scores, so the two legs need no score normalization. A chunk ranked
first by both legs beats one ranked first by only one leg. Ties break in a
stable way on chunk id. That rank-not-score property holds *within* one
collection only: a multi-collection turn must not compare per-collection RRF
scores across collections (every collection's rank-1 ties), which is why the
cross-collection merge re-fuses the pooled candidates globally from each
hit's raw leg measurements — §13.5 entry 17 tells that story.

The
`lexical_rank` / `vector_rank` fields on every hit (§15's stage-5 exhibit
carried them) are your diagnostic: they tell you *which leg found it*, which
is the first question to ask when a retrieval quality report lands on your
desk.

The knobs and their defaults (`SearchParams`,
[src/munarium-retrieval-pg/src/collections.rs](../../src/munarium-retrieval-pg/src/collections.rs)):
`top_k: 10`, `rrf_k: 60`, and `candidate_n: 50`. The runbook's
`retrieval:` block (due-diligence raises `candidateN` to 120,
support-knowledge to 150; wider corpora keep more candidates per leg so
fusion has something to fuse). These are **query-time** knobs: changing them
requires a new runbook version and takes effect at once with no rebuild.
Chunking (`max_chars`, in the *shape*) is different. It is part of index
identity, and changing it means rebuilding, which is the lifecycle section's
subject.

**The embedder you get for free is deterministic and keyless.**
`local-hash@1` is 256-dimensional, L2-normalized feature hashing. It is a hash
function, not a model. Be clear about what that means: it sharpens the
lexical signal and is fully reproducible from bytes, but it does not
understand paraphrase. "Attorney" will not retrieve "lawyer" on the vector
leg. It is not a demo crutch, though: the measured retrieval results
§19 cites were earned with lexical-first discipline, and
every example in this book runs on it without a single provider key.

**When to bring your own embeddings.** The telltale is vocabulary mismatch:
your users ask natural-language questions in words the corpus does not use,
and `vector_rank` is null on the hits that matter. The fix is a
configuration choice, not code: the provider gateway's embedding surface
(OpenAI-dialect providers, which also covers Azure-OpenAI-style compatible
endpoints; Anthropic exposes no embeddings API) swaps in per index build.
The embedder, provider, model, and dimensions are **part of index
identity** and named in every manifest and provenance envelope
(`provider_fingerprint`).

You are accepting three costs: a rebuild of every
collection through its approval-gated cutover; a bill of about *chunk count ×
your provider's embedding price*; and a measurement duty. A Tier-2 corpus
costs dollars, not hundreds. Run your
graded question set before and after (§19's method), because "better
embeddings" is a hypothesis, not a fact. Embedding calls are cached by
request hash, so subsequent rebuilds re-pay only for text that actually
changed. The platform-side plumbing is Part I's business (§6's
provider-adapter recipe; §8's index-identity treatment); from your seat it
is one provider config and one run.

### Extraction realities by corpus type

A document you uploaded is not necessarily a document you can retrieve.
Extraction turns stored bytes into text the chunker can use. It runs at
**index time**, not ingest time: the raw bytes stay canonical in the object
store, which is why an extractor improvement is a rebuild, never a
re-upload. What extraction can actually deliver depends entirely on what
your corpus is made of:

| Corpus type | `extraction_method` | Cost |
|---|---|---|
| Born-digital text (markdown, plain, CSV, JSON) | `text` | free, exact |
| DOCX | `docx` | free, local |
| PDF with an embedded text layer | `pdf-text` | free, local |
| Scanned PDF, JPEG/Flate-encoded pages | `ocr` (feature-gated) | free, local, quality varies |
| Scanned PDF, **JBIG2/CCITT**-encoded pages | none | the wall: no pure-Rust decoder exists |

Every source row records how extraction went. Here is the demonstration
corpus's valuation memo, read back after the build:

```json
{ "source_id": "src-90582b48c4f00655",
  "filename": "briefing/finance/meridian-valuation.md",
  "extraction_status": "ok",
  "extraction_method": "text", "…": "…" }
```

`extraction_status` is `ok`, `empty`, or `failed`, and **`empty` is the
invisible-document signal**: the source is present, listed, findable by
name and contributed zero chunks. No retrieval will ever surface its
content. A scanned PDF with no text layer reads `empty`; so does
whitespace-only output, deliberately, because calling it `ok` would hide
the miss. Extraction never fails a build (a bad document is recorded
per source while the rest still index). This is why you must *look*. After
the first build over any new corpus type, sweep your sources
for `empty` before you trust a single answer. An invisible document is the
failure mode nobody reports, because every query about it returns a
confident answer from everything else.

The wall row is real and measured: JBIG2 and CCITT encodings are common in
older court filings, and legal-appeal's 358 Juliana PDFs are the committed
example ([loading-corpora.md](loading-corpora.md) tables the per-corpus
realities).

For exactly those documents there is the **document-intelligence
escalation**. Read [document-intelligence.md](document-intelligence.md)
before enabling it. This hosted analyzer reads what local extraction
cannot, at roughly **$1.50 per 1,000 pages** (Azure `prebuilt-read`). It
only runs for documents local extraction could not read. Clean corpora cost
nothing, no matter their size.

It is **off by default** for policy reasons.
The service bills per page and
it **sends your documents outside the cluster**, which for a data room
under NDA or anything with a residency obligation is a decision a human
makes once in writing. It is not a framework default. Before committing a
scanned corpus either way, sample a dozen PDFs and count real text layers;
that decides whether indexing costs pennies or real money. And note the
last honest detail: `extraction_method: ocr` is recorded per source because
OCR'd text and a genuine text layer are *not equivalent evidence*, and an
application reasoning over citations deserves to know which it got.

### The index lifecycle from the application seat

Everything above becomes an **index version**. Index versions are immutable
and identified by content:
`index_version = hash(shape_ref, chunker, embedder, sorted source set)`,
with the extractor set version joining the identity too, so an extractor
improvement correctly invalidates and rebuilds. The lifecycle is §15's run
pipeline, and this chapter's demonstration exercised the part the miniature
could not: the *second* build.

The demonstration corpus was edited after its first run. It was only a
wording pass, but the type of change does not matter. The first run had built
and activated `idx-2b07ec136e61e89f` / `idx-1294e8661a3dc49f`. Re-ingesting
the four files under the **same paths** returned the same four
`source_id`s with new `sha256` values. The identities stayed the same, but
the bytes changed, so a rebuild was due. The second run, whose step listing appears
above) built `idx-a45848a2ff892dfc` / `idx-5e3ada519dd3b2ff` **side by
side** while sessions kept answering from the old active versions. It paused
at two cutover gates. Approval then switched each collection at once.
The turn captures in the exhibit name the new versions in their envelopes.
that is how you know which build answered.

From the application seat, the lifecycle contract is three promises:

- **You never see a half-built index.** Search and sessions always read the
  *active* version. A build in progress is invisible until a human approves
  its cutover. Your application needs no maintenance mode, no "reindexing,
  please wait" state, no cache-busting logic.
- **You notice a rebuild only if you look.** The `index_version` in the
  provenance envelope changes; hits may differ. If your UI shows "answered
  against index …" (and after §17 it should), that field updating *is* the
  notification.
- **Old answers stay verifiable.** `retireOld: { keep_versions: 2 }`
  reclaims old chunk *rows* eventually, but retired versions' manifests
  keep resolving. A provenance envelope your application stored in
  March still verifies in August against the exact index that produced it
  (invariant 5, made operational).

Keep the following rebuild table beside the runbook:

| Change | Rebuild? |
|---|---|
| Document bytes at a bound path; sources added/removed under a prefix | yes; next run builds a new version |
| Shape chunking (`max_chars`), embedder, extractor set version | yes; identity changed |
| Runbook `retrieval:` knobs (`topK`, `rrfK`, `candidateN`) | no; query-time, new runbook version suffices |
| `models:` policy, `completion:` template | no; resolved per call (§17) |

### Where this leaves you

You will make the design choices in a clear order. Draw a prefix layout on
governance boundaries, review it like schema, and fix it before the first
upload. Use a few strictly nested levels and compartments for same-level
audiences that must stay separate. One comparison enforces this model, as the
two-clearance example showed. Inherit the hybrid mechanics: two RRF legs and
a keyless default. Treat BYOK embeddings as a measured choice that requires a
rebuild. Sweep `extraction_status` to find invisible documents. Trust a
lifecycle whose rebuilds users never see and whose old answers remain
verifiable.

As of 2026-08-19 this checklist is also *executable*: the server's guided
authoring surface asks exactly these questions in exactly this order
(`GET /v1/authoring/patterns`, `POST /v1/authoring/drafts`, `mmctl
author` — see runbooks/README.md "Authoring your own set") and
deterministically materializes the shape + runbook your answers imply,
with set-level validation and a hash-manifested export bundle for the
GitOps path to production.

With the design complete, users can now *talk to* the corpus. Section 17 puts
the model and the conversation on top: sessions and turns, completion
routing under the runbook's model policy, the grounding discipline a chat
application needs, and exactly what your UI should render from every turn.
§19 catalogs the patterns when your corpus resembles one already worked
through; §21 loads a whole data room and takes the due-diligence application
end to end.

## 17. Chat and AI-assisted research sessions

Section 16 left you with a designed corpus that users can talk to. This
chapter covers the **session/turn plane**, including the two routes a chat UI
may call thousands of times a day (three since 2026-08-23, counting the
streamed form of the turn). It also covers the grounding rules that
decide whether users should trust the result.

The chapter has two linked halves. The first covers server mechanics. It
shows what a session pins, what a turn returns, and how much conversation the
server carries between turns. The answer is **none**. The second half gives
four grounding lessons. A chat UI will meet these failure modes quickly, so
its design must account for them.

Everything in the first half was executed against v0.1.2 on the standing
Part II rig. The source server used ports 18080/19090, and compose Postgres
used 5433. The run used a fresh tenant, `ch17-a41c`. The small example has two
collections and the five stages from §15. Only two details matter here: a
`harbor-press` collection (level 0, one press release) and a
`harbor-filings` collection (level 1, an executed agreement and a permit
note), bound by a runbook `harbor-desk@1` that declares `retrieval: {topK:
4}`, a `models:` block, and a `completion:` template. The corpus has three
documents chosen to show the conversation-state finding. The press release
announces a berth expansion. The filings record who signed the agreement and
when. A user asks those three questions in a natural, anaphoric order.

### The session: what gets pinned, what gets snapshotted

A session is created against a runbook by name and belongs to the uid that
opens it:

```bash
curl -s -X POST http://127.0.0.1:18080/v1/runbooks/harbor-desk/sessions \
  -H "Authorization: Bearer $ALICE_TOKEN" -H "X-Munarium-Uid: alice@example.com"
```

```json
{ "session_id": "ses-019ff42acc92782396cfdc4845596efb",
  "runbook_ref": "harbor-desk@1",
  "permitted_collections": ["harbor-press", "harbor-filings"] }
```

Two things happened at that moment. Both protect a conversation from changes
in the outside world
([sessions_api.rs](../../src/munarium-server/src/sessions_api.rs)
`op_create_session`; the sessions table is
[0013_sessions.sql](../../src/munarium-store-pg/migrations/0013_sessions.sql)):

- **The runbook ref was pinned.** The bare name `harbor-desk` resolved to
  the latest non-removed version at creation. The *resolved*
  `name@version`, `harbor-desk@1`, is what every turn of this session will
  load. Publish `harbor-desk@2` tomorrow, and new sessions get it. This
  session does not. A mid-conversation version cutover never shifts the
  ground under an ongoing exchange.
- **The clearance was snapshotted.** Alice's token carried level 1, no
  compartments. The session recorded exactly that (`GET /v1/sessions/{id}`
  echoes `"access_level": 1, "compartments": []` beside `"state": "open"`).
  Every future turn filters the runbook's collections through the
  *snapshot*, not through whatever token arrives with the request. The
  comment in `op_turn` says it in ten words: *"Access filtering uses the
  SESSION's snapshot, not the live token"* (sessions_api.rs:223).

`permitted_collections` is the least-privilege echo. It lists the collections
this session will search after filtering. Had Alice been minted at level 0,
the response would have read `["harbor-press"]` alone, and we captured
exactly that below.

The snapshot semantics are worth one demonstration because they cut both
ways. After the turns you are about to see, we minted a *second* token for
the same uid at **level 0** and used it against the *existing* session, then
to create a new one:

```text
turn on the OLD session with the new level-0 token:
  ordinal 6, collections_searched: ["harbor-press", "harbor-filings"]
NEW session created with the level-0 token:
  permitted_collections: ["harbor-press"]
```

The open session kept its level-1 snapshot. The downgraded token still drew
`harbor-filings` hits, while the new session got the smaller world. This is
the intended behavior because a mid-session token change must not alter an
ongoing conversation.

The behavior has an important operational result: **a clearance downgrade
does not reach inside open sessions.** If your API manager demotes or
offboards a user, revoke the token with
`POST /v1/access-tokens/{jti}/revoke`. The turn's auth check enforces
revocation when that check is enabled (§14). Do not trust the next mint to
change an existing session.

The sessions table has a `state` column with `open | closed | expired`
values, and `op_turn` rejects a non-open session — since 2026-08-17 with
the typed `session-not-open` slug, and closure is finally operable:
`POST /v1/sessions/{id}/close` ends a session idempotently (owner or
rw/mgmt; §13 entry 11, closed). Token lifecycle remains the second
control, and the idle-EXPIRY sweep stays a tracked follow-up.

Two more limits of the pin come from
[platform-features.md](platform-features.md) §8: the pin protects
against *new versions*, not against an in-place re-apply. If you re-`POST` the
same `name@version` with changed YAML and each turn re-reads the current
text of that ref, so retrieval knobs, the completion template, and the model
policy can shift mid-conversation (the snapshot still caps reach: a session
never *gains* collections beyond its saved clearance. Publish a new version
when a running conversation must not change at all). A removed runbook
answers every session route with `410 runbook-removed`.
render it as "this assistant has been retired", not as a crash.

### Turns, demonstrated: three questions, and the finding

A turn is `POST /v1/sessions/{id}/turns` with a query. (A streaming twin,
`POST /v1/sessions/{id}/turns/stream` (2026-08-23), runs the identical turn
— the same `op_turn`, the same persistence, the same refusals — but
delivers it as SSE `progress` events at each real stage boundary, then one
`done` event carrying the same TurnResponse this chapter walks through, or
one `error` carrying the problem+json. Everything below applies to both;
"The streaming twin" after the completion path covers the event vocabulary,
the failure-delivery rule, and the same-day buffering defect it surfaced.)
The three questions a
person actually asks, in order:

```bash
d1='{"query": "What did Harbor Terminal announce about Pier 4?"}'
d2='{"query": "Who signed it?"}'
d3='{"query": "And when?"}'
```

Turn 1 behaves as §16 described. Hybrid search runs across both permitted
collections. The server pools every collection's candidates, re-ranks them
globally per leg by their raw measurements (`ts_rank`; cosine distance),
fuses the global ranks, and tags each hit with its collection (since
2026-08-24 — §13.5 entry 17; per-collection RRF scores are rank-derived and
not comparable across collections, and merging by them starved relevant
collections. The transcript below was captured before that fix, so its
`score` values are the old per-collection RRF numbers):

```json
{ "session_id": "ses-019ff42acc92…", "ordinal": 1,
  "collections_searched": ["harbor-press", "harbor-filings"],
  "skipped": [],
  "hits": [
    { "collection": "harbor-press",
      "source_path": "harborco/press/berth-expansion-announcement.md",
      "source_content_hash": "00f2996bfa57052b…", "text": "…", "score": 0.0328 },
    { "collection": "harbor-filings",
      "source_path": "harborco/filings/berth-expansion-agreement.md", "score": 0.0164 },
    { "collection": "harbor-filings",
      "source_path": "harborco/filings/pier4-dredging-permit.md", "score": 0.0161 } ],
  "envelopes": [ { "collection": "harbor-press",   "envelope": { … } },
                 { "collection": "harbor-filings", "envelope": { … } } ] }
```

(With only three documents, all of them land inside `topK`, so the *scores*
tell the story. A score of 0.0328 means both retrieval legs ranked the
document first. A score near 0.0164 comes from one leg. The press release
signals on both legs in turn 1.)

`skipped` names each permitted collection that has no active index. Such a
collection is reported rather than silently dropped. Each collection also
returns its **own** provenance envelope because each answer came from its own
immutable index version. Here is turn 1's `harbor-filings` envelope in
full; §14 called this the "verifiable as of" affordance and §16 built the
index identity it names:

```json
{ "collection": "harbor-filings",
  "envelope": {
    "chunk_ids": ["src-34741fd36638e35f#0", "src-99e1a1e8887f6805#0"],
    "source_ids": ["src-34741fd36638e35f", "src-99e1a1e8887f6805"],
    "source_paths": ["harborco/filings/pier4-dredging-permit.md",
                     "harborco/filings/berth-expansion-agreement.md"],
    "source_content_hashes": ["07b7c7a2fdb6ab8c…", "a6abe0147d7d78f7…"],
    "index_version": "idx-91ed3f8ce322f75c",
    "event_watermark": 0,
    "provider_fingerprint": "local/local-hash@1/256" } }
```

Now consider turn 2, `"Who signed it?"`, using the scoring lens from §16:

```text
ordinal 2:  berth-expansion-agreement.md   score 0.0328   ← both legs
            berth-expansion-announcement.md score 0.0164
            pier4-dredging-permit.md        score 0.0161
```

The right document ranks first. It *looks* like the server understood "it,"
but it did not. Open
[sessions_api.rs](../../src/munarium-server/src/sessions_api.rs) `op_turn` and
trace the query's path: the handler loads the session row, filters
collections through the snapshot, and calls `search_collection(&info.id,
&req.query, …)`: **the literal query string, nothing else**. No prior turn
is loaded before searching; the completion prompt (below) is built from
`{context}` and `{query}` alone. The agreement ranked first because the word
*"signed"* appears in its text. This is lexical luck, not conversation state. Turn
3 removes the luck:

```text
ordinal 3, "And when?":
            berth-expansion-announcement.md score 0.0164
            berth-expansion-agreement.md    score 0.0164
            pier4-dredging-permit.md        score 0.0161
```

This is rank noise. `"And when?"` contains no useful content term. No document
signals on both legs, so the three scores are close to a coin toss. A UI that
sends raw user phrases into turns may work well in demos because follow-ups
often repeat a word from the prior question. It will fail when users depend
most on context.

So here is the finding, stated as bluntly as the code states it:

> **The server persists conversation state; it does not use it. Each turn
> is an independent retrieval of the literal query string. Carrying the
> conversation by resolving "it," "and when?," or "the second one" into a
> query that stands alone. This is the client's job by design.**

The persistence half is real and important. Every turn is appended to
`session_turns` with its ordinal, query, hits, envelopes, and completion. The
ordinal is allocated inside the INSERT with a unique-violation retry. This
makes concurrent turns serialize per session (sessions_api.rs:561-575).

`GET /v1/sessions/{id}` returns the whole transcript in order. That is your
audit trail, page-refresh state, and support view. A capability token reads
only sessions for its uid under the same scope and revocation rules as a
turn. Control and mgmt tokens can read any session in the tenant. The server
does not read this transcript back into retrieval.

![A session timeline: the client owns conversation state across three anaphoric messages and rewrites the third before sending, so only the query string crosses the boundary. The session pins a runbook version and snapshots the token's level at creation, and each turn independently runs snapshot-filtered retrieval and returns one provenance envelope per collection](images/ch17-session-turn.svg)

What the client must therefore do is query rewriting, and the captures
show both the fix and its sharp edge. Our first rewrite of turn 3 was
`"When was the Pier 4 berth expansion agreement signed?"` It was plausible
and specific, but *wrong for this corpus*. The execution record never contains
the string "Pier 4", so the lexical leg found no document matching all
terms and the scores stayed noise (ordinal 4, all three hits ≈0.016). The
second rewrite reused the vocabulary the conversation had already been
*served*. Turn 2's top hit is titled "Berth Expansion Agreement" and says
"Signed":

```text
ordinal 5, "When was the berth expansion agreement signed?":
            berth-expansion-agreement.md    score 0.0328   ← both legs again
```

The transferable rule: **rewrite queries out of the evidence you have
already shown the user, not out of the assistant's paraphrase of it.** The
The hit `text` from prior turns is the corpus's own vocabulary. A rewrite built
from it inherits the corpus's terms. Whether you rewrite with a cheap model
call ("rewrite this follow-up as a standalone question, reusing the quoted
evidence's wording"), with the condenser pattern (below), or by
concatenating the user's last utterance with the previous turn's top-hit
title, the contract is the same: by the time a query crosses the wire, it
must stand alone.

### The completion path (described from code, not executed on this rig)

Everything above ran keyless and free. A turn may also carry
`"complete": true`. The path below is described from code with anchors
because this rig has no provider credentials. The guide does not fake
transcripts. The path begins in
`op_turn` (sessions_api.rs:195, the completion branch from :316):

1. The runbook must declare `spec.completion.promptTemplate`; asking for
   completion without one is `400` ("this runbook declares no completion
   step", sessions_api.rs:321).
2. The merged hits form a context block, with each hit rendered as
   `[collection/chunk_id] text`, accumulated until `CONTEXT_CHAR_BUDGET`
   (16,000 chars, sessions_api.rs:27) would be exceeded. It then stops. The
   template's `{context}` and `{query}` placeholders are substituted
   literally.
3. The provider and model are chosen by the **one task-level model
   resolver** ([models.rs](../../src/munarium-server/src/models.rs)
   `resolve_model`). Every model-using task in the server follows the same chain:
   walks: **request override → `models.tasks.completion` →
   `models.default` → the tenant default-provider rule** (anthropic, then
   OpenAI, then OpenRouter, using the first family with a usable credential.
   Section 11's
   `/healthai` table shows exactly which). The resolved choice records its `source`
   (`"override" | "task" | "runbook-default" | "tenant-default"`) and a
   `was_override` flag (sessions_api.rs:332).
4. The call goes through the provider gateway with `max_tokens` set to
   the tenant's `turn_completion` budget — built-in 2,048 since 2026-09-02,
   when every per-call output budget was doubled (1,024 before), settable
   with `MUNARIUM_MAX_TOKENS_TURN_COMPLETION` on the container or replaced
   as part of the whole set through `POST /v1/max-tokens`
   (`max_tokens_api.rs`; reference: docs/tokenbudgets.md); a runbook's
   `completion.maxTokens` wins over all of those. That number is a
   **ceiling, not spend**: the provider bills the tokens the model
   generates, and the ceiling only bounds the worst case. The turn
   response gains `completion: { provider, model, was_override, text,
   input_tokens, output_tokens }`. The same facts are written into the
   turn row's `completion` column, which is how `GET /v1/reports/cost`
   can split native from overridden spend (§20).
5. **The truncation-aware retry (2026-08-23).** Reasoning models —
   `gpt-5.4`, `z-ai/glm-5.2`, exactly the models the `capable` tier
   resolves to for two of the three families — draw their hidden
   reasoning from that same completion budget. A turn could therefore
   exhaust the ceiling before any visible text and come back with an
   empty `completion.text` under a perfectly honest model badge. §11's
   `/healthai` probe already handled this trap for its probes (256 tokens
   then, 512 since 2026-09-02);
   the turn path now does too. When the provider's stop reason is
   `max_tokens` (Anthropic dialect) or `length` (OpenAI dialect) — the
   adapters pass the stop reason through verbatim — or when the text is
   empty, `op_turn` pays for **exactly one** retry at four times the
   budget (sessions_api.rs:403-420). `input_tokens`/`output_tokens` on
   the response are the sum of both calls, so the readout stays honest
   about what the turn cost. The opt-in corrective verification retries
   of §13.5 entry 10 inherit the raised budget (sessions_api.rs:489), so
   a reasoning model gets the same headroom for its repaired answer. The
   ceiling itself did not move — a 4× ceiling on every turn would change
   nothing on non-reasoning models and only widen the worst case.
   Reproduced live the morning it landed: a short OpenRouter turn cost
   328 output tokens for a roughly 140-token visible answer; the rest was
   reasoning. The same lesson shows up in narrative chat (lesson 1 below),
   and this is that stop-reason retry ported
   server-side. A turn that still comes back empty after the retry is
   not an error — it is a renderable state, and §17's table below names
   it.

The **override policy** is the part we *could* execute keyless, because the
gate fires before any provider call. Our runbook's `models:` block declares
no `allowOverrides`, and the policy defaults closed:

```bash
curl -s -X POST …/turns -H "$AT" -H "Content-Type: application/json" \
  -d '{"query": "When was it signed?",
  "complete": true, "model_override": {"provider": "openai-main"}}'
```

```json
{ "type": "https://munarium.ioka.io/problems/override-not-allowed",
  "title": "model override not allowed", "status": 403,
  "detail": "runbook 'harbor-desk@1' does not permit overriding to provider 'openai-main' (models.allowOverrides)" }
```

The executed result was real and free. The 403 landed before a token was spent. To open
the gate, the runbook names providers (`allowOverrides: [default]` is the
committed experiments' idiom because it overrides tier, not provider) or sets
`allowOverrides: true`. A bare model/tier override rides the runbook's
resolved provider and is still policy-checked. An override *without*
`complete: true` is rejected outright rather than silently ignored
(sessions_api.rs:555-558). This is your fast/capable toggle: a runbook that
defaults `tasks.completion` to the capable tier can let the UI request
`{"tier": "fast"}` per turn for cheap factual follow-ups. This follows §19's economics
row, with every choice recorded and attributable.

Two executed details complete the picture. First, asking for
completion on this keyless rig returns `502 provider-error` with a detail
that names exactly what was checked (`"no default provider credential
configured (checked MUNARIUM_SECRET_ANTHROPIC, MUNARIUM_SECRET_OPENAI,
MUNARIUM_SECRET_OPENROUTER and applied configs)"`). Second, **a turn that fails at
completion is not persisted**: the transcript still held five turns after
that 502 because the INSERT happens after the completion call succeeds.
The retrieval work is done and discarded. A resilient chat client treats a
completion 502 as retryable *without* fearing duplicate transcript rows,
or runs the retrieval turn first and requests completion as a second turn
only when it needs one.

### The streaming twin: the same turn as phase progress (2026-08-23)

A turn with completion and verification is several seconds of silence
from the user's seat: retrieval across every permitted collection, an RRF
merge, a policy-checked model resolution, a paid completion, possibly a
second paid completion for truncation, possibly two more for
verification. A chat UI that shows a spinner for all of it is hiding the
very stages §14 asked you to make renderable. `POST
/v1/sessions/{id}/turns/stream` (`turn_stream`, sessions_api.rs:779-844)
exists for that UI. It takes the same `TurnRequest`, runs the **same**
`op_turn`, persists the same `session_turns` row, applies the same
auth and refusals — and answers as `text/event-stream`.

The mechanism is deliberately small and honest by construction. `op_turn`
gained an optional `TurnProgressTx` (an unbounded mpsc sender,
sessions_api.rs:187); the unary route and the gRPC twin pass `None`, the
streaming route passes a sender, and `emit` is a no-op when nothing is
attached. Events are sent at the real stage boundaries in the code — the
retrieval loop, the merge, the resolver, each completion, each
verification pass — nothing is synthesized, and a dropped receiver never
fails the turn (the client can vanish; the turn still completes and
persists). The forwarder task drains the progress channel to SSE; the
terminal event is sent only after the forwarder has exited, so `done` or
`error` is always strictly last.

The event vocabulary, from `TurnProgressEvent` in munarium-api-types
(internally tagged on `stage`):

| SSE event | `stage` | Payload | Emitted when |
|---|---|---|---|
| `progress` | `retrieval` | `collection`, `hits`, `skipped` | once per permitted collection, in order; `skipped: true` with `hits: 0` is a collection with no active index (the same fact `skipped[]` reports on the response) |
| `progress` | `merge` | `hits` | the RRF merge across searched collections is done |
| `progress` | `model` | `provider`, `model?`, `tier?`, `was_override` | the completion model resolved and passed policy, *before* any paid call; `model` is absent when only a tier resolved — the concrete id then appears on the `completion` event |
| `progress` | `completion` | `attempt`, `provider`, `model`, `input_tokens`, `output_tokens` | one paid completion returned: `attempt: 0` for the first answer (the truncation retry re-emits `attempt: 0` with its own token counts), `1..` for corrective verification retries |
| `progress` | `verify` | `attempt`, `checks[]`, `violations` | a deterministic verification pass ran over the current answer (`checks` names `quotes`/`citations`; non-zero `violations` means a corrective retry may follow) |
| `done` | — | the full `TurnResponse` | exactly once, last, on success |
| `error` | — | the problem+json body | exactly once, last, on any failure after the stream opened |

The one thing to get right on the client is **where a failure arrives**.
Anything refused *before* the stream starts — no uid, a bad body, a
token the data-plane guard rejects — is an ordinary problem+json
response with its status. Anything that fails *after* — including
`override-not-allowed`, `session-not-open`, a provider 502 — is an
`error` event on an HTTP **200**, because the status line was sent when
the stream opened. Executed on this chapter's keyless memory-mode rig,
both shapes in two calls against a session that does not exist:

```bash
# no X-Munarium-Uid: refused before the stream opens — plain problem+json
curl -s -i -X POST …/v1/sessions/ses-0000/turns/stream \
  -H "Authorization: Bearer devtoken" -H "Content-Type: application/json" \
  -d '{"query":"x"}'
```

```text
HTTP/1.1 400 Bad Request
content-type: application/problem+json

{"type":"https://munarium.ioka.io/problems/uid-required","title":"uid required","status":400,
 "detail":"X-Munarium-Uid header (REST) / munarium-uid metadata (gRPC) is required on every /v1 request"}
```

```bash
# with a uid: the stream opens, and the refusal is its terminal event
curl -s -i -X POST …/v1/sessions/ses-0000/turns/stream \
  -H "Authorization: Bearer devtoken" -H "X-Munarium-Uid: user-1" \
  -H "Content-Type: application/json" -d '{"query":"x"}'
```

```text
HTTP/1.1 200 OK
content-type: text/event-stream
cache-control: no-cache
x-munarium-request-id: req-01a02f33f0db728294ce1c739109cd27
content-length: 204

event: error
data: {"type":"https://munarium.ioka.io/problems/invalid-input","title":"invalid input","status":400,
       "detail":"invalid input: this endpoint requires the postgres store (MUNARIUM_STORE=postgres)"}
```

Key every branch on the problem slug inside the `error` payload, exactly
as §20 says for the unary plane; the HTTP status of a stream tells you
only that it opened.

Now look at what the second capture carries that it should not: a
**`content-length`**. An SSE response with a known length is a stream
that was buffered somewhere, and it was — the REST interaction-capture
middleware read every `/v1` response body to completion in order to
record it (`to_bytes(res_body, usize::MAX)`), the SSE body included, so
on the morning the route shipped the event sequence reached the client
**in one burst at turn end**: framing intact, ordering correct, nothing
live. The streamed OpenRouter turn had been verified end-to-end the same
morning, and that verification proved what it could prove — the event
contract, the strictly-last terminal event, the persisted turn — but a
client reading a complete, well-formed event sequence cannot tell from
the events alone whether they trickled or arrived together. The header
could. This is the book's standard working as intended: a composed
description of the stream would have said "live" and been wrong. It was
filed as §13 entry 16 and closed the same day; the capture above is
kept because it is the evidence, and because the tell is worth knowing.

The fix (middleware.rs, `SseCapture`) is the gRPC plane's own shape
brought to REST: when the handler's response is `text/event-stream`, the
head goes out immediately with its `x-munarium-request-id`, the body is
wrapped and forwarded frame by frame with no exact size hint (so hyper
uses chunked transfer), and the interaction row plus the RED metrics are
recorded when the stream ends — the latency is the real elapsed time, the
response is summarized as `{streamed: true, content_type, bytes_len}`,
and a `StreamOutcome` slot the handler inserts into the response
extensions supplies what a stream has no finished response to carry: the
same session/runbook/collection attribution the unary turn attaches, and
the status of the **terminal event** (200 on `done`, the problem status
on `error`), so the row and the metrics record the outcome rather than
the 200 the stream opened with. Re-executed on the same rig after the
fix, the same request:

```text
HTTP/1.1 200 OK
content-type: text/event-stream
cache-control: no-cache
x-munarium-request-id: req-01a02f6b194f74a1a6c0e9fe46c6a5fc
transfer-encoding: chunked

event: error
data: {"type":"https://munarium.ioka.io/problems/invalid-input","title":"invalid input","status":400, …}
```

and on the ops plane, the call's RED metric recorded under the event's
status, not the head's (the interaction row, which memory mode does not
persist, carries the same status by the same code path):

```text
munarium_http_requests_total{plane="rest",route="/v1/sessions/{id}/turns/stream",method="POST",status_class="4xx"} 1
```

Two regression tests hold it (middleware.rs, `tests`). The first is a
gate, not a timer: a handler sends one event and then *waits for the
test to acknowledge it* before sending the second and ending; a buffering
middleware can never return the response head, so the test fails by a
five-second timeout rather than by timing luck (it was run against the
buffered path on purpose and failed in 5.01 s). The second drives the
real route through the real router and asserts the body advertises no
exact length, the refusal rides the terminal `error` event, and the
recorded metric is the 4xx the event carried.

One honesty remains. The route is **REST-only**; `session.proto` has no
server-streaming `Turn` yet, so a gRPC client gets the unary turn and
nothing in between.

### The grounding lessons

The mechanics above give you hits with hashes and a completion template. They
do not guarantee that the completion *tells the truth about the hits*. Every
lesson below applies to your UI, because the failure lives in the model rather
than in a specific retrieval loop.

One honest note must come before the numbers. Since 2026-08-18 the server
implements the deterministic half of these lessons: an opt-in runbook block
(`completion.verification: {quotes, citations, maxRetries}`) makes the turn
loop check that quoted spans resolve verbatim in served hit text and that
bracketed citations name served content, with up to two corrective
completions carrying the violations and the full served context
(§13.5 entry 10; `munarium-server/src/verification.rs`). **[gap]** *What is
NOT ported is fetch-on-cite*: the corrective round re-serves what the turn
already served — it cannot fetch a cited-but-unserved document without a
fresh retrieval round — and lesson 1 below is exactly why that matters. The block is also opt-in and default-off.

The lessons below therefore remain *your* checklist — for runbooks that
do not opt in, and for the fetch-on-cite half the server does not do. A
client can implement each one from the data a turn already returns.

**Lesson 1: models cite what they saw but never read. Serve the evidence
instead of scolding.** Deterministic single-question asking is remarkably
scale-invariant — growing a corpus by an order of magnitude costs little
accuracy. Narrative-synthesis *chat* turns are where grounding breaks: models
cite documents that search results *surfaced* — titles, dates, snippets — but
that they never fetched.

A fetch-before-cite instruction in the prompt does not fix this, and neither
does a verify-retry that merely names the violations. Detection without
evidence is not repair: the model can only repeat the answer, remove useful
content, or spend its remaining budget fetching. The fix that moves the score
is **fetch-on-cite** — when verification finds ungrounded citations, *serve
the cited documents' content* in the corrective round instead of only naming
them.

Expect an unmasking rather than a clean win the first time you turn it on:
turns that finally pass the deterministic layer then fail judged narrative
quality on the merits, which is the honest state to be in. What your UI
inherits: if
you build an answer-verification pass (and lesson 2 says you should), its
retry prompt must *include the text* of every cited-but-unverified source.
You have it in `hits[].text`. Do not send only a bare
instruction to "fix or drop citation X".

**Lesson 2: quotes must resolve verbatim in served text, and citations
must name content actually served.** The striking thing about iterating a
grounded Q&A application is how little of the work is about answer quality:
the content is right almost immediately, and the movement is contract
compliance. Models retrieve the right documents early and then leak points on
the evidence ceremony — uncited counts, unfetched ids, paraphrase presented as
quotation. Most of the fixes are declarative: prompt text and answer-key
hygiene, not runtime changes. The mechanical versions of this lesson are the
`verify_quotes` / `verify_citations` corrective retries.

Your
client can implement the detection half *deterministically today*: a quoted
span in the completion either appears verbatim in some served `hits[].text`
or it does not. String search needs no model. A cited `source_path` either
appeared in this turn's hits or it did not. Render what fails
the check as unverified (or spend one corrective completion with the
evidence attached, per lesson 1).

**Lesson 3: anaphora needs conversation state threaded and condensed by
the client, and carried facts should be restated.** Far more of "chat is
hard" comes from conversation plumbing than from model capability. Where a
chat arm looks broken, the cause is usually dialect friction and a strict
parser rather than wrong answers — protocol brittleness, not reasoning
failure.

A chat driver threads history into each turn and condenses it when it grows:
the last
few messages stay verbatim, everything older folds into one digest that
*must preserve backticked identifiers, verbatim quotes, and every number,
date, and section reference*. It uses a deterministic extractive fallback
when no model is available. Score `carry_retention` too: a follow-up answer
must **restate the facts it depends on** ("Restating the frame: this is
Boston, within the 1770–1776 window…"), which makes carried
context visible, checkable, and cheap to verify. Your UI inherits the whole
pattern because the server carries nothing, as shown above.

Keep the
transcript client-side (or re-read it from `GET /v1/sessions/{id}`),
condense it with exactly that preservation contract when you feed a model,
rewrite follow-ups into standalone queries, and prompt your completion to
restate its antecedents so a user (and your QA) can see what the assistant
thinks the conversation established.

**Lesson 4: cited-or-insufficient beats confident-and-wrong, and
insufficiency is a *success* state.** An answer must either carry citations
that resolve or declare that the corpus does not establish it, and your
grading must score an honest insufficiency as **correct behavior**. Build
negative controls — decoy probes and scoped-absence questions — that pass on
exactly that.

The leanest possible answer, a denial backed by an exact count and nothing
more, is the right one. A more "helpful" version that garnishes a correct
denial with two facts it was never served is worse, not better: on absence
answers, every extra helpful fact is a new liability surface.

There is another side to this. When evidence is an exact metadata count,
forcing citation ceremony onto it causes failures; the fix
(`allow_uncited_count`) *widens* what counts as properly grounded rather than
weakening the contract. Your inheritance is a prompt clause and
a render state: the committed completion templates end with a version of
*"if the context does not establish the answer, say insufficient evidence
and stop"* (ours does; the thirteen committed under
[runbooks/applications/](../../runbooks/applications/) all carry a version
worded for their corpus). Your UI must present that outcome as the system
working, which it is.

### What the UI renders from a turn

Section 14 defined a governed application as one whose failure modes become
renderable states. A turn response is a bundle of those states. The table
lists them in the order a user meets them:

| Turn field | Render as |
|---|---|
| `completion.text` (or your own model call over `hits`) | the answer; verify every quoted span against `hits[].text` before adding quotation marks (lesson 2) |
| `completion.text` empty after the truncation retry (2026-08-23) | a designed state, not a blank bubble — render something like "(The model returned no answer text this turn — its token budget went to hidden reasoning. Ask again to retry.)" rather than an empty answer |
| `completion.provider` / `.model` / `.was_override` + `input_tokens` / `output_tokens` | the answer badge: which model actually answered, whether the user's override took, and what the turn cost in tokens (both calls summed when the truncation retry fired); pair it with the pre-turn label from `GET /v1/providers` (§11) so the selector and the badge agree |
| `progress` events on the streaming twin | a phase strip — one chip per event: each collection searched with its hit count (or "skipped"), the merge, the resolved model, each completion with its token counts, each verification pass with its violation count — rendered as events arrive (live since the 2026-08-23 capture fix — see "The streaming twin"); design the strip to read well as a summary too, because the `done` payload carries the same facts for a page refresh |
| `hits[]` (`source_path`, `collection`, `text`, `score`) | citations as first-class links: path + collection badge, click-through to the source (its bytes are addressable by `source_id`), snippet on hover |
| `envelopes[]` (`index_version`, `event_watermark`, `source_content_hashes`) | an expandable "answered against index …" line per collection; this makes the answer re-verifiable in August (§16's lifecycle promise) |
| the insufficient-evidence outcome (lesson 4) | a designed state: "the corpus does not establish this," with the searched collections listed; never an error toast |
| `skipped[]` | a caveat chip: "N collections you can see have no active index yet", the honest partial-coverage warning |
| `permitted_collections` (at create) | scope-setting furniture: show the user what worlds this conversation can draw from; it also explains *why* two colleagues get different answers (§16's two-clearance capture) |
| `403 override-not-allowed`, `410 runbook-removed`, session-uid mismatch | policy states with human wording, "this assistant's model policy is fixed", "this assistant has been retired" |

Add the two client duties created by the finding: rewrite before you send,
and verify before you render. That table *is* the chat UI's contract. Note
what is absent. Nothing requires trusting the model.
Every affordance is grounded in fields the server computed
deterministically.

A reference rendering of every row was built and measured: a server-side
BFF over exactly this contract — several corpus pages behind one menu, a
provider × tier selector labelled pre-turn from `GET /v1/providers`, the
model and token badge on every answer, the streamed phase strip, the
insufficient-evidence and empty-answer states, and verification badges. It
is not part of this repository. When a row in the table feels abstract, the
official clients' session guide
([clients/docs/guides/sessions.md](../../../clients/docs/guides/sessions.md))
is the executable reading of it: each client exposes the same fields as
typed members.

### Model economics per turn

The turn structure gives you a clear cost dial. Retrieval is Postgres work
and is nearly free per turn. Completion is the only metered call, and the
shape of the bill is worth internalizing before you budget.

A graded question set of a few dozen questions over a mid-sized corpus costs
around a dollar on a capable-tier model and rather less on a fast tier. A
couple of dozen chat turns cost a fraction of that. The expensive case is
synthesis-heavy chat over a very large corpus with many turns, which can run
to double digits per run. Chat over a governed corpus is cheap. Large
*synthesis* contexts are where the money goes.

You already control the operational settings. The runbook's
`models.tasks.completion` sets the native tier. `allowOverrides` decides
whether the UI may drop to `fast` for follow-ups or climb to `capable` for
synthesis. The committed support-knowledge runbook uses a fast-tier default
with a capable tier available per turn, which is the shape to copy: the fast
tier is the honest default and the capable tier stays one override away.
`GET /healthai` is the preflight that tells you which
provider families have usable credentials before a user's first turn
(each call spends real tokens, so it is a deploy-time check rather than a
health poll; §11), and `GET /v1/providers` is the free, pollable
disclosure of *which* model each family's tiers will use — a runbook's
`allowOverrides` can name an OpenRouter config as a third family (fast
`deepseek/deepseek-v4-flash`, capable `z-ai/glm-5.2` by default), and a
UI's selector should label those names from that route rather than from a
hard-coded table. `GET /v1/reports/cost` splits native from overridden spend,
which keeps override costs visible (§20). Per-tenant
budget enforcement sits where your gateway sits, in front (§14's division
of labor).

One economics detail is new enough to state plainly. Both `capable`
tiers outside Anthropic now resolve to reasoning models, and reasoning
models spend output tokens you never see. The 2,048-token completion
ceiling (1,024 until 2026-09-02) is a *ceiling* — a non-reasoning model answering in 140 tokens
is billed 140 — but a reasoning model can burn the ceiling on hidden
reasoning and answer with nothing, which is what the completion path's
truncation-aware retry now catches (one retry at 4×, keyed on the
provider's stop reason or empty text). Budget for it: the live
reproduction was 328 output tokens for a ~140-token visible answer on
the OpenRouter capable tier, and a retried turn reports both calls'
tokens summed. If a workload is mostly short factual follow-ups, the
`fast` tier is not only cheaper per token; it also skips the class of
turn that pays twice.

The deeper point about tiering (§19's catalog): fast-tier
models mostly fail *ceremony*, such as uncited counts and dropped
restatements. They do not usually fail retrieval. Verification (lesson 2)
therefore recovers more of the gap than
upgrading the model does, at a fraction of the price.

### Where this leaves you

You now have the complete conversational data plane. Sessions pin a runbook
version and save a clearance so nothing shifts during an exchange. Turns
return clearance-filtered evidence with provenance envelopes for exactly the
query you send. Your client owns the conversation. The completion path
resolves model choice through policy and records `was_override`. Four
measured rules turn served evidence into answers a user can check.

Sessions cannot *know* things. You can ask many questions about the berth
agreement, but nothing accumulates. No fact survives the conversation. The
system does not notice a conflict between this month's filing and last
month's filing. A transcript cannot answer "what did we believe in March?"

Section 18 covers that accumulation. It explains extraction into the
governed ledger, disputed claims as a review queue, point-in-time pins, and
the pattern that serves canonical facts beside retrieval. The patterns
catalog in §19 holds the chat row this chapter has been building toward
(pattern 2, research chat), and §21 runs a session like this one over a whole
data room, end to end.

## 18. Beyond RAG: canonical memory over the corpus

§17 ended on retrieval's honest limit: sessions answer, but nothing
accumulates. Ask about the berth agreement every day for a month and the
corpus answers every time. On the day two filings disagree about the
signing date, the assistant answers from whichever fragment ranked first,
confidently, differently on different days. Nothing noticed the fight.
Nothing can say what the answer *was* in March. This chapter is the turn
from an application that **finds passages** to one that **knows things**:
extraction into the governed ledger, disputed claims as a review queue,
point-in-time pins as the compliance answer, the composed brief as your
prompt backbone, and the blend pattern that serves canonical facts beside
document retrieval.

You have already operated every base mechanism. The Introduction's
first-contact walkthrough showed this machinery at toy scale: a claim
accepted, a contradiction landing `disputed` with a finding, a correction
superseding through the front door, a pin returning the pre-correction
world, a composed brief with a `content_hash`. This chapter does not re-run
those six commands; it re-frames them as *application architecture* and
adds the three things first contact could not show: obligations
(promises), budget degradation in the brief, and the two-surface prompt.

### First, the honest gate: when not to build this

Canonical memory is the part of the platform with real adoption cost. It needs an
extraction capability to prompt, a review queue to staff, a claim-key
vocabulary to keep disciplined. So the when-not-to comes first. The
strongest single-question results come from applications using **zero**
ledger machinery: a well-designed corpus (§16) and a grounded
session loop (§17) carry the entire load. One-shot Q&A over a static
corpus needs none of this chapter.

Reach for the ledger when a **requirement smell** appears. Any one is
sufficient, and they compound:

- **Contradiction matters.** Your sources disagree, such as a stale KB article versus a
  release note, two versions of a contract, PROFILE-2020 vs the schedule it
  anachronistically contradicts. "Whichever fragment ranked first" is
  a wrong answer someone will act on. You need disagreement to be a
  *detected, reviewable event*.
- **Time matters.** Somebody with authority, such as a regulator, a litigator,
  or your own postmortem, will ask "what did we know when we decided?" A
  a transcript of what was *said* is not an answer about what was *known*.
- **Obligations matter.** The corpus creates commitments, including filings due,
  renewals owed, and payoffs promised. "Still open?" must be answerable
  on any date, including retroactively.

If none of the three is in your requirements, stop reading and go build
§16–§17. If one is, everything below was measured before it was written
down.

![Two memory surfaces converging on one prompt: the document index returns clearance-filtered hits with provenance and says what documents say, while canonical memory sends extracted claims through deterministic gates so accepted claims enter the budgeted brief and disputed claims enter a human review queue that never reaches the brief](images/ch18-beyond-rag.svg)

### The rig, and the world

Everything below was executed against v0.1.2: the from-source binary on
the Part II alternate ports (REST 18080, ops 19090), fresh tenant
`ch18-b7e2`, uid `dana@example.com`, with one deliberate difference from
§17's rig: `MUNARIUM_STORE=memory`. Every route in this chapter is the
kernel plane, and the conformance suite proves kernel behavior identical
on the memory and Postgres backends (§7), so the throwaway store
demonstrates exactly what production would. Headers are shown once and
elided after, per the Introduction's standards; command bodies want
`Content-Type: application/json` alongside the three headers you know:

```bash
B=http://127.0.0.1:18080
V=$(curl -s -X POST $B/v1/versions \
  -H "Authorization: Bearer ch18token" -H "X-Munarium-Uid: dana@example.com" \
  -H "Content-Type: application/json" -H "Idempotency-Key: $(uuidgen)" \
  -d '{}' | jq -r .version_id)
# -> memv-69f6bccd620e468da6ff7a66783d00ee
```

The world is §17's harbor, one step later in the engagement: the analyst
team has finished *talking* to the corpus and now wants the application to
*hold* what the documents established: who operates Pier 4, what the
expansion costs, when the agreement was signed.

### Extraction into governed memory

The pipeline has three stages, and only the middle stage uses a model.
Documents already in your collections (§15–§16) go through an extraction
pass. Your prompt and provider account ask for short `subject`/`key`/
`value` triples as JSON. The results go to
`POST /v1/versions/{id}/claims`, with one write per claim.

The division of labor from §14 stays the same: the model *proposes*, while
the mesh runs deterministic gates and *disposes*. Your extraction
prompt needs no notion of contradiction, because detecting contradiction
is not the model's job.

Four claims extracted from the filings land as canon (responses
compressed to the fields that matter):

```text
seq 1  pier4.operator=Harbor Terminal Group   accepted
seq 2  pier4.budget=$18.5M                    accepted
seq 3  pier4.berths=3                         accepted
seq 4  agreement.signed_on=2026-03-12         accepted
```

Then the press release goes through the same pass, and its two claims meet
the ledger:

```bash
curl -s -X POST $B/v1/versions/$V/claims … \
  -d '{"claim_type":"fact","subject":"pier4","key":"budget","value":"$21M"}'
```

```json
{ "claim": { "id": "claim-57cd0d9efdee41fe88fbc540e07670e8",
             "normalized_text": "pier4.budget=$21M",
             "status": "disputed", "seq": 5, … },
  "findings": [
    { "rule_id": "gate.ledger-conflict", "severity": "block",
      "message": "claim 'pier4.budget=$21M' conflicts with accepted canon 'pier4.budget=$18.5M' (use a correction to supersede)",
      "detail": { "claim_key": "pier4.budget",
                  "canon_claim_id": "claim-c7e865ad74c646558b60874a67197e51",
                  "canon_seq": 2, "canon_value": "$18.5M",
                  "proposed_value": "$21M" } } ],
  "head_seq": 5 }
```

The press release's `agreement.signed_on=2026-04-02` lands the same way at
seq 6. It is `disputed`, with `gate.ledger-conflict` and canon
`2026-03-12` named in the detail. Two documents disagreed with the filings twice, and both fights
are now *data*: HTTP 200, sequence consumed, nothing dropped, nothing
silently overwritten. (Extraction batches that should stand or fall
together have a dedicated route. `POST /v1/versions/{id}/events` gates a
batch as one unit. `expected_head` gives you the optimistic-
concurrency loop when multiple extractors write one lineage; the client
libraries encode the retry.)

### The red-flag queue is a query, not a feature you build

Here is the reframe that makes this an application chapter rather than an
API tour. In a naive pipeline, those two conflicts are the failure case,
the thing your error handling swallows. On the mesh they are the
**product**. A due-diligence application's core screen asks, "show me every
place the data room disagrees with itself." It requires one query:

```bash
curl -s "$B/v1/versions/$V/facts?statuses=disputed" …
```

```json
{ "facts": [
    { "seq": 5, "normalized_text": "pier4.budget=$21M",
      "status": "disputed", "provenance": "witnessed", … },
    { "seq": 6, "normalized_text": "agreement.signed_on=2026-04-02",
      "status": "disputed", … } ],
  "as_of_seq": 0, "head_seq": 6 }
```

The `statuses` filter takes a comma list (`accepted,disputed`); unfiltered
reads default to `accepted`. This is why every brief, every fact read,
every downstream consumer sees only vetted canon unless it explicitly asks
to see the fights. The review queue is opt-in by construction.

One boundary this section originally taught you to design around —
**findings ride the write response, and only the write response** — closed
on 2026-08-17 (§13 entry 12): the server now persists findings at write
time and `GET /v1/versions/{id}/findings?severity=&rule_id=&as_of_seq=`
replays them, so the evidence pane behind a disputed claim survives a
dropped response. The two client-side patterns this section demonstrated
(persist the `findings` array yourself; reconstruct the pair by joining
the disputed row's `claim_key` against current canon) remain valid and
their captures below are unchanged — they were executed against v0.1.2,
before the endpoint existed, and the reconstruction join is still what
you want when you need CURRENT canon beside the historical finding.

The reviewer's three verdicts map to three actions. *The new value is
right*: post a `correction` naming `supersedes_id`. You watched this in
first contact step 4; the correction is accepted, canon moves, the audit
trail keeps both values forever. *The old value is right*: do nothing.
The disputed claim is already permanently recorded and permanently
excluded from briefs. *Genuinely unresolved*: leave it in the queue; the
queue is a state, not an error condition.

Does this work outside a toy? Point a consistency pipeline at a synthetic
M&A data room with a hand-verified finding key and the naive baseline surfaces
a small fraction of the planted red flags. Promise findings, governance
re-severity and alias normalization move that by roughly a factor of six on
the same suite — which is a statement about vocabulary discipline, not about
model strength.

The instructive negative: the *stronger* model's baseline can be **worse**.
Its more precise vocabulary makes conflicting claims about the same
real-world fact land under different keys, so they never collide at the gate.
The identity half behaves the same way — on a corpus where several actors hide
behind multi-vendor alias sets, alias-aware resolution is what collapses them
onto one entity, and it must do so without merging two real actors that
happen to look similar.

Read those two results as one engineering lesson for your extraction
prompt: **the gate can only catch what collides, and collision is a
vocabulary problem.** The normalization layers this relies on (terminology
folding, value canonicalization, entity registries) are driver machinery and
have not been ported to the server. On the server today, key discipline
lives in your extraction prompt.

Three rules carry most of the gains. First, keep values terse and
normalized — a number, a date, a short phrase. Without this rule a corpus
buries its one real conflict under phrasing-collision false positives.
Second, use one stable key per real-world fact. Third, keep **keys
dot-free**. The composite
`claim_key` is `subject.key`, and a dotted key silently corrupts the
identity the gates reason over (the lesson is baked into
[runbooks/shapes/knowledge-sources.yaml](../../runbooks/shapes/knowledge-sources.yaml)).

### Time: the as-of answer

The compliance question, *what did we know when the report was issued?*,
is unanswerable by transcript and unanswerable by index, because both
record what was said, not what was known. On the mesh it is a query
parameter. Every kernel query route takes `?as_of_seq=N`, and invariant #3
means one pin bounds *everything*: facts, promises, anchors, counters, and
the composed brief. They use the single clock every write shares. You saw the
fact-side pin in first contact step 5; the section after this one
demonstrates the pin doing something more surprising, on promises.

The worked validation is a sequential advisory arm.
Six dated documents build an advisory client's ledger in chronological
order. One of them carries a planted anachronism: an ownership
figure describing a later capital raise in an earlier document. The run
catches it as **exactly one** ledger conflict (`62%` vs accepted `68%`,
blocked, disputed), while the legitimate constraint ratchet across
the same documents, a liquidity floor moving $750K → $1.5M → $2.5M →
$2.75M, landed as clean supersessions.

Then the pinned reads answered the
auditor's question at every quarter: pinned to 2021-06-30 the ledger
reads a 60% equity cap, $1,500,000 floor, crypto prohibited; pinned to
2023-12-31 it reads 72%, $2,750,000, with crypto capped at 2%. Supersession is
resolved *at the pin*, never-superseded claims surviving it.

**Date-pinned** reads, where a calendar date is resolved to a seq pin, are
the natural next ask. Here the server rejects
`as_of_date` explicitly (`invalid-input`) rather than silently ignoring
it — and since the 2026-08-17 audit this is a documented DECISION, not a
lag: that kernel resolved dates through per-unit `as_of:` assertion-date
metadata, the server records no equivalent, and resolving against
`recorded_at` (ingestion time) would silently diverge from those
semantics exactly where date pins matter (the guard comment in
service.rs carries the full rationale). Until per-event assertion-date
metadata exists, your application owns the date→seq map, and the pattern
is one line of bookkeeping: after each ingestion batch, record
(`document effective date`, response `head_seq`) in your own store.
Compliance questions arrive as dates; you look up the seq; the pin does
the rest.

### Obligations: promises, and the pin's best trick

A promise is an obligation with a stable key. It is opened against the lineage
and fulfilled by key. The corpus said "the renewal must be filed," and
your application must be able to answer "was it?" indefinitely:

```bash
curl -s -X POST $B/v1/versions/$V/promises … \
  -d '{"key": "dredging-permit-renewal", "kind": "obligation",
       "description": "File the Pier 4 dredging permit renewal before expansion work begins"}'
# -> { "id": "prom-82574b7fa5734e9dbe730fcc6ae0cebe", "status": "open", "seq": 7, … }

curl -s "$B/v1/versions/$V/promises?status=open" …          # -> the promise, open
curl -s -X POST $B/v1/versions/$V/promises/dredging-permit-renewal/fulfill … -d '{}'
# -> { "fulfilled": true }
curl -s "$B/v1/versions/$V/promises?status=open" …          # -> { "promises": [] }
```

At head, the record shows both moments on the one clock: `"seq": 7,
"fulfilled_seq": 8`. And that is what makes the compliance story
mechanical. The obligation was fulfilled at seq 8; the report went out
when the ledger stood at seq 7. Was the obligation open when the report
was issued?

```bash
curl -s "$B/v1/versions/$V/promises?status=open&as_of_seq=7" …
```

```json
{ "promises": [ { "key": "dredging-permit-renewal", "kind": "obligation",
                  "status": "open", "seq": 7, … } ] }
```

**Open.** A promise fulfilled after the pin reads back open at the pin,
not as a special case your code implements, but as the same invariant that
pinned the facts (the kernel's `status_as_of` in
[munarium-core/src/promises.rs](../../src/munarium-core/src/promises.rs) is a
single match expression, and the conformance suite holds both transports
to it). Your
obligations dashboard needs exactly two queries: `?status=open` is the
live view; `?status=open&as_of_seq=N` is the deposition answer.

One kernel-vs-surface boundary closed on 2026-08-17 (§13 entry 14): the
kernel's ported overdue check (`find_overdue`, where open promises past
their `due_scope` become `gate.promise-unfulfilled` warn findings) is
now invocable from the wire — add `?overdue_scope=<scope>` or
`?final=true` to `ListPromises` and the response carries
`overdue_findings` computed over the full pinned slice. The client-side
filter this section originally prescribed still works and its captures
predate the surface.

### The composed brief as your prompt backbone

Everything so far produced governed *state*. The brief is how state
reaches a model. First contact showed `GET /context` at toy scale; the
application-scale question is the one every hand-rolled RAG prompt
answers badly: *what happens when the context does not fit?* Hand-rolled
prompt stuffing degrades by truncation. Whatever happened to be last is
gone, silently, unreproducibly. The composer degrades by **policy**, and
you can watch it:

```bash
curl -s "$B/v1/versions/$V/context" …            # no budget
```

```text
## Canon digest
[] 4 facts
pier4.operator=Harbor Terminal Group … agreement.signed_on=2026-03-12

## Accepted facts
pier4.operator=Harbor Terminal Group
pier4.budget=$18.5M
pier4.berths=3
agreement.signed_on=2026-03-12

## Open promises
[berth-commissioning-report] Deliver the berth 3 commissioning report to the port authority
```

(90 estimated tokens; a second promise from the harbor world is open
here. Note what is *absent*: the two disputed claims from the review
queue. Disputed never enters a brief. The model is briefed on vetted
canon only, which is the whole point of the gates.) Now shrink the
budget and watch the same brief degrade deterministically:
`?budget_tokens=` 80, 70, 60, 25:

```text
80 -> 76 tokens  digest coarsens to the rollup line; all 4 facts held
70 -> 66 tokens  oldest fact trimmed first (pier4.operator, seq 1)
60 -> 58 tokens  facts down to the newest (agreement.signed_on)
25 -> 45 tokens  facts gone; the rollup digest and the open promise REMAIN
```

Two design facts emerge from that ladder. Degradation has an *order*:
digest resolution coarsens first, then facts trim oldest-first, exactly as
the composer header states
([munarium-core/src/composer.rs](../../src/munarium-core/src/composer.rs)). Thus,
what survives pressure is a decision the platform made once,
deterministically, not an accident of string length. And the ladder has a
**floor**: at `budget_tokens=25` the brief still spends 45 tokens, because
the coarsest rung never silently drops open obligations. The composer
does not lie to fit a budget; if you need a hard cap below the floor,
enforce it client-side and know you chose to.

Then the receipt. Every brief carries a `content_hash`, and a pinned brief
is reproducible *forever*:

```bash
curl -s "$B/v1/versions/$V/context?as_of_seq=6" …   # run twice
# -> content_hash cb277cdc0453…  (identical both runs)
```

The pinned brief opens with its own honesty header: `## Point-in-time
pin / This brief reflects the ledger as of seq 6. Later facts are not
visible.` Pinned before seq 7, it contains no promises section at
all: the pin governs every section of the brief, not just facts. The
operational habit this enables is cheap and pays compound interest:
**log (`version_id`, `as_of_seq` = the `head_seq` you composed at,
`content_hash`) beside every model call your application makes.** That
triple is the March answer. Recompose in August, compare it byte for byte,
and verify the hash. It costs you three fields in a log line.

### The blend: what we know AND what the documents say

Now put the two surfaces together, as shown by the chapter-opening figure's converging
arrows. A knowledge application's hardest queries need both: *current
state* questions ("which bugs are fixed in 4.2?") that canon pre-answers,
and *evidence* questions ("show me the SLA language behind that") that
only retrieval can serve. The worked case is a knowledge corpus spread over
ten source systems, including planted conflicts where a
stale KB article claims a fix shipped in a version that the release notes
refute.

Serve a canonical release ledger *up front* as a first-class citable source,
with the corpus behind it as residual retrieval, and cross-source joins and
stale-KB conflict questions both land. Stale-KB-vs-release-note fights resolve
in canon's favour, and conflict answers still carry citations to both
documents. The `version_id` route uses the same fact block sliced live from a
real memory version.

The maintenance story came almost free. Rebuilding the lineage against the
epoch-2 evolved corpus cost **$0.09**. Cross-lineage drift reported **zero**
unexplained fact changes.

The honest framing for this book: a blend of this kind wants a retrieval
backend that indexes the fact block beside the corpus, and the server has
none. Nothing in `/v1/search` knows the
ledger exists, and that is a design choice, not an omission (§13.4
records the judgment call): retrieval stays a pure document surface, and
the prompt is your application's property. On the server today, the
blend is an **application-layer composition**, and it is two calls you
already know how to make:

```text
GET  /v1/versions/{id}/context?budget_tokens=1200      # what we KNOW
POST /v1/search {"q": …, "filter": {"collections": …}} # what the documents SAY
```

assembled by your turn handler (this is an illustrative structure, not an executed
transcript; the model call is yours, per §17's session loop):

```text
brief  = mesh.context(V, budget_tokens=1200)        # record content_hash
hits   = mesh.search(q, collections=permitted)      # keep the envelope
prompt = f"""
  ## What we know (canonical memory — cite as [ledger])
  {brief.text}
  ## What the documents say (cite as [doc_id])
  {render(hits)}
  ## Question
  {q}"""
answer = your_provider.complete(prompt)             # §17's verify-before-render applies
log(V, brief.as_of_seq, brief.content_hash, hits.envelopes)
```

The ordering is the part that matters: canon *first*, with retrieval as
residual. Ledger-up-front is decisive for controlling
values, because the model meets the vetted current state before it meets
the stale document that contradicts it. The composed brief's budget
parameter is what makes the two surfaces share one context window on your
terms. This composition is the architecture behind "the assistant knows
the current state AND can dig into the documents behind it." §19's
*living knowledge base* pattern is exactly this shape, measured.

### Chronology: the sixth gate, honestly placed

Corpora where dates are load-bearing, such as dockets, filings, and clinical
histories, need a gate that reasons about *order*: signed before
effective, filed before the deadline, no overlapping engagements. The
kernel ships the complete chronology engine
([munarium-core/src/chrono_gate.rs](../../src/munarium-core/src/chrono_gate.rs),
`check_chronology` in [gates.rs](../../src/munarium-core/src/gates.rs)):
claim values parse into intervals with explicit precision and an
uncertainty flag, and rules fire **only on CERTAIN violations**. "Circa
1943" beside an exact 1944 files nothing, because the parser never
guesses and an intentionally uncertain date is never a violation by
itself. It is declaratively armed by design: rules are an asset the
deployment declares, not an always-on heuristic.

The honest server status flipped on 2026-08-17 (§13 entry 13, closed):
the arming surface exists. `ChronologyRules` is a kind-sniffed
declarative asset (`POST /v1/chronology-rules`, `mmctl apply -f`)
with a declarative `chronology:` vocabulary, and a memory version arms
the gate by naming an asset in its creation metadata
(`{"chronology_rules": "<name>"}`); the write path then runs
`check_chronology` beside `run_gates` on both planes. Through v0.1.2 the
sixth gate was kernel-complete and conformance-tested, but unreachable
from the wire.

What this section originally prescribed as preparation is now simply the
input contract: put dates in claim *values* in ISO form, and
name temporal keys by the kernel's own convention (`*_date`, `*_on`,
`date_*`, `*_deadline`, `*_due`, and `*_when`). These are the default
temporal-key patterns in chrono_gate.rs. Our `agreement.signed_on` above is already the right
shape. This also means the signing-date conflict the queue is holding
is precisely the kind of fight the armed gate would adjudicate with
interval algebra instead of string inequality.

### Where this leaves you

You now hold both of the application's memory surfaces. The document index
answers *what the documents say*, with provenance (§16–§17). Canonical
memory answers *what we know*. Extraction passes through gates so contradiction
becomes a queue (`facts?statuses=disputed`) instead of a coin flip;
promises so obligations survive their conversations; one pin
(`as_of_seq`) so "what did we know when?" is a query parameter; a
composed, budget-degraded, hash-receipted brief so the model is briefed
on vetted canon by policy rather than by prompt stuffing.

The blend uses
two calls and one prompt when a question needs both surfaces at once. The
boundaries were stated where they live: date→seq mapping, overdue views,
finding persistence, and the blend composition sit in your application
today; chronology arming waits on a server surface. And per the house
rules, the platform-side halves are on the ledger in §13 entries 12–14.
The blend is recorded as §13.4's judgment call. Each is therefore scheduled
work you can track, not folklore you had to be told.

§19 turns these mechanisms into the catalog: *red-flag review*, *living
knowledge base*, and *entity-centric intelligence* are this chapter's
machinery arranged into shapes that were measured end-to-end, each with a
committed runbook to copy. And §21's worked tutorial runs the full cycle
you just saw in miniature. It extracts from two conflicting Northgate
documents, watches the gate file the dispute, renders the queue, corrects via
supersession, and re-asks under a pre-correction pin over all 613 documents
of the data room.

## 19. The application patterns catalog

Part II has been building toward this chapter. §15–§16 gave you the corpus
machinery, §17 the conversational plane, §18 canonical memory. What
remains is the question you actually arrived with: *which of these do I
assemble, for my corpus and my users?* This catalog answers by precedent:
seven application shapes, each worked through against a real corpus before
it was written down here.

How to use it: scan the summary table, pick the row that smells like your
requirements (the closing section makes the smells explicit), then read
that pattern's subsection. Each follows the same skeleton: *the shape*;
*what it composes*, with §-references back to the mechanics chapters,
which are not re-taught here; *where it lives today*; and *start from*,
naming the committed runbook to copy and the §21 tutorial steps that apply.
Appendix E holds the full thirteen-runbook cross-reference when you want the
precedent closest to your corpus rather than closest to your pattern.

One honesty rule governs the whole chapter, stated once so the
subsections can be brief about it. Several of these patterns want machinery
this server does not (yet) have: multi-round retrieval drivers with
verification retries, a consistency pipeline that reconciles hundreds of
sources, an entity registry, a drift engine, sweep orchestration.

The server owns
the governed substrate those instruments run on: ingest, collections,
clearance-filtered search, sessions, and the memory kernel. §13's
gaps ledger plus the **[gap]** markers through §17–§18 already showed you
individual seams. So every pattern below carries a *where it lives today*
verdict in two halves: what you can build on the server now, and what the
pattern's fuller form adds that you would build client-side or wait
for. No pattern here is aspirational, but several are bigger than the
server alone, and this chapter will not let you discover that in
production.

| Pattern | Shape | When to reach for it |
|---|---|---|
| **1. Ask the corpus** | single-question grounded Q&A with citations, graded cite-or-insufficient | A question is answerable from a bounded set of documents, and the honest answer is sometimes "the corpus does not establish this" |
| **2. Research chat** | multi-turn sessions, anaphora, condensed history | Users ask follow-ups, and each turn must still stand on its own citations |
| **3. Red-flag review** | cross-source consistency: independent analysis → reconciliation → disputed claims as a review queue | Sources disagree and someone will act on the wrong answer; the deliverable is the queue, not an answer |
| **4. Living knowledge base** | canonical memory + blend retrieval; corrections supersede, staleness resolved by canon | The newest document wins, and the superseded one still has to be explainable |
| **5. Entity-centric intelligence** | alias resolution across sources into one registry | The disagreements hide behind naming — facts that never collide are never checked |
| **6. Comprehensive audit sweeps** | open-ended "find everything" decomposed into planned checklists + coverage audit + verified synthesis | The requirement is a mandate rather than a question, and one big prompt silently misses much of what is there |
| **7. Long-horizon assistant memory** | versioned memory per engagement, promises, pins, drift between epochs | The engagement outlives any conversation and its timeline will be interrogated retrospectively |


### Pattern 1: Ask the corpus

**The shape.** A user asks one question; the application retrieves
clearance-filtered evidence, a model answers from it with citations, and
what reaches the UI is one of two designed states: citations that
resolve, or an honest "the corpus does not establish this." No
conversation, no accumulation. This is the workhorse, the pattern most
corpus applications should build first.

**What it composes.** Prefix layout and clearance design (§15–§16);
`POST /v1/search` with provenance envelopes (§16); either a one-off
session turn or search-plus-your-own-completion (§17); and §17's four
grounding lessons as the client checklist, especially lesson 4, that
insufficiency is a *success* state your UI must design for. Note what it
does not compose: §18 opened with the honest gate that both flagship
results below used **zero** ledger machinery. A well-designed corpus and
a grounded loop carried the entire load.

**Where it lives today.** The single-shot shape is fully server-buildable
now: ingest → collections → clearance-filtered search → your completion,
with the provenance envelope making every answer re-verifiable (§15–§17).
What a fuller form adds is the *loop*: bounded multi-round directive loops
(search, count, fetch) with corrective verification retries. The server's
single-shot turn does not run this machinery
(**[gap]**, proven in §17), and the grading harness behind
every number above is not part of the server. Both halves are client-implementable:
§17's lessons give the verification recipe, and §21 step 8 gives the
grading practice. Budget for them because they produced the last few points
in every score above.

**Start from.**
[runbooks/applications/financial-advisory.yaml](../../runbooks/applications/financial-advisory.yaml)
for a records corpus,
[history-revolution.yaml](../../runbooks/applications/history-revolution.yaml)
for an archival one. Legal-appeal, legal-contracts, patent-analysis,
and customer-support are all committed beside them when your corpus is
closer to dockets, contracts, patents, or tickets. §21 steps 1–5 are this
pattern end to end; step 8 is the answer-key discipline that turns "it
seems to work" into a number.

### Pattern 2: Research chat

**The shape.** The same grounded answering, made conversational: a
session over permitted collections, follow-up questions that lean on
antecedents ("what did *he* say about it?"), history carried and
condensed by the client, every turn's citations still held to the
resolve-or-insufficient contract.

**What it composes.** Sessions that pin a runbook version and snapshot
clearance at create (§17); per-turn retrieval with envelopes (§16–§17);
client-owned conversation state. The server deliberately carries nothing
between turns, so query rewriting and history condensation are yours
(§17 lesson 3); and the render-states table §17 closed with.

**Where it lives today.** Sessions, turns, clearance snapshots, and
per-turn evidence are server-real today (§17 executed them). Everything
conversational is client-side by design: history threading, condensation
with the preservation contract, follow-up rewriting, restatement
prompting. A chat driver owns that loop, and §17 lesson 3 is the port guide.
The verification retries that separate a genuinely grounded transcript from a
plausible-looking one are likewise client work today (**[gap]** per §17).

**Start from.**
[runbooks/applications/regulatory-compliance.yaml](../../runbooks/applications/regulatory-compliance.yaml)
(the first-worked-corpus shape) or
[support-knowledge.yaml](../../runbooks/applications/support-knowledge.yaml)
(the heterogeneous one). §21 steps 1–5 and 7; grade turns, not just
answers. Carry-retention is checkable (§17 lesson 3).

### Pattern 3: Red-flag review

**The shape.** The corpus is interrogated not by a user but by an
extraction pass, source by source; claims meet the ledger; and the
product is the *queue*. It shows every place the corpus disagrees with itself,
each fight carrying both values, both sources, and the rule that caught
it, waiting for a human verdict. Due diligence, compliance review, audit
prep: applications where the disagreements *are* the deliverable.

**What it composes.** Extraction into governed memory, gates, the
disputed-claims query, and the three reviewer verdicts (§18, which built
this pattern's mechanics end to end); prefix/clearance design so the
review team sees the right slice (§16); and §18's three vocabulary rules
for extraction prompts, which the evidence below shows are not optional.

**Where it lives today.** The queue itself is server-real and §18
demonstrated it: gates fire on write, disputed claims persist, the queue
is `facts?statuses=disputed`, corrections supersede through the front
door. What a driver still owns is the *pipeline around* the
queue: the consistency pass that analyzes each source independently
before reconciling (so one source's noise cannot poison another's
extraction), a recall scorer, and the terminology and value-normalization
layers, which are where most of the recall lives.
On the server, your ingestion worker is that orchestrator
and must extract per source, write per source, and persist findings from
write responses (§18's boundary note). Vocabulary discipline lives in
your extraction prompt, where §18's three rules carry most of the gains.

**Start from.**
[runbooks/applications/due-diligence.yaml](../../runbooks/applications/due-diligence.yaml)
uses a compartment layout that serves as the review-team access model. §21 steps 1–4
set up the corpus; step 6 is this pattern's core loop in miniature. Grade
against a seeded answer key before trusting recall: the baseline
numbers above are what "looks fine in the demo" actually measures.

### Pattern 4: Living knowledge base

**The shape.** A knowledge corpus keeps moving. Release notes supersede KB
articles, policies change, and tickets contradict docs. It has an
assistant that answers from *current vetted state first* and documents
second. Canon answers "what is true now"; retrieval answers "show me the
language behind that"; corrections move canon without deleting history.

**What it composes.** Everything §18 built, plus its blend section
specifically: the composed, budget-degraded brief as the prompt's first
surface; document retrieval as the residual second surface; corrections
and supersession as the maintenance verb; pins for "what did the KB say
when the customer was told X."

**Where it lives today.** The ledger, the brief, corrections, pins, and
search are all server-real; §18's blend section showed the two-call
composition and was explicit about the boundary. A blend wants a backend that
indexes the fact block beside the corpus; the server has none. This is a documented design
judgment in §13.4, not an oversight. On the server, the
blend is application-layer prompt assembly (canon first, retrieval
residual — the ordering is the part that matters).

The other half the server lacks is the
**drift engine**: explaining every change between epochs wants a
deterministic cross-lineage drift classifier, and the server has no drift
surface at all. An epoch comparison today is a client
script diffing pinned fact reads across two lineages. That is workable
(pins make it deterministic) but it is yours to write.

**Start from.**
[runbooks/applications/support-knowledge.yaml](../../runbooks/applications/support-knowledge.yaml)
is the starting point. Note its shape,
[runbooks/shapes/knowledge-sources.yaml](../../runbooks/shapes/knowledge-sources.yaml),
carries the dot-free-keys lesson in its comments. §21 steps 6–7, then
§18's blend composition as the turn handler. Keep §18's habit: log
(`version_id`, `as_of_seq`, `content_hash`) beside every model call.

### Pattern 5: Entity-centric intelligence

**The shape.** Sources describe the same actors under different names, such as
threat groups christened per vendor, counterparties abbreviated per
drafter or patients per system. The application's value is the
*registry*: one canonical entity per real-world actor, every alias
attached, facts and findings converging instead of fragmenting.

**What it composes.** Extraction into governed memory with subjects as
entity identifiers (§18); the vocabulary discipline of §18's three rules,
promoted from hygiene to the core product mechanism; clearance design
when different teams may see different actors (§16).

**Where it lives today.** This is the catalog's widest server gap, stated
plainly. A registry wants alias-aware resolution against the
lineage, alias provenance events and manual merge/split tooling, and there is
**no server counterpart at all**. There
is no entity route on either plane, and the kernel crate has no
resolution module. It has only a dormant `Entity` record in its snapshot types
(aliases, `merged_into`) and an `entity_id` slot on claims that no API
yet populates, a landing zone rather than a feature.

What is
server-real is the substrate the registry needs: claims whose subjects
*are* your entity ids, gates that collide facts when subjects match, and
supersession for identity corrections. The difference between actors that
fragment and actors that resolve is precisely the value of the missing half.
On the server today, you
build it client-side: maintain the alias→canonical-subject map in your
application (a declarative alias asset is the shape to copy),
normalize subjects *before* writing claims, and treat an ambiguous alias
as a review-queue item, not a guess.

**Start from.**
[runbooks/applications/threat-intelligence.yaml](../../runbooks/applications/threat-intelligence.yaml).
§21 step 6, with your alias map in front of the claim writes. Seed a
handful of multi-alias actors and one over-merge trap in a test corpus
and score yourself the same way: resolved-to-one is the metric,
over-merges are the disqualifier.

### Pattern 6: Comprehensive audit sweeps

**The shape.** This is not a question but a mandate: *find everything wrong in
this corpus*. The naive form, one open-ended prompt over everything, is
the failure mode; the pattern is decomposition: plan (or structurally
enumerate) targeted sub-questions, run each as a grounded ask, audit the
plan for coverage, and merge under provenance so synthesis cannot quietly
drop what a sub-question found.

**What it composes.** Pattern 1 as the inner loop, many times; corpus
structure as the enumeration skeleton (§15's prefix layout doubles as
one); and a merge contract your synthesis step must obey. Optionally
pattern 3 downstream, when swept findings should become governed claims.

**Where it lives today.** This pattern is an orchestration you write; none of
it is server machinery. The server contributes what makes the orchestration
governable: clearance-filtered search for every sub-question, provenance
envelopes so each sub-answer is re-verifiable, and interaction capture so
the sweep itself is auditable (§20). The two *contracts* are fully
portable today and are the pattern's real content: enumerate coverage
from corpus structure rather than trusting a planner (your prefix layout
is the checklist skeleton), and merge under provenance with strict-JSON
finding ids, every id mapped exactly once, fail closed before rendering.

**Start from.**
[runbooks/applications/sweep-coverage.yaml](../../runbooks/applications/sweep-coverage.yaml)
and [sweep-v2.yaml](../../runbooks/applications/sweep-v2.yaml). §21 steps
1–3, then your sweep runner over `/v1/search`; step 8's grading habit
matters doubly here. Seed elements, score element recall per sweep, and
keep a negative control in the set to price your false-positive tax.

### Pattern 7: Long-horizon assistant memory

**The shape.** An engagement that outlives any conversation: a claim file
worked over months, an advisory relationship over years, a serial
narrative over volumes. Each new document (or chapter, or meeting) is a
*unit*: compose the brief from memory so far, process the unit, gate the
extracted claims, and accept them into a new version. This creates a lineage whose every state
is pinned and whose contradictions were caught at the moment they
entered.

**What it composes.** The full §18 stack, iterated: version lineage
(`GET /v1/versions/{id}/lineage`), the composed budget-degraded brief as
each unit's context, claims/updates/corrections with gates, promises for
obligations that must survive until a later unit pays them off, pins for
retrospective reads. This is the pattern the platform's kernel was
originally built to serve.

**Where it lives today.** The memory substrate is server-complete and §18
demonstrated every piece: lineage, briefs, gates, promises, pins. What
a driver owns is the per-unit
compose→generate→gate→accept loop, checkpointed resume, the repair-task
lifecycle that re-litigates blocked claims, and the drift engine that
compares lineages between epochs. On the server, the unit loop is a loop
your application writes (each iteration is §18's calls in order, with
`expected_head` for write safety), repair is a reviewer workflow over
the disputed queue rather than an automated re-extraction, and drift is
the same client-side pinned-diff noted under pattern 4.

**Start from.**
[runbooks/applications/insurance-claims.yaml](../../runbooks/applications/insurance-claims.yaml)
(document-driven units) or
[financial-advisory.yaml](../../runbooks/applications/financial-advisory.yaml)
(dated units and pinned reads). §21 step 6 is one unit of this loop;
run it N times with the brief recomposed between units and you have the
pattern. Record each unit's (`document date`, `head_seq`) pair as you go
because §18's date→seq bookkeeping makes the retrospective reads
answerable.

### Choosing your pattern

This catalog is also served by a running server — `GET
/v1/authoring/patterns` (or `mmctl author patterns`) returns the seven
patterns with their exemplars and guidance, and `mmctl author
new <name> --pattern <id>` starts a guided draft from one.

Run your requirements past §18's three smells, plus two this catalog
adds. With no smell at all, users ask questions and the corpus is what it
is. In that case,
build pattern 1, and add pattern 2 when they ask follow-ups; the
flagship ask results needed nothing more. **Contradiction matters**
(sources disagree and someone will act on the wrong answer): pattern 3.
If the disagreements hide behind *naming* (same actor, many names),
pattern 5 is contradiction's identity-shaped twin, because facts that
never collide are never checked. **Time matters** and the corpus keeps
moving: pattern 4, which is pattern 3 plus a maintenance discipline and
the blend prompt. **Obligations matter**, or the engagement itself has a
timeline your users will interrogate retrospectively: pattern 7.

If the requirement is not a question at all but a mandate to *find
everything*: pattern 6, whose first lesson is that the one-big-prompt
version of your requirement silently misses much of what is there. The
patterns compose — one corpus can carry ask, chat, consistency and the blend
at once, which is why the support-knowledge runbook is cited from four
subsections above.

Whichever pattern you land on, it ships into an platform: someone must
mint the tokens (§14), answer for who-asked-what, budget the model
spend, and decide what "delete" means. That perimeter includes identity at
the
front door, auditability as a feature, data lifecycle, cost governance,
tenancy, and is the subject of §20.

## 20. Platform integration concerns

§19 closed with an observation. Every pattern you build ships into an
platform, which will ask questions that your pattern choice does not
answer. Who signs the user in, and how does that identity
reach the mesh? When the security team says *kill that credential now*, what
actually happens? What does the compliance dashboard read? What does
"delete" mean in a system whose first invariant is that nothing is ever
deleted? Who pays the model bill, and who is allowed to make it bigger? And
where does one customer's data end and another's begin?

This chapter answers those six questions as the rest of Part II does. It
runs the machinery and reads the responses. Keep two companion documents
open beside it:
[security-posture.md](../security-posture.md), the normative argument for
*why* the trust model is shaped this way (§14 walked its identity chain
end to end), and
[platform-features.md](platform-features.md), the copy-pasteable
operator walkthrough of the same surface. This chapter does not repeat
either. It uses live transcripts to settle three questions that integration
teams often debate: the revocation trade-off, what the audit record contains,
and what removal removes. It then gives you a frame for writing your own
platform story against these primitives.

![The platform ring: Munarium at the centre inside a tenant boundary where every table is keyed by tenant_id, ringed by the five touchpoints an integration owns — identity and capability-token minting, observability, cost governance, data lifecycle, and tenancy](images/ch20-platform-ring.svg)

Every transcript below was executed against v0.1.2: a from-source server on
the alternate ports (18080/19090, per the Introduction's two-world rule)
over the compose Postgres, with a **fresh tenant** so the reports start
empty. The boot is §14's two-role rig. It uses an `rw` token for the operator
plane, a `mgmt` token for the manager plane, and a token secret to arm
minting, with one planned omission you will notice later:

```powershell
$env:MUNARIUM_HTTP_ADDR = '127.0.0.1:18080'
$env:MUNARIUM_GRPC_ADDR = 'disabled'
$env:MUNARIUM_OPS_ADDR  = '127.0.0.1:19090'
$env:MUNARIUM_STORE = 'postgres'
$env:MUNARIUM_DATABASE_URL = 'postgres://munarium:munarium-dev@localhost:5433/munarium'
$env:MUNARIUM_SOURCE_STORE = 'pg'
$env:MUNARIUM_AUTH_MODE = 'static'
$env:MUNARIUM_STATIC_TOKENS = 'ops20:ch20-tenant:rw,mgmt20:ch20-tenant:mgmt'
$env:MUNARIUM_TOKEN_SECRET = 'dev-guide-ch20-secret-32-bytes-min!!'
# MUNARIUM_TOKEN_REVOCATION_CHECK deliberately left at its default (false) — act one below.
cargo run -p munarium-server
```

### Your API manager is the front door

§14 made the argument and demonstrated the chain: the mesh is deliberately
not an identity provider. Your API-management layer authenticates the human
and exchanges its `mgmt` credential for a short-lived capability JWT. That
JWT *reduces* the trust already established by the manager. Section 14 did
not show the small integration service you will build, so here it is:

1. **One endpoint in your gateway** (`POST /mesh-token`, or a policy inline
   in APIM/Apigee/Kong) that fires only for an authenticated session.
2. It maps the user's directory groups to the three policy inputs:
   `access_level` (hierarchical), `compartments` (need-to-know tags), and
   `scopes` (`query`, `ingest`), plus optional `runbook_refs` to pin the
   token to named applications. This mapping table is the *entire* policy
   integration; there is no policy language to learn on the mesh side.
3. It calls `POST /v1/access-tokens` with its `mgmt` static token, gets back
   `{token, jti, expires_at}`, and forwards the JWT as the bearer (plus
   `X-Munarium-Uid`) on the user's data-plane calls. Cache it against the
   user's session keyed by `expires_at`. There is no refresh. When it
   expires, mint another token.

**TTL strategy.** The default lifetime is `MUNARIUM_TOKEN_TTL_SECS` (3600);
the ceiling is a hard-coded 24 hours that silently clamps anything larger
(§14 demonstrated the clamp live). Set the TTL to your manager's own
session length, and prefer a shorter value. Minting uses one cheap local
HS256 signature and an audit insert. A token per session or even per hour
costs little. Short TTLs are the main limit on a stolen token, not just a
useful default. They make the next trade-off acceptable.

**The revocation trade-off, demonstrated.** Revocation is *optional* on
this platform, and that surprises security reviewers, so run the experiment
before the meeting. Mint a token for alice, then revoke it by `jti` using the
mgmt-plane kill switch:

```bash
curl -s -X POST http://127.0.0.1:18080/v1/access-tokens/$JTI/revoke \
  -H "Authorization: Bearer mgmt20" -H "X-Munarium-Uid: api-manager"
```

```json
{ "jti": "tok-019ff44dc3647d11ba0dda4a7c6e8d06",
  "revoked": true,
  "revocation_check_enabled": false }
```

Read the third field carefully: the deny-list entry was written, and the
response is telling you the server is **not checking it**. On this boot
(the planned omission above), the revoked token keeps working. Here it is
opening a session against `advisor-desk`, the one-collection demo runbook
the next section's activity publishes:

```bash
curl -s -o /dev/null -w '%{http_code}\n' \
  -X POST http://127.0.0.1:18080/v1/runbooks/advisor-desk/sessions \
  -H "Authorization: Bearer $JWT"
# -> 200        (revoked, and still opening sessions)
```

Now restart the server with `MUNARIUM_TOKEN_REVOCATION_CHECK=true`. Use the
same secret and Postgres, where the deny-list row remains. Replay the same
call:

```json
{ "type": "https://munarium.ioka.io/problems/token-revoked",
  "title": "access token revoked",
  "status": 401,
  "detail": "the access token has been revoked" }
```

That is the whole trade-off. With the check off, which is the default, JWT
verification uses only CPU and has no hot-path database dependency. A
revoked token dies only at its `exp`, and the deny-list serves as an audit
record.

With the check on, each verification adds one indexed primary-key lookup
against `access_tokens` (`state.rs:434`, `check_revocation`). Revocation then
takes effect at once. The lookup is cheap. The real question is your
security posture.

If your compliance rules require a clear kill switch for events such as a
terminated employee or incident response, turn the check on. Use the
transcript above as evidence. If your TTLs are short and your threat model
accepts "a stolen token dies within the hour," the default keeps
verification self-contained. Record the choice for each environment. The
deployment checklist in
[security-posture.md](../security-posture.md) has a line item for exactly
this decision. Your client code should handle the `token-revoked` slug the
same way as `token-expired` either way: both are 401s whose remedy is
"mint a new token" (the client libraries already map both to their
re-authenticate error kind).

**Key rotation.** The signing secret (`MUNARIUM_TOKEN_SECRET` /
`MUNARIUM_TOKEN_SECRET_FILE`, ≥ 32 bytes) is the one credential the mesh
holds for this scheme. In deployments, it comes from a vault and never a
compose file. Rotating it invalidates every active token at once. Short TTLs
limit the effect because every user will mint again within the hour at
worst.

A dual-secret rotation window, where the old key verifies and the new key
signs, is the M16 hardening item **[gap]**. Until it lands, rotate during a
window that can handle a burst of new token requests.

### Auditability as a feature

Most platforms treat audit as exhaust. Here, it is a product surface. The
middleware captures every `/v1` request in the `interactions` table before
your application does anything. Each row includes the uid and token. The
reports API is the read side used by your governance dashboard.

To show it, this rig created a small morning of activity. The operator,
`dana.lead` on the `rw` token, created a memory version and two claims. The
operator also published a one-collection retrieval runbook,
`advisor-desk@1`. The manager minted alice a query-scoped JWT. Alice opened
a session and asked one question. The dashboard then asks the mgmt plane
three questions.

**Who is using what:** `GET /v1/reports/usage`, grouped by `uid`,
`session`, `runbook`, or `collection`, with RFC 3339 `from`/`to` bounds:

```bash
curl -s "http://127.0.0.1:18080/v1/reports/usage?group_by=uid" \
  -H "Authorization: Bearer mgmt20" -H "X-Munarium-Uid: api-manager"
```

```json
{ "group_by": "uid",
  "rows": [
    { "key": "dana.lead", "interactions": 6, "turns": 0,
      "completion_input_tokens": 0, "completion_output_tokens": 0,
      "avg_latency_ms": 26.83 },
    { "key": "alice@example.com", "interactions": 2, "turns": 1,
      "completion_input_tokens": 0, "completion_output_tokens": 0,
      "avg_latency_ms": 11.0 },
    { "key": "api-manager", "interactions": 1, "turns": 0, … } ] }
```

Every human and service principal that touched the tenant is a row.
operators are users too. `interactions` counts API calls;
`turns` counts session questions; the token columns stay zero until a
completion model is wired (§17), at which point this same query becomes
your per-user model-spend attribution. `group_by=runbook` answers "which
application is busy" (`advisor-desk@1`: 2 interactions, 1 turn), and
`group_by=collection` follows retrieval traffic into specific indexes.

The uid does not have to be a person for this to be useful. An application
with no accounts — one that admits visitors by access code, say — can still
assert a uid on every data-plane call: the admitted code's slug
(`code-<its letters and digits>`), carried in a signed cookie, so the reports
above roll up per code — which code was used, how much, when — usage
tracking with no new storage. A uid that identifies no one works too
(`visitor-<hash of a per-browser nonce>`, rotating at UTC midnight), at the
cost of per-visitor continuity. Capability JWTs are minted per (runbook,
clearance, visitor) with that uid as `sub`, so the server's uid contract
holds (a JWT's `sub` must match the asserted header), and a turn against
a session created under a different uid is refused exactly as the
session-uid mismatch rule says — the application recreates the session once
and retries, the same way it handles `session-not-open`. The payoff is this
same query: `group_by=uid` shows one row per visitor with exact
`completion_input_tokens`/`completion_output_tokens`, which is what a
gated stats page renders (model spend, usage by corpus and visitor,
sessions over seven days, traffic over 24 hours — each section degrading
independently if its report call fails) without any server-side change.
If your own application fronts anonymous or pooled
users, the pattern transfers: mint a stable pseudonymous uid per
principal-you-can-distinguish and the reports do the rest.

**Who asked what:** `GET /v1/reports/audit`, the interaction trail,
newest first, filterable by `uid`, `session_id`, `runbook`, and time:

```json
{ "entries": [
    { "id": "int-019ff44ddc827510b4879f788c776331",
      "uid": "alice@example.com",
      "session_id": "ses-019ff44dd68370019fae51b8d3573e61",
      "request_id": "req-019ff44ddc747eb2a19b4894aaa40fd4",
      "plane": "rest",
      "method": "POST /v1/sessions/ses-019ff44dd683…/turns",
      "runbook_ref": "advisor-desk@1",
      "token_jti": "tok-019ff44dc3647d11ba0dda4a7c6e8d06",
      "status": 200, "latency_ms": 13,
      "created_at": "2026-08-12 04:49:31.268703+00" }, … ] }
```

One row gives the whole chain of custody. It names the human (`uid`),
credential (`token_jti`), action (`method`), application (`runbook_ref`),
and result. Join `token_jti` to the issuance audit in `GET
/v1/access-tokens` to learn who minted it, when, and with what clearance.

By default, rows contain only the envelope. Add `?bodies=true` to return
the captured request and response bodies as a flight recorder. Bodies are
limited by `MUNARIUM_INTERACTION_BODY_MAX`, which defaults to 32 KiB. Larger or
non-JSON bodies are stored as a `{sha256, bytes_len}` summary. The gRPC
plane records envelopes only, so REST is the full-body audit surface.

Under overload, capture is best-effort by design. Records travel through a
bounded channel to one writer task. When the channel fills, the system
drops a record with a warning instead of slowing the data plane.

One body deserves a close look because it proves a security property you
should test in your environment. The manager's token mint is itself a
captured interaction, and its response held a live credential. Request it
with bodies:

```bash
curl -s "http://127.0.0.1:18080/v1/reports/audit?uid=api-manager&bodies=true" \
  -H "Authorization: Bearer mgmt20" -H "X-Munarium-Uid: api-manager"
```

```json
{ "id": "int-019ff44dc36773309e916adee81aea23",
  "method": "POST /v1/access-tokens",
  "request": { "uid": "alice@example.com", "access_level": 0,
               "scopes": ["query"], "runbook_refs": ["advisor-desk"],
               "ttl_secs": 900 },
  "response": { "redacted": "response contains secret material; not stored" }, … }
```

The request records who asked for a token and with what claims. The
response carried the signed JWT, so the stored response is a redaction
marker. The mint handler marks its response as secret-bearing, and the
capture middleware stores the marker instead.

This upholds the rule that token material is *never* persisted. It does not
appear in `access_tokens`, which stores only jti and claims. It also does
not appear in the audit trail or logs. Your compliance reviewer gets the
audit trail without turning it into a credential store.

**What the models cost:** `GET /v1/reports/cost` is the third view:
completion token spend rolled up per resolved provider/model, with an
`overridden_turns` column splitting native from caller-overridden traffic
(the server reports token facts; your platform converts them to dollars).

This route also supports the Preface's first commitment. During the draft's
verification pass, it returned `500 storage-error` on Postgres. The rollup
SQL ordered by its output aliases with `ORDER BY itok + otok`. Postgres
resolves aliases only as bare column references, not within an expression.
This shows that the route had not run against Postgres before this book
called it.

The fix repeats the aggregate expressions in `ORDER BY`
(reports_api.rs:325-329). A comment records the rule so the next rollup does
not learn it again. The corrected route was checked live on a fresh
Postgres-backed tenant. It returned `200` with an empty `rows` array, which
fills as §17's completion turns arrive. The incident appears in §13.5's
closed ledger.

**Correlation into your own observability.** Every response carries an
`x-munarium-request-id` header, and the same id is stored on the interaction
row:

```bash
curl -s -D - -o /dev/null "http://127.0.0.1:18080/v1/versions/$V/facts" \
  -H "Authorization: Bearer ops20" -H "X-Munarium-Uid: dana.lead" | grep -i request-id
# x-munarium-request-id: req-019ff44fe13b7832a639985ffb0cfb91
#   …and the audit row for that exact request:
# { "request_id": "req-019ff44fe13b7832a639985ffb0cfb91", "uid": "dana.lead",
#   "method": "GET /v1/versions/memv-abcf…/facts", "status": 200, "latency_ms": 9 }
```

Log that header in your gateway's access log and attach it to your APM
spans. Every trace in your observability stack can then join to a
uid-attributed row in the mesh audit trail. The server's tracing spans use
the same id. When the March question arrives, the join is already built.

Carry two retention notes into your design review. First, the audit trail
lives in the same tenant-scoped database as everything else. Your Postgres
backup and access controls cover it; it is not a separate logging pipeline.
Second, the retention/TTL policy for interaction rows is yours to define
**[gap]** under the M14 governance item. Until then, the table grows without
a limit. Put a DBA-owned archive policy for `interactions` in the same
change-control document as the deletion runbook below.

### Data lifecycle: what "delete" means here

The platform's first invariant says append-only and nothing deleted. That
rule conflicts with platform data-retention policies, so this section is
clear about what each lever does. There are exactly four, in order of
effect:

**1. Soft removal (API, double-pass).** Retiring an application is a
two-step contract, executed here against the demo runbook:

```bash
curl -s -X POST "http://127.0.0.1:18080/v1/runbooks/advisor-desk@1/remove-request" \
  -H "Authorization: Bearer ops20" -H "X-Munarium-Uid: dana.lead"
# -> { "runbook_ref": "advisor-desk@1",
#      "removal_id": "rm-019ff44f83d0718299f22fec215dca2d",
#      "expires_at": "2026-08-12T05:06:19Z" }              (15-minute window)

curl -s -X POST "http://127.0.0.1:18080/v1/runbooks/advisor-desk@1/remove-confirm" \
  -H "Authorization: Bearer ops20" -H "X-Munarium-Uid: dana.lead" \
  -H "Content-Type: application/json" \
  -d '{ "removal_id": "rm-019ff44f83d0718299f22fec215dca2d" }'
# -> { "runbook_ref": "advisor-desk@1", "status": "removed" }
```

The request targets an **exact** `name@version`. The server rejects a bare
name because it would silently remove the latest version. The confirmation
must include the matching `removal_id` within the TTL, or it returns
`409 removal-not-confirmed`.

Reapplying the yaml before confirmation clears the pending `removal_id`.
A removal approved for old content cannot remove fresh content. Sessions
against the removed ref now fail with a `410` whose detail states the
contract:

```json
{ "type": "https://munarium.ioka.io/problems/runbook-removed",
  "title": "runbook removed",
  "status": 410,
  "detail": "runbook 'advisor-desk@1' has been removed (soft; its data is retained)" }
```

The default listing also omits it. Removal does *not* erase the data.
`GET /v1/runbooks?include_removed=true` still returns the runbook, status
`removed`, its collection intact; the yaml, run history, collections,
index data, sessions, and audit trail are all retained, and a removed ref
can never be resurrected by re-apply (publish a new version instead).
**Access closed; evidence kept.** Render the 410 in your UX as "this
assistant has been retired," not as an error.

**2. `retireOld` (API, storage reclaim).** The one deletion any API
performs: the runbook step reclaims chunk rows for *inactive* index
versions beyond `keep_versions`. Manifests stay resolvable, so past
provenance envelopes keep verifying. This is disk cleanup, not data
lifecycle.

**3. Physical deletion (DBA, documented, manual).** No API deletes a
collection or its active index data. Neither `rw` nor `mgmt` can do it on
either transport. The queries cannot express it. The
only sanctioned path is
[index-deletion-runbook.md](../ops/index-deletion-runbook.md): a
change-controlled PostgreSQL operation that detaches and drops the
collection's partition (each collection owns one LIST partition of
`collection_chunks`, so the drop is O(1) catalog work), while deliberately
keeping the rows that provenance needs: the collection row, index-version
manifests, source rows, sessions, interactions, ledger events.

**4. The blobs.** Document bytes are not in Postgres (unless
`MUNARIUM_SOURCE_STORE=pg`); they live in your object store at
`{tenant}/{filename}`. The partition drop does not touch them. The DBA
runbook's final section covers the per-backend purge (`az storage blob
delete-batch`, `aws s3 rm`, …) with two cautions that belong in your
change ticket verbatim: sources are shared across collections (deleting
bytes silently breaks every future rebuild that binds them), and prefix
matching is literal `starts_with`. Scope with the trailing slash.

Write your retention story *against* these primitives instead of around
them. "Decommission the assistant" uses lever 1. Access closes today, while
evidence survives the postmortem. "Reclaim storage" uses lever 2, which is
already in your runbook steps. "Legal purge," such as right-to-erasure or a
court order, uses levers 3 and 4 under change control.

The runbook's deliberately-kept table is your clear statement of what
remains: content-free metadata such as ids, hashes, and audit rows. The
platform will not provide a `DELETE` endpoint that quietly makes history
impossible to audit. If a regulator asks what the assistant knew in March,
"we deleted that" is the exact answer that append-only design prevents you
from giving by accident.

### Cost governance

Model spend on this platform is structurally *your* spend. BYOK means
every completion bills to your provider accounts, with your keys resolved
at call time from your vault (§14). Governance is therefore about who may
spend and how fast, and the levers live server-side where clients cannot
argue with them:

- **Rate/throughput budgets** ride the provider config: `budgets: {rpm:
  300, tpm: 200000}` in the applied ProviderConfig yaml is enforced as a
  fixed 60-second-window counter (the code's comments say "token bucket";
  the window resets whole rather than refilling), checked before every
  call. Exhaustion is a typed rate-limit error, not a surprise invoice.
  Budgets are per provider config, and provider configs are per tenant,
  so a noisy tenant throttles itself.
- **Tier routing** keeps model names out of application code: a runbook's
  `models:` block sets per-task defaults (`completion`, `validation`,
  `embedding`) as `tier: fast|capable|frontier`, resolved through the
  provider config's tier map or the built-in per-family table. Route
  drafting to `fast`, adjudication to `capable` (or, where the evals
  earned it, `frontier`), and change the economics by editing yaml, not
  redeploying clients.
- **`allowOverrides` defaults closed.** A caller may request a different
  provider/model on a turn *only* if the runbook's
  `models.allowOverrides` permits it. Omitted means `false`, and §17
  demonstrated the `403 override-not-allowed` a closed policy returns. Who
  may pick the expensive model is decided in the runbook. This artifact is
  owned by the operator, versioned, and audited. The client never negotiates
  this choice.
- **Attribution closes the loop.** Every turn records the provider and
  model that actually served it and whether it was an override; the usage
  report splits token spend by uid/runbook/collection and the cost report
  by provider/model with the override split. §17's
  per-turn cost readout is the same data at single-conversation
  granularity, and since 2026-08-23 it is two-sided: `GET /v1/providers`
  discloses *before* the call which concrete model each allowed family's
  tier resolves to (free, cacheable — a UI can mirror it behind a short
  TTL), and the turn's `completion` block reports *after* the call
  which model answered and what it cost, both calls summed when the
  truncation retry fired. A UI that shows both has nothing to hide about
  model choice; the habit of recording provider and model on every
  result row arrives at the application surface.
- **Ceilings are not budgets.** The turn path's 2,048-token completion
  ceiling (1,024 until 2026-09-02; since then a setting — `MUNARIUM_MAX_TOKENS_*`
  on the container or `POST /v1/max-tokens` per tenant, docs/tokenbudgets.md)
  bounds the worst case of one call; it does not cap a
  conversation, a user, or a day. Reasoning-model tiers can spend that
  ceiling on hidden reasoning and trigger the one 4× retry (§17), so the
  worst case per turn is five ceilings' worth of output tokens before
  verification, and each opt-in corrective retry (at most two) can add
  four more. Per-uid and per-tenant spend limits remain your gateway's
  job, and the usage report is the meter it should read.

### Multi-tenancy postures

For an application team, a **tenant** is the unit of *structural*
isolation: your static token maps to exactly one tenant, every table is
keyed by `tenant_id`, and storage handles are pre-scoped before any query
runs. Design around the rule from [rest.md](../api/rest.md): **there is no
cross-tenant read path outside the Admin API**. At v0.1.2, that API is
reserved and not implemented. Today, there is no cross-tenant read path at
all.

This is not a WHERE clause rule that you must audit. It is the shape of the
storage layer, as §4 explains. A capability JWT inherits the tenant from
the `mgmt` token that minted it. A manager cannot mint across tenants.

The decision you own is granularity, and the test is simple: **a tenant
boundary is where sharing must be impossible; compartments are where
sharing is need-to-know.** Per-customer tenancy is the clear choice for a
customer-facing system. One customer's documents, sessions, audit trail,
and reports must never appear beside another's. Per-tenant provider configs
also separate model spending.

Per-department tenancy applies the same logic within one platform, such
as legal, M&A, and engineering. Use separate tenants when the *audit* must
be separate. Usage and audit reports cover the whole tenant for the mgmt
role. Anyone who can read the report sees every uid in that tenant.

If users should share a corpus but see different parts, do not use separate
tenants. Use levels and compartments within one tenant (§16). One runbook
can then serve every clearance.

In either design, the boundary has practical effects. Your API manager
holds one `mgmt` token per tenant. Ingest prefixes and reports never
collide. "Delete customer X" becomes a tenant-scoped operation with a clear
blast radius. For scale, the documented posture
([architecture.md](../architecture.md)) is **database-per-tenant cells**
using CNPG-operated Postgres. Each cell has one primary plus replicas.
You scale by adding cells, with no state spanning them. The logical
isolation boundary is also the sharding model. A shared database is the
one-cell case used by your dev rig.

### Client libraries vs raw REST

The honest client-coverage story, so you can plan the integration without
discovering it mid-sprint. The four official libraries
([clients/](../../../clients/README.md), for Rust, Python,.NET, and Java
over REST and gRPC) cover the **kernel planes**: `commands`, `query`, `ingest`
(sources), `retrieval`, `runbooks` (v1 runs/approvals), and `providers`.
They include the invariants as ergonomics: disputed-is-success as a status
check, head-conflict retry loops, one-pin reads, provenance envelopes as
required members, fail-closed decoding of governance enums. They are
conformance-proven against the same scenarios the server proves itself
with, and they already send the uid contract and map every platform problem
slug to typed error kinds.

What they do **not** yet wrap is most of what this chapter just used: the
platform application surface. This includes sessions/turns, `POST
/v1/ingest(+/batch)`, the runbook v2 collections surface, access-tokens,
and reports. The server serves both transports for it since 2026-08-18
(§13.5 entry 9) — with two REST-only exceptions added 2026-08-23, the
streaming turn (`POST /v1/sessions/{id}/turns/stream`) and the provider
listing (`GET /v1/providers`). When this passage was written the official
clients wrapped neither transport of it; **since 2026-08-24
all four clients carry the full surface**, and the 2026-09-02 token-budget
pair (`GET`/`POST /v1/max-tokens`) joined them the day it shipped, so the
thin layer below is now a choice, not a necessity. It remains a sound
architecture for an application team: a **thin typed layer over REST**
inside your own codebase for the platform surface. It has five small
planes, and every route is in [rest.md](../api/rest.md) and the generated
`openapi.json` (93 paths at 1.0), so code
generation is an option. Use this layer beside the official client for
kernel work. One such thin layer, written in C# with zero NuGet packages
for a demo front end, contained exactly this: token minting per (runbook,
clearance, uid), session create-or-recreate on the two refusal slugs, unary
and streamed turns, the providers listing behind a TTL cache, and the
reports API behind a gated page — a worked answer to "what does the thin
layer contain."


One rule makes the seam easy: **key every error branch on the problem slug,
never message text.**
`token-expired` and `token-revoked` → re-mint; `override-not-allowed` and
`runbook-removed` → policy states with human wording (§17's table);
`removal-not-confirmed` → re-request. Slugs are the stable contract on
both transports; wording is allowed to improve. Code written that way
survives both server upgrades and the day your thin layer retires into
the library.

### Where this leaves you

The perimeter is now assembled. Your API manager authenticates people and
mints short-lived capability tokens with the least privilege. If you enable
the check, you can revoke them at once. You watched both modes work.

Every call enters an audit trail tied to its uid and jti. Your dashboard
reads it through three report views. Secret-bearing responses are redacted,
and a request-id header joins mesh evidence to your observability system.

Retirement closes access while keeping evidence. Physical deletion exists
in one documented place under DBA change control. Model spending uses your
accounts behind budgets, tiers, and an override policy that is closed by
default. Tenants provide structural isolation and a documented path to a
database per tenant.

Your code uses official clients where they reach. It uses a slug-keyed REST
layer where they do not. Every part of this summary ran against v0.1.2 on a
fresh tenant.

What remains is one end-to-end build: load the corpus, mint tokens, show
clearances, ground sessions, dispute conflicts, and read costs back. You can
run the full build on a clean clone. That is §21,
the worked example this Part has been pointing at since §15.

### Spend governance: daily caps and per-call ceilings (2026-09-01/02)

Two mechanisms bound what a tenant can spend on model calls, and they answer
different questions. The **daily token caps** bound a day; the **per-call
ceilings** bound one call. Both are the server's own, independent of the
gateway's rate limits.

**Daily caps.** A provider config declares `spec.budgets.dailyTokens`
per tier — `{ fast, capable, frontier }`, input + output combined, UTC day;
absent = unlimited, the house rule that an undecided policy defaults to off.
Enforcement is in `op_complete`, the single completion funnel, as a
**reserve → work → settle-or-release** ledger ported from Matrix
(`munarium-core::budget`, memory and Postgres stores, migration 0029,
`pg_advisory_xact_lock` on the `(tenant, config, tier)` scope, because a
ceiling checked without a reservation is not atomic under READ COMMITTED —
Matrix measured ten concurrent requests for 2 units against a ceiling of 10
granting six). The reserve is the same estimate the rpm/tpm bucket uses,
`prompt/4 + max_tokens`, settled to the provider's actual counts after; a
lost settle leaves the budget *spent*, never free, and a janitor stamps
held reservations older than six hours settled at their estimate. An
exhausted scope answers **429 `daily-cap-reached`** with `Retry-After` =
seconds to midnight UTC (every 429 now carries `Retry-After`; an upstream
provider 429 surfaces as `rate-limited`, no longer flattened to 502).
`/healthai` is deliberately exempt and an explicit-model request names no
tier and passes uncapped. The operator's view is `GET /v1/reports/budgets`
(mgmt), which reads usage through the enforcer's own window expression so
the report and the refusal cannot disagree about which day it is; a
configured cap with no traffic yet still gets a zero row. On a fresh tenant
with no provider configs it is honestly empty:

```text
$ curl -s http://127.0.0.1:18080/v1/reports/budgets -H "Authorization: Bearer guide-mgmt" -H "X-Munarium-Uid: api-manager"
{"rows":[]}
```

The example provider configs under `runbooks/providers/` carry placeholder
caps of 1M / 500K / 250K tokens per day for fast / capable / frontier,
sized to be replaced. Size the frontier tier from its measured per-turn
cost: on a hard multi-document question a frontier model's turns measured
roughly three times a capable model's tokens, so the same ceiling buys a
third of the questions.

**Per-call ceilings.** Every paid call carries a `max_tokens` ceiling, and
since 2026-09-02 the eight of them are one object, `MaxTokensBudgets`
(§17 step 4 names the turn's; [docs/tokenbudgets.md](../tokenbudgets.md) is
the reference). They are read with `GET /v1/max-tokens` (any role) and
replaced **as a whole** with `POST /v1/max-tokens` (rw). Precedence per
call: a runbook's own `completion.maxTokens` /
`modelQueryExpansion.maxTokens` > the tenant's replacement > the container's
`MUNARIUM_MAX_TOKENS_*` > the built-ins. The transcript below ran on the
same throwaway server as §8A's, started with
`MUNARIUM_MAX_TOKENS_HEALTHAI_PROBE=640` so the environment layer is
visible:

```text
$ curl -s http://127.0.0.1:18080/v1/max-tokens -H "Authorization: Bearer guide-mgmt" -H "X-Munarium-Uid: api-manager"
{"turn_completion":2048,"query_expansion":256,"complete_default":1024,"healthai_probe":640,
 "hierarchy_classifier":32,"hierarchy_intent":480,"runbook_advisory":2048,"authoring_assist":8192,
 "source":"environment"}

$ curl -s -X POST http://127.0.0.1:18080/v1/max-tokens -H "Authorization: Bearer guide-rw" -H "X-Munarium-Uid: casey.ops" \
    -H "Content-Type: application/json" \
    -d '{"turn_completion":4096,"query_expansion":256,"complete_default":1024,"healthai_probe":640,
         "hierarchy_classifier":32,"hierarchy_intent":480,"runbook_advisory":2048,"authoring_assist":8192}'
{"turn_completion":4096,"query_expansion":256,"complete_default":1024,"healthai_probe":640,
 "hierarchy_classifier":32,"hierarchy_intent":480,"runbook_advisory":2048,"authoring_assist":8192,
 "source":"tenant","updated_at":"2026-09-02T23:37:28.750326Z"}
```

There is no partial update by construction — the wire type has eight
required fields, so a body missing one is refused before anything is
written, and a value outside its range names the field:

```text
$ …the same POST without "authoring_assist"
{"type":"https://munarium.ioka.io/problems/invalid-input","title":"invalid input","status":400,
 "detail":"invalid input: Failed to deserialize the JSON body into the target type: missing field `authoring_assist` at line 1 column 164"}

$ …the same POST with "query_expansion":9999
{"type":"https://munarium.ioka.io/problems/invalid-input","title":"invalid input","status":400,
 "detail":"invalid input: query_expansion must be in 32..=512, got 9999"}

$ …the full POST with the mgmt bearer
{"type":"https://munarium.ioka.io/problems/forbidden","title":"forbidden","status":403,
 "detail":"forbidden: role 'mgmt' cannot execute commands (rw required)"}
```

On Postgres the replacement is one row per tenant (migration 0031) shared
by every replica; each replica caches it and re-reads after
`MUNARIUM_REGISTRY_TTL_SECS`, the same convergence promise provider configs
make. A ceiling is not spend — a non-reasoning model answering in 140
tokens is billed 140 — but the daily-cap *reservation* estimates against
it, so oversizing inflates transient holds. The reason the numbers exist at
all is §17 step 5's reasoning-model trap: a model that spends the whole
ceiling on hidden reasoning answers with nothing, and 2,048 with the 4×
retry is what covered `z-ai/glm-5.3`'s measured ~5k hidden tokens. All four
clients carry the pair on their providers plane.

## 21. Worked example (tutorial spine): ask + chat over a data room

Every chapter of Part II ended by pointing here, and here is what "here"
means: the whole of Part II, run once, in order, against the real corpus,
with nothing elided. §15 built the miniature and promised the full-scale
load; §16 designed the clearances; §17 owned the conversation; §18 turned
contradiction into a queue; §19 named the pattern this application
instantiates: *ask + chat over a data room*, the catalog's rows 1 and 2.
Section 20 assembled the perimeter. This chapter puts them to use.

It is a tutorial in the strict sense because you can type it. It is also a
real demonstration. **Everything below ran against v0.1.2 on 2026-08-12,
and every id, count, score, and timing is a capture, not an illustration.**
The transcripts are brief because you will run them again. The guide's job
is to prove them first.

The corpus is the one Part II has cited since §15: the **Northgate data
room**, a synthetic M&A data room of 613 markdown documents (~0.4 MB) with
a hand-verified answer key (`seeded_findings.json`, 42 findings) that never
enters any index. The corpus itself is not part of this repository;
[loading-corpora.md](loading-corpora.md) describes the corpus behind each
sample runbook. Everything else you need is on a fresh clone: the shape
([runbooks/shapes/dataroom-documents.yaml](../../runbooks/shapes/dataroom-documents.yaml)),
the runbook application
([runbooks/applications/due-diligence.yaml](../../runbooks/applications/due-diligence.yaml)),
and the compose Postgres from Part I §2. The walk reads the same against
any data-room-shaped corpus of your own; the captured numbers are what one
run over Northgate produced.

![The tutorial in eight steps: stand up with tokens and a fresh tenant, load the corpus, apply a shape and runbook and run it through its human approvals, compare two clearances on the same question, take chat turns with client-side rewriting, work a ledger conflict through the review queue to a correction and a pin, run a completion under your own provider key, and grade against an answer key held in CI](images/ch21-tutorial-map.svg)

### Step 1: Stand up, and the three credentials

The rig is §20's two-role boot with one addition: the BYOK secret, enabled
now so step 7 needs no restart. The tenant is fresh (`ch21-da33`, never
used before this run, so every id below was created by the calls shown, per
§15's fresh-tenant rule). The ports are the Introduction's
two-world alternates, and the source store is `pg` because at 0.4 MB the
Tier-2 rule applies: Postgres-backed bytes are the zero-setup choice, and
a real corpus points at blob storage instead
([loading-corpora.md](loading-corpora.md), [source-stores.md](source-stores.md)).

```powershell
$env:MUNARIUM_HTTP_ADDR = '127.0.0.1:18080'
$env:MUNARIUM_GRPC_ADDR = 'disabled'
$env:MUNARIUM_OPS_ADDR  = '127.0.0.1:19090'
$env:MUNARIUM_STORE = 'postgres'
$env:MUNARIUM_DATABASE_URL = 'postgres://munarium:munarium-dev@localhost:5433/munarium'
$env:MUNARIUM_SOURCE_STORE = 'pg'
$env:MUNARIUM_AUTH_MODE = 'static'
$env:MUNARIUM_STATIC_TOKENS = 'ops21:ch21-da33:rw,mgmt21:ch21-da33:mgmt'
$env:MUNARIUM_TOKEN_SECRET = 'dev-guide-ch21-secret-32-bytes-min!!'
# BYOK, armed for step 7. Resolve the key from wherever you keep secrets —
# the value never appears in a transcript, a config file, or this book.
$env:MUNARIUM_SECRET_ANTHROPIC = Read-Host -AsSecureString 'Anthropic key' | ConvertFrom-SecureString -AsPlainText
cargo run -p munarium-server
```

Three credentials have three jobs, just as §14 taught. `ops21` (`rw`) runs
the operator plane and cannot mint. `mgmt21` (`mgmt`) mints capability
tokens and reads reports but cannot write the ledger. The token secret
enables `POST /v1/access-tokens`.

The first contract every client meets is shown once and omitted later.
Every `/v1` request must carry a uid, or it returns:

```json
{ "type": "https://munarium.ioka.io/problems/uid-required",
  "title": "uid required", "status": 400,
  "detail": "X-Munarium-Uid header (REST) / munarium-uid metadata (gRPC) is required on every /v1 request" }
```

### Step 2: Load the data room: 613 documents in five calls

§15's one rule governs this entire step: **a document's filename is its
identity, its blob path, and the string collections bind.** The corpus tree
matches the runbook's upload format. Everything goes under `northgate/`,
with one subfolder for each functional area. The loop below is the
whole loader. PowerShell, batches of 150 (the batch route takes up to
500), skipping any index or manifest files that sit beside the
`.md` sources (they are tooling, not corpus):

```powershell
$SRC = 'C:\corpora\dataroom'
$H = @{ Authorization = 'Bearer ops21'; 'X-Munarium-Uid' = 'casey.ops' }
$files = Get-ChildItem $SRC -Recurse -Filter *.md
$sw = [Diagnostics.Stopwatch]::StartNew()
$new = 0; $batches = 0
for ($i = 0; $i -lt $files.Count; $i += 150) {
  $batch = foreach ($f in $files[$i..([Math]::Min($i+149, $files.Count-1))]) {
    $rel = $f.FullName.Substring($SRC.Length+1) -replace '\\','/'
    @{ filename = "northgate/$rel"; media_type = 'text/markdown'
       content_base64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes($f.FullName)) } }
  $r = Invoke-RestMethod -Method Post -Uri http://127.0.0.1:18080/v1/ingest/batch `
        -Headers $H -ContentType 'application/json' -Body (@{ files = $batch } | ConvertTo-Json -Depth 4)
  $new += @($r.results | Where-Object { -not $_.existed }).Count
  $batches++
}
$sw.Stop()
"files=$($files.Count) batches=$batches new=$new elapsed=$([math]::Round($sw.Elapsed.TotalSeconds,1))s"
```

```text
files=613 batches=5 new=613 elapsed=3.3s
```

The whole data room took three and a third seconds. Every result row
returned `"bound_to": []`, the honest empty list from §15. No runbook
exists yet, and binding is always *derived* from configuration, not
bookkeeping. Replay any file to prove ingest is idempotent. It returns the
same `source_id` and `"existed": true`, with no new row or bytes:

```json
{ "filename": "northgate/09_privacy_security/ng-dd-0478_security_incident_report__record_01.md",
  "source_id": "src-43855c4ca52ef32d",
  "sha256": "14e02c3d32ae67aa9f37479b6d788662e670d5d5270772915a0cde94d46b4b8f",
  "existed": true, "bound_to": [] }
```

(Keep that `source_id` in mind. Its document carries this
chapter's planted conflict, and its `sha256` is about to become part of an
audit trail.) The operator then checks: *did it land, and where?*

```json
{ "source_id": "src-43855c4ca52ef32d",
  "filename": "northgate/09_privacy_security/ng-dd-0478_security_incident_report__record_01.md",
  "media_type": "text/markdown",
  "content_hash": "14e02c3d32ae67aa9f37479b6d788662e670d5d5270772915a0cde94d46b4b8f",
  "bytes_len": 679,
  "storage_backend": "pg",
  "blob_uri": "pg://source_blobs/ch21-da33/northgate/09_privacy_security/ng-dd-0478_security_incident_report__record_01.md",
  "created_at": "2026-08-12 05:06:32.390085+00" }
```

Backend named, tenant and logical path both inside the credential-free
`blob_uri`. And note what did **not** get uploaded: `seeded_findings.json`
stayed in `inputs/`, outside the corpus tree, per the rule every committed
runbook header repeats. The answer key belongs to the grader (step 8),
and a key inside the index is not a measurement.

### Step 3: Shape, runbook, run: thirteen approvals

Both YAML artifacts are committed and applied verbatim. This is the
"configuration, not code" claim of §15 with every dial turned. The shape
first, then validate-and-apply the runbook (the validator is free and
deterministic; CI runs the same check on every committed sample):

```powershell
Invoke-RestMethod -Method Post -Uri http://127.0.0.1:18080/v1/shapes -Headers $H `
  -Body (Get-Content ..\server\runbooks\shapes\dataroom-documents.yaml -Raw)
# -> { "shape_ref": "dataroom-documents@1",
#      "yaml_hash": "d95e7cce202321dc9b4bc300e3d277d1bdd79c4a630bb84178d4b8e5c9ea8de4" }

Invoke-RestMethod -Method Post -Uri http://127.0.0.1:18080/v1/runbooks/validate -Headers $H `
  -Body (Get-Content..\server\runbooks\applications\due-diligence.yaml -Raw)
# -> { "valid": true, "findings": [], "suggestions": [] }

Invoke-RestMethod -Method Post -Uri http://127.0.0.1:18080/v1/runbooks -Headers $H `
  -Body (Get-Content..\server\runbooks\applications\due-diligence.yaml -Raw)
# -> { "runbook_ref": "due-diligence@1" }
```

The apply sends all 613 existing sources through the thirteen collection
matchers, as promised by §15's retroactive claim. The following run
therefore resolves a fully bound world.

A run executes the five-step pipeline once for each collection. That means
thirteen collections, sixty-five steps, and thirteen separate approval
gates. Section 15's rule says that **no index serves traffic because a build
succeeded**. A person must approve cutover thirteen times.

In production, each approval is a decision with an owner. On this tutorial
rig, the loop approves whatever is waiting. The run record makes that easy
choice visible because each approval records the approving uid. Section
20's audit chapter reads those records back:

```powershell
$run = Invoke-RestMethod -Method Post -Uri http://127.0.0.1:18080/v1/runbooks/due-diligence/runs -Headers $H
# -> { "run_id": "run-019ff45e029978b1a4cf18cb78faf7dc", "state": "awaiting_approval" }
$sw = [Diagnostics.Stopwatch]::StartNew()
$approvals = 0
while ($true) {
  $st = Invoke-RestMethod -Uri "http://127.0.0.1:18080/v1/runs/$($run.run_id)" -Headers $H
  if ($st.state -ne 'awaiting_approval') { break }
  $step = $st.steps | Where-Object { $_.state -eq 'awaiting_approval' } | Select-Object -First 1
  Invoke-RestMethod -Method Post -Headers $H `
    -Uri "http://127.0.0.1:18080/v1/runs/$($run.run_id)/steps/$($step.ordinal)/approve" | Out-Null
  $approvals++
}
$sw.Stop()
"state=$($st.state) approvals=$approvals total=$([math]::Round($sw.Elapsed.TotalSeconds,1))s"
```

```text
state=done approvals=13 total=4.2s
```

The run took 4.2 seconds from `POST …/runs` to `done`, including all
thirteen builds. The local hash embedder explains the speed. A BYOK
embedder would use real time and money, and the envelope below would name
it. The step details now show at scale the story §15 taught you to read:

```text
resolveSources:*   13 steps, 613 sources total  (the third matcher trigger)
buildIndex:*       13 steps — idx-2d9a50863e656f8e (dataroom-index),
                   idx-aa63d11dae606c73 (corporate), idx-b6675dd4f22f7ce2 (equity),
                   idx-67a23c01dfc11f28 (finance), … each watermark: 0
verify:*           13 steps, 614 chunks total    (613 docs; the index doc spans two)
cutover:*          13 approvals, each recorded with uid casey.ops
retireOld:*        13 steps, nothing to retire on a first build
```

The payoff is one scoped search before the sessions begin. It uses
the question this data room exists to answer badly until someone reads
carefully:

```powershell
POST /v1/search { "query": "What is the outstanding principal on the Granite Bank term loan?",
                  "top_k": 3, "filter": { "collections": ["dataroom-finance"] } }
```

```json
{ "hits": [
    { "source_path": "northgate/03_finance/ng-dd-0096_debt_schedule__period_06.md", "score": 0.0164, "…": "…" },
    { "source_path": "northgate/03_finance/ng-dd-0113_budget_variance__period_23.md", "score": 0.0161, "…": "…" },
    { "source_path": "northgate/03_finance/ng-dd-0166_accounts_receivable_aging__period_76.md", "score": 0.0159, "…": "…" } ],
  "envelope": {
    "source_content_hashes": ["08f89e403549fcba…", "2ddaa3090ef26db8…", "8676484411a5afdb…"],
    "index_version": "idx-67a23c01dfc11f28",
    "event_watermark": 0,
    "provider_fingerprint": "local/local-hash@1/256", "…": "…" } }
```

The debt schedule, top-ranked, wearing the envelope §15 dissected:
`index_version` is the exact id `buildIndex:dataroom-finance` reported and
a recorded human approved; the hashes are ingest's `sha256` values; the
watermark says no memory lineage was attached at build time; step 6 creates
one.

At 613 documents, the top score is a single-leg 0.0164 rather than the
miniature's double-signal 0.0328. The lexical and vector legs disagree more
often in a real corpus. RRF fusion exists for that reason, as §16's
mechanics section explains.

### Step 4: Two clearances, one question

The runbook's thirteen collections encode the data room's real governance
boundaries (§15's dissection printed the table): index at level 0; the
operational areas at level 1; equity, finance, and tax at level 2 in the
`finance` compartment; IP and legal-compliance at level 2 in `legal`;
employment and privacy-security at level 3 behind `hr` and `security`.
The `mgmt` plane mints two tokens against that table: one for a
first-day analyst and deal counsel on the finance workstream:

```powershell
$M = @{ Authorization = 'Bearer mgmt21'; 'X-Munarium-Uid' = 'api-manager' }
POST /v1/access-tokens { "uid": "priya.analyst@asterpeak.com",
                         "access_level": 0, "scopes": ["query"], "ttl_secs": 3600 }
# -> jti tok-019ff45e7ff57f0392e808f436a35d29
POST /v1/access-tokens { "uid": "marco.counsel@asterpeak.com",
                         "access_level": 2, "compartments": ["finance"],
                         "scopes": ["query"], "ttl_secs": 3600 }
# -> jti tok-019ff45e800973008d83ca65e30b68a8
```

Each opens a session against the same runbook. Under §16's snapshot rule,
the sessions already disagree about what exists:

```json
{ "session_id": "ses-019ff45e80247be1a1632ccbbb7a154b",
  "runbook_ref": "due-diligence@1",
  "permitted_collections": ["dataroom-index"] }

{ "session_id": "ses-019ff45e803573609c3953edfc1adbd5",
  "runbook_ref": "due-diligence@1",
  "permitted_collections": ["dataroom-index", "dataroom-corporate", "dataroom-equity",
    "dataroom-finance", "dataroom-tax", "dataroom-commercial", "dataroom-real-estate",
    "dataroom-insurance", "dataroom-operations"] }
```

One collection stands against nine. Marco's level 2 + `finance` clears
every level-≤2 collection whose compartments are a subset of his. It
excludes IP and legal-compliance, which are level 2 but use `legal`. It also
excludes both level-3 areas. The axes are independent, just as §16 showed
with its wrong-compartment example.

Now send the same question through both sessions:
`{"query": "What debt does Northgate owe Granite Bank?"}`:

```text
priya (level 0):   searched [dataroom-index] — 2 hits, both from
                   northgate/000_data_room_index.md (its two chunks)
  "…Granite Bank debt is approved and outstanding despite a
   no-long-term-debt assertion.…"

marco (lvl 2 + finance): searched 9 collections — 10 hits, 9 envelopes,
  [dataroom-corporate]  ng-dd-0007_board_minutes__record_07.md
  [dataroom-finance]    ng-dd-0096_debt_schedule__period_06.md
  [dataroom-equity]     ng-dd-0071_warrant_register__item_29.md   …
```

Read Priya's result carefully because it means more than "she sees less."
The data room's index document is *itself* a level-0 artifact that names
the red flags. She learns that a Granite Bank issue exists because the
corpus put that fact at level 0. She cannot retrieve any underlying
document because the corpus placed them at levels 1 and 2.

The platform did not choose what the index document reveals. The corpus
authors did. Clearance design is corpus design, as §16 argues. The mesh
enforces the model by filtering before ranking. Blocked documents are never
candidates, and no 403 reveals that they exist. A direct search against a
collection she cannot clear returns 404, as §16 showed.

Marco receives ten hits across nine collections and **nine** envelopes.
This is the session plane's one-envelope-per-collection rule from §17 at
full width.

### Step 5: The conversation is yours: three turns, rewritten

§17's finding, applied where it bites. The server persists conversation
state and never uses it. Each turn retrieves the literal query string. The
client therefore rewrites follow-ups before sending. This transcript shows
the rewrite in Marco's session. Turn one is self-contained:

```text
T1  "What is the outstanding principal on the Granite Bank term loan?"
    ordinal 2 · 10 hits · board minutes + debt schedule at the top
```

The user's actual second utterance was *"Who approved it?"*, an anaphor
the server would retrieve as noise. Rewritten client-side out of the
evidence already served (turn 1's top hit is titled "Board Minutes" and
says "The board approved a USD 2,000,000 term loan from Granite Bank"),
following §17's transferable rule. Rewrite from the corpus's vocabulary,
not the assistant's paraphrase of it:

```text
T2  sent: "Who approved the Granite Bank term loan?"
    ordinal 3 · ng-dd-0007_board_minutes  score 0.0328   ← both legs
```

We sent the third utterance, *"And when does it mature?"*, **raw
first**, deliberately, as the §17 contrast:

```text
T3-raw       "And when does it mature?"
    ordinal 4 · ng-dd-0007_board_minutes  score 0.0273   ← one real content word
T3-rewritten "When does the Granite Bank term loan mature?"
    ordinal 5 · ng-dd-0007_board_minutes  score 0.0328   ← both legs again
```

The raw anaphor partly worked because "mature" is a content word this
corpus uses ("The loan matures in 2027 and is secured by substantially
all company assets"). That is the demo-luck trap §17 warned about.
Follow-ups often share one term with the earlier question and work well in
demos. They fail when a user depends most on context. "Who approved it?"
has no such anchor.

The rule remains: a query must stand alone before it crosses the wire.
Retrieval-only turns use Postgres, so all five turns were free. The full
transcript is available at `GET /v1/sessions/{id}` for reload and audit. It
is appended but never consulted.

### Step 6: The red-flag act: one planted conflict, caught

Now the chapter §18 promised: extraction from two conflicting documents,
over the real corpus, against a conflict the answer key names.
`seeded_findings.json` calls it `rf-vectorpay-population-conflict`
(severity **high**): the VectorPay breach exposed data for 84,216
individuals, but the company's incident register recorded 12,400. That is a 6.8×
understatement sitting in the data room for any buyer who reads both
documents. The two documents the key names, quoted:

> **NG-DD-0478** (`09_privacy_security/…security_incident_report__record_01.md`,
> `src-43855c4ca52ef32d` from step 2): "On September 14, 2022, a
> compromised service account exposed names, contact data, member
> identifiers, and limited claims data for 84,216 individuals. The
> incident register initially recorded 12,400 affected individuals."
>
> **NG-DD-0450** (`08_legal_compliance/…settlement_agreement__matter_21.md`):
> "The California Attorney General opened an inquiry into delayed notice
> of the VectorPay incident affecting 84,216 individuals."

An extraction pass working through the room in folder order meets the
register's original figure first and mints it (the model proposes; your
prompt asks for terse `subject`/`key`/`value` triples and nothing more,
§18's division of labor). Create the lineage with the `rw` token, then
land the first claim:

```bash
# $B/$A/$U/$C — base URL and the three standing headers (Authorization:
# Bearer ops21, X-Munarium-Uid, Content-Type), §18's rig idiom.
V=$(curl -s -X POST $B/v1/versions -H "$A" -H "$U" -H "$C" \
  -H "Idempotency-Key: $(uuidgen)" -d '{}' | jq -r .version_id)
# -> memv-b48eb0fb423d415da202b8763c47ffb3

POST /v1/versions/$V/claims
  {"claim_type":"fact","subject":"vectorpay_incident","key":"affected_individuals","value":"12400"}
# -> seq 1, status "accepted", id claim-7f791c43361241fa8986362a8972766f
```

Notice the key discipline doing quiet work: a folded subject, dot-free key,
and brief numeric value. These are the three prompt rules §18 drew from
the measured recall gains. They make the next write *collide* instead
of landing under another spelling. Then the legal file's figure meets the
ledger:

```json
{ "claim": { "id": "claim-6d90d1d9b1e44afdbde9712ff7e15908",
             "normalized_text": "vectorpay_incident.affected_individuals=84216",
             "status": "disputed", "seq": 2, "…": "…" },
  "findings": [
    { "rule_id": "gate.ledger-conflict", "severity": "block",
      "message": "claim 'vectorpay_incident.affected_individuals=84216' conflicts with accepted canon 'vectorpay_incident.affected_individuals=12400' (use a correction to supersede)",
      "detail": { "claim_key": "vectorpay_incident.affected_individuals",
                  "canon_claim_id": "claim-7f791c43361241fa8986362a8972766f",
                  "canon_seq": 1, "canon_value": "12400",
                  "proposed_value": "84216" } } ],
  "head_seq": 2 }
```

HTTP 200. Sequence consumed. Nothing was dropped or overwritten. The
understatement planted by the answer key is now a *detected, reviewable
event*. The finding contains both values and both claim ids. Persist that
array because §18's rule says findings travel only in the write response.
The due-diligence application's core screen is the query that §18 reframed
as the product:

```bash
GET /v1/versions/$V/facts?statuses=disputed
# -> [ { "seq": 2, "normalized_text": "vectorpay_incident.affected_individuals=84216",
#        "status": "disputed", "provenance": "witnessed" } ]   head_seq: 2
```

The reviewer reads both documents, notes that the incident report itself
concedes the register figure was the initial undercount and the AG
inquiry corroborates 84,216, and issues the first verdict from §18's
three: *the new value is right.* A correction supersedes through the
front door:

```bash
POST /v1/versions/$V/claims
  {"claim_type":"correction","subject":"vectorpay_incident","key":"affected_individuals",
   "value":"84216","supersedes_id":"claim-7f791c43361241fa8986362a8972766f"}
# -> seq 3, status "accepted", findings: []       head_seq: 3

GET /v1/versions/$V/facts
# -> [ { "seq": 3, "claim_type": "correction", "value": "84216", "status": "accepted",
#        "supersedes_id": "claim-7f791c43361241fa8986362a8972766f" } ]
```

Canon now answers 84,216. The last read is the one a transcript
can never give you. The buyer's preliminary risk memo went out while the
ledger stood at seq 1. What did the application *know* when that memo was
written?

```bash
GET /v1/versions/$V/facts?as_of_seq=1
# -> [ { "seq": 1, "value": "12400", "status": "accepted" } ]   as_of_seq: 1, head_seq: 3
```

**12,400, the world as it was believed and reproduced exactly,** with the
response honest about both truths at once (`as_of_seq: 1`,
`head_seq: 3`). One pin, one clock, invariant #3 doing in one query
parameter what §18's sequential validation did across four
audited quarters. This is the March answer on your own corpus. Its price is
remembering a sequence number.

### Step 7: Completion under your own key

Step 1 armed `MUNARIUM_SECRET_ANTHROPIC`; §20's cost-governance rules now
apply to this rig, so the preflight comes first. `/healthai` fires nine
probes across three families and three tiers. Each *live* probe spends real
tokens. It is a deploy-time check, not a health poll, as §17's economics
section explains:

```text
GET /healthai        -> healthy: true
  anthropic  fast     claude-haiku-4-5            ok      822 ms
  anthropic  capable  claude-sonnet-5             ok    1,897 ms
  openai     fast/capable                         skipped  "credential env var 'MUNARIUM_SECRET_OPENAI' is not set"
  openrouter fast/capable                         skipped  "credential env var 'MUNARIUM_SECRET_OPENROUTER' is not set"
```

One key is live, and four checks are honestly skipped. This is the shape §17's
resolver chain predicts for an anthropic-only rig. The committed runbook
routes `tasks.completion` to the **capable** tier and declares
`allowOverrides: [default]`, so a caller may drop to the fast tier per
turn without renegotiating policy (§17's economics dial; §20's
default-closed argument). One completion turn in Marco's session, on the
cheapest tier:

```powershell
POST /v1/sessions/ses-019ff45e8035…/turns
  { "query": "What is the outstanding principal on the Granite Bank term loan, and how has management classified it?",
    "complete": true, "model_override": { "tier": "fast" } }
```

```json
{ "ordinal": 6,
  "completion": { "provider": "anthropic", "model": "claude-haiku-4-5",
                  "was_override": true, "input_tokens": 2411, "output_tokens": 298,
                  "text": "…" } }
```

The completion text, shortened to its main points, shows the value of the
runbook template. "A conflict between documents is a FINDING" is prompt
policy from git, not model virtue:

> **Outstanding Principal:** According to the **Debt Schedule, Period
> 06** (NG-DD-0096, dated 2024-01-10), the Granite Bank term-loan
> principal was **USD 1,620,000** at December 31, 2023 […]
> **Management Classification:** Management classified the term loan
> **within other non-current liabilities rather than debt** [NG-DD-0096].
> **Key Conflict:** This classification conflicts with the **Board
> Minutes, Record 07** (NG-DD-0007) […] *"This approval conflicts with
> later management statements that the company has no long-term-debt."*
> [NG-DD-0007] **Finding:** […] a material misclassification or
> disclosure inconsistency requiring resolution.

The response is grounded, cited, and clear about the conflict. Now apply
§17's Lesson 2: every rendered answer needs a deterministic quote check.
This capture makes the case better than a planted example.

The model's quote says "no long-term-**debt**," while the served text of
NG-DD-0007 says "no long-term debt." The difference is one hyphen. Search
for the quoted span in `hits[].text`, and it **does not resolve**. It is a
paraphrase inside quotation marks. The check needs no model, key, or server
feature.

Render the quote as unverified, or spend one corrective turn with the
evidence attached in Lesson 1's fetch-on-cite form. "Citations checkable
rather than decorative" costs one string search that you must run.

The attribution loop closes on the reports plane (§20): the turn's
resolved model, its override status, and its tokens are already in the
mgmt-role cost report:

```json
GET /v1/reports/cost -> { "rows": [ { "provider": "anthropic", "model": "claude-haiku-4-5",
  "turns": 1, "overridden_turns": 1, "input_tokens": 2411, "output_tokens": 298 } ] }
```

The usage report splits the same spend by uid. Total model spend
for this entire step, probes included: well under one cent, billed to
this rig's own provider account, key material never surfaced anywhere.
That is BYOK working as designed: the picture `was_override: true`
paints for the auditor costs nothing extra to keep.

### Step 8: Grade before you ship

For a complete methodology without code examples, follow
[Creating a laboratory for your corpus application](creating-a-lab.md).
It covers case design, independent answer keys, controlled shape/runbook
comparisons, failure diagnosis and acceptance on your actual Server deployment.

This tutorial ends where every corpus application should begin: with an
answer key. It was not run here at scale because a graded harness deserves
your corpus, not this chapter's. The pattern is small enough to show in
full. Its discipline is the strongest habit Part II can leave you with.

Here are three questions from the same `seeded_findings.json` facts used by
this chapter. They use a graded-kind vocabulary:

```yaml
# answer-key.yaml — versioned in git beside your runbook, NEVER ingested
questions:
  - id: q1                      # graded kind: contains_all
    ask: "What is the outstanding principal on the Granite Bank term loan?"
    expect: { contains_all: ["1,620,000"], cite_any: ["ng-dd-0096"] }
  - id: q2                      # graded kind: conflict — both sides, both cites
    ask: "How many individuals were affected by the VectorPay incident?"
    expect: { sides: [["84,216"], ["12,400"]],
              cite_all: ["ng-dd-0478"], cite_any: ["ng-dd-0450"] }
  - id: q3                      # graded kind: absence — the negative control
    ask: "What is Northgate's outstanding balance with Meridian Bank?"
    expect: { insufficient: true }    # no such LENDER; a confident answer FAILS
```

The check loop is one page of client code, not a framework. Open a session
for each model tier, send every `ask`, and score the result with fixed
rules. `contains_all` checks for substrings in the completion. `conflict`
requires *both* sides and their citations. Question 2 fails a model that
"helpfully" averages away the conflict, which is the failure that §18's
gate catches earlier. `absence` treats an honest refusal as a pass. Under
§17's Lesson 4, insufficiency is a success state. The negative control
proves that your assistant can report it.

Question 3 is not a random missing name. **Meridian Health is this data
room's largest customer**, so retrieval will find Meridian hits. The
control tests refusal during a lexical collision, where invented answers
occur. It does not test refusal against an empty index.

Every expectation above can be checked with string search against fields
already returned by the turn: completion text, `hits[].source_path`, and
the envelopes.

Then hold that standard. Grow the key toward twenty or more questions
using the corpus's real threads, each with a deterministic graded kind. A
serious key for a data room of this size runs to eighty or so questions.

Run the key against two tiers before choosing your default. Fast-tier models
tend to fail *ceremony*, not retrieval, and verification recovers more of that
gap than upgrading the model does.

Keep the harness in CI so a runbook edit that quietly harms grounding fails
a build instead of a customer. Your corpus deserves its own evidence file, and
you have now seen every mechanism it would measure.

### Striking the set

The tutorial tenant is large. It has 613 sources, 614 chunks in thirteen
partitions, a ledger lineage, sessions, tokens, and about sixty audit rows.
This book does not leave rigs running, as required by §2's reaping rule.

Teardown follows the order taught by the system. Stop the server process
**by port identity**. Another chapter's rig was live on 18085 during this
run. Killing by name would kill a colleague's work. Stopping this process
also drops the in-memory BYOK secret.

Physical deletion is not an API. Section 20's fourth lever makes it a DBA
operation on a shared rig. Here, it ran directly against our own compose
Postgres in one transaction. First, drop the thirteen
`collection_chunks_p_*` partitions for `ch21-da33`. Then delete the tenant
rows children-first: interactions (60), session turns (7), sessions (2),
access tokens (2), run steps (65), the run, index versions (13),
collection-source bindings (613), collections (13), the runbook, the shape,
source blobs and sources (613 each), three claims and their ledger events,
and the memory version.

The check afterward found **zero rows and zero partitions** for the tenant.
Ports 18080/19090 were free, and `MUNARIUM_SECRET_ANTHROPIC` left with the
process environment. The evidence remains where it belongs: in this
chapter.

### Where this leaves you, and where it leaves the book

Take stock of what happened. A fresh clone became a governed AI application
over a 613-document M&A data room. It was divided by clearance, ready for
conversation, able to detect conflict, and able to answer at a point in
time. The work took **eight steps, four seconds of index build, thirteen
recorded human decisions, and less than a cent of model spend**. It required
no server code.

Every concept from §14–§20 appeared in its working setting. You saw the uid
contract and three-role credential split. You saw filename-as-identity,
empty `bound_to`, the approval gate, and the envelope. Clearances filtered
before ranking, while the client owned the conversation. The conflict
passed through a gate, queue, correction, and pin. The resolver chain,
`was_override`, cost report, and answer key outside the index completed the
picture.

This makes Part II's argument real: **your application owns the user
experience; the mesh owns the truth discipline**. Between them is a stack
of YAML in your repo, a few HTTP calls, and decisions with names attached.

To the reader who arrived through Part I: you now know this system from
both sides of its API. You can change the platform without breaking the
five invariants. You know which crate owns each part, which tests hold the
line, and where the gaps ledger marks the edges.

You can also build on the platform as this chapter did, taking those
invariants as given and putting them to use. The two skills are one skill.
An engineer who knows why `disputed` is a success status writes better
review queues. An engineer who has rendered a review queue writes better
gates. Parts I and II are two books with one spine because the system is
two disciplines with one ledger.

The ledger is the part you keep. This guide followed three standards: run
every claim, state every gap, and track versions in the same change. These
are not rules of documentation style. They apply to prose the same
properties that the platform enforces on memory. You inherit both.

Your corpus will meet a real contradiction. A regulator may ask what your
assistant knew in March. A quote may fail its string check by one hyphen.
The machinery in these chapters helps you answer. The habit of proving what
you publish gives others a reason to believe that answer.

Append honestly, supersede openly, and pin what you rely on. Odin feared
losing memory more than thought. Build accordingly.

There is a third discipline, and it comes before the other two rather than
after them. Everything Parts I and II asked you to take on faith — that the
gates catch what they claim to, that fetch-before-cite is worth a retry
round, that a vocabulary collision costs more recall than a weaker model —
was decided by measurement somewhere, on a corpus, at a stated cost. Your
corpus deserves the same. Before a shape or a runbook goes to production,
give it a lab — a disposable server, a slice of the real documents, and a
graded answer key — and let measurement, not this book, decide what your
corpus needs.

## 21A. Operating runbooks: publish, run, gate, evolve, retire

Two lettered chapters close Part II from the practitioner's seat. §15
taught what a runbook *is*, §16 how to design one, and §21 walked the
worked spine; what remains is the day-two material — the complete operating
loop for the runbooks you now have (this chapter), and the server-guided
path for creating new ones (§21B). They are lettered rather than renumbered
so that every existing chapter reference stays true.

Everything below was executed against a from-source server on a fresh
database: postgres store, `pg` source store, static tokens
(`guide-rw`/`guide-mgmt` on one tenant), no provider keys — the whole
chapter is $0.00 and reproducible with nothing but the repo and Docker.

```powershell
$env:MUNARIUMCTL_URL = 'http://127.0.0.1:18080'
$env:MUNARIUMCTL_TOKEN = 'guide-rw'
$env:MUNARIUMCTL_UID = 'guide-author'
```

![Operating a runbook: apply publishes the shape then the runbook as a kind-routed upsert; validate reports deterministic findings; run --watch executes resolveSources, buildIndex and verify side by side; the run pauses awaiting approval at cutover until a human approves; and retireOld keeps versions for rollback. An evolve loop re-applies edited YAML, and a retire lane shows double-pass soft removal with no resurrection](images/ch21a-runbook-lifecycle.svg)

### Publish: apply is kind-routed, and shapes go first

`mmctl apply -f <file>` reads the document's `kind:` (parsed from the
YAML, not sniffed from its text) and routes to the matching endpoint. A
collection cannot bind to an unpublished shape — the executor refuses at
materialization rather than silently falling back to default chunking — so
the order is always shape, then runbook:

```powershell
.\target\debug\mmctl.exe apply -f runbooks\shapes\dataroom-documents.yaml
```

```json
{
  "event_id": null,
  "shape_ref": "dataroom-documents@1",
  "yaml_hash": "d95e7cce202321dc9b4bc300e3d277d1bdd79c4a630bb84178d4b8e5c9ea8de4"
}
```

```powershell
.\target\debug\mmctl.exe apply -f runbooks\applications\due-diligence.yaml
```

```json
{
  "runbook_ref": "due-diligence@1"
}
```

Applying a v2 runbook materializes its collections immediately, so the
application is inspectable before its first run. `runbook info` answers the
governance question — which indexes does this application reach, and what
clearance does each require:

```powershell
.\target\debug\mmctl.exe runbook info due-diligence
```

```json
{
  "collections": [
    {
      "access_level": 0,
      "collection_id": "col-01a01c9a7ffb719081f9838624997d37",
      "compartments": [],
      "name": "dataroom-index",
      "shape_ref": "dataroom-documents@1",
      "source_count": 0
    },
    {
      "access_level": 2,
      "collection_id": "col-01a01c9a801b7f53b419157fd28a1530",
      "compartments": [
        "finance"
      ],
      "name": "dataroom-equity",
      "shape_ref": "dataroom-documents@1",
      "source_count": 0
    }
  ]
}
```

(abridged — the full response lists all thirteen collections, sibling
versions, the models block, and retrieval knobs.)

### Validate before you run — and know what --suggest costs

`runbook validate -f` runs the deterministic checker (the ~30 stable dotted
codes of §16) with zero model calls; `--suggest` adds a BYOK advisory pass.
On a keyless server the deterministic half still answers in full and the
advisory half says so honestly:

```powershell
.\target\debug\mmctl.exe runbook validate -f runbooks\applications\due-diligence.yaml --suggest
```

```json
{
  "findings": [],
  "suggest_note": "suggestions unavailable: provider error: no default provider credential configured (checked MUNARIUM_SECRET_ANTHROPIC, MUNARIUM_SECRET_OPENAI, MUNARIUM_SECRET_OPENROUTER and applied configs)",
  "suggestions": [],
  "valid": true
}
```

That degrade shape recurs across the whole surface (§21B's assist behaves
the same way): deterministic checks never depend on a provider, and a
missing key is a note, never a failure.

### Run: the five steps, and the pause that is the point

A v1 pipeline is the cleanest demonstration of the executor. Publishing
`support-tickets@1` and `tickets-reindex@1` and running immediately teaches
the first operational lesson — an index over nothing is refused, not built:

```text
mmctl: 400 Bad Request: invalid input: no sources bound to shape 'support-tickets@1' — PutSource with shape_ref first
```

Upload one document (raw bytes; `X-Filename` is the identity and blob path,
`X-Shape-Ref` the binding) and run again:

```powershell
Invoke-RestMethod -Method Put -Uri "$env:MUNARIUMCTL_URL/v1/sources" -Headers @{
  Authorization = 'Bearer guide-rw'; 'x-munarium-uid' = 'guide-author'
  'X-Filename' = 'tickets/2026/ticket-4821.md'
  'X-Shape-Ref' = 'support-tickets@1'; 'Content-Type' = 'text/markdown'
} -Body $ticket
```

```json
{
  "source_id": "src-c2f2c57dc4c5a5ea",
  "content_hash": "c3822c8a21c202d35dbb055b0eebfb5cf1025f77fbf2b5fac9797f1cd485c81b",
  "bytes_len": 130,
  "already_existed": false
}
```

```powershell
.\target\debug\mmctl.exe run tickets-reindex --watch
```

`--watch` polls every two seconds and prints each state until the run
leaves `running`. Every step records a `detail` you can read back forever —
how many sources resolved, which immutable index version was built, whether
the self-probe found its own chunks:

```json
{
  "run_id": "run-01a01c9afa9d7cf08a832e3b264a5bca",
  "runbook_ref": "tickets-reindex@1",
  "state": "awaiting_approval",
  "steps": [
    { "detail": { "sources": 1 }, "name": "resolveSources", "ordinal": 0, "state": "done" },
    { "detail": { "index_version": "idx-681ab3e402ddc1f2", "watermark": 0 },
      "name": "buildIndex", "ordinal": 1, "state": "done" },
    { "detail": { "chunks": 1, "self_probe_hits": 1 },
      "name": "verify", "ordinal": 2, "state": "done" },
    { "detail": null, "name": "cutover", "ordinal": 3, "state": "awaiting_approval" },
    { "detail": null, "name": "retireOld", "ordinal": 4, "state": "pending" }
  ]
}
```

```text
approve with: mmctl approve run-01a01c9afa9d7cf08a832e3b264a5bca <step-ordinal>
```

The pause is the design, not an inconvenience: the new index exists
side-by-side, verified, and *not serving*. Nothing goes live until a human
says so:

```powershell
.\target\debug\mmctl.exe approve run-01a01c9afa9d7cf08a832e3b264a5bca 3
```

```json
{
  "run_id": "run-01a01c9afa9d7cf08a832e3b264a5bca",
  "state": "done"
}
```

`retireOld: {keep_versions: 2}` then reclaims older inactive versions while
keeping two for rollback. For a v2 application the same five steps run once
per collection, and each collection's cutover is its own approval — a
thirteen-collection data room pauses thirteen times, deliberately.

### Evolve: what a change costs

Re-applying a runbook with the same `name@version` is the documented
in-place upgrade (an upsert). Shapes are stricter — additive versioning
means the same `name@version` with different content is refused with "bump
the version", because old versions are provenance. §16's closing table is
the operator's decision matrix; compressed:

| You changed | You owe |
|---|---|
| document bytes, new/removed sources | a new run (rebuild) |
| shape `chunking.max_chars` | a new shape VERSION + rebuild (index identity) |
| runbook `retrieval:` knobs | re-apply only — query-time, no rebuild |
| `models:` / `completion:` blocks | re-apply only |

Sessions pin `name@version` at creation, so an upgrade mid-conversation
never changes what a running session can see.

### Retire: double-pass, soft, and final

Removal is a two-step handshake with a 15-minute window, and it is a soft
transition — every run, step, and event row is retained:

```powershell
POST /v1/runbooks/tickets-reindex@1/remove-request
```

```json
{"runbook_ref":"tickets-reindex@1","removal_id":"rm-01a01c9bb5cd7ed38bf23a6f743d42cb","expires_at":"2026-08-20T00:54:21Z"}
```

```powershell
POST /v1/runbooks/tickets-reindex@1/remove-confirm   # body: {"removal_id": "..."}
```

```json
{"runbook_ref":"tickets-reindex@1","status":"removed"}
```

Two rules keep removal honest. A removed ref never resurrects:

```text
mmctl: 400 Bad Request: invalid input: runbook 'tickets-reindex@1' was removed; publish a new version instead
```

And re-applying while a removal request is *armed* resets it — the YAML
changed, so a `removal_id` issued against the old content must not be able
to remove the fresh version. (We hit that live while writing this chapter:
a re-apply between request and confirm cleanly disarmed the pending
removal, and the second request minted a new id.)

### Where this leaves you

Publish shape-then-runbook, validate before you run, let `--watch` carry
you to the gate, approve deliberately, and know the cost table before you
edit. `mmctl` and curl are interchangeable because every operation here is
one REST call — and since 2026-08-27 the operator console at
`/admin/runbooks` (mgmt login) is a third reading of the same objects: the
hub lists every hosted runbook (removed ones included, with their status),
every published shape, and every chronology-rules asset; each has a viewer
(`/admin/runbooks/{ref}`, `/admin/shapes/{ref}`,
`/admin/chronology-rules/{name}`) that shows the applied YAML beside what
`info` reports — collections and their active indexes, the flattened plan
with its gates, retrieval knobs, models, the completion template. A run's
page (`/admin/runs/{id}`) shows the step machine step by step, and a step
that is `awaiting_approval` carries the one action the console offers on
runbooks: approve the gate. Because approval is an **rw** operation (the
transition becomes a ledger event when the run names a lineage), the form
asks for the rw token every time and never stores it — the mgmt cookie
alone cannot approve, which is the role split §13 documented, kept intact.
Publishing stays on `mmctl apply -f` by design: the deploy artifact is the
applied bytes, and git is their source of truth. What this chapter did not
cover is where a *well-designed* runbook comes from in the first place —
that is §21B.

## 21B. Creating runbooks and shapes: the guided authoring path

The thirteen committed applications under `runbooks/applications/` show
what good looks like; §16 teaches why. As of 2026-08-19 the server also
*executes* that teaching: a guided authoring surface — REST under
`/v1/authoring/*` and the `mmctl author` command family — that
interviews you in §16's decision order,
deterministically materializes the shape + runbook your answers imply,
validates the SET across documents, optionally lets a BYOK model refine the
draft, and exports a hash-manifested bundle whose deploy path is the same
two routes §21A used. Drafts need the postgres store and the rw role;
patterns are readable by any authenticated role.

As in §21A, every output below is captured from a live keyless run —
including the one step that *wants* a provider key and says so when there
is none.

![The guided authoring loop: the pattern catalog seeds a draft with an ordered interview; answers materialize and validate deterministically with per-document and set-level findings; an optional assist pass re-validates and discards bad output; export refuses while error findings exist and writes shapes, runbooks and a bundle into git; and bundle apply verifies every hash before posting, so any byte drift since export kills the deploy](images/ch21b-authoring-loop.svg)

### Start from a pattern

`author patterns` serves §19's catalog with each pattern's exemplar and its
guidance attached, so the starting point is a worked precedent, not a blank
page:

```powershell
.\target\debug\mmctl.exe author patterns
```

```json
{
  "patterns": [
    {
      "description": "One question in, clearance-filtered evidence retrieved, a cited answer out — or an honest \"the corpus does not establish this\". No conversation, no accumulation; the workhorse pattern.",
      "guidance": "Strongest when a question is answerable from a bounded set of documents. Design against the confident answer the corpus does not actually establish — insufficiency is a correct outcome, not a failure.",
      "has_completion": true,
      "id": "ask-the-corpus",
      "name": "Ask the corpus",
      "start_from": "financial-advisory"
    }
  ]
}
```

(abridged — seven patterns; `author pattern <id>` returns one with its full
exemplar YAML and decision notes. `author new <name> --pattern <id> --seed`
copies the exemplar documents into the draft, renamed, when you would
rather edit downward from a working application than answer upward from
questions.)

### A draft, and the interview that comes with it

```powershell
.\target\debug\mmctl.exe author new field-notes --pattern ask-the-corpus
```

```text
draft_id: draft-01a01c9c016a79a3842cb106c047bb12  state: interview
```

The response carries the whole interview: sections in §16's revisability
order (identity → prefix layout → access → retrieval → extraction →
lifecycle → completion), each question with the guidance prose attached at
the moment of decision and a `doc_ref` naming the chapter that teaches it
in full. The completion section only appears for patterns that have a
completion arm.

A draft with no documents refuses to export — the refusal names the fix:

```text
mmctl: 409 Conflict: draft 'draft-01a01c9c016a79a3842cb106c047bb12' has no documents — answer the interview or seed from a pattern first
```

### Answers materialize documents — and the set checks catch what per-document validation cannot

Answers are one flat YAML map keyed by question id. This first attempt
plants a classic mistake on purpose:

```yaml
identity.description: Field engineering notes and the incidents they explain.
prefix.root: field/
prefix.areas:
  - { path: notes/, description: per-visit engineering notes }
  - { path: incidents/, description: incident writeups }
  - { path: ground_truth/, description: the graded incident key }
access.uniform_public: false
access.area_levels: { notes: 1, incidents: 2, ground_truth: 2 }
access.area_compartments: { incidents: [field-ops], ground_truth: [field-ops] }
```

```powershell
.\target\debug\mmctl.exe author answer draft-01a01c9c... -f field-answers.yaml
```

The documents materialize and validate in the same call, and the SET layer
— checks that need to see across documents — flags the plant:

```json
{
  "code": "set.answer-key-filename",
  "message": "collection 'field-notes-ground-truth' binds 'field/ground_truth/', which looks like an answer key — a key inside the retrieval index is not a measurement; keys belong to the grader, not the corpus",
  "path": "runbooks/field-notes.yaml: spec.collections[2].sources.filenamePrefix",
  "severity": "warn"
}
```

The other `set.*` codes work the same altitude: a collection binding a
shape that is neither in the set nor published (`set.shape-unresolved`,
Error — apply would fail, so authoring fails first), an in-set shape
version colliding with content already published on this server
(`set.shape-version-conflict`, the additive-versioning preflight), a
less-restricted collection whose prefix covers a more-restricted one's
documents (`set.prefix-shadows-restricted`), and an unused shape
(`set.shape-unused`). Dropping the `ground_truth/` area and re-answering:

```text
valid: True  todos: 0  docs: 2
```

Two documents, because the interview produced a shape as well. The
materialized runbook is ordinary YAML you could have written by hand — the
point is that its values carry the measured lessons without you having to
remember them:

```yaml
# Field engineering notes and the incidents they explain.
#
# Areas (one collection per governance boundary):
#   notes/ — per-visit engineering notes
#   incidents/ — incident writeups
#
# Materialized by munarium authoring (pattern: ask-the-corpus; exemplar: financial-advisory).
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata:
  name: field-notes
  version: 1
spec:
  collections:
  - accessLevel: 1
    name: field-notes-notes
    shape: field-notes-documents@1
    sources:
      filenamePrefix: field/notes/
  - accessLevel: 2
    compartments:
    - field-ops
    name: field-notes-incidents
    shape: field-notes-documents@1
    sources:
      filenamePrefix: field/incidents/
  completion:
    promptTemplate: |
      You answer questions about this document corpus using ONLY the retrieved evidence below. Cite every claim as doc#node. If the evidence does not establish an answer, say so plainly — an honest "the corpus does not establish this" beats a guess. A search hit you did not read is not a citation. When a question asks about an enumerable set (all X, every Y), enumerate the set from the evidence rather than sampling it.

      Evidence:
      {context}

      Question: {query}
    verification:
      citations: true
      maxRetries: 1
      quotes: true
  models:
    allowOverrides: false
    default: { provider: default, tier: capable }
    tasks:
      completion: { provider: default, tier: capable }
      validation: { provider: default, tier: fast }
  retrieval: { candidateN: 100, rrfK: 60, topK: 10 }
  sources: { container: sources, prefix: field/ }
  steps:
  - resolveSources: {}
  - buildIndex: {}
  - verify: {}
  - cutover: { approval: required }
  - retireOld: { keep_versions: 2 }
```

(reformatted for the page; the emitted file spells nested maps out
long-form.) The prompt template's three rules — cite-or-insufficient, a
search hit you did not read is not a citation, enumerate enumerable sets —
are §17's grounding lessons and the financial-advisory fix that took a
0.25 question to 1.0. The shape it emitted alongside enforces folded
subjects and DOT-FREE keys, the `subject.key` split rule §18
explains. Partial answers are fine at every step: unanswered questions become
`todos` plus placeholder values, so a fresh draft validates with "red
TODOs expected" rather than errors.

### Assist: a model may refine the draft, and its failures are notes

`author assist` sends the corpus description, the answers, the current
documents, and the deterministic findings ("do not repeat them") to a BYOK
model, which may replace whole documents and add suggestions. Three rails
hold it: only paths already in the draft are accepted, every returned
document must parse or the whole edit is discarded (suggestions survive),
and the result re-validates before it lands. On this keyless server:

```text
assist_note: assist unavailable: provider error: no default provider credential configured (checked MUNARIUM_SECRET_ANTHROPIC, MUNARIUM_SECRET_OPENAI, MUNARIUM_SECRET_OPENROUTER and applied configs)
```

— a 200 with a note, the draft untouched. Assist never fails the request
and never downgrades a validated draft it did not change.

### Export: the bundle is the deploy contract

```powershell
.\target\debug\mmctl.exe author export draft-01a01c9c... --out field-out
```

```text
exported + verified 2 files to C:\...\field-out (manifest 739737f305a01d8c4fdaec0828143515b54d2f39a202b30ba7edea137d64f8d2)
```

```text
field-out\bundle.json
field-out\runbooks\field-notes.yaml
field-out\shapes\field-notes-documents.yaml
```

Export re-validates inline and refuses (409 `authoring-draft-invalid`)
while any error finding exists — the stored state is a progress display,
never the gate. `bundle.json` carries the files verbatim, a sha256 per
file, `apply_order` (shapes first), and a `manifest_hash` over the sorted
(path, hash) pairs; mmctl re-reads what it wrote to disk and verifies
before reporting success. This directory is what you commit: git stays the
source of truth, and the bundle makes review byte-precise.

### Deploy: the same two routes, now with teeth

`bundle apply` needs no new server surface — it verifies, then POSTs each
file through the `/v1/shapes` and `/v1/runbooks` routes of §21A. What it
adds is the refusal to deploy anything that is not byte-identical to the
validated export. One appended comment line:

```text
mmctl: 'runbooks/field-notes.yaml' does not match its declared hash — bundle content drifted since export
```

Restore the file and point at production (here: a second tenant's rw
token):

```powershell
$env:MUNARIUMCTL_TOKEN = 'prod-rw'
.\target\debug\mmctl.exe bundle apply -f field-out\bundle.json --dir field-out
```

```text
applied shapes/field-notes-documents.yaml
applied runbooks/field-notes.yaml
```

```powershell
.\target\debug\mmctl.exe runbook info field-notes
```

```json
{ "runbook_ref": "field-notes@1", "status": "active" }
```

— with both collections materialized, ready for §21A's upload → run →
approve loop. `--dir` makes the git-reviewed files on disk the source
(each still verified against the declared hash, LF-normalized so a Windows
autocrlf checkout is not a false alarm); CI wires the identical two
commands, and additive versioning remains the compatibility guarantee — a
re-export whose shape changed without a version bump gets production's
"bump the version" refusal, and the authoring server's
`set.shape-version-conflict` preflight catches the same mistake earlier.

### No browser path, and what to read next

Authoring has no page in the operator console. Between 2026-08-19 and
2026-08-27 `/admin/authoring` offered the create/answer/validate/export
loop as HTML forms; it was removed when the console became the control
plane, because the loop's product is a git-reviewed bundle whose deploy
path is `mmctl bundle apply` — a form that ends in a download served no
real purpose beside the CLI, and every mutating admin page is surface to
defend. What the console does show is the RESULT of authoring once
applied: the shape and runbook viewers in §21A read the same documents
`bundle apply` published. Assist and apply were always API/CLI-only.

Authoring gives you a *well-formed* application whose values carry
measured defaults. Whether those defaults are right *for your
corpus* is a measurement question — which is why a lab of your own, with a
graded answer key over your documents, belongs between authoring and
production.

## 21C. Structured evidence: documents beside the database they describe

Part II so far has built applications over documents. This chapter is the
one that adds the other kind of evidence — a **system of record** — and it
is lettered like §21A/§21B because it belongs to Part II's arc (an
application is built here). It was
written against what exists: everything
below names a committed file, a served route or a measured cycle, and the
two places where the book's standard — an executed example — was not met
are marked `[gap]`.

### What "structured evidence" is, in one paragraph

A structured answer cites rows, not documents: `[evidence/<id>#<row>]`.
The `<id>` is an artifact **Munarium Matrix** executed against a governed
source and sealed into this server through the evidence plane
(`POST /v1/evidence` and its grant/commit variants, the read routes in §3.x
of `docs/api/rest.md`). The manifest it carries is the vendored contract's
`EvidenceManifest`, verbatim; two hashes travel with it — the
`logical_result_hash` over the canonical encoding, which is the identity a
later reader compares, and the `artifact_hash` over the stored bytes, which
is what a purge deletes — and the server verifies both before it stores a
byte. The claim the whole plane rests on is small and exact: *these rows,
at this engine position, under this authorization class, produced by this
contract or this semantic view whose definition was fingerprinted before it
ran.* Everything an answer then says about a number is a citation into that.

### The three modes, as the server sees them

Matrix has three modes and the server meets each differently.

- **Mode A, materialize.** A source entity rendered as one document per row
  and uploaded under its source id (`crm/…`) through the bulk upload API —
  so a record sits *beside* the documents in a collection whose shape
  declares `kind: system_record` (`runbooks/shapes/record-documents.yaml`).
  To the retrieval loop it is a document; to the evidence hierarchy
  it is a `supporting` layer whose `max_authority` is capped by the shape,
  because a rendered row reports what the database held and is never the
  signed instrument the database describes. On Databricks the path is the
  Change Data Feed: a delete arrives as a **tombstone**
  document at the row's own path, never as a row that quietly stopped
  appearing.
- **Mode B, verified query.** A `dataViews` entry binds a **query
  contract** — SQL an author wrote and Matrix's compiler allowlist-walked —
  or, since 2026-08-30, a **metric view** or a **native data view** asked
  with a *semantic intent*: measures, dimensions and equality filters chosen
  by name from the asset's closed lists. The server never writes SQL and
  never invents a measure; the `intent` model task picks names, Matrix
  compiles them, and the definition the view ran under was fingerprinted at
  verification and re-read before the execution (`metric_view_changed`
  otherwise).
- **Mode C, reconcile.** Matrix reads a source, resolves each row to a
  ledger subject, compares typed values with the claims documents produced,
  and files `matrix.discrepancy-candidate` findings through
  `POST /v1/versions/{id}/findings` (§13 entry 22) — or, once a mapping is
  **promoted**, proposes claims with a `connector` origin. The server
  recognises nothing special about them: a connector claim is `witnessed`
  like any other, and `document_over_source` is the default precedence.

### The runbook grammar

A worked example, as a runbook declares it. The shape names are the
committed `runbooks/shapes/archival-documents.yaml` and
`record-documents.yaml`; the register corpus and its query contracts are
not in this repository:

```yaml
spec:
  collections:
    - { name: register-documents, shape: archival-documents@1, sources: { filenamePrefix: "register/" } }
    - { name: register-records,   shape: record-documents@1,   sources: { filenamePrefix: "crm/" } }
  dataViews:
    - { name: open-pipeline, contract: open-pipeline-by-region@3, parameters: { as_of: { type: date, value: "2026-06-30" } } }
    - { name: holdings,      contract: captable-holdings@1 }
    - { name: pipeline,      contract: pipeline-by-region@2, kind: data_view }   # semantic: asked by the intent task
  retrieval:
    defaultResearchProfile: register
    researchProfiles:
      - name: register
        layers:                                   # ORDER IS THE HIERARCHY
          - { name: register,  sources: [matrix:open-pipeline, matrix:holdings, matrix:pipeline], role: controlling }
          - { name: records,   sources: [register-records],   role: supporting }
          - { name: documents, sources: [register-documents], role: primary }
  models:
    tasks: { completion: { tier: capable }, validation: { tier: fast }, intent: { tier: fast } }
  steps: [resolveSources, buildIndex, verify, verifyDataViews, cutover, retireOld]
```

Three rules the validator enforces: a `dataViews` name must be reachable
from a layer as `matrix:<name>`; a semantic view (`kind: metric_view` or
`data_view`) requires `models.tasks.intent`, because it is that task which
turns the question into names; and `verifyDataViews` runs every bound
view's own verified questions on Matrix **and reads the verdict** — a 200
with `failed: 1` fails the step, naming the question (dev-guide §13.5 entry
23 records the day it did not).

### A turn

`POST /v1/sessions/{id}/turns` with `research_profile: register` (or none,
when the runbook names a default) runs the layers in declared order and
returns an `EvidenceHierarchyDecision` beside the answer; the streaming
twin emits `profile`, `layer_start`, `layer_source`, `layer_complete`,
`coverage` and `compose` stages, plus `intent` and `selection` when a
semantic view is in play. A layer that refuses answers with a refusal
BLOCK, not an error — `intent-unresolved` when no selection was produced,
`source-not-bound` when the profile names a view the runbook does not
declare, Matrix's own typed code otherwise — and the completion prompt is
told which layers said nothing so the model does not answer as if they had.
A turn naming no profile is **byte-identical** to a turn on a runbook with
no `dataViews` at all; four wire-level tests guard that.

Measured on a live rig with a register corpus: four questions green — the
open pipeline answered from the
contract with its sealed row cited (`evidence/ev-52b58e0a…`, one row,
2,520,000.50), the planted 90,500-vs-90,000 disagreement between the
register and a subscription agreement reported with *both* citations, the
Copperline holdings answered from the materialized records, and a question
no source can answer declined rather than invented. The fifth — the
semantic data view — passed the next day: asked "which stage holds the most
pipeline amount", the `intent`
task chose `pipeline_amount` and `opportunity_count` grouped by **stage**,
Matrix compiled the aggregate over the fact table, and the answer read
"Negotiation, $2,400,000.50 across 2 opportunities", citing the sealed
evidence. Note what that demonstrates: `stage` is a grouping no query
contract on that source offers, so the answer could not have come from a
pre-declared statement — and the model still never wrote SQL.

### Resolving a citation

A client resolves `[evidence/<id>#<row>]` with two reads —
`GET /v1/evidence/{id}` for the manifest and `GET /v1/evidence/{id}/rows`
for canonical CSV — under a token that dominates the manifest's class;
every resolution, including a denied one, is audited as *a read happened*
and never *what was read*. The four official clients expose this as their
evidence plane; the worked guide is
[`clients/docs/guides/evidence.md`](../../../clients/docs/guides/evidence.md).
Sealing is deliberately absent from all four.

`[gap]` This chapter's own examples were not re-executed while it was
written: the runbook above is illustrative, and no fresh session transcript
was captured for the page. The next run of a register-backed runbook should
paste its turn here.

The refusals a resolver meets are typed, and the clients key on them:
`evidence-forbidden` (403 — the SESSION's clearance, not the sealer's,
decides), `evidence-not-committed` (409 — a grant was taken and the bytes
never committed), `evidence-expired` (410 — retention purged the bytes and
the citation was real), `evidence-on-hold` (a purge refused under a legal
hold), `evidence-grant-invalid` (a spent or unknown upload grant),
`evidence-hash-mismatch` (bytes that do not hash to their manifest, refused
before anything is stored), `required-evidence-unavailable` (424 — a
REQUIRED layer of a research profile produced nothing) and
`unknown-research-profile` (400). [errors.md](../api/errors.md) is the
registry; §10's documentation-coverage test keeps it complete.

### What the live tiers found on the way

The plane earned its keep by being wrong in ways a mock could not show.
Matrix posted `{"batch": …}` where the contract says `{"manifest": …}`;
`GET /v1/evidence/{id}` wrapped a manifest the contract returns bare — the
one case where the server was wrong and the client right; `verifyDataViews`
sent no credentials and then treated any 200 as verified; Matrix's verify
sealed under a literal tenant `"verify"` that the evidence plane refuses;
and a contract's verified question expected three rows from a table its
principal is row-level-secured to one region of. Each is recorded where it
was found — §13.5 entries 22–23 here, the Matrix component's own record
there — and each closed with a test that names it.

## Appendices

### Appendix A: Environment variable quick reference

The server's env contract lives in **one place**: the
[README's Configuration table](../../README.md#configuration-env-vars).
This book does not duplicate it. Under Preface commitment 3, any change must
update that table in the same change.

The table below lists the **dev- and test-side** variables that the server
never reads. Each variable was verified at its source.

| Var | Read by | Default | Meaning |
|---|---|---|---|
| `MUNARIUM_TEST_DATABASE_URL` | [pg_integration.rs](../../src/munarium-store-pg/tests/pg_integration.rs), [collections_integration.rs](../../src/munarium-retrieval-pg/tests/collections_integration.rs); set/unset around the run by `test.ps1 -Postgres` | unset; the gated tests skip vacuously | Un-skips the pg integration tests against the compose Postgres (§3). You rarely set it by hand |
| `MUNARIUM_TEST_S3_ENDPOINT` | [s3_integration.rs](../../src/munarium-store-objects/tests/s3_integration.rs) | unset; skip vacuously | Opts into the MinIO object-store roundtrip (§3). No tier sets it, and CI never starts MinIO |
| `MUNARIUM_TEST_S3_BUCKET` / `_REGION` / `_ACCESS_KEY` / `_SECRET_KEY` | s3_integration.rs:31–36 | `sources` / `us-east-1` / `minioadmin` / `minioadmin` | Companions to the endpoint var. These are the compose MinIO throwaways |
| `MUNARIUM_TEST_PROVIDER_KEY` | [contract.rs](../../src/munarium-providers/tests/contract.rs):91 | set *by* the tests | **Not a live gate**. It is a fake credential that exercises the `CredentialRef::Env` resolution seam. The real live smokes (§3) gate on `MUNARIUM_LIVE_PROVIDER_TESTS=1` plus the family's `MUNARIUM_SECRET_*` key |
| `MUNARIUM_LIVE_PROVIDER_TESTS` | contract.rs live smokes | unset; the smokes skip vacuously | `1` arms the paid provider live smokes; each additionally requires its family's `MUNARIUM_SECRET_ANTHROPIC` / `_OPENAI` / `_OPENROUTER` key (§3). Never set in CI |
| `MUNARIUMCTL_URL` | [munarium-cli main.rs](../../src/munarium-cli/src/main.rs) | `http://localhost:8080` | Base URL for `mmctl` (§4, [ops/mmctl.md](../ops/mmctl.md)) |
| `MUNARIUMCTL_TOKEN` | main.rs | unset; requests go out with no bearer | Bearer token for `mmctl` calls |
| `MUNARIUMCTL_UID` | main.rs | `mmctl` | The uid header value (the uid contract, §5) |
| `MUNARIUM_LOG` | server main.rs:46–51; *owned by the README table* and repeated here for the syntax pointer | `info`; malformed falls back to `info`, the one deliberate fail-open | tracing's env filter in standard `target=level` comma syntax (`munarium_server=debug,sqlx=warn`). §11 has the full leverage discussion |

### Appendix B: Port map

The table below shows every port used in this book. §2 explains the two-world
rule: canonical ports for compose and deployed systems, with +10000 alternates
for anything booted from source.

The Introduction's debugging habit still applies: **if an example fails to
bind or connect, suspect the port before the code** (§2).

| Port(s) | World | Role |
|---|---|---|
| 443 | deployed environments | the gateway's public edge |
| 8080 | canonical (server default, compose) | REST + `/docs` + health (`MUNARIUM_HTTP_ADDR`) |
| 50051 | canonical | direct gRPC (`MUNARIUM_GRPC_ADDR`; literal `disabled` turns it off) |
| 9090 | canonical | ops: metrics + health, never exposed via ingress (`MUNARIUM_OPS_ADDR`) |
| 5433 | compose host | Postgres, mapped to container 5432. This is the one hardcoded mapping. Every script connection string reads `localhost:5433` |
| 9000 / 9001 | compose `--profile s3` | MinIO S3 API / console |
| 8443 | compose gateway default host port | **Windows-reserved** on many machines (WinNAT exclusion ranges; `netsh interface ipv4 show excludedportrange protocol=tcp`). This fact created the alternate world |
| 18080 / 15051 / 19090 / 18443 | +10000 alternates | from-source boots, the local scripts, and the black-box conformance tier (REST / gRPC / ops / gateway) |
| 18081 / 19091 | platform test tier | runs beside a black-box run so the two coexist (test.ps1:10–11) |
| 18085 / 19095 | this book's verification rigs | One line of honesty: chapters were verified on +5-offset rigs so two chapters' servers could coexist. §21 found one still live on 18085 and killed it by name |

Two house rules complete the picture. Host ports are compose interpolations
(`${MUNARIUM_HOST_HTTP:-8080}` and friends; the gateway via
`MUNARIUM_HOST_GATEWAY`). A machine with different conflicts remaps ports by
environment variable, not by editing files.

The alternate ports belong to the scripts. `gates.ps1` reaps **by identity,
never by port** (gates.ps1:88–107): a listener on 18080/15051/19090 (and on
the platform and cluster alternates) is stopped only if its process is
named `munarium-server`. Anything else makes the script throw with the
owner's pid and name. `test.ps1` stops only the servers it started itself,
by pid. Copy that pattern, never a bare `Stop-Process` (§2).

### Appendix C: Script inventory

Four `.ps1` files sit at `server/` root, and all four are real programs.
Line counts were measured for this edition:

| Script | Lines | What it is |
|---|---|---|
| [build.ps1](../../build.ps1) | 49 | Workspace build: `-Release`, `-Lint` (what CI enforces), `-Image` (the distroless musl image) (§3, §10) |
| [test.ps1](../../test.ps1) | 226 | The five-tier test ladder: offline / `-Postgres` / `-BlackBox` / `-Platform` / `-Cluster` / `-All` (§3) |
| [localdeploy.ps1](../../localdeploy.ps1) | 93 | Full stack on local Docker Desktop with persistent volumes, no cloud account; owner of the port-conflict header Appendix B quotes (§2, §3) |
| [gates.ps1](../../gates.ps1) | 431 | The local gate set — the same gates CI's lint-test + cargo-deny jobs run, against the same pg container; deploys nothing (§10) |

There is no deploy script. Deployment is `helm install` from the chart or
`terraform apply` on the example module (§10,
[ops/deployment-runbook.md](../ops/deployment-runbook.md)), and the image
is `build.ps1 -Image` plus a push to a registry you name. Every script
resolves paths from its own location (`$PSScriptRoot`), so each runs from
any working directory; `gates.ps1` is the one that reaps leftover
listeners, and it does so by identity (Appendix B).

### Appendix D: Glossary

The book's working vocabulary, alphabetical, each term defined the way the
chapters actually use it, with the section where it is taught.

- **blend**: the two-surface prompt: the composed canon brief first ("what
  we know"), document retrieval second ("what the documents say"). The
  ordering is the measured part. On the server, the application composes
  the blend; there is no blend backend (§18, §13.4).
- **BYOK**: bring-your-own-key. Model calls go from your environment
  directly to your provider accounts; the vendor is never in the data path
  (Introduction, §20).
- **canonical memory (canon)**: the vetted current state of a lineage. It
  contains accepted claims after supersession is resolved. The server serves
  it as the composed, budget-degraded brief (§18).
- **capability token**: a short-lived, tenant-scoped token your gateway
  mints per user, carrying clearance level, compartments, and scopes (§14).
- **claim**: one extracted `subject.key=value` assertion written to the
  ledger, where the gates accept or dispute it (§18).
- **clearance**: a caller's access level plus compartments. It filters
  retrieval *before* ranking and is snapshotted onto a session when created
  (§16, §17).
- **collection**: a clearance-scoped retrieval unit declared by a runbook.
  It has an access level, optional compartments, and source prefixes. It is
  built into immutable index versions (§15–§16).
- **compartment**: a category label beside levels, or "Bell-LaPadula 'simple
  security' with categories." A caller must hold every compartment a
  collection names (§4, §16).
- **conformance suite**: one scenario set run against the in-process store,
  Postgres, and both wire planes. It proves that REST and gRPC cannot drift
  apart (§7).
- **disputed**: the *success* status a write earns when a gate collides it
  with existing memory. The claim persists, a machine-readable finding is
  attached, and a human verdict is owed. It is never an error (Introduction,
  §18).
- **gate**: a deterministic write-time check on claims. Five are always on.
  The chronology gate exists in the kernel but has no arming surface yet.
  Gates file findings; they never make model calls (§18).
- **index version**: one immutable build of a collection's chunks. Cutover
  moves the active pointer. `retireOld` reclaims only inactive versions
  (§8, §16).
- **ingest (filename-as-identity)**: `POST /v1/ingest`. The logical path
  *is* the source's identity. The same path plus the same bytes replays
  idempotently. The same path plus new bytes is an update that requires a
  rebuild (§15).
- **interaction capture**: middleware records every `/v1` request into
  `interactions`. This makes who-asked-what a feature. Bodies are capped by
  `MUNARIUM_INTERACTION_BODY_MAX` (§20).
- **ledger**: the append-only event sequence behind memory. There is no
  `UPDATE` in the write path, at the spec level (Introduction, §18).
- **lineage**: a chain of memory versions sharing one monotonic sequence. It is
  walked by `GET /v1/versions/{id}/lineage` (Introduction, §18).
- **MMP**: the Munarium Protocol. [proto/mmp/v1/](../../proto/mmp/v1/)
  is the normative behavior spec. REST and gRPC are two transports over it
  (Introduction). The abbreviation `MMP` and the wire package `mmp.v1` predate
  the 2026-08-26 rename to Munarium and were deliberately left frozen: renaming
  the proto package would change every gRPC service full name and force all four
  clients to regenerate stubs and deploy in lockstep with the server, for no
  behavioral gain. Read `MMP` as an opaque legacy token — it no longer expands.
- **pin**: a point-in-time read. It is a (version, `as_of_seq`) pair that
  bounds facts, promises, and the brief. It gives the exact answer to "what
  did we know when" (§18).
- **promise**: an obligation registered under a stable key. It must
  survive until a later unit pays it off. Overdue promises surface as
  findings (§18).
- **provenance envelope**: the re-verification block on every search
  result: source content hashes, the index version that served it, the
  ledger event watermark (§16).
- **red-flag queue**: the review surface for contradictions:
  `facts?statuses=disputed`, each entry carrying both values, both sources,
  and the rule that caught it (§18).
- **runbook**: YAML in *your* repo, applied through the API. It declares
  collections (levels, compartments, prefixes) and drives the
  resolve → build → verify → approval-gated cutover → retire run (§15).
- **seq**: the single monotonic per-lineage sequence that every write draws
  from. "The state as of seq N" is a complete description (Introduction).
- **session / turn**: a session pins a runbook version and snapshots
  clearance when created. A turn is one grounded question with its evidence.
  Conversation state between turns is client-owned by design (§17).
- **streaming turn**: the same turn delivered as SSE — `progress` events
  at the real stage boundaries (retrieval per collection, merge, model,
  each paid completion with token counts, each verification pass), then
  exactly one `done` or `error`. Same `op_turn`, same persistence,
  forwarded live (interaction capture records it at end of stream with
  the terminal event's status); REST-only (§17, §13.5 entry 16).
- **shape**: YAML declaring how sources are chunked and indexed. Changing
  an extractor or shape invalidates existing index versions by design (§15).
- **source / blob / chunk**: the three lives of a document: the `sources`
  row (identity + content hash), the bytes in the object store
  (`storage_backend` + `blob_uri`), and the indexed slices inside an
  immutable index version (§8, §15).
- **supersession**: a correction is a *new* record naming what it replaces.
  The superseded record remains readable forever underneath (Introduction,
  §18).
- **tenant**: the isolation unit. Every token is tenant-scoped, and every
  row hangs off one (§14).
- **tier (fast / capable)**: the two model roles a runbook's `models:`
  block names instead of model ids. Each provider config or the built-in
  per-family table resolves a tier to a concrete model; `GET /v1/providers`
  discloses that resolution for free, `/healthai` probes it for tokens
  (§6, §11, §17).
- **truncation-aware retry**: the turn completion's one 4× re-ask when the
  provider's stop reason is `max_tokens`/`length` or the text is empty —
  the guard against reasoning-model tiers spending the 1,024-token
  ceiling on hidden reasoning. Token counts on the response sum both
  calls (§17).
- **uid contract**: every `/v1` call names the human or agent behind it via
  `X-Munarium-Uid` / `munarium-uid`, enforced by middleware, not handlers (§5,
  §14).

### Appendix E: Sample runbook index

The thirteen committed [runbooks/applications/](../../runbooks/applications/)
samples, and which modelling decision each one is there to demonstrate. Start
from the precedent closest to your *corpus shape*, not just your pattern.
[loading-corpora.md](loading-corpora.md) says what to put under each prefix.

| Runbook | §19 pattern(s) | The decision it demonstrates |
|---|---|---|
| customer-support | 1 Ask | One source system split into two collections at different exposure; binding by media type as well as prefix |
| due-diligence | 3 Red-flag review | A compartment per functional area — one runbook, many audiences, one index |
| financial-advisory | 1 Ask, 7 Long-horizon | PII as the compartment boundary, including documents needing two compartments at once |
| history-revolution | 1 Ask, 2 Chat | Sharding a large corpus by BYTES rather than document count, with stable hash assignment |
| insurance-claims | 7 Long-horizon | Loss type as the collection boundary, with one type escalated behind its own compartment |
| legal-appeal | 1 Ask | When *not* to compartmentalize, and how to say so in a runbook |
| legal-contracts | 1 Ask | A full corpus behind a clearance plus a level-0 smoke slice for cheap end-to-end verification |
| patent-analysis | 1 Ask | Privilege as a real three-level boundary, up to attorney work product alone at the top |
| regulatory-compliance | 1 Ask, 2 Chat | Two level-0 collections separated for retrieval reasons rather than governance reasons |
| support-knowledge | 1, 2, 3, 4 | A compartment model derived from source ownership; DOCX/PDF binding by content type |
| sweep-coverage | 6 Audit sweeps | Sharing one collection handle between two runbooks so the index is built once |
| sweep-v2 | 6 Audit sweeps | Two applications over one collection differing only in completion policy |
| threat-intelligence | 5 Entity-centric | The vendor feed as the compartment boundary, which is what makes aliasing visible |

### Appendix F: Route index

Every path the server serves, from the served OpenAPI document, with the
chapter that teaches it. **Generated** by `docs/route-index.py` (2026-09-02)
and **enforced**: the server crate's `docs_coverage` tests fail `cargo test`
when a served route is missing here or from [rest.md](../api/rest.md), or a
problem slug from [errors.md](../api/errors.md). Regenerate the table after
a route change; the chapter column is the script's prefix table, so a new
route family needs one rule there.

| Route | Methods | OpenAPI tag | Where this book teaches it |
|---|---|---|---|
| `/healthai` | GET | providers | §11 |
| `/healthz` | GET | meta | §5, §11 |
| `/readyz` | GET | meta | §5, §8A (datastore readiness), §11 |
| `/v1/access-tokens` | GET · POST | access-tokens | §20 |
| `/v1/access-tokens/{jti}/revoke` | POST | access-tokens | §20 |
| `/v1/authoring/drafts` | GET · POST | authoring | §21B |
| `/v1/authoring/drafts/{draft_id}` | GET · DELETE | authoring | §21B |
| `/v1/authoring/drafts/{draft_id}/answers` | PUT | authoring | §21B |
| `/v1/authoring/drafts/{draft_id}/apply` | POST | authoring | §21B |
| `/v1/authoring/drafts/{draft_id}/assist` | POST | authoring | §21B |
| `/v1/authoring/drafts/{draft_id}/export` | POST | authoring | §21B |
| `/v1/authoring/drafts/{draft_id}/validate` | POST | authoring | §21B |
| `/v1/authoring/patterns` | GET | authoring | §21B |
| `/v1/authoring/patterns/{id}` | GET | authoring | §21B |
| `/v1/chronology-rules` | POST | command | §18 |
| `/v1/chronology-rules/{name}` | GET | query | §18 |
| `/v1/claims/{claim_id}` | GET | query | §18 |
| `/v1/collections` | GET · POST | collections | §16 |
| `/v1/collections/{collection_id}/activate-index` | POST | index-artifacts | §8A |
| `/v1/collections/{id}` | GET | collections | §16 |
| `/v1/evidence` | POST | evidence | §21C |
| `/v1/evidence/{evidence_id}` | GET · DELETE | evidence | §21C |
| `/v1/evidence/{evidence_id}/accesses` | GET | evidence | §21C |
| `/v1/evidence/{evidence_id}/bytes` | PUT | evidence | §21C |
| `/v1/evidence/{evidence_id}/commit` | POST | evidence | §21C |
| `/v1/evidence/{evidence_id}/legal-hold` | POST | evidence | §21C |
| `/v1/evidence/{evidence_id}/rows` | GET | evidence | §21C |
| `/v1/index-artifacts/backfill` | POST | index-artifacts | §8A |
| `/v1/index-artifacts/{index_version_id}` | GET | index-artifacts | §8A |
| `/v1/index-artifacts/{index_version_id}/bind` | POST | index-artifacts | §8A |
| `/v1/index-artifacts/{index_version_id}/promote` | POST | index-artifacts | §8A |
| `/v1/index-artifacts/{index_version_id}/rebuild` | POST | index-artifacts | §8A |
| `/v1/index-artifacts/{index_version_id}/verify` | POST | index-artifacts | §8A |
| `/v1/index-build-jobs` | GET · POST | index-artifacts | §8A |
| `/v1/index-build-jobs/{job_id}` | GET | index-artifacts | §8A |
| `/v1/index-build-jobs/{job_id}/cancel` | POST | index-artifacts | §8A |
| `/v1/indexes/{shape_ref}` | GET | retrieval | §16 |
| `/v1/indexes/{shape_ref}/build` | POST | retrieval | §16 |
| `/v1/ingest` | POST | ingest | §15, §21 |
| `/v1/ingest/batch` | POST | ingest | §15, §21 |
| `/v1/ingest/bulk` | POST | ingest | §15, §21 |
| `/v1/ingest/bulk/{bulk_id}` | GET | ingest | §15, §21 |
| `/v1/ingest/bulk/{bulk_id}/chunk` | POST | ingest | §15, §21 |
| `/v1/ingest/bulk/{bulk_id}/complete` | POST | ingest | §15, §21 |
| `/v1/max-tokens` | GET · POST | providers | §20 (spend governance); docs/tokenbudgets.md |
| `/v1/providers` | GET · POST | providers | §11 (BYOK diagnostic), §17 |
| `/v1/providers/{name}/complete` | POST | providers | §11 (BYOK diagnostic), §17 |
| `/v1/providers/{name}/embed` | POST | providers | §11 (BYOK diagnostic), §17 |
| `/v1/providers/{name}/health` | GET | providers | §11 (BYOK diagnostic), §17 |
| `/v1/reports/audit` | GET | reports | §20; rest.md Reports rows |
| `/v1/reports/budgets` | GET | reports | §20 (spend governance) |
| `/v1/reports/cost` | GET | reports | §20; rest.md Reports rows |
| `/v1/reports/endpoints` | GET | reports | §20; rest.md Reports rows |
| `/v1/reports/evidence` | GET | reports | §20; rest.md Reports rows |
| `/v1/reports/matrix` | GET | reports | §20; rest.md Reports rows |
| `/v1/reports/runbooks` | GET | reports | §20; rest.md Reports rows |
| `/v1/reports/sessions` | GET | reports | §20; rest.md Reports rows |
| `/v1/reports/timeseries` | GET | reports | §20; rest.md Reports rows |
| `/v1/reports/usage` | GET | reports | §20; rest.md Reports rows |
| `/v1/retrieval-rollout` | PUT | retrieval-rollout | §8A |
| `/v1/retrieval-rollout/{scope_kind}/{scope_id}` | GET | retrieval-rollout | §8A |
| `/v1/runbooks` | GET · POST | runbooks | §21A |
| `/v1/runbooks/validate` | POST | runbooks | §21A |
| `/v1/runbooks/{name}` | GET | runbooks | §21A |
| `/v1/runbooks/{name}/remove-confirm` | POST | runbooks | §21A |
| `/v1/runbooks/{name}/remove-request` | POST | runbooks | §21A |
| `/v1/runbooks/{name}/runs` | POST | runbooks | §21A |
| `/v1/runbooks/{name}/sessions` | POST | sessions | §21A |
| `/v1/runs/{run_id}` | GET | runbooks | §21A |
| `/v1/runs/{run_id}/steps/{ordinal}/approve` | POST | runbooks | §21A |
| `/v1/search` | POST | retrieval | §16 |
| `/v1/sessions/{id}` | GET | sessions | §17 |
| `/v1/sessions/{id}/close` | POST | sessions | §17 |
| `/v1/sessions/{id}/turns` | POST | sessions | §17 |
| `/v1/sessions/{id}/turns/stream` | POST | sessions | §17 |
| `/v1/shapes` | POST | shapes | §16 |
| `/v1/sources` | PUT | ingest | §6, §15 |
| `/v1/sources/{source_id}` | GET | ingest | §6, §15 |
| `/v1/versions` | POST | command | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/anchors` | GET · POST | command, query | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/claims` | POST | command | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/context` | GET | query | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/counters` | GET · POST | command, query | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/digests` | GET · PUT | command, query | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/events` | POST | command | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/facts` | GET | query | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/findings` | GET · POST | command, query | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/head` | GET | query | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/ingests` | POST | ingest | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/lineage` | GET | query | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/promises` | GET · POST | command, query | §5, §6 (recipes), §18 |
| `/v1/versions/{version_id}/promises/{key}/fulfill` | POST | command | §5, §6 (recipes), §18 |
| `/version` | GET | meta | §5 |
