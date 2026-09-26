## What and why

<!-- One paragraph. Link the issue if there is one. -->

## Behavior and review scope

<!-- Describe the observable change, its regression and an adjacent valid case.
     Above 500 added + deleted non-generated lines, split by behavior or explain
     why one review is coherent. Report excluded generated files and their checks.
     Size triggers a review decision; it is not a correctness or merge gate. -->

## Validation evidence and limitations

<!-- Name commands, results, source identity, and unavailable coverage. Explain
     what each checker proves and cannot prove; transport success alone does not
     establish semantic success. Keep automatic CI enabled. A waiver must link a
     numbered known gap with the original outcome, owner, reason and revisit
     condition; it does not turn failure or missing evidence into a passed check. -->

## Checks

- [ ] Every commit is signed off (`git commit -s`; the DCO, see CONTRIBUTING.md).
- [ ] The gates for the components this change touches pass locally: `server/gates.ps1`,
      `matrix/test.ps1` (with `boundaries.py` and `doclint.py`), the client gates for the
      language(s) touched, and `clients/check_compatibility.py` if a version moved.
- [ ] New source files carry `SPDX-License-Identifier: Apache-2.0` on the first line; `check_license.py` is green.
- [ ] Documentation that states the changed behavior is updated in this pull request.

## Disclosure

Answer each; "none" is an answer.

1. **Third-party code** in this pull request (any file or fragment you did not write), with its license:
2. **Generated code** (what generated it, from what):
3. **AI-tool provenance** (which tools helped write this, and that you reviewed every line):
4. **Employer or contractual restrictions** on contributing this:

I have the right to submit every file in this pull request under the Apache License 2.0.

## Maintainer self-review

<!-- For a pull request the owner merges on their own review — the compensating control
     for a sole approver. -->

- [ ] Read the whole diff once more after CI went green, as a reviewer would.
- [ ] No credential, hostname, internal path, or private document entered the tree.
- [ ] `server/contract/` is untouched, or was re-vendored by its publisher, never edited.
