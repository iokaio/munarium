// SPDX-License-Identifier: Apache-2.0
//! munarium-core — the Munarium kernel, ported faithfully from the research
//! implementation that first proved it. The model proposes; the mesh disposes.
//!
//! Semantic invariants carried over verbatim (the conformance suite enforces
//! them against every storage backend):
//!
//! - The ledger is append-only. A correction is a NEW claim naming
//!   `supersedes_id`; nothing is ever updated in place.
//! - `seq` is monotonic across a version lineage (one counting domain), and
//!   every store stamps from it, so ONE `as_of_seq` pin bounds facts, anchors,
//!   promises, counters, and entities together.
//! - `slice_facts` resolves supersession AS OF the pin: the superseded-set is
//!   itself filtered by `seq <= as_of_seq`, so a claim superseded later still
//!   reads as current at the pin.
//! - Gate-blocked claims are recorded `disputed`, never dropped.
//! - Digests are deterministically REBUILT under a pin, never served stored.

pub mod budget;
pub mod chrono_gate;
pub mod composer;
pub mod counters;
pub mod digests;
pub mod docintel;
pub mod error;
pub mod evidence;
pub mod gates;
pub mod hierarchy;
pub mod ledger;
pub mod promises;
pub mod provider;
pub mod retrieval;
pub mod similarity;
pub mod sources;
pub mod storage;
pub mod types;

pub use error::KernelError;
pub type Result<T> = std::result::Result<T, KernelError>;
