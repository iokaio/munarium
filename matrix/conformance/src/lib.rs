// SPDX-License-Identifier: Apache-2.0
//! Conformance scenarios.
//!
//! Every scenario is a named property from the guarantee map (G1–G7) or from a
//! phase exit gate. A scenario is written ONCE and runs in whichever tiers it
//! can: pure ones run offline, store-backed ones are `#[ignore]`d unless
//! `MUNARIUM_MATRIX_TEST_DATABASE_URL` is set, and HTTP ones additionally need
//! `MUNARIUM_MATRIX_TEST_HTTP`.
//!
//! That gating is deliberate. A suite that silently skips is a suite that
//! reports green when it tested nothing, so [`tier`] prints what it is running
//! and the runner (`test.ps1`) names the tier it asked for.

#![forbid(unsafe_code)]
#![allow(clippy::result_large_err)]

pub mod admin_tier;
pub mod measure;
pub mod scenarios;

/// Which tier this process can run, decided by the environment rather than by
/// a flag, so CI and a laptop make the same decision the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tier {
    /// No external anything.
    Offline,
    /// A real Postgres is available.
    Postgres { url: String },
    /// A Matrix server is running and reachable.
    Http {
        url: String,
        token: Option<String>,
        db: String,
    },
}

pub fn tier() -> Tier {
    let db = std::env::var("MUNARIUM_MATRIX_TEST_DATABASE_URL").ok();
    let http = std::env::var("MUNARIUM_MATRIX_TEST_HTTP").ok();
    match (db, http) {
        (Some(db), Some(_)) => Tier::Http {
            url: std::env::var("MUNARIUM_MATRIX_TEST_URL")
                .unwrap_or_else(|_| "http://localhost:8180".into()),
            token: std::env::var("MUNARIUM_MATRIX_TEST_TOKEN").ok(),
            db,
        },
        (Some(url), None) => Tier::Postgres { url },
        _ => Tier::Offline,
    }
}

/// The gRPC plane under test, or `None` when the operator did not ask for
/// that tier. `MUNARIUM_MATRIX_TEST_GRPC` is a URL (`http://127.0.0.1:50151`
/// on compose, `https://<fqdn>` on a deployment, where TLS terminates at the
/// h2 ingress). Like every other tier: unset is a skip that says so, and a
/// URL that does not answer is a failure.
pub fn grpc_url() -> Option<String> {
    std::env::var("MUNARIUM_MATRIX_TEST_GRPC")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// The MySQL server under test, or `None` when the operator did not ask for
/// that tier. `MUNARIUM_MATRIX_TEST_MYSQL` is a URL —
/// `mysql://matrix:matrix-dev@127.0.0.1:3307/crm` under
/// `docker compose --profile mysql up -d`. Unset is a skip that says so.
pub fn mysql_url() -> Option<String> {
    std::env::var("MUNARIUM_MATRIX_TEST_MYSQL")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// The SQL Server under test, or `None` when the operator did not ask for that
/// tier. `MUNARIUM_MATRIX_TEST_SQLSERVER` is an ADO.NET connection
/// string — SQL Server's own vocabulary, and what tiberius parses:
///
/// ```text
/// Server=tcp:127.0.0.1,14330;User Id=matrix_reader;Password=Matrix-Reader-Dev1!;
/// Database=crm;TrustServerCertificate=true
/// ```
///
/// under `docker compose --profile sqlserver up -d`. Unset is a skip that says
/// so out loud; a string that does not connect is a failure.
pub fn sqlserver_connection_string() -> Option<String> {
    std::env::var("MUNARIUM_MATRIX_TEST_SQLSERVER")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// The tenant the test token belongs to. The intent's authorization snapshot
/// must name it, or the execute is refused as cross-tenant.
pub fn test_tenant() -> String {
    std::env::var("MUNARIUM_MATRIX_TEST_TENANT").unwrap_or_else(|_| "tenant-default".into())
}

/// The database URL, or a skip. Returns `None` when the tier cannot run —
/// callers print why rather than silently passing.
pub fn database_url() -> Option<String> {
    match tier() {
        Tier::Postgres { url } => Some(url),
        Tier::Http { db, .. } => Some(db),
        Tier::Offline => None,
    }
}

/// The scenario registry: every named scenario, the guarantee it proves, and
/// the phase that owns it. `SCENARIOS.md` is generated from this, so the
/// documented list cannot drift from the implemented one.
pub struct ScenarioInfo {
    pub name: &'static str,
    pub guarantee: &'static str,
    pub phase: &'static str,
    pub tier: &'static str,
}

pub const SCENARIOS: &[ScenarioInfo] = &[
    ScenarioInfo {
        name: "canon.identity_is_stable_under_permutation",
        guarantee: "G1",
        phase: "0",
        tier: "offline",
    },
    ScenarioInfo {
        name: "canon.truncated_never_equals_complete",
        guarantee: "G4",
        phase: "0",
        tier: "offline",
    },
    ScenarioInfo {
        name: "canon.unidentifiable_result_refuses_sealing",
        guarantee: "G1",
        phase: "0",
        tier: "offline",
    },
    ScenarioInfo {
        name: "evidence.seal_is_idempotent_by_logical_hash",
        guarantee: "G1",
        phase: "2",
        tier: "offline",
    },
    ScenarioInfo {
        name: "evidence.under_cleared_session_cannot_resolve",
        guarantee: "G6",
        phase: "2",
        tier: "offline",
    },
    ScenarioInfo {
        name: "evidence.replays_after_the_source_changes",
        guarantee: "G1",
        phase: "2",
        tier: "offline",
    },
    ScenarioInfo {
        name: "sync.replayed_checkpoint_creates_no_duplicates",
        guarantee: "G4",
        phase: "2",
        tier: "offline",
    },
    ScenarioInfo {
        name: "sync.coverage_reports_excluded_rows",
        guarantee: "G4",
        phase: "2",
        tier: "offline",
    },
    ScenarioInfo {
        name: "sync.drift_refuses_then_compat_accepts",
        guarantee: "G7",
        phase: "2",
        tier: "offline",
    },
    ScenarioInfo {
        name: "postgres.watermark_advances_and_an_unchanged_source_reads_nothing",
        guarantee: "G4",
        phase: "2",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "postgres.watermark_reads_the_columns_the_source_declared",
        guarantee: "G4",
        phase: "2",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "policy.denied_column_never_appears_anywhere",
        guarantee: "G6",
        phase: "2",
        tier: "offline",
    },
    ScenarioInfo {
        name: "queue.a_stale_running_job_is_reclaimed_after_its_lease",
        guarantee: "-",
        phase: "6",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "registry.apply_is_idempotent_and_refuses_mutation",
        guarantee: "-",
        phase: "1",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "registry.concurrent_appliers_of_one_new_version_insert_once_and_never_fail",
        guarantee: "-",
        phase: "6",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "registry.matrix_owner_cannot_write_public",
        guarantee: "-",
        phase: "1",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "budget.concurrent_reservations_cannot_exceed_the_ceiling",
        guarantee: "G7",
        phase: "1",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "queue.two_workers_claim_disjoint_jobs",
        guarantee: "-",
        phase: "1",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "contract.committed_contract_compiles",
        guarantee: "G7",
        phase: "3",
        tier: "offline",
    },
    ScenarioInfo {
        name: "contract.undeclared_source_column_refuses",
        guarantee: "G7",
        phase: "3",
        tier: "offline",
    },
    ScenarioInfo {
        name: "contract.denied_column_beats_a_reads_declaration",
        guarantee: "G6",
        phase: "3",
        tier: "offline",
    },
    ScenarioInfo {
        name: "freshness.stale_result_refuses_under_a_bound",
        guarantee: "G3",
        phase: "3",
        tier: "offline",
    },
    ScenarioInfo {
        name: "freshness.manifest_states_a_snapshot_marker_per_source",
        guarantee: "G3",
        phase: "2",
        tier: "offline",
    },
    ScenarioInfo {
        name: "verification.derivation_recomputes_from_the_sealed_cells",
        guarantee: "G5",
        phase: "3",
        tier: "offline",
    },
    ScenarioInfo {
        name: "verification.derivation_over_a_truncated_result_is_not_a_total",
        guarantee: "G5",
        phase: "3",
        tier: "offline",
    },
    ScenarioInfo {
        name: "budget.an_execution_spends_a_unit_and_a_refusal_refunds_it",
        guarantee: "G7",
        phase: "3",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "roles.a_claimed_job_always_reaches_a_terminal_state",
        guarantee: "-",
        phase: "2",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "planner.assist_admits_only_a_permitted_trusted_asset",
        guarantee: "G6",
        phase: "6",
        tier: "offline",
    },
    ScenarioInfo {
        name: "planner.evaluation_records_and_admits_nothing",
        guarantee: "G6",
        phase: "6",
        tier: "offline",
    },
    ScenarioInfo {
        name: "planner.an_unpinned_plan_is_a_label_not_a_failure",
        guarantee: "G2",
        phase: "6",
        tier: "offline",
    },
    ScenarioInfo {
        name: "grpc.reflection_lists_the_query_service",
        guarantee: "-",
        phase: "6",
        tier: "grpc",
    },
    ScenarioInfo {
        name: "grpc.an_unauthenticated_call_is_a_status",
        guarantee: "G6",
        phase: "6",
        tier: "grpc",
    },
    ScenarioInfo {
        name: "grpc.a_refusal_is_a_message_not_a_status",
        guarantee: "G7",
        phase: "6",
        tier: "grpc",
    },
    ScenarioInfo {
        name: "grpc.execute_streams_the_block_rest_returns",
        guarantee: "G1",
        phase: "6",
        tier: "grpc",
    },
    ScenarioInfo {
        name: "grpc.a_past_deadline_is_refused_on_the_stream",
        guarantee: "G7",
        phase: "6",
        tier: "grpc",
    },
    ScenarioInfo {
        name: "grpc.tier_mcp_lists_declared_tools_and_a_call_seals_evidence",
        guarantee: "G1",
        phase: "6",
        tier: "grpc",
    },
    ScenarioInfo {
        name: "grpc.tier_native_data_view_verifies_and_executes_over_rest",
        guarantee: "G1",
        phase: "6",
        tier: "grpc",
    },
    ScenarioInfo {
        name: "grpc.tier_an_empty_result_is_a_complete_answer",
        guarantee: "G4",
        phase: "3",
        tier: "grpc",
    },
    ScenarioInfo {
        name: "mysql.probe_reaches_a_real_server",
        guarantee: "-",
        phase: "6",
        tier: "mysql",
    },
    ScenarioInfo {
        name: "mysql.an_exact_decimal_survives_the_driver",
        guarantee: "G1",
        phase: "6",
        tier: "mysql",
    },
    ScenarioInfo {
        name: "mysql.a_positional_parameter_binds_rather_than_interpolates",
        guarantee: "G6",
        phase: "6",
        tier: "mysql",
    },
    ScenarioInfo {
        name: "mysql.an_unmodelled_type_is_refused_and_names_the_column",
        guarantee: "G7",
        phase: "6",
        tier: "mysql",
    },
    ScenarioInfo {
        name: "mysql.a_snapshot_read_reports_no_marker_when_the_server_has_none",
        guarantee: "G2",
        phase: "6",
        tier: "mysql",
    },
    ScenarioInfo {
        name: "mysql.introspect_reports_row_security_as_absent_rather_than_omitting_it",
        guarantee: "G6",
        phase: "6",
        tier: "mysql",
    },
    ScenarioInfo {
        name: "mysql.watermark_advances_by_the_declared_columns",
        guarantee: "G4",
        phase: "6",
        tier: "mysql",
    },
    ScenarioInfo {
        name: "cdc.a_missing_slot_is_refused_with_the_statement_that_creates_it",
        guarantee: "G7",
        phase: "6",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "cdc.a_slot_that_decodes_with_test_decoding_is_refused",
        guarantee: "G6",
        phase: "6",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "cdc.a_publication_without_a_row_filter_on_a_secured_table_is_refused",
        guarantee: "G6",
        phase: "6",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "cdc.a_publication_that_does_not_match_the_projection_is_refused",
        guarantee: "G6",
        phase: "6",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "cdc.inserts_updates_and_deletes_arrive_distinguishable_with_their_lsn",
        guarantee: "G4",
        phase: "6",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "cdc.a_checkpoint_behind_the_slot_is_reported_as_a_gap",
        guarantee: "G4",
        phase: "6",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "cdc.the_slots_retained_wal_is_observable",
        guarantee: "G3",
        phase: "6",
        tier: "postgres",
    },
    ScenarioInfo {
        name: "sqlserver.probe_reaches_a_real_server",
        guarantee: "-",
        phase: "6",
        tier: "sqlserver",
    },
    ScenarioInfo {
        name: "sqlserver.an_exact_decimal_survives_the_driver",
        guarantee: "G1",
        phase: "6",
        tier: "sqlserver",
    },
    ScenarioInfo {
        name: "sqlserver.a_positional_parameter_binds_rather_than_interpolates",
        guarantee: "G6",
        phase: "6",
        tier: "sqlserver",
    },
    ScenarioInfo {
        name: "sqlserver.an_unmodelled_type_is_refused_and_names_the_column",
        guarantee: "G7",
        phase: "6",
        tier: "sqlserver",
    },
    ScenarioInfo {
        name: "sqlserver.a_snapshot_read_reports_a_marker_only_from_a_consistent_view",
        guarantee: "G2",
        phase: "6",
        tier: "sqlserver",
    },
    ScenarioInfo {
        name: "sqlserver.introspect_reports_row_security_as_present",
        guarantee: "G6",
        phase: "6",
        tier: "sqlserver",
    },
    ScenarioInfo {
        name: "sqlserver.watermark_advances_by_the_declared_columns",
        guarantee: "G4",
        phase: "6",
        tier: "sqlserver",
    },
    ScenarioInfo {
        name: "semantic.an_adapter_without_the_capability_is_metric_not_covered",
        guarantee: "G7",
        phase: "6",
        tier: "offline",
    },
    ScenarioInfo {
        name: "semantic.a_view_with_no_verification_on_record_is_not_covered",
        guarantee: "G7",
        phase: "6",
        tier: "offline",
    },
    ScenarioInfo {
        name: "semantic.a_changed_definition_is_refused_before_the_statement",
        guarantee: "G7",
        phase: "6",
        tier: "offline",
    },
    ScenarioInfo {
        name: "reconcile.a_pass_over_its_declared_ceiling_is_refused_before_it_writes",
        guarantee: "G7",
        phase: "6",
        tier: "offline",
    },
    ScenarioInfo {
        name: "reconcile.shadow_leaves_canon_byte_identical",
        guarantee: "G7",
        phase: "4",
        tier: "offline",
    },
    ScenarioInfo {
        name: "reconcile.ambiguous_identity_never_merges",
        guarantee: "G7",
        phase: "4",
        tier: "offline",
    },
    ScenarioInfo {
        name: "reconcile.replayed_batch_creates_no_duplicates",
        guarantee: "G4",
        phase: "4",
        tier: "offline",
    },
    ScenarioInfo {
        name: "reconcile.backdated_change_requires_review",
        guarantee: "G7",
        phase: "4",
        tier: "offline",
    },
    ScenarioInfo {
        name: "reconcile.discrepancy_carries_both_evidence_sides",
        guarantee: "G1",
        phase: "4",
        tier: "offline",
    },
    ScenarioInfo {
        name: "reconcile.absent_declared_holder_is_missing_in_source",
        guarantee: "G4",
        phase: "4",
        tier: "offline",
    },
    ScenarioInfo {
        name: "reconcile.precision_and_recall_on_the_t0_answer_key",
        guarantee: "G7",
        phase: "4",
        tier: "offline",
    },
    ScenarioInfo {
        name: "authority.unpromoted_mapping_proposes_nothing",
        guarantee: "G7",
        phase: "5",
        tier: "offline",
    },
    ScenarioInfo {
        name: "authority.promoted_mapping_proposes_only_in_scope",
        guarantee: "G6",
        phase: "5",
        tier: "offline",
    },
    ScenarioInfo {
        name: "authority.document_outranks_source_by_default",
        guarantee: "G7",
        phase: "5",
        tier: "offline",
    },
    ScenarioInfo {
        name: "authority.replayed_run_proposes_nothing_twice",
        guarantee: "G4",
        phase: "5",
        tier: "offline",
    },
    ScenarioInfo {
        name: "authority.backdated_never_proposes",
        guarantee: "G7",
        phase: "5",
        tier: "offline",
    },
    ScenarioInfo {
        name: "authority.disputed_proposal_is_counted_not_dropped",
        guarantee: "G7",
        phase: "5",
        tier: "offline",
    },
    ScenarioInfo {
        name: "authority.rollback_supersedes_with_origin",
        guarantee: "G1",
        phase: "5",
        tier: "offline",
    },
    ScenarioInfo {
        name: "authority.rollback_of_a_chain_restores_the_original",
        guarantee: "G1",
        phase: "5",
        tier: "offline",
    },
    ScenarioInfo {
        name: "authority.a_rollback_claim_holds_under_document_precedence",
        guarantee: "G7",
        phase: "5",
        tier: "offline",
    },
    // The operator console, over real HTTP. The unit properties
    // (role gating, CSRF, Origin, the header set) live in the server crate at
    // the assembled router; these are the ones that need a real service.
    ScenarioInfo {
        name: "admin.every_read_page_renders_without_script",
        guarantee: "-",
        phase: "7",
        tier: "http",
    },
    ScenarioInfo {
        name: "admin.is_mgmt_only_over_the_wire",
        guarantee: "G6",
        phase: "7",
        tier: "http",
    },
    ScenarioInfo {
        name: "admin.a_write_without_a_csrf_token_is_refused",
        guarantee: "G6",
        phase: "7",
        tier: "http",
    },
    ScenarioInfo {
        name: "admin.an_action_needs_the_rw_credential_not_the_admins_own",
        guarantee: "G6",
        phase: "7",
        tier: "http",
    },
    ScenarioInfo {
        name: "admin.an_exported_draft_applies_identically_to_applying_in_place",
        guarantee: "-",
        phase: "7",
        tier: "http",
    },
    ScenarioInfo {
        name: "admin.a_console_write_is_journaled_as_admin_ui",
        guarantee: "G3",
        phase: "7",
        tier: "http",
    },
    ScenarioInfo {
        name: "admin.the_drift_flag_sets_on_apply_in_place_and_clears_when_the_bundle_lands",
        guarantee: "-",
        phase: "7",
        tier: "http",
    },
];

/// Render `SCENARIOS.md`. Called by a test so the committed file cannot drift.
pub fn scenarios_markdown() -> String {
    let mut out = String::from(
        "# Conformance scenarios\n\n\
         Generated from `SCENARIOS` in `conformance/src/lib.rs` — edit the code, not this file.\n\
         Every guarantee in the plan maps to at least one row here; a guarantee with no scenario\n\
         is a claim with no test.\n\n\
         | Scenario | Guarantee | Phase | Tier |\n|---|---|---|---|\n",
    );
    for s in SCENARIOS {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            s.name, s.guarantee, s.phase, s.tier
        ));
    }
    out.push_str("\n## Guarantee coverage\n\n");
    for g in ["G1", "G2", "G3", "G4", "G5", "G6", "G7"] {
        let n = SCENARIOS.iter().filter(|s| s.guarantee == g).count();
        let note = match (g, n) {
            // Measured 2026-08-29: Delta time travel WORKS (the same query at
            // `VERSION AS OF 2` returned the pre-mutation rows byte-for-byte),
            // but a statement response carries no version anywhere, so the
            // claim is true of the SOURCE and not substantiable from a READ.
            // Pinning one needs a second `DESCRIBE HISTORY` that is not atomic
            // with the statement. The adapter landing did not close this.
            // G2 was zero until 2026-08-29. Delta time travel had
            // been demonstrated BY HAND and written down, which is a paragraph
            // that cannot fail; `databricks.source_time_travel_returns_the_prior_state`
            // is the same demonstration as a test. The narrower statement still
            // holds and has its own scenario: a statement response carries no
            // version, so the claim is substantiable from the SOURCE and not
            // from a READ (`databricks.execute_reports_no_snapshot_marker`).
            ("G2", 0) => " — no scenario; source time travel is unproven",
            ("G3", 0) => " — freshness; lands with the Postgres adapter's snapshot marker",
            ("G5", 0) => " — answer verification; server-side",
            _ => "",
        };
        out.push_str(&format!("- **{g}**: {n} scenario(s){note}\n"));
    }

    // A count of scenarios is not a count of measurements. Two tiers in this
    // registry have never executed against the thing they name, because no
    // account exists for either — and a guarantee tally that did not say so
    // would read as coverage the project does not have. This is the same
    // failure the SKIPPED prints exist to prevent, one level up.
    out.push_str("\n## Tiers, and whether they have ever run\n\n");
    out.push_str("| Tier | Scenarios | Run against a real thing? |\n|---|---|---|\n");
    let mut tiers: Vec<&str> = SCENARIOS.iter().map(|s| s.tier).collect();
    tiers.sort_unstable();
    tiers.dedup();
    for tier in tiers {
        let n = SCENARIOS.iter().filter(|s| s.tier == tier).count();
        let state = match tier {
            "offline" => "yes — every push, $0",
            "postgres" | "grpc" | "http" => "yes — compose, $0",
            "mysql" | "sqlserver" => "yes — compose, $0, behind a profile and a variable",
            _ => "unrecorded — add it to this table",
        };
        out.push_str(&format!("| `{tier}` | {n} | {state} |\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered scenario has a test behind it.
    ///
    /// `http.registry_round_trips_byte_identically` and
    /// `http.validation_findings_match_the_local_validators` were listed here,
    /// counted in SCENARIOS.md, and never written — the registry claimed
    /// coverage that did not exist, one level above the guarantee map it
    /// exists to keep honest. Both properties ARE tested, by the live
    /// checks against real ingress; they were removed from this registry
    /// rather than implemented twice.
    ///
    /// The scrape is deliberately crude: it reads this crate's own sources and
    /// looks for a function whose name matches the scenario's, so adding a row
    /// without a test fails here rather than in a cycle that costs money.
    #[test]
    fn every_registered_scenario_has_a_test() {
        let src = concat!(
            include_str!("scenarios.rs"),
            include_str!("admin_tier.rs"),
            include_str!("lib.rs"),
        );
        let missing: Vec<&str> = SCENARIOS
            .iter()
            .map(|s| s.name)
            .filter(|name| {
                // Two conventions are in use, both fine. Most scenarios encode
                // the prefix in the function name
                // (`canon.identity_is_stable` -> `fn canon_identity_is_stable`);
                // the `authority.*` ones drop it because the module already
                // says it (`scenarios::authority::rollback_supersedes...`).
                // Accept either, or this check becomes a rename campaign.
                let full = name.replace('.', "_");
                let suffix = name.split_once('.').map(|(_, s)| s).unwrap_or(name);
                !src.contains(&format!("fn {full}(")) && !src.contains(&format!("fn {suffix}("))
            })
            .collect();
        assert!(
            missing.is_empty(),
            "registered scenarios with no test behind them: {missing:?}"
        );
    }

    #[test]
    fn every_scenario_name_is_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for s in SCENARIOS {
            assert!(seen.insert(s.name), "duplicate scenario name {}", s.name);
        }
    }

    #[test]
    fn the_committed_scenario_list_matches_the_code() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("SCENARIOS.md");
        let expected = scenarios_markdown();
        // The file is GENERATED, so regenerating it is a command rather than a
        // transcription exercise:
        //
        //     MUNARIUM_MATRIX_WRITE_SCENARIOS=1 cargo test -p munarium-matrix-conformance
        //
        // Hand-copying a generator's output back into its own artifact is how a
        // "generated" file quietly stops matching the code it claims to come from.
        if std::env::var("MUNARIUM_MATRIX_WRITE_SCENARIOS").is_ok() {
            std::fs::write(&path, &expected).expect("write SCENARIOS.md");
        }
        let actual = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert_eq!(
            actual.trim(),
            expected.trim(),
            "SCENARIOS.md is stale — regenerate it (the list is generated from code)"
        );
    }

    // The §18.3 measurement-discipline lint lived HERE as well as in
    // `scripts/doclint.py`, and the two implementations of one rule
    // disagreed -- which is the defect this tree keeps finding in other
    // people's work. This one scanned every backticked eight-character token
    // under `docs/`, so the moment a document cited the commit it was
    // measured from ("from the committed tree (`212c37ec`)") it demanded a
    // results file for a git sha, and the offline tier went red for a
    // correctly written sentence. It also carried its own copy of the
    // exemption list that `conformance/results/UNRECORDED` already holds, so
    // an exemption had to be written twice to be believed once.
    //
    // The rule now has ONE implementation: `scripts/doclint.py`, which reads
    // the whole of `matrix/**/*.md` plus the root `CLAUDE.md`, takes a cycle
    // id to be a backticked eight-character token in the same sentence as the
    // word "cycle" or "run id", and exempts only what UNRECORDED declares
    // with a reason. `test.ps1` and `matrix-ci.yml` both call it.
}
