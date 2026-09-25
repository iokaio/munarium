# Wire compatibility: integer inventory and evolution policy

P11 inventory of Server 1.2.1 / MMP v1 and the four Server SDKs at 1.1.1,
starting from `53e273e` (P10). The compatibility record supports Server minors
1.2 and 1.1. Transport capacity and supported storage ranges are separate:
an exact `u64` on the wire is not necessarily persistable in PostgreSQL `BIGINT`.
P10 erasure and continuous reauthorization are outside this work.

## Integer field inventory

Normative sources are [REST DTOs](../src/munarium-api-types/src/lib.rs),
[typed MMP](../proto/mmp/v1/) and the
[native bridge](../proto/mmp/v1/server_api.proto). Integers remain JSON numbers;
typed protobuf tags and scalar types are unchanged. The native bridge carries
UTF-8 JSON bytes, not `google.protobuf.Struct` floating-point numbers.

| Field family (including request and response occurrences) | Wire carrier | Store/domain boundary |
|---|---|---|
| Ledger `seq`, `head_seq`, `expected_head`, `as_of_seq`, `fulfilled_seq`, digest `built_from_seq`; problem `expected` / `actual` | REST `u64`; protobuf `uint64` | Persisted sequence/head is `0..=9223372036854775807`. Appending at the maximum fails before mutation. A larger inclusive read pin still includes every persistable sequence; it is clamped only for the SQL bound. Expected-head values remain exact for conflicts. |
| Counter `count`, `total`, optional `budget` | REST `u64`; protobuf `uint64` | Each PostgreSQL count/budget fits nonnegative `BIGINT`; SQL aggregate totals fail with the existing storage error when their sum exceeds signed 64-bit capacity. Memory aggregates support `0..=u64::MAX` and reject sum overflow. |
| Retrieval `event_watermark`, build `watermark_seq` | REST `u64`; protobuf `uint64` | Persisted nonnegative `BIGINT`, checked on writes and reads. Index reactivation must not wrap an oversized watermark into a negative value. |
| Provider/turn `input_tokens`, `output_tokens`; P01 usage evidence components; reservation `units`, `original_units`, `accounted_units`, `held_units`, `settled_units`, reservation count | REST integers; typed completion/turn protobuf `uint64`; evidence in JSON extensions | Individual PG reservation amounts are bounded by signed 64-bit. Core usage arithmetic is checked `u64`; unknown usage is not inferred from zero or missing evidence. |
| Reports: interactions, turns, overrides, tokens, held/settled tokens, reservations, limit/remaining, requests/errors, runs/steps, active users, refusals/completions, percentiles, hierarchy/completeness totals, bucket seconds | REST `i64`; native bridge only for full reports | Signed 64-bit DTOs and SQL aggregates. Converting memory-store `u64` budget totals or adding held/settled amounts must fail instead of wrapping. |
| Source/file `bytes_len`; bulk `total`, `already_present`, `stored`, `skipped_existing`, `pending`, `failed`, missing/mismatched counts | REST `u64`; typed source length `uint64` | Bulk manifest lengths persist as `BIGINT`; HTTP source payload has an independent 256 MiB ceiling and file/batch input has a 500-item ceiling. Collection `source_count` is `i64` / protobuf `int64`. |
| Digest `tier` | REST `u8`; protobuf `uint32` | REST numeric carrier maximum 255; typed gRPC must reject 256 before narrowing. This carrier limit is separate from supported digest tiers. |
| `top_k`, limits/fact limits, `max_tokens`, token budgets, embedding `dimensions`, ranks, ordinals, retries, disclosed conflicts, runbook versions | Mostly REST/protobuf `u32`; some REST `usize` or `u64` | Per-operation limits apply before allocation; the [token budget table](tokenbudgets.md) defines the eight configured ceilings. PostgreSQL chunk/approval ordinals are nonnegative `INTEGER` (maximum 2147483647). Typed embedding dimensions use `uint32`, while the REST DTO uses `u64`. |
| Access/minimum access levels; interaction status/latency; artifact format version/file count/attempts | REST `i32`; relevant typed fields `int32` | Signed 32-bit carrier, with endpoint-specific valid values. These are not general unsigned counters. |
| Artifact bytes, evidence row windows, generation and expected/staged/serving generations | REST `i64`; native bridge | Signed 64-bit database fields, with domain checks for valid generations/window bounds. |
| Evidence `from`, optional total; recorded/skipped duplicate/rule counts | REST `usize` | Architecture-sized local carrier, exact integer JSON. Evidence `from` is also bounded by signed-64 audit storage; window defaults and caps remain operation-specific. |
| Context estimated/budget tokens, health latency/failure counts, turn elapsed milliseconds, authoring finding totals, datastore chunk count, build `max_chars`, token `ttl_secs` | REST `u64` / `usize`; relevant typed fields `uint64` | In-memory/computed quantities are not all persisted `BIGINT`s. Token TTL retains its 24-hour clamp. Existing admission and resource caps remain in effect. |

The PostgreSQL implementation is in
[store-pg](../src/munarium-store-pg/src/) and
[retrieval-pg](../src/munarium-retrieval-pg/src/). Existing budget reservation
range checks and core checked usage sums are controls, not new fixes. SQL
aggregation failure is an error, never a truncated successful report. The
inventory does not assert that every platform-sized allocation or internal
generation arithmetic site has been qualified at its theoretical maximum.

Request narrowing failures (bulk lengths, approval ordinals, evidence offsets
and typed digest tiers) use the existing `invalid-input` mapping. Unrepresentable
store/report values use the existing `storage-error` mapping. No error slug or
database migration is added. Previously wrapped negative stored values now fail
the affected unsigned reads; this change does not rewrite or repair those rows.
Valid existing data and wire payloads retain their formats. Rolling back code
requires no data conversion, but restores the former unchecked behavior.

## SDK carriers and browser boundary

| Consumer | Exact integer representation | Relevant limit |
|---|---|---|
| Rust | DTO `u64`/`i64`, serde JSON integer numbers, prost integer fields | Native type bounds; `ApiResponse` keeps bytes and can decode a typed DTO or raw value |
| Python | Arbitrary-precision `int`, JSON parser integers, protobuf integer fields | Sequence, watermark, byte-length and counter carriers enforce unsigned-64 bounds; a float/bool/string is not an integer token |
| .NET | `ulong` / `long`, `JsonElement`, protobuf unsigned/signed fields | Typed bounds; raw JSON permits exact `GetUInt64` / `GetInt64` |
| Java | `long` in the typed facade, Jackson `JsonNode`, protobuf Java `long` bit carrier | Typed nonnegative `uint64` domain is `0..=Long.MAX_VALUE`; full API raw JSON supports exact large integer nodes. A fractional JSON number must not truncate to an integer |
| Browser JavaScript (no official Server SDK in this tree) | Ordinary `JSON.parse` uses `Number` | Exact only through `2^53 - 1`; parsing `9007199254740993` this way loses information before a reviver can repair it |

Applications requiring larger browser integers must use a lossless JSON parser
on the response text or an integer-aware intermediary. Do not convert an already
rounded `Number` to `BigInt`. P11 does not silently change v1 numeric members to
strings. An additive exact representation needs a separate contract proposal
with old/new precedence and conflict rules.

## Unknown-field and presence policy by direction

| Direction/surface | Existing policy retained and tested |
|---|---|
| REST requests | Unknown object members remain permissive. Known fields must satisfy their types and endpoint validation. Unknown fields cannot opt a caller into new authority. No global `deny_unknown_fields`. |
| Typed protobuf requests | Unknown tags are skipped. Existing scalar-zero sentinels remain; the typed clients reject explicit zero when it cannot be distinguished from omission. Native bridge JSON retains explicit zero/null/empty-list distinctions. |
| REST typed responses | Added fields are tolerated; missing optional fields and explicit null map to absent where the DTO declares an option. Rust/Python typed governance enums reject unknown strings; Java/.NET string members preserve them, and disputed predicates treat only explicit `accepted` as non-disputed. Java/.NET primitive members can default to zero when absent. Consumers must not infer permission or success from an unrecognized string. |
| Typed protobuf responses | Unknown fields are skipped. Unknown/unset claim status, provenance and finding severity decode conservatively as disputed, emergent and block. Raw enum values must never become accepted/witnessed/info by default. |
| Extension maps / full API responses | Preserve raw JSON, unknown fields and opaque IDs without inventing domain meaning. Reading a future usage quality string does not establish known usage. |
| SSE | Existing parsers ignore unknown event kinds/stages as documented and retain terminal done/error handling. New stages must not become successful completion. Reuse the existing SSE controls. |
| Persisted documents | Apply each document's schema/version policy. JSONB metadata is an extension point; governed artifact/version formats retain their version checks. Wire tolerance is not permission to decode every persisted future version. |
| Native bridge | `body` remains bytes with content type; endpoint JSON rules apply after routing. Protobuf additions to the wrapper are independent of additions inside the body. |

Presence is field-specific: optional null and omission may both mean absent;
explicit zero and an empty list can have real semantics. The old typed proto3
sentinels are not retroactively given presence. Use the full API for those cases.

## Compatibility evidence

Boundary fixtures exercise `2^53 - 1`, `2^53`, `2^53 + 1`, signed-64 maximum,
maximum-plus-one and unsigned maximum as appropriate to the carrier. Invalid
JSON numbers include negative, fractional, boolean, string and oversized values.
PostgreSQL tests seed only test-owned rows near the sequence limit, then exercise
ordinary writes/reads/pins; no production sequence setter or enormous write loop
is introduced.

Synthetic previous/current response fixtures cover the 1.1 baseline and additive
usage/optional metadata, missing/null fields, future fields/enums and opaque IDs.
They qualify current decoders against those shapes; they are not an execution
claim for every previously published SDK/server binary. The full named API still
requires Server 1.2; Server 1.1 compatibility applies to the historical typed
facades described in [the SDK record](../../clients/compatibility.json).

The actual SDK source references are `clients-v1.1.0` at
`afd3eb7bfcea253ce7f48f7060c72c23e26920c0` and `clients-v1.1.1` at
`95af0e76160bfe024c4954bc66f0bf72d714a7a1`. Their completion-reader source blobs
are identical: Rust wire DTOs `c17f221d4777182ed6df0a102c1fcd188a9f2d47`, Python
models `2e6c903a572e162e283972c1403f252e1e9119be`, .NET models
`4143483013bb439b4d25cc1f394352f5acf5510f`, and Java provider models
`37b4aed21be319618f6fd00c69365da6d73493c6`. The Rust fixture freezes the previous
seven-field completion reader and decodes current additive responses with both
models, using current serializer dependencies. This is previous-release model
compatibility evidence, distinct from the supported Server minor-version policy
and from executing released binaries. It does not attest that older clients have
the conservative unknown-enum fixes introduced here.

No normative DTO fields, proto tags, generated SDK files or locked contracts
change. Source and generated versions remain Server wire crates 1.2.1, SDKs
1.1.1 and MMP major 1. The existing publisher/generator and drift checks remain
automatic; no re-vendoring is needed for tests or conversion implementation fixes.

Validation results and exact regression commands are recorded with
[P11 in the implementation plan](lessons-from-vcp-impl.md#11-wire-compatibility).
