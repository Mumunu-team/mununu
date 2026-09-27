use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub column: u32,
}

impl Span {
    pub const fn new(start: usize, end: usize, line: u32, column: u32) -> Self {
        Self {
            start,
            end,
            line,
            column,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Keyword {
    Context,
    Alphabet,
    Constants,
    Ranges,
    Automata,
    Controllers,
    Composition,
    MuFormulas,
    Label,
    Const,
    Range,
    Automaton,
    Controller,
    Meta,
    Id,
    Comment,
    Parameters,
    Param,
    In,
    Variables,
    Var,
    Controllable,
    Internal,
    States,
    State,
    StateGroups,
    Group,
    Predicates,
    Predicate,
    Wildcard,
    Initial,
    Vars,
    Valuations,
    Transitions,
    Transition,
    On,
    Epsilon,
    Guard,
    Effects,
    Source,
    Satisfying,
    Export,
    Members,
    Over,
    All,
    Formula,
    Body,
    Bool,
    I64,
    Synchronous,
    Asynchronous,
    Superset,
    Minimize,
    Diagnostics,
    Counterexample,
    DeadlockTraces,
    MaxCounterTraces,
    ProofObligations,
    True,
    False,
    // Per-state 3-valued (Kleene) predicate block: `predicates_3v { p = unknown; }`
    Predicates3v,
    // Enum types
    Enums, // "enums" section keyword
    Enum,  // "enum" declaration keyword
    // Formula syntax markers
    Ltl, // Explicit LTL syntax marker
    Mu,  // Explicit μ-calculus syntax marker (optional, for clarity)
         // Note: LTL operators (G, F, X, U, W, R) are not DSL keywords
         // because they can be used as identifiers. The LTL parser recognizes
         // them directly from raw strings.
}

impl Keyword {
    pub fn from_ident(ident: &str) -> Option<Self> {
        use Keyword::*;
        Some(match ident {
            "context" => Context,
            "alphabet" => Alphabet,
            "constants" => Constants,
            "ranges" => Ranges,
            "automata" => Automata,
            "controllers" => Controllers,
            "composition" => Composition,
            "mu_formulas" => MuFormulas,
            "label" => Label,
            "const" => Const,
            "range" => Range,
            "automaton" => Automaton,
            "controller" => Controller,
            "meta" => Meta,
            "id" => Id,
            "comment" => Comment,
            "parameters" => Parameters,
            "param" => Param,
            "in" => In,
            "variables" => Variables,
            "var" => Var,
            "controllable" => Controllable,
            "internal" => Internal,
            "states" => States,
            "state" => State,
            "state_groups" => StateGroups,
            "group" => Group,
            "predicates" => Predicates,
            "predicate" => Predicate,
            "wildcard" => Wildcard,
            "initial" => Initial,
            "vars" => Vars,
            "valuations" => Valuations,
            "predicates_3v" => Predicates3v,
            "transitions" => Transitions,
            "transition" => Transition,
            "on" => On,
            "epsilon" => Epsilon,
            "guard" => Guard,
            "effects" => Effects,
            "source" => Source,
            "satisfying" => Satisfying,
            "export" => Export,
            "members" => Members,
            "over" => Over,
            "all" => All,
            "formula" => Formula,
            "body" => Body,
            "bool" => Bool,
            "i64" => I64,
            "synchronous" => Synchronous,
            "asynchronous" => Asynchronous,
            "superset" => Superset,
            "minimize" => Minimize,
            "diagnostics" => Diagnostics,
            "counterexample" => Counterexample,
            "deadlock_traces" => DeadlockTraces,
            "max_counter_traces" => MaxCounterTraces,
            "proof_obligations" => ProofObligations,
            "true" => True,
            "false" => False,
            "enums" => Enums,
            "enum" => Enum,
            "ltl" => Ltl,
            "mu" => Mu,
            // Note: LTL operators are not keywords - they're recognized by the LTL parser
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Symbol {
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Semicolon,
    Assign,
    Arrow,
    RangeInclusive,
    Dot,
    Colon,
    Percent,
    Plus,
    Minus,
    Star,
    Slash,
    AmpAmp,
    PipePipe,
    /// mununu#570 — `|`, the ALTERNATION separator in a transition label list
    /// (`on a | b`), as distinct from the comma form, which is a synchronisation vector.
    Pipe,
    Bang,
    EqEq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Symbol::*;
        let text = match self {
            LBrace => "{",
            RBrace => "}",
            LParen => "(",
            RParen => ")",
            LBracket => "[",
            RBracket => "]",
            Comma => ",",
            Semicolon => ";",
            Assign => "=",
            Arrow => "->",
            RangeInclusive => "..=",
            Dot => ".",
            Colon => ":",
            Percent => "%",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Slash => "/",
            AmpAmp => "&&",
            PipePipe => "||",
            Pipe => "|",
            Bang => "!",
            EqEq => "==",
            NotEq => "!=",
            Lt => "<",
            Lte => "<=",
            Gt => ">",
            Gte => ">=",
        };
        write!(f, "{text}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Identifier(String),
    Integer(i64),
    String(String),
    Keyword(Keyword),
    Symbol(Symbol),
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ltl_keyword_parsing() {
        // Note: LTL operators (G, F, X, U, W, R) are NOT DSL keywords
        // because they can be used as identifiers (e.g., formula names, label names).
        // The LTL parser recognizes them directly from raw strings.

        // Verify LTL operators are NOT keywords (they should be identifiers)
        assert_eq!(Keyword::from_ident("G"), None);
        assert_eq!(Keyword::from_ident("F"), None);
        assert_eq!(Keyword::from_ident("X"), None);
        assert_eq!(Keyword::from_ident("U"), None);
        assert_eq!(Keyword::from_ident("W"), None);
        assert_eq!(Keyword::from_ident("R"), None);

        // Word keywords are also NOT DSL keywords
        assert_eq!(Keyword::from_ident("always"), None);
        assert_eq!(Keyword::from_ident("eventually"), None);
        assert_eq!(Keyword::from_ident("next"), None);
        assert_eq!(Keyword::from_ident("until"), None);
        assert_eq!(Keyword::from_ident("weak_until"), None);
        assert_eq!(Keyword::from_ident("release"), None);
    }
}
