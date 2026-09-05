# Trademark policy

**Munarium™** and **Ioka™** are trademarks of Ioka LLC, claimed through use. The software in this
repository is free under the Apache License, Version 2.0. The names are not part of that grant:
section 6 of the License says so, and this file says what Ioka does and does not permit, so that
you do not have to guess.

The short version: **use the names to say true things about the software, and do not use them to
name your own thing or to suggest Ioka stands behind it.**

## You may, without asking

- **Say what your software works with.** "Built on Munarium", "compatible with Munarium Server
  1.0", "a Munarium adapter", "supports the Munarium Memory Protocol". Accuracy is the only
  condition.
- **Use the name in writing about the project** — documentation, articles, talks, comparisons,
  academic work, job listings, conference sessions.
- **Name the packages you depend on.** `munarium-client`, `munarium-server` and the other released
  artifact names may be quoted as the names they are.
- **Redistribute the software unmodified**, with its name and notices intact, under the License.
- **Run a user group or meetup**, provided its material does not present itself as Ioka's.

## You may not, without written permission

- **Name your product, company, service or domain with "Munarium"**, or with a name close enough
  to be mistaken for it. A hosted service called "Munarium Cloud", a company called "Munarium
  Systems", or a package published as `munarium-anything` you did not get from Ioka are all
  outside this policy.
- **Use "Munarium Enterprise" or "Munarium Certified" at all.** Those name Ioka's commercial
  distribution and its attestation that a specific artifact was tested on a specific platform.
  They are never descriptive terms.
- **Present a modified build as Munarium.** See below.
- **Imply endorsement, partnership, certification or affiliation** that does not exist — including
  by using the marks in a logo, a product badge, or a way that makes Ioka look like the source of
  your offering.

## Forks and modified builds

The License lets you fork and modify freely, and Ioka encourages it. What the License does not
give you is the name.

**A modified build must be named as yours, not as Munarium.** Say what it is derived from —
"a fork of Munarium Server", "based on Munarium Matrix 1.0" — and call the artifact something
else. This is the ordinary rule for open-source projects with a trademark, and it exists so that
"Munarium" continues to mean one specific thing when someone files a bug against it.

If you distribute a modified build, keep `LICENSE` and `NOTICE` intact and mark the files you
changed, as License section 4 requires.

## How Ioka's own releases are identified

An official artifact is one Ioka signed. Every released container image digest is signed with
cosign, and every release tag is signed. **The signature, not the name, is the thing to verify** —
it is checkable, and a name is not.

## Asking

Anything not permitted above, and anything you are unsure about, goes to **info@ioka.io**.
Permission is often given; it is given in writing or not at all.

## Scope

This one file covers every component in the repository. Munarium Enterprise is separately
licensed and its agreement carries its own trademark term.
