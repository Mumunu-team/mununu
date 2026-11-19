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
    RealizedContext, RealizedController, RealizedFormula, TemplateExpansion,
    realize as realize_context, realize_with_templates, realized_automaton_names,
};
pub use runtime::ResolvedControllerOptions;

/// `value` as a CTXDSL string literal — the one way a caller writes user-supplied text into
/// generated CTXDSL (mununu#966: the API's `template_ref` arguments). Escapes exactly what the
/// lexer unescapes (`"`, `\`, a line break, a tab; see `lexer::lexes_string_escapes`), so the
/// literal round-trips to `value` and cannot close the clause it sits in or start another
/// declaration whatever `value` holds.
pub fn string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod string_literal_tests {
    use super::*;

    /// Every value the escaper handles comes back through the lexer unchanged, including the
    /// characters that would otherwise end a binding or a block.
    #[test]
    fn a_string_literal_round_trips_through_the_lexer_whatever_it_holds() {
        for value in [
            "Lof",
            "a}b",
            "x; y",
            "say \"hi\"",
            "back\\slash",
            "two\nlines",
            "tab\tbed",
            "} formula evil { over a; body = true; }",
            "",
        ] {
            let lit = string_literal(value);
            let tokens = lex(&lit).unwrap_or_else(|e| panic!("{lit}: {e:?}"));
            match &tokens[0].kind {
                token::TokenKind::String(s) => assert_eq!(s, value, "{lit}"),
                other => panic!("{lit}: {other:?}"),
            }
            assert!(
                tokens.len() <= 2,
                "{lit}: one string token (plus EOF), got {}",
                tokens.len()
            );
        }
    }
}

#[cfg(test)]
mod tests;
