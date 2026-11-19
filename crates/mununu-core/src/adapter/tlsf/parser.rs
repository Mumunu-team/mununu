//! TLSF (Temporal Logic Synthesis Format) parser.
//!
//! Parses non-parametric TLSF v1.1 specifications with INFO and MAIN sections.
//! Produces a [`TlsfSpec`] containing inputs, outputs, assumptions, invariants,
//! and guarantees as LTL formulas.

use crate::adapter::ir::{GameSemantics, RealizabilityStatus};
use crate::adapter::{AdapterError, AdapterErrorKind, SourceLocation};
use crate::ltl::LtlFormula;

/// Parsed TLSF specification.
#[derive(Debug, Clone)]
pub struct TlsfSpec {
    pub title: Option<String>,
    pub description: Option<String>,
    pub semantics: Option<GameSemantics>,
    pub known_status: Option<RealizabilityStatus>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub assumptions: Vec<LtlFormula>,
    pub invariants: Vec<LtlFormula>,
    pub guarantees: Vec<LtlFormula>,
}

/// Parse a TLSF specification from text.
/// The MAIN sections of the full TLSF format that the basic format unfolds into
/// ASSUMPTIONS / INVARIANTS / GUARANTEES; this reader takes the basic format only, and
/// names the gap when it meets one of these (mununu#1106 item 1).
const FULL_FORMAT_SECTIONS: &[&str] = &[
    "INITIALLY",
    "PRESET",
    "REQUIRE",
    "ASSERT",
    "ASSUME",
    "GUARANTEE",
];

pub fn parse(content: &str) -> Result<TlsfSpec, AdapterError> {
    let mut parser = TlsfParser::new(content);
    parser.parse_spec()
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    LBrace,
    RBrace,
    LParen,
    RParen,
    Semi,
    Comma,
    Colon,
    // LTL operators
    Not,
    And,
    Or,
    Implies,
    Iff,
    Next,      // X
    Always,    // G
    Finally,   // F
    Until,     // U
    WeakUntil, // W
    Release,   // R
    True,
    False,
    // Keywords
    Info,
    Main,
    Global,
    Title,
    Description,
    Semantics,
    Target,
    Inputs,
    Outputs,
    Assumptions,
    Invariants,
    Guarantees,
    Mealy,
    Moore,
    // Identifiers and strings
    Ident(String),
    StringLit(String),
    Eof,
}

struct Lexer<'a> {
    input: &'a str,
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek_char()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace
            while self.peek_char().is_some_and(|c| c.is_whitespace()) {
                self.advance();
            }
            // Skip line comments (//)
            if self.input[self.pos..].starts_with("//") {
                while self.peek_char().is_some_and(|c| c != '\n') {
                    self.advance();
                }
                continue;
            }
            // Skip block comments (/* */)
            if self.input[self.pos..].starts_with("/*") {
                self.advance();
                self.advance();
                while self.pos + 1 < self.input.len() {
                    if self.input[self.pos..].starts_with("*/") {
                        self.advance();
                        self.advance();
                        break;
                    }
                    self.advance();
                }
                continue;
            }
            break;
        }
    }

    fn location(&self) -> SourceLocation {
        SourceLocation {
            line: self.line,
            column: self.col,
        }
    }

    fn next_token(&mut self) -> Result<Token, AdapterError> {
        self.skip_whitespace_and_comments();

        let Some(c) = self.peek_char() else {
            return Ok(Token::Eof);
        };

        match c {
            '{' => {
                self.advance();
                Ok(Token::LBrace)
            }
            '}' => {
                self.advance();
                Ok(Token::RBrace)
            }
            '(' => {
                self.advance();
                Ok(Token::LParen)
            }
            ')' => {
                self.advance();
                Ok(Token::RParen)
            }
            ';' => {
                self.advance();
                Ok(Token::Semi)
            }
            ',' => {
                self.advance();
                Ok(Token::Comma)
            }
            ':' => {
                self.advance();
                Ok(Token::Colon)
            }
            '!' => {
                self.advance();
                Ok(Token::Not)
            }
            '&' => {
                self.advance();
                if self.peek_char() == Some('&') {
                    self.advance();
                }
                Ok(Token::And)
            }
            '|' => {
                self.advance();
                if self.peek_char() == Some('|') {
                    self.advance();
                }
                Ok(Token::Or)
            }
            '-' => {
                self.advance();
                if self.peek_char() == Some('>') {
                    self.advance();
                    Ok(Token::Implies)
                } else {
                    Err(self.err("Expected '>' after '-'"))
                }
            }
            '<' => {
                self.advance();
                if self.peek_char() == Some('-') {
                    self.advance();
                    if self.peek_char() == Some('>') {
                        self.advance();
                        Ok(Token::Iff)
                    } else {
                        Err(self.err("Expected '>' after '<-'"))
                    }
                } else {
                    Err(self.err("Expected '-' after '<'"))
                }
            }
            '"' => {
                self.advance();
                let mut s = String::new();
                while self.peek_char().is_some_and(|c| c != '"') {
                    s.push(self.advance().unwrap());
                }
                if self.peek_char() == Some('"') {
                    self.advance();
                }
                Ok(Token::StringLit(s))
            }
            _ if c.is_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while self
                    .peek_char()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    ident.push(self.advance().unwrap());
                }
                Ok(match ident.as_str() {
                    "INFO" => Token::Info,
                    "MAIN" => Token::Main,
                    "GLOBAL" => Token::Global,
                    "TITLE" => Token::Title,
                    "DESCRIPTION" => Token::Description,
                    "SEMANTICS" => Token::Semantics,
                    "TARGET" => Token::Target,
                    "INPUTS" => Token::Inputs,
                    "OUTPUTS" => Token::Outputs,
                    "ASSUMPTIONS" => Token::Assumptions,
                    "INVARIANTS" => Token::Invariants,
                    "GUARANTEES" => Token::Guarantees,
                    "Mealy" => Token::Mealy,
                    "Moore" => Token::Moore,
                    "G" => Token::Always,
                    "F" => Token::Finally,
                    "X" => Token::Next,
                    "U" => Token::Until,
                    "W" => Token::WeakUntil,
                    "R" => Token::Release,
                    "true" => Token::True,
                    "false" => Token::False,
                    _ => Token::Ident(ident),
                })
            }
            _ => {
                self.advance();
                Err(self.err(&format!("Unexpected character '{c}'")))
            }
        }
    }

    fn err(&self, msg: &str) -> AdapterError {
        AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: msg.to_string(),
            location: Some(self.location()),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct TlsfParser<'a> {
    lexer: Lexer<'a>,
    current: Token,
}

impl<'a> TlsfParser<'a> {
    fn new(input: &'a str) -> Self {
        let mut lexer = Lexer::new(input);
        let current = lexer.next_token().unwrap_or(Token::Eof);
        Self { lexer, current }
    }

    fn advance(&mut self) -> Result<Token, AdapterError> {
        let prev = std::mem::replace(&mut self.current, self.lexer.next_token()?);
        Ok(prev)
    }

    fn expect(&mut self, expected: &Token) -> Result<(), AdapterError> {
        if std::mem::discriminant(&self.current) == std::mem::discriminant(expected) {
            self.advance()?;
            Ok(())
        } else {
            Err(self
                .lexer
                .err(&format!("Expected {:?}, got {:?}", expected, self.current)))
        }
    }

    fn parse_spec(&mut self) -> Result<TlsfSpec, AdapterError> {
        let mut spec = TlsfSpec {
            title: None,
            description: None,
            semantics: None,
            known_status: None,
            inputs: vec![],
            outputs: vec![],
            assumptions: vec![],
            invariants: vec![],
            guarantees: vec![],
        };

        // Parse INFO section (optional in practice)
        if self.current == Token::Info {
            self.parse_info(&mut spec)?;
        }

        // Skip GLOBAL section if present
        if self.current == Token::Global {
            self.skip_block()?;
        }

        // Parse MAIN section
        if self.current == Token::Main {
            self.parse_main(&mut spec)?;
        }

        // Try to parse SYNTCOMP status from remaining content
        // (appears as comments after MAIN)

        Ok(spec)
    }

    fn parse_info(&mut self, spec: &mut TlsfSpec) -> Result<(), AdapterError> {
        self.expect(&Token::Info)?;
        self.expect(&Token::LBrace)?;

        while self.current != Token::RBrace && self.current != Token::Eof {
            match &self.current {
                Token::Title => {
                    self.advance()?;
                    self.expect(&Token::Colon)?;
                    if let Token::StringLit(s) = &self.current {
                        spec.title = Some(s.clone());
                        self.advance()?;
                    }
                }
                Token::Description => {
                    self.advance()?;
                    self.expect(&Token::Colon)?;
                    if let Token::StringLit(s) = &self.current {
                        spec.description = Some(s.clone());
                        self.advance()?;
                    }
                }
                Token::Semantics => {
                    self.advance()?;
                    self.expect(&Token::Colon)?;
                    spec.semantics = match &self.current {
                        Token::Mealy => Some(GameSemantics::Mealy),
                        Token::Moore => Some(GameSemantics::Moore),
                        _ => None,
                    };
                    if spec.semantics.is_some() {
                        self.advance()?;
                    }
                }
                Token::Target => {
                    self.advance()?;
                    self.expect(&Token::Colon)?;
                    // Skip target value (Mealy/Moore)
                    if matches!(self.current, Token::Mealy | Token::Moore | Token::Ident(_)) {
                        self.advance()?;
                    }
                }
                _ => {
                    // Skip unknown INFO fields
                    self.advance()?;
                }
            }
        }

        self.expect(&Token::RBrace)?;
        Ok(())
    }

    fn parse_main(&mut self, spec: &mut TlsfSpec) -> Result<(), AdapterError> {
        self.expect(&Token::Main)?;
        self.expect(&Token::LBrace)?;

        while self.current != Token::RBrace && self.current != Token::Eof {
            match &self.current {
                Token::Inputs => {
                    self.advance()?;
                    self.expect(&Token::LBrace)?;
                    while self.current != Token::RBrace && self.current != Token::Eof {
                        if let Token::Ident(name) = &self.current {
                            spec.inputs.push(name.clone());
                            self.advance()?;
                        } else {
                            self.advance()?;
                        }
                        // Optional semicolon/comma separator
                        if matches!(self.current, Token::Semi | Token::Comma) {
                            self.advance()?;
                        }
                    }
                    self.expect(&Token::RBrace)?;
                }
                Token::Outputs => {
                    self.advance()?;
                    self.expect(&Token::LBrace)?;
                    while self.current != Token::RBrace && self.current != Token::Eof {
                        if let Token::Ident(name) = &self.current {
                            spec.outputs.push(name.clone());
                            self.advance()?;
                        } else {
                            self.advance()?;
                        }
                        if matches!(self.current, Token::Semi | Token::Comma) {
                            self.advance()?;
                        }
                    }
                    self.expect(&Token::RBrace)?;
                }
                Token::Assumptions => {
                    self.advance()?;
                    self.expect(&Token::LBrace)?;
                    while self.current != Token::RBrace && self.current != Token::Eof {
                        let formula = self.parse_ltl()?;
                        spec.assumptions.push(formula);
                        if self.current == Token::Semi {
                            self.advance()?;
                        }
                    }
                    self.expect(&Token::RBrace)?;
                }
                Token::Invariants => {
                    self.advance()?;
                    self.expect(&Token::LBrace)?;
                    while self.current != Token::RBrace && self.current != Token::Eof {
                        let formula = self.parse_ltl()?;
                        spec.invariants.push(formula);
                        if self.current == Token::Semi {
                            self.advance()?;
                        }
                    }
                    self.expect(&Token::RBrace)?;
                }
                Token::Guarantees => {
                    self.advance()?;
                    self.expect(&Token::LBrace)?;
                    while self.current != Token::RBrace && self.current != Token::Eof {
                        let formula = self.parse_ltl()?;
                        spec.guarantees.push(formula);
                        if self.current == Token::Semi {
                            self.advance()?;
                        }
                    }
                    self.expect(&Token::RBrace)?;
                }
                // mununu#1106 item 1 — an unknown section used to be skipped one token at a
                // time; its closing `}` then ended MAIN, and every section after it (the
                // GUARANTEES included) never reached `TlsfSpec`, so the spec went to synthesis
                // with fewer obligations than it declares. Refuse, naming the section and the
                // sections this reader takes.
                Token::Ident(name) => {
                    let remedy = if FULL_FORMAT_SECTIONS.contains(&name.as_str()) {
                        "it is a full-format TLSF section; this reader takes the basic format: \
                         unfold the specification to basic form (syfco's basic output) or rewrite \
                         the section as ASSUMPTIONS / INVARIANTS / GUARANTEES"
                    } else {
                        "the sections this reader takes are INPUTS, OUTPUTS, ASSUMPTIONS, \
                         INVARIANTS and GUARANTEES"
                    };
                    return Err(self.lexer.err(&format!(
                        "unknown MAIN section '{name}' (the sections after it would not be read): {remedy}"
                    )));
                }
                other => {
                    return Err(self.lexer.err(&format!(
                        "unexpected {other:?} in MAIN: expected a section (INPUTS, OUTPUTS, \
                         ASSUMPTIONS, INVARIANTS, GUARANTEES) or the closing brace"
                    )));
                }
            }
        }

        self.expect(&Token::RBrace)?;
        Ok(())
    }

    fn skip_block(&mut self) -> Result<(), AdapterError> {
        self.advance()?; // skip keyword
        self.expect(&Token::LBrace)?;
        let mut depth = 1;
        while depth > 0 && self.current != Token::Eof {
            if self.current == Token::LBrace {
                depth += 1;
            } else if self.current == Token::RBrace {
                depth -= 1;
            }
            if depth > 0 {
                self.advance()?;
            }
        }
        if depth == 0 {
            self.advance()?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // LTL formula parser (recursive descent, precedence climbing)
    // -----------------------------------------------------------------------
    // Precedence (low to high):
    //   <-> (iff)
    //   ->  (implies)
    //   ||  (or)
    //   &&  (and)
    //   U, W, R (binary temporal)
    //   G, F, X, ! (unary)
    //   atom (true, false, ident, parenthesized)

    fn parse_ltl(&mut self) -> Result<LtlFormula, AdapterError> {
        self.parse_iff()
    }

    fn parse_iff(&mut self) -> Result<LtlFormula, AdapterError> {
        let mut left = self.parse_implies()?;
        while self.current == Token::Iff {
            self.advance()?;
            let right = self.parse_implies()?;
            // A <-> B = (A -> B) && (B -> A)
            left = LtlFormula::And(
                Box::new(LtlFormula::Implies(
                    Box::new(left.clone()),
                    Box::new(right.clone()),
                )),
                Box::new(LtlFormula::Implies(Box::new(right), Box::new(left))),
            );
        }
        Ok(left)
    }

    fn parse_implies(&mut self) -> Result<LtlFormula, AdapterError> {
        let left = self.parse_or()?;
        if self.current == Token::Implies {
            self.advance()?;
            let right = self.parse_implies()?; // right-associative
            Ok(LtlFormula::Implies(Box::new(left), Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn parse_or(&mut self) -> Result<LtlFormula, AdapterError> {
        let mut left = self.parse_and()?;
        while self.current == Token::Or {
            self.advance()?;
            let right = self.parse_and()?;
            left = LtlFormula::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<LtlFormula, AdapterError> {
        let mut left = self.parse_temporal_binary()?;
        while self.current == Token::And {
            self.advance()?;
            let right = self.parse_temporal_binary()?;
            left = LtlFormula::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_temporal_binary(&mut self) -> Result<LtlFormula, AdapterError> {
        let left = self.parse_unary()?;
        match &self.current {
            Token::Until => {
                self.advance()?;
                let right = self.parse_unary()?;
                Ok(LtlFormula::Until {
                    left: Box::new(left),
                    right: Box::new(right),
                })
            }
            Token::WeakUntil => {
                self.advance()?;
                let right = self.parse_unary()?;
                Ok(LtlFormula::WeakUntil {
                    left: Box::new(left),
                    right: Box::new(right),
                })
            }
            Token::Release => {
                self.advance()?;
                let right = self.parse_unary()?;
                Ok(LtlFormula::Release {
                    left: Box::new(left),
                    right: Box::new(right),
                })
            }
            _ => Ok(left),
        }
    }

    fn parse_unary(&mut self) -> Result<LtlFormula, AdapterError> {
        match &self.current {
            Token::Not => {
                self.advance()?;
                let inner = self.parse_unary()?;
                Ok(LtlFormula::Not(Box::new(inner)))
            }
            Token::Always => {
                self.advance()?;
                let inner = self.parse_unary()?;
                Ok(LtlFormula::Always(Box::new(inner)))
            }
            Token::Finally => {
                self.advance()?;
                let inner = self.parse_unary()?;
                Ok(LtlFormula::Eventually(Box::new(inner)))
            }
            Token::Next => {
                self.advance()?;
                let inner = self.parse_unary()?;
                Ok(LtlFormula::Next(Box::new(inner)))
            }
            _ => self.parse_atom(),
        }
    }

    fn parse_atom(&mut self) -> Result<LtlFormula, AdapterError> {
        match &self.current {
            Token::True => {
                self.advance()?;
                Ok(LtlFormula::True)
            }
            Token::False => {
                self.advance()?;
                Ok(LtlFormula::False)
            }
            Token::Ident(name) => {
                let name = name.clone();
                self.advance()?;
                Ok(LtlFormula::Predicate(name))
            }
            Token::LParen => {
                self.advance()?;
                let inner = self.parse_ltl()?;
                self.expect(&Token::RParen)?;
                Ok(inner)
            }
            _ => Err(self
                .lexer
                .err(&format!("Expected LTL atom, got {:?}", self.current))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lilydemo03() {
        let input = r#"
INFO {
  TITLE:       "Lily Demo V3"
  DESCRIPTION: "One of the Lily demo files"
  SEMANTICS:   Mealy
  TARGET:      Mealy
}

MAIN {
  INPUTS {
    req;
    cancel;
    go;
  }

  OUTPUTS {
    grant;
  }

  ASSUMPTIONS {
    G (cancel -> X go);
  }

  INVARIANTS {
    req -> X (grant || X (grant || X grant));
    grant -> X !grant;
    cancel -> X (!grant U go);
  }
}
"#;
        let spec = parse(input).unwrap();
        assert_eq!(spec.title, Some("Lily Demo V3".into()));
        assert_eq!(spec.inputs, vec!["req", "cancel", "go"]);
        assert_eq!(spec.outputs, vec!["grant"]);
        assert_eq!(spec.assumptions.len(), 1);
        assert_eq!(spec.invariants.len(), 3);
        assert_eq!(spec.guarantees.len(), 0);
        assert_eq!(spec.semantics, Some(GameSemantics::Mealy));
    }

    #[test]
    fn parse_simple_safety() {
        let input = r#"
INFO {
  TITLE: "Simple"
}
MAIN {
  INPUTS { a; }
  OUTPUTS { b; }
  GUARANTEES {
    G (!a || b);
  }
}
"#;
        let spec = parse(input).unwrap();
        assert_eq!(spec.inputs, vec!["a"]);
        assert_eq!(spec.outputs, vec!["b"]);
        assert_eq!(spec.guarantees.len(), 1);
    }

    /// mununu#1106 item 1 — REPRODUCER (the falsifier of the review's reading): a section this
    /// reader does not know, placed before GUARANTEES, is refused by name. On the parent the
    /// same text parsed `Ok` with ZERO of its one guarantee reaching `TlsfSpec` (the unknown
    /// section's closing brace ended MAIN), so the review's consequence is confirmed.
    #[test]
    fn an_unknown_main_section_is_refused_by_name_instead_of_dropping_what_follows() {
        let input = r#"
MAIN {
  INPUTS { a; }
  OUTPUTS { b; }
  ASSERT { G (b); }
  GUARANTEES {
    G (!a || b);
  }
}
"#;
        let e = parse(input).expect_err("an unknown MAIN section is refused");
        assert!(
            e.message.contains("unknown MAIN section 'ASSERT'")
                && e.message.contains("would not be read")
                && e.message.contains("full-format")
                && e.message.contains("ASSUMPTIONS / INVARIANTS / GUARANTEES"),
            "{}",
            e.message
        );
        // A section that is not a full-format one gets the list of sections read.
        let e =
            parse("MAIN { INPUTS { a; } FOO { x; } GUARANTEES { G (a); } }").expect_err("refused");
        assert!(
            e.message.contains("unknown MAIN section 'FOO'")
                && e.message
                    .contains("INPUTS, OUTPUTS, ASSUMPTIONS, INVARIANTS and GUARANTEES"),
            "{}",
            e.message
        );
        // A stray token that is no section name is refused too, not skipped.
        let e = parse("MAIN { INPUTS { a; } ; GUARANTEES { G (a); } }").expect_err("refused");
        assert!(e.message.contains("in MAIN"), "{}", e.message);
        // Negative control: the known sections in any order still parse in full.
        let spec = parse(
            "MAIN { GUARANTEES { G (a); } INVARIANTS { a; } ASSUMPTIONS { F (a); } OUTPUTS { b; } INPUTS { a; } }",
        )
        .expect("every known section parses");
        assert_eq!(
            (
                spec.inputs.len(),
                spec.outputs.len(),
                spec.assumptions.len(),
                spec.invariants.len(),
                spec.guarantees.len()
            ),
            (1, 1, 1, 1, 1)
        );
    }
}
