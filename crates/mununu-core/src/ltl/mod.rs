//! LTL (Linear Temporal Logic) infrastructure: AST representation and translation.
//!
//! This module provides the typed representation for LTL formulas that can be
//! translated to μ-calculus for evaluation.

pub mod ast;
pub mod parser;
pub mod translator;
pub mod universal;

pub use ast::LtlFormula;
pub use parser::{ParseError, parse};
pub use translator::{TranslationError, translate, translate_existential};
