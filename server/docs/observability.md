# Observability in Munarium Server

## How a governed memory and evidence service reveals operational health and change over time

**Status:** Code-grounded reference

**As of:** September 2, 2026

## Executive answer

Munarium Server has a production observability system today, but its boundary
is different from that of a conventional agent-observability product.

Munarium Server is not an autonomous agent runtime. It does not accept an
open-ended goal, invent a plan, and expose a tree of internal reasoning and tool
spans. It is a governed memory and evidence service. It stores versioned claims,
retrieves evidence, executes declared and bounded runbooks, optionally invokes
models, and records whether new knowledge was accepted, disputed, superseded,
or refused. Its runbooks are versioned declarative pipelines whose step
transitions are persisted and whose cutovers can require human approval; see
the [runbook implementation](../src/munarium-runbooks/src/lib.rs).

An agentic application can use Munarium Server as its durable memory and
evidence layer. The application's observability platform remains responsible
for the outer trace—conversation flow, autonomous planning, application tools,
retries, and incident lifecycle—while Munarium contributes the deeper record
inside its boundary: correlated requests, model usage, evidence provenance,
runbook state, governance findings, and memory lineage.

That division of labor leads to a precise answer to “how do you know whether it
is performing better or worse over time?” Munarium Server measures improvement
as a vector rather than a single score:

- **service reliability:** availability, request volume, error classes,
  saturation, and load shedding;
- **service performance:** median and tail latency by time window and operation;
- **model efficiency:** provider outcomes, latency, token use, and remaining
  budget;
- **workflow health:** runbook and step outcomes, wall time, sessions, and
  turns;
- **evidence quality:** which evidence layers participated, refused, or could
  support completeness; and
- **memory integrity:** accepted versus disputed state, provenance,
  supersession, and governance findings.

Munarium Server exposes enough information to trend each dimension, correlate
an anomaly to its underlying evidence, and integrate the result into an
platform monitoring stack. It does not currently reduce them to a universal
semantic-quality score, and it should not: lower latency is not an improvement
if evidence completeness or memory integrity deteriorates.

## 1. The server's observability boundary

The central observable unit in Munarium Server is a governed state transition,
not a hidden chain of thought. The useful questions are concrete:

- What request entered the service, and which identity and session made it?
- Which runbook, collections, memory version, and evidence supported the work?
- Which provider and model ran, how long did it take, and how many tokens did
  it consume?
- Did the operation complete, fail, refuse, wait for approval, or shed load?
- Did a proposed claim enter canonical memory, become disputed, or supersede an
  earlier claim?
- Can an operator reconstruct that decision later without retaining secrets?

The server answers these questions with several records that overlap by design.
Process metrics are inexpensive and label-bounded. Structured logs preserve
diagnostic context. Interaction records support tenant-aware reporting.
Session-turn records preserve retrieval and completion evidence. The append-only
ledger and governance findings explain what the system ultimately accepted.

The boundary to keep in mind: an agentic application uses Munarium Server as
its governed memory and evidence layer, and platform observability spans
both systems. Munarium Server is the observable memory and evidence substrate
inside a larger application, not the application's autonomous agent runtime.

## 2. What the server observes

### 2.1 Service health and saturation

The internal operations plane exposes liveness, readiness, and Prometheus text
metrics. Readiness is not a static process check: it accounts for draining and
probes the backing store. The fixed metric registry includes:

- request counts and duration by protocol plane, route template, method, and
  status class;
- database-pool connection and idle counts;
- interaction-writer queue depth, dropped records, and insert failures;
- provider calls, outcomes, duration, and input/output tokens;
- runbook step transitions;
- load-shed events; and
- index-build outcomes, work volume, and phase duration.

The histograms use fixed buckets from milliseconds through the provider timeout
range. Gauges are read from live state when the metrics document is rendered.
The registry and cardinality rules are implemented in
[metrics.rs](../src/munarium-server/src/metrics.rs), and the health and
readiness behavior is implemented in
[ops.rs](../src/munarium-server/src/ops.rs).

Metric labels deliberately exclude tenant, user, and instance identifiers.
Routes are templates rather than raw object paths. This keeps the scrape target
bounded and avoids publishing tenant data through infrastructure telemetry.
The monitoring system assigns the instance identity; tenant-aware analysis
comes from authenticated reports over the shared database.

### 2.2 Correlated requests and structured logs

The server instruments REST and gRPC requests with tracing spans and can emit
structured JSON lines. Every REST response carries a generated request ID; that
same identifier appears in the request span and interaction row. The middleware
measures caller-observed latency, including authentication and request-body
capture, and records the terminal outcome of ordinary and streamed responses.
The implementation is in
[middleware.rs](../src/munarium-server/src/middleware.rs), with logging
configuration documented in the [server README](../README.md#configuration-env-vars).

The request ID is the main cross-system seam. An API gateway, application, or
agent runtime can attach it to its own trace. An operator can then move from an
outer application span to the exact Munarium interaction and from that
interaction to its session, runbook, retrieval evidence, provider use, and
governed memory state.

### 2.3 Interaction and session evidence

The server captures `/v1` interactions through a bounded asynchronous writer so
observability work cannot block the serving path. A persisted interaction can
include tenant, user, session, request ID, plane, operation, runbook,
collections, token identity, status, latency, instance, and bounded
request/response evidence. Oversized bodies are represented by a hash and byte
length. Responses carrying secret material are replaced with a redaction
marker. The behavior is defined in
[interactions.rs](../src/munarium-server/src/interactions.rs) and the
[interaction schema](../src/munarium-store-pg/migrations/0010_identity_interactions.sql).

Session turns add the evidence needed to understand a retrieval-backed answer:
the user's query, collections searched, retrieval hits, provenance envelope,
resolved model, generated text, and token usage. Sessions pin a runbook version
and an authorization snapshot, so later analysis is tied to the policy that was
actually active. See the [session schema](../src/munarium-store-pg/migrations/0013_sessions.sql).

This audit path has an intentional availability tradeoff. If its bounded queue
is saturated, the server drops the interaction rather than backpressuring user
traffic, increments a dropped-record counter, and emits a warning. The audit
trail is therefore best-effort under overload. Monitoring that drop counter is
part of monitoring the integrity of observability itself.

### 2.4 Model use and cost governance

Provider calls are guarded by rate and token budgets. Call counters separate
provider, resolved model, operation kind, and outcome; duration histograms
separate provider and operation kind; token counters separate provider, model,
and input/output direction. The tenant reports show token use by provider and
model as well as daily reservations, settled use, limits, and remaining
capacity.

When a direct provider request names a memory version, the server also appends
an invocation-provenance event to that lineage. It records the request hash,
provider, model, token counts, latency, and cache status without storing keys or
prompt bodies. That makes model use part of the governed evidence history, not
only an ephemeral metric. See
[providers_api.rs](../src/munarium-server/src/providers_api.rs).

The server reports token facts rather than maintaining a live provider-dollar
price book. That is deliberate: platform billing and provider-specific price
changes belong upstream. A broader cost platform can apply its own price model
to Munarium's provider/model/token dimensions.

The live AI-health diagnostic is not ordinary monitoring. It makes real bounded
calls to configured default models, spends provider tokens, and bypasses tenant
budgets. It is an authenticated operator probe for diagnosing connectivity, not
a free readiness check to poll continuously.

### 2.5 Workflow health

Munarium runbooks are deterministic orchestration, not free-running agency. A
runbook declares its sources, collections, retrieval and completion policy,
models, ordered maintenance steps, and approval requirements. The executor
persists every transition and can pause before a cutover. Deterministic
validation rejects structurally unsafe definitions before execution; see
[runbook validation](../src/munarium-runbooks/src/validate.rs).

The observability surface records runbook runs by state, average wall time,
step counts by state, and process-level step-transition counters. Session
reports add time-bucketed sessions opened, turns, and active users. These
signals reveal stuck approvals, failing steps, rising wall time, and changes in
actual use without treating the workflow as an autonomous agent trace.

### 2.6 Evidence quality

A successful request can still produce a thinner answer than the runbook
promised. For that reason, the server records how its evidence hierarchy
behaved on each turn. The evidence report aggregates, by research profile and
layer:

- turns observed;
- refusals;
- whether the layer could support a completeness claim;
- refusal codes, ordered by frequency; and
- median and tail duration.

This is one of the most important semantic signals in the server. A layer that
quietly refuses on most turns may be unavailable or misconfigured while every
request still returns a superficially successful status. The report makes that
degradation visible. Its calculation is in
[reports_api.rs](../src/munarium-server/src/reports_api.rs), and its data
contract is in
[the evidence report types](../src/munarium-api-types/src/lib.rs).

### 2.7 Memory integrity and governance

Ordinary APM can show that a write returned successfully; it cannot show whether
the write should have become truth. Munarium's command path evaluates new claims
against deterministic governance gates. A blocked claim is retained as
disputed rather than silently discarded. Corrections append new claims and
supersession links rather than rewriting history. Findings record the governing
rule, severity, message, scope, sequence, and detail. Point-in-time reads can
reconstruct what was knowable at an earlier sequence.

This gives operators a semantic audit trail:

- provenance answers where a claim came from;
- status answers whether it was accepted or disputed;
- supersession answers which later claim replaced it;
- findings answer which invariant was threatened; and
- version and sequence pins answer what a caller could have observed at that
  moment.

The PostgreSQL write path and finding persistence are implemented in
[the store](../src/munarium-store-pg/src/lib.rs), with finding storage
defined by the [gate-finding migration](../src/munarium-store-pg/migrations/0017_gate_findings.sql).

## 3. The operator views

Munarium Server has two complementary reporting surfaces.

The built-in management console is server-rendered HTML with inline SVG and no
client-side JavaScript. Its monitoring views cover overview, traffic,
endpoints, usage, providers, runbooks, sessions, audit, findings, and health.
The same authenticated report functions are available as structured data for
another monitoring system. The console is implemented in
[the dashboard module](../src/munarium-server/src/dashboard/mod.rs).

The persistent reports aggregate across every server instance sharing the
tenant database. They provide:

| View | Current server measurement |
|---|---|
| Traffic | Time-bucketed request volume, client/server errors, and p50/p95 latency |
| Endpoints | Volume, error rate, average latency, and p95 latency by operation |
| Usage | Interactions, turns, tokens, and average latency grouped by user, session, runbook, or collection |
| Providers and budgets | Provider/model tokens, overrides, reserved and settled use, limits, and remaining capacity |
| Runbooks | Run counts by state, average wall time, and step counts by state |
| Sessions | Sessions opened, turns, and active users over time |
| Evidence | Layer participation, refusal, completeness support, refusal causes, and p50/p95 duration |
| Audit and findings | Correlated interaction envelopes, optional bounded bodies, and recent governance findings |

Traffic, operation, runbook, session, and evidence views accept bounded windows
of one hour, one day, seven days, or thirty days. Usage and token reports accept
explicit time bounds. The queries run over persisted tenant records rather than
one process's memory, so a cluster reads as one logical service.

## 4. How to decide whether the server is improving

No single signal can answer this honestly. The useful decision is a scorecard
across five questions.

### 4.1 Is it more reliable?

Compare request rate with client and server error counts, readiness failures,
load shedding, database-pool pressure, and audit-writer health. Improvement
means equal or greater useful traffic with stable or lower failure and
saturation signals. A lower error rate caused only by lower traffic is not, by
itself, improvement.

### 4.2 Is it faster?

Compare median and p95 request latency over equivalent windows and operations.
Then separate provider duration, evidence-layer duration, runbook wall time,
and index-build phases to locate the cause. Improvement means the relevant tail
latency falls without moving work into refusals, load shedding, or incomplete
evidence.

### 4.3 Is model use more efficient?

Compare provider outcomes, duration, and input/output tokens for the same model
and workload. Check reserved and settled daily usage and remaining budget.
Improvement may mean fewer tokens for the same supported answer, fewer failed
calls, or less budget held in incomplete work. A model switch must be kept
visible as a changed independent variable rather than blended into a single
aggregate.

### 4.4 Are workflows and evidence healthier?

Compare runbook versions, run and step state distributions, wall time, session
activity, evidence-layer refusals, refusal codes, and availability of
completeness support. Improvement means fewer failed or stalled steps and fewer
unexpected evidence refusals while serving the intended workload. A faster
answer with a collapsed evidence layer is a regression.

### 4.5 Is memory becoming more trustworthy?

Track governance finding counts and severities, disputed claims, supersession
patterns, and provenance completeness in the context of write volume. More
findings can mean the source data worsened, the policy became stricter, or the
system caught more issues; counts require denominator and context. The durable
ledger lets an operator inspect the actual claim sequence instead of guessing
from the metric alone.

The practical rule is:

> A release is better when the targeted dimensions improve, the protected
> dimensions remain within their guardrails, and the underlying evidence can
> explain the change.

## 5. Integration with an platform observability platform

Munarium Server is designed to join a larger monitoring platform without
requiring that platform to adopt Munarium as its general trace store. One
workable result is an application-layer dashboard that combines application
traffic and release context with Munarium's reliability, evidence, governance,
workflow, token, and audit signals — vendor-neutral, because every signal
below is a plain text or JSON surface.

The integration pattern is straightforward:

1. **Scrape process telemetry.** Send the internal Prometheus-compatible metric
   stream to the cluster or platform metrics backend. The Helm chart includes
   optional scrape-discovery annotations; see
   [the chart values](../deploy/helm/munarium/values.yaml).
2. **Ingest structured logs.** Enable JSON-line logging and collect stdout with
   the deployment platform's normal log agent.
3. **Propagate correlation.** Record the Munarium request ID on the calling
   application's trace or gateway access log, then use it to join into the
   tenant audit trail.
4. **Import domain signals.** Poll or query authenticated reports for
   tenant-aware usage, budget, workflow, evidence, and finding signals that do
   not belong in public metric labels.
5. **Build alerts at the right layer.** Infrastructure alerts should watch
   readiness, errors, tail latency, load shedding, pool pressure, and audit
   drops. Application-quality alerts should watch evidence refusals,
   completeness support, runbook failures, and governance findings.
6. **Retain governed evidence.** Keep the outer distributed trace in the APM
   system and the memory/evidence decision in Munarium. The correlation ID makes
   them one investigable incident without duplicating the full ledger into a
   span store.

The seams are portable: Prometheus text, JSON logs, authenticated JSON
reports, and request correlation are not tied to one cloud or to one
platform's log agent.

Native OpenTelemetry export is not implemented today, and no Grafana dashboard
bundle ships with the server. An external platform can still integrate the
present surfaces immediately. OTLP would be an adapter that simplifies span
transport, not a prerequisite for collecting Munarium's operational and
domain-specific evidence.

## 6. Honest limits of the current server

Munarium Server's observability is substantial, but several boundaries matter
when interpreting “better or worse”:

- It does not ingest or reconstruct arbitrary agent spans. It observes work
  inside the Munarium boundary.
- It does not compute a universal semantic-correctness score for ordinary
  production answers. Evidence completeness, refusals, provenance, and
  governance are observable proxies and controls, not a ground-truth label.
- It does not provide an automatic release-to-release statistical comparison.
  Operators or an external observability platform must compare equivalent time
  windows, workloads, runbook versions, and model configurations.
- Process metrics live in memory and restart with the process. Long-term metric
  history belongs in the scraper's backend.
- Persistent traffic reports cover windows up to thirty days. Longer retention
  and rollups are deployment policy rather than a built-in time-series store.
- The interaction audit writer is best-effort under saturation. Dropped and
  failed writes are observable but cannot reconstruct records that were never
  persisted.
- Provider cost reporting is token-based, not dollar-based.
- The live AI connectivity diagnostic spends real provider tokens and should
  not be used as an ordinary polling monitor.
- Tenant-aware signals are intentionally kept out of infrastructure metric
  labels; they require management authorization through the reports and audit
  surfaces.

These are not reasons to call the server unobservable. They define the right
architecture: Munarium owns durable memory, evidence, and governance facts;
the platform platform owns long-term telemetry retention, distributed traces,
alerts, incident workflow, and cross-service analysis.

## 7. Recommended production scorecard

A small scorecard is enough to make the improvement claim testable:

| Dimension | Compare before and after | Guardrail |
|---|---|---|
| Reliability | Request volume, server errors, load shed, readiness | No hidden traffic collapse; no audit-drop increase |
| Latency | Request and evidence-layer p50/p95, provider duration, runbook wall time | Do not trade latency for refusal or incomplete evidence |
| Efficiency | Provider outcomes and tokens, settled/held budget | Compare equivalent workload and model configuration |
| Workflow | Run/step states and wall time by runbook version | Approval waits separated from failures |
| Evidence | Layer participation, refusals, refusal causes, completeness support | Successful status is insufficient when evidence thins |
| Memory integrity | Findings by severity, disputed claims, supersessions, provenance | Interpret counts relative to write volume and policy changes |

Before a rollout, record the active server build, runbook version, provider/model
configuration, and comparison window. During rollout, keep the earlier and later
cohorts distinguishable in the surrounding monitoring system. After rollout,
inspect both the aggregate shift and correlated examples from the audit and
ledger. That prevents a configuration change, traffic-mix change, or stricter
policy from being misreported as a model improvement or regression.

## Conclusion

Munarium Server is not agentic AI in the traditional sense. It is the governed
memory and evidence substrate that an agentic application can call. Its
observability is consequently strongest where ordinary agent tracing is
weakest: what evidence was used, which version of memory was visible, what the
system accepted or disputed, which policy fired, what model work was attributable
to that lineage, and how the decision can be reconstructed later.

The server knows whether its operation is improving by tracking reliability,
tail latency, provider efficiency, workflow state, evidence completeness, and
memory integrity over comparable periods. It exposes those facts through
bounded Prometheus metrics, correlated structured logs, persistent interaction
and session records, authenticated reports, a management console, and the
append-only ledger itself.

Those surfaces are designed to complement the observability platform already
surrounding an application or platform cloud deployment. The external system
can own end-to-end agent traces and long-term telemetry; Munarium Server supplies
the durable evidence needed to answer the more consequential questions: not
only *what ran*, but *what the application remembered, why it trusted it, and
whether that trust became more or less defensible over time*.
