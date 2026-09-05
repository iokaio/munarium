# The Munarium MMP contract bundle

This directory is the interface of **Munarium Server** that a client needs in order to
build and to prove compatibility: the wire protocol, the REST document, the error
registry, the two Rust crates that carry the wire types, and the conformance scenarios
in prose. It is cut from the server tree by a publisher and vendored, never hand-edited;
`contract.lock` names the source commit and the sha256 of every file, and
`publish.py --verify` re-checks them.

It is licensed under the **Apache License, Version 2.0** (`LICENSE`; notices in `NOTICE`)
so that the client libraries, and anyone building against the protocol, can use,
modify and redistribute it. The server that implements the protocol is published in
the same repository under the same license.

## Layout

```text
VERSION                    mmp: v1 / server: <the server version this was cut from>
contract.lock              source commit, sha256 per file, bundle digest
proto/mmp/v1/*.proto       the Munarium Protocol (MMP), ten files; the normative gRPC surface
openapi.json               the REST surface: every route the server serves, generated from the DTOs
errors.md                  the problem-slug registry (RFC 9457 `application/problem+json`)
rust/munarium-api-types/   the REST DTO crate: one struct per wire message; JSON casing decided here
rust/munarium-proto/       the generated MMP stub crate (prost + tonic; vendored protoc at build)
conformance/SCENARIOS.md   the eight conformance scenarios every backend and client passes
```

## Using it

- **Rust**: path-depend on `rust/munarium-api-types` (feature `proto` adds the pb↔DTO
  mapping and pulls `rust/munarium-proto`). Both crates build standalone; neither depends
  on anything of the server.
- **Python, .NET, Java**: point the gRPC code generator at `proto/` (`mmp/v1/*.proto`;
  the only external import is `google/protobuf/timestamp.proto`). REST models are written
  against `openapi.json`; error decoding against `errors.md`.
- **Any client**: `conformance/SCENARIOS.md` is the text a conformance suite is checked
  against; the server's own suite is the reference implementation.

## Compatibility

`VERSION` carries the MMP contract version and the server version the bundle was cut
from. A client release declares the server versions it supports; the policy (current and
previous minor) is stated in the client repository.

## Regenerating

From the server tree: `py contract/mmp/publish.py --out <dir>`; `--check <dir>` says
whether a vendored copy still matches what the tree would cut; `--self-test` proves two
cuts are byte-identical. Every text file is written UTF-8, LF, no BOM, so a cut is the
same bytes on every platform.
