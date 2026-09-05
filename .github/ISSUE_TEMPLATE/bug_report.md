---
name: Bug report
about: A component behaves differently from its contract or its documentation
labels: bug
---

<!-- A vulnerability does not go here: SECURITY.md names the private channel. -->

**Component**: server, matrix, or a client (which language)
**Version** (`GET /version` on the server or on Matrix, or the package version):
**Transport** (REST or gRPC), where it matters:
**Operating system and runtime**:

**What you did**

**What you expected** — cite the guide, the API reference, or the conformance
scenario (`server/conformance/SCENARIOS.md`, `matrix/conformance/SCENARIOS.md`)
that says so, if one does.

**What happened instead** — the typed error (its problem slug or refusal code)
or the wrong value, and a stack trace if there is one.

**Smallest reproduction** — a unit-test-sized snippet, credentials and hostnames removed.
