//! policy — the repository-policy tests in one binary (mununu#943, 2026-11-10).
//!
//! Each file was its own top-level test binary and linked mununu-core once more; none of them
//! needs the crate at all — they read the tree. A module keeps every test's name as
//! `policy::<file>::<test>`, so `cargo test -p mununu-core --test policy <substring>` finds it.
//! The issue-numbered reproducers stay separate binaries: `make verify-repro` builds a parent
//! commit by target name, and a parent has no `policy` target.

mod child_waits_go_through_one_helper_1196;
mod consumer_neutral_output_g5;
mod docs_references_resolve_d2;
mod env_reads_go_through_env_var_974;
mod env_vars_documented_d1;
mod hook_features_match_the_cli_1165;
mod ignored_tests_have_a_runner;
mod licence_never_reaches_a_verdict_h1;
mod merged_clock_domains_reported_1181;
mod no_links_into_untracked_paths_g4;
mod no_source_indentation_in_messages_1209;
mod one_yosys_minimum_version_1121;
mod remedies_name_real_flags_1106;
mod remedies_on_the_verb_1121;
mod scratch_names_are_per_call_1221;
mod server_paths_resolve_through_one_function;
mod source_built_tools_are_pinned_1242;
mod source_scan;
mod third_party_inventory_g1;
mod timeout_setter_named_on_its_surface_1197;
mod unconfirmed_paths_reach_every_report_1175;
mod verilator_build_directories_1176;
mod written_scripts_wait_out_a_busy_file_1247;
