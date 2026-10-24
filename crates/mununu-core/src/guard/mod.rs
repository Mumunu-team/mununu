//! Guard expression parsing and comparison operators.
//!
//! This module provides guard parsing functionality for CLTS DSL expressions.
//! It includes identifier sanitization, comparison operators, and guard
//! expression normalization.

use std::fmt;

/// Sanitizes an identifier to be valid in CLTS DSL.
///
/// This function:
/// - Replaces all non-alphanumeric characters (except underscore) with underscore
/// - Collapses multiple consecutive special characters into a single underscore
/// - Handles empty strings by using a default name
/// - Prepends underscore if the identifier starts with a digit
/// - Handles Unicode characters by converting them to underscore
///
/// # Examples
///
/// ```
/// use mununu_core::guard::sanitize_identifier;
///
/// assert_eq!(sanitize_identifier("Task$1"), "Task_1");
/// assert_eq!(sanitize_identifier("Process Handling"), "Process_Handling");
/// assert_eq!(sanitize_identifier("Service#2"), "Service_2");
/// assert_eq!(sanitize_identifier("User's Task"), "User_s_Task");
/// assert_eq!(sanitize_identifier("Task-1"), "Task_1");
/// assert_eq!(sanitize_identifier("123Task"), "_123Task");
/// assert_eq!(sanitize_identifier(""), "fragment");
/// ```
pub fn sanitize_identifier(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut last_was_special = false;

    for ch in input.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => {
                result.push(ch);
                last_was_special = false;
            }
            _ => {
                // Only add underscore if previous char wasn't also special
                // This collapses multiple consecutive special chars into one underscore
                if !last_was_special {
                    result.push('_');
                }
                last_was_special = true;
            }
        }
    }

    // Handle empty result
    if result.is_empty() {
        result.push_str("fragment");
    }

    // Handle leading digit
    if result
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_digit())
    {
        result.insert(0, '_');
    }

    result
}

/// Sanitizes a collection of identifiers and ensures they are unique by appending numeric suffixes.
///
/// This function:
/// 1. Sanitizes each identifier using `sanitize_identifier`
/// 2. Detects duplicates and appends numeric suffixes (e.g., "Task_1", "Task_2")
/// 3. Preserves the original order of identifiers
///
/// # Examples
///
/// ```
/// use mununu_core::guard::sanitize_and_deduplicate;
///
/// let names = vec!["Task 1".to_string(), "Task#2".to_string(), "Task 1".to_string()];
/// let sanitized = sanitize_and_deduplicate(&names);
/// assert_eq!(sanitized, vec!["Task_1", "Task_2", "Task_1_1"]);
/// ```
pub fn sanitize_and_deduplicate(names: &[String]) -> Vec<String> {
    let sanitized: Vec<String> = names.iter().map(|n| sanitize_identifier(n)).collect();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut result = Vec::with_capacity(sanitized.len());

    for name in sanitized.iter() {
        let count = counts.entry(name.clone()).or_insert(0);
        *count += 1;

        if *count == 1 {
            result.push(name.clone());
        } else {
            result.push(format!("{}_{}", name, *count - 1));
        }
    }

    result
}

/// Escapes special characters in a string for use in DSL meta comments.
pub fn escape_meta_string(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Checks if a guard expression is a single identifier (invalid in DSL guard contexts).
pub fn is_single_identifier_guard(expression: &str) -> bool {
    let normalized = expression.trim();
    if normalized.is_empty()
        || normalized.eq_ignore_ascii_case("true")
        || normalized.eq_ignore_ascii_case("false")
    {
        return false;
    }
    if normalized.parse::<i64>().is_ok() || normalized.parse::<f64>().is_ok() {
        return false;
    }
    if normalized.contains(' ')
        || normalized.contains('&')
        || normalized.contains('|')
        || normalized.contains('!')
        || normalized.contains('(')
        || normalized.contains(')')
        || normalized.contains('=')
        || normalized.contains('<')
        || normalized.contains('>')
    {
        return false;
    }
    true
}

/// A helper for building DSL strings with automatic escaping and sanitization.
pub struct DslWriter {
    buffer: String,
    indent_level: usize,
    indent_string: String,
}

impl DslWriter {
    /// Creates a new `DslWriter` with default indentation (4 spaces).
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            indent_level: 0,
            indent_string: "    ".to_string(),
        }
    }

    /// Creates a new `DslWriter` with custom indentation.
    pub fn with_indent(indent: usize) -> Self {
        Self {
            buffer: String::new(),
            indent_level: 0,
            indent_string: " ".repeat(indent),
        }
    }

    /// Writes a line to the DSL buffer with current indentation.
    pub fn write_line(&mut self, line: &str) {
        if !line.is_empty() {
            let indent = self.indent_string.repeat(self.indent_level);
            self.buffer.push_str(&indent);
            self.buffer.push_str(line);
        }
        self.buffer.push('\n');
    }

    /// Writes an empty line to the DSL buffer.
    pub fn write_empty_line(&mut self) {
        self.buffer.push('\n');
    }

    /// Writes a meta comment block with automatic escaping.
    pub fn write_meta_comment(&mut self, id: &str, comment: &str) {
        let escaped_comment = escape_meta_string(comment);
        self.write_line(&format!(
            r#"meta {{ id = "{}"; comment = "{}"; }}"#,
            id, escaped_comment
        ));
    }

    /// Writes a meta comment block with only a comment (no id).
    pub fn write_meta_comment_only(&mut self, comment: &str) {
        let escaped_comment = escape_meta_string(comment);
        self.write_line(&format!(r#"meta {{ comment = "{}"; }}"#, escaped_comment));
    }

    /// Writes a sanitized identifier.
    pub fn write_identifier(&mut self, ident: &str) {
        let sanitized = sanitize_identifier(ident);
        self.buffer.push_str(&sanitized);
    }

    /// Writes a sanitized identifier as a line.
    pub fn write_identifier_line(&mut self, ident: &str) {
        let sanitized = sanitize_identifier(ident);
        self.write_line(&sanitized);
    }

    /// Increases the indentation level.
    pub fn indent(&mut self) {
        self.indent_level += 1;
    }

    /// Decreases the indentation level.
    pub fn deindent(&mut self) {
        if self.indent_level > 0 {
            self.indent_level -= 1;
        }
    }

    /// Returns the current DSL string and consumes the writer.
    pub fn finish(self) -> String {
        self.buffer
    }

    /// Returns a reference to the current DSL string.
    pub fn as_str(&self) -> &str {
        &self.buffer
    }

    /// Appends raw content to the buffer (use with caution).
    pub fn push_str(&mut self, s: &str) {
        self.buffer.push_str(s);
    }

    /// Appends a character to the buffer.
    pub fn push(&mut self, ch: char) {
        self.buffer.push(ch);
    }
}

impl Default for DslWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Comparison operator recognised in guard expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub enum ComparisonOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl fmt::Display for ComparisonOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let symbol = match self {
            ComparisonOp::Eq => "==",
            ComparisonOp::Ne => "!=",
            ComparisonOp::Lt => "<",
            ComparisonOp::Le => "<=",
            ComparisonOp::Gt => ">",
            ComparisonOp::Ge => ">=",
        };
        write!(f, "{symbol}")
    }
}

/// Simplified guard expression produced by the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardExpr {
    True,
    False,
    Predicate(String),
    Comparison {
        left: String,
        op: ComparisonOp,
        right: String,
    },
}

fn split_comparison(expr: &str) -> Option<(String, ComparisonOp, String)> {
    // Strip balanced outer parentheses before splitting, e.g. "(mode==0)" → "mode==0"
    let expr = strip_outer_parens(expr);

    let candidates = [
        ("<=", ComparisonOp::Le),
        (">=", ComparisonOp::Ge),
        ("==", ComparisonOp::Eq),
        ("!=", ComparisonOp::Ne),
        ("=", ComparisonOp::Eq),
        ("<", ComparisonOp::Lt),
        (">", ComparisonOp::Gt),
    ];

    for (symbol, op) in candidates {
        if let Some(idx) = expr.find(symbol) {
            let left = expr[..idx].trim();
            let right = expr[idx + symbol.len()..].trim();
            if !left.is_empty() && !right.is_empty() {
                return Some((left.to_string(), op, right.to_string()));
            }
        }
    }
    None
}

/// Strips balanced outer parentheses from a string.
fn strip_outer_parens(s: &str) -> &str {
    let s = s.trim();
    if s.starts_with('(') && s.ends_with(')') {
        // Verify the parens are balanced (the closing paren matches the opening one)
        let inner = &s[1..s.len() - 1];
        let mut depth = 0i32;
        let balanced = inner.chars().all(|c| {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            depth >= 0
        });
        if balanced && depth == 0 {
            return inner;
        }
    }
    s
}

/// Checks if a string is a numeric literal (integer or decimal).
fn is_numeric_literal(s: &str) -> bool {
    s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok()
}

/// Checks if a string is a boolean literal.
fn is_boolean_literal(s: &str) -> bool {
    s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false")
}

/// Checks if a string is a string literal (quoted).
fn is_string_literal(s: &str) -> bool {
    (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
}

fn sanitize_guard_token(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }
    if is_numeric_literal(trimmed) || is_boolean_literal(trimmed) {
        return trimmed.to_string();
    }
    if is_string_literal(trimmed) {
        let inner = &trimmed[1..trimmed.len() - 1];
        return sanitize_identifier(inner);
    }
    sanitize_identifier(trimmed)
}

#[allow(clippy::while_let_on_iterator)]
fn collect_parenthesized_content(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut depth = 1;
    let mut content = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '(' => {
                depth += 1;
                content.push(ch);
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                content.push(ch);
            }
            _ => content.push(ch),
        }
    }
    content
}

#[allow(clippy::while_let_on_iterator)]
fn normalize_helper_calls(expr: &str) -> String {
    use std::iter::Peekable;
    use std::str::Chars;

    fn consume_whitespace(chars: &mut Peekable<Chars<'_>>) {
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
            } else {
                break;
            }
        }
    }

    fn is_known_helper(ident: &str) -> bool {
        let key: String = ident
            .chars()
            .map(|c| {
                if c == ':' || c == '.' {
                    '_'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
        // No known helper functions; kept as extension point.
        let _ = key;
        false
    }

    let mut result = String::new();
    let mut chars = expr.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut ident = String::new();
            ident.push(ch);
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || matches!(next, '_' | ':' | '.') {
                    ident.push(chars.next().unwrap());
                } else {
                    break;
                }
            }

            if ident.eq_ignore_ascii_case("not") {
                let mut lookahead = chars.clone();
                consume_whitespace(&mut lookahead);
                if lookahead.peek() == Some(&'(') {
                    result.push('!');
                    consume_whitespace(&mut chars);
                    continue;
                }
            }

            let mut lookahead = chars.clone();
            consume_whitespace(&mut lookahead);
            let is_helper_call = is_known_helper(&ident) && lookahead.peek() == Some(&'(');

            if is_helper_call {
                consume_whitespace(&mut chars);
                chars.next(); // consume '('
                let arg = collect_parenthesized_content(&mut chars);
                let sanitized_ident = sanitize_identifier(&ident);
                let sanitized_arg = sanitize_guard_token(arg.trim());
                if sanitized_arg.is_empty() {
                    result.push_str(&sanitized_ident);
                } else {
                    result.push_str(&format!("{}_{}", sanitized_ident, sanitized_arg));
                }
            } else {
                result.push_str(&ident);
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Sanitizes identifiers in a guard expression while preserving structure.
fn sanitize_guard_identifiers(expr: &str) -> String {
    let normalized_expr = normalize_helper_calls(expr);
    let trimmed = normalized_expr.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("true") {
        return trimmed.to_string();
    }

    if let Some((left, op, right)) = split_comparison(trimmed) {
        let left_trimmed = left.trim();
        let right_trimmed = right.trim();
        let left_sanitized = sanitize_guard_token(left_trimmed);
        let right_sanitized = sanitize_guard_token(right_trimmed);
        return format!("{} {} {}", left_sanitized, op, right_sanitized);
    }

    let mut result = String::new();
    let mut current_token = String::new();
    let mut chars = trimmed.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '&' if chars.peek() == Some(&'&') => {
                if !current_token.is_empty() {
                    let token_trimmed = current_token.trim();
                    result.push_str(&sanitize_guard_token(token_trimmed));
                    current_token.clear();
                }
                result.push_str("&&");
                chars.next();
            }
            '|' if chars.peek() == Some(&'|') => {
                if !current_token.is_empty() {
                    let token_trimmed = current_token.trim();
                    result.push_str(&sanitize_guard_token(token_trimmed));
                    current_token.clear();
                }
                result.push_str("||");
                chars.next();
            }
            '!' | '=' | '<' | '>' | '(' | ')' | ' ' => {
                if !current_token.is_empty() {
                    let token_trimmed = current_token.trim();
                    result.push_str(&sanitize_guard_token(token_trimmed));
                    current_token.clear();
                }
                result.push(ch);
            }
            _ => {
                current_token.push(ch);
            }
        }
    }

    if !current_token.is_empty() {
        let token_trimmed = current_token.trim();
        result.push_str(&sanitize_guard_token(token_trimmed));
    }

    result.trim().to_string()
}

/// Determines if a guard expression is static (constant value) or dynamic (state-dependent).
/// Returns `Some(true)` if always enabled, `Some(false)` if always disabled, `None` if dynamic.
pub fn is_static_guard(guard_expr: &GuardExpr) -> Option<bool> {
    match guard_expr {
        GuardExpr::True => Some(true),
        GuardExpr::False => Some(false),
        GuardExpr::Predicate(_) => None,
        GuardExpr::Comparison { left, op, right } => {
            let left_is_const = is_numeric_literal(left) || is_boolean_literal(left);
            let right_is_const = is_numeric_literal(right) || is_boolean_literal(right);

            if left_is_const && right_is_const {
                Some(evaluate_constant_comparison(left, *op, right))
            } else {
                None
            }
        }
    }
}

/// Evaluates a constant comparison expression.
fn evaluate_constant_comparison(left: &str, op: ComparisonOp, right: &str) -> bool {
    if let (Ok(left_num), Ok(right_num)) = (left.parse::<f64>(), right.parse::<f64>()) {
        match op {
            ComparisonOp::Eq => (left_num - right_num).abs() < f64::EPSILON,
            ComparisonOp::Ne => (left_num - right_num).abs() >= f64::EPSILON,
            ComparisonOp::Lt => left_num < right_num,
            ComparisonOp::Le => left_num <= right_num,
            ComparisonOp::Gt => left_num > right_num,
            ComparisonOp::Ge => left_num >= right_num,
        }
    } else if let (Ok(left_bool), Ok(right_bool)) = (left.parse::<bool>(), right.parse::<bool>()) {
        match op {
            ComparisonOp::Eq => left_bool == right_bool,
            ComparisonOp::Ne => left_bool != right_bool,
            _ => false,
        }
    } else {
        let left_normalized = left.trim().to_lowercase();
        let right_normalized = right.trim().to_lowercase();
        match op {
            ComparisonOp::Eq => left_normalized == right_normalized,
            ComparisonOp::Ne => left_normalized != right_normalized,
            _ => false,
        }
    }
}

/// Parses a guard into its structured representation.
/// Returns (normalized_guard_string, GuardExpr).
pub fn parse_guard(expr: &str) -> (String, GuardExpr) {
    let sanitized = sanitize_guard_identifiers(expr);
    let normalized = sanitized.trim().to_string();

    if normalized.is_empty() {
        return (normalized, GuardExpr::True);
    }
    if normalized.eq_ignore_ascii_case("true") {
        return (normalized, GuardExpr::True);
    }
    if normalized.eq_ignore_ascii_case("false") {
        return (normalized, GuardExpr::False);
    }
    if let Some((left, op, right)) = split_comparison(&normalized) {
        return (
            normalized,
            GuardExpr::Comparison {
                left: left.trim().to_string(),
                op,
                right: right.trim().to_string(),
            },
        );
    }

    (normalized.clone(), GuardExpr::Predicate(normalized))
}

/// A guard with its boolean structure: `&&`, `||`, `!` and parentheses over the comparison /
/// predicate leaves [`parse_guard`] reads. `parse_guard` alone splits at the first comparison
/// operator, so `a >= 1 && b == 0` came back as one mangled comparison whose variable no state
/// knew, and the transition silently never fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardTree {
    Leaf(GuardExpr),
    And(Box<GuardTree>, Box<GuardTree>),
    Or(Box<GuardTree>, Box<GuardTree>),
    Not(Box<GuardTree>),
}

impl GuardTree {
    /// Every leaf, left to right.
    pub fn leaves(&self) -> Vec<&GuardExpr> {
        match self {
            GuardTree::Leaf(e) => vec![e],
            GuardTree::And(a, b) | GuardTree::Or(a, b) => {
                let mut v = a.leaves();
                v.extend(b.leaves());
                v
            }
            GuardTree::Not(a) => a.leaves(),
        }
    }

    /// `Some(value)` when every leaf the result depends on is constant ([`is_static_guard`]).
    pub fn static_value(&self) -> Option<bool> {
        match self {
            GuardTree::Leaf(e) => is_static_guard(e),
            GuardTree::Not(a) => a.static_value().map(|v| !v),
            GuardTree::And(a, b) => match (a.static_value(), b.static_value()) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            GuardTree::Or(a, b) => match (a.static_value(), b.static_value()) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
        }
    }
}

/// The byte offset of the first `op` (`"&&"` / `"||"`) outside any parentheses.
fn find_top_level(expr: &str, op: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ if depth == 0 && expr[i..].starts_with(op) => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parse a guard into its boolean structure. `||` binds loosest, then `&&`, then `!`; a leaf is
/// read by [`parse_guard`]. `!=` is a comparison, not a negation.
pub fn parse_guard_tree(expr: &str) -> GuardTree {
    let trimmed = expr.trim();
    for (op, mk) in [
        (
            "||",
            GuardTree::Or as fn(Box<GuardTree>, Box<GuardTree>) -> GuardTree,
        ),
        ("&&", GuardTree::And),
    ] {
        if let Some(i) = find_top_level(trimmed, op) {
            return mk(
                Box::new(parse_guard_tree(&trimmed[..i])),
                Box::new(parse_guard_tree(&trimmed[i + op.len()..])),
            );
        }
    }
    if let Some(rest) = trimmed.strip_prefix('!')
        && !rest.starts_with('=')
    {
        return GuardTree::Not(Box::new(parse_guard_tree(rest)));
    }
    let inner = strip_outer_parens(trimmed);
    if inner.len() != trimmed.len() {
        return parse_guard_tree(inner);
    }
    GuardTree::Leaf(parse_guard(trimmed).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_tree_reads_conjunction_disjunction_and_negation() {
        let leaf = |l: &str, op, r: &str| {
            GuardTree::Leaf(GuardExpr::Comparison {
                left: l.into(),
                op,
                right: r.into(),
            })
        };
        assert_eq!(
            parse_guard_tree("((a>=1)&&(b==0))"),
            GuardTree::And(
                Box::new(leaf("a", ComparisonOp::Ge, "1")),
                Box::new(leaf("b", ComparisonOp::Eq, "0"))
            )
        );
        // `||` binds loosest; `!=` stays a comparison; `!(…)` negates.
        assert_eq!(
            parse_guard_tree("a != 2 || !(b < 3) && c == 1"),
            GuardTree::Or(
                Box::new(leaf("a", ComparisonOp::Ne, "2")),
                Box::new(GuardTree::And(
                    Box::new(GuardTree::Not(Box::new(leaf("b", ComparisonOp::Lt, "3")))),
                    Box::new(leaf("c", ComparisonOp::Eq, "1"))
                ))
            )
        );
        assert_eq!(
            parse_guard_tree("1 == 1 && 2 > 3").static_value(),
            Some(false)
        );
        assert_eq!(
            parse_guard_tree("1 == 1 || x > 3").static_value(),
            Some(true)
        );
        assert_eq!(parse_guard_tree("x > 3 && 1 == 1").static_value(), None);
    }

    #[test]
    fn test_sanitize_identifier() {
        assert_eq!(sanitize_identifier("Task$1"), "Task_1");
        assert_eq!(sanitize_identifier("Process Handling"), "Process_Handling");
        assert_eq!(sanitize_identifier("Service#2"), "Service_2");
        assert_eq!(sanitize_identifier("User's Task"), "User_s_Task");
        assert_eq!(sanitize_identifier("Task-1"), "Task_1");
        assert_eq!(sanitize_identifier("123Task"), "_123Task");
        assert_eq!(sanitize_identifier(""), "fragment");
        assert_eq!(sanitize_identifier("Task___1"), "Task___1");
    }

    #[test]
    fn test_sanitize_and_deduplicate() {
        let names = vec![
            "Task 1".to_string(),
            "Task#2".to_string(),
            "Task 1".to_string(),
        ];
        let sanitized = sanitize_and_deduplicate(&names);
        assert_eq!(sanitized, vec!["Task_1", "Task_2", "Task_1_1"]);
    }

    #[test]
    fn test_escape_meta_string() {
        assert_eq!(
            escape_meta_string(r#"He said "Hello""#),
            r#"He said \"Hello\""#
        );
        assert_eq!(escape_meta_string("Line 1\nLine 2"), r#"Line 1\nLine 2"#);
        assert_eq!(escape_meta_string("Tab\tHere"), r#"Tab\tHere"#);
        assert_eq!(escape_meta_string("Back\\Slash"), r#"Back\\Slash"#);
        assert_eq!(escape_meta_string("O'Brien"), "O'Brien");
    }

    #[test]
    fn test_is_single_identifier_guard() {
        assert!(is_single_identifier_guard("Handling"));
        assert!(is_single_identifier_guard("process"));
        assert!(!is_single_identifier_guard("x == 5"));
        assert!(!is_single_identifier_guard("true"));
        assert!(!is_single_identifier_guard("false"));
        assert!(!is_single_identifier_guard("123"));
        assert!(!is_single_identifier_guard("x && y"));
    }

    #[test]
    fn test_dsl_writer_basic() {
        let mut writer = DslWriter::new();
        writer.write_line("context MyContext {");
        writer.indent();
        writer.write_line("alphabet {");
        writer.indent();
        writer.write_line("label start;");
        writer.deindent();
        writer.write_line("}");
        writer.deindent();
        writer.write_line("}");

        let dsl = writer.finish();
        assert!(dsl.contains("context MyContext {"));
        assert!(dsl.contains("    alphabet {"));
        assert!(dsl.contains("        label start;"));
    }

    #[test]
    fn parses_simple_guard() {
        let (normalized, guard) = parse_guard("counter >= 2");
        assert_eq!(normalized, "counter >= 2");
        assert_eq!(
            guard,
            GuardExpr::Comparison {
                left: "counter".into(),
                op: ComparisonOp::Ge,
                right: "2".into(),
            }
        );
    }

    #[test]
    fn falls_back_to_predicate() {
        let (normalized, guard) = parse_guard("req && ready");
        assert_eq!(normalized, "req && ready");
        assert_eq!(guard, GuardExpr::Predicate("req && ready".into()));
    }

    #[test]
    fn handles_true() {
        let (_, guard) = parse_guard("true");
        assert_eq!(guard, GuardExpr::True);
    }

    #[test]
    fn handles_empty() {
        let (_, guard) = parse_guard("");
        assert_eq!(guard, GuardExpr::True);
    }

    #[test]
    fn sanitizes_identifiers_in_comparison() {
        let (normalized, guard) = parse_guard("Handling == true");
        assert_eq!(normalized, "Handling == true");
        assert_eq!(
            guard,
            GuardExpr::Comparison {
                left: "Handling".into(),
                op: ComparisonOp::Eq,
                right: "true".into(),
            }
        );
    }

    #[test]
    fn sanitizes_identifiers_in_predicate() {
        let (normalized, guard) = parse_guard("MIWG.process");
        assert_eq!(normalized, "MIWG_process");
        assert_eq!(guard, GuardExpr::Predicate("MIWG_process".into()));
    }

    #[test]
    fn detects_static_true_guard() {
        let (_, guard) = parse_guard("true");
        assert_eq!(is_static_guard(&guard), Some(true));
    }

    #[test]
    fn detects_static_constant_comparison() {
        let (_, guard) = parse_guard("2 >= 1");
        assert_eq!(is_static_guard(&guard), Some(true));

        let (_, guard) = parse_guard("1 >= 2");
        assert_eq!(is_static_guard(&guard), Some(false));
    }

    #[test]
    fn detects_dynamic_guard_with_variable() {
        let (_, guard) = parse_guard("x >= 1");
        assert_eq!(is_static_guard(&guard), None);
    }
}
