// SPDX-License-Identifier: Apache-2.0
//! munarium-datastore's own public-API integration tests, compiled as a
//! consumer outside the Server workspace (P15/D5).
//!
//! Integration tests can reach only the crate's public API, so including them
//! here runs the same assertions as an embedder would see them, under this
//! fixture's own dependency resolution and feature selection. Each module keeps
//! its source in the datastore crate; nothing is copied.
//!
//! Not included: `lexical_parity` (its fixtures are resolved relative to the
//! datastore crate's manifest), `diskann_contract` (it tests the pinned
//! `diskann` crate's own API, not this one), and `vector_crossover` (an
//! ignored measurement).

#[path = "../../../src/munarium-datastore/tests/round_trip.rs"]
mod round_trip;

#[path = "../../../src/munarium-datastore/tests/contract_vectors.rs"]
mod contract_vectors;

#[path = "../../../src/munarium-datastore/tests/retrieval_characterization.rs"]
mod retrieval_characterization;
