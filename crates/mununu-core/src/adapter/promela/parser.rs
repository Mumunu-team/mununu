//! Promela bounded-subset parser.
//!
//! Parses the supported Promela subset into an AST ([`Program`]).
//! Unsupported constructs (embedded C, `_last`, `_nr_pr`) are rejected.

use super::ast::*;
use crate::adapter::{AdapterError, AdapterErrorKind, SourceLocation};

/// Parse a Promela program from text.
pub fn parse(content: &str) -> Result<Program, AdapterError> {
    let mut parser = PromelaParser::new(content);
    parser.parse_program()
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    // Delimiters
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Semi,
    Comma,
    Dot,
    Colon,
    ColonColon,
    Arrow,
    // Operators
    Assign,
    Bang,
    Recv,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    AmpAmp,
    PipePipe,
    Amp,
    Pipe,
    Caret,
    Tilde,
    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,
    Shl,
    Shr,
    // Keywords
    Active,
    Proctype,
    Init,
    If,
    Fi,
    Do,
    Od,
    Atomic,
    DStep,
    Goto,
    Break,
    Skip,
    Assert,
    Printf,
    Provided,
    Chan,
    Of,
    Mtype,
    Typedef,
    Ltl,
    Inline,
    Unless,
    DProctype,
    Timeout,
    Trace,
    Notrace,
    At, // '@' for remote references (process@label)
    // Type keywords
    Bit,
    Bool,
    Byte,
    Short,
    Int,
    // LTL operators
    Always,
    Eventually,
    Next,
    Until,
    WeakUntil,
    Release,
    Iff,
    // Literals
    True,
    False,
    IntLit(i64),
    StringLit(String),
    // Identifier
    Ident(String),
    Eof,
}

/// A token prints as its Promela lexeme, so a parse error names what the user wrote
/// (mununu#1121): punctuation, literals and identifiers in backticks, a keyword as itself,
/// the end of the file as "end of input".
impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (lexeme, quoted) = match self {
            Token::LBrace => ("{", true),
            Token::RBrace => ("}", true),
            Token::LBracket => ("[", true),
            Token::RBracket => ("]", true),
            Token::LParen => ("(", true),
            Token::RParen => (")", true),
            Token::Semi => (";", true),
            Token::Comma => (",", true),
            Token::Dot => (".", true),
            Token::Colon => (":", true),
            Token::ColonColon => ("::", true),
            Token::Arrow => ("->", true),
            Token::Assign => ("=", true),
            Token::Bang => ("!", true),
            Token::Recv => ("?", true),
            Token::Plus => ("+", true),
            Token::Minus => ("-", true),
            Token::Star => ("*", true),
            Token::Slash => ("/", true),
            Token::Percent => ("%", true),
            Token::AmpAmp => ("&&", true),
            Token::PipePipe => ("||", true),
            Token::Amp => ("&", true),
            Token::Pipe => ("|", true),
            Token::Caret => ("^", true),
            Token::Tilde => ("~", true),
            Token::EqEq => ("==", true),
            Token::BangEq => ("!=", true),
            Token::Lt => ("<", true),
            Token::Le => ("<=", true),
            Token::Gt => (">", true),
            Token::Ge => (">=", true),
            Token::Shl => ("<<", true),
            Token::Shr => (">>", true),
            Token::At => ("@", true),
            Token::Always => ("[]", true),
            Token::Eventually => ("<>", true),
            Token::Iff => ("<->", true),
            Token::Active => ("active", false),
            Token::Proctype => ("proctype", false),
            Token::Init => ("init", false),
            Token::If => ("if", false),
            Token::Fi => ("fi", false),
            Token::Do => ("do", false),
            Token::Od => ("od", false),
            Token::Atomic => ("atomic", false),
            Token::DStep => ("d_step", false),
            Token::Goto => ("goto", false),
            Token::Break => ("break", false),
            Token::Skip => ("skip", false),
            Token::Assert => ("assert", false),
            Token::Printf => ("printf", false),
            Token::Provided => ("provided", false),
            Token::Chan => ("chan", false),
            Token::Of => ("of", false),
            Token::Mtype => ("mtype", false),
            Token::Typedef => ("typedef", false),
            Token::Ltl => ("ltl", false),
            Token::Inline => ("inline", false),
            Token::Unless => ("unless", false),
            Token::DProctype => ("d_proctype", false),
            Token::Timeout => ("timeout", false),
            Token::Trace => ("trace", false),
            Token::Notrace => ("notrace", false),
            Token::Bit => ("bit", false),
            Token::Bool => ("bool", false),
            Token::Byte => ("byte", false),
            Token::Short => ("short", false),
            Token::Int => ("int", false),
            Token::Next => ("X", false),
            Token::Until => ("U", false),
            Token::WeakUntil => ("W", false),
            Token::Release => ("R", false),
            Token::True => ("true", false),
            Token::False => ("false", false),
            Token::IntLit(n) => return write!(f, "`{n}`"),
            Token::StringLit(s) => return write!(f, "`\"{s}\"`"),
            Token::Ident(s) => return write!(f, "`{s}`"),
            Token::Eof => return f.write_str("end of input"),
        };
        if quoted {
            write!(f, "`{lexeme}`")
        } else {
            f.write_str(lexeme)
        }
    }
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

    fn skip_ws_and_comments(&mut self) {
        loop {
            while self.peek_char().is_some_and(|c| c.is_whitespace()) {
                self.advance();
            }
            if self.input[self.pos..].starts_with("//") {
                while self.peek_char().is_some_and(|c| c != '\n') {
                    self.advance();
                }
                continue;
            }
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
        self.skip_ws_and_comments();
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
            '[' => {
                self.advance();
                if self.peek_char() == Some(']') {
                    self.advance();
                    Ok(Token::Always)
                } else {
                    Ok(Token::LBracket)
                }
            }
            ']' => {
                self.advance();
                Ok(Token::RBracket)
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
            '.' => {
                self.advance();
                Ok(Token::Dot)
            }
            '+' => {
                self.advance();
                Ok(Token::Plus)
            }
            '*' => {
                self.advance();
                Ok(Token::Star)
            }
            '/' => {
                self.advance();
                Ok(Token::Slash)
            }
            '%' => {
                self.advance();
                Ok(Token::Percent)
            }
            '^' => {
                self.advance();
                Ok(Token::Caret)
            }
            '~' => {
                self.advance();
                Ok(Token::Tilde)
            }
            ':' => {
                self.advance();
                if self.peek_char() == Some(':') {
                    self.advance();
                    Ok(Token::ColonColon)
                } else {
                    Ok(Token::Colon)
                }
            }
            '!' => {
                self.advance();
                if self.peek_char() == Some('=') {
                    self.advance();
                    Ok(Token::BangEq)
                } else {
                    Ok(Token::Bang)
                }
            }
            '?' => {
                self.advance();
                Ok(Token::Recv)
            }
            '=' => {
                self.advance();
                if self.peek_char() == Some('=') {
                    self.advance();
                    Ok(Token::EqEq)
                } else {
                    Ok(Token::Assign)
                }
            }
            '&' => {
                self.advance();
                if self.peek_char() == Some('&') {
                    self.advance();
                    Ok(Token::AmpAmp)
                } else {
                    Ok(Token::Amp)
                }
            }
            '|' => {
                self.advance();
                if self.peek_char() == Some('|') {
                    self.advance();
                    Ok(Token::PipePipe)
                } else {
                    Ok(Token::Pipe)
                }
            }
            '<' => {
                self.advance();
                if self.peek_char() == Some('=') {
                    self.advance();
                    Ok(Token::Le)
                } else if self.peek_char() == Some('<') {
                    self.advance();
                    Ok(Token::Shl)
                } else if self.peek_char() == Some('-') {
                    self.advance();
                    if self.peek_char() == Some('>') {
                        self.advance();
                        Ok(Token::Iff)
                    } else {
                        Err(self.err("Expected '>' after '<-'"))
                    }
                } else if self.peek_char() == Some('>') {
                    self.advance();
                    Ok(Token::Eventually)
                } else {
                    Ok(Token::Lt)
                }
            }
            '>' => {
                self.advance();
                if self.peek_char() == Some('=') {
                    self.advance();
                    Ok(Token::Ge)
                } else if self.peek_char() == Some('>') {
                    self.advance();
                    Ok(Token::Shr)
                } else {
                    Ok(Token::Gt)
                }
            }
            '-' => {
                self.advance();
                if self.peek_char() == Some('>') {
                    self.advance();
                    Ok(Token::Arrow)
                } else {
                    Ok(Token::Minus)
                }
            }
            '"' => {
                self.advance();
                let mut s = String::new();
                while self.peek_char().is_some_and(|c| c != '"') {
                    let ch = self.advance().unwrap();
                    if ch == '\\' {
                        if let Some(esc) = self.advance() {
                            match esc {
                                'n' => s.push('\n'),
                                't' => s.push('\t'),
                                '"' => s.push('"'),
                                '\\' => s.push('\\'),
                                _ => {
                                    s.push('\\');
                                    s.push(esc);
                                }
                            }
                        }
                    } else {
                        s.push(ch);
                    }
                }
                if self.peek_char() == Some('"') {
                    self.advance();
                }
                Ok(Token::StringLit(s))
            }
            '@' => {
                self.advance();
                Ok(Token::At)
            }
            _ if c.is_ascii_digit() => {
                let mut num = String::new();
                if c == '0' {
                    num.push(self.advance().unwrap());
                    if self.peek_char() == Some('x') || self.peek_char() == Some('X') {
                        num.push(self.advance().unwrap());
                        while self.peek_char().is_some_and(|c| c.is_ascii_hexdigit()) {
                            num.push(self.advance().unwrap());
                        }
                        return Ok(Token::IntLit(
                            i64::from_str_radix(&num[2..], 16).unwrap_or(0),
                        ));
                    }
                }
                while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                    num.push(self.advance().unwrap());
                }
                Ok(Token::IntLit(num.parse().unwrap_or(0)))
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
                    "active" => Token::Active,
                    "proctype" => Token::Proctype,
                    "init" => Token::Init,
                    "if" => Token::If,
                    "fi" => Token::Fi,
                    "do" => Token::Do,
                    "od" => Token::Od,
                    "atomic" => Token::Atomic,
                    "d_step" => Token::DStep,
                    "goto" => Token::Goto,
                    "break" => Token::Break,
                    "skip" => Token::Skip,
                    "assert" => Token::Assert,
                    "printf" => Token::Printf,
                    "provided" => Token::Provided,
                    "chan" => Token::Chan,
                    "of" => Token::Of,
                    "mtype" => Token::Mtype,
                    "typedef" => Token::Typedef,
                    "ltl" => Token::Ltl,
                    "inline" => Token::Inline,
                    "unless" => Token::Unless,
                    "d_proctype" => Token::DProctype,
                    "timeout" => Token::Timeout,
                    "trace" => Token::Trace,
                    "notrace" => Token::Notrace,
                    "bit" => Token::Bit,
                    "bool" => Token::Bool,
                    "byte" => Token::Byte,
                    "short" => Token::Short,
                    "int" => Token::Int,
                    "true" => Token::True,
                    "false" => Token::False,
                    "U" => Token::Until,
                    "W" => Token::WeakUntil,
                    "V" | "R" => Token::Release,
                    "X" => Token::Next,
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

struct PromelaParser<'a> {
    lexer: Lexer<'a>,
    current: Token,
}

impl<'a> PromelaParser<'a> {
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
                .err(&format!("Expected {expected}, got {}", self.current)))
        }
    }

    fn eat_semis(&mut self) -> Result<(), AdapterError> {
        while self.current == Token::Semi {
            self.advance()?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Top-level program
    // -----------------------------------------------------------------------

    fn parse_program(&mut self) -> Result<Program, AdapterError> {
        let mut program = Program {
            mtypes: vec![],
            globals: vec![],
            channels: vec![],
            proctypes: vec![],
            init: None,
            ltl_properties: vec![],
            inlines: vec![],
            traces: vec![],
            notraces: vec![],
        };

        while self.current != Token::Eof {
            self.eat_semis()?;
            if self.current == Token::Eof {
                break;
            }

            match &self.current {
                Token::Mtype => {
                    let mtype = self.parse_mtype()?;
                    program.mtypes.push(mtype);
                }
                Token::Active => {
                    let proc = self.parse_proctype()?;
                    program.proctypes.push(proc);
                }
                Token::Proctype | Token::DProctype => {
                    let proc = self.parse_proctype()?;
                    program.proctypes.push(proc);
                }
                Token::Init => {
                    self.advance()?;
                    self.expect(&Token::LBrace)?;
                    let body = self.parse_sequence()?;
                    self.expect(&Token::RBrace)?;
                    program.init = Some(body);
                }
                Token::Ltl => {
                    let ltl = self.parse_ltl_block()?;
                    program.ltl_properties.push(ltl);
                }
                Token::Chan => {
                    let chan = self.parse_chan_decl()?;
                    program.channels.push(chan);
                }
                Token::Inline => {
                    let inline_def = self.parse_inline_def()?;
                    program.inlines.push(inline_def);
                }
                Token::Typedef => {
                    // Skip typedef for now
                    self.advance()?;
                    if let Token::Ident(_) = &self.current {
                        self.advance()?;
                    }
                    self.expect(&Token::LBrace)?;
                    self.skip_braces()?;
                }
                Token::Trace => {
                    self.advance()?;
                    self.expect(&Token::LBrace)?;
                    let body = self.parse_sequence()?;
                    self.expect(&Token::RBrace)?;
                    program.traces.push(body);
                }
                Token::Notrace => {
                    self.advance()?;
                    self.expect(&Token::LBrace)?;
                    let body = self.parse_sequence()?;
                    self.expect(&Token::RBrace)?;
                    program.notraces.push(body);
                }
                Token::Bit
                | Token::Bool
                | Token::Byte
                | Token::Short
                | Token::Int
                | Token::Ident(_) => {
                    let decl = self.parse_var_decl()?;
                    program.globals.push(decl);
                    if self.current == Token::Semi {
                        self.advance()?;
                    }
                }
                _ => {
                    // Skip unknown top-level tokens
                    self.advance()?;
                }
            }
        }

        Ok(program)
    }

    // -----------------------------------------------------------------------
    // Declarations
    // -----------------------------------------------------------------------

    fn parse_mtype(&mut self) -> Result<MtypeDecl, AdapterError> {
        self.expect(&Token::Mtype)?;
        if self.current == Token::Assign {
            self.advance()?;
        }
        self.expect(&Token::LBrace)?;
        let mut values = Vec::new();
        while self.current != Token::RBrace && self.current != Token::Eof {
            if let Token::Ident(name) = &self.current {
                values.push(name.clone());
                self.advance()?;
            } else {
                self.advance()?;
            }
            if self.current == Token::Comma {
                self.advance()?;
            }
        }
        self.expect(&Token::RBrace)?;
        if self.current == Token::Semi {
            self.advance()?;
        }
        Ok(MtypeDecl { values })
    }

    fn parse_typename(&mut self) -> Result<TypeName, AdapterError> {
        let tn = match &self.current {
            Token::Bit => TypeName::Bit,
            Token::Bool => TypeName::Bool,
            Token::Byte => TypeName::Byte,
            Token::Short => TypeName::Short,
            Token::Int => TypeName::Int,
            Token::Mtype => TypeName::Mtype,
            Token::Chan => TypeName::Chan,
            Token::Ident(name) => TypeName::UserDefined(name.clone()),
            _ => {
                return Err(self
                    .lexer
                    .err(&format!("Expected type name, got {}", self.current)));
            }
        };
        self.advance()?;
        Ok(tn)
    }

    fn parse_var_decl(&mut self) -> Result<VarDecl, AdapterError> {
        let typename = self.parse_typename()?;
        let name = match &self.current {
            Token::Ident(n) => n.clone(),
            _ => return Err(self.lexer.err("Expected variable name")),
        };
        self.advance()?;

        let array_size = if self.current == Token::LBracket {
            self.advance()?;
            let size = self.parse_const_expr()?;
            self.expect(&Token::RBracket)?;
            Some(size as usize)
        } else {
            None
        };

        let init = if self.current == Token::Assign {
            self.advance()?;
            Some(self.parse_expr()?)
        } else {
            None
        };

        Ok(VarDecl {
            typename,
            name,
            array_size,
            init,
        })
    }

    fn parse_chan_decl(&mut self) -> Result<ChanDecl, AdapterError> {
        self.expect(&Token::Chan)?;
        let name = match &self.current {
            Token::Ident(n) => n.clone(),
            _ => return Err(self.lexer.err("Expected channel name")),
        };
        self.advance()?;
        self.expect(&Token::Assign)?;
        self.expect(&Token::LBracket)?;
        let capacity = self.parse_const_expr()? as usize;
        self.expect(&Token::RBracket)?;
        self.expect(&Token::Of)?;
        self.expect(&Token::LBrace)?;
        let mut message_types = Vec::new();
        while self.current != Token::RBrace && self.current != Token::Eof {
            message_types.push(self.parse_typename()?);
            if self.current == Token::Comma {
                self.advance()?;
            }
        }
        self.expect(&Token::RBrace)?;
        if self.current == Token::Semi {
            self.advance()?;
        }
        Ok(ChanDecl {
            name,
            capacity,
            message_types,
        })
    }

    fn parse_proctype(&mut self) -> Result<Proctype, AdapterError> {
        let mut active_count = 0;
        if self.current == Token::Active {
            self.advance()?;
            active_count = 1;
            if self.current == Token::LBracket {
                self.advance()?;
                active_count = self.parse_const_expr()? as usize;
                self.expect(&Token::RBracket)?;
            }
        }
        let deterministic = self.current == Token::DProctype;
        if deterministic {
            self.advance()?;
        } else {
            self.expect(&Token::Proctype)?;
        }
        let name = match &self.current {
            Token::Ident(n) => n.clone(),
            _ => return Err(self.lexer.err("Expected proctype name")),
        };
        self.advance()?;
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        while self.current != Token::RParen && self.current != Token::Eof {
            params.push(self.parse_var_decl()?);
            if self.current == Token::Semi {
                self.advance()?;
            }
        }
        self.expect(&Token::RParen)?;

        let provided = if self.current == Token::Provided {
            self.advance()?;
            self.expect(&Token::LParen)?;
            let expr = self.parse_expr()?;
            self.expect(&Token::RParen)?;
            Some(expr)
        } else {
            None
        };

        self.expect(&Token::LBrace)?;
        let body = self.parse_sequence()?;
        self.expect(&Token::RBrace)?;

        Ok(Proctype {
            name,
            active_count,
            params,
            provided,
            deterministic,
            body,
        })
    }

    // -----------------------------------------------------------------------
    // Sequences and statements
    // -----------------------------------------------------------------------

    fn parse_sequence(&mut self) -> Result<Sequence, AdapterError> {
        let mut steps = Vec::new();
        self.eat_semis()?;
        while !matches!(
            self.current,
            Token::RBrace | Token::Fi | Token::Od | Token::Eof
        ) {
            // Check for local variable declaration
            if matches!(
                self.current,
                Token::Bit | Token::Bool | Token::Byte | Token::Short | Token::Int | Token::Mtype
            ) {
                let decl = self.parse_var_decl()?;
                steps.push(Step::Decl(decl));
            } else {
                let stmt = self.parse_statement()?;
                steps.push(Step::Statement(stmt));
            }
            // Consume optional semicolons and arrows (-> is sequence separator in Promela)
            while matches!(self.current, Token::Semi | Token::Arrow) {
                self.advance()?;
            }
        }
        Ok(steps)
    }

    fn parse_statement(&mut self) -> Result<Statement, AdapterError> {
        let stmt = self.parse_statement_inner()?;

        // Check for `unless { escape }` postfix
        if self.current == Token::Unless {
            self.advance()?;
            self.expect(&Token::LBrace)?;
            let escape = self.parse_sequence()?;
            self.expect(&Token::RBrace)?;
            return Ok(Statement::Unless {
                body: Box::new(stmt),
                escape,
            });
        }

        Ok(stmt)
    }

    fn parse_statement_inner(&mut self) -> Result<Statement, AdapterError> {
        // Check for label: stmt
        if let Token::Ident(name) = &self.current {
            let name = name.clone();
            // Peek ahead for colon (label)
            // We need to be careful: ident could be varref = expr, or label: stmt, or guard
            // Heuristic: if next is ':', it's a label. Otherwise treat as expr/assign.
            let saved_pos = self.lexer.pos;
            let saved_line = self.lexer.line;
            let saved_col = self.lexer.col;
            let saved_current = self.current.clone();

            self.advance()?;
            if self.current == Token::Colon {
                self.advance()?;
                let stmt = self.parse_statement_inner()?;
                return Ok(Statement::Label {
                    name,
                    stmt: Box::new(stmt),
                });
            }
            // Not a label — restore and parse as assignment or expr
            self.lexer.pos = saved_pos;
            self.lexer.line = saved_line;
            self.lexer.col = saved_col;
            self.current = saved_current;
        }

        match &self.current {
            Token::If => self.parse_if(),
            Token::Do => self.parse_do(),
            Token::Atomic => {
                self.advance()?;
                self.expect(&Token::LBrace)?;
                let body = self.parse_sequence()?;
                self.expect(&Token::RBrace)?;
                Ok(Statement::Atomic { body })
            }
            Token::DStep => {
                self.advance()?;
                self.expect(&Token::LBrace)?;
                let body = self.parse_sequence()?;
                self.expect(&Token::RBrace)?;
                Ok(Statement::DStep { body })
            }
            Token::LBrace => {
                self.advance()?;
                let body = self.parse_sequence()?;
                self.expect(&Token::RBrace)?;
                Ok(Statement::Block { body })
            }
            Token::Goto => {
                self.advance()?;
                let label = match &self.current {
                    Token::Ident(n) => n.clone(),
                    _ => return Err(self.lexer.err("Expected label name after goto")),
                };
                self.advance()?;
                Ok(Statement::Goto { label })
            }
            Token::Break => {
                self.advance()?;
                Ok(Statement::Break)
            }
            Token::Skip => {
                self.advance()?;
                Ok(Statement::Skip)
            }
            Token::Assert => {
                self.advance()?;
                self.expect(&Token::LParen)?;
                let expr = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(Statement::Assert { expr })
            }
            Token::Printf => {
                self.advance()?;
                self.expect(&Token::LParen)?;
                let format = match &self.current {
                    Token::StringLit(s) => {
                        let s = s.clone();
                        self.advance()?;
                        s
                    }
                    _ => String::new(),
                };
                let mut args = Vec::new();
                while self.current == Token::Comma {
                    self.advance()?;
                    args.push(self.parse_expr()?);
                }
                self.expect(&Token::RParen)?;
                Ok(Statement::Printf { format, args })
            }
            _ => {
                // Assignment or expression-as-statement
                let expr = self.parse_expr()?;
                if self.current == Token::Assign {
                    // It's an assignment: expr = value
                    let target = self.expr_to_varref(&expr)?;
                    self.advance()?;
                    let value = self.parse_expr()?;
                    Ok(Statement::Assign { target, value })
                } else if self.current == Token::Bang {
                    // Channel send: ch ! args
                    let channel = self.expr_to_name(&expr)?;
                    self.advance()?;
                    let mut args = vec![self.parse_expr()?];
                    while self.current == Token::Comma {
                        self.advance()?;
                        args.push(self.parse_expr()?);
                    }
                    Ok(Statement::Send { channel, args })
                } else if self.current == Token::Recv {
                    // Channel recv: ch ? args
                    let channel = self.expr_to_name(&expr)?;
                    self.advance()?;
                    let mut args = vec![self.parse_recv_arg()?];
                    while self.current == Token::Comma {
                        self.advance()?;
                        args.push(self.parse_recv_arg()?);
                    }
                    Ok(Statement::Recv { channel, args })
                } else {
                    // Expression as guard/condition
                    Ok(Statement::ExprStmt { expr })
                }
            }
        }
    }

    fn parse_if(&mut self) -> Result<Statement, AdapterError> {
        self.expect(&Token::If)?;
        let mut options = Vec::new();
        while self.current == Token::ColonColon {
            self.advance()?;
            let seq = self.parse_sequence()?;
            options.push(seq);
        }
        self.expect(&Token::Fi)?;
        Ok(Statement::If { options })
    }

    fn parse_do(&mut self) -> Result<Statement, AdapterError> {
        self.expect(&Token::Do)?;
        let mut options = Vec::new();
        while self.current == Token::ColonColon {
            self.advance()?;
            let seq = self.parse_sequence()?;
            options.push(seq);
        }
        self.expect(&Token::Od)?;
        Ok(Statement::Do { options })
    }

    fn parse_recv_arg(&mut self) -> Result<RecvArg, AdapterError> {
        if let Token::Ident(name) = &self.current {
            let vr = VarRef {
                name: name.clone(),
                index: None,
                field: None,
            };
            self.advance()?;
            Ok(RecvArg::Var(vr))
        } else {
            let expr = self.parse_expr()?;
            Ok(RecvArg::Const(expr))
        }
    }

    // -----------------------------------------------------------------------
    // Expressions
    // -----------------------------------------------------------------------

    fn parse_expr(&mut self) -> Result<Expr, AdapterError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, AdapterError> {
        let mut left = self.parse_and()?;
        while self.current == Token::PipePipe {
            self.advance()?;
            let right = self.parse_and()?;
            left = Expr::BinOp {
                op: BinOp::Or,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, AdapterError> {
        let mut left = self.parse_comparison()?;
        while self.current == Token::AmpAmp {
            self.advance()?;
            let right = self.parse_comparison()?;
            left = Expr::BinOp {
                op: BinOp::And,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, AdapterError> {
        let left = self.parse_additive()?;
        let op = match &self.current {
            Token::EqEq => BinOp::Eq,
            Token::BangEq => BinOp::Ne,
            Token::Lt => BinOp::Lt,
            Token::Le => BinOp::Le,
            Token::Gt => BinOp::Gt,
            Token::Ge => BinOp::Ge,
            _ => return Ok(left),
        };
        self.advance()?;
        let right = self.parse_additive()?;
        Ok(Expr::BinOp {
            op,
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    fn parse_additive(&mut self) -> Result<Expr, AdapterError> {
        let mut left = self.parse_multiplicative()?;
        while matches!(self.current, Token::Plus | Token::Minus) {
            let op = if self.current == Token::Plus {
                BinOp::Add
            } else {
                BinOp::Sub
            };
            self.advance()?;
            let right = self.parse_multiplicative()?;
            left = Expr::BinOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, AdapterError> {
        let mut left = self.parse_unary()?;
        while matches!(self.current, Token::Star | Token::Slash | Token::Percent) {
            let op = match &self.current {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => BinOp::Mod,
            };
            self.advance()?;
            let right = self.parse_unary()?;
            left = Expr::BinOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, AdapterError> {
        match &self.current {
            Token::Bang => {
                self.advance()?;
                let operand = self.parse_unary()?;
                Ok(Expr::UnOp {
                    op: UnOp::Not,
                    operand: Box::new(operand),
                })
            }
            Token::Minus => {
                self.advance()?;
                let operand = self.parse_unary()?;
                Ok(Expr::UnOp {
                    op: UnOp::Neg,
                    operand: Box::new(operand),
                })
            }
            Token::Tilde => {
                self.advance()?;
                let operand = self.parse_unary()?;
                Ok(Expr::UnOp {
                    op: UnOp::BitNot,
                    operand: Box::new(operand),
                })
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, AdapterError> {
        match &self.current {
            Token::IntLit(n) => {
                let n = *n;
                self.advance()?;
                Ok(Expr::IntLit(n))
            }
            Token::True => {
                self.advance()?;
                Ok(Expr::BoolLit(true))
            }
            Token::False => {
                self.advance()?;
                Ok(Expr::BoolLit(false))
            }
            Token::LParen => {
                self.advance()?;
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(Expr::Paren(Box::new(inner)))
            }
            Token::Timeout => {
                self.advance()?;
                Ok(Expr::Timeout)
            }
            Token::Ident(name) => {
                let name = name.clone();
                self.advance()?;

                // Check for remote reference: process@label
                if self.current == Token::At {
                    self.advance()?;
                    let label = match &self.current {
                        Token::Ident(l) => l.clone(),
                        _ => return Err(self.lexer.err("Expected label after '@'")),
                    };
                    self.advance()?;
                    return Ok(Expr::RemoteRef {
                        process: name,
                        label,
                    });
                }

                // Check for remote variable: process:var (only if not a label context)
                // Note: ':' is ambiguous with label syntax; we only parse it here
                // when the identifier is followed by ':' and then another identifier
                // and it's NOT in a label context (labels are handled in parse_statement_inner).

                let index = if self.current == Token::LBracket {
                    self.advance()?;
                    let idx = self.parse_expr()?;
                    self.expect(&Token::RBracket)?;
                    Some(Box::new(idx))
                } else {
                    None
                };
                Ok(Expr::VarRef(VarRef {
                    name,
                    index,
                    field: None,
                }))
            }
            _ => Err(self
                .lexer
                .err(&format!("Expected expression, got {}", self.current))),
        }
    }

    fn parse_const_expr(&mut self) -> Result<i64, AdapterError> {
        match &self.current {
            Token::IntLit(n) => {
                let n = *n;
                self.advance()?;
                Ok(n)
            }
            _ => Err(self.lexer.err("Expected constant expression")),
        }
    }

    // -----------------------------------------------------------------------
    // LTL
    // -----------------------------------------------------------------------

    fn parse_ltl_block(&mut self) -> Result<LtlProperty, AdapterError> {
        self.expect(&Token::Ltl)?;
        let name = if let Token::Ident(n) = &self.current {
            let n = n.clone();
            self.advance()?;
            Some(n)
        } else {
            None
        };
        self.expect(&Token::LBrace)?;
        let formula = self.parse_ltl_expr()?;
        self.expect(&Token::RBrace)?;
        Ok(LtlProperty { name, formula })
    }

    fn parse_ltl_expr(&mut self) -> Result<LtlExpr, AdapterError> {
        self.parse_ltl_implies()
    }

    fn parse_ltl_implies(&mut self) -> Result<LtlExpr, AdapterError> {
        let left = self.parse_ltl_or()?;
        if self.current == Token::Arrow {
            self.advance()?;
            let right = self.parse_ltl_implies()?;
            Ok(LtlExpr::Implies(Box::new(left), Box::new(right)))
        } else if self.current == Token::Iff {
            self.advance()?;
            let right = self.parse_ltl_implies()?;
            Ok(LtlExpr::Iff(Box::new(left), Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn parse_ltl_or(&mut self) -> Result<LtlExpr, AdapterError> {
        let mut left = self.parse_ltl_and()?;
        while self.current == Token::PipePipe {
            self.advance()?;
            let right = self.parse_ltl_and()?;
            left = LtlExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_ltl_and(&mut self) -> Result<LtlExpr, AdapterError> {
        let mut left = self.parse_ltl_binary_temporal()?;
        while self.current == Token::AmpAmp {
            self.advance()?;
            let right = self.parse_ltl_binary_temporal()?;
            left = LtlExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_ltl_binary_temporal(&mut self) -> Result<LtlExpr, AdapterError> {
        let left = self.parse_ltl_unary()?;
        match &self.current {
            Token::Until => {
                self.advance()?;
                let r = self.parse_ltl_unary()?;
                Ok(LtlExpr::Until(Box::new(left), Box::new(r)))
            }
            Token::WeakUntil => {
                self.advance()?;
                let r = self.parse_ltl_unary()?;
                Ok(LtlExpr::WeakUntil(Box::new(left), Box::new(r)))
            }
            Token::Release => {
                self.advance()?;
                let r = self.parse_ltl_unary()?;
                Ok(LtlExpr::Release(Box::new(left), Box::new(r)))
            }
            _ => Ok(left),
        }
    }

    fn parse_ltl_unary(&mut self) -> Result<LtlExpr, AdapterError> {
        match &self.current {
            Token::Bang => {
                self.advance()?;
                let inner = self.parse_ltl_unary()?;
                Ok(LtlExpr::Not(Box::new(inner)))
            }
            Token::Always => {
                self.advance()?;
                let inner = self.parse_ltl_unary()?;
                Ok(LtlExpr::Always(Box::new(inner)))
            }
            Token::Eventually => {
                self.advance()?;
                let inner = self.parse_ltl_unary()?;
                Ok(LtlExpr::Eventually(Box::new(inner)))
            }
            Token::Next => {
                self.advance()?;
                let inner = self.parse_ltl_unary()?;
                Ok(LtlExpr::Next(Box::new(inner)))
            }
            _ => self.parse_ltl_atom(),
        }
    }

    fn parse_ltl_atom(&mut self) -> Result<LtlExpr, AdapterError> {
        match &self.current {
            Token::True => {
                self.advance()?;
                Ok(LtlExpr::True)
            }
            Token::False => {
                self.advance()?;
                Ok(LtlExpr::False)
            }
            Token::Ident(name) => {
                let n = name.clone();
                self.advance()?;
                Ok(LtlExpr::Predicate(n))
            }
            Token::LParen => {
                self.advance()?;
                let inner = self.parse_ltl_expr()?;
                self.expect(&Token::RParen)?;
                Ok(inner)
            }
            _ => Err(self
                .lexer
                .err(&format!("Expected LTL atom, got {}", self.current))),
        }
    }

    // -----------------------------------------------------------------------
    // Inline definitions
    // -----------------------------------------------------------------------

    fn parse_inline_def(&mut self) -> Result<InlineDef, AdapterError> {
        self.expect(&Token::Inline)?;
        let name = match &self.current {
            Token::Ident(n) => n.clone(),
            _ => return Err(self.lexer.err("Expected inline name")),
        };
        self.advance()?;
        self.expect(&Token::LParen)?;

        let mut params = Vec::new();
        while self.current != Token::RParen && self.current != Token::Eof {
            if let Token::Ident(p) = &self.current {
                params.push(p.clone());
                self.advance()?;
            } else {
                self.advance()?;
            }
            if self.current == Token::Comma || self.current == Token::Semi {
                self.advance()?;
            }
        }
        self.expect(&Token::RParen)?;
        self.expect(&Token::LBrace)?;
        let body = self.parse_sequence()?;
        self.expect(&Token::RBrace)?;

        Ok(InlineDef { name, params, body })
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn expr_to_varref(&self, expr: &Expr) -> Result<VarRef, AdapterError> {
        match expr {
            Expr::VarRef(vr) => Ok(vr.clone()),
            _ => Err(self
                .lexer
                .err("Expected variable reference on left side of assignment")),
        }
    }

    fn expr_to_name(&self, expr: &Expr) -> Result<String, AdapterError> {
        match expr {
            Expr::VarRef(vr) => Ok(vr.name.clone()),
            _ => Err(self.lexer.err("Expected channel name")),
        }
    }

    fn skip_braces(&mut self) -> Result<(), AdapterError> {
        let mut depth = 1;
        while depth > 0 && self.current != Token::Eof {
            if self.current == Token::LBrace {
                depth += 1;
            }
            if self.current == Token::RBrace {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_peterson() {
        let input = r#"
byte turn = 0;
bool flag0 = false;
bool flag1 = false;
bool cs0 = false;
bool cs1 = false;

active proctype P0() {
    do
    :: true ->
        flag0 = true;
        turn = 1;
        (flag1 == false || turn == 0);
        cs0 = true;
        cs0 = false;
        flag0 = false;
    od
}

active proctype P1() {
    do
    :: true ->
        flag1 = true;
        turn = 0;
        (flag0 == false || turn == 1);
        cs1 = true;
        cs1 = false;
        flag1 = false;
    od
}

ltl mutex { [] !(cs0 && cs1) }
"#;
        let program = parse(input).unwrap();
        assert_eq!(program.globals.len(), 5);
        assert_eq!(program.proctypes.len(), 2);
        assert_eq!(program.proctypes[0].name, "P0");
        assert_eq!(program.proctypes[0].active_count, 1);
        assert_eq!(program.proctypes[1].name, "P1");
        assert_eq!(program.ltl_properties.len(), 1);
        assert_eq!(program.ltl_properties[0].name, Some("mutex".into()));
    }

    #[test]
    fn parse_channel_example() {
        let input = r#"
mtype = { msg, ack };
chan ch = [2] of { mtype };

active proctype Sender() {
    ch ! msg;
    ch ? ack;
}

active proctype Receiver() {
    mtype x;
    ch ? x;
    ch ! ack;
}
"#;
        let program = parse(input).unwrap();
        assert_eq!(program.mtypes.len(), 1);
        assert_eq!(program.mtypes[0].values, vec!["msg", "ack"]);
        assert_eq!(program.channels.len(), 1);
        assert_eq!(program.channels[0].name, "ch");
        assert_eq!(program.channels[0].capacity, 2);
        assert_eq!(program.proctypes.len(), 2);
    }

    /// mununu#1121 — a token prints as its lexeme and an `expect` mismatch names both sides
    /// that way, never as Rust variant names (`Expected Semi, got Ident("x")`).
    #[test]
    fn x1121_the_promela_token_displays_its_lexeme() {
        assert_eq!(Token::Semi.to_string(), "`;`");
        assert_eq!(Token::Proctype.to_string(), "proctype");
        assert_eq!(Token::Always.to_string(), "`[]`");
        assert_eq!(Token::IntLit(3).to_string(), "`3`");
        assert_eq!(Token::Ident("x".into()).to_string(), "`x`");
        assert_eq!(Token::Eof.to_string(), "end of input");
        let mut parser = PromelaParser::new("x");
        let err = parser
            .expect(&Token::Semi)
            .expect_err("an identifier is not `;`");
        assert_eq!(err.message, "Expected `;`, got `x`");
        let mut parser = PromelaParser::new("");
        let err = parser
            .expect(&Token::RBrace)
            .expect_err("the end of the file is not `}`");
        assert_eq!(err.message, "Expected `}`, got end of input");
    }
}
