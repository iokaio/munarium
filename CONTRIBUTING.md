# Contributing to Munarium

Contributions are welcome from anyone. What follows is the whole process; there is no contributor
license agreement to sign.

## Rights and license

- **Every commit carries a Developer Certificate of Origin sign-off**: `git commit -s`, which adds
  `Signed-off-by: Your Name <you@example.com>`. By signing off you certify the
  [DCO](https://developercertificate.org/) — that the work is yours to submit under this
  repository's license, or that you have the right to submit it. A pull request with an unsigned
  commit fails its check.
- **Accepted code is Apache-2.0**, the license of the whole repository ([LICENSE](LICENSE)), by
  section 5 of the license itself: a contribution intentionally submitted for inclusion is licensed
  under the same terms, copyright and patent alike. That is why no CLA exists; a CLA would add the
  right to relicense your contribution later, and that right is not wanted.
- A contribution grants no right to Munarium Enterprise and changes nothing about Ioka's trademarks
  ([TRADEMARK.md](TRADEMARK.md)) or the support boundary ([SUPPORT.md](SUPPORT.md)).

## Disclosure

The pull request template asks four questions; answer each, and "none" is an answer:

1. **third-party code** — any file or fragment you did not write, with its license;
2. **generated code** — what generated it, from what;
3. **AI-tool provenance** — which tools helped, and that you reviewed every line;
4. **employer or contractual restrictions** on what you may contribute.

You must have the right to submit every file in the pull request.

## Process

1. Fork the repository (maintainers: a topic branch) and make the change.
2. Run the gates for the component you touched, below. Every new source file carries
   `SPDX-License-Identifier: Apache-2.0` on its first line — the second, after a shebang or an XML
   declaration — and `check_license.py` at the repository root names any file that does not.
3. Open a pull request against `main` with the local commands run, their results, and reasons for
   any skipped checks. Automatic CI runs DCO and repository hygiene. Full builds and integration
   suites run locally first; a maintainer can dispatch the affected workflow for hosted validation.
   Nothing in a pull request can reach a registry or a deployment.
4. A code owner reviews; Ioka squash-merges.

## Local validation and hosted CI

Read the tracked [AGENTS.md](AGENTS.md) and [CLAUDE.md](CLAUDE.md) contributor
instructions; keep them identical when editing either. Run the affected component
gates below before pushing. Fix compile, formatting, and test failures locally,
reuse build caches, and batch related fixes before requesting another review.
Documentation/workflow-only changes need the relevant documentation, hygiene,
and workflow checks rather than full product rebuilds.

Automatic CI runs license, private-material, secret, documentation, workflow syntax,
client compatibility, and documentation-tool regression checks. Full `server-ci`,
`matrix-ci`, `clients-ci`, and `matrix-server-contract` suites are manual. A maintainer
can select the relevant workflow and branch under Actions > Run workflow, or run
`gh workflow run server-ci.yml --ref <branch>` (substitute the affected workflow).
Use this for Linux reproduction, cross-component changes, or release validation,
not as the normal edit/build loop. These workflows retain their full test steps and
use standard `ubuntu-latest` runners. The manual `clientbuild` publishing workflow
is separate and must not be used to test an ordinary contribution.

Green automatic checks do not prove full integration coverage. Record local
evidence in the PR, and arrange the specific manual suite before merge if a
required local check cannot run. Do not claim unavailable or skipped checks passed.

## Gates per component

| Component | Run before you push |
|---|---|
| `server/` | `.\gates.ps1` — everything CI runs, against a compose PostgreSQL. Or the ladder: `.\test.ps1` (offline), `-Postgres`, `-BlackBox`, `-Enterprise`, `-Cluster` |
| `matrix/` | `.\test.ps1` (offline tier: unit tests, boundaries, contract checks, doclint), `-Gates` for fmt and clippy, `-BlackBox` for the compose tiers |
| `clients/rust` | `cargo fmt -p munarium-client -p munarium-client-conformance --check` (package-scoped: `--all` reaches the server's wire crates), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` |
| `clients/python`, `clients/matrix-python` | `ruff check`, `ruff format --check`, `mypy`, `pytest` |
| `clients/dotnet`, `clients/matrix-dotnet` | `dotnet build` (warnings are errors), `dotnet test` |
| `clients/java`, `clients/matrix-java` | `./gradlew build` |
| Everything | `py check_license.py` and `py clients/check_compatibility.py` from the repository root |

Rules the gates enforce that are easy to trip:

- **Documentation.** Every directory under a component's `docs/` has an index; an unlisted document
  is an unread document. The server's `docs_coverage` test fails the build when a served route or a
  problem slug is missing from the API reference, or when a relative link under `docs/` is dead.
  The root `docs/` tree and the root markdown files are held to the same two rules by
  `scripts/docs_linkcheck.py`, which the repository-wide hygiene workflow runs.
- **Migrations are additive-only**, enforced by the local gates and full CI suites. This repository has been at 1.0 since its first
  release, so an applied migration is never edited: `sqlx` validates a checksum per migration and
  an edit stops the server booting against any existing database.
- **The kernels stay pure.** The local gates and full CI suites reject any change that lets `munarium-core` or
  `munarium-matrix-core` depend on the web, database or HTTP-client layers.
- **Matrix never depends on a server crate.** `matrix/scripts/boundaries.py` rules on the shipping
  dependency graph. The two talk over a wire contract, not a crate edge — one repository does not
  change that.
- **The contract directories are not hand-edited.** `server/contract/matrix/` is a locked vendored
  copy; a change arrives as a re-cut, not an edit.

## Conduct and venues

[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) applies everywhere. Questions go to GitHub Discussions,
defects to Issues, and suspected vulnerabilities to the private channel [SECURITY.md](SECURITY.md)
names — never to a public issue or a proof-of-concept pull request.

## Protected files

Only Ioka changes `LICENSE`, `NOTICE`, `TRADEMARK.md`, this file, `CODE_OF_CONDUCT.md`,
`SECURITY.md`, `SUPPORT.md`, anything under `.github/`, the contract directories, and any signing or
release configuration. A pull request that touches them is declined unless a maintainer opened it.
