#![allow(clippy::pedantic, clippy::nursery)]

//! Mununu core library — formal verification for reactive systems
//! modeled as Compositional Labeled Transition Systems (CLTS).

// The exact-symbolic BDD engine catches OxiDD OutOfMemory via std::panic::catch_unwind
// (see adapter/btor2/symbolic_bitblast.rs) and abstains to the rest of the portfolio when a
// cone is too wide to bit-blast. A `panic = "abort"` build silently defeats that backstop:
// abort() fires before catch_unwind can intercept, turning a sound abstain into a process
// SIGABRT on wide cones (release-only, since dev/test default to unwind). Enforce unwind at
// compile time so the release profile can never regress this. (`cfg(panic = ...)` is stable
// since Rust 1.60; inert under the unwind default.)
#[cfg(panic = "abort")]
compile_error!(
    "mununu-core requires panic = \"unwind\" (see [profile.release] in the workspace Cargo.toml): \
     the exact-symbolic engine relies on catch_unwind to abstain on BDD OutOfMemory; \
     panic = \"abort\" turns that abstain into a SIGABRT."
);

pub mod abstraction;
pub mod adapter;
pub mod build_info;
pub mod clts;
pub mod codesign;
pub mod composition;
pub mod context;
pub mod context_dsl;
pub mod contract;
pub mod controllability;
pub mod corpus;
pub mod deployment;
pub mod examples;
pub mod guard;
pub mod iter;
pub mod library;
pub mod llvm_ir;
pub mod ltl;
pub mod mu_calculus;
pub mod mununu_annotations;
pub mod persistence;
pub mod planner;
pub mod verdict;
pub mod verify;

#[cfg(any(test, feature = "test_support"))]
pub mod test_support;

#[cfg(any(test, feature = "test_support"))]
pub mod bench_support;

#[cfg(feature = "api")]
pub mod api;
pub mod trace;

/// Temporary placeholder to ensure the crate builds.
pub fn init() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes() {
        init();
    }
}
