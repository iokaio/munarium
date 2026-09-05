# Adapter support matrix

What each adapter can actually do, and what it refuses. A row here is only
worth reading if it says what proved it, so each one does.

**Two editions, one interface.** Munarium Matrix core — this repository — carries the adapters for
the operational databases an application already writes to, and for file and blob sources. The
adapters for analytics platforms an enterprise buys and administers separately are **Munarium
Matrix Enterprise**, a separate proprietary product that links this repository as a library and
registers them through the public `SourceAdapter` interface. The **Edition** column says which is
which.

The asset grammar does not change between them. A `DataSource` naming `databricks` parses,
validates and applies against a core build exactly as it does against an Enterprise one, and is
refused only when something tries to execute it — `adapter_not_available`, naming what would serve
it. That is what lets one set of assets move between deployments. It also means a core build never
silently substitutes a different adapter: it refuses, by name.

| Adapter | Mode A (materialize) | Mode B (query) | Replay level | Verified |
|---|---|---|---|---|
| **postgres** | snapshot + watermark + **cdc** (logical replication) | yes | `sealed_result` | compose, 7/7 in the `cdc.*` set; watermark advance and the source's declared columns each pinned by their own scenario |
| **landing** | manifest + snapshot, over `file` and Azure Blob (`store: az`, managed identity) | refused `not_covered` — an export serves materialization, not query contracts | `sealed_result` | `file` locally; the blob transport over managed identity |
| **mysql** | snapshot + watermark | yes | `sealed_result` | compose, 7/7 in the `mysql` tier |
| **sqlserver** | snapshot + watermark | yes | `sealed_result` | compose, 7/7 in the `sqlserver` tier |
| *Enterprise adapters* — databricks, snowflake, bigquery, cube, dbt | — | — | — | **not in this repository**; registered through `adapters::AdapterRegistry`, and refused by a core build as `adapter_not_available` |

**What an Enterprise adapter must satisfy** is a specification, not an
inventory of runs: exact decimals survive the wire at their declared scale; a
named parameter binds rather than being interpolated into the statement text;
an unmodelled column type is refused by column name rather than coerced; row
security is reported in the sealed coverage rather than silently omitted; and
a source that cannot pin a snapshot says so rather than inventing a marker.
Those are the properties this repository's conformance guarantees (G1–G7)
express, and they are what a third party writing a new adapter needs. The
planner surface, which executes nothing and therefore has no mode, is
described in [../api/planner.md](../api/planner.md).

## The watermark columns are the source's, not the adapter's

Closed; it had been a `[gap]` on the postgres row and an unearned
✅ on four others.

`DataSource.spec.sync.watermark` declares the column an incremental read
compares by, whether the comparison is inclusive, and the tie-break that stops
two rows sharing a watermark value from straddling the boundary. It was
**validated and then read by nobody**: `validate_sync` checked the pairing at
apply time, and five adapters went on to read `(updated_at, id)` because the
`SourceAdapter` trait did not carry the declaration down. A source declaring
`modified_on` therefore validated and was then queried by a column it had never
named — on Postgres a `not_covered` refusal, on the others a statement the
engine rejected. An asset field nothing reads is a lie waiting to be believed.

Three things changed:

* **`ReadMode`** carries the mode and its declaration as one value, so
  `read_batch` cannot be handed "watermark" without the columns to read by;
  `Watermark::resolve` is the single place a declaration becomes columns, and
  a watermark read with no declaration is a **refusal**, never a fallback to
  the old pair. Reading the wrong column is worse than not reading.
* **`inclusive` is honoured.** Exclusive without a tie-break stays refused
  (that is the configuration that loses rows); inclusive *without* one is
  legitimate — it re-reads the boundary rows every run — and was unreachable
  while the pair was hard-coded.
* **The checkpoint advances on every engine.** Every adapter once returned
  `next_checkpoint = the checkpoint it was given`, so an "incremental" run
  re-read the whole table forever and looked like convergence because nothing
  had changed. Worse, the first watermark read was not always ordered, so the
  row a run would have resumed from was whatever the engine returned last.

**How an adapter carried a support claim for a mode with no scenario:** each
tier's scenarios were probe, decimal, parameter binding, an unmodelled type,
the snapshot marker and row security. Not one of them read a watermark, and the
table said "snapshot + watermark" from the day the adapter landed. There is now
a `watermark_advances_by_the_declared_columns` scenario in the `mysql` and
`sqlserver` tiers and `postgres.watermark_reads_the_columns_the_source_declared`
in the `postgres` tier; each declares `(id, name)` — deliberately not the old
pair — and asserts the checkpoint came back holding an **id**, which under the
old convention would have been a timestamp. A support column with no scenario
behind it is the failure mode this document exists to prevent.

## postgres

The only adapter whose role posture is *proven* rather than configured: it
reads `pg_roles` and `pg_tables` at connect time and refuses a superuser, an
owner, or a role holding DML. The live tier runs the service itself under the
restricted `matrix_owner`, so the posture the design specifies is the posture
deployed.

### Mode A over logical replication

**Live, and it very nearly could not be.** Modes A (snapshot, watermark, **cdc**)
and B are all live on this adapter; the CDC path is proven in the `postgres`
tier, which compose stands up for $0.

#### The measurement that shaped the whole design

Logical decoding reads WAL, and **WAL is written before any policy is
consulted**. Measured on a real PostgreSQL 16: a role restricted to EMEA by a
row policy and denied the `secret` column outright — a role whose `SELECT`
returns exactly one row and cannot name `secret` at all — saw this through a
`test_decoding` replication slot:

```
table crm.opportunities: UPDATE: id[bigint]:3 ... region[text]:'AMER' secret[text]:'hush2'
table crm.opportunities: INSERT: id[bigint]:9 ... region[text]:'APAC' secret[text]:'topsecret'
```

Both denied rows, and the denied column's contents. That is a complete bypass of
row-level security **and** of column privileges, which is to say a complete
bypass of G6, through a channel the posture checks cannot see.

So `test_decoding` is **refused by name** (`cdc_slot_wrong_plugin`), and the
adapter reads `pgoutput` only — the plugin the built-in replication uses, which
applies the **publication's** column list and row filter while decoding. The
same measurement through `pgoutput` with
`FOR TABLE ... (id, name, amount, region) WHERE (region = 'EMEA')` returned only
the EMEA rows, and `secret` did not appear even in the Relation message that
describes the shape.

#### What Postgres will not let you have all at once

Both of these are engine refusals, provoked rather than read about:

| Attempt | The engine's answer |
|---|---|
| `REPLICA IDENTITY FULL` + a column list that withholds a column | "Column list used by the publication does not cover the replica identity" |
| A row filter naming a non-key column + `REPLICA IDENTITY DEFAULT` | "Column used in the publication WHERE expression is not part of the replica identity" — updates and deletes on the table are refused outright |

A source that needs a row filter on a non-key column **and** a column list that
withholds one therefore needs `REPLICA IDENTITY USING INDEX` over a unique index
covering the key and the filter's columns. The fixture does exactly that
(`CREATE UNIQUE INDEX ... (id, region)`), and it works.

#### The role posture holds

`REPLICATION` is a role attribute, not superuser: it does not bypass row
security, grants no DML and confers no ownership. A role holding it still passes
every check `introspect` makes, so **CDC did not require widening the posture**
— which is the outcome that made building this legitimate rather than a
compromise. It is checked in the CDC path only (`cdc_role_lacks_replication`),
never added to the posture, because a non-CDC source has no use for it.

The service role deliberately does not hold it. The conformance tier stages its
slots as the bootstrap superuser, which is what an operator would do — the first
run of the tier said so itself: *"Only roles with the REPLICATION attribute may
use replication slots."*

#### The slot is durable state on someone else's database

**Matrix never creates one.** A replication slot makes the server RETAIN WAL
until something consumes it: a slot nobody reads fills the disk and stops the
database, and it goes on doing that after Matrix is uninstalled. Creating one
implicitly would make Matrix the author of an outage it never announced. So
`cdc_slot_missing` refuses with the exact statement to run, and says why.

The names are a **convention** derived from the source id
(`munarium_matrix_<source>` for both the slot and the publication) unless the
DataSource declares them — `spec.sync.cdc: {slot, publication}`, either or
both, since — for the same reason: a refusal has to be able to
print the statement an operator should run, and it can only do that when the
name is derived or declared, never guessed. A declared name must be a plain
Postgres identifier (`sync.cdc-name` at validation, with its fixture), because
it is interpolated into that statement.

Retention is reported, not acted on: `cdc_retained_bytes()` returns
`pg_wal_lsn_diff(pg_current_wal_lsn(), confirmed_flush_lsn)`, so an operator can
watch retention grow before a disk fills. Matrix is in no position to decide
that a customer's slot should be dropped.

#### Peek, then advance on the next call

`pg_logical_slot_get_changes` **consumes**: what it returns is gone from the slot
whether or not the caller managed to persist a checkpoint, so a crash in between
loses changes silently. Every read here **peeks** — non-destructive — and the
slot is advanced to the checkpoint's LSN at the start of the *following* call,
where the checkpoint's existence is itself the proof that the previous batch was
durably recorded. The cost is a little extra retained WAL between runs, which is
exactly what `cdc_retained_bytes()` exposes.

The conformance scenario asserts this directly: reading twice from the same
checkpoint returns the same three changes.

#### The first read, and which way to be wrong

A slot has no history before it existed, so the first CDC read is a **snapshot**
pinned to an LSN — and the LSN is read as the FIRST statement of the same
`REPEATABLE READ` transaction. That ordering is the whole point: it fixes the
transaction's snapshot at the moment it reads the position, so a commit that
interleaves is delivered **twice** rather than never. Duplicates are free here
because the rendering is idempotent by row path; a miss would be permanent.

#### Gaps, truncations and absent values

* **`cdc_checkpoint_gap`** — the checkpoint is behind the slot's
  `confirmed_flush_lsn`, so the WAL that held those changes has been released.
  Reported, never silently resnapshotted: that is what lets the sync worker
  record `resnapshotted: true` instead of implying continuous coverage.
* **`cdc_truncate_not_covered`** — a `TRUNCATE` means every row went away at
  once, and there is no way to render that as records. Refused rather than
  skipped, because reporting nothing would leave the collection claiming rows
  the source no longer has.
* **`cdc_unchanged_toast`** — an out-of-line value that did not change is not in
  the stream. Sealing NULL in its place would put a value in evidence the source
  never held, so the record is refused and the column is named.
* **`cdc_unsupported_message`** — a protocol message this build does not model
  is refused, not stepped over. It may be a change the collection would
  otherwise miss.

#### A delete carries the identity, and the tombstone says so

With `REPLICA IDENTITY DEFAULT` or `USING INDEX`, a DELETE carries **only** the
identity columns; everything else arrives NULL because the engine did not send
it, which is a different fact from the row having held a null. The sync worker's
tombstone wording was changed to match: it now says the fields are *"the values
the source sent with the deletion — some engines send only the row's identity"*,
which is true of every change-feed transport. It previously said "the row's last
known values", which is true only of some of them.

#### Verified on compose

`docker compose down -v && docker compose up -d postgres` (the `-v` matters:
Postgres runs its init directory only on an empty data dir, and `test.ps1` now
fails with that instruction rather than letting the tier refuse
`cdc_publication_missing` and look like a code defect).

Seven `cdc.*` scenarios green in the `postgres` tier, twice in a row and verified
with `--nocapture` so a silent skip could not read as a pass:

| Scenario | What it proved |
|---|---|
| `a_missing_slot_is_refused_with_the_statement_that_creates_it` | Matrix creates no slot, and the refusal is actionable |
| `a_slot_that_decodes_with_test_decoding_is_refused` | the policy-bypassing plugin cannot be used by accident |
| `a_publication_without_a_row_filter_on_a_secured_table_is_refused` | a secured table cannot be streamed unfiltered |
| `a_publication_that_does_not_match_the_projection_is_refused` | the column list is the policy, so it must be exact |
| `inserts_updates_and_deletes_arrive_distinguishable_with_their_lsn` | three `ChangeKind`s in commit order, each with an LSN; `900000.75` exact; delete keyed `1\|EMEA`; the AMER row and `secret` absent; replay non-consuming; resume empty |
| `a_checkpoint_behind_the_slot_is_reported_as_a_gap` | `Incomplete`, so the worker resnapshots and says it did |
| `the_slots_retained_wal_is_observable` | retention is a number an operator can watch |

The decoder itself is tested against **captured bytes** from a real PostgreSQL 16
(`src/munarium-matrix-adapter-postgres/tests/captured-pgoutput.txt`), not against
a constructed shape — eight further unit tests, including that a truncated
message refuses rather than panics.

#### What is NOT built

* **The streaming replication protocol.** This adapter reads slots through the
  SQL interface (`pg_logical_slot_peek_binary_changes`), which is pull-based and
  bounded per call. `START_REPLICATION` would give lower latency and needs a
  replication connection sqlx does not speak.
* **Proto version 2+.** Streamed in-progress transactions are not decoded; the
  adapter asks for `proto_version 1`, where a transaction arrives only once it
  has committed. That is the conservative choice for a sealer.
* **Proof that the publication's filter matches the RLS policy.** Comparing two
  SQL expressions for equivalence is undecidable, so the filter is recorded
  verbatim in the sealed coverage (`filter=...` in the marker) and the
  equivalence is **an operator's assertion**. That is a weaker guarantee than
  RLS, and it is the honest limit of this path.

## landing

Immutable CSV/JSONL exports over `file://` or, since, an **Azure
Blob container** read with the process's managed identity (`connection:
{store: az, account, container, prefix}`; the blob endpoint is the egress
host and must be in `allowHosts`; the identity needs `Storage Blob Data
Reader` on the container). The blob transport arrived well after the
filesystem one — for weeks the live scenario "manifest sync from blob via
managed identity" existed on paper and could not run, and the docs index said
"`file` only" as if that were a choice. The client is `object_store`'s, built
`from_env()` exactly as the server's own blob store is, because Azure Container
Apps have no classic IMDS and a client built any other way black-holes against
a link-local address.

Refuses `execute` by design (`capabilities().query_contracts == false`), and
refuses any sync mode but manifest/snapshot, because an immutable export has no
watermark and no change feed and declaring either "would be a support claim we
cannot honour". No S3 or GCS transport: nothing needs one yet, and a transport
with no live proof is a support claim too.

## mysql

**The second SQL engine behind the same seam** (built, and
the reason to build it was to find out what the seam had assumed about
Postgres. Modes A (snapshot, watermark) and B (query contracts, native data
views) are live; there is no CDC, so a binlog path stays unbuilt
rather than implied — a watermark read cannot see a delete, and this adapter
does not pretend otherwise.

**Four defects a real server found on first contact**, none of them reachable
offline:

1. **Transaction characteristics must come BEFORE the transaction.** Postgres
   accepts `SET TRANSACTION` as the first statement inside one; MySQL answers
   1568/25001 "Transaction characteristics can't be changed while a
   transaction is in progress". The session is now configured on one acquired
   connection and the transaction opened on that same connection — a pool
   hands out a different session each time, and settings applied to another
   would be a no-op nobody would notice.
2. **Transaction control cannot be prepared.** sqlx prepares by default;
   MySQL answers 1295 "not supported in the prepared statement protocol yet"
   for `START TRANSACTION`. Executing a bare `&str` runs it as a simple query.
3. **`SHOW GRANTS` returns VARBINARY**, not VARCHAR — a decode-time type
   mismatch, invisible at compile time.
4. **So does `information_schema`.** Both are cast in SQL rather than decoded
   as bytes in four places.

**Three things it made explicit about the seam.** Quoting is per engine
(`` `ident` `` here, `"ident"` there). A snapshot marker is not universal:
MySQL's analogue is a GTID set, which exists only with GTID mode on — off by
default and off in the fixture — so the adapter reports one when the server
has one and `None` when it does not, and its `replay_level` is
`sealed_result`, the honest floor. And row-level security is not a given:
MySQL has no policy engine, so `introspect` REPORTS `subject_to_row_security`
as failing rather than omitting the check — a reader comparing postures across
engines must see that this protection is absent here and is supplied by
per-class grants and views instead.

Two decoding decisions worth keeping. An UNSIGNED BIGINT that does not fit
`i64` is refused rather than wrapped, because a wrapped id cites the wrong
row. And `TIMESTAMP` (stored UTC) and `DATETIME` (no zone) map to different
canon@1 types, which is the distinction the whole value layer exists for.

**Measured — compose.** `docker compose --profile mysql up -d`
brings up MySQL 8.4 with the fixture in `fixtures/mysql/`. **Seven** `mysql.*`
scenarios green: the probe reaches it; `900000.50` survives the driver with
its trailing zero; a bound parameter binds (the compiler's `$1` renumbered to
`?` in the adapter, so one plan hash runs on both engines); a GEOMETRY column
is refused `schema_drift` naming the column rather than guessed at; a snapshot
read reports no marker because the server has none; `introspect` reports
row security as absent; and a watermark read advances its checkpoint by the
columns the source DECLARED — the seventh, added, which is when
this row stopped claiming a mode nothing tested.

## sqlserver

**The third SQL engine behind the same seam** (built, and
the first one whose differences from Postgres are about *guarantees* rather
than syntax. Modes A (snapshot, watermark) and B (query contracts, native data
views) are live. Change tracking could serve a change feed; reading a version is
not reading one, so `sync_modes` says what is built.

**Measured — compose.** `docker compose --profile sqlserver up -d`
brings up SQL Server 2022 Developer with the fixture in `fixtures/sqlserver/`.
**Seven** `sqlserver.*` scenarios green against a real engine, run as the
fixture's row-level-secured `matrix_reader` login rather than as SA:

| Scenario | What it proved |
|---|---|
| `probe_reaches_a_real_server` | TDS connect, TLS, login |
| `an_exact_decimal_survives_the_driver` | `900000.50` keeps its trailing zero through TDS, which carries a decimal as an integer plus a scale — and the read ran under `snapshot` isolation, reported as such |
| `a_positional_parameter_binds_rather_than_interpolates` | the compiler's `$1` renumbered to `@P1`, bound, four EMEA rows |
| `an_unmodelled_type_is_refused_and_names_the_column` | `geography` AND `money` refused `schema_drift`, each naming its column and its type |
| `a_snapshot_read_reports_a_marker_only_from_a_consistent_view` | four rows (the policy is in force), `ct:<n>` marker, every record stamped with it |
| `introspect_reports_row_security_as_present` | the policy is OBSERVED on `opportunities` and absent on `shapes`; the schema-wide claim is correctly false |
| `watermark_advances_by_the_declared_columns` | a watermark read by `(id, name)` — the DECLARED pair, not `[updated_at]`/`[id]` — advances the checkpoint, and the resumed read returns nothing. Added; before it, `next_checkpoint` handed back the checkpoint it was given |

### What a real server found that nothing offline could

**1. The driver PANICS on a spatial column, before any row exists.** (True of
`tiberius` 0.12.3, the driver at the time; `tiberius-ng` 0.13, adopted the same
day, fixes it — see the decision below. The finding stands because the
pre-flight it forced is still the design.) tiberius
`todo!()`s while parsing the column-metadata token for a `Udt` — which is how
`geography`, `geometry` and `hierarchyid` all arrive
(`tiberius-0.12.3/src/tds/codec/type_info.rs:317`). The first version of this
adapter refused an unmodelled type from the driver's own result metadata, which
is what the MySQL adapter does; against a real server that call brought the test
process down instead of returning a refusal, so the refusal was unreachable for
exactly the type that most needed it.

The fix is engine-native rather than defensive: `execute` and `read_batch` now
ask `sys.dm_exec_describe_first_result_set` what the statement WILL return,
before running it. It names every output column and its type as text, costs a
plan compilation and no execution, and lets the adapter refuse by name. A
statement the engine cannot describe is refused rather than attempted — fail
closed, because the alternative here is a panic. It buys two things beyond
survival: a bad contract costs a compile rather than a scan, and the check is
the engine's opinion of the statement rather than a parse of it in Rust.

**2. Proving the posture needs a metadata grant a data reader has no other
reason to hold.** SQL Server filters catalog metadata by permission, so a login
with `db_datareader` sees ZERO rows in `sys.security_predicates` — and
`introspect` would report "no row security" for a table that has it. Measured
by revoking it: with `GRANT VIEW DEFINITION` the fixture's reader sees one
enabled predicate; without it, none. An absence of evidence read as evidence of
absence, on the one check where that is most dangerous. Postgres has no
equivalent trap because `pg_catalog` is world-readable. The fixture grants it
and says why; a deployment that does not grant it gets a posture that
under-reports, which is the safe direction but must not be mistaken for a fact.

**3. `DATABASEPROPERTYEX(DB_NAME(), 'SnapshotIsolationState')` returns NULL to a
least-privileged reader**, and a NULL there would have read as "snapshot
isolation is off" and silently downgraded every read to read committed —
which would in turn have suppressed the snapshot marker. `sys.databases`
answers the same question for the same login and is used instead.

### Three things it made explicit about the seam

**T-SQL has no read-only transaction.** Postgres has `SET TRANSACTION READ
ONLY`; MySQL has `START TRANSACTION READ ONLY`; SQL Server has neither.
Read-only here is a property of the PRINCIPAL and of the TOPOLOGY, so the
adapter proves it in `introspect` (server role, database roles, and
INSERT/UPDATE/DELETE/ALTER permission on the schema) and sets
`ApplicationIntent=ReadOnly` on the connection, which is a no-op on a
standalone server and a real engine-enforced guarantee against an availability
group listener. Writing "the transaction is read only" in this adapter would
have been a comfortable sentence about a flag that does not exist.

**Characteristics come before the transaction, on the same session** — the same
rule MySQL taught, for a different reason: SQL Server refuses SNAPSHOT
specifically (3951), while accepting other isolation changes inside a
transaction. This adapter opens a FRESH session per operation rather than
pooling, because `SET LOCK_TIMEOUT`, `SET ROWCOUNT` and the isolation level are
all session state and a pooled session handed back carrying any of them is the
next caller's silent problem. One TDS handshake per operation buys the property
outright; the MySQL adapter had to pin a connection out of its pool to get the
same thing.

**Transaction control cannot go through the parameterised path.** tiberius's
`query()` is an `sp_executesql` RPC, and changing `@@TRANCOUNT` inside a
procedure is error 266 on return; `simple_query()` sends a plain batch. Same
shape as MySQL's 1295, arriving for a different reason.

### A marker, and the condition on it

`snapshot_marker` is `ct:<version>` from `CHANGE_TRACKING_CURRENT_VERSION()`,
and it is reported **only** when change tracking is on AND the transaction
actually started in snapshot isolation — which is the arrangement Microsoft's
own change-tracking guidance prescribes, because the version then names the
same consistent view the rows came from. Read outside a snapshot transaction it
would be a number that raced the read. The presence half is proven live; the
`None` half (change tracking off, or a read committed transaction) is asserted
by `snapshot_marker_for`'s unit tests, because one fixture cannot be in both
states at once. `replay_level` stays `sealed_result`: a change-tracking version
is a POSITION, not a retained history, so there is nothing to re-run against.

### `money` is refused, on purpose

`money` and `smallmoney` are EXACT four-decimal currency types on the server and
IEEE-754 doubles in this driver (`money.rs`: `read_i32_le() as f64 / 1e4`). A
currency silently becoming a float is the precise failure canon@1 exists to
prevent, so both are unmodelled types and a read of one is refused naming the
column and the type. A deployment that needs the column casts it in the
contract's statement. The fixture carries a `money` column so the refusal is
reached rather than merely written.

### `[decision]` — the driver moved to `tiberius-ng`

sqlx dropped its MSSQL driver in 0.7 and has not brought it back, so this is the
only adapter that cannot ride the workspace's driver. `tiberius` 0.12.3 — the
last release upstream published — pins `tokio-rustls` 0.24 → `rustls` 0.21 →
`rustls-webpki` 0.101, which carries **RUSTSEC-2026-0098**, **-0099**
(name-constraint validation accepted where it should not be) and **-0104** (a
reachable panic parsing CRLs). The first two are certificate-chain flaws in the
path this adapter walks on every connect, not something reachable only through
an unusual API call.

**There was no in-range fix.** The advisories are answered by `rustls-webpki`
≥ 0.103 and `rustls` 0.21 cannot take it, so `cargo update` had nothing to
offer. Three moves existed: the fork, suppress the gate, or drop SQL Server
support.

**The fork was taken.** `tiberius = { package = "tiberius-ng", version = "0.13" }`
is the entire change — no source edits — and after it:

| | |
|---|---|
| `cargo deny check advisories` | **-0098, -0099 and -0104 are gone**; `rustls` 0.21 leaves the graph entirely, leaving one `rustls` 0.23.43 |
| `boundary: no openssl` | still clean — `default-features = false` remains load-bearing, because the defaults include `native-tls`. What the check does NOT ban, since: `openssl-probe`, which is in the graph BECAUSE of `rustls-native-certs` and links nothing. The old prefix match could not tell the two apart and failed CI on it (`scripts/boundaries.py`) |
| the `sqlserver` tier | **6/6** against the same compose fixture, `an_unmodelled_type_is_refused_and_names_the_column` included (7/7 since the watermark scenario landed) |
| one of the two `rustls-pemfile` paths | gone with it |

**What was accepted, stated rather than buried.** `tiberius-ng` is one
maintainer and, at the time of writing, one published version. An unaudited
crate running in-process with a database credential is a real supply-chain
risk, and it is not smaller than the CVEs merely because it is newer. It was
taken because the alternative was shipping a driver whose certificate
validation is known-broken while a gate stayed green by suppression.

**Re-evaluate when:** upstream `tiberius` publishes past 0.12.3 — switching
back is the same one-line `package =` rename — or `tiberius-ng` goes quiet, or
any advisory lands against it.

A side effect worth knowing: `tiberius-ng`'s `type_info.rs` handles the `Udt`
token that made the pinned crate panic. The `describe_first_result_set`
pre-flight **stays anyway**. Asking the engine is the fail-closed design
independent of any driver bug — it refuses an unmodelled type by NAME rather
than after a decode, it is the only thing that would catch the next such token,
and a refusal that depends on the driver being correct is a refusal that moves
when the driver does.

### `cargo deny` is green, with two reasoned ignores

`advisories ok, bans ok, licenses ok, sources ok`.

Two advisories are ignored, each with the condition that revokes it recorded in
`deny.toml`:

- **RUSTSEC-2023-0071**, the Marvin timing sidechannel in `rsa`, reached
  through `sqlx-mysql`. There is no fixed release, so it cannot be updated
  away. **Not reachable here, and that was checked by reading the dependency
  rather than inferred from the advisory title**: Marvin recovers a PRIVATE key
  from DECRYPTION timing, and `sqlx-mysql/src/connection/auth.rs` imports only
  `RsaPublicKey` and calls only `encrypt` — this process holds no RSA private
  key and performs no private-key operation. The path is skipped outright over
  TLS as well (`if stream.is_tls { return Ok(to_asciz(password)) }`).
- **RUSTSEC-2025-0134**, `rustls-pemfile` unmaintained, now one path (tonic).
  An *unmaintained* advisory, not a vulnerability; the server tree carries the
  same entry for the same reason, since both trees pin the same tonic.

`cargo deny check licenses` had also been failing on a first-party rule: a
crate added without a `[[licenses.exceptions]]` stanza turned the gate red over
something with nothing to do with third-party licence drift. Every workspace
member now carries one, and `scripts/boundaries.py` checks that set equality in
both directions, so a dead exception fails as loudly as a missing one.

## Placeholders and quoting, across the SQL adapters

The compiler renders ONE plan with Postgres-style `$1` placeholders, and the
plan hash is over the parsed AST — so the hash does not move when the engine
does. Each adapter rewrites on the way in:

| Adapter | Identifier quoting | Placeholder | Rewritten where |
|---|---|---|---|
| postgres | `"ident"` | `$1` | not rewritten |
| mysql | `` `ident` `` | `?` | `positional_to_question_marks` |
| sqlserver | `[ident]` | `@P1` | `positional_to_at_p` |
| snowflake | `"IDENT"` (folded) | `?` + ordinal bindings | `positional_to_question_marks` |
| bigquery | `` `ident` `` (no escape) | `@p1` + named parameters | `positional_to_named` |
| databricks | `` `ident` `` | `:name` | emitted named by the semantic compiler |

In every case a placeholder beyond the bound count is left alone: if the
compiler and the binder ever disagree, the engine must see the discrepancy and
refuse rather than receive a placeholder with nothing bound.

### The semantic compiler's dialect table

`core::semantic::SemanticScope::with_dialect` mapped `"databricks"` to
backtick/named and **everything else** to double-quote/positional. Correct for
postgres and sqlserver (which accepts `"ident"` under QUOTED_IDENTIFIER, on by
default for TDS clients) and correct for snowflake apart from case folding —
but **wrong for mysql and bigquery**: MySQL reads `"opportunities"` as a string
literal unless `ANSI_QUOTES` is set, and the compose fixture's
`--sql-mode=STRICT_ALL_TABLES` does not set it. A native `DataView` on a MySQL
source emitted a statement that runs and means something else.

**The catch-all was the defect, not the missing rows.** A default that produces
a plausible statement for an engine nobody taught it about fails as a wrong
ANSWER rather than as a refusal, and a wrong answer from a system built for
verifiable evidence is the worst failure it has.

`try_with_dialect` is now exhaustive over the six dialects an adapter reports —
databricks (`` ` ``/`:name`), postgres (`"`/`$1`), mysql (`` ` ``/`?`),
snowflake (`"`/`?`), bigquery (`` ` ``/`@name`), sqlserver (`[`/`@name`) — and
**refuses an unknown one by name**. Two tests pin it: one walks all six, one
asserts the refusal. `Quoting::Bracket` and the two new placeholder styles came
with it.
