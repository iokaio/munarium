# Conformance scenarios

Generated from `SCENARIOS` in `conformance/src/lib.rs` — edit the code, not this file.
Every guarantee in the plan maps to at least one row here; a guarantee with no scenario
is a claim with no test.

| Scenario | Guarantee | Phase | Tier |
|---|---|---|---|
| `canon.identity_is_stable_under_permutation` | G1 | 0 | offline |
| `canon.truncated_never_equals_complete` | G4 | 0 | offline |
| `canon.unidentifiable_result_refuses_sealing` | G1 | 0 | offline |
| `evidence.seal_is_idempotent_by_logical_hash` | G1 | 2 | offline |
| `evidence.under_cleared_session_cannot_resolve` | G6 | 2 | offline |
| `evidence.replays_after_the_source_changes` | G1 | 2 | offline |
| `sync.replayed_checkpoint_creates_no_duplicates` | G4 | 2 | offline |
| `sync.coverage_reports_excluded_rows` | G4 | 2 | offline |
| `sync.drift_refuses_then_compat_accepts` | G7 | 2 | offline |
| `postgres.watermark_advances_and_an_unchanged_source_reads_nothing` | G4 | 2 | postgres |
| `postgres.watermark_reads_the_columns_the_source_declared` | G4 | 2 | postgres |
| `policy.denied_column_never_appears_anywhere` | G6 | 2 | offline |
| `queue.a_stale_running_job_is_reclaimed_after_its_lease` | - | 6 | postgres |
| `registry.apply_is_idempotent_and_refuses_mutation` | - | 1 | postgres |
| `registry.concurrent_appliers_of_one_new_version_insert_once_and_never_fail` | - | 6 | postgres |
| `registry.matrix_owner_cannot_write_public` | - | 1 | postgres |
| `budget.concurrent_reservations_cannot_exceed_the_ceiling` | G7 | 1 | postgres |
| `queue.two_workers_claim_disjoint_jobs` | - | 1 | postgres |
| `contract.committed_contract_compiles` | G7 | 3 | offline |
| `contract.undeclared_source_column_refuses` | G7 | 3 | offline |
| `contract.denied_column_beats_a_reads_declaration` | G6 | 3 | offline |
| `freshness.stale_result_refuses_under_a_bound` | G3 | 3 | offline |
| `freshness.manifest_states_a_snapshot_marker_per_source` | G3 | 2 | offline |
| `verification.derivation_recomputes_from_the_sealed_cells` | G5 | 3 | offline |
| `verification.derivation_over_a_truncated_result_is_not_a_total` | G5 | 3 | offline |
| `budget.an_execution_spends_a_unit_and_a_refusal_refunds_it` | G7 | 3 | postgres |
| `roles.a_claimed_job_always_reaches_a_terminal_state` | - | 2 | postgres |
| `planner.assist_admits_only_a_permitted_trusted_asset` | G6 | 6 | offline |
| `planner.evaluation_records_and_admits_nothing` | G6 | 6 | offline |
| `planner.an_unpinned_plan_is_a_label_not_a_failure` | G2 | 6 | offline |
| `grpc.reflection_lists_the_query_service` | - | 6 | grpc |
| `grpc.an_unauthenticated_call_is_a_status` | G6 | 6 | grpc |
| `grpc.a_refusal_is_a_message_not_a_status` | G7 | 6 | grpc |
| `grpc.execute_streams_the_block_rest_returns` | G1 | 6 | grpc |
| `grpc.a_past_deadline_is_refused_on_the_stream` | G7 | 6 | grpc |
| `grpc.tier_mcp_lists_declared_tools_and_a_call_seals_evidence` | G1 | 6 | grpc |
| `grpc.tier_native_data_view_verifies_and_executes_over_rest` | G1 | 6 | grpc |
| `grpc.tier_an_empty_result_is_a_complete_answer` | G4 | 3 | grpc |
| `mysql.probe_reaches_a_real_server` | - | 6 | mysql |
| `mysql.an_exact_decimal_survives_the_driver` | G1 | 6 | mysql |
| `mysql.a_positional_parameter_binds_rather_than_interpolates` | G6 | 6 | mysql |
| `mysql.an_unmodelled_type_is_refused_and_names_the_column` | G7 | 6 | mysql |
| `mysql.a_snapshot_read_reports_no_marker_when_the_server_has_none` | G2 | 6 | mysql |
| `mysql.introspect_reports_row_security_as_absent_rather_than_omitting_it` | G6 | 6 | mysql |
| `mysql.watermark_advances_by_the_declared_columns` | G4 | 6 | mysql |
| `cdc.a_missing_slot_is_refused_with_the_statement_that_creates_it` | G7 | 6 | postgres |
| `cdc.a_slot_that_decodes_with_test_decoding_is_refused` | G6 | 6 | postgres |
| `cdc.a_publication_without_a_row_filter_on_a_secured_table_is_refused` | G6 | 6 | postgres |
| `cdc.a_publication_that_does_not_match_the_projection_is_refused` | G6 | 6 | postgres |
| `cdc.inserts_updates_and_deletes_arrive_distinguishable_with_their_lsn` | G4 | 6 | postgres |
| `cdc.a_checkpoint_behind_the_slot_is_reported_as_a_gap` | G4 | 6 | postgres |
| `cdc.the_slots_retained_wal_is_observable` | G3 | 6 | postgres |
| `sqlserver.probe_reaches_a_real_server` | - | 6 | sqlserver |
| `sqlserver.an_exact_decimal_survives_the_driver` | G1 | 6 | sqlserver |
| `sqlserver.a_positional_parameter_binds_rather_than_interpolates` | G6 | 6 | sqlserver |
| `sqlserver.an_unmodelled_type_is_refused_and_names_the_column` | G7 | 6 | sqlserver |
| `sqlserver.a_snapshot_read_reports_a_marker_only_from_a_consistent_view` | G2 | 6 | sqlserver |
| `sqlserver.introspect_reports_row_security_as_present` | G6 | 6 | sqlserver |
| `sqlserver.watermark_advances_by_the_declared_columns` | G4 | 6 | sqlserver |
| `semantic.an_adapter_without_the_capability_is_metric_not_covered` | G7 | 6 | offline |
| `semantic.a_view_with_no_verification_on_record_is_not_covered` | G7 | 6 | offline |
| `semantic.a_changed_definition_is_refused_before_the_statement` | G7 | 6 | offline |
| `reconcile.a_pass_over_its_declared_ceiling_is_refused_before_it_writes` | G7 | 6 | offline |
| `reconcile.shadow_leaves_canon_byte_identical` | G7 | 4 | offline |
| `reconcile.ambiguous_identity_never_merges` | G7 | 4 | offline |
| `reconcile.replayed_batch_creates_no_duplicates` | G4 | 4 | offline |
| `reconcile.backdated_change_requires_review` | G7 | 4 | offline |
| `reconcile.discrepancy_carries_both_evidence_sides` | G1 | 4 | offline |
| `reconcile.absent_declared_holder_is_missing_in_source` | G4 | 4 | offline |
| `reconcile.precision_and_recall_on_the_t0_answer_key` | G7 | 4 | offline |
| `authority.unpromoted_mapping_proposes_nothing` | G7 | 5 | offline |
| `authority.promoted_mapping_proposes_only_in_scope` | G6 | 5 | offline |
| `authority.document_outranks_source_by_default` | G7 | 5 | offline |
| `authority.replayed_run_proposes_nothing_twice` | G4 | 5 | offline |
| `authority.backdated_never_proposes` | G7 | 5 | offline |
| `authority.disputed_proposal_is_counted_not_dropped` | G7 | 5 | offline |
| `authority.rollback_supersedes_with_origin` | G1 | 5 | offline |
| `authority.rollback_of_a_chain_restores_the_original` | G1 | 5 | offline |
| `authority.a_rollback_claim_holds_under_document_precedence` | G7 | 5 | offline |
| `admin.every_read_page_renders_without_script` | - | 7 | http |
| `admin.is_mgmt_only_over_the_wire` | G6 | 7 | http |
| `admin.a_write_without_a_csrf_token_is_refused` | G6 | 7 | http |
| `admin.an_action_needs_the_rw_credential_not_the_admins_own` | G6 | 7 | http |
| `admin.an_exported_draft_applies_identically_to_applying_in_place` | - | 7 | http |
| `admin.a_console_write_is_journaled_as_admin_ui` | G3 | 7 | http |
| `admin.the_drift_flag_sets_on_apply_in_place_and_clears_when_the_bundle_lands` | - | 7 | http |

## Guarantee coverage

- **G1**: 12 scenario(s)
- **G2**: 3 scenario(s)
- **G3**: 4 scenario(s)
- **G4**: 13 scenario(s)
- **G5**: 2 scenario(s)
- **G6**: 17 scenario(s)
- **G7**: 23 scenario(s)

## Tiers, and whether they have ever run

| Tier | Scenarios | Run against a real thing? |
|---|---|---|
| `grpc` | 8 | yes — compose, $0 |
| `http` | 7 | yes — compose, $0 |
| `mysql` | 7 | yes — compose, $0, behind a profile and a variable |
| `offline` | 40 | yes — every push, $0 |
| `postgres` | 17 | yes — compose, $0 |
| `sqlserver` | 7 | yes — compose, $0, behind a profile and a variable |
