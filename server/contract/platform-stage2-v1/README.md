# Stage 2 single-cell contract candidate v1

**Inert candidate; not accepted, released or executable authority.** Prepared
under [ADR 0012](../0012-stage2-candidate-profile.md) and the
[directed packet](../../stage2-contract-packet.md). Stage 1 candidate bytes and
consumers remain unchanged. This bundle is not a production trust configuration.

## Files and exact interpretation

| File | Role |
|---|---|
| [schema.json](schema.json) | Draft 2020-12 closed records and typed references; only the listed profile/kinds are supported |
| [profile.json](profile.json) | Immutable offline profile, ordered participants, event owners and numeric limits; runtime dependency qualification remains pending |
| [vectors.json](vectors.json) | Fictional records, fixed canonical bytes/digests, trusted fixture inputs and deterministic accept/refuse mutations |
| [bundle-lock.json](bundle-lock.json) | Exact LF-normalized file hashes and aggregate digest, including this semantic README |

Canonicalization and domain-separated digests are specified in ADR 0012. Qualified
references must agree with the independently admitted domain/tenant/deployment/cell.
The request binds stable intent separately from its authorization context. Current
identity, task lineage, evidence, approval status and ratification come from
independently authenticated service state, not from fields in a submitted record.
Fixture context lists exact event, receipt and acknowledgement digests already
authenticated by their owning services. Those lists model an embedding host's
verified inputs; they are not self-issued signatures, runtime trust stores or
permission to copy a digest into a request. Merely naming an allowed producer or
participant is insufficient without the matching independently supplied fact.
The fictional artifact/schema/evaluator references stand for retrieved admitted
artifacts; these fixtures do not supply a runnable policy or target installation.

An approval requires the exact recorded decision and one distinct currently
eligible enrolled human, outside the requester chain. A transition needs exact
ratification and complete authenticated pause/apply receipts. Producer ownership,
registered per-generation sequence and exact acknowledgements are enforced by
the offline oracle. Delayed historical facts need separate current recovery
authority and retained original event/cursor evidence. Delivery is audit only;
neither acknowledgement lookup nor successful fixture validation permits a send.

Cases use dot-path `changes` on one named base record and `context_changes` on a
copy of the independently supplied fictional trusted context. Integer path segments
index arrays. Each case expects `accept` or one refusal category. Where several
defects exist, consumers may report any applicable refusal; fixed vectors isolate
the category they assert. Public records and diagnostic errors must not disclose
foreign-tenant content. Errors distinguish InvalidShape, InvalidBinding,
Unauthorized, StaleContext, Expired, Denied, UnavailableEvidence, InvalidSequence
and IncompleteActivation. No error implies permission to resubmit an effect.

## Validation, generation and export

From the hub root:

```console
python -m unittest discover -s scripts -p "test_*.py"
python scripts/generate_stage2_candidate.py --check
python scripts/check_stage2_jsonschema.py
python scripts/vendor_stage2.py --destination NEW_CONSUMER_CANDIDATE_DIRECTORY
```

The ordinary test suite is dependency-free beyond the existing OpenSSL tests.
The separate standards comparison requires the already available `jsonschema`
package; it must fail if unavailable, not silently skip. Shape checks alone do not
establish semantic acceptance. Neither Python checker is an independent Rust,
.NET or deployed client; cross-language consumer conformance remains open.

[The generator](../../../scripts/generate_stage2_candidate.py) builds a fresh
candidate deterministically and refuses an existing lock. Check mode writes
nothing. Never regenerate an old bundle to alter its meaning; use a new reviewed
version/destination and retain its old vectors. No keys or secrets are generated.

[The exporter](../../../scripts/vendor_stage2.py) requires all candidate files to
match committed source. It verifies the lock and preflights every destination
collision, then writes a vendor lock with the source revision and bundle digest.
Re-vendoring into a new component namespace is a separate consumer packet. No
existing Stage 1 vendor directory is modified. LF hashes and the aggregate use
sorted filename, NUL, lowercase file hash and newline, matching the hub convention.

## Coverage boundary

The suite checks CR-01–06/08/10/12 wire distinctions and selected EV-01/03–08/12/13/16
refusals: identity/task substitution, scope/epoch errors, stale approval, incomplete
activation, missing or unsupported obligations, exact event/ack bindings and
historical audit delivery. The token-refresh case checks stable request bytes under
a later current admission context; it is not provider token verification. Action
capacity fixtures check recorded reservation bindings, not concurrent cap enforcement.

H03-01–13 and EV-02 concurrency, EV-09 export/access, EV-10 retention collection,
EV-11 suspension, EV-14 federation and EV-15 model settlement still require their
own implementations and evidence. These fixtures do not prove isolation, signatures,
producer durability, independent target effects, clock bounds or restore custody.
The producer event samples are binding examples in separate registered streams,
not a claimed executed history. Nothing here advances qualification or catalog state.
