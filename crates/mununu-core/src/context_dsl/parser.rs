use crate::context_dsl::ast::{FormulaExpr, LtlExpr, *};
use crate::context_dsl::canonicalize;
use crate::context_dsl::error::ParseError;
use crate::context_dsl::lexer::lex;
use crate::context_dsl::token::{Keyword, Span, Symbol, Token, TokenKind};
use crate::ltl;
use std::convert::TryFrom;

/// Parses a context DSL source string into an AST, applying canonical ordering afterwards.
pub fn parse(source: &str) -> Result<ContextDoc, ParseError> {
    let tokens = lex(source)?;
    let mut parser = Parser::new(source, tokens);
    let mut doc = parser.parse_context()?;
    canonicalize::canonicalize(&mut doc);
    Ok(doc)
}

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, tokens: Vec<Token>) -> Self {
        Self {
            source,
            tokens,
            pos: 0,
        }
    }

    /// Parses a context document with all section types.
    ///
    /// # Coverage Status
    /// Covered by test: `parses_all_context_sections`
    fn parse_context(&mut self) -> Result<ContextDoc, ParseError> {
        self.expect_keyword(Keyword::Context)?;
        let name = self.expect_ident()?;
        let start_span = name.span;
        self.expect_symbol(Symbol::LBrace)?;

        let mut alphabet = Vec::new();
        let mut constants = Vec::new();
        let mut ranges = Vec::new();
        let mut enums = Vec::new();
        let mut automata = Vec::new();
        let mut compositions = Vec::new();
        let mut controllers = Vec::new();
        let mut mu_formulas = Vec::new();

        while !self.check_symbol(Symbol::RBrace) {
            match self.peek_kind() {
                TokenKind::Keyword(Keyword::Alphabet) => {
                    alphabet.extend(self.parse_alphabet_section()?);
                }
                TokenKind::Keyword(Keyword::Constants) => {
                    constants.extend(self.parse_constants_section()?);
                }
                TokenKind::Keyword(Keyword::Ranges) => {
                    ranges.extend(self.parse_ranges_section()?);
                }
                TokenKind::Keyword(Keyword::Enums) => {
                    enums.extend(self.parse_enums_section()?);
                }
                TokenKind::Keyword(Keyword::Automata) => {
                    automata.extend(self.parse_automata_section()?);
                }
                TokenKind::Keyword(Keyword::Composition) => {
                    compositions.extend(self.parse_composition_section()?);
                }
                TokenKind::Keyword(Keyword::Controllers) => {
                    controllers.extend(self.parse_controllers_section()?);
                }
                TokenKind::Keyword(Keyword::MuFormulas) => {
                    mu_formulas.extend(self.parse_mu_formulas_section()?);
                }
                other => {
                    // Coverage Status: Covered by test `rejects_unexpected_section_keyword`
                    let span = self.peek().span;
                    return Err(ParseError::UnexpectedToken {
                        found: other.clone(),
                        expected: "section keyword",
                        span,
                    });
                }
            }
        }

        let closing = self.expect_symbol(Symbol::RBrace)?;
        let span = join_span(&start_span, &closing.span);

        Ok(ContextDoc {
            name,
            alphabet,
            constants,
            ranges,
            enums,
            automata,
            compositions,
            controllers,
            mu_formulas,
            span,
            state_valuations: Default::default(),
            transition_observations: Default::default(),
        })
    }

    fn parse_alphabet_section(&mut self) -> Result<Vec<AlphabetEntry>, ParseError> {
        self.expect_keyword(Keyword::Alphabet)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut entries = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            self.expect_keyword(Keyword::Label)?;
            let name = self.expect_ident()?;
            let display = if self.match_symbol(Symbol::Assign) {
                Some(self.expect_string()?.0)
            } else {
                None
            };
            self.expect_symbol(Symbol::Semicolon)?;
            entries.push(AlphabetEntry { name, display });
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(entries)
    }

    fn parse_constants_section(&mut self) -> Result<Vec<ConstantEntry>, ParseError> {
        self.expect_keyword(Keyword::Constants)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut entries = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            self.expect_keyword(Keyword::Const)?;
            let name = self.expect_ident()?;
            self.expect_symbol(Symbol::Assign)?;
            let (value, span) = self.expect_integer()?;
            self.expect_symbol(Symbol::Semicolon)?;
            if entries
                .iter()
                .any(|entry: &ConstantEntry| entry.name.name == name.name)
            {
                return Err(ParseError::DuplicateItem {
                    name: name.name,
                    span,
                });
            }
            entries.push(ConstantEntry { name, value });
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(entries)
    }

    /// Parses a ranges section with range definitions.
    ///
    /// # Coverage Status
    /// Covered by test: `parses_ranges_section`
    fn parse_ranges_section(&mut self) -> Result<Vec<RangeEntry>, ParseError> {
        self.expect_keyword(Keyword::Ranges)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut entries = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            self.expect_keyword(Keyword::Range)?;
            let name = self.expect_ident()?;
            self.expect_symbol(Symbol::Assign)?;
            let lower = self.parse_expr()?;
            self.expect_symbol(Symbol::RangeInclusive)?;
            let upper = self.parse_expr()?;
            self.expect_symbol(Symbol::Semicolon)?;
            entries.push(RangeEntry { name, lower, upper });
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(entries)
    }

    fn parse_enums_section(&mut self) -> Result<Vec<EnumDecl>, ParseError> {
        self.expect_keyword(Keyword::Enums)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut entries = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            entries.push(self.parse_enum_entry()?);
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(entries)
    }

    fn parse_enum_entry(&mut self) -> Result<EnumDecl, ParseError> {
        self.expect_keyword(Keyword::Enum)?;
        let name = self.expect_ident()?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut variants = Vec::new();
        if !self.check_symbol(Symbol::RBrace) {
            variants.push(self.expect_ident()?);
            while self.match_symbol(Symbol::Comma) {
                if self.check_symbol(Symbol::RBrace) {
                    break; // trailing comma
                }
                variants.push(self.expect_ident()?);
            }
        }
        self.expect_symbol(Symbol::RBrace)?;
        self.expect_symbol(Symbol::Semicolon)?;
        Ok(EnumDecl { name, variants })
    }

    fn parse_automata_section(&mut self) -> Result<Vec<Automaton>, ParseError> {
        self.expect_keyword(Keyword::Automata)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut automata = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            automata.push(self.parse_automaton()?);
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(automata)
    }

    fn parse_composition_section(&mut self) -> Result<Vec<Composition>, ParseError> {
        self.expect_keyword(Keyword::Composition)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut compositions = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            compositions.push(self.parse_composition_entry()?);
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(compositions)
    }

    fn parse_composition_entry(&mut self) -> Result<Composition, ParseError> {
        let (kind, start_span) = self.expect_composition_kind()?;
        let name = self.expect_ident()?;
        self.expect_symbol(Symbol::LBrace)?;

        let mut meta = Meta::default();
        if self.check_keyword(Keyword::Meta) {
            meta = self.parse_meta_block()?;
        }

        self.expect_keyword(Keyword::Members)?;
        let members = self.parse_members_clause()?;
        let end = self.expect_symbol(Symbol::RBrace)?.span;
        let span = join_span(&start_span, &end);

        Ok(Composition {
            name,
            meta,
            kind,
            members,
            span,
        })
    }

    fn expect_composition_kind(&mut self) -> Result<(CompositionKind, Span), ParseError> {
        let token = self.advance().clone();
        match token.kind {
            TokenKind::Keyword(Keyword::Synchronous) => {
                Ok((CompositionKind::Synchronous, token.span))
            }
            TokenKind::Keyword(Keyword::Asynchronous) => {
                Ok((CompositionKind::Asynchronous, token.span))
            }
            TokenKind::Keyword(Keyword::Superset) => Ok((CompositionKind::Superset, token.span)),
            other => Err(ParseError::UnexpectedToken {
                found: other,
                expected: "composition kind",
                span: token.span,
            }),
        }
    }

    fn parse_members_clause(&mut self) -> Result<Vec<MemberRef>, ParseError> {
        self.expect_symbol(Symbol::LBracket)?;
        let mut members = Vec::new();
        if !self.check_symbol(Symbol::RBracket) {
            members.push(self.parse_member_ref()?);
            while self.match_symbol(Symbol::Comma) {
                members.push(self.parse_member_ref()?);
            }
        }
        self.expect_symbol(Symbol::RBracket)?;
        self.expect_symbol(Symbol::Semicolon)?;
        Ok(members)
    }

    fn parse_member_ref(&mut self) -> Result<MemberRef, ParseError> {
        let name = self.expect_ident()?;
        let index = if self.match_symbol(Symbol::LBracket) {
            let expr = self.parse_expr()?;
            self.expect_symbol(Symbol::RBracket)?;
            Some(expr)
        } else {
            None
        };
        Ok(MemberRef { name, index })
    }

    fn parse_controllers_section(&mut self) -> Result<Vec<Controller>, ParseError> {
        self.expect_keyword(Keyword::Controllers)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut controllers = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            controllers.push(self.parse_controller_entry()?);
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(controllers)
    }

    fn parse_controller_entry(&mut self) -> Result<Controller, ParseError> {
        self.expect_keyword(Keyword::Controller)?;
        let name = self.expect_ident()?;
        self.expect_symbol(Symbol::LBrace)?;

        let mut meta = Meta::default();
        if self.check_keyword(Keyword::Meta) {
            meta = self.parse_meta_block()?;
        }

        self.expect_keyword(Keyword::Source)?;
        let source = self.expect_ident()?;
        self.expect_symbol(Symbol::Semicolon)?;

        self.expect_keyword(Keyword::Satisfying)?;
        let formula = self.expect_ident()?;
        self.expect_symbol(Symbol::Semicolon)?;

        let mut export = None;
        let mut options = ControllerOptions::default();

        while !self.check_symbol(Symbol::RBrace) {
            match self.peek_kind() {
                TokenKind::Keyword(Keyword::Export) => {
                    self.expect_keyword(Keyword::Export)?;
                    let (path, _) = self.expect_string()?;
                    self.expect_symbol(Symbol::Semicolon)?;
                    export = Some(path);
                }
                TokenKind::Keyword(Keyword::Minimize) => {
                    self.expect_keyword(Keyword::Minimize)?;
                    self.expect_symbol(Symbol::Assign)?;
                    let value = self.parse_bool_literal()?;
                    self.expect_symbol(Symbol::Semicolon)?;
                    options.minimize = Some(value);
                }
                TokenKind::Keyword(Keyword::Diagnostics) => {
                    let diagnostics = self.parse_controller_diagnostics_block()?;
                    options.diagnostics = Some(diagnostics);
                }
                other => {
                    return Err(ParseError::UnexpectedToken {
                        found: other.clone(),
                        expected: "export, minimize, or diagnostics",
                        span: self.peek().span,
                    });
                }
            }
        }

        let end = self.expect_symbol(Symbol::RBrace)?.span;
        let span = join_span(&name.span, &end);

        Ok(Controller {
            name,
            meta,
            source,
            formula,
            export,
            options,
            span,
        })
    }

    fn parse_automaton(&mut self) -> Result<Automaton, ParseError> {
        self.expect_keyword(Keyword::Automaton)?;
        let name = self.expect_ident()?;
        self.expect_symbol(Symbol::LBrace)?;

        let mut meta = Meta::default();
        let mut parameters = Vec::new();
        let mut alphabet = Vec::new();
        let mut controllable = Vec::new();
        let mut internal = Vec::new();
        let mut controllable_declared = false;
        let mut internal_declared = false;
        let mut variables = Vec::new();
        let mut state_groups = Vec::new();
        let mut predicates = Vec::new();
        let mut states = Vec::new();
        let mut transitions = Vec::new();

        while !self.check_symbol(Symbol::RBrace) {
            match self.peek_kind() {
                TokenKind::Keyword(Keyword::Meta) => {
                    meta = self.parse_meta_block()?;
                }
                TokenKind::Keyword(Keyword::Parameters) => {
                    parameters = self.parse_parameters_block()?;
                }
                TokenKind::Keyword(Keyword::Alphabet) => {
                    alphabet = self.parse_label_block(Keyword::Alphabet)?;
                }
                TokenKind::Keyword(Keyword::Controllable) => {
                    controllable = self.parse_label_block(Keyword::Controllable)?;
                    controllable_declared = true;
                }
                TokenKind::Keyword(Keyword::Internal) => {
                    internal = self.parse_label_block(Keyword::Internal)?;
                    internal_declared = true;
                }
                TokenKind::Keyword(Keyword::Variables) => {
                    variables = self.parse_variables_block()?;
                }
                TokenKind::Keyword(Keyword::StateGroups) => {
                    state_groups = self.parse_state_groups_block()?;
                }
                TokenKind::Keyword(Keyword::States) => {
                    states = self.parse_states_block()?;
                }
                TokenKind::Keyword(Keyword::Transitions) => {
                    transitions = self.parse_transitions_block()?;
                }
                TokenKind::Keyword(Keyword::Predicates) => {
                    predicates = self.parse_predicates_block()?;
                }
                other => {
                    let span = self.peek().span;
                    return Err(ParseError::UnexpectedToken {
                        found: other.clone(),
                        expected: "automaton block keyword",
                        span,
                    });
                }
            }
        }

        self.expect_symbol(Symbol::RBrace)?;

        Ok(Automaton {
            name,
            meta,
            parameters,
            alphabet,
            controllable,
            internal,
            controllable_declared,
            internal_declared,
            variables,
            state_groups,
            states,
            transitions,
            predicates,
        })
    }

    fn parse_meta_block(&mut self) -> Result<Meta, ParseError> {
        self.expect_keyword(Keyword::Meta)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut meta = Meta::default();
        while !self.check_symbol(Symbol::RBrace) {
            match self.peek_kind() {
                TokenKind::Keyword(Keyword::Id) => {
                    self.expect_keyword(Keyword::Id)?;
                    self.expect_symbol(Symbol::Assign)?;
                    let (value, _) = self.expect_string()?;
                    self.expect_symbol(Symbol::Semicolon)?;
                    meta.id = Some(value);
                }
                TokenKind::Keyword(Keyword::Comment) => {
                    self.expect_keyword(Keyword::Comment)?;
                    self.expect_symbol(Symbol::Assign)?;
                    let (value, _) = self.expect_string()?;
                    self.expect_symbol(Symbol::Semicolon)?;
                    meta.comment = Some(value);
                }
                other => {
                    let span = self.peek().span;
                    return Err(ParseError::UnexpectedToken {
                        found: other.clone(),
                        expected: "meta field",
                        span,
                    });
                }
            }
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(meta)
    }

    fn parse_predicates_block(&mut self) -> Result<Vec<PredicateDecl>, ParseError> {
        self.expect_keyword(Keyword::Predicates)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut decls = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            self.expect_keyword(Keyword::Predicate)?;
            let name = self.expect_ident()?;
            self.expect_symbol(Symbol::Assign)?;
            self.expect_keyword(Keyword::State)?;
            let state = self.parse_state_ref()?;
            self.expect_symbol(Symbol::Semicolon)?;
            decls.push(PredicateDecl {
                name,
                target: PredicateTarget::State(state),
            });
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(decls)
    }

    fn parse_parameters_block(&mut self) -> Result<Vec<Parameter>, ParseError> {
        self.expect_keyword(Keyword::Parameters)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut params = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            self.expect_keyword(Keyword::Param)?;
            let name = self.expect_ident()?;
            self.expect_keyword(Keyword::In)?;

            let spec = if matches!(self.peek_kind(), TokenKind::Identifier(_)) {
                // capture to decide if this is a named range or expression bounds
                let saved_pos = self.pos;
                let ident = self.expect_ident()?;
                if self.match_symbol(Symbol::RangeInclusive) {
                    // revert to before ident to parse as expression range
                    self.pos = saved_pos;
                    let lower = self.parse_expr()?;
                    self.expect_symbol(Symbol::RangeInclusive)?;
                    let upper = self.parse_expr()?;
                    RangeSpec::Bounds { lower, upper }
                } else {
                    RangeSpec::Named(ident)
                }
            } else {
                let lower = self.parse_expr()?;
                self.expect_symbol(Symbol::RangeInclusive)?;
                let upper = self.parse_expr()?;
                RangeSpec::Bounds { lower, upper }
            };

            self.expect_symbol(Symbol::Semicolon)?;
            params.push(Parameter { name, spec });
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(params)
    }

    fn parse_label_block(&mut self, kind: Keyword) -> Result<Vec<AlphabetRef>, ParseError> {
        self.expect_keyword(kind)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut refs = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            self.expect_keyword(Keyword::Label)?;
            let name = self.expect_ident()?;
            let index = if self.match_symbol(Symbol::LBracket) {
                let expr = self.parse_expr()?;
                self.expect_symbol(Symbol::RBracket)?;
                Some(expr)
            } else {
                None
            };
            self.expect_symbol(Symbol::Semicolon)?;
            refs.push(AlphabetRef { name, index });
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(refs)
    }

    fn parse_variables_block(&mut self) -> Result<Vec<VariableDecl>, ParseError> {
        self.expect_keyword(Keyword::Variables)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut vars = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            self.expect_keyword(Keyword::Var)?;
            let name = self.expect_ident()?;
            let index = if self.match_symbol(Symbol::LBracket) {
                let expr = self.parse_expr()?;
                self.expect_symbol(Symbol::RBracket)?;
                Some(expr)
            } else {
                None
            };
            self.expect_symbol(Symbol::Colon)?;
            let ty = self.parse_type_name()?;
            self.expect_symbol(Symbol::Assign)?;
            let init = self.parse_expr()?;
            self.expect_symbol(Symbol::Semicolon)?;
            vars.push(VariableDecl {
                name,
                index,
                ty,
                init,
            });
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(vars)
    }

    /// Parses a type name (bool or i64).
    ///
    /// # Coverage Status
    /// Covered by tests: `parses_type_names`, `rejects_invalid_type_name`
    fn parse_type_name(&mut self) -> Result<TypeName, ParseError> {
        match self.advance().kind.clone() {
            TokenKind::Keyword(Keyword::Bool) => Ok(TypeName::Bool),
            TokenKind::Keyword(Keyword::I64) => Ok(TypeName::I64),
            TokenKind::Identifier(name) => Ok(TypeName::Enum(name)),
            kind => Err(ParseError::UnexpectedToken {
                found: kind,
                expected: "type name (bool, i64, or enum name)",
                span: self.previous_span(),
            }),
        }
    }

    fn parse_states_block(&mut self) -> Result<Vec<StateDecl>, ParseError> {
        self.expect_keyword(Keyword::States)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut states = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            states.push(self.parse_state_decl()?);
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(states)
    }

    fn parse_state_groups_block(&mut self) -> Result<Vec<StateGroup>, ParseError> {
        self.expect_keyword(Keyword::StateGroups)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut groups = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            groups.push(self.parse_state_group()?);
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(groups)
    }

    fn parse_state_group(&mut self) -> Result<StateGroup, ParseError> {
        let start = self.peek().span;
        self.expect_keyword(Keyword::Group)?;
        let name = self.expect_ident()?;
        self.expect_symbol(Symbol::Assign)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut members = Vec::new();
        if !self.check_symbol(Symbol::RBrace) {
            loop {
                members.push(self.parse_state_selector(false)?);
                if !self.match_symbol(Symbol::Comma) {
                    break;
                }
            }
        }
        self.expect_symbol(Symbol::RBrace)?;
        let end = self.expect_symbol(Symbol::Semicolon)?;
        let span = join_span(&start, &end.span);
        Ok(StateGroup {
            name,
            members,
            span,
        })
    }

    fn parse_state_decl(&mut self) -> Result<StateDecl, ParseError> {
        self.expect_keyword(Keyword::State)?;
        let name = self.expect_ident()?;
        let index = if self.match_symbol(Symbol::LBracket) {
            let saved_pos = self.pos;
            if matches!(self.peek_kind(), TokenKind::Identifier(_)) {
                let symbol = self.expect_ident()?;
                if self.match_keyword(Keyword::In) {
                    let range = self.expect_ident()?;
                    self.expect_symbol(Symbol::RBracket)?;
                    Some(StateIndexSpec::Range { symbol, range })
                } else {
                    self.pos = saved_pos;
                    let expr = self.parse_expr()?;
                    self.expect_symbol(Symbol::RBracket)?;
                    Some(StateIndexSpec::Expr(expr))
                }
            } else {
                let expr = self.parse_expr()?;
                self.expect_symbol(Symbol::RBracket)?;
                Some(StateIndexSpec::Expr(expr))
            }
        } else {
            None
        };

        let is_initial = self.match_keyword(Keyword::Initial);
        // Optional outer block carrying zero or more sub-blocks. Each sub-block
        // is either `vars { ... }` (per-state initialiser overrides) or
        // `valuations { ... }` (per-state structured display metadata). They
        // may appear in any order; either may be omitted.
        let (overrides, valuations, three_valued) = if self.match_symbol(Symbol::LBrace) {
            let mut overrides: Vec<Assignment> = Vec::new();
            let mut valuations: Vec<Assignment> = Vec::new();
            let mut three_valued: Vec<ThreeValuedDecl> = Vec::new();
            let mut saw_vars = false;
            let mut saw_valuations = false;
            let mut saw_3v = false;
            while !self.check_symbol(Symbol::RBrace) {
                let block_span = self.peek().span;
                if self.match_keyword(Keyword::Vars) {
                    if saw_vars {
                        return Err(ParseError::DuplicateItem {
                            name: "vars".into(),
                            span: block_span,
                        });
                    }
                    saw_vars = true;
                    self.expect_symbol(Symbol::LBrace)?;
                    while !self.check_symbol(Symbol::RBrace) {
                        overrides.push(self.parse_assignment()?);
                    }
                    self.expect_symbol(Symbol::RBrace)?;
                } else if self.match_keyword(Keyword::Valuations) {
                    if saw_valuations {
                        return Err(ParseError::DuplicateItem {
                            name: "valuations".into(),
                            span: block_span,
                        });
                    }
                    saw_valuations = true;
                    self.expect_symbol(Symbol::LBrace)?;
                    while !self.check_symbol(Symbol::RBrace) {
                        valuations.push(self.parse_valuation_pair()?);
                    }
                    self.expect_symbol(Symbol::RBrace)?;
                } else if self.match_keyword(Keyword::Predicates3v) {
                    if saw_3v {
                        return Err(ParseError::DuplicateItem {
                            name: "predicates_3v".into(),
                            span: block_span,
                        });
                    }
                    saw_3v = true;
                    self.expect_symbol(Symbol::LBrace)?;
                    while !self.check_symbol(Symbol::RBrace) {
                        three_valued.push(self.parse_three_valued_pair()?);
                    }
                    self.expect_symbol(Symbol::RBrace)?;
                } else {
                    let found = self.peek_kind().clone();
                    return Err(ParseError::UnexpectedToken {
                        found,
                        expected: "`vars`, `valuations`, or `predicates_3v` block",
                        span: block_span,
                    });
                }
            }
            self.expect_symbol(Symbol::RBrace)?;
            (overrides, valuations, three_valued)
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };

        self.expect_symbol(Symbol::Semicolon)?;

        Ok(StateDecl {
            name,
            index,
            is_initial,
            overrides,
            valuations,
            three_valued,
        })
    }

    /// Parse one `<predicate> = <tristate>;` entry inside a `predicates_3v { … }`
    /// block. The tristate literal is `true` / `false` (existing keywords) or the
    /// identifier `unknown`.
    fn parse_three_valued_pair(&mut self) -> Result<ThreeValuedDecl, ParseError> {
        let name = self.expect_ident()?;
        self.expect_symbol(Symbol::Assign)?;
        let value = self.parse_tristate_lit()?;
        self.expect_symbol(Symbol::Semicolon)?;
        Ok(ThreeValuedDecl { name, value })
    }

    fn parse_tristate_lit(&mut self) -> Result<TristateLit, ParseError> {
        let span = self.peek().span;
        if self.match_keyword(Keyword::True) {
            Ok(TristateLit::True)
        } else if self.match_keyword(Keyword::False) {
            Ok(TristateLit::False)
        } else if matches!(self.peek_kind(), TokenKind::Identifier(s) if s.as_str() == "unknown") {
            self.advance();
            Ok(TristateLit::Unknown)
        } else {
            Err(ParseError::UnexpectedToken {
                found: self.peek_kind().clone(),
                expected: "`true`, `false`, or `unknown`",
                span,
            })
        }
    }

    fn parse_assignment(&mut self) -> Result<Assignment, ParseError> {
        let target = self.expect_ident()?;
        self.expect_symbol(Symbol::Assign)?;
        let expr = self.parse_expr()?;
        self.expect_symbol(Symbol::Semicolon)?;
        Ok(Assignment { target, expr })
    }

    /// Like `parse_assignment` but tolerant of the LHS being a keyword (e.g.
    /// `state = ...;`). Adapter-emitted CTXDSL inside `valuations { … }` blocks
    /// often references signal names that collide with reserved keywords —
    /// accept those by re-tokenising the keyword as an identifier for the
    /// purposes of the LHS only.
    fn parse_valuation_pair(&mut self) -> Result<Assignment, ParseError> {
        let target = match self.peek_kind().clone() {
            crate::context_dsl::token::TokenKind::Identifier(_) => self.expect_ident()?,
            crate::context_dsl::token::TokenKind::Keyword(_) => {
                let span = self.peek().span;
                let name = self.source[span.start..span.end].to_string();
                self.advance();
                Ident::new(name, span)
            }
            other => {
                let span = self.peek().span;
                return Err(ParseError::UnexpectedToken {
                    found: other,
                    expected: "identifier (valuation key)",
                    span,
                });
            }
        };
        self.expect_symbol(Symbol::Assign)?;
        let expr = self.parse_expr()?;
        self.expect_symbol(Symbol::Semicolon)?;
        Ok(Assignment { target, expr })
    }

    fn parse_transitions_block(&mut self) -> Result<Vec<TransitionDecl>, ParseError> {
        self.expect_keyword(Keyword::Transitions)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut transitions = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            // mununu#570 — one declaration may yield SEVERAL transitions: `on a | b` is
            // alternation and desugars here into one single-label transition per alternative.
            transitions.extend(self.parse_transition()?);
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(transitions)
    }

    fn parse_transition(&mut self) -> Result<Vec<TransitionDecl>, ParseError> {
        self.expect_keyword(Keyword::Transition)?;
        let source = self.parse_state_selector(true)?;
        self.expect_symbol(Symbol::Arrow)?;
        // R.5 Item K sub-item K.1b (2026-06-06) — accept either a
        // single target (`-> t`) OR a bracketed-list of
        // hyper-targets (`-> [t1, t2, t3]`). The list form is the
        // multi-target hyper-must syntax; the parser stores `t1` as
        // `target` and the rest in `additional_targets`. The
        // bracketed syntax is independent of the trailing modality
        // attribute — pairing with `[must]` produces a multi-target
        // `MustHyperOnly` at realize; pairing with `[may]` or no
        // attribute is currently accepted but ignored by realize
        // (singleton may-edge to `t1` only). Realize emits a
        // warning when the additional_targets are dropped this way.
        let (target, additional_targets) = self.parse_transition_target()?;
        self.expect_keyword(Keyword::On)?;

        // mununu#570 — THREE spellings, two meanings.
        //
        //   `on a, b`    synchronisation vector (legacy spelling, unchanged)
        //   `on {a, b}`  synchronisation vector, explicit — preferred, because a comma reads
        //                like "or" to most authors and that misreading froze a real model
        //   `on a | b`   ALTERNATION — desugars to one transition per label
        //
        // Mixing the separators is refused rather than guessed: `a, b | c` has no single
        // meaning, and picking one silently is how the original defect happened.
        let braced = self.match_symbol(Symbol::LBrace);
        let label = self.parse_transition_label()?;
        let mut additional_labels = Vec::new();
        let mut saw_comma = false;
        let mut alternation = false;
        loop {
            if self.check_symbol(Symbol::Comma) {
                if alternation {
                    return Err(ParseError::UnexpectedToken {
                        found: TokenKind::Symbol(Symbol::Comma),
                        expected: "`|` — this label list mixes `,` (synchronisation vector: the \
                                   labels fire TOGETHER as one action) with `|` (alternation: \
                                   any ONE of them fires). Pick one",
                        span: self.peek().span,
                    });
                }
                self.match_symbol(Symbol::Comma);
                saw_comma = true;
                additional_labels.push(self.parse_transition_label()?);
            } else if self.check_symbol(Symbol::Pipe) {
                if braced {
                    return Err(ParseError::UnexpectedToken {
                        found: TokenKind::Symbol(Symbol::Pipe),
                        expected: "`,` or `}` — `{…}` is the synchronisation-vector spelling, so \
                                   its labels are separated by `,`. For alternation drop the \
                                   braces and write `on label a | label b`",
                        span: self.peek().span,
                    });
                }
                if saw_comma {
                    return Err(ParseError::UnexpectedToken {
                        found: TokenKind::Symbol(Symbol::Pipe),
                        expected: "`,` — this label list mixes `,` (synchronisation vector) with \
                                   `|` (alternation). Pick one",
                        span: self.peek().span,
                    });
                }
                self.match_symbol(Symbol::Pipe);
                alternation = true;
                additional_labels.push(self.parse_transition_label()?);
            } else {
                break;
            }
        }
        if braced {
            self.expect_symbol(Symbol::RBrace)?;
        }

        let mut guard = None;
        let mut effects = Vec::new();

        if self.match_keyword(Keyword::Guard) {
            guard = Some(self.parse_expr()?);
        }

        if self.match_keyword(Keyword::Effects) {
            self.expect_symbol(Symbol::LBrace)?;
            while !self.check_symbol(Symbol::RBrace) {
                effects.push(self.parse_assignment()?);
            }
            self.expect_symbol(Symbol::RBrace)?;
        }

        // R.5 Item K sub-item K.1 (2026-06-04) — optional
        // `[may]` / `[must]` / `[sharp]` modality attribute.
        // Defaults to Sharp when absent. The attribute is
        // accepted at the trailing position (after effects,
        // before semicolon) to minimise grammar disruption.
        let modality = self.parse_optional_transition_modality()?;

        self.expect_symbol(Symbol::Semicolon)?;

        // mununu#570 — ALTERNATION desugars here, into one single-label transition per
        // alternative. Doing it at the AST boundary is what keeps the change to the surface
        // syntax: `realize`, the CLTS model, the emitter and every adapter see exactly what they
        // saw before, because "N transitions with one label each" was always expressible — CTXDSL
        // simply had no way to ASK for it on one line.
        //
        // Each copy keeps the same source, target, guard, effects and modality: the alternatives
        // differ only in which label fires the otherwise-identical step.
        if alternation {
            let mut out = Vec::with_capacity(additional_labels.len() + 1);
            for l in std::iter::once(label).chain(additional_labels) {
                out.push(TransitionDecl {
                    source: source.clone(),
                    target: target.clone(),
                    label: l,
                    additional_labels: Vec::new(),
                    guard: guard.clone(),
                    effects: effects.clone(),
                    modality,
                    additional_targets: additional_targets.clone(),
                });
            }
            return Ok(out);
        }

        Ok(vec![TransitionDecl {
            source,
            target,
            label,
            additional_labels,
            guard,
            effects,
            modality,
            additional_targets,
        }])
    }

    /// R.5 Item K sub-item K.1b (2026-06-06) — parse the
    /// transition's target slot. Accepts:
    ///
    /// - Single target: `s -> t`. Returns `(t, vec![])`.
    /// - Hyper-target list: `s -> [t1, t2, t3]`. Returns
    ///   `(t1, vec![t2, t3])`. The list must be non-empty;
    ///   `s -> []` errors. Group selectors (`s -> @group`)
    ///   are not allowed inside the bracketed list (the list
    ///   form is reserved for explicit per-state hyper-targets
    ///   per the K.1b syntax spec).
    fn parse_transition_target(
        &mut self,
    ) -> Result<(StateSelector, Vec<StateSelector>), ParseError> {
        if self.match_symbol(Symbol::LBracket) {
            let mut targets = Vec::new();
            targets.push(self.parse_state_selector(false)?);
            while self.match_symbol(Symbol::Comma) {
                targets.push(self.parse_state_selector(false)?);
            }
            self.expect_symbol(Symbol::RBracket)?;
            let primary = targets.remove(0);
            Ok((primary, targets))
        } else {
            let single = self.parse_state_selector(true)?;
            Ok((single, Vec::new()))
        }
    }

    /// R.5 Item K sub-item K.1 (2026-06-04) — parse the
    /// optional modality attribute `[may]` / `[must]` /
    /// `[sharp]` on a transition declaration.
    ///
    /// Returns `TransitionModalitySpec::Sharp` when no
    /// attribute is present (the default; matches pre-K.1
    /// grammar). Errors on an unknown attribute name with a
    /// diagnostic listing the accepted values.
    fn parse_optional_transition_modality(
        &mut self,
    ) -> Result<crate::context_dsl::ast::TransitionModalitySpec, ParseError> {
        use crate::context_dsl::ast::TransitionModalitySpec;
        if !self.match_symbol(Symbol::LBracket) {
            return Ok(TransitionModalitySpec::Sharp);
        }
        let span = self.peek().span;
        let modality_text = self.expect_ident()?;
        self.expect_symbol(Symbol::RBracket)?;
        match modality_text.name.as_str() {
            "may" => Ok(TransitionModalitySpec::MayOnly),
            "must" => Ok(TransitionModalitySpec::MustOnly),
            "sharp" => Ok(TransitionModalitySpec::Sharp),
            other => Err(ParseError::UnexpectedToken {
                found: crate::context_dsl::token::TokenKind::Identifier(other.to_string()),
                expected: "`may`, `must`, or `sharp` modality attribute",
                span,
            }),
        }
    }

    fn parse_state_selector(&mut self, allow_group: bool) -> Result<StateSelector, ParseError> {
        if allow_group && self.check_keyword(Keyword::Group) {
            self.expect_keyword(Keyword::Group)?;
            let name = self.expect_ident()?;
            return Ok(StateSelector::Group(name));
        }

        if self.check_keyword(Keyword::Wildcard) {
            let start = self.peek().span;
            self.expect_keyword(Keyword::Wildcard)?;
            let (pattern, span) = self.expect_string()?;
            let combined = join_span(&start, &span);
            return Ok(StateSelector::Wildcard(WildcardPattern {
                pattern,
                span: combined,
            }));
        }

        let state = self.parse_state_ref()?;
        Ok(StateSelector::Named(state))
    }

    fn parse_state_ref(&mut self) -> Result<StateRef, ParseError> {
        let name = self.expect_ident()?;
        let indices = if self.match_symbol(Symbol::LBracket) {
            let mut list = Vec::new();
            if !self.check_symbol(Symbol::RBracket) {
                loop {
                    list.push(self.parse_expr()?);
                    if self.match_symbol(Symbol::Comma) {
                        continue;
                    }
                    break;
                }
            }
            self.expect_symbol(Symbol::RBracket)?;
            Some(list)
        } else {
            None
        };
        Ok(match indices {
            Some(indices) => StateRef::Indexed { name, indices },
            None => StateRef::Simple(name),
        })
    }

    fn parse_transition_label(&mut self) -> Result<TransitionLabel, ParseError> {
        match self.peek_kind() {
            TokenKind::Keyword(Keyword::Label) => {
                self.advance();
                let name = self.expect_ident()?;
                // R.5 Item K sub-item K.1c (2026-06-06) — when the
                // next token sequence is `[ident]` AND `ident` is
                // a modality keyword (`may` / `must` / `sharp`),
                // do NOT consume the bracket as an index
                // expression — leave it for the trailing modality
                // attribute parser. This resolves the K.1
                // ambiguity that previously forced authors to
                // wrap the modality in `effects { } [may]` or use
                // the bare `on epsilon [may]` form.
                let index = if self.check_symbol(Symbol::LBracket)
                    && !self.next_bracket_is_modality_attribute()
                {
                    self.match_symbol(Symbol::LBracket);
                    let expr = self.parse_expr()?;
                    self.expect_symbol(Symbol::RBracket)?;
                    Some(expr)
                } else {
                    None
                };
                Ok(TransitionLabel::Named { name, index })
            }
            TokenKind::Keyword(Keyword::Epsilon) => {
                let span = self.advance().span;
                Ok(TransitionLabel::Epsilon(span))
            }
            other => Err(ParseError::UnexpectedToken {
                found: other.clone(),
                expected: "`label` or `epsilon`",
                span: self.peek().span,
            }),
        }
    }

    /// R.5 Item K sub-item K.1c (2026-06-06) — peek-ahead for the
    /// modality-attribute form `[<ident>]` where `<ident>` is
    /// `may` / `must` / `sharp`. Returns `true` iff the current
    /// token is `[` AND the next is an identifier with one of
    /// the modality-attribute names AND the token after that is
    /// `]`. Does NOT consume any tokens.
    ///
    /// Used by `parse_transition_label` to disambiguate the
    /// `on label name [may]` syntax from the indexed-label
    /// expression `on label name [index]` (where `index` is an
    /// integer or identifier).
    fn next_bracket_is_modality_attribute(&self) -> bool {
        if !matches!(self.peek_kind(), TokenKind::Symbol(Symbol::LBracket)) {
            return false;
        }
        // Peek tokens at pos + 1 and pos + 2.
        let Some(ident_token) = self.tokens.get(self.pos + 1) else {
            return false;
        };
        let Some(rbracket_token) = self.tokens.get(self.pos + 2) else {
            return false;
        };
        let TokenKind::Identifier(name) = &ident_token.kind else {
            return false;
        };
        if !matches!(name.as_str(), "may" | "must" | "sharp") {
            return false;
        }
        matches!(rbracket_token.kind, TokenKind::Symbol(Symbol::RBracket))
    }

    fn parse_mu_formulas_section(&mut self) -> Result<Vec<MuFormula>, ParseError> {
        self.expect_keyword(Keyword::MuFormulas)?;
        self.expect_symbol(Symbol::LBrace)?;
        let mut formulas = Vec::new();
        while !self.check_symbol(Symbol::RBrace) {
            formulas.push(self.parse_formula()?);
        }
        self.expect_symbol(Symbol::RBrace)?;
        Ok(formulas)
    }

    fn parse_formula(&mut self) -> Result<MuFormula, ParseError> {
        self.expect_keyword(Keyword::Formula)?;
        let name = self.expect_ident()?;
        self.expect_symbol(Symbol::LBrace)?;

        let mut meta = Meta::default();
        if self.check_keyword(Keyword::Meta) {
            meta = self.parse_meta_block()?;
        }

        self.expect_keyword(Keyword::Over)?;
        let targets = if self.match_keyword(Keyword::All) {
            let span = self.previous_span();
            self.expect_symbol(Symbol::Semicolon)?;
            FormulaTargets::All(span)
        } else {
            let mut list = Vec::new();
            list.push(self.expect_ident()?);
            while self.match_symbol(Symbol::Comma) {
                list.push(self.expect_ident()?);
            }
            self.expect_symbol(Symbol::Semicolon)?;
            FormulaTargets::Named(list)
        };

        self.expect_keyword(Keyword::Body)?;
        self.expect_symbol(Symbol::Assign)?;
        let body = self.parse_formula_body()?;
        self.expect_symbol(Symbol::Semicolon)?;
        self.expect_symbol(Symbol::RBrace)?;

        Ok(MuFormula {
            name,
            meta,
            targets,
            body,
        })
    }

    fn parse_formula_body(&mut self) -> Result<FormulaExpr, ParseError> {
        // Check for explicit syntax markers
        if self.check_keyword(Keyword::Ltl) {
            // Parse as LTL
            self.advance(); // consume "ltl"
            let ltl_body = self.parse_ltl_body()?;
            return Ok(FormulaExpr::Ltl(ltl_body));
        }

        // For μ-calculus, check if there's an explicit 'mu' marker
        // The marker is only valid if followed by another fixpoint operator (mu/nu)
        // Otherwise, 'mu' is part of the formula (the fixpoint operator)
        let mu_start_token = self.peek().span;
        let mu_start = mu_start_token.start;

        // Check if there's an explicit 'mu' marker
        // If present, consume it but include it in the raw string to preserve the complete formula
        let has_mu_marker = self.check_keyword(Keyword::Mu);
        if has_mu_marker {
            self.advance(); // consume "mu" marker
        }

        // Parse μ-calculus body (captures from current position, after marker if present)
        let mu_body = self.parse_mu_body_with_start(self.peek().span.start, self.peek().span)?;

        // Always include the marker in the raw string if it was present
        // This ensures formulas like "mu nu X. ..." are preserved completely
        let raw = if has_mu_marker {
            format!("mu {}", mu_body.raw)
        } else {
            mu_body.raw
        };

        // Span should cover from the original start (including marker if present)
        let span = Span::new(
            mu_start,
            mu_body.span.end,
            mu_start_token.line,
            mu_start_token.column,
        );

        Ok(FormulaExpr::MuCalculus(MuExpr { raw, span }))
    }

    fn parse_ltl_body(&mut self) -> Result<LtlExpr, ParseError> {
        let start_token = self.peek().span;
        let start = start_token.start;

        if self.check_symbol(Symbol::Semicolon) {
            return Err(ParseError::InvalidExpr {
                span: self.peek().span,
                message: "LTL body cannot be empty".to_string(),
            });
        }

        // Read the LTL formula text until semicolon
        let mut last_end = None;
        while !self.check_symbol(Symbol::Semicolon) {
            last_end = Some(self.advance().span.end);
        }
        let end = last_end.ok_or_else(|| ParseError::InvalidExpr {
            span: self.peek().span,
            message: "LTL body cannot be empty".to_string(),
        })?;

        let raw = self.source[start..end].trim().to_string();

        // Parse using LTL parser
        let formula = ltl::parser::parse(&raw).map_err(|e| ParseError::InvalidExpr {
            span: Span::new(start, end, start_token.line, start_token.column),
            message: format!("LTL parse error: {}", e),
        })?;

        Ok(LtlExpr {
            formula,
            span: Span::new(start, end, start_token.line, start_token.column),
        })
    }

    fn parse_mu_body_with_start(
        &mut self,
        start: usize,
        start_token: Span,
    ) -> Result<MuExpr, ParseError> {
        if self.check_symbol(Symbol::Semicolon) {
            return Err(ParseError::InvalidExpr {
                span: self.peek().span,
                message: "μ-calculus body cannot be empty".to_string(),
            });
        }
        let mut last_end = None;
        while !self.check_symbol(Symbol::Semicolon) {
            last_end = Some(self.advance().span.end);
        }
        let end = last_end.ok_or_else(|| ParseError::InvalidExpr {
            span: self.peek().span,
            message: "μ-calculus body cannot be empty".to_string(),
        })?;
        let raw = self.source[start..end].trim().to_string();
        Ok(MuExpr {
            raw,
            span: Span::new(start, end, start_token.line, start_token.column),
        })
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_or_expr()
    }

    fn parse_or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_and_expr()?;
        while self.match_symbol(Symbol::PipePipe) {
            let rhs = self.parse_and_expr()?;
            expr = make_binary(expr, BinaryOp::Or, rhs);
        }
        Ok(expr)
    }

    fn parse_and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_equality_expr()?;
        while self.match_symbol(Symbol::AmpAmp) {
            let rhs = self.parse_equality_expr()?;
            expr = make_binary(expr, BinaryOp::And, rhs);
        }
        Ok(expr)
    }

    fn parse_equality_expr(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_rel_expr()?;
        loop {
            if self.match_symbol(Symbol::EqEq) {
                let rhs = self.parse_rel_expr()?;
                expr = make_binary(expr, BinaryOp::Eq, rhs);
            } else if self.match_symbol(Symbol::NotEq) {
                let rhs = self.parse_rel_expr()?;
                expr = make_binary(expr, BinaryOp::Ne, rhs);
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_rel_expr(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_add_expr()?;
        loop {
            if self.match_symbol(Symbol::Lt) {
                let rhs = self.parse_add_expr()?;
                expr = make_binary(expr, BinaryOp::Lt, rhs);
            } else if self.match_symbol(Symbol::Lte) {
                let rhs = self.parse_add_expr()?;
                expr = make_binary(expr, BinaryOp::Le, rhs);
            } else if self.match_symbol(Symbol::Gt) {
                let rhs = self.parse_add_expr()?;
                expr = make_binary(expr, BinaryOp::Gt, rhs);
            } else if self.match_symbol(Symbol::Gte) {
                let rhs = self.parse_add_expr()?;
                expr = make_binary(expr, BinaryOp::Ge, rhs);
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_add_expr(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_mul_expr()?;
        loop {
            if self.match_symbol(Symbol::Plus) {
                let rhs = self.parse_mul_expr()?;
                expr = make_binary(expr, BinaryOp::Add, rhs);
            } else if self.match_symbol(Symbol::Minus) {
                let rhs = self.parse_mul_expr()?;
                expr = make_binary(expr, BinaryOp::Sub, rhs);
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_mul_expr(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_unary_expr()?;
        loop {
            if self.match_symbol(Symbol::Star) {
                let rhs = self.parse_unary_expr()?;
                expr = make_binary(expr, BinaryOp::Mul, rhs);
            } else if self.match_symbol(Symbol::Slash) {
                let rhs = self.parse_unary_expr()?;
                expr = make_binary(expr, BinaryOp::Div, rhs);
            } else if self.match_symbol(Symbol::Percent) {
                let rhs = self.parse_unary_expr()?;
                expr = make_binary(expr, BinaryOp::Mod, rhs);
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_unary_expr(&mut self) -> Result<Expr, ParseError> {
        if self.match_symbol(Symbol::Bang) {
            let op_span = self.previous_span();
            let expr = self.parse_unary_expr()?;
            let span = join_span(&op_span, &expr.span);
            Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(expr),
                },
                span,
            })
        } else if self.match_symbol(Symbol::Minus) {
            let op_span = self.previous_span();
            let expr = self.parse_unary_expr()?;
            let span = join_span(&op_span, &expr.span);
            Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(expr),
                },
                span,
            })
        } else {
            self.parse_primary_expr()
        }
    }

    fn parse_primary_expr(&mut self) -> Result<Expr, ParseError> {
        match self.peek_kind() {
            TokenKind::Integer(_) => {
                let token = self.advance().clone();
                match token.kind {
                    TokenKind::Integer(value) => Ok(Expr {
                        kind: ExprKind::Integer(value),
                        span: token.span,
                    }),
                    _ => unreachable!("token kind changed between peek and advance"),
                }
            }
            TokenKind::Identifier(_) => {
                let ident = self.expect_ident()?;
                let ident_span = ident.span;
                if self.match_symbol(Symbol::LBracket) {
                    let expr = self.parse_expr()?;
                    let end = self.expect_symbol(Symbol::RBracket)?.span;
                    let span = join_span(&ident_span, &end);
                    Ok(Expr {
                        kind: ExprKind::Index {
                            target: ident,
                            expr: Box::new(expr),
                        },
                        span,
                    })
                } else {
                    Ok(Expr {
                        kind: ExprKind::Ident(ident),
                        span: ident_span,
                    })
                }
            }
            TokenKind::Symbol(Symbol::LParen) => {
                let open = self.expect_symbol(Symbol::LParen)?.span;
                let expr = self.parse_expr()?;
                let close = self.expect_symbol(Symbol::RParen)?.span;
                Ok(Expr {
                    kind: ExprKind::Group(Box::new(expr.clone())),
                    span: join_span(&open, &close),
                })
            }
            TokenKind::Keyword(Keyword::True | Keyword::False) => {
                let token = self.advance().clone();
                let name = match token.kind {
                    TokenKind::Keyword(Keyword::True) => "true",
                    TokenKind::Keyword(Keyword::False) => "false",
                    _ => unreachable!("token kind changed between peek and advance"),
                };
                let ident = Ident::new(name.to_owned(), token.span);
                Ok(Expr {
                    kind: ExprKind::Ident(ident.clone()),
                    span: ident.span,
                })
            }
            other => Err(ParseError::UnexpectedToken {
                found: other.clone(),
                expected: "expression",
                span: self.peek().span,
            }),
        }
    }

    fn parse_bool_literal(&mut self) -> Result<bool, ParseError> {
        match self.peek_kind() {
            TokenKind::Keyword(Keyword::True) => {
                self.advance();
                Ok(true)
            }
            TokenKind::Keyword(Keyword::False) => {
                self.advance();
                Ok(false)
            }
            TokenKind::Identifier(name) if name == "true" => {
                let _ = self.advance();
                Ok(true)
            }
            TokenKind::Identifier(name) if name == "false" => {
                let _ = self.advance();
                Ok(false)
            }
            other => Err(ParseError::UnexpectedToken {
                found: other.clone(),
                expected: "boolean literal",
                span: self.peek().span,
            }),
        }
    }

    fn parse_controller_diagnostics_block(&mut self) -> Result<DiagnosticsConfig, ParseError> {
        self.expect_keyword(Keyword::Diagnostics)?;
        self.expect_symbol(Symbol::LBrace)?;

        let mut config = DiagnosticsConfig::default();

        while !self.check_symbol(Symbol::RBrace) {
            match self.peek_kind() {
                TokenKind::Keyword(Keyword::Counterexample) => {
                    self.expect_keyword(Keyword::Counterexample)?;
                    self.expect_symbol(Symbol::Assign)?;
                    let value = self.parse_bool_literal()?;
                    self.expect_symbol(Symbol::Semicolon)?;
                    config.counterexample = Some(value);
                }
                TokenKind::Keyword(Keyword::DeadlockTraces) => {
                    self.expect_keyword(Keyword::DeadlockTraces)?;
                    self.expect_symbol(Symbol::Assign)?;
                    let value = self.parse_bool_literal()?;
                    self.expect_symbol(Symbol::Semicolon)?;
                    config.deadlock_traces = Some(value);
                }
                TokenKind::Keyword(Keyword::MaxCounterTraces) => {
                    self.expect_keyword(Keyword::MaxCounterTraces)?;
                    self.expect_symbol(Symbol::Assign)?;
                    let (value, span) = self.expect_integer()?;
                    let converted = u32::try_from(value).map_err(|_| ParseError::InvalidExpr {
                        span,
                        message: "max_counter_traces must be non-negative".to_owned(),
                    })?;
                    config.max_counter_traces = Some(converted);
                    self.expect_symbol(Symbol::Semicolon)?;
                }
                TokenKind::Keyword(Keyword::ProofObligations) => {
                    self.expect_keyword(Keyword::ProofObligations)?;
                    self.expect_symbol(Symbol::Assign)?;
                    let value = self.parse_bool_literal()?;
                    self.expect_symbol(Symbol::Semicolon)?;
                    config.proof_obligations = Some(value);
                }
                other => {
                    return Err(ParseError::UnexpectedToken {
                        found: other.clone(),
                        expected: "counterexample, deadlock_traces, proof_obligations, or max_counter_traces",
                        span: self.peek().span,
                    });
                }
            }
        }

        self.expect_symbol(Symbol::RBrace)?;
        Ok(config)
    }

    fn expect_ident(&mut self) -> Result<Ident, ParseError> {
        match self.advance().clone() {
            Token {
                kind: TokenKind::Identifier(name),
                span,
            } => Ok(Ident { name, span }),
            token => Err(ParseError::UnexpectedToken {
                found: token.kind,
                expected: "identifier",
                span: token.span,
            }),
        }
    }

    fn expect_keyword(&mut self, keyword: Keyword) -> Result<(), ParseError> {
        match self.advance().clone() {
            Token {
                kind: TokenKind::Keyword(found),
                ..
            } if found == keyword => Ok(()),
            token => Err(ParseError::UnexpectedToken {
                found: token.kind,
                expected: "keyword",
                span: token.span,
            }),
        }
    }

    fn expect_symbol(&mut self, symbol: Symbol) -> Result<Token, ParseError> {
        let token = self.advance().clone();
        match token.kind {
            TokenKind::Symbol(s) if s == symbol => Ok(token),
            _ => Err(ParseError::UnexpectedToken {
                found: token.kind,
                expected: "symbol",
                span: token.span,
            }),
        }
    }

    fn expect_string(&mut self) -> Result<(String, Span), ParseError> {
        match self.advance().clone() {
            Token {
                kind: TokenKind::String(text),
                span,
            } => Ok((text, span)),
            token => Err(ParseError::UnexpectedToken {
                found: token.kind,
                expected: "string literal",
                span: token.span,
            }),
        }
    }

    fn expect_integer(&mut self) -> Result<(i64, Span), ParseError> {
        match self.advance().clone() {
            Token {
                kind: TokenKind::Integer(value),
                span,
            } => Ok((value, span)),
            token => Err(ParseError::UnexpectedToken {
                found: token.kind,
                expected: "integer literal",
                span: token.span,
            }),
        }
    }

    fn match_keyword(&mut self, keyword: Keyword) -> bool {
        if self.check_keyword(keyword) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn check_keyword(&self, keyword: Keyword) -> bool {
        matches!(self.peek_kind(), TokenKind::Keyword(k) if *k == keyword)
    }

    fn match_symbol(&mut self, symbol: Symbol) -> bool {
        if matches!(self.peek_kind(), TokenKind::Symbol(s) if *s == symbol) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn check_symbol(&self, symbol: Symbol) -> bool {
        matches!(self.peek_kind(), TokenKind::Symbol(s) if *s == symbol)
    }

    fn advance(&mut self) -> &Token {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        &self.tokens[self.pos - 1]
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn previous_span(&self) -> Span {
        if self.pos == 0 {
            Span::new(0, 0, 0, 0)
        } else {
            self.tokens[self.pos - 1].span
        }
    }
}

fn join_span(a: &Span, b: &Span) -> Span {
    Span::new(a.start, b.end, a.line, a.column)
}

fn make_binary(lhs: Expr, op: BinaryOp, rhs: Expr) -> Expr {
    let span = Span::new(lhs.span.start, rhs.span.end, lhs.span.line, lhs.span.column);
    Expr {
        kind: ExprKind::Binary {
            left: Box::new(lhs),
            op,
            right: Box::new(rhs),
        },
        span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_dsl::ast::CompositionKind;

    fn parse_ok(source: &str) -> ContextDoc {
        parse(source).expect("expected parse success")
    }

    /// mununu#570 — the three spellings, and what each produces.
    fn labels_of(doc: &ContextDoc) -> Vec<Vec<String>> {
        doc.automata[0]
            .transitions
            .iter()
            .map(|t| {
                std::iter::once(&t.label)
                    .chain(t.additional_labels.iter())
                    // NAMES only — `Debug` embeds spans, and `{` shifts every offset by one, so
                    // comparing Debug output would make two identical actions look different.
                    .map(|l| match l {
                        TransitionLabel::Named { name, .. } => name.name.clone(),
                        TransitionLabel::Epsilon(_) => "epsilon".to_string(),
                    })
                    .collect()
            })
            .collect()
    }

    fn one_automaton(transitions: &str) -> String {
        format!(
            "context c {{ automata {{ automaton A {{ states {{ state S initial; state T; }} \
             transitions {{ {transitions} }} }} }} }}"
        )
    }

    /// `on a | b` is ALTERNATION: two transitions, one label each.
    ///
    /// This is the spelling that did not exist, and its absence is what froze a 7-automaton
    /// model — 19 "I ignore the other labels" self-loops written with commas became 19
    /// unsatisfiable compound actions.
    #[test]
    fn xl570_pipe_is_alternation_and_desugars_to_one_transition_per_label() {
        let doc = parse_ok(&one_automaton(
            "transition S -> T on label a | label b | label c;",
        ));
        let got = labels_of(&doc);
        assert_eq!(got.len(), 3, "one transition per alternative: {got:?}");
        for t in &got {
            assert_eq!(t.len(), 1, "each alternative is SINGLE-label: {t:?}");
        }
    }

    /// `on a, b` stays a synchronisation vector — one transition carrying both labels. Existing
    /// models (the v10 fabric handshake, emit.rs's `{env_*, ctrl_*}` rounds) depend on this.
    #[test]
    fn xl570_comma_is_still_a_synchronisation_vector() {
        let doc = parse_ok(&one_automaton("transition S -> T on label a, label b;"));
        let got = labels_of(&doc);
        assert_eq!(got.len(), 1, "ONE compound action: {got:?}");
        assert_eq!(got[0].len(), 2, "carrying both labels: {got:?}");
    }

    /// `on {a, b}` is the same thing, spelled so the reader cannot mistake it for "or".
    #[test]
    fn xl570_braces_are_the_explicit_synchronisation_vector() {
        let braced = labels_of(&parse_ok(&one_automaton(
            "transition S -> T on {label a, label b};",
        )));
        let comma = labels_of(&parse_ok(&one_automaton(
            "transition S -> T on label a, label b;",
        )));
        assert_eq!(
            braced, comma,
            "`{{a,b}}` and `a, b` must be the same action"
        );
    }

    /// Mixing the separators has no single meaning, so it is refused rather than guessed.
    /// Silently picking one is how the original defect happened.
    #[test]
    fn xl570_mixing_separators_is_refused_with_a_reason() {
        let err = parse(&one_automaton(
            "transition S -> T on label a, label b | label c;",
        ))
        .expect_err("mixing `,` and `|` must be refused");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("synchronisation vector") && msg.contains("alternation"),
            "the error must explain BOTH meanings, not just name a bad token: {msg}"
        );

        let err2 = parse(&one_automaton(
            "transition S -> T on label a | label b, label c;",
        ))
        .expect_err("the other order must be refused too");
        assert!(format!("{err2:?}").contains("Pick one"), "got: {err2:?}");
    }

    /// A braced list is the sync-vector spelling, so `|` inside it is a contradiction.
    #[test]
    fn xl570_pipe_inside_braces_is_refused() {
        let err = parse(&one_automaton("transition S -> T on {label a | label b};"))
            .expect_err("`|` inside `{{…}}` must be refused");
        assert!(
            format!("{err:?}").contains("drop the braces"),
            "the error must say how to get alternation: {err:?}"
        );
    }

    /// Alternation copies the whole step, not just the label — same target, guard and effects.
    #[test]
    fn xl570_alternation_copies_guard_and_target_to_every_alternative() {
        let doc = parse_ok(&one_automaton(
            "transition S -> T on label a | label b guard x == 1 effects { x = 2; };",
        ));
        assert_eq!(doc.automata[0].transitions.len(), 2);
        for t in &doc.automata[0].transitions {
            assert!(t.guard.is_some(), "each alternative keeps the guard");
            assert_eq!(t.effects.len(), 1, "and the effects");
        }
    }

    #[test]
    fn rejects_unknown_composition_kind() {
        let source = r"
context bad_kind {
    automata {
        automaton A { states { state S initial; } transitions { transition S -> S on epsilon; } }
    }
    composition {
        parallel Bad { members [A]; }
    }
}
";

        let err = parse(source).expect_err("parser should reject unknown kind");
        match err {
            ParseError::UnexpectedToken { expected, .. } => {
                assert_eq!(expected, "composition kind");
            }
            other => panic!("unexpected error kind: {:?}", other),
        }
    }

    #[test]
    fn rejects_missing_members_semicolon() {
        let source = r"
context missing_semicolon {
    automata {
        automaton A { states { state S initial; } transitions { transition S -> S on epsilon; } }
    }
    composition {
        synchronous Sync {
            members [A]
        }
    }
}
";

        assert!(
            parse(source).is_err(),
            "parser should require trailing semicolon"
        );
    }

    #[test]
    fn parses_all_context_sections() {
        // Test parser initialization and all section keywords (lines 35-74)
        let source = r#"
context full_context {
    alphabet {
        label tick;
        label sync = "sync_label";
    }
    constants {
        const MAX = 100;
    }
    ranges {
        range depth = 0 ..= 10;
    }
    automata {
        automaton A { states { state S initial; } transitions { transition S -> S on epsilon; } }
    }
    composition {
        synchronous Sync { members [A]; }
    }
    controllers {
        controller C {
            source A;
            satisfying F;
        }
    }
    mu_formulas {
        formula F { over A; body = true; }
    }
}
"#;
        let doc = parse(source).expect("should parse all sections");
        assert_eq!(doc.name.name, "full_context");
        assert_eq!(doc.alphabet.len(), 2);
        assert_eq!(doc.constants.len(), 1);
        assert_eq!(doc.ranges.len(), 1);
        assert_eq!(doc.automata.len(), 1);
        assert_eq!(doc.compositions.len(), 1);
        assert_eq!(doc.controllers.len(), 1);
        assert_eq!(doc.mu_formulas.len(), 1);
    }

    #[test]
    fn parses_alphabet_with_display() {
        // Test alphabet parsing with optional display (lines 103-107)
        let source = r#"
context test {
    alphabet {
        label tick = "tick_label";
        label sync;
    }
}
"#;
        let doc = parse(source).expect("should parse alphabet with display");
        assert_eq!(doc.alphabet.len(), 2);
        // Find labels by name since order may vary
        let tick = doc.alphabet.iter().find(|a| a.name.name == "tick").unwrap();
        let sync = doc.alphabet.iter().find(|a| a.name.name == "sync").unwrap();
        assert_eq!(tick.display.as_ref().unwrap(), "tick_label");
        assert!(sync.display.is_none());
    }

    #[test]
    fn parses_ranges_section() {
        // Test ranges parsing (lines 145-156)
        let source = r"
context test {
    constants {
        const MAX = 10;
    }
    ranges {
        range depth = 0 ..= MAX;
        range count = -5 ..= 5;
    }
}
";
        let doc = parse(source).expect("should parse ranges");
        assert_eq!(doc.ranges.len(), 2);
        // Canonicalization may reorder, so check both names exist
        let names: Vec<_> = doc.ranges.iter().map(|r| r.name.name.as_str()).collect();
        assert!(names.contains(&"depth"));
        assert!(names.contains(&"count"));
    }

    #[test]
    fn parses_constants_section() {
        // Test constants parsing (lines 115-135)
        let source = r"
context test {
    constants {
        const MAX = 100;
        const MIN = 10;
    }
    automata {
        automaton A { states { state S initial; } transitions { transition S -> S on epsilon; } }
    }
}
";
        let doc = parse(source).expect("should parse constants");
        assert_eq!(doc.constants.len(), 2);
        // Canonicalization may reorder, so check both names exist
        let names: Vec<_> = doc.constants.iter().map(|c| c.name.name.as_str()).collect();
        assert!(names.contains(&"MAX"));
        assert!(names.contains(&"MIN"));
    }

    #[test]
    fn parses_automaton_with_all_blocks() {
        // Test automaton parsing with all block types (lines 314-356)
        let source = r#"
context test {
    ranges {
        range depth = 0 ..= 10;
    }
    automata {
        automaton A {
            meta { comment = "test"; }
            parameters { param P in depth; }
            alphabet { label tick; }
            variables { var flag: bool = false; }
            state_groups { group G = { S }; }
            states { state S initial; }
            transitions { transition S -> S on epsilon; }
        }
    }
}
"#;
        let doc = parse(source).expect("should parse automaton with all blocks");
        assert_eq!(doc.automata.len(), 1);
        let automaton = &doc.automata[0];

        // Verify meta block content
        assert_eq!(automaton.meta.comment.as_ref().unwrap(), "test");

        // Verify parameters content
        assert_eq!(automaton.parameters.len(), 1);
        let param = &automaton.parameters[0];
        assert_eq!(param.name.name, "P");
        match &param.spec {
            crate::context_dsl::ast::RangeSpec::Named(range_name) => {
                assert_eq!(range_name.name, "depth");
            }
            _ => panic!("expected Named range spec"),
        }

        // Verify alphabet content
        assert_eq!(automaton.alphabet.len(), 1);
        assert_eq!(automaton.alphabet[0].name.name, "tick");
        assert!(automaton.alphabet[0].index.is_none());

        // Verify variables content
        assert_eq!(automaton.variables.len(), 1);
        let var = &automaton.variables[0];
        assert_eq!(var.name.name, "flag");
        assert!(var.index.is_none());
        assert_eq!(var.ty, crate::context_dsl::ast::TypeName::Bool);
        match &var.init.kind {
            crate::context_dsl::ast::ExprKind::Ident(id) => {
                assert_eq!(id.name, "false");
            }
            _ => panic!("expected false identifier"),
        }

        // Verify state groups content
        assert_eq!(automaton.state_groups.len(), 1);
        let group = &automaton.state_groups[0];
        assert_eq!(group.name.name, "G");
        assert_eq!(group.members.len(), 1);
        match &group.members[0] {
            crate::context_dsl::ast::StateSelector::Named(
                crate::context_dsl::ast::StateRef::Simple(ident),
            ) => {
                assert_eq!(ident.name, "S");
            }
            _ => panic!("expected named state selector with simple ref"),
        }

        // Verify states content
        assert_eq!(automaton.states.len(), 1);
        let state = &automaton.states[0];
        assert_eq!(state.name.name, "S");
        assert!(state.is_initial);

        // Verify transitions content
        assert_eq!(automaton.transitions.len(), 1);
        let trans = &automaton.transitions[0];
        match &trans.source {
            crate::context_dsl::ast::StateSelector::Named(
                crate::context_dsl::ast::StateRef::Simple(ident),
            ) => {
                assert_eq!(ident.name, "S");
            }
            _ => panic!("expected named state selector"),
        }
        match &trans.target {
            crate::context_dsl::ast::StateSelector::Named(
                crate::context_dsl::ast::StateRef::Simple(ident),
            ) => {
                assert_eq!(ident.name, "S");
            }
            _ => panic!("expected named state selector"),
        }
        match &trans.label {
            crate::context_dsl::ast::TransitionLabel::Epsilon(_) => {}
            _ => panic!("expected epsilon label"),
        }
    }

    #[test]
    fn parses_variables_with_indexed() {
        // Test variable parsing with indexed variables (lines 471-488)
        // Indexed variables have index expressions like var arr[10]: i64 = 0;
        let source = r"
context test {
    automata {
        automaton A {
            variables {
                var flag: bool = false;
                var arr[10]: i64 = 0;
            }
            states { state S initial; }
            transitions { transition S -> S on epsilon; }
        }
    }
}
";
        let doc = parse(source).expect("should parse indexed variables");
        let automaton = &doc.automata[0];
        assert_eq!(automaton.variables.len(), 2);
        // Find variables by name since order may vary
        let flag_var = automaton
            .variables
            .iter()
            .find(|v| v.name.name == "flag")
            .unwrap();
        let arr_var = automaton
            .variables
            .iter()
            .find(|v| v.name.name == "arr")
            .unwrap();
        // flag has no index
        assert!(flag_var.index.is_none());
        // arr has index expression
        assert!(arr_var.index.is_some());
    }

    #[test]
    fn parses_alphabet_refs_with_index() {
        // Test alphabet reference parsing with indices (lines 450-461)
        let source = r"
context test {
    automata {
        automaton A {
            alphabet {
                label tick[0];
                label sync[10];
            }
            states { state S initial; }
            transitions { transition S -> S on epsilon; }
        }
    }
}
";
        let doc = parse(source).expect("should parse alphabet refs with indices");
        let automaton = &doc.automata[0];
        assert_eq!(automaton.alphabet.len(), 2);
        assert!(automaton.alphabet[0].index.is_some());
        assert!(automaton.alphabet[1].index.is_some());
    }

    #[test]
    fn rejects_unexpected_section_keyword() {
        // Test error recovery for unexpected section keyword (lines 69-76)
        let source = r"
context test {
    invalid_section {
    }
}
";
        let err = parse(source).expect_err("should reject invalid section");
        match err {
            ParseError::UnexpectedToken { expected, .. } => {
                assert_eq!(expected, "section keyword");
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn rejects_unexpected_automaton_block_keyword() {
        // Test error recovery for unexpected automaton block (lines 347-354)
        let source = r"
context test {
    automata {
        automaton A {
            invalid_block { }
        }
    }
}
";
        let err = parse(source).expect_err("should reject invalid automaton block");
        match err {
            ParseError::UnexpectedToken { expected, .. } => {
                assert_eq!(expected, "automaton block keyword");
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn parses_mu_formulas_section() {
        // Test mu-formula parsing (lines 747-819)
        let source = r#"
context test {
    automata {
        automaton A { states { state S initial; } transitions { transition S -> S on epsilon; } }
        automaton B { states { state T initial; } transitions { transition T -> T on epsilon; } }
    }
    mu_formulas {
        formula F1 {
            over A;
            body = true;
        }
        formula F2 {
            meta { id = "f2"; comment = "test formula"; }
            over A, B;
            body = <labels={tick}>true;
        }
        formula F3 {
            over all;
            body = mu X. (p || <> X);
        }
        // Note: The "mu" in "mu X. (p || <> X)" is consumed as a syntax marker,
        // so the raw body will be "X. (p || <> X)" (without the "mu" prefix)
    }
}
"#;
        let doc = parse(source).expect("should parse mu-formulas");
        assert_eq!(doc.mu_formulas.len(), 3);

        // Find formulas by name since order may vary due to canonicalization
        let f1 = doc
            .mu_formulas
            .iter()
            .find(|f| f.name.name == "F1")
            .unwrap();
        let f2 = doc
            .mu_formulas
            .iter()
            .find(|f| f.name.name == "F2")
            .unwrap();
        let f3 = doc
            .mu_formulas
            .iter()
            .find(|f| f.name.name == "F3")
            .unwrap();

        // Verify F1: simple formula with single target, no meta
        assert!(f1.meta.id.is_none());
        assert!(f1.meta.comment.is_none());
        match &f1.targets {
            crate::context_dsl::ast::FormulaTargets::Named(list) => {
                assert_eq!(list.len(), 1);
                assert_eq!(list[0].name, "A");
            }
            _ => panic!("expected Named targets"),
        }
        match &f1.body {
            FormulaExpr::MuCalculus(mu_expr) => {
                assert_eq!(mu_expr.raw.trim(), "true");
            }
            FormulaExpr::Ltl(_) => panic!("Expected μ-calculus formula in test"),
        }

        // Verify F2: formula with meta, multiple targets
        assert_eq!(f2.meta.id.as_ref().unwrap(), "f2");
        assert_eq!(f2.meta.comment.as_ref().unwrap(), "test formula");
        match &f2.targets {
            crate::context_dsl::ast::FormulaTargets::Named(list) => {
                assert_eq!(list.len(), 2);
                assert_eq!(list[0].name, "A");
                assert_eq!(list[1].name, "B");
            }
            _ => panic!("expected Named targets"),
        }
        match &f2.body {
            FormulaExpr::MuCalculus(mu_expr) => {
                assert_eq!(mu_expr.raw.trim(), "<labels={tick}>true");
            }
            FormulaExpr::Ltl(_) => panic!("Expected μ-calculus formula in test"),
        }

        // Verify F3: formula with "over all"
        match &f3.targets {
            crate::context_dsl::ast::FormulaTargets::All(_) => {}
            _ => panic!("expected All targets"),
        }
        match &f3.body {
            FormulaExpr::MuCalculus(mu_expr) => {
                // The "mu" marker is consumed but included in raw string for completeness
                assert_eq!(mu_expr.raw.trim(), "mu X. (p || <> X)");
            }
            FormulaExpr::Ltl(_) => panic!("Expected μ-calculus formula in test"),
        }
    }

    #[test]
    fn parses_mu_marker_with_nu_fixpoint() {
        // Test case: body = mu nu X. ...; where 'mu' is marker and 'nu' is fixpoint operator
        let source = r#"
context test {
    automata {
        automaton A {
            states { state s initial; }
            transitions { transition s -> s on epsilon; }
        }
    }
    mu_formulas {
        formula test {
            over A;
            body = mu nu X. (safe && [] X);
        }
    }
}
"#;
        let doc = parse(source).expect("should parse");
        let formula = doc
            .mu_formulas
            .iter()
            .find(|f| f.name.name == "test")
            .expect("formula should exist");

        match &formula.body {
            FormulaExpr::MuCalculus(mu_expr) => {
                // The "mu" marker should be included in the raw string
                // so the complete formula "mu nu X. (safe && [] X)" is preserved
                assert_eq!(
                    mu_expr.raw.trim(),
                    "mu nu X. (safe && [] X)",
                    "Raw formula should include the 'mu' marker when present"
                );
            }
            FormulaExpr::Ltl(_) => panic!("Expected μ-calculus formula"),
        }
    }

    #[test]
    fn parses_type_names() {
        // Test type name parsing (lines 494-503)
        let source = r"
context test {
    automata {
        automaton A {
            variables {
                var flag: bool = false;
                var count: i64 = 0;
            }
            states { state S initial; }
            transitions { transition S -> S on epsilon; }
        }
    }
}
";
        let doc = parse(source).expect("should parse type names");
        let automaton = &doc.automata[0];
        assert_eq!(automaton.variables.len(), 2);
        // Find variables by name since order may vary
        let flag_var = automaton
            .variables
            .iter()
            .find(|v| v.name.name == "flag")
            .unwrap();
        let count_var = automaton
            .variables
            .iter()
            .find(|v| v.name.name == "count")
            .unwrap();
        match flag_var.ty {
            TypeName::Bool => {}
            _ => panic!("expected bool type for flag"),
        }
        match count_var.ty {
            TypeName::I64 => {}
            _ => panic!("expected i64 type for count"),
        }
    }

    #[test]
    fn identifier_parses_as_enum_type() {
        // Identifiers are now accepted as enum type references.
        // Validation that the enum exists happens during realization, not parsing.
        let source = r"
context test {
    automata {
        automaton A {
            variables {
                var x: MyEnum = 0;
            }
            states { state S initial; }
            transitions { transition S -> S on epsilon; }
        }
    }
}
";
        let doc = parse_ok(source);
        let var = &doc.automata[0].variables[0];
        assert_eq!(var.ty, TypeName::Enum("MyEnum".to_string()));
    }

    #[test]
    fn rejects_non_identifier_type_name() {
        // Non-identifier tokens (numbers, symbols) should still be rejected.
        let source = r#"
context test {
    automata {
        automaton A {
            variables {
                var x: 42 = 0;
            }
            states { state S initial; }
            transitions { transition S -> S on epsilon; }
        }
    }
}
"#;
        let err = parse(source).expect_err("should reject numeric type");
        match err {
            ParseError::UnexpectedToken { expected, .. } => {
                assert!(expected.contains("type name"));
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn canonicalises_composition_members() {
        let source = r"
context comp_order {
    automata {
        automaton A { states { state SA initial; } transitions { transition SA -> SA on epsilon; } }
        automaton B { states { state SB initial; } transitions { transition SB -> SB on epsilon; } }
        automaton C { states { state SC initial; } transitions { transition SC -> SC on epsilon; } }
    }
    composition {
        asynchronous Async {
            members [C, A, B];
        }
    }
}
";

        let doc = parse_ok(source);
        let comp = &doc.compositions[0];
        assert_eq!(comp.kind, CompositionKind::Asynchronous);
        let member_names: Vec<&str> = comp.members.iter().map(|m| m.name.name.as_str()).collect();
        assert_eq!(
            member_names,
            vec!["A", "B", "C"],
            "members should be canonicalised"
        );
    }

    // R.5 Item K sub-item K.1 (2026-06-04) — CTXDSL parser
    // + AST surface for `[may]` / `[must]` / `[sharp]` modality
    // attributes on transitions.

    use crate::context_dsl::ast::TransitionModalitySpec;

    fn first_transition(src: &str) -> super::TransitionDecl {
        let doc = parse_ok(src);
        doc.automata
            .first()
            .expect("at least one automaton")
            .transitions
            .first()
            .cloned()
            .expect("at least one transition")
    }

    #[test]
    fn r5_subitem_k1_transition_without_modality_defaults_to_sharp() {
        // Backward-compat sanity: pre-K.1 grammar (no attribute)
        // parses with `modality = Sharp`.
        let src = r"
context k1_default {
    automata {
        automaton A {
            states { state S initial; state T; }
            transitions { transition S -> T on epsilon; }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, TransitionModalitySpec::Sharp);
    }

    #[test]
    fn r5_subitem_k1_transition_parses_may_attribute() {
        let src = r"
context k1_may {
    automata {
        automaton A {
            states { state S initial; state T; }
            transitions { transition S -> T on epsilon [may]; }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, TransitionModalitySpec::MayOnly);
    }

    #[test]
    fn r5_subitem_k1_transition_parses_must_attribute() {
        let src = r"
context k1_must {
    automata {
        automaton A {
            states { state S initial; state T; }
            transitions { transition S -> T on epsilon [must]; }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, TransitionModalitySpec::MustOnly);
    }

    #[test]
    fn r5_subitem_k1_transition_parses_explicit_sharp_attribute() {
        // `[sharp]` parses but is equivalent to no-attribute
        // default. Useful when the author wants to be explicit.
        let src = r"
context k1_sharp {
    automata {
        automaton A {
            states { state S initial; state T; }
            transitions { transition S -> T on epsilon [sharp]; }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, TransitionModalitySpec::Sharp);
    }

    #[test]
    fn r5_subitem_k1_transition_rejects_unknown_modality_attribute() {
        let src = r"
context k1_bogus {
    automata {
        automaton A {
            states { state S initial; state T; }
            transitions { transition S -> T on epsilon [bogus]; }
        }
    }
}
";
        let err = parse(src).expect_err("unknown attribute must error");
        match err {
            ParseError::UnexpectedToken { expected, .. } => {
                assert!(
                    expected.contains("may") && expected.contains("must"),
                    "diagnostic must name valid attributes; got: {expected}"
                );
            }
            other => panic!("unexpected error kind: {:?}", other),
        }
    }

    #[test]
    fn r5_subitem_k1_modality_attribute_after_effects_block() {
        // The attribute is accepted at the trailing position
        // (after `effects { ... }`). Verifies grammar position
        // doesn't conflict with effects parsing.
        let src = r"
context k1_post_effects {
    automata {
        automaton A {
            states { state S initial; state T; }
            transitions { transition S -> T on epsilon effects { } [may]; }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, TransitionModalitySpec::MayOnly);
    }

    /// R.5 Item K sub-item K.1b (2026-06-06) — bracketed-list
    /// target syntax `transition s -> [t1, t2, t3] on a [must];`
    /// stores `t1` in `target` and `[t2, t3]` in
    /// `additional_targets`.
    #[test]
    fn r5_subitem_k1b_multi_target_hyper_must_parses_correctly() {
        let src = r"
context k1b_multi_target {
    automata {
        automaton A {
            states { state S initial; state T1; state T2; state T3; }
            transitions {
                transition S -> [T1, T2, T3] on epsilon [must];
            }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, super::TransitionModalitySpec::MustOnly);
        assert_eq!(t.additional_targets.len(), 2);
        // Primary target is T1; additional are T2, T3.
        let target_names: Vec<String> = std::iter::once(&t.target)
            .chain(t.additional_targets.iter())
            .map(|sel| match sel {
                super::StateSelector::Named(super::StateRef::Simple(ident)) => ident.name.clone(),
                _ => "?".to_string(),
            })
            .collect();
        assert_eq!(target_names, vec!["T1", "T2", "T3"]);
    }

    /// R.5 Item K sub-item K.1b — single-target form (no brackets)
    /// continues to parse with `additional_targets.is_empty()`.
    #[test]
    fn r5_subitem_k1b_single_target_form_leaves_additional_empty() {
        let src = r"
context k1b_single_target {
    automata {
        automaton A {
            states { state S initial; state T; }
            transitions { transition S -> T on epsilon [must]; }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, super::TransitionModalitySpec::MustOnly);
        assert!(t.additional_targets.is_empty());
    }

    /// R.5 Item K sub-item K.1b — bracketed list with a single
    /// element parses correctly and leaves `additional_targets`
    /// empty (degenerate but legal).
    #[test]
    fn r5_subitem_k1b_singleton_bracketed_list_is_legal() {
        let src = r"
context k1b_singleton_brackets {
    automata {
        automaton A {
            states { state S initial; state T; }
            transitions { transition S -> [T] on epsilon [must]; }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, super::TransitionModalitySpec::MustOnly);
        assert!(t.additional_targets.is_empty());
    }

    /// R.5 Item K sub-item K.1c (2026-06-06) — parser
    /// disambiguates `[may]` / `[must]` / `[sharp]` as a modality
    /// attribute (NOT a labelled-index expression) when it appears
    /// directly after `label <name>`. Pre-K.1c this form was
    /// ambiguous and the parser would consume `[may]` as
    /// `label_name[may]` (an indexed label expression) — forcing
    /// authors to use the `on epsilon [may]` or
    /// `on label name effects { } [may]` workaround.
    #[test]
    fn r5_subitem_k1c_label_followed_by_modality_attribute_parses_correctly() {
        let src = r"
context k1c_label_modality {
    automata {
        automaton A {
            alphabet { label tick; }
            states { state S initial; state T; }
            transitions {
                transition S -> T on label tick [may];
            }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, super::TransitionModalitySpec::MayOnly);
        // The label is `tick`, NOT a label-with-index.
        match &t.label {
            super::TransitionLabel::Named { name, index } => {
                assert_eq!(name.name, "tick");
                assert!(
                    index.is_none(),
                    "K.1c disambiguation: `[may]` after `label name` is a modality, not an index"
                );
            }
            other => panic!("expected Named label, got {other:?}"),
        }
    }

    /// R.5 Item K sub-item K.1c — `[must]` after `label name`
    /// also disambiguates as modality.
    #[test]
    fn r5_subitem_k1c_label_followed_by_must_attribute_parses_correctly() {
        let src = r"
context k1c_label_must {
    automata {
        automaton A {
            alphabet { label tick; }
            states { state S initial; state T; }
            transitions {
                transition S -> T on label tick [must];
            }
        }
    }
}
";
        let t = first_transition(src);
        assert_eq!(t.modality, super::TransitionModalitySpec::MustOnly);
    }

    /// R.5 Item K sub-item K.1c — indexed-label syntax
    /// `label name[<integer>]` continues to parse as a label with
    /// an integer index, NOT as a modality attribute. The
    /// disambiguation only fires for the 3 modality keywords.
    #[test]
    fn r5_subitem_k1c_integer_index_after_label_still_parses_as_index() {
        let src = r"
context k1c_integer_index {
    alphabet { label channel; }
    automata {
        automaton A {
            alphabet { label channel; }
            states { state S initial; state T; }
            transitions {
                transition S -> T on label channel[3];
            }
        }
    }
}
";
        let t = first_transition(src);
        // `[3]` is consumed as the label's index expression
        // (K.1c does NOT disambiguate integer indices).
        match &t.label {
            super::TransitionLabel::Named { name, index } => {
                assert_eq!(name.name, "channel");
                assert!(
                    index.is_some(),
                    "integer index `[3]` should still parse as an index expression"
                );
            }
            other => panic!("expected Named label, got {other:?}"),
        }
        // Modality defaults to Sharp (no attribute).
        assert_eq!(t.modality, super::TransitionModalitySpec::Sharp);
    }
}
