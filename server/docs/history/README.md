# Pull-request history

Read the [working outline](outline.md) for the merged history through PR #46
and the separately labeled open follow-ups #47–#49, reviewed 24 September 2026.
This is a dated architectural record, not the current API or release reference.
PR headings link to the public source records. Validation and release statements
summarize those records; this review did not rerun historical release exercises.

## Diagrams

The outline embeds PNGs. The matching SVGs below are the editable sources.
Keep both formats together when correcting a diagram; all PNGs use twice the
SVG's logical width and height. Dates in the timeline are UTC merge dates.

| Diagram | Editable source | Rendered image |
|---|---|---|
| Pr timeline | [SVG](images/00-pr-timeline.svg) | [PNG](images/00-pr-timeline.png) |
| Initial architecture | [SVG](images/01-initial-architecture.svg) | [PNG](images/01-initial-architecture.png) |
| Release certification | [SVG](images/02-release-certification.svg) | [PNG](images/02-release-certification.png) |
| Datastore layers | [SVG](images/03-datastore-layers.svg) | [PNG](images/03-datastore-layers.png) |
| Turn model override | [SVG](images/04-turn-model-override.svg) | [PNG](images/04-turn-model-override.png) |
| Server api generation | [SVG](images/05-server-api-generation.svg) | [PNG](images/05-server-api-generation.png) |
| Collection query | [SVG](images/06-collection-query.svg) | [PNG](images/06-collection-query.png) |
| Clientbuild train | [SVG](images/07-clientbuild-train.svg) | [PNG](images/07-clientbuild-train.png) |
| Usage settlement | [SVG](images/08-usage-settlement.svg) | [PNG](images/08-usage-settlement.png) |
| Vcp roadmap | [SVG](images/09-vcp-roadmap.svg) | [PNG](images/09-vcp-roadmap.png) |
| Validation outcomes | [SVG](images/10-validation-outcomes.svg) | [PNG](images/10-validation-outcomes.png) |

## Rendering corrections

These are native SVG diagrams; edit their text and geometry directly. A local
headless Chromium browser can render each SVG without a network service. For
example, from this directory with Chrome on `PATH`:

```powershell
chrome --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=2 --window-size=1500,860 --screenshot=roadmap.png "file:///absolute/path/to/09-vcp-roadmap.svg"
```

Use the source SVG's `width` and `height` for `--window-size`, write the result to
the corresponding PNG, and inspect the rendered image for clipping, overlaps and
legibility. A separate temporary `--user-data-dir` avoids reusing an interactive
browser profile. Rendering here was checked with local Chrome at 2× scale.

When updating the history, verify merge dates and state from the linked PRs.
Distinguish proposed work, merged source and published artifacts. Preserve
historical versions and label new status with its review date. Contributor
attestations and human-review checkboxes must not be inferred from a merge.
