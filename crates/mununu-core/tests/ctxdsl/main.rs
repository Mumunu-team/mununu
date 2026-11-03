//! ctxdsl — one test binary for the 14 integration-test files below (N0.4, 2026-11-03).
//!
//! Each file used to be its own crate and linked mununu-core once; 55 of them were
//! ~4 min of the 7.7-min PR gate. A module keeps every test's name as
//! `ctxdsl::<file>::<test>`, so `cargo test <substring>` still finds it. Files a
//! script or a rule names by binary stay separate: `differential_oracle_e2e`,
//! `wall_class_matrix`, `properties`, `api_graph_valuations`.

mod asynchronous_examples;
mod context_dsl_controllability;
mod context_dsl_integration;
mod ctxdsl_variable_atom_binding;
mod diagnostics;
mod dual_condition_start;
mod integration_tests;
mod ltl_patterns;
mod mu_calculus_response_pattern;
mod r5_0_subitem_4_6_audit;
mod shared_label_controllability;
mod sterile_batch_release;
mod synchronous_examples;
mod tier1_features;
