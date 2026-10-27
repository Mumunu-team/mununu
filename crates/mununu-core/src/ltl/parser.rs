//! LTL (Linear Temporal Logic) parser.
//!
//! This module provides a recursive descent parser for LTL formulas with the following
//! operator precedence (from highest to lowest):
//! - `!` (negation)
//! - `G`, `F`, `X` (temporal unary operators)
//! - `U`, `W`, `R` (temporal binary operators)
//! - `&&` (conjunction)
//! - `||` (disjunction)
//! - `->` (implication)

use thiserror::Error;

use super::ast::LtlFormula;

/// Parses a textual LTL formula into the structured [`LtlFormula`] AST.
pub fn parse(input: &str) -> Result<LtlFormula, ParseError> {
    let mut parser = Parser::new(input);
    let formula = parser.parse_formula()?;
    parser.skip_whitespace();
    if !parser.is_eof() {
        return Err(parser.error_here(ParseErrorKind::UnexpectedTrailing));
    }
    Ok(formula)
}

/// Errors that can occur while parsing LTL source.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("unexpected token at byte {pos}: {message}")]
    Unexpected { pos: usize, message: String },
    #[error("unexpected trailing input at byte {pos}")]
    UnexpectedTrailing { pos: usize },
}

#[derive(Debug)]
enum ParseErrorKind {
    UnexpectedTrailing,
    Expected(&'static str),
    UnexpectedChar(char),
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn parse_formula(&mut self) -> Result<LtlFormula, ParseError> {
        self.parse_implies()
    }

    // Precedence level 1: Implication (lowest)
    fn parse_implies(&mut self) -> Result<LtlFormula, ParseError> {
        // Left-associative chain of `->`
        let mut left = self.parse_or()?;
        loop {
            self.skip_whitespace();
            if self.try_consume_str("->") {
                let right = self.parse_or()?;
                left = LtlFormula::Implies(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    // Precedence level 2: Disjunction
    fn parse_or(&mut self) -> Result<LtlFormula, ParseError> {
        // Left-associative chain of `||`
        let mut left = self.parse_and()?;
        loop {
            self.skip_whitespace();
            if self.try_consume_str("||") {
                let right = self.parse_and()?;
                left = LtlFormula::Or(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    // Precedence level 3: Conjunction
    fn parse_and(&mut self) -> Result<LtlFormula, ParseError> {
        // Left-associative chain of `&&`
        let mut left = self.parse_until()?;
        loop {
            self.skip_whitespace();
            if self.try_consume_str("&&") {
                let right = self.parse_until()?;
                left = LtlFormula::And(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    // Precedence level 4: Until, WeakUntil, Release
    fn parse_until(&mut self) -> Result<LtlFormula, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            self.skip_whitespace();
            if self.try_consume_keyword("U") || self.try_consume_keyword("until") {
                let right = self.parse_unary()?;
                left = LtlFormula::Until {
                    left: Box::new(left),
                    right: Box::new(right),
                };
            } else if self.try_consume_keyword("W") || self.try_consume_keyword("weak_until") {
                let right = self.parse_unary()?;
                left = LtlFormula::WeakUntil {
                    left: Box::new(left),
                    right: Box::new(right),
                };
            } else if self.try_consume_keyword("R") || self.try_consume_keyword("release") {
                let right = self.parse_unary()?;
                left = LtlFormula::Release {
                    left: Box::new(left),
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }
        Ok(left)
    }

    // Precedence level 5: Unary operators (!, G, F, X)
    fn parse_unary(&mut self) -> Result<LtlFormula, ParseError> {
        self.skip_whitespace();

        // Negation
        if self.try_consume_char('!') {
            let operand = self.parse_unary()?;
            return Ok(LtlFormula::Not(Box::new(operand)));
        }

        // Temporal unary operators
        if self.try_consume_keyword("G") || self.try_consume_keyword("always") {
            self.skip_whitespace();
            // Support both G(φ) and G φ
            let operand = if self.try_consume_char('(') {
                let formula = self.parse_formula()?;
                self.skip_whitespace();
                self.expect_char(')')?;
                formula
            } else {
                self.parse_unary()?
            };
            return Ok(LtlFormula::Always(Box::new(operand)));
        }

        if self.try_consume_keyword("F") || self.try_consume_keyword("eventually") {
            self.skip_whitespace();
            let operand = if self.try_consume_char('(') {
                let formula = self.parse_formula()?;
                self.skip_whitespace();
                self.expect_char(')')?;
                formula
            } else {
                self.parse_unary()?
            };
            return Ok(LtlFormula::Eventually(Box::new(operand)));
        }

        if self.try_consume_keyword("X") || self.try_consume_keyword("next") {
            self.skip_whitespace();
            let operand = if self.try_consume_char('(') {
                let formula = self.parse_formula()?;
                self.skip_whitespace();
                self.expect_char(')')?;
                formula
            } else {
                self.parse_unary()?
            };
            return Ok(LtlFormula::Next(Box::new(operand)));
        }

        self.parse_primary()
    }

    // Precedence level 6: Primary expressions (atoms, parentheses)
    fn parse_primary(&mut self) -> Result<LtlFormula, ParseError> {
        self.skip_whitespace();

        // Parentheses
        if self.try_consume_char('(') {
            let formula = self.parse_formula()?;
            self.skip_whitespace();
            self.expect_char(')')?;
            return Ok(formula);
        }

        // Boolean literals
        if self.try_consume_keyword("true") {
            return Ok(LtlFormula::True);
        }
        if self.try_consume_keyword("false") {
            return Ok(LtlFormula::False);
        }

        // Predicate: an identifier, or a comparison atom in the shape the mu-calculus parser
        // reads (`ident op decimal`, `ident op ident [+ k]`), so an LTL body over RTL signals
        // (`G (state_q == 5 -> !motor_up)`) carries the comparison through to the translation
        // as one predicate. `->` is left to the implication level: only the six comparison
        // operators are consumed here, and `-` is not one of them.
        if let Some(identifier) = self.parse_identifier() {
            let saved = self.pos;
            self.skip_whitespace();
            if let Some(op) = self.try_consume_comparison_op() {
                self.skip_whitespace();
                if let Some(rhs) = self.parse_comparison_rhs() {
                    return Ok(LtlFormula::Predicate(format!("{identifier} {op} {rhs}")));
                }
                return Err(self.error_here(ParseErrorKind::Expected(
                    "a number or an identifier after the comparison operator",
                )));
            }
            self.pos = saved;
            return Ok(LtlFormula::Predicate(identifier));
        }

        Err(self.error_here(ParseErrorKind::Expected("formula")))
    }

    /// One of `==`, `!=`, `<=`, `>=`, `<`, `>`; the two-character forms first so `<=` is not
    /// read as `<`. `->` never matches: it starts with `-`.
    fn try_consume_comparison_op(&mut self) -> Option<&'static str> {
        ["==", "!=", "<=", ">=", "<", ">"]
            .into_iter()
            .find(|op| self.try_consume_str(op))
    }

    /// The right-hand side of a comparison atom: a decimal (optionally negative), or an
    /// identifier with an optional `+ k` offset (`cnt_q == cnt_q__past + 1`).
    fn parse_comparison_rhs(&mut self) -> Option<String> {
        let start = self.pos;
        if self.try_consume_char('-') || self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
            while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                self.advance();
            }
            let text = &self.input[start..self.pos];
            return if text.chars().any(|c| c.is_ascii_digit()) {
                Some(text.to_string())
            } else {
                self.pos = start;
                None
            };
        }
        let ident = self.parse_identifier()?;
        let saved = self.pos;
        self.skip_whitespace();
        if self.try_consume_char('+') {
            self.skip_whitespace();
            let k_start = self.pos;
            while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                self.advance();
            }
            if self.pos > k_start {
                return Some(format!("{ident} + {}", &self.input[k_start..self.pos]));
            }
        }
        self.pos = saved;
        Some(ident)
    }

    // Helper methods for parsing

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn try_consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_char(&mut self, expected: char) -> Result<(), ParseError> {
        if self.try_consume_char(expected) {
            Ok(())
        } else {
            Err(self.error_here(ParseErrorKind::UnexpectedChar(expected)))
        }
    }

    fn try_consume_str(&mut self, s: &str) -> bool {
        if self.input[self.pos..].starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn try_consume_keyword(&mut self, keyword: &str) -> bool {
        let saved_pos = self.pos;
        if self.try_consume_str(keyword) {
            // Check that it's followed by whitespace, operator, or end
            if let Some(ch) = self.peek_char()
                && (ch.is_alphanumeric() || ch == '_')
            {
                // It's part of a longer identifier, backtrack
                self.pos = saved_pos;
                return false;
            }
            true
        } else {
            false
        }
    }

    fn parse_identifier(&mut self) -> Option<String> {
        self.skip_whitespace();
        let start = self.pos;
        let mut first = true;

        while let Some(ch) = self.peek_char() {
            if ch.is_alphabetic() || ch == '_' || (!first && ch.is_numeric()) {
                self.advance();
                first = false;
            } else {
                break;
            }
        }

        if self.pos > start {
            Some(self.input[start..self.pos].to_string())
        } else {
            None
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn error_here(&self, kind: ParseErrorKind) -> ParseError {
        match kind {
            ParseErrorKind::UnexpectedTrailing => ParseError::UnexpectedTrailing { pos: self.pos },
            ParseErrorKind::Expected(msg) => ParseError::Unexpected {
                pos: self.pos,
                message: format!("expected {}", msg),
            },
            ParseErrorKind::UnexpectedChar(ch) => ParseError::Unexpected {
                pos: self.pos,
                message: format!("expected `{}`", ch),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_always() {
        // Test G(φ) syntax
        let formula = parse("G(safe)").unwrap();
        assert!(matches!(formula, LtlFormula::Always(_)));

        // Test G φ syntax
        let formula = parse("G safe").unwrap();
        assert!(matches!(formula, LtlFormula::Always(_)));

        // Test "always" keyword
        let formula = parse("always safe").unwrap();
        assert!(matches!(formula, LtlFormula::Always(_)));
    }

    #[test]
    fn test_parse_eventually() {
        let formula = parse("F(completed)").unwrap();
        assert!(matches!(formula, LtlFormula::Eventually(_)));

        let formula = parse("F completed").unwrap();
        assert!(matches!(formula, LtlFormula::Eventually(_)));

        let formula = parse("eventually completed").unwrap();
        assert!(matches!(formula, LtlFormula::Eventually(_)));
    }

    #[test]
    fn test_parse_next() {
        let formula = parse("X(alarm)").unwrap();
        assert!(matches!(formula, LtlFormula::Next(_)));

        let formula = parse("X alarm").unwrap();
        assert!(matches!(formula, LtlFormula::Next(_)));

        let formula = parse("next alarm").unwrap();
        assert!(matches!(formula, LtlFormula::Next(_)));
    }

    #[test]
    fn test_parse_until() {
        let formula = parse("request U grant").unwrap();
        assert!(matches!(formula, LtlFormula::Until { .. }));

        let formula = parse("request until grant").unwrap();
        assert!(matches!(formula, LtlFormula::Until { .. }));
    }

    #[test]
    fn test_parse_weak_until() {
        let formula = parse("request W grant").unwrap();
        assert!(matches!(formula, LtlFormula::WeakUntil { .. }));

        let formula = parse("request weak_until grant").unwrap();
        assert!(matches!(formula, LtlFormula::WeakUntil { .. }));
    }

    #[test]
    fn test_parse_release() {
        let formula = parse("request R grant").unwrap();
        assert!(matches!(formula, LtlFormula::Release { .. }));

        let formula = parse("request release grant").unwrap();
        assert!(matches!(formula, LtlFormula::Release { .. }));
    }

    #[test]
    fn test_parse_not() {
        let formula = parse("!deadlock").unwrap();
        assert!(matches!(formula, LtlFormula::Not(_)));

        let formula = parse("G !deadlock").unwrap();
        match formula {
            LtlFormula::Always(inner) => {
                assert!(matches!(*inner, LtlFormula::Not(_)));
            }
            _ => panic!("Expected Always(Not(_))"),
        }
    }

    /// An LTL body over RTL signals keeps each comparison as ONE predicate, in the text the
    /// mu-calculus parser reads: the annotation `ltl G (state_q == 5 -> !motor_up)` must not
    /// stop at `state_q` and report `==` as trailing input.
    #[test]
    fn comparison_atoms_parse_as_single_predicates() {
        let f = parse("G (state_q == 5 -> !motor_up)").unwrap();
        assert_eq!(
            f,
            LtlFormula::Always(Box::new(LtlFormula::Implies(
                Box::new(LtlFormula::Predicate("state_q == 5".to_string())),
                Box::new(LtlFormula::Not(Box::new(LtlFormula::Predicate(
                    "motor_up".to_string()
                )))),
            )))
        );
        let f = parse("G (cnt_q == cnt_q__past + 1 || cnt_q <= -1)").unwrap();
        assert_eq!(
            f,
            LtlFormula::Always(Box::new(LtlFormula::Or(
                Box::new(LtlFormula::Predicate(
                    "cnt_q == cnt_q__past + 1".to_string()
                )),
                Box::new(LtlFormula::Predicate("cnt_q <= -1".to_string())),
            )))
        );
    }

    /// `->` is implication, never a `>` comparison; and a dangling operator is an error that
    /// names what was expected, not a silent bare predicate.
    #[test]
    fn implication_is_not_a_comparison_and_a_dangling_operator_is_an_error() {
        assert_eq!(
            parse("a -> b").unwrap(),
            LtlFormula::Implies(
                Box::new(LtlFormula::Predicate("a".to_string())),
                Box::new(LtlFormula::Predicate("b".to_string())),
            )
        );
        let err = parse("G (a == )").unwrap_err();
        assert!(
            err.to_string().contains("after the comparison operator"),
            "{err}"
        );
    }

    #[test]
    fn test_parse_and() {
        let formula = parse("safe && bounded").unwrap();
        assert!(matches!(formula, LtlFormula::And(_, _)));
    }

    #[test]
    fn test_parse_or() {
        let formula = parse("error || warning").unwrap();
        assert!(matches!(formula, LtlFormula::Or(_, _)));
    }

    #[test]
    fn test_parse_implies() {
        let formula = parse("request -> F grant").unwrap();
        assert!(matches!(formula, LtlFormula::Implies(_, _)));
    }

    #[test]
    fn test_parse_precedence() {
        // Test that && binds tighter than ||
        let formula = parse("a && b || c").unwrap();
        match formula {
            LtlFormula::Or(left, right) => {
                assert!(matches!(*left, LtlFormula::And(_, _)));
                assert!(matches!(*right, LtlFormula::Predicate(_)));
            }
            _ => panic!("Expected Or at top level"),
        }

        // Test that -> binds loosest
        let formula = parse("a || b -> c").unwrap();
        assert!(matches!(formula, LtlFormula::Implies(_, _)));
    }

    #[test]
    fn test_parse_parentheses() {
        let formula = parse("G ((a && b) || c)").unwrap();
        assert!(matches!(formula, LtlFormula::Always(_)));
    }

    #[test]
    fn test_parse_nested() {
        let formula = parse("G (request -> F grant)").unwrap();
        assert!(matches!(formula, LtlFormula::Always(_)));

        let formula = parse("F G idle").unwrap();
        match formula {
            LtlFormula::Eventually(inner) => {
                assert!(matches!(*inner, LtlFormula::Always(_)));
            }
            _ => panic!("Expected Eventually(Always(_))"),
        }

        let formula = parse("G F heartbeat").unwrap();
        match formula {
            LtlFormula::Always(inner) => {
                assert!(matches!(*inner, LtlFormula::Eventually(_)));
            }
            _ => panic!("Expected Always(Eventually(_))"),
        }
    }

    #[test]
    fn test_parse_predicate() {
        let formula = parse("safe").unwrap();
        assert!(matches!(formula, LtlFormula::Predicate(ref s) if s == "safe"));
    }

    #[test]
    fn test_parse_true_false() {
        let formula = parse("true").unwrap();
        assert!(matches!(formula, LtlFormula::True));

        let formula = parse("false").unwrap();
        assert!(matches!(formula, LtlFormula::False));
    }

    #[test]
    fn test_parse_errors() {
        // Unclosed parenthesis
        assert!(parse("G (safe").is_err());

        // Unexpected trailing
        assert!(parse("safe extra").is_err());

        // Empty input
        assert!(parse("").is_err());
    }

    #[test]
    fn test_parse_complex() {
        // Complex nested formula
        let formula = parse("G (!deadlock && (request -> F grant))").unwrap();
        assert!(matches!(formula, LtlFormula::Always(_)));
    }
}
