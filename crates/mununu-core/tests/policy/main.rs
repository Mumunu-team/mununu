//! policy — the repository-policy tests in one binary (mununu#943, 2026-11-10).
//!
//! Each file was its own top-level test binary and linked mununu-core once more; none of them
//! needs the crate at all — they read the tree. A module keeps every test's name as
//! `policy::<file>::<test>`, so `cargo test -p mununu-core --test policy <substring>` finds it.
//! The issue-numbered reproducers stay separate binaries: `make verify-repro` builds a parent
//! commit by target name, and a parent has no `policy` target.

mod consumer_neutral_output_g5;
mod docs_references_resolve_d2;
mod env_vars_documented_d1;
mod ignored_tests_have_a_runner;
mod licence_never_reaches_a_verdict_h1;
mod no_links_into_untracked_paths_g4;
mod third_party_inventory_g1;
