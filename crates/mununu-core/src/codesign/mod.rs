//! HW/SW codesign — formal verification across the boundary.
//!
//! Implements Document C of mununu's four-document arc — see
//! [`docs/archive/design/hw-sw-codesign-extraction.md`](../../../../docs/archive/design/hw-sw-codesign-extraction.md).
//!
//! The conceptual model: peripheral RTL + firmware C, modelled as two
//! reactive modules (Alur & Henzinger, *Reactive Modules*, FMSD 1999)
//! that share a set of *coupled variables*. Each memory-mapped register
//! is a coupled variable; the SV side and the C side both have
//! read/write access governed by the register's direction, visibility
//! class, and access path. The coupling spec lives in a JSON
//! "register-map sidecar" that names each register, its bit-layout, and
//! how each side accesses it.
//!
//! What ships today:
//!
//! - **C1 — register-map sidecar schema** in [`register_map`]. JSON
//!   schema + serde types + structural validation.
//! - **C2 — coupling synthesis (slice 1)** in [`coupling`]. Emits the
//!   CTXDSL fragment (alphabet + chaotic peripheral stub + async
//!   composition) the user splices into a hand-authored context.
//! - **C3 — interleaved trace origin classifier (slice 1)** in
//!   [`trace`]. Tags trace steps as `[SW]` / `[HW]` / `[BUS]` based on
//!   a caller-supplied label partition.
//! - **C4 — codesign composition (slice 1)** in [`compose`]. Splices
//!   the coupling fragment into a user-authored firmware CTXDSL and
//!   round-trip-validates the result through the parser. The CLI /
//!   HTTP surfaces in `mununu-cli` and `api::handlers` consume it.
//! - **C6 — CMSIS-SVD importer** in [`svd_import`]. Translates a
//!   CMSIS-SVD XML file into one `RegisterMap` per peripheral. The
//!   `sv_signal` and `c_accessor` fields start empty per Doc C
//!   §C.9.6 — the user authors those post-import.
//!
//! Follow-up slices in §C.9 of the design doc: C5 (libclang C
//! extraction + `@mununu_*` C wrappers). IP-XACT and SystemRDL
//! importers are deferred sibling tasks under C6's umbrella.

pub mod c_extract;
pub mod c_extract_llvm;
pub mod cmsis_emit;
pub mod compose;
pub mod coupling;
pub mod project_config;
pub mod reconcile;
pub mod register_map;
pub mod svd_import;
pub mod trace;
