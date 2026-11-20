//! Promela AST representation for the bounded subset.
//!
//! Represents the parsed structure of a Promela program before CFG extraction.
//! Only the supported subset is modeled; unsupported constructs are rejected
//! during parsing.

/// A complete Promela program.
#[derive(Debug, Clone)]
pub struct Program {
    /// mtype declarations (shared enum values).
    pub mtypes: Vec<MtypeDecl>,
    /// Global variable declarations.
    pub globals: Vec<VarDecl>,
    /// Channel declarations.
    pub channels: Vec<ChanDecl>,
    /// Process type declarations.
    pub proctypes: Vec<Proctype>,
    /// Init block (optional).
    pub init: Option<Sequence>,
    /// LTL property declarations.
    pub ltl_properties: Vec<LtlProperty>,
    /// Inline macro definitions (parsed but expansion is partial).
    pub inlines: Vec<InlineDef>,
    /// Trace assertions (parsed but not yet fully supported).
    pub traces: Vec<Sequence>,
    /// Notrace assertions (parsed but not yet fully supported).
    pub notraces: Vec<Sequence>,
}

/// mtype { name1, name2, ... }
#[derive(Debug, Clone)]
pub struct MtypeDecl {
    pub values: Vec<String>,
}

/// Variable declaration.
#[derive(Debug, Clone)]
pub struct VarDecl {
    pub typename: TypeName,
    pub name: String,
    pub array_size: Option<usize>,
    pub init: Option<Expr>,
}

/// Channel declaration: chan name = [capacity] of { type1, type2, ... }
#[derive(Debug, Clone)]
pub struct ChanDecl {
    pub name: String,
    pub capacity: usize,
    pub message_types: Vec<TypeName>,
}

/// Process type declaration.
#[derive(Debug, Clone)]
pub struct Proctype {
    pub name: String,
    /// Number of active instances (0 = not active, must be started via run).
    pub active_count: usize,
    /// Parameters.
    pub params: Vec<VarDecl>,
    /// Provided clause (schedulability guard).
    pub provided: Option<Expr>,
    /// Whether this is a `d_proctype` (deterministic process).
    pub deterministic: bool,
    /// Process body.
    pub body: Sequence,
}

/// Inline macro definition: `inline name(params) { body }`.
#[derive(Debug, Clone)]
pub struct InlineDef {
    pub name: String,
    pub params: Vec<String>,
    pub body: Sequence,
}

/// LTL property block: ltl name { formula }
#[derive(Debug, Clone)]
pub struct LtlProperty {
    pub name: Option<String>,
    pub formula: LtlExpr,
}

/// A sequence of steps (statements or local declarations).
pub type Sequence = Vec<Step>;

/// A step in a sequence.
#[derive(Debug, Clone)]
pub enum Step {
    Statement(Statement),
    Decl(VarDecl),
}

/// Promela statement.
#[derive(Debug, Clone)]
pub enum Statement {
    /// var = expr
    Assign { target: VarRef, value: Expr },
    /// channel ! expr, expr, ...
    Send { channel: String, args: Vec<Expr> },
    /// channel ? var, var, ...
    Recv { channel: String, args: Vec<RecvArg> },
    /// if :: seq :: seq ... fi
    If { options: Vec<Sequence> },
    /// do :: seq :: seq ... od
    Do { options: Vec<Sequence> },
    /// atomic { sequence }
    Atomic { body: Sequence },
    /// d_step { sequence }
    DStep { body: Sequence },
    /// { sequence }
    Block { body: Sequence },
    /// goto label
    Goto { label: String },
    /// label: statement
    Label { name: String, stmt: Box<Statement> },
    /// break
    Break,
    /// assert(expr)
    Assert { expr: Expr },
    /// skip
    Skip,
    /// Expression as statement (guard condition).
    ExprStmt { expr: Expr },
    /// printf("...", args) — ignored but parsed.
    Printf { format: String, args: Vec<Expr> },
    /// stmt unless { escape } — preemption construct.
    Unless {
        body: Box<Statement>,
        escape: Sequence,
    },
}

/// Receive argument.
#[derive(Debug, Clone)]
pub enum RecvArg {
    /// Store into variable.
    Var(VarRef),
    /// Match constant.
    Const(Expr),
    /// eval(expr).
    Eval(Expr),
}

/// Variable reference: name[index].field
#[derive(Debug, Clone)]
pub struct VarRef {
    pub name: String,
    pub index: Option<Box<Expr>>,
    pub field: Option<Box<VarRef>>,
}

/// Type names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeName {
    Bit,
    Bool,
    Byte,
    Short,
    Int,
    Mtype,
    Chan,
    UserDefined(String),
}

impl TypeName {
    /// Default range for this type (min, max inclusive).
    pub fn default_range(&self) -> (i64, i64) {
        match self {
            TypeName::Bit => (0, 1),
            TypeName::Bool => (0, 1),
            TypeName::Byte => (0, 255),
            TypeName::Short => (-32768, 32767),
            TypeName::Int => (-2_147_483_648, 2_147_483_647),
            TypeName::Mtype => (0, 255), // mtype values are small integers
            TypeName::Chan => (0, 0),
            TypeName::UserDefined(_) => (0, 255),
        }
    }

    /// Whether this type needs auto-bounding (not inherently finite and small).
    pub fn needs_bounding(&self) -> bool {
        matches!(self, TypeName::Short | TypeName::Int)
    }

    /// The Promela keyword the declaration used (`bit`, `bool`, `byte`, `short`, `int`,
    /// `mtype`, `chan`), or the user-defined type's own name.
    pub fn keyword(&self) -> &str {
        match self {
            TypeName::Bit => "bit",
            TypeName::Bool => "bool",
            TypeName::Byte => "byte",
            TypeName::Short => "short",
            TypeName::Int => "int",
            TypeName::Mtype => "mtype",
            TypeName::Chan => "chan",
            TypeName::UserDefined(name) => name,
        }
    }
}

/// Expression AST node.
#[derive(Debug, Clone)]
pub enum Expr {
    /// Integer literal.
    IntLit(i64),
    /// Boolean literal.
    BoolLit(bool),
    /// Variable reference.
    VarRef(VarRef),
    /// Binary operation.
    BinOp {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// Unary operation.
    UnOp { op: UnOp, operand: Box<Expr> },
    /// Channel length: len(ch).
    Len(String),
    /// Channel empty: empty(ch).
    Empty(String),
    /// Channel non-empty: nempty(ch).
    Nempty(String),
    /// Channel full: full(ch).
    Full(String),
    /// Channel not full: nfull(ch).
    Nfull(String),
    /// Mtype constant name.
    MtypeName(String),
    /// Parenthesized expression.
    Paren(Box<Expr>),
    /// `timeout` — fires when no other transition is enabled.
    /// Conservative approximation: treated as always-enabled guard.
    Timeout,
    /// Remote reference: `process@label` — tests if process is at label.
    /// Approximated: emits a test label for synchronization.
    RemoteRef { process: String, label: String },
    /// Remote variable: `process:var` — reads a variable from another process.
    /// Approximated: emits a test label for synchronization.
    RemoteVar { process: String, var: String },
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Shl,
    Shr,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
    BitNot,
}

/// LTL formula expression.
#[derive(Debug, Clone)]
pub enum LtlExpr {
    True,
    False,
    Predicate(String),
    Not(Box<LtlExpr>),
    And(Box<LtlExpr>, Box<LtlExpr>),
    Or(Box<LtlExpr>, Box<LtlExpr>),
    Implies(Box<LtlExpr>, Box<LtlExpr>),
    Iff(Box<LtlExpr>, Box<LtlExpr>),
    Always(Box<LtlExpr>),
    Eventually(Box<LtlExpr>),
    Next(Box<LtlExpr>),
    Until(Box<LtlExpr>, Box<LtlExpr>),
    WeakUntil(Box<LtlExpr>, Box<LtlExpr>),
    Release(Box<LtlExpr>, Box<LtlExpr>),
}

#[cfg(test)]
mod tests {
    use super::TypeName;

    /// mununu#1121 — a type prints as its Promela keyword, never as the Rust variant name.
    #[test]
    fn x1121_the_promela_type_name_displays_its_keyword() {
        assert_eq!(TypeName::Int.keyword(), "int");
        assert_eq!(TypeName::Short.keyword(), "short");
        assert_eq!(TypeName::Mtype.keyword(), "mtype");
        assert_eq!(TypeName::UserDefined("msg_t".into()).keyword(), "msg_t");
    }
}
