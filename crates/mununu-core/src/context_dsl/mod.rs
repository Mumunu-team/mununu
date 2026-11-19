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
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether `text` is exactly one CTXDSL identifier — what the lexer reads as a single
/// `Identifier` token and nothing else (so not a keyword, not empty, no punctuation, no
/// whitespace). The check a caller applies to user-supplied text it writes into generated
/// CTXDSL in an IDENTIFIER position (a formula or template name, an argument key, an automaton
/// name), where a string literal is not the grammar and escaping is no help: anything else is
/// refused before any text is built (mununu#966 follow-up — a `template_ref` id such as
/// `x { } } } context evil {` closed the synthesized sidecar's blocks and declared its own).
pub fn is_identifier(text: &str) -> bool {
    match lex(text) {
        Ok(tokens) => {
            tokens.len() == 2
                && matches!(&tokens[0].kind, token::TokenKind::Identifier(s) if s == text)
                && matches!(tokens[1].kind, token::TokenKind::Eof)
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod string_literal_tests {
    use super::*;

    /// The identifier check is the lexer's rule: one identifier token, nothing else.
    #[test]
    fn is_identifier_accepts_exactly_one_identifier_token() {
        for ok in ["reachable", "TARGET", "u_x", "_a1", "Light"] {
            assert!(is_identifier(ok), "{ok}");
        }
        for bad in [
            "",
            " a",
            "a b",
            "a;",
            "x { } } } context evil {",
            "a}",
            "two\nlines",
            "\"quoted\"",
            "42",
            "body",
        ] {
            assert!(!is_identifier(bad), "{bad:?}");
        }
    }

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
