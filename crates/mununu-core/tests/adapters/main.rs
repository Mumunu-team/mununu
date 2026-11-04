//! adapters — one test binary for the 18 integration-test files below (N0.4, 2026-11-03).
//!
//! Each file used to be its own crate and linked mununu-core once; 55 of them were
//! ~4 min of the 7.7-min PR gate. A module keeps every test's name as
//! `adapters::<file>::<test>`, so `cargo test <substring>` still finds it. Files a
//! script or a rule names by binary stay separate: `differential_oracle_e2e`,
//! `wall_class_matrix`, `properties`, `api_graph_valuations`.

mod adapter_aiger;
mod adapter_cross_format;
#[cfg(feature = "syntcomp")]
mod adapter_debug;
mod adapter_output_format;
mod adapter_perf;
mod adapter_promela;
mod adapter_roundtrip;
mod adapter_solidity_viability;
#[cfg(feature = "syntcomp")]
mod adapter_syntcomp;
mod adapter_xstate_system;
mod adapter_xstate_viability;
mod emit_valuations_roundtrip;
#[cfg(feature = "ast-extract")]
mod extraction_composition_e2e;
mod past_shadow_input_e2e;
mod support;
mod sv_per_module_btor2;
mod sv_state_splitting;
mod sv_yosys_multi_module_verify;
