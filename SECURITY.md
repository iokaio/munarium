# Security

Do not file a vulnerability as an issue or a pull request.

Report a suspected vulnerability in any component of this repository privately, by either route:

- GitHub's private vulnerability reporting ("Report a vulnerability" under the Security tab), or
- email to **info@ioka.io** with "security" in the subject.

Say what you found, where, and how to reproduce it. Do not include live credentials, customer data,
or a proof of concept run against a system you do not operate. You will get an acknowledgement
within two business days, and a fix — or a recorded decision — on the affected path before any
related release. Credit is given if you ask for it.

## Supported versions

Security fixes go to the current minor release of each component and to the previous one for six
months after its successor ships. An older release gets a fix only where the vulnerability is in a
wire contract it still speaks.

## What is deliberate, and is not a defect

Read [server/docs/security-posture.md](server/docs/security-posture.md) first. Two positions there
are by design:

- **Munarium Server is not an identity provider.** It performs no login, holds no user directory,
  and implements no OIDC or JWKS. An enterprise API-management layer in front of it authenticates
  humans and asserts the end-user id downstream. A report that the server "does not authenticate
  users" describes the design.
- **The server governs what an already-authenticated caller may touch** — tenant scoping,
  capability attenuation, access levels and compartments at query time, an immutable per-uid
  interaction audit, and an index lifecycle with no delete API. A failure in *those* is a
  vulnerability, and a serious one.

For **Matrix**, two classes of finding matter most and will be taken seriously and quickly:

- **A read that escapes its declared scope** — a query contract executing a statement its allowlist
  should have refused, a row filter or column mask not surviving to the source, an adapter reading
  a column the source never declared, or a watermark advancing past rows it did not read.
- **Evidence that does not describe what happened** — a sealed result whose logical hash does not
  bind the rows it claims, a truncated result presented as complete, a replay returning something
  other than the state it pinned, or a promotion writing outside its declared authority scope.

Deployment defaults documented as development conveniences — `MUNARIUM_AUTH_MODE=disabled`, the
compose stacks' example tokens, plaintext direct gRPC — are not vulnerabilities in themselves. A
path by which they reach a production deployment unnoticed is.

## Munarium Enterprise

Munarium Enterprise is a separate proprietary distribution built from this software. A vulnerability
found in it goes through the same private channel; subscribers are notified under their agreement,
and the fix reaches this repository in the ordinary way.

## Secrets

If you have committed a token or key, treat it as compromised: rotate it first, then report it.
