//! General N-source-model verification framework.
//!
//! The verify module accepts a `verify.toml` listing N typed sources
//! (any combination of supported source languages: C firmware, SV RTL,
//! hand-authored CTXDSL protocol specs, XState JSON, microprograms,
//! agentic-orchestration JSON, …), an alphabet-binding strategy
//! describing how labels across sources synchronise, a composition
//! strategy (synchronous / asynchronous / superset), and a list of
//! properties to verify. The orchestrator runs each source through
//! its adapter, applies the binding, composes the results into a
//! unified CTXDSL document, parses and realises it, and evaluates
//! each property via `mu_calculus`.
//!
//! ## Module layout (per A2 plan slices)
//!
//! - **`config`** (this slice — A2.1) — `VerifyConfig` TOML schema +
//!   validation. Pure data model, no orchestration.
//! - **`binding`** (A2.2) — `AlphabetBinding` strategies + the label-
//!   rewriting pass applied to each adapter's emitted CTXDSL before
//!   composition.
//! - **`assemble`** (A2.3) — assembles N `AdapterOutput`s into a
//!   single parseable CTXDSL document with alphabet, automata,
//!   compositions, and a sidecar `mu_formulas` block for properties.
//! - **`orchestrator`** (A2.4) — `verify_project()`: the pipeline
//!   (parse → validate → dispatch each source → apply binding →
//!   assemble → parse + realise → evaluate properties → produce
//!   `VerifyReport`).
//! - **`report`** (A2.4) — `VerifyReport`, `PropertyVerdict`,
//!   `PropertyFormulaSource`, error types.
//!
//! Codesign C+SV verification is one specialization of this
//! framework; the `mununu.codesign.toml` schema in
//! [`crate::codesign::project_config`] (when shipped) translates
//! mechanically into a `VerifyConfig`.

pub mod assemble;
pub mod binding;
pub mod codesign_shorthand;
pub mod config;
pub mod fairness;
pub mod memory_check;
pub mod orchestrator;
pub mod register_map_rewriter;
pub mod report;

pub use orchestrator::{
    AutomatonInspection, CompositionInspection, InspectionReport, inspect_project, verify_project,
};
pub use report::{
    CompositionInfo, PropertyFormulaSource, PropertyVerdict, SourceSummary, VerifyError,
    VerifyReport,
};
