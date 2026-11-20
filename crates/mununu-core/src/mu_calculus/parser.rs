use thiserror::Error;

use super::{Control, Formula, FormulaBuilder, FormulaVarId, Guard, ModalKind, Node, NodeId};

/// Parses a textual μ-calculus formula into the structured [`Formula`] arena.
///
/// The supported syntax covers:
/// - Boolean connectives (`and`, `or`, `not`, `&&`, `||`, `!`, `¬`, `∧`, `∨`).
/// - Fixpoint operators (`mu` / `μ` and `nu` / `ν`) with bound variables.
/// - Modal operators (`[]`, `<>`, `⟨⟩`) with optional guards.
/// - Guard components for modalities (`labels`, `req_cur`, `forb_cur`, `req_next`,
///   `forb_next`, `ctrl`, and `steps`), normalised into a [`Guard`] value.
///
/// # Parameters
///
/// * `input` - Source string containing a single μ-calculus formula. Leading and trailing
///   whitespace is ignored. Any non-whitespace characters that remain after a successful
///   parse are treated as an error (see [`ParseError::UnexpectedTrailing`]).
///
/// # Returns
///
/// On success, returns a fully constructed [`Formula`] whose nodes encode the parsed
/// abstract syntax tree and fixpoint variable table.
///
/// # Errors
///
/// Returns a [`ParseError`] when the input cannot be parsed. Typical causes include:
/// - Unterminated expressions or missing delimiters (e.g., missing `)` or `.`).
/// - Unexpected tokens where a subformula or identifier is required.
/// - Malformed or duplicate guard fields inside modal guards.
/// - Trailing characters after an otherwise valid formula.
pub fn parse(input: &str) -> Result<Formula, ParseError> {
    let mut parser = Parser::new(input);
    let root = parser.parse_formula()?;
    parser.skip_whitespace();
    if !parser.is_eof() {
        return Err(parser.error_here(ParseErrorKind::UnexpectedTrailing));
    }

    let Parser { builder, .. } = parser;

    Ok(Formula::new(root, builder.nodes, builder.vars))
}

/// Errors that can occur while parsing μ-calculus source.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("unexpected token at byte {pos}: {message}")]
    Unexpected { pos: usize, message: String },
    #[error("guard field `{field}` specified multiple times (byte {pos})")]
    DuplicateGuardField { field: String, pos: usize },
}

/// Maximum nesting depth to prevent stack overflow on adversarial input.
const MAX_PARSE_DEPTH: usize = 256;

#[derive(Debug)]
struct Parser<'a> {
    input: &'a str,
    pos: usize,
    builder: FormulaBuilder,
    binder_stack: Vec<Binder>,
    depth: usize,
}

#[derive(Debug)]
struct Binder {
    name: String,
    id: FormulaVarId,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            builder: FormulaBuilder::default(),
            binder_stack: Vec::new(),
            depth: 0,
        }
    }

    fn parse_formula(&mut self) -> Result<NodeId, ParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<NodeId, ParseError> {
        let mut left = self.parse_and()?;
        loop {
            self.skip_whitespace();
            if self.try_consume_or() {
                let right = self.parse_and()?;
                left = self.builder.push_node(Node::Or(left, right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<NodeId, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            self.skip_whitespace();
            if self.try_consume_and() {
                let right = self.parse_unary()?;
                left = self.builder.push_node(Node::And(left, right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<NodeId, ParseError> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err(self.error_here(ParseErrorKind::Expected(
                "expression (maximum nesting depth exceeded)",
            )));
        }
        let result = self.parse_unary_inner();
        self.depth -= 1;
        result
    }

    fn parse_unary_inner(&mut self) -> Result<NodeId, ParseError> {
        self.skip_whitespace();
        if self.try_consume_not() {
            let operand = self.parse_unary()?;
            return Ok(self.builder.push_node(Node::Not(operand)));
        }

        if let Some(kind) = self.try_consume_fixpoint()? {
            return Ok(kind);
        }

        if let Some(node) = self.try_consume_modal()? {
            return Ok(node);
        }

        self.parse_primary()
    }

    fn try_consume_fixpoint(&mut self) -> Result<Option<NodeId>, ParseError> {
        self.skip_whitespace();
        if self.consume_keyword("mu") || self.try_consume_char('μ') {
            self.skip_whitespace();
            let name = self
                .parse_identifier()
                .ok_or_else(|| self.error_here(ParseErrorKind::Expected("fixpoint variable")))?;
            self.skip_whitespace();
            self.expect_char('.')?;
            let var_id = self.builder.push_var(name.clone());
            self.binder_stack.push(Binder { name, id: var_id });
            // Textbook mu-calculus convention: a fixpoint binder extends as
            // far right as possible — `mu X. φ ∧ ψ` ≡ `mu X. (φ ∧ ψ)`. Parse
            // the body with parse_or (the lowest-precedence parser) so the
            // body absorbs following `&&` / `||` at the same nesting level.
            // Existing call sites use explicit parens around the body, so
            // their parse tree is unchanged.
            let body = self.parse_or()?;
            self.binder_stack.pop();
            let node = self.builder.push_node(Node::Mu { var: var_id, body });
            return Ok(Some(node));
        }
        if self.consume_keyword("nu") || self.try_consume_char('ν') {
            self.skip_whitespace();
            let name = self
                .parse_identifier()
                .ok_or_else(|| self.error_here(ParseErrorKind::Expected("fixpoint variable")))?;
            self.skip_whitespace();
            self.expect_char('.')?;
            let var_id = self.builder.push_var(name.clone());
            self.binder_stack.push(Binder { name, id: var_id });
            let body = self.parse_or()?;
            self.binder_stack.pop();
            let node = self.builder.push_node(Node::Nu { var: var_id, body });
            return Ok(Some(node));
        }
        Ok(None)
    }

    fn try_consume_modal(&mut self) -> Result<Option<NodeId>, ParseError> {
        self.skip_whitespace();
        if self.try_consume_char('[') {
            let guard = self.parse_guard(']')?;
            let target = self.parse_unary()?;
            let node = self.builder.push_node(Node::Modal {
                kind: ModalKind::Box,
                guard,
                target,
            });
            return Ok(Some(node));
        }
        if self.try_consume_char('⟨') {
            let guard = self.parse_guard('⟩')?;
            let target = self.parse_unary()?;
            let node = self.builder.push_node(Node::Modal {
                kind: ModalKind::Diamond,
                guard,
                target,
            });
            return Ok(Some(node));
        }
        if self.try_consume_char('<') {
            let guard = self.parse_guard('>')?;
            let target = self.parse_unary()?;
            let node = self.builder.push_node(Node::Modal {
                kind: ModalKind::Diamond,
                guard,
                target,
            });
            return Ok(Some(node));
        }
        Ok(None)
    }

    fn parse_primary(&mut self) -> Result<NodeId, ParseError> {
        self.skip_whitespace();
        if self.try_consume_char('(') {
            let node = self.parse_formula()?;
            self.skip_whitespace();
            self.expect_char(')')?;
            return Ok(node);
        }

        if self.consume_keyword("true") {
            return Ok(self.builder.push_node(Node::True));
        }
        if self.consume_keyword("false") {
            return Ok(self.builder.push_node(Node::False));
        }

        if let Some(identifier) = self.parse_identifier() {
            if let Some(var_id) = self.lookup_binder(&identifier) {
                return Ok(self.builder.push_node(Node::Variable(var_id)));
            }
            // Phase A.3 follow-up — capture `signal == const`, `signal != const`,
            // `signal < N`, `signal <= N`, `signal > N`, `signal >= N` as a single
            // `Node::Predicate(full_expression)`. The evaluator's on-demand path
            // (`evaluate_expression_on_demand`) parses the full string as a
            // guard expression and evaluates it against per-state abstract values
            // populated from the CLTS's `state_valuations` by `RealizedContext::environment_for`.
            //
            // Without this widening, `signal == 5` would parse as `Predicate("signal")`
            // followed by garbage; the evaluator never sees the constant.
            let mut full = identifier.clone();
            self.skip_whitespace();
            if let Some(op_str) = self.try_consume_comparison_op() {
                full.push(' ');
                full.push_str(op_str);
                self.skip_whitespace();
                if let Some(rhs) = self.parse_comparison_rhs() {
                    full.push(' ');
                    full.push_str(&rhs);
                    // H.G — an arithmetic-addend RHS `<reg> + <int>`
                    // (`cnt_q == cnt_q__past + 1`, sysrst `CntIncr_A`). Only when
                    // the RHS is a register (identifier), consume an optional
                    // ` + <int>` so the atom string carries the full arithmetic
                    // relational for `parse_predicate_expr` (→ `CmpRegAddend`).
                    if rhs.starts_with(is_ident_start)
                        && let Some(addend) = self.try_consume_addend()
                    {
                        full.push_str(&addend);
                    }
                    return Ok(self.builder.push_node(Node::Predicate(full)));
                }
                // No valid RHS — fall back to predicate-only. The leading
                // identifier already consumed; mark the formula as malformed.
                return Err(self.error_here(ParseErrorKind::Expected("comparison rhs")));
            }
            return Ok(self.builder.push_node(Node::Predicate(identifier)));
        }

        Err(self.error_here(ParseErrorKind::Expected("formula")))
    }

    /// Try to consume one of the comparison operators (`==`, `!=`, `<=`, `>=`,
    /// `<`, `>`) and return its canonical text form. Returns `None` if the
    /// next characters do not form a comparison operator.
    fn try_consume_comparison_op(&mut self) -> Option<&'static str> {
        let two = self
            .input
            .get(self.pos..self.pos.saturating_add(2))
            .unwrap_or("");
        let canonical = match two {
            "==" => Some("=="),
            "!=" => Some("!="),
            "<=" => Some("<="),
            ">=" => Some(">="),
            _ => None,
        };
        if let Some(op) = canonical {
            self.pos += op.len();
            return Some(op);
        }
        let one = self.peek_char()?;
        let single = match one {
            '<' => Some("<"),
            '>' => Some(">"),
            _ => None,
        };
        if let Some(op) = single {
            // Cheap reject of cases like `>` inside diamonds — the parser
            // never reaches parse_primary with a leading identifier when the
            // surrounding modality is being consumed, so it's safe to treat
            // a bare `<` / `>` here as comparison.
            self.consume_char();
            return Some(op);
        }
        None
    }

    /// Parse the right-hand side of a comparison: an integer literal, a
    /// boolean literal, or an identifier. Returns `None` if none of these
    /// match.
    fn parse_comparison_rhs(&mut self) -> Option<String> {
        self.skip_whitespace();
        // Integer (possibly signed)
        let start = self.pos;
        if matches!(self.peek_char(), Some('-' | '+')) {
            self.consume_char();
        }
        let mut has_digit = false;
        while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
            self.consume_char();
            has_digit = true;
        }
        if has_digit {
            return Some(self.input[start..self.pos].to_string());
        }
        // Reset (no number consumed) and try identifier or boolean literal.
        self.pos = start;
        if self.consume_keyword("true") {
            return Some("true".to_string());
        }
        if self.consume_keyword("false") {
            return Some("false".to_string());
        }
        self.parse_identifier()
    }

    /// H.G — after a comparison RHS register, optionally consume ` + <int>` (the
    /// arithmetic addend of `cnt_q == cnt_q__past + 1`). Returns the canonical
    /// ` + <digits>` suffix to append to the predicate atom string (so
    /// `parse_predicate_expr` reads a `CmpRegAddend`), or `None` if no `+ <int>`
    /// follows (position restored). Only `+` (a single constant addend) — no
    /// general arithmetic.
    fn try_consume_addend(&mut self) -> Option<String> {
        let save = self.pos;
        self.skip_whitespace();
        if self.peek_char() != Some('+') {
            self.pos = save;
            return None;
        }
        self.consume_char(); // '+'
        self.skip_whitespace();
        let dstart = self.pos;
        let mut has_digit = false;
        while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
            self.consume_char();
            has_digit = true;
        }
        if !has_digit {
            self.pos = save;
            return None;
        }
        let digits = self.input[dstart..self.pos].to_string();
        Some(format!(" + {digits}"))
    }

    /// Parses a modal guard up to the provided closing delimiter.
    ///
    /// Guards may be written in bracketed form (e.g. `[(labels = {a}, ctrl = controllable)]`)
    /// or as a single component (e.g. `<labels = {tick}>`). The parser normalises the
    /// result into a [`Guard`] with label, variable, controllability and step constraints.
    ///
    /// # Parameters
    ///
    /// * `closing` - The closing delimiter that terminates the guard (`]` for box,
    ///   `>` or `⟩` for diamond).
    ///
    /// # Returns
    ///
    /// A [`Guard`] value describing the parsed constraints. When the guard is empty
    /// (`[]` or `<>` with no components), this function returns [`Guard::default()`].
    ///
    /// # Errors
    ///
    /// Returns a [`ParseError`] when the guard syntax is malformed, when a guard
    /// field appears more than once, or when expected delimiters are missing.
    fn parse_guard(&mut self, closing: char) -> Result<Guard, ParseError> {
        self.skip_whitespace();
        if self.try_consume_char(closing) {
            return Ok(Guard::default());
        }

        let mut guard = Guard::default();
        let mut seen_labels = false;
        let mut seen_req_cur = false;
        let mut seen_forb_cur = false;
        let mut seen_req_next = false;
        let mut seen_forb_next = false;
        let mut seen_ctrl = false;
        let mut seen_steps = false;

        self.skip_whitespace();
        if self.try_consume_char('(') {
            loop {
                self.skip_whitespace();
                if self.try_consume_char(')') {
                    break;
                }

                let component = self.read_component(&[',', ')'])?;
                self.apply_guard_component(
                    component.as_str(),
                    &mut guard,
                    GuardSeen {
                        labels: &mut seen_labels,
                        req_cur: &mut seen_req_cur,
                        forb_cur: &mut seen_forb_cur,
                        req_next: &mut seen_req_next,
                        forb_next: &mut seen_forb_next,
                        ctrl: &mut seen_ctrl,
                        steps: &mut seen_steps,
                    },
                )?;

                self.skip_whitespace();
                if self.try_consume_char(',') {
                    continue;
                }
                if self.try_consume_char(')') {
                    break;
                }
                if self.peek_char().is_some() {
                    return Err(self.error_here(ParseErrorKind::Expected("`)` or `,`")));
                }
                return Err(ParseError::UnexpectedEof);
            }
        } else {
            let component = self.read_until(closing)?;
            self.apply_guard_component(
                component.trim(),
                &mut guard,
                GuardSeen {
                    labels: &mut seen_labels,
                    req_cur: &mut seen_req_cur,
                    forb_cur: &mut seen_forb_cur,
                    req_next: &mut seen_req_next,
                    forb_next: &mut seen_forb_next,
                    ctrl: &mut seen_ctrl,
                    steps: &mut seen_steps,
                },
            )?;
        }

        self.skip_whitespace();
        self.expect_char(closing)?;
        Ok(guard)
    }

    /// Applies a single guard component string to the in-progress [`Guard`].
    ///
    /// Components are of the form `key = {v1, v2}` (for sets) or `key = value`
    /// (for `ctrl` and `steps`). This helper is responsible for updating the
    /// appropriate field in `guard` and enforcing that each field appears at
    /// most once.
    ///
    /// # Parameters
    ///
    /// * `component` - Raw component text (without surrounding commas or braces).
    /// * `guard` - The [`Guard`] being constructed.
    /// * `seen` - Book-keeping flags tracking which keys have already been used.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` when the component has been successfully incorporated
    /// into the guard.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::DuplicateGuardField`] when a guard field is assigned
    /// more than once, or [`ParseError::Unexpected`] when a component has an
    /// unknown key or unsupported comparator.
    fn apply_guard_component(
        &self,
        component: &str,
        guard: &mut Guard,
        seen: GuardSeen<'_>,
    ) -> Result<(), ParseError> {
        let trimmed = component.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("steps") {
            ensure_once(seen.steps, "steps", self.pos)?;
            let mut remainder = trimmed[5..].trim_start();
            if remainder.starts_with(':') {
                remainder = remainder[1..].trim_start();
            }
            if remainder.is_empty() {
                return Err(ParseError::Unexpected {
                    pos: self.pos,
                    message: "expected comparator for steps bound".into(),
                });
            }

            let value_str = if let Some(stripped) = remainder.strip_prefix("<=") {
                stripped.trim_start()
            } else if let Some(stripped) = remainder.strip_prefix('=') {
                stripped.trim_start()
            } else {
                return Err(ParseError::Unexpected {
                    pos: self.pos,
                    message: format!(
                        "unsupported steps comparator in `{trimmed}`; expected `steps <= N`"
                    ),
                });
            };

            if value_str.is_empty() {
                return Err(ParseError::Unexpected {
                    pos: self.pos,
                    message: "missing numeric value for steps bound".into(),
                });
            }

            let value_clean = value_str
                .trim()
                .trim_start_matches('{')
                .trim_end_matches('}')
                .trim_end_matches(';')
                .trim();

            let value: u32 = value_clean.parse().map_err(|_| ParseError::Unexpected {
                pos: self.pos,
                message: format!("invalid steps bound `{value_clean}`"),
            })?;

            guard.max_steps = Some(value);
            return Ok(());
        }

        if let Some((key, value)) = split_once(trimmed, '=') {
            let key_norm = key.trim().to_ascii_lowercase();
            match key_norm.as_str() {
                "labels" | "label" | "alphabet" => {
                    ensure_once(seen.labels, key.trim(), self.pos)?;
                    guard.labels.extend(parse_set(value));
                }
                "req_cur" | "require" | "require_current" => {
                    ensure_once(seen.req_cur, key.trim(), self.pos)?;
                    guard.current.required.extend(parse_set(value));
                }
                "forb_cur" | "forbid_current" => {
                    ensure_once(seen.forb_cur, key.trim(), self.pos)?;
                    guard.current.forbidden.extend(parse_set(value));
                }
                "req_next" | "require_next" => {
                    ensure_once(seen.req_next, key.trim(), self.pos)?;
                    guard.next.required.extend(parse_set(value));
                }
                "forb_next" | "forbid_next" => {
                    ensure_once(seen.forb_next, key.trim(), self.pos)?;
                    guard.next.forbidden.extend(parse_set(value));
                }
                "ctrl" | "control" | "controllability" => {
                    ensure_once(seen.ctrl, key.trim(), self.pos)?;
                    guard.control = parse_control(value, self.pos)?;
                }
                other => {
                    return Err(
                        self.error_here(ParseErrorKind::UnexpectedGuardKey(other.to_owned()))
                    );
                }
            }
        } else {
            guard.labels.extend(parse_set(trimmed));
        }
        Ok(())
    }

    fn read_component(&mut self, delimiters: &[char]) -> Result<String, ParseError> {
        let mut depth_brace = 0usize;
        let mut out = String::new();
        while let Some(ch) = self.peek_char() {
            if depth_brace == 0 && delimiters.contains(&ch) {
                break;
            }
            let ch = self.consume_char().unwrap();
            if ch == '{' {
                depth_brace += 1;
            } else if ch == '}' && depth_brace > 0 {
                depth_brace -= 1;
            }
            out.push(ch);
        }
        Ok(out.trim().to_owned())
    }

    fn read_until(&mut self, closing: char) -> Result<String, ParseError> {
        let mut out = String::new();
        while let Some(ch) = self.peek_char() {
            if ch == closing {
                break;
            }
            out.push(self.consume_char().unwrap());
        }
        Ok(out)
    }

    fn lookup_binder(&self, name: &str) -> Option<FormulaVarId> {
        self.binder_stack
            .iter()
            .rev()
            .find(|binder| binder.name == name)
            .map(|binder| binder.id)
    }

    fn parse_identifier(&mut self) -> Option<String> {
        self.skip_whitespace();
        let rest = &self.input[self.pos..];
        let first = rest.chars().next()?;
        if !is_ident_start(first) {
            return None;
        }
        let mut len = first.len_utf8();
        // Absorb identifier chars, plus a hierarchical separator `.` (`a.b.c`,
        // e.g. a flattened SV register `byte_controller.bit_controller.cnt`) —
        // but ONLY when the `.` is followed by another identifier char. This keeps
        // a fixpoint binder's `.` in `nu X.(…)` (a `.` before `(`/whitespace) for
        // the binder parser, so hierarchical atom names do not swallow the binder
        // separator. Bit-selects (`[N]`) are still not part of an identifier.
        loop {
            let tail = &rest[len..];
            let mut it = tail.chars();
            let Some(ch) = it.next() else { break };
            // A `.` joins the identifier only when followed by another identifier
            // char (`a.b.c`); a binder's `.` in `nu X.(…)` is left unconsumed.
            if is_ident_continue(ch) || (ch == '.' && it.next().is_some_and(is_ident_continue)) {
                len += ch.len_utf8();
            } else {
                break;
            }
        }
        let ident = rest[..len].to_owned();
        self.pos += len;
        Some(ident)
    }

    fn expect_char(&mut self, ch: char) -> Result<(), ParseError> {
        self.skip_whitespace();
        if self.try_consume_char(ch) {
            Ok(())
        } else {
            Err(self.error_here(ParseErrorKind::ExpectedChar(ch)))
        }
    }

    fn try_consume_char(&mut self, ch: char) -> bool {
        if self.peek_char() == Some(ch) {
            self.consume_char();
            true
        } else {
            false
        }
    }

    fn consume_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.consume_char();
            } else {
                break;
            }
        }
    }

    fn consume_keyword(&mut self, kw: &str) -> bool {
        if self.input[self.pos..].to_ascii_lowercase().starts_with(kw) {
            let next = self.pos + kw.len();
            if next < self.input.len()
                && let Some(ch) = self.input[next..].chars().next()
                && is_ident_continue(ch)
            {
                return false;
            }
            self.pos += kw.len();
            true
        } else {
            false
        }
    }

    fn try_consume_not(&mut self) -> bool {
        if self.try_consume_char('¬') {
            return true;
        }
        if self.try_consume_char('!') {
            return true;
        }
        if self.input[self.pos..]
            .to_ascii_lowercase()
            .starts_with("not")
        {
            let next = self.pos + 3;
            if next >= self.input.len()
                || self.input[next..]
                    .chars()
                    .next()
                    .is_none_or(|c| !is_ident_continue(c))
            {
                self.pos += 3;
                return true;
            }
        }
        false
    }

    fn try_consume_and(&mut self) -> bool {
        if self.try_consume_char('∧') {
            return true;
        }
        if self.input[self.pos..].starts_with("&&") {
            self.pos += 2;
            return true;
        }
        if self.try_consume_char('&') {
            return true;
        }
        if self.consume_keyword("and") {
            return true;
        }
        false
    }

    fn try_consume_or(&mut self) -> bool {
        if self.try_consume_char('∨') {
            return true;
        }
        if self.input[self.pos..].starts_with("||") {
            self.pos += 2;
            return true;
        }
        if self.try_consume_char('|') {
            return true;
        }
        if self.consume_keyword("or") {
            return true;
        }
        false
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn error_here(&self, kind: ParseErrorKind) -> ParseError {
        match kind {
            ParseErrorKind::Expected(message) => ParseError::Unexpected {
                pos: self.pos,
                message: format!("expected {message}"),
            },
            ParseErrorKind::ExpectedChar(ch) => ParseError::Unexpected {
                pos: self.pos,
                message: format!("expected `{ch}`"),
            },
            ParseErrorKind::UnexpectedGuardKey(key) => ParseError::Unexpected {
                pos: self.pos,
                message: format!(
                    "unexpected guard key `{key}` (expected one of: {})",
                    GUARD_KEYS.join(", ")
                ),
            },
            ParseErrorKind::UnexpectedTrailing => ParseError::Unexpected {
                pos: self.pos,
                message: "trailing characters after formula".into(),
            },
        }
    }
}

#[derive(Debug)]
enum ParseErrorKind {
    Expected(&'static str),
    ExpectedChar(char),
    UnexpectedGuardKey(String),
    UnexpectedTrailing,
}

struct GuardSeen<'a> {
    labels: &'a mut bool,
    req_cur: &'a mut bool,
    forb_cur: &'a mut bool,
    req_next: &'a mut bool,
    forb_next: &'a mut bool,
    ctrl: &'a mut bool,
    steps: &'a mut bool,
}

fn ensure_once(flag: &mut bool, field: &str, pos: usize) -> Result<(), ParseError> {
    if *flag {
        return Err(ParseError::DuplicateGuardField {
            field: field.to_owned(),
            pos,
        });
    }
    *flag = true;
    Ok(())
}

/// Parses a comma/whitespace-separated set literal into a list of strings.
///
/// The input may optionally be wrapped in `{}` braces (e.g. `{a, b}`); braces
/// and surrounding whitespace are stripped before splitting on commas.
///
/// # Parameters
///
/// * `raw` - Raw textual representation of the set, possibly including braces.
///
/// # Returns
///
/// A `Vec<String>` containing all non-empty symbols found in the set, in the
/// order they appear after trimming.
fn parse_set(raw: &str) -> Vec<String> {
    let mut value = raw.trim();
    if let Some(idx) = value.find('{') {
        value = &value[idx + 1..];
    }
    if let Some(idx) = value.rfind('}') {
        value = &value[..idx];
    }
    value
        .split(|ch: char| ch == ',' || ch.is_whitespace())
        .filter_map(|s| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        })
        .collect()
}

/// Parses a controllability specifier into a [`Control`] value.
///
/// Supported values (case-insensitive) are:
/// - `all`          → [`Control::All`]
/// - `controllable` → [`Control::Controllable`]
/// - `controller`   → [`Control::Controllable`]
///
/// # Parameters
///
/// * `raw` - Raw textual representation of the controllability token.
/// * `pos` - Byte offset used for error reporting when the value is invalid.
///
/// # Returns
///
/// A [`Control`] variant corresponding to the parsed value.
///
/// # Errors
///
/// Returns [`ParseError::Unexpected`] when `raw` does not match a known
/// controllability keyword.
fn parse_control(raw: &str, pos: usize) -> Result<Control, ParseError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "all" => Ok(Control::All),
        "controllable" | "controller" => Ok(Control::Controllable),
        "environment" | "env" => Ok(Control::Environment),
        other => Err(ParseError::Unexpected {
            pos,
            message: format!(
                "unknown controllability `{other}` (expected one of: {})",
                CONTROL_WORDS.join(", ")
            ),
        }),
    }
}

/// The words [`parse_control`] accepts — what an unknown one is told (mununu#1121). Kept
/// beside the match it mirrors.
const CONTROL_WORDS: &[&str] = &["all", "controllable", "controller", "environment", "env"];

/// The guard keys `apply_guard_component` accepts (`steps` is matched by prefix, the rest by
/// `key = value`) — what an unexpected key is told (mununu#1121). Kept beside the match.
const GUARD_KEYS: &[&str] = &[
    "labels",
    "label",
    "alphabet",
    "req_cur",
    "require",
    "require_current",
    "forb_cur",
    "forbid_current",
    "req_next",
    "require_next",
    "forb_next",
    "forbid_next",
    "ctrl",
    "control",
    "controllability",
    "steps",
];

fn split_once(input: &str, delimiter: char) -> Option<(&str, &str)> {
    let idx = input.find(delimiter)?;
    let (lhs, rhs) = input.split_at(idx);
    Some((lhs, &rhs[delimiter.len_utf8()..]))
}

fn is_ident_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_' || ch == '$'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mu_calculus::{Control, Guard, ModalKind, Node, VariableGuard};

    /// mununu#1121 — an unknown controllability word is told the words the guard accepts
    /// (`docs/policies/failure-paths.md` Rule 5: an "unknown X" lists the candidates).
    #[test]
    fn x1121_an_unknown_controllability_word_lists_the_accepted_ones() {
        let err = parse("[ ( ctrl = bogus ) ] p").expect_err("`bogus` is not a controllability");
        let text = err.to_string();
        assert!(text.contains("unknown controllability `bogus`"), "{text}");
        for word in ["all", "controllable", "environment"] {
            assert!(text.contains(word), "{text} does not name `{word}`");
        }
        let err = parse("[ ( colour = {red} ) ] p").expect_err("`colour` is not a guard key");
        let text = err.to_string();
        assert!(text.contains("unexpected guard key `colour`"), "{text}");
        for key in ["labels", "req_next", "ctrl", "steps"] {
            assert!(text.contains(key), "{text} does not name `{key}`");
        }
    }

    #[test]
    fn fixpoint_binder_extends_right_through_conjunction() {
        // Verifies that `nu X. φ and [] X` binds X across the trailing
        // `[] X` (textbook right-extending convention). Each formula is the
        // canonical liveness-implication body — the `[] X` references the
        // outer nu's bound variable; if the parser bound it as a free
        // predicate, model checking would silently miscompile.
        let formulas = [
            "nu X. ((not scoring_request_received) or (mu Y. (scoring_request_handled or [] Y))) and [] X",
            "nu X. ((not score_received_) or (mu Y. (report_delay or [] Y))) and [] X",
            "nu X. ((not Fulfill_order_exit) or (mu Y. (Order_completed or [] Y))) and [] X",
        ];

        for formula_text in formulas {
            let formula = parse(formula_text).unwrap_or_else(|e| {
                panic!("formula failed to parse: {e:?}\n  text: {formula_text}")
            });
            let Node::Nu { var: nu_var, body } = formula.node(formula.root()) else {
                panic!(
                    "expected outer nu, got {:?}\n  text: {formula_text}",
                    formula.node(formula.root())
                );
            };
            let Node::And(_, rhs) = formula.node(*body) else {
                panic!("expected `... and [] X` at nu body\n  text: {formula_text}");
            };
            let Node::Modal { target, .. } = formula.node(*rhs) else {
                panic!("expected `[] X` modal at rhs\n  text: {formula_text}");
            };
            match formula.node(*target) {
                Node::Variable(var) => assert_eq!(
                    var, nu_var,
                    "`[] X` must bind to outer nu X\n  text: {formula_text}"
                ),
                other => panic!(
                    "`[] X` resolved to {other:?} instead of bound Variable\n  text: {formula_text}"
                ),
            }
        }
    }

    #[test]
    fn hierarchical_dotted_atom_names_parse() {
        // A flattened SV register `a.b.c` is ONE atom (shadow-register grounding on
        // an internal register), while the fixpoint binder's `.` in `nu X.(…)` is
        // NOT absorbed into the variable.
        let f = parse("mu Z. (byte_controller.bit_controller.cnt == 0 || <> Z)")
            .expect("dotted atom parses");
        assert!(
            f.nodes().iter().any(|n| matches!(
                n, Node::Predicate(p) if p == "byte_controller.bit_controller.cnt == 0"
            )),
            "hierarchical atom kept whole: {:?}",
            f.nodes()
        );
        // `reg == reg` with dotted operands on BOTH sides (relational atom).
        parse("nu Y. ((a.b == c.d) && [] Y)").expect("dotted reg==reg parses");
        // The binder variable is X (bound), not swallowed into a dotted atom — the
        // `.` before `(` stays the binder separator.
        let b = parse("nu X.(<> true && [] X)").expect("binder still parses");
        assert!(
            b.nodes().iter().any(|n| matches!(n, Node::Variable(_))),
            "X binds as a variable, the binder `.` is not absorbed"
        );
    }

    fn predicate_name(formula: &Formula, id: NodeId) -> &str {
        match formula.node(id) {
            Node::Predicate(name) => name,
            other => panic!("expected predicate node, got {other:?}"),
        }
    }

    #[test]
    fn parses_simple_predicate() {
        let formula = parse("p").unwrap();
        assert!(matches!(formula.node(formula.root()), Node::Predicate(_)));
        assert_eq!(predicate_name(&formula, formula.root()), "p");
    }

    #[test]
    fn parses_arithmetic_addend_predicate() {
        // H.G — `cnt_q == cnt_q__past + 1` must parse as ONE predicate atom
        // carrying the arithmetic addend (previously the `+ 1` was left dangling,
        // so `[] (cnt_q == cnt_q__past + 1)` failed with "expected `)`").
        let formula = parse("cnt_q == cnt_q__past + 1").unwrap();
        assert_eq!(
            predicate_name(&formula, formula.root()),
            "cnt_q == cnt_q__past + 1"
        );
        // Inside a box + fixpoint (sva_15's exact shape) it parses end-to-end.
        parse("nu X. (((!((cnt_en && (!(cnt_clr)))) || [] (cnt_q == cnt_q__past + 1))) && [] X)")
            .expect("sva_15-shaped formula parses");
        // A literal RHS keeps its own `+`-free form; a `+` after a literal is NOT
        // consumed as an addend (only after an identifier RHS).
        let lit = parse("cnt_q == 3").unwrap();
        assert_eq!(predicate_name(&lit, lit.root()), "cnt_q == 3");
    }

    #[test]
    fn parses_box_with_guard_components() {
        let formula = parse("[ ( sync, req_cur = {flag}, ctrl = controllable ) ] q").unwrap();
        match formula.node(formula.root()) {
            Node::Modal {
                kind,
                guard,
                target,
            } => {
                assert_eq!(*kind, ModalKind::Box);
                assert_eq!(
                    guard,
                    &Guard {
                        labels: vec!["sync".into()],
                        current: VariableGuard {
                            required: vec!["flag".into()],
                            forbidden: Vec::new(),
                        },
                        next: VariableGuard::default(),
                        control: Control::Controllable,
                        max_steps: None,
                    }
                );
                assert_eq!(predicate_name(&formula, *target), "q");
            }
            other => panic!("expected modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_fixpoint_with_variable_reference() {
        let formula = parse("mu X. (X ∨ p)").unwrap();
        match formula.node(formula.root()) {
            Node::Mu { var, body } => {
                assert_eq!(formula.var(*var).name, "X");
                match formula.node(*body) {
                    Node::Or(left, right) => {
                        assert!(matches!(formula.node(*left), Node::Variable(_)));
                        assert!(matches!(formula.node(*right), Node::Predicate(_)));
                    }
                    other => panic!("expected disjunction, got {other:?}"),
                }
            }
            other => panic!("expected mu node, got {other:?}"),
        }
    }

    #[test]
    fn parses_diamond_with_label_list() {
        let formula = parse("< labels = {tick, ack} > r").unwrap();
        match formula.node(formula.root()) {
            Node::Modal {
                kind,
                guard,
                target,
            } => {
                assert_eq!(*kind, ModalKind::Diamond);
                assert_eq!(
                    guard.labels,
                    vec![String::from("tick"), String::from("ack")]
                );
                assert_eq!(predicate_name(&formula, *target), "r");
            }
            other => panic!("expected modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_modal_with_step_bound() {
        let formula = parse("< ( steps <= 3 ) > goal").unwrap();
        match formula.node(formula.root()) {
            Node::Modal {
                kind,
                guard,
                target,
            } => {
                assert_eq!(*kind, ModalKind::Diamond);
                assert_eq!(guard.max_steps, Some(3));
                assert!(guard.labels.is_empty());
                assert_eq!(predicate_name(&formula, *target), "goal");
            }
            other => panic!("expected modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_true_constant() {
        // Test true constant parsing (line 181-182)
        let formula = parse("true").unwrap();
        assert!(matches!(formula.node(formula.root()), Node::True));
    }

    #[test]
    fn parses_false_constant() {
        // Test false constant parsing (line 184-185)
        let formula = parse("false").unwrap();
        assert!(matches!(formula.node(formula.root()), Node::False));
    }

    #[test]
    fn parses_parenthesized_expression() {
        // Test parenthesized expression parsing (lines 174-178)
        let formula = parse("(p)").unwrap();
        assert_eq!(predicate_name(&formula, formula.root()), "p");
    }

    #[test]
    fn parses_not_operator() {
        // Test not operator parsing (lines 88-90)
        let formula = parse("not p").unwrap();
        match formula.node(formula.root()) {
            Node::Not(inner) => {
                assert_eq!(predicate_name(&formula, *inner), "p");
            }
            other => panic!("expected Not node, got {other:?}"),
        }
    }

    #[test]
    fn parses_conjunction_with_and_keyword() {
        // Test conjunction with "and" keyword (line 517-518)
        let formula = parse("p and q").unwrap();
        match formula.node(formula.root()) {
            Node::And(left, right) => {
                assert_eq!(predicate_name(&formula, *left), "p");
                assert_eq!(predicate_name(&formula, *right), "q");
            }
            other => panic!("expected And node, got {other:?}"),
        }
    }

    #[test]
    fn parses_conjunction_with_ampersand() {
        // Test conjunction with & operator (line 514-515)
        let formula = parse("p & q").unwrap();
        match formula.node(formula.root()) {
            Node::And(..) => {}
            other => panic!("expected And node, got {other:?}"),
        }
    }

    #[test]
    fn parses_conjunction_with_double_ampersand() {
        // Test conjunction with && operator (line 510-512)
        let formula = parse("p && q").unwrap();
        match formula.node(formula.root()) {
            Node::And(..) => {}
            other => panic!("expected And node, got {other:?}"),
        }
    }

    #[test]
    fn parses_conjunction_with_wedge() {
        // Test conjunction with ∧ operator (line 507-508)
        let formula = parse("p ∧ q").unwrap();
        match formula.node(formula.root()) {
            Node::And(..) => {}
            other => panic!("expected And node, got {other:?}"),
        }
    }

    #[test]
    fn parses_disjunction_with_or_keyword() {
        // Test disjunction with "or" keyword (line 534-535)
        let formula = parse("p or q").unwrap();
        match formula.node(formula.root()) {
            Node::Or(left, right) => {
                assert_eq!(predicate_name(&formula, *left), "p");
                assert_eq!(predicate_name(&formula, *right), "q");
            }
            other => panic!("expected Or node, got {other:?}"),
        }
    }

    #[test]
    fn parses_disjunction_with_pipe() {
        // Test disjunction with | operator (line 531-532)
        let formula = parse("p | q").unwrap();
        match formula.node(formula.root()) {
            Node::Or(..) => {}
            other => panic!("expected Or node, got {other:?}"),
        }
    }

    #[test]
    fn parses_disjunction_with_double_pipe() {
        // Test disjunction with || operator (line 527-529)
        let formula = parse("p || q").unwrap();
        match formula.node(formula.root()) {
            Node::Or(..) => {}
            other => panic!("expected Or node, got {other:?}"),
        }
    }

    #[test]
    fn parses_disjunction_with_vee() {
        // Test disjunction with ∨ operator (line 524-525)
        let formula = parse("p ∨ q").unwrap();
        match formula.node(formula.root()) {
            Node::Or(..) => {}
            other => panic!("expected Or node, got {other:?}"),
        }
    }

    #[test]
    fn parses_nu_fixpoint() {
        // Test nu fixpoint parsing (lines 120-132)
        let formula = parse("nu X. (X and p)").unwrap();
        match formula.node(formula.root()) {
            Node::Nu { var, body } => {
                // Body should be an And node
                match formula.node(*body) {
                    Node::And(..) => {}
                    other => panic!("expected And in nu body, got {other:?}"),
                }
                // Variable should be bound
                assert!(formula.var(*var).name == "X");
            }
            other => panic!("expected Nu node, got {other:?}"),
        }
    }

    #[test]
    fn parses_mu_with_unicode() {
        // Test mu fixpoint with unicode μ (line 106)
        let formula = parse("μ X. (X or p)").unwrap();
        match formula.node(formula.root()) {
            Node::Mu { .. } => {}
            other => panic!("expected Mu node, got {other:?}"),
        }
    }

    #[test]
    fn parses_nu_with_unicode() {
        // Test nu fixpoint with unicode ν (line 120)
        let formula = parse("ν X. (X and p)").unwrap();
        match formula.node(formula.root()) {
            Node::Nu { .. } => {}
            other => panic!("expected Nu node, got {other:?}"),
        }
    }

    #[test]
    fn parses_box_modal() {
        // Test box modal operator with [] (lines 139-147)
        let formula = parse("[ ] p").unwrap();
        match formula.node(formula.root()) {
            Node::Modal { kind, .. } => {
                assert_eq!(*kind, ModalKind::Box);
            }
            other => panic!("expected Modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_diamond_modal_with_angle_brackets() {
        // Test diamond modal with <> (lines 159-167)
        let formula = parse("< > p").unwrap();
        match formula.node(formula.root()) {
            Node::Modal { kind, .. } => {
                assert_eq!(*kind, ModalKind::Diamond);
            }
            other => panic!("expected Modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_diamond_modal_with_unicode() {
        // Test diamond modal with ⟨⟩ (lines 149-157)
        let formula = parse("⟨ ⟩ p").unwrap();
        match formula.node(formula.root()) {
            Node::Modal { kind, .. } => {
                assert_eq!(*kind, ModalKind::Diamond);
            }
            other => panic!("expected Modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_empty_guard() {
        // Test empty guard parsing (lines 200-202)
        let formula = parse("[ ] p").unwrap();
        match formula.node(formula.root()) {
            Node::Modal { guard, .. } => {
                assert!(guard.labels.is_empty());
                assert_eq!(guard.max_steps, None);
            }
            other => panic!("expected Modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_guard_with_steps_equals() {
        // Test guard with steps = (line 297-298)
        let formula = parse("< ( steps = 5 ) > p").unwrap();
        match formula.node(formula.root()) {
            Node::Modal { guard, .. } => {
                assert_eq!(guard.max_steps, Some(5));
            }
            other => panic!("expected Modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_complex_nested_formula() {
        // Test complex nested formula parsing
        let formula = parse("(p and (q or r)) and not false").unwrap();
        match formula.node(formula.root()) {
            Node::And(..) => {}
            other => panic!("expected And node, got {other:?}"),
        }
    }

    #[test]
    fn rejects_trailing_characters() {
        // Test error for trailing characters (lines 10-11)
        let result = parse("p extra");
        assert!(result.is_err());
        match result {
            Err(ParseError::Unexpected { message, .. }) => {
                assert!(message.contains("trailing"));
            }
            _ => panic!("expected Unexpected error with trailing message"),
        }
    }

    #[test]
    fn rejects_unexpected_eof() {
        // Test error for unexpected EOF (line 246)
        let result = parse("p and");
        assert!(result.is_err());
        // The error might be UnexpectedEof or Unexpected depending on where it occurs
        match result {
            Err(ParseError::UnexpectedEof | ParseError::Unexpected { .. }) => {}
            _ => panic!("expected UnexpectedEof or Unexpected error"),
        }
    }

    #[test]
    fn rejects_missing_closing_paren() {
        // Test error for missing closing parenthesis
        let result = parse("(p and q");
        assert!(result.is_err());
    }

    #[test]
    fn parses_guard_with_control_controllable() {
        // Test guard with control = controllable
        let formula = parse("[ ( ctrl = controllable ) ] p").unwrap();
        match formula.node(formula.root()) {
            Node::Modal { guard, .. } => {
                assert_eq!(guard.control, Control::Controllable);
            }
            other => panic!("expected Modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_guard_with_control_all() {
        // Test guard with control = all (Control enum only has All and Controllable)
        let formula = parse("[ ( ctrl = all ) ] p").unwrap();
        match formula.node(formula.root()) {
            Node::Modal { guard, .. } => {
                assert_eq!(guard.control, Control::All);
            }
            other => panic!("expected Modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_guard_with_req_cur() {
        // Test guard with req_cur
        let formula = parse("[ ( req_cur = {x, y} ) ] p").unwrap();
        match formula.node(formula.root()) {
            Node::Modal { guard, .. } => {
                assert_eq!(guard.current.required.len(), 2);
                assert!(guard.current.required.contains(&"x".to_string()));
                assert!(guard.current.required.contains(&"y".to_string()));
            }
            other => panic!("expected Modal node, got {other:?}"),
        }
    }

    #[test]
    fn parses_guard_with_forb_cur() {
        // Test guard with forb_cur
        let formula = parse("[ ( forb_cur = {z} ) ] p").unwrap();
        match formula.node(formula.root()) {
            Node::Modal { guard, .. } => {
                assert_eq!(guard.current.forbidden.len(), 1);
                assert!(guard.current.forbidden.contains(&"z".to_string()));
            }
            other => panic!("expected Modal node, got {other:?}"),
        }
    }
}
