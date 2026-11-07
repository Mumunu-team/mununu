//! Shared LLVM IR types + line-based regex parser — phase L1 of the
//! C-extraction principled lift.
//!
//! The shape and design is documented in
//! [`docs/archive/design/c-extraction-correctness-scope.md`](../../../docs/archive/design/c-extraction-correctness-scope.md)
//! and the implementation plan at
//! `~/.claude/plans/i-want-you-to-distributed-orbit.md`.
//!
//! ## What this module is
//!
//! A language-neutral parser for LLVM IR (textual `.ll` output of
//! `clang -emit-llvm -S` or `rustc --emit=llvm-ir`). It covers the
//! instruction shapes mununu's two extractor pipelines need:
//!
//! - **C codesign extraction** (Doc C task C5 — see [`crate::codesign::c_extract`]
//!   once the LLVM backend lands) reads firmware C code and lifts
//!   `load volatile` / `store volatile` accesses against a register
//!   map. It needs `inttoptr` literals, GEP chains rooted at external
//!   globals, and load-modify-store bit-field sequences.
//! - **Rust source extraction** (existing `mununu-extract --backend
//!   llvm`) reads `rustc` IR and lifts method-level field accesses on
//!   `%self`-rooted GEPs. It needs `define` headers with arbitrary
//!   parameter lists, basic-block labels with the standard
//!   `; preds = …` comment, and the same load/store/GEP/icmp/branch
//!   subset.
//!
//! The shared types in [`types`] sit underneath both consumers; the
//! shared parser in [`parser`] handles the language-neutral parts.
//! Language-specific post-processing (Rust demangling, C register-map
//! matching) lives in the consumer crate.
//!
//! ## Why a regex parser
//!
//! Same trade-off as the rest of mununu's pragmatic choices: zero new
//! Rust build-time dependencies, fast to ship, easy to audit. The
//! textual IR format is stable across LLVM versions back to ~10. We
//! parse only the instruction shapes we need; unknown lines fall
//! through to [`Instruction::Other`] which preserves them as raw
//! text for diagnostics.
//!
//! A true IR consumer (`llvm-sys`, `inkwell`, parsing bitcode
//! directly) is the principled-er alternative; it's the same kind of
//! "build dependency vs hand-rolled parser" decision as
//! [`crate::codesign::c_extract`] makes for the AST path. When a real
//! case demands proper type-info-aware analysis, this module is the
//! seam to replace.

pub mod parser;
pub mod types;

pub use parser::{ParseError, parse_module};
pub use types::{
    BasicBlock, BinaryOp, CastKind, Function, FunctionParameter, GepIndex, Global, GlobalKind,
    IcmpPred, Instruction, Module, PointerOperand, StructType, Terminator, ValueOperand,
};
