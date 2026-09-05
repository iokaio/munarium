# Guided authoring: pattern → draft → validate → export → hosted

Writing a production runbook + shape set from scratch means internalizing a
design guide. The `authoring` plane packages that guide as a service: start
from a measured **pattern**, answer an **interview**, let deterministic
**validation** (and optional AI **assist**) drive the YAML to green, then
**export** a hash-manifested bundle or **apply** it directly. Eleven
methods over nine routes, REST-only (no authoring RPCs exist — the gRPC
clients raise the typed `Unsupported` error).

One structural note up front: `delete_draft` is the entire client surface's
**one DELETE** — it soft-removes a *workspace draft*, never ledger data, so
the append-only invariant is untouched. Everything else here is drafting.

## The pattern catalog

Patterns are the server's committed application archetypes: each carries a
description, the exemplar runbook + shape YAML to start from, design notes
the deterministic validator cannot police, and **guidance** on what the
pattern is strongest at and the failure mode to design against — so choosing
a pattern is choosing a worked precedent, not a blank page.

**Rust**
```rust
let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("author"))?;
for p in client.authoring.list_patterns().await?.patterns {
    println!("{}: {} — {}", p.id, p.name, p.guidance);
}
let detail = client.authoring.get_pattern("support-knowledge").await?;
println!("{}", detail.runbook_yaml);   // the exemplar, verbatim
```

**Python**
```python
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="author"))
for p in client.authoring.list_patterns():
    print(f"{p.id}: {p.name} — {p.guidance}")
detail = client.authoring.get_pattern("support-knowledge")
```

**.NET**
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "author" });
foreach (var p in await client.Authoring.ListPatternsAsync())
    Console.WriteLine($"{p.Id}: {p.Name} — {p.Guidance}");
var detail = await client.Authoring.GetPatternAsync("support-knowledge");
```

**Java**
```java
for (var p : client.authoring.listPatterns().patterns()) {
    System.out.println(p.id() + ": " + p.name() + " — " + p.guidance());
}
var detail = client.authoring.getPattern("support-knowledge");
```

## Draft → answers → validate

A draft holds an interview (sections of typed questions, each documenting
the slot it fills), the stored answers, and the **materialized documents**
(shapes + runbook) those answers produce. `put_answers` replaces the
answers and by default re-materializes; pass `materialize: false` to store
answers without touching documents (a seeded or assist-edited draft you
don't want overwritten). `validate` is deterministic and free: per-document
findings, cross-document `set.*` findings, and `todos` — red TODOs are
*expected* on a fresh draft; the loop below is how they drain.

**Rust**
```rust
let draft = client.authoring.create_draft(dto::CreateDraftRequest {
    name: "hr-handbook".into(),
    pattern_id: Some("support-knowledge".into()),
    seed_from_exemplar: false,
}).await?;

let draft = client.authoring.put_answers(&draft.draft_id,
    dto::UpdateAnswersRequest { answers, materialize: true }).await?;

let v = client.authoring.validate(&draft.draft_id).await?;
println!("valid: {} (todos: {})", v.valid, v.todos.len());
```

**Python**
```python
draft = client.authoring.create_draft(
    name="hr-handbook", pattern_id="support-knowledge")

draft = client.authoring.put_answers(draft.draft_id, answers)  # materialize=True

v = client.authoring.validate(draft.draft_id)
print("valid:", v.valid, "todos:", len(v.todos))
```

**.NET**
```csharp
var draft = await client.Authoring.CreateDraftAsync(
    "hr-handbook", patternId: "support-knowledge");

draft = await client.Authoring.PutAnswersAsync(draft.DraftId, answers);

var v = await client.Authoring.ValidateAsync(draft.DraftId);
Console.WriteLine($"valid: {v.Valid} (todos: {v.Todos.Count})");
```

**Java**
```java
var draft = client.authoring.createDraft(
        Authoring.CreateDraftRequest.of("hr-handbook", "support-knowledge"));

draft = client.authoring.putAnswers(draft.draftId(), answers, true);

var v = client.authoring.validate(draft.draftId());
System.out.println("valid: " + v.valid() + " (todos: " + v.todos().size() + ")");
```

## Assist: degrades, never fails

`assist` runs an AI drafting pass over the documents (BYOK provider —
`default` engages the tenant fallback chain, or name a config/model/tier).
Its contract is the one to design around: **assist NEVER fails the
request.** A keyless tenant, an exhausted budget, or an unparseable model
response returns HTTP 200 with the documents **unchanged** and
`assist_note` explaining the degradation — so an authoring UI can offer the
button unconditionally, and the deterministic validation in the same
response tells you whether the pass actually helped.

**Rust**
```rust
let out = client.authoring.assist(&draft.draft_id, dto::AssistDraftRequest {
    instructions: Some("split the finance area into AP and AR".into()),
    ..Default::default()
}).await?;
if let Some(note) = &out.assist_note { println!("degraded: {note}"); }
```

**Python**
```python
out = client.authoring.assist(
    draft.draft_id, instructions="split the finance area into AP and AR")
if out.assist_note:
    print("degraded:", out.assist_note)
```

**.NET**
```csharp
var outp = await client.Authoring.AssistAsync(
    draft.DraftId, instructions: "split the finance area into AP and AR");
if (outp.AssistNote is { } note) Console.WriteLine($"degraded: {note}");
```

**Java**
```java
var out = client.authoring.assist(draft.draftId(),
        new Authoring.AssistRequest(null, "split the finance area into AP and AR",
                null, null, null));
if (out.assistNote() != null) System.out.println("degraded: " + out.assistNote());
```

## Export: the hash-manifested bundle — verify it yourself

`export` returns a self-contained bundle: every YAML verbatim (`files`),
per-file sha256 (`hashes`), the `apply_order` (shapes before runbooks), and
a `manifest_hash` = sha256 over the byte-sorted `path\0hash\n` lines. The
bundle is designed to cross machines and reviews, so **verify it
client-side** on receipt — the server states the algorithm precisely so
that any holder can check integrity without trusting the channel:

**Rust**
```rust
use sha2::{Digest, Sha256};

let bundle = client.authoring.export(&draft.draft_id).await?;
let mut buf = String::new();
for (path, hash) in &bundle.hashes {          // BTreeMap: already byte-sorted
    buf.push_str(path); buf.push('\0'); buf.push_str(hash); buf.push('\n');
}
assert_eq!(hex::encode(Sha256::digest(buf.as_bytes())), bundle.manifest_hash);
for (path, yaml) in &bundle.files {           // and each file against its hash
    assert_eq!(hex::encode(Sha256::digest(yaml.as_bytes())), bundle.hashes[path].as_str());
}
```

**Python**
```python
import hashlib

bundle = client.authoring.export(draft.draft_id)
lines = "".join(f"{p}\0{h}\n" for p, h in sorted(bundle.hashes.items()))
assert hashlib.sha256(lines.encode()).hexdigest() == bundle.manifest_hash
for path, yaml_text in bundle.files.items():
    assert hashlib.sha256(yaml_text.encode()).hexdigest() == bundle.hashes[path]
```

**.NET**
```csharp
using System.Security.Cryptography;

var bundle = await client.Authoring.ExportAsync(draft.DraftId);
var lines = string.Concat(bundle.Hashes
    .OrderBy(kv => kv.Key, StringComparer.Ordinal)
    .Select(kv => $"{kv.Key}\0{kv.Value}\n"));
var hex = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(lines)));
if (hex != bundle.ManifestHash) throw new InvalidOperationException("tampered bundle");
```

**Java**
```java
var bundle = client.authoring.export(draft.draftId());
var buf = new StringBuilder();
new TreeMap<>(bundle.hashes())
        .forEach((p, h) -> buf.append(p).append('\0').append(h).append('\n'));
var digest = MessageDigest.getInstance("SHA-256")
        .digest(buf.toString().getBytes(StandardCharsets.UTF_8));
if (!HexFormat.of().formatHex(digest).equals(bundle.manifestHash())) {
    throw new IllegalStateException("tampered bundle");
}
```

(Bundle paths are ASCII by construction — draft names match
`^[a-z0-9][a-z0-9-]*$` — so each language's default string sort IS byte
order here.)

## Apply, and what "hosted" means

`apply` pushes the draft's documents to THIS server, validating inline
first, in `apply_order` — shapes before the runbook that references them.
The response lists each applied doc with its resulting `shape_ref` /
`runbook_ref` (name@version) and YAML hash. From that moment the runbook is
**hosted** like any hand-applied one: it shows in `runbooks.list()` /
`get_info`, its collections bind ingested sources, and
`sessions.create(name)` opens on it — the authoring plane has no special
runtime, it just writes the same surface you could have written by hand.

**Rust**
```rust
let applied = client.authoring.apply(&draft.draft_id).await?;
for d in &applied.applied { println!("{} -> {}", d.path, d.r#ref); }
let info = client.runbooks.get_info("hr-handbook").await?; // now hosted
```

**Python**
```python
applied = client.authoring.apply(draft.draft_id)
for d in applied.applied:
    print(d.path, "->", d.ref)
info = client.runbooks.get_info("hr-handbook")   # now hosted
```

**.NET**
```csharp
var applied = await client.Authoring.ApplyAsync(draft.DraftId);
foreach (var d in applied) Console.WriteLine($"{d.Path} -> {d.Ref}");
var info = await client.Runbooks.GetInfoAsync("hr-handbook");  // now hosted
```

**Java**
```java
var applied = client.authoring.apply(draft.draftId());
for (var d : applied.applied()) System.out.println(d.path() + " -> " + d.ref());
var info = client.runbooks.getInfo("hr-handbook");   // now hosted
```

## Cleaning up: the one DELETE

**Rust**
```rust
let gone = client.authoring.delete_draft(&draft.draft_id).await?;
assert_eq!(gone.status, "deleted");   // soft — the row is retained
```

**Python**
```python
gone = client.authoring.delete_draft(draft.draft_id)   # workspace-only, soft
```

**.NET**
```csharp
var gone = await client.Authoring.DeleteDraftAsync(draft.DraftId);
```

**Java**
```java
var gone = client.authoring.deleteDraft(draft.draftId());
```

Notes:

- A draft with error-severity findings refuses both `export` and `apply`
  with the typed `authoring-draft-invalid` error (409) — validation is not
  advisory, and an invalid set can neither leave nor land.
- Draft `state` (`interview | drafted | validated | exported`) is progress
  display only: export and apply always re-validate inline, so a stale
  state label can never smuggle an invalid document through.
- `seed_from_exemplar: true` copies the pattern's exemplar documents into
  the draft (renamed) instead of starting from interview materialization —
  the "edit a working example" path.
