# Tokens and reports: the management plane

Two credentials exist in this system, and keeping them straight is most of
the guide:

- **Static tokens** authenticate *services* and carry a role:
  `rw` (full data plane), `ro` (reads only — writes like `sessions.close`
  are refused), or `mgmt` (the management plane: token minting, revocation,
  and every report — and NOT the data plane, so a leaked mgmt token cannot
  read your corpus).
- **Capability JWTs** authenticate *end users*, minted by your API manager
  through the `tokens` plane. Short-lived (TTL clamped to a 24 h ceiling),
  scoped (`query` and/or `ingest`), bounded by an access level +
  need-to-know compartments, and optionally pinned to a runbook allowlist.

The `tokens` plane rides both transports (the gRPC twin is AdminService's
served trio); the `reports` plane is REST-only — the gRPC clients raise the
typed `Unsupported` error on every method.

## Minting a capability JWT (mgmt role)

The mint call is the trust handoff: YOU authenticated the end user, the
server signs what that user may do. The token material is returned **once**
and never persisted server-side — treat it as a secret; only its metadata
(jti, uid, scopes, expiry) enters the audit.

**Rust**
```rust
let mgmt = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devmgmt").uid("admin"))?;
let grant = mgmt.tokens.mint(dto::IssueTokenRequest {
    uid: "user-1".into(),               // becomes the JWT `sub`
    access_level: 2,
    compartments: vec!["finance".into()],
    scopes: vec!["query".into()],       // "query" and/or "ingest"
    runbook_refs: None,                 // None = any runbook the level permits
    ttl_secs: Some(3600),               // clamped to the 24 h ceiling
}).await?;
println!("jti {} expires {}", grant.jti, grant.expires_at);
```

**Python**
```python
mgmt = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devmgmt", uid="admin"))
grant = mgmt.tokens.mint(
    uid="user-1", access_level=2, compartments=["finance"],
    scopes=["query"], ttl_secs=3600)
print(grant.jti, grant.expires_at)
```

**.NET**
```csharp
await using var mgmt = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devmgmt", Uid = "admin" });
var grant = await mgmt.Tokens.MintAsync(
    "user-1", accessLevel: 2, scopes: ["query"],
    compartments: ["finance"], ttlSecs: 3600);
Console.WriteLine($"jti {grant.Jti} expires {grant.ExpiresAt}");
```

**Java**
```java
var grant = mgmt.tokens.mint(
        Tokens.IssueTokenRequest.of("user-1", 2, List.of("query"))
                .withCompartments(List.of("finance")));
System.out.println("jti " + grant.jti() + " expires " + grant.expiresAt());
```

## Using the minted token: the uid contract

A capability JWT is an ordinary bearer to the client libraries — construct
a data-plane client with it. One rule binds them: the `uid` you set **must
equal the token's `sub`**. A mismatch draws the typed `uid-mismatch` error
(Forbidden family) — the uid is not a courtesy header, it is the audit
identity every interaction is attributed to, and the token asserts whose it
is.

**Rust**
```rust
let user = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080")
        .token(&grant.token).uid("user-1"))?;   // uid == the JWT sub
let session = user.sessions.create("ent-support").await?;
```

**Python**
```python
user = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token=grant.token, uid="user-1"))
session = user.sessions.create("ent-support")
```

**.NET**
```csharp
await using var user = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = grant.Token, Uid = "user-1" });
var session = await user.Sessions.CreateAsync("ent-support");
```

**Java**
```java
try (var user = MunariumClient.rest(
        MunariumClientOptions.of("http://127.0.0.1:8080")
                .withToken(grant.token()).withUid("user-1"))) {
    var session = user.sessions.create("ent-support");
}
```

What the token's claims then do: `query` scope opens sessions and searches;
`ingest` scope feeds the file plane ([ingest-v2.md](ingest-v2.md)); the
access level + compartments filter which collections every operation can
see (the session-create response echoes the permitted set); a
`runbook_refs` allowlist confines the user to named runbooks.

## The issuance audit + revoke

`list` returns metadata only — never token material. `revoke` deny-lists a
token by `jti`; note `revocation_check_enabled` in the response — the
deny-list is only consulted when the server runs with
`MUNARIUM_TOKEN_REVOCATION_CHECK=true`, and the clients surface that fact
rather than letting you assume a revocation bites when it doesn't.

**Rust**
```rust
let issued = mgmt.tokens.list(TokenListQuery {
    uid: Some("user-1".into()), active: Some(true) }).await?;
let r = mgmt.tokens.revoke(&grant.jti).await?;
println!("revoked: {} (checked: {})", r.revoked, r.revocation_check_enabled);
```

**Python**
```python
issued = mgmt.tokens.list(uid="user-1", active=True)
r = mgmt.tokens.revoke(grant.jti)
print(r.revoked, r.revocation_check_enabled)
```

**.NET**
```csharp
var issued = await mgmt.Tokens.ListAsync(uid: "user-1", active: true);
var r = await mgmt.Tokens.RevokeAsync(grant.Jti);
Console.WriteLine($"revoked: {r.Revoked} (checked: {r.RevocationCheckEnabled})");
```

**Java**
```java
var issued = mgmt.tokens.list(Params.TokenListQuery.forUid("user-1"));
var r = mgmt.tokens.revoke(grant.jti());
System.out.println("revoked: " + r.revoked()
        + " (checked: " + r.revocationCheckEnabled() + ")");
```

## The seven reports (mgmt role, REST-only)

All seven read the same interactions audit trail the servers write on every
call, so a multi-replica cluster reads as one series. What each answers:

| Report | The question it answers |
|---|---|
| `usage` | Who/what is consuming the system — interactions, turns, and completion token spend grouped by `uid`, `session`, `runbook`, or `collection`, over an RFC 3339 window. |
| `audit` | What exactly happened — the per-interaction trail (uid, plane, method, status, latency, token jti), keyset-paged via `before` = the previous page's `next_before`; `bodies` opts into the captured request/response payloads (heavy). |
| `cost` | What the models cost — token totals per resolved provider/model, with override-chosen turns split out (dollar pricing lives upstream; the server reports the token facts). |
| `timeseries` | Is the service healthy — bucketed requests / 4xx / 5xx / p50 / p95 per window (`1h` \| `24h` \| `7d` \| `30d`), optionally filtered to one plane (`rest` \| `grpc`). |
| `endpoints` | Where the traffic and errors concentrate — per-endpoint request counts, error rate, and latency. |
| `runbooks` | How the step machines are doing — run counts by state with mean wall time, plus step-state counts. |
| `sessions` | Is anyone out there — sessions opened, turns taken, and distinct active uids per bucket. |

**Rust**
```rust
let usage = mgmt.reports.usage(UsageQuery {
    group_by: Some("runbook".into()), ..Default::default() }).await?;
let page = mgmt.reports.audit(AuditQuery {
    uid: Some("user-1".into()), ..Default::default() }).await?;
let next = page.next_before;   // pass back verbatim for the older page
let ts = mgmt.reports.timeseries(Some("24h"), None).await?;
```

**Python**
```python
usage = mgmt.reports.usage(group_by="runbook")
page = mgmt.reports.audit(uid="user-1")
older = mgmt.reports.audit(uid="user-1", before=page.next_before)
ts = mgmt.reports.timeseries(window="24h")
```

**.NET**
```csharp
var usage = await mgmt.Reports.UsageAsync(groupBy: "runbook");
var page = await mgmt.Reports.AuditAsync(uid: "user-1");
var older = await mgmt.Reports.AuditAsync(uid: "user-1", before: page.NextBefore);
var ts = await mgmt.Reports.TimeseriesAsync(window: "24h");
```

**Java**
```java
var usage = mgmt.reports.usage(new Params.UsageQuery("runbook", null, null));
var page = mgmt.reports.audit(Params.AuditQuery.forUid("user-1"));
var ts = mgmt.reports.timeseries("24h", null);
```

Notes:

- Role partition is enforced, not advisory: a data-plane (`rw`) token
  calling mint or a report draws the typed Forbidden error, and a `mgmt`
  token cannot take data-plane calls — the conformance suites prove both
  directions.
- gRPC sentinel note: an explicit `ttl_secs: 0` cannot ride the proto3
  wire and is rejected with a typed invalid-input error (as with every
  zero sentinel, omit it or use REST). The same holds for an explicit
  `runbook_refs: []`: REST reads it as "no runbook allowed", but a proto3
  empty repeated field is indistinguishable from absent, which the server
  reads as "any runbook" — so the gRPC clients refuse it with the typed
  invalid-input error rather than silently widen the grant. Omit the field
  (`None`/`null`) for "any runbook", or mint over REST.
- `audit.next_before` is present only when the page was full — absence
  means the trail is exhausted, not an error.
