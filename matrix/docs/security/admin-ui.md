# The admin console: threat model and posture

Covers `/admin/*` in `munarium-matrix-server`.

The console is a **write surface on a deployed service**. That is the whole
reason this document exists: an internal console can assume loopback, and
Matrix cannot. What follows is what it defends against, what it does not, and
why each choice is the one it is.

## What it is

Server-rendered HTML with inline SVG, served by the Matrix binary on the REST
port. **Zero JavaScript** — not "progressive enhancement that degrades", none.
That is a security decision before it is an aesthetic one: with no script on
any page, the CSP can name no script source at all, and a future edit that adds
one is blocked by the browser rather than shipped unnoticed.

No separate image, no Node at runtime, no CDN, no second listener, no writable
filesystem, no embedded assets beyond what is compiled in.

## Reachability

| Condition | Result |
| --- | --- |
| role `query`, `sync`, `reconcile` | **404** on every `/admin` path |
| `MUNARIUM_MATRIX_ADMIN=disabled` | **404**, including `/admin/login` |
| role `control` or `all`, admin enabled | served |

The surface is **absent**, not guarded. The routes are not mounted, so there is
no check to misconfigure back on, and a login page that still answered under
`disabled` would advertise a console that is not there. Both are asserted at
the router.

## Authentication

Mgmt-only. Two credential paths:

- `Authorization: Bearer <mgmt token>` — curl, scripts, a fronting proxy;
- the `__munarium_matrix_admin` cookie that `POST /admin/login` sets.

The cookie holds the mgmt token itself. That is the same posture the server's
console documents, and the same trade: no session table, no server-side
expiry, and the credential dies when the operator's browser session does.
Attributes: `HttpOnly`, `SameSite=Strict`, `Path=/admin`, and `Secure` **only
when the request arrived over TLS** (`X-Forwarded-Proto: https`, which is what
a TLS-terminating ingress sends). A `Secure` cookie on a plain-http loopback deployment is
one the browser silently drops, and the symptom is "login does not work".

A failed authentication **redirects** to the login form rather than answering
401: the overwhelmingly common cause is a browser with no cookie yet. A script
reads the `Location` header and learns the same thing.

**rw and ro tokens are refused.** They are valid credentials with the wrong
role, and that is the interesting case the tests cover — not a garbage token.

## Authorization: the role split is the point

A leaked mgmt token cannot change what the system does.

- **Reads and administration are mgmt.** Every page.
- **Every write asks for the rw credential in the form, per submission**, and
  the console never stores one. That is the same split `/v1` draws: mgmt reads
  and administers; applying an asset, running a sync, promoting a mapping are
  rw.
- An rw token **for another tenant** is refused rather than silently applied
  across the boundary.
- An mgmt token offered as the rw credential is refused, which is the whole
  reason a second credential is asked for at all.

## No second policy

Every action handler calls the **same `op_*` function** `/v1` calls — not a
copy of it, and not a privileged in-process shortcut past it. Same tenant
resolution, same role checks, same gates, same budget, same evidence sealing,
same journal row, with `via: admin-ui` and the rw principal as actor.

This is what makes "there is no second policy" true by construction rather than
by review: a gate added to `/v1` tomorrow applies to the console without anyone
remembering to add it. The pattern is the one `execute.rs` established for REST
and gRPC.

`via` is a **parameter**, never a request header. An audit field a caller can
set is one nobody can trust.

## CSRF

A stateless synchronizer token on every state-changing form:

```
sha256( boot_secret || sha256( boot_secret ":" credential ) )
```

- **Bound to the credential**, so a token minted for one operator does not
  authorize a form submitted with another's.
- **Bound to a per-process random boot secret**, so it dies with the process. A
  form left open across a restart is refused rather than replayed. There is no
  session table because there are no sessions: the credential *is* the session.
- Compared in **constant time** over the fixed-length hex.

## Origin and Host

On top of CSRF, every write compares the request's `Origin` (falling back to
`Referer`) authority against its `Host`.

Deliberately: an **absent** Origin is allowed. Some same-origin form posts send
none, `curl` sends none, and refusing there would break local operation for no
gain while the CSRF token is still required. What is refused is an Origin that
is **present and disagrees**.

`POST /admin/login` takes the Origin check but cannot take a CSRF token —
there is no session to bind one to yet. That is the one honest exception.

## Response headers

On **every** admin response, including the login page and the redirect. A
header set only on pages that render is a header missing exactly where a
redirect could be framed.

| Header | Value |
| --- | --- |
| `Content-Security-Policy` | `default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'` |
| `X-Frame-Options` | `DENY` |
| `X-Content-Type-Options` | `nosniff` |
| `Referrer-Policy` | `same-origin` — **not** `no-referrer`, which was the value until 2026-08-30: under it the Fetch spec turns the `Origin` header of a form POST into `null`, the Origin/Host check refused every browser submission, and the login form could not sign anyone in. Only a browser could show it (reqwest sends no `Origin`, which the check accepts); the first run of `ui-smoke` did, in its second assertion. `same-origin` still leaks nothing to a third party |
| `Cache-Control` | `no-store, max-age=0` |

`style-src 'unsafe-inline'` is the one relaxation: the stylesheet is inlined so
there is no second request that could fail while a page renders. There is no
`script-src` **at all**, which is stricter than allowing `'self'`.

`no-store` is not incidental — these pages read live operational state, and a
cached copy in a shared proxy would show one operator another tenant's rows.

## What never renders

- **Secrets.** `credentialRef` is a name. A probe result is reachable /
  unreachable with the adapter's typed reason. Connection details come from the
  applied YAML, which the validator already refuses to accept with an inline
  secret in it. A test greps this module's own source for `resolve_secret` and
  friends — a blunt instrument, and the right one for a failure that looks like
  "someone added a field to a table without thinking about what is in it".
- **Evidence rows.** An evidence id is shown; resolving it is the *server's*
  job, and that resolver is access-checked per session. The console shows
  manifests, ids, counts and hashes.
- **Journal payloads.** They are redacted at write time and the console offers
  no reveal. A parameter value is customer data, and an operator console is the
  wrong place to make it readable.
- **The login token.** The password field is re-rendered with no `value`,
  asserted by a test — a re-echoed credential lives in the page source and in
  the browser's back-forward cache.

## Fronting proxies

A trusted view-only proxy may send `X-Munarium-Admin-View-Only: 1` — the same
header name the server's console uses, so one proxy fronting both sends one
header. Every action then renders as a note instead of a button, **and writes
are refused**. Rendering-only would leave a POST a determined client could
still make behind a proxy that cannot pass it.

The header only ever removes capability, so it is parsed leniently: anything
but an explicit `0`/`false`/`no` turns it on.

## What this does NOT defend against

Stated plainly, because a threat model that only lists wins is not one.

- **A compromised mgmt token reads everything** this console shows, for its
  tenant. It cannot write without an rw token, but it can read the registry,
  the journal metadata, and every operational number.
- **There is no rate limit on the login form.** The static-token auth mode
  compares in constant time, so a token cannot be recovered a byte at a time,
  but an attacker with network reach can guess as fast as the service answers.
  Deployments front this with ingress-level throttling; that is the current
  posture, not a claim that it is handled here.
- **There is no session expiry** beyond the browser session. The cookie carries
  the token, so revoking access means rotating the token.
- **TLS is the ingress's job.** Nothing here terminates it, and the `Secure`
  attribute follows `X-Forwarded-Proto` — which means a proxy that sets that
  header untruthfully gets the cookie attribute it asked for.
- **A tenant's operator sees that tenant's data only** by the same tenant
  scoping `/v1` uses. There is no additional compartment check here; the
  console is not a data-access surface.

## Tests

`src/munarium-matrix-server/src/admin/security_tests.rs` drives the **assembled
router**, not the helpers, because a correct check behind a route that never
calls it is a check that does not exist. That distinction paid immediately: the
first run found the login redirect leaving without its security headers.

Covered: role gating, the disable switch, mgmt-only on every read page
(enumerated, not spot-checked), the header set on renders and redirects, cookie
attributes with and without TLS, non-mgmt login setting no cookie, a missing
CSRF token, another credential's CSRF token, a previous process's CSRF token, a
cross-origin post with a *valid* token, the rw credential check, an mgmt token
offered as rw, and the view-only path.
