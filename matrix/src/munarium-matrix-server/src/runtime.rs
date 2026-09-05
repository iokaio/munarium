// SPDX-License-Identifier: Apache-2.0
//! Turning registered assets into live connections.
//!
//! Everything below this module is pure or unit-testable; everything above it
//! is HTTP. This is the seam where a `DataSource` document — YAML that someone
//! applied — becomes an open pool against a real database, and it is the one
//! place that decides what a stored asset is *allowed* to do.
//!
//! Three rules live here because they must hold on every path that reaches a
//! source, and putting them in the handlers would mean re-deciding them per
//! route:
//!
//! 1. **Egress is default-deny at open time, not only at validate time.** An
//!    asset applied before a host was removed from the allowlist must not keep
//!    working. The check is re-run against the *current* document every time a
//!    connection is opened.
//! 2. **Secrets resolve at call time and are never stored.** A `credentialRef`
//!    is a name; it becomes a value here, is handed to the adapter, and is
//!    never journaled, logged or returned.
//! 3. **An adapter that cannot honour the asset refuses instead of degrading.**
//!    An unconfigured landing root, a missing credential, an adapter with no
//!    live path — each is a typed refusal naming what is missing.

use crate::state::AppState;
use munarium_matrix_adapter::SourceAdapter;
use munarium_matrix_core::Refusal;
use munarium_matrix_server_client::{HttpServerClient, ServerClient};
use munarium_matrix_types::assets::{AdapterKind, DataSourceDoc};
use munarium_matrix_types::{Asset, QueryContractDoc};
use std::sync::Arc;
use std::time::Duration;

/// Read one registered asset and parse it, or say precisely which of the two
/// failed. "Not found" and "stored but unparseable" are different operator
/// problems and must not collapse into one message.
pub async fn load_asset(
    state: &AppState,
    tenant: &str,
    kind: &str,
    name: &str,
) -> Result<Asset, Refusal> {
    let stored = state
        .store
        .get_asset(tenant, kind, name)
        .await
        .map_err(|e| match e {
            munarium_matrix_store::StoreError::NotFound { .. } => {
                Refusal::not_covered(format!("no {kind} named '{name}' is registered"))
            }
            other => Refusal::source_unavailable(format!("registry read failed: {other}")),
        })?;
    stored.parse().map_err(|e| {
        Refusal::invalid(
            "registry_corrupt",
            format!(
                "{kind} '{name}' is stored but does not parse ({e}). It was applied by a \
                 different version of this schema; re-apply it."
            ),
        )
    })
}

pub async fn load_data_source(
    state: &AppState,
    tenant: &str,
    name: &str,
) -> Result<DataSourceDoc, Refusal> {
    match load_asset(state, tenant, "DataSource", name).await? {
        Asset::DataSource(d) => Ok(*d),
        other => Err(Refusal::invalid(
            "wrong_kind",
            format!("'{name}' is a {}, not a DataSource", other.kind()),
        )),
    }
}

pub async fn load_contract(
    state: &AppState,
    tenant: &str,
    name: &str,
) -> Result<QueryContractDoc, Refusal> {
    match load_asset(state, tenant, "QueryContract", name).await? {
        Asset::QueryContract(d) => Ok(*d),
        other => Err(Refusal::invalid(
            "wrong_kind",
            format!("'{name}' is a {}, not a QueryContract", other.kind()),
        )),
    }
}

/// Either semantic asset by name: a `kind` hint from the route,
/// or — over gRPC and the contracts route, where the intent's `kind` alone
/// says "semantic" — a metric view first, then a data view.
pub async fn load_semantic_view(
    state: &AppState,
    tenant: &str,
    name: &str,
    hint: Option<&str>,
) -> Result<SemanticViewDoc, Refusal> {
    let kinds: &[&str] = match hint {
        Some("DataView") => &["DataView"],
        Some("MetricView") => &["MetricView"],
        _ => &["MetricView", "DataView"],
    };
    let mut last = None;
    for kind in kinds {
        match load_asset(state, tenant, kind, name).await {
            Ok(Asset::MetricView(d)) => return Ok(SemanticViewDoc::Metric(*d)),
            Ok(Asset::DataView(d)) => return Ok(SemanticViewDoc::Native(*d)),
            Ok(other) => {
                return Err(Refusal::invalid(
                    "wrong_kind",
                    format!("'{name}' is a {}, not a semantic view", other.kind()),
                ))
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| Refusal::not_covered(format!("no semantic view named '{name}'"))))
}

/// An owned semantic asset; `as_view()` borrows it for the workers.
pub enum SemanticViewDoc {
    Metric(munarium_matrix_types::MetricViewDoc),
    Native(munarium_matrix_types::DataViewDoc),
}

impl SemanticViewDoc {
    pub fn as_view(&self) -> munarium_matrix_workers::SemanticView<'_> {
        match self {
            SemanticViewDoc::Metric(d) => munarium_matrix_workers::SemanticView::Metric(d),
            SemanticViewDoc::Native(d) => munarium_matrix_workers::SemanticView::Native(d),
        }
    }
}

/// Re-check egress against the document as it stands NOW.
///
/// Validation already refuses an empty allowlist at apply time. This is the
/// second gate, and it is the one that matters operationally: assets are
/// long-lived, allowlists change, and a connection opened tomorrow must be
/// judged by tomorrow's document.
fn check_egress(
    doc: &DataSourceDoc,
    host: Option<&str>,
    default_deny: bool,
) -> Result<(), Refusal> {
    let allow = &doc.spec.egress.allow_hosts;
    if allow.is_empty() {
        if default_deny {
            return Err(Refusal::policy_denied(format!(
                "source '{}' has an empty egress allowlist and egress is default-deny; \
                 nothing is reachable until a host is listed",
                doc.metadata.name
            )));
        }
        return Ok(());
    }
    match host {
        Some(h) if !allow.iter().any(|a| a == h) => Err(Refusal::policy_denied(format!(
            "host '{h}' is not in source '{}' egress allowlist {allow:?}",
            doc.metadata.name
        ))),
        _ => Ok(()),
    }
}

fn connection_str(doc: &DataSourceDoc, key: &str) -> Option<String> {
    doc.spec
        .connection
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Open an adapter for a registered source.
///
/// The returned adapter owns its own pool. Callers are expected to open one per
/// unit of work and drop it — a per-source pool cache is a later concern and
/// would need invalidation on every re-apply to stay correct, which is exactly
/// the kind of thing that is wrong for a year before anyone notices.
pub async fn open_adapter(
    state: &AppState,
    doc: &DataSourceDoc,
) -> Result<Box<dyn SourceAdapter>, Refusal> {
    match doc.spec.adapter {
        AdapterKind::Postgres => {
            let host = connection_str(doc, "host");
            check_egress(doc, host.as_deref(), state.config.egress_default_deny)?;

            let reference = doc.spec.credential_ref.as_deref().ok_or_else(|| {
                Refusal::invalid(
                    "missing_credential",
                    format!(
                        "source '{}' is a postgres source with no credentialRef; \
                         there is no ambient credential to fall back to, by design",
                        doc.metadata.name
                    ),
                )
            })?;
            // The one moment a secret exists as a value. It goes straight into
            // the adapter and is never returned, logged or journaled.
            let url = crate::config::resolve_secret(reference).map_err(|e| {
                Refusal::invalid(
                    "credential_unresolved",
                    format!(
                        "credentialRef '{reference}' for source '{}' did not resolve: {e}",
                        doc.metadata.name
                    ),
                )
            })?;
            let schema = connection_str(doc, "schema").unwrap_or_else(|| "public".into());
            let adapter = munarium_matrix_adapter_postgres::PostgresAdapter::connect(
                &doc.metadata.name,
                &url,
                &schema,
                state.config.db_max_conns.min(4),
            )
            .await?;
            // A slot or publication the operator named (2026-08-30); absent,
            // the `munarium_matrix_<source>` convention stands.
            let adapter = match doc.spec.sync.as_ref().and_then(|s| s.cdc.as_ref()) {
                Some(cdc) => {
                    adapter.with_cdc_objects(cdc.slot.as_deref(), cdc.publication.as_deref())
                }
                None => adapter,
            };
            Ok(Box::new(adapter))
        }

        AdapterKind::Mysql => {
            let host = connection_str(doc, "host");
            check_egress(doc, host.as_deref(), state.config.egress_default_deny)?;

            let reference = doc.spec.credential_ref.as_deref().ok_or_else(|| {
                Refusal::invalid(
                    "missing_credential",
                    format!(
                        "source '{}' is a mysql source with no credentialRef; \
                         there is no ambient credential to fall back to, by design",
                        doc.metadata.name
                    ),
                )
            })?;
            // The one moment a secret exists as a value. It goes straight into
            // the adapter and is never returned, logged or journaled.
            let url = crate::config::resolve_secret(reference).map_err(|e| {
                Refusal::invalid(
                    "credential_unresolved",
                    format!(
                        "credentialRef '{reference}' for source '{}' did not resolve: {e}",
                        doc.metadata.name
                    ),
                )
            })?;
            let schema = connection_str(doc, "schema").unwrap_or_else(|| "public".into());
            let adapter = munarium_matrix_adapter_mysql::MySqlAdapter::connect(
                &doc.metadata.name,
                &url,
                &schema,
                state.config.db_max_conns.min(4),
            )
            .await?;
            Ok(Box::new(adapter))
        }

        AdapterKind::Sqlserver => {
            // The credential is a whole ADO.NET connection string, which
            // carries the host as well as the password — so the host is ALSO
            // declared in `connection`, and validation refuses a sqlserver
            // source without one. A secret cannot be read at validation time,
            // and an egress allowlist checked against nothing is not a check.
            let host = connection_str(doc, "host");
            check_egress(doc, host.as_deref(), state.config.egress_default_deny)?;

            let reference = doc.spec.credential_ref.as_deref().ok_or_else(|| {
                Refusal::invalid(
                    "missing_credential",
                    format!(
                        "source '{}' is a sqlserver source with no credentialRef; \
                         there is no ambient credential to fall back to, by design",
                        doc.metadata.name
                    ),
                )
            })?;
            // The one moment a secret exists as a value. It goes straight into
            // the adapter and is never returned, logged or journaled.
            let connection_string = crate::config::resolve_secret(reference).map_err(|e| {
                Refusal::invalid(
                    "credential_unresolved",
                    format!(
                        "credentialRef '{reference}' for source '{}' did not resolve: {e}",
                        doc.metadata.name
                    ),
                )
            })?;
            // `dbo` is SQL Server's default schema, as `public` is Postgres's.
            let schema = connection_str(doc, "schema").unwrap_or_else(|| "dbo".into());
            let adapter = munarium_matrix_adapter_sqlserver::SqlServerAdapter::connect(
                &doc.metadata.name,
                &connection_string,
                &schema,
                state.config.db_max_conns.min(4),
            )
            .await?;
            Ok(Box::new(adapter))
        }

        AdapterKind::Landing => {
            // `store: file` (the default) reads under MUNARIUM_MATRIX_FILE_ROOT;
            // `store: az` reads a blob container with the process's ambient
            // identity (2026-08-30). The manifest path is relative to the
            // prefix either way; `prefix` is a folder, and for the file store
            // it is folded into the manifest path as before.
            let store_kind = connection_str(doc, "store").unwrap_or_else(|| "file".into());
            match store_kind.as_str() {
                "az" | "azure" => {
                    let account = connection_str(doc, "account").ok_or_else(|| {
                        Refusal::invalid(
                            "not_covered",
                            "an `az` landing source names `connection.account`",
                        )
                    })?;
                    let container = connection_str(doc, "container").ok_or_else(|| {
                        Refusal::invalid(
                            "not_covered",
                            "an `az` landing source names `connection.container`",
                        )
                    })?;
                    // The blob endpoint IS the egress host, and it is checked
                    // like any other: a container in an account the allowlist
                    // does not name is not readable, whatever the identity
                    // could do.
                    let host = format!("{account}.blob.core.windows.net");
                    check_egress(doc, Some(&host), state.config.egress_default_deny)?;
                    let prefix = connection_str(doc, "prefix").unwrap_or_default();
                    let manifest =
                        connection_str(doc, "manifest").unwrap_or_else(|| "manifest.json".into());
                    Ok(Box::new(
                        munarium_matrix_adapter_landing::LandingAdapter::new_azure(
                            &doc.metadata.name,
                            &account,
                            &container,
                            &prefix,
                            &manifest,
                        )?,
                    ))
                }
                "file" => {
                    check_egress(doc, connection_str(doc, "host").as_deref(), false)?;
                    let root = state.config.file_root.as_deref().ok_or_else(|| {
                        Refusal::invalid(
                            "landing_root_unset",
                            "MUNARIUM_MATRIX_FILE_ROOT is unset, so a landing source has no base \
                             directory. Set it to the mount holding the export.",
                        )
                    })?;
                    let manifest = connection_str(doc, "manifest")
                        .or_else(|| {
                            connection_str(doc, "prefix").map(|p| format!("{p}manifest.json"))
                        })
                        .unwrap_or_else(|| "manifest.json".into());
                    Ok(Box::new(
                        munarium_matrix_adapter_landing::LandingAdapter::new_file(
                            &doc.metadata.name,
                            root,
                            &manifest,
                        ),
                    ))
                }
                other => Err(Refusal::invalid(
                    "not_covered",
                    format!(
                        "landing store '{other}' is not one this build reads; `file` and `az` are"
                    ),
                )),
            }
        }
        // Every other kind: whatever this build registered, else a refusal that
        // names what would serve it. The Enterprise adapters are not in this
        // repository; they reach the runtime through `adapters::AdapterRegistry`
        // rather than through a patch to this function.
        kind => match state.adapters.get(kind) {
            Some(factory) => factory.open(state, doc).await,
            None => Err(Refusal::adapter_not_available(kind.as_str())),
        },
    }
}

/// Build a client for the munarium-server.
///
/// A role that seals evidence cannot run without one, and discovering that at
/// the moment of the first seal — after the source has been read and the work
/// paid for — is the wrong time. Callers open this FIRST.
pub fn open_server_client(state: &AppState) -> Result<Box<dyn ServerClient>, Refusal> {
    let url = state.config.server_url.as_deref().ok_or_else(|| {
        Refusal::source_unavailable(
            "MUNARIUM_MATRIX_SERVER_URL is unset. Sealing evidence needs a server; a role \
             that seals cannot run without one.",
        )
    })?;
    let token = match state.config.server_token_ref.as_deref() {
        Some(reference) => crate::config::resolve_secret(reference).map_err(|e| {
            Refusal::invalid(
                "credential_unresolved",
                format!("MUNARIUM_MATRIX_SERVER_TOKEN_REF '{reference}' did not resolve: {e}"),
            )
        })?,
        None => String::new(),
    };
    let client =
        HttpServerClient::new_http1(url, &token, Duration::from_secs(30)).map_err(|e| {
            Refusal::source_unavailable(format!("could not build a server client: {e}"))
        })?;
    Ok(Box::new(client))
}

/// Everything a unit of work needs, opened together so a missing piece fails
/// before any source is touched.
pub struct Wiring {
    pub adapter: Box<dyn SourceAdapter>,
    pub server: Box<dyn ServerClient>,
    pub source: DataSourceDoc,
}

/// The planner surface a source declares, if any.
///
/// Read out of `spec.connection`, which is adapter-owned by design: Genie is a
/// Databricks surface, and putting it in the shared `DataSourceSpec` would ask
/// every other adapter to carry a field it can never use — and would move the
/// cross-tree contract for a vendor feature.
///
/// A malformed block returns `None` rather than an error here. The asset's own
/// validation already refused it at apply time, so reaching this with one
/// means somebody wrote to the registry underneath us; answering "no planner"
/// is the fail-closed reading.
pub fn planner_spec(
    source: &DataSourceDoc,
) -> Option<munarium_matrix_adapter::planner::PlannerSpec> {
    let raw = source.spec.connection.get("genie")?;
    serde_json::from_value(raw.clone()).ok()
}

pub async fn wire(
    state: &Arc<AppState>,
    tenant: &str,
    source_name: &str,
) -> Result<Wiring, Refusal> {
    let source = load_data_source(state, tenant, source_name).await?;
    // Server first: it is the cheap check, and failing it after opening a
    // source pool would mean paying for a connection we cannot use.
    let server = open_server_client(state)?;
    let adapter = open_adapter(state, &source).await?;
    Ok(Wiring {
        adapter,
        server,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(hosts: &[&str]) -> DataSourceDoc {
        let yaml = format!(
            "apiVersion: munarium.ioka.io/v1\nkind: DataSource\n\
             metadata: {{ name: crm, version: 1 }}\n\
             spec:\n  adapter: postgres\n  connection: {{ host: db.example.com }}\n\
             \x20 credentialRef: matrix-crm\n  egress: {{ allowHosts: [{}] }}\n\
             \x20 authorization: {{ strategy: source_native }}\n",
            hosts.join(", ")
        );
        match munarium_matrix_types::parse_asset(&yaml).expect("fixture parses") {
            Asset::DataSource(d) => *d,
            _ => unreachable!(),
        }
    }

    #[test]
    fn an_allowed_host_passes_and_a_different_one_is_denied() {
        let d = doc(&["db.example.com"]);
        assert!(check_egress(&d, Some("db.example.com"), true).is_ok());
        let err = check_egress(&d, Some("evil.example.com"), true).expect_err("must deny");
        assert_eq!(err.class, munarium_matrix_core::RefusalClass::Denied);
    }

    #[test]
    fn an_empty_allowlist_denies_under_default_deny() {
        // The operational case this exists for: the asset was applied when the
        // list had an entry, and the entry was later removed. Open time, not
        // apply time, is what decides.
        let d = doc(&[]);
        let err = check_egress(&d, Some("db.example.com"), true).expect_err("must deny");
        assert!(err.message.contains("default-deny"), "{}", err.message);
        assert!(
            check_egress(&d, Some("db.example.com"), false).is_ok(),
            "with default-deny off an empty list is 'unset', not 'nothing'"
        );
    }

    #[test]
    fn a_host_is_not_checked_when_the_adapter_has_no_host_to_check() {
        // A file-backed landing source reaches no network host. Denying it for
        // having no host would be a check misfiring on its own absence.
        let d = doc(&["blob.core.windows.net"]);
        assert!(check_egress(&d, None, true).is_ok());
    }

    #[test]
    fn connection_values_are_read_as_strings_or_absent_never_coerced() {
        let d = doc(&["db.example.com"]);
        assert_eq!(
            connection_str(&d, "host").as_deref(),
            Some("db.example.com")
        );
        assert_eq!(connection_str(&d, "nosuchkey"), None);
    }

    fn databricks_doc(genie: &str) -> DataSourceDoc {
        let yaml = format!(
            "apiVersion: munarium.ioka.io/v1\nkind: DataSource\n\
             metadata: {{ name: dbx, version: 1 }}\n\
             spec:\n  adapter: databricks\n\
             \x20 connection:\n    host: adb-1.2.azuredatabricks.net\n\
             \x20   warehouseId: abc123\n    catalog: main\n    schema: crm\n\
             \x20   auth: personal_access_token\n{genie}\
             \x20 credentialRef: matrix-dbx\n\
             \x20 egress: {{ allowHosts: [adb-1.2.azuredatabricks.net] }}\n\
             \x20 authorization: {{ strategy: source_native }}\n"
        );
        match munarium_matrix_types::parse_asset(&yaml).expect("fixture parses") {
            Asset::DataSource(d) => *d,
            _ => unreachable!(),
        }
    }

    /// The asset's `genie:` block is read as a planner spec.
    ///
    /// The pin this exists for: the planner route and the adapter builder must
    /// read the SAME asset block. A builder that hard-set this to `None` once
    /// meant the route parsed a spec, spent nothing, and asked an adapter that
    /// had never been told it had a planner -- so the route could not succeed
    /// on any deployment, and no registered scenario said so.
    #[test]
    fn the_assets_genie_block_is_read_as_a_planner_spec() {
        let with = databricks_doc("    genie:\n      spaceId: sp-1\n      trustedAssets: [ta-9]\n");
        let spec = planner_spec(&with).expect("the declared planner surface is read");
        assert_eq!(spec.space_id, "sp-1");
        assert_eq!(spec.trusted_assets, vec!["ta-9".to_string()]);

        assert!(
            planner_spec(&databricks_doc("")).is_none(),
            "no block means no planner, not a default"
        );
    }
}
