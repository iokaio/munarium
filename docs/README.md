# Munarium guides that span components

Documentation for each component lives beside its code:
[server/docs](../server/docs/README.md), [matrix/docs](../matrix/docs/README.md) and
[clients/docs](../clients/README.md#documentation). This directory holds the guides that do not
belong to one component because they teach a practice that uses several of them together.

| Guide | What it is |
|---|---|
| [lab/README.md](lab/README.md) | **Build your own AI memory governance lab**: a disposable Server, a slice of your documents, an answer key held by a grader, and the loop that grades every shape and runbook before it reaches production. Runnable, with a worked example. |

Every page under this directory is listed from the `README.md` of its own directory, and every
relative link is checked by `scripts/docs_linkcheck.py` in the repository-wide hygiene workflow.
