//! Entry points and shared types for the CLTS Context DSL subsystem.
//! The module exposes the AST, lexer, parser, canonicaliser, and incremental
//! loader used by the rest of the crate.
pub mod ast;
pub mod canonicalize;
pub mod error;
pub mod lexer;
pub mod loader;
pub mod parser;
pub mod realize;
pub mod runtime;
mod state_matching;
pub mod token;
mod traversal;

/// Parsed document produced by the context DSL.
pub use ast::ContextDoc;
/// Lexing errors raised while tokenising DSL source.
pub use error::{LexError, ParseError};
/// Tokenises a context DSL source string.
pub use lexer::lex;
/// Incremental loading primitives for dependency-aware rebuilds.
pub use loader::{IncrementalState, LoadPlan};
/// Parses a context DSL document and returns the canonicalised AST.
pub use parser::parse;
pub use realize::{
    FormulaTargetsKind, GuardExpressionMetadata, PredicateMetadata, RealizationError,
    RealizedContext, RealizedController, RealizedFormula, realize as realize_context,
    realized_automaton_names,
};
pub use runtime::ResolvedControllerOptions;

#[cfg(test)]
mod tests;
