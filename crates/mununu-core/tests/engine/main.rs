//! engine — one test binary for the 19 integration-test files below (N0.4, 2026-11-03).
//!
//! Each file used to be its own crate and linked mununu-core once; 55 of them were
//! ~4 min of the 7.7-min PR gate. A module keeps every test's name as
//! `engine::<file>::<test>`, so `cargo test <substring>` still finds it. Files a
//! script or a rule names by binary stay separate: `differential_oracle_e2e`,
//! `wall_class_matrix`, `properties`, `api_graph_valuations`.

mod abstraction_tests;
mod btor2_coi_constraint_soundness;
mod btor2_differential;
mod btor2_kmts_lift_sweep;
mod exact_run_budget;
mod exact_two_player_fairness;
mod exact_two_player_game;
mod exact_two_player_rtl;
mod exact_two_player_strategy;
mod fair_cycle_l2s;
mod fault_adversary;
mod kmts_two_player_game;
mod m3_5_failure_subgame;
mod predicate_3valued_atom_binding;
mod predicate_image_recall;
mod r3_kleene_baseline;
mod scalable_gr1;
mod soundness;
mod unrolling_tests;
mod v6_controllability_kmts;
