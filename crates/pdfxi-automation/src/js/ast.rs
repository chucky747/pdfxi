//! Abstract syntax tree for the JavaScript sandbox subset.
//!
//! Every node carries a [`Pos`] so runtime errors can report the exact source
//! location that produced them. The tree is deliberately small: it contains only
//! the statements and expressions the subset supports, which makes an
//! exhaustive match in the interpreter a compile-time guarantee that no
//! unsupported construct can slip through.

use crate::js::error::Pos;

/// A parsed program: an ordered list of top-level statements.
#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub(crate) body: Vec<Stmt>,
}

/// A statement with its source position.
#[derive(Clone, Debug)]
pub(crate) struct Stmt {
    pub(crate) kind: StmtKind,
    pub(crate) pos: Pos,
}

/// The statement forms supported by the subset.
#[derive(Clone, Debug)]
pub(crate) enum StmtKind {
    /// `let`, `var`, or `const` declaration with an optional initializer.
    VarDecl {
        kind: VarKind,
        name: String,
        init: Option<Expr>,
    },
    /// An expression evaluated for its side effects.
    Expr(Expr),
    /// A brace-delimited block.
    Block(Vec<Stmt>),
    /// `if (cond) then [else other]`.
    If {
        cond: Expr,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
    },
    /// `while (cond) body`.
    While { cond: Expr, body: Box<Stmt> },
    /// `for (init; cond; update) body`; any clause may be absent.
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    /// A named function declaration, hoisted when its enclosing scope starts.
    FunctionDecl {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    /// `return [value]` from a function.
    Return(Option<Expr>),
    /// `break` out of the innermost loop.
    Break,
    /// `continue` to the next iteration of the innermost loop.
    Continue,
    /// An empty statement (`;`).
    Empty,
}

/// The three declaration kinds; `const` bindings cannot be reassigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VarKind {
    Let,
    Var,
    Const,
}

/// An expression with its source position.
#[derive(Clone, Debug)]
pub(crate) struct Expr {
    pub(crate) kind: ExprKind,
    pub(crate) pos: Pos,
}

/// The expression forms supported by the subset.
#[derive(Clone, Debug)]
pub(crate) enum ExprKind {
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
    Ident(String),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Assign {
        op: AssignOp,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Member {
        object: Box<Expr>,
        property: String,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
}

/// Unary operators, all at the highest precedence below postfix access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Neg,
    Pos,
    Not,
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Assignment operators, including the documented compound forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}
