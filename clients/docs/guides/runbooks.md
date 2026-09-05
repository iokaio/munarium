# Shapes and runbooks: declarative YAML, checkpointed steps, human approval

**Shapes** are JSON-Schema gates for claims (`kind: Shape` YAML). Applying
one with a `version_id` records the publication as a witnessed ledger claim
— the lineage explains why gating changed. A claim violating its shape draws
a `shape.schema-violation` **block** finding and is recorded disputed.

**Runbooks** (`kind: Runbook`) are checkpointed step machines — every step
transition, retry, and approval is itself a ledger event when the run names
a version. Runs pause at `approval: required` gates. Requires the postgres
store.

**Rust**
```rust
let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
client.runbooks.apply_shape(shape_yaml, Some(&v)).await?;    // event_id returned
client.runbooks.apply_runbook(runbook_yaml).await?;
let run = client.runbooks.run_runbook("tickets-reindex", Some(&v)).await?;
if run.state == "awaiting_approval" {
    let status = client.runbooks.get_run(&run.run_id).await?;
    let gate = status.steps.iter().find(|s| s.state == "awaiting_approval").unwrap();
    let done = client.runbooks.approve_step(&run.run_id, gate.ordinal).await?;
    assert_eq!(done.state, "done");
}
```

**Python**
```python
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
client.runbooks.apply_shape(shape_yaml, version_id=v)
client.runbooks.apply_runbook(runbook_yaml)
run = client.runbooks.run_runbook("tickets-reindex", version_id=v)
if run.state == "awaiting_approval":
    status = client.runbooks.get_run(run.run_id)
    gate = next(s for s in status.steps if s.state == "awaiting_approval")
    done = client.runbooks.approve_step(run.run_id, gate.ordinal)
```

**.NET**
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
await client.Runbooks.ApplyShapeAsync(shapeYaml, v);
await client.Runbooks.ApplyRunbookAsync(runbookYaml);
var run = await client.Runbooks.RunRunbookAsync("tickets-reindex", v);
if (run.State == "awaiting_approval")
{
    var status = await client.Runbooks.GetRunAsync(run.RunId);
    var gate = status.Steps.First(s => s.State == "awaiting_approval");
    var done = await client.Runbooks.ApproveStepAsync(run.RunId, gate.Ordinal);
}
```

**Java**
```java
client.runbooks.applyShape(shapeYaml, v);      // eventId returned
client.runbooks.applyRunbook(runbookYaml);
var run = client.runbooks.runRunbook("tickets-reindex", v);
if ("awaiting_approval".equals(run.state())) {
    var status = client.runbooks.getRun(run.runId());
    var gate = status.steps().stream()
            .filter(s -> "awaiting_approval".equals(s.state()))
            .findFirst().orElseThrow();
    var done = client.runbooks.approveStep(run.runId(), gate.ordinal());
}
```

Reference YAML lives in the server tree:
[shapes/support-tickets.yaml](../../../server/runbooks/shapes/support-tickets.yaml),
[pipelines/tickets-reindex.yaml](../../../server/runbooks/pipelines/tickets-reindex.yaml)
(resolveSources → buildIndex → verify → cutover `approval: required` →
retireOld).

Notes:

- Step states: `pending | running | awaiting_approval | done | failed`; run
  states: `running | awaiting_approval | done | failed`.
- `RunStatus.version_id` rides both transports since C5 (the proto gained
  the field) — it names the lineage every step transition was evented into.
- Shape/runbook application takes no idempotency key (documented un-keyed
  scope); re-applying identical YAML is a no-op upsert.
