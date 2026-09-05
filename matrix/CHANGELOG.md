# Munarium Matrix — release notes

## 1.0.0

The first public release.

**What 1.0 commits to.** The wire contract, the asset grammar, the refusal
registry and the adapter interface, all under semantic versioning. It does not
claim every planned capability is finished. The list below is that gap, stated
rather than implied.

### Accepted limitations

- **Core ships four adapters**: `postgres`, `mysql`, `sqlserver` and
  `landing`. The analytics-platform adapters — Databricks, Snowflake,
  BigQuery, Cube and dbt — are **Munarium Matrix Enterprise**, a separate
  proprietary distribution, and their crates are not in this repository.
- **The asset grammar does not change between editions.** A core build still
  *accepts* a DataSource naming one of those adapters, and refuses it at
  execution by name with `adapter_not_available`, rather than failing to parse
  it. That is deliberate: the grammar is one contract, and a refusal that says
  which product serves the asset is more useful than a parse error.
- **Out-of-tree adapters register through `adapters::AdapterRegistry`.** The
  `AdapterFactory` trait is the public seam; there is no patch point inside
  `runtime::open_adapter`.
- **The conformance registry describes this repository.** 86 scenarios across
  six tiers — offline, postgres, grpc, http, mysql and sqlserver — every one of
  which this tree can build and run, on compose or on every push. Scenario
  names for adapters that are not here are not listed: a registry that
  advertises coverage the tree cannot execute is the failure mode the suite
  exists to prevent.
- **The Helm chart's image repository is a placeholder** (`<your registry>/…`).
  A default `helm install` will not pull. Supply your own registry, or build
  the image from this repository.

### Verification

`cargo clippy --workspace --all-features --all-targets -- -D warnings` and
`cargo test --workspace --all-features` are both green, and both run in CI.
`scripts/boundaries.py` enforces the adapter inventory, the no-server-crate
rule, the rustls-only rule and the additive-migration rule.
