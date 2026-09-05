// SPDX-License-Identifier: Apache-2.0
//! The role workers.
//!
//! Each role is a loop that claims work from its own queue and runs one job:
//! [`sync`] materializes records (mode A), the query path executes contracts
//! (mode B), and the reconcile path turns rows into observations (mode C).
//!
//! The workers own orchestration only. Every rule they enforce — canon@1
//! identity, the refusal taxonomy, class resolution, rendering — lives in a
//! pure module below them, so a worker bug can lose a run but cannot corrupt
//! evidence.

#![forbid(unsafe_code)]
#![allow(clippy::result_large_err)]

pub mod authority;
pub mod classes;
pub mod evidence;
pub mod genie;
pub mod observe;
pub mod query;
pub mod reconcile;
pub mod rollback;
pub mod semantic;
pub mod sync;

pub use authority::{decide as decide_authority, AuthorityContext, AuthorityDecision};
pub use classes::{resolve_classes, ResolvedClass};
pub use evidence::{build_manifest, canonical_csv, count_result, seal, SealContext};
pub use observe::{observe, ObserveContext, ObserveStats};
pub use query::{
    execute, execute_traced, execute_with_result, verify, ExecuteContext, ExecuteTimings, Traced,
    VerifiedQuestionOutcome,
};
pub use reconcile::{
    compare, reconcile, reconcile_with, ProposalLedger, ProposalRecord, ReconcileOptions,
    ReconcileOutcome, Verdict,
};
pub use rollback::{rollback, rollback_key, RollbackOutcome, RollbackRequest};
pub use semantic::{
    execute_metric, execute_metric_traced, execute_metric_with_result, verify_metric,
    MetricVerifyOutcome, SemanticView,
};
pub use sync::{run_sync, SyncOutcome, SyncRequest};
