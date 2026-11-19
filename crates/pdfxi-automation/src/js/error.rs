//! Typed, position-aware errors for the JavaScript sandbox.
//!
//! Every rejection carries a [`Pos`] so callers can point at the exact source
//! location that caused it. The crate is intentionally dependency-free, so
//! [`std::fmt::Display`] and [`std::error::Error`] are implemented directly
//! rather than through a derive macro (the same convention used by
//! [`crate::AutomationError`]).

use std::fmt;

/// A source position in a script.
///
/// `line` and `column` are one-based; `offset` is a zero-based character index
/// into the original source. `Pos::default()` is the sentinel `0:0`, used only
/// for errors that are not tied to a particular token (for example a host
/// object that was called through a registry entry).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pos {
    /// One-based line number, or `0` when unknown.
    pub line: usize,
    /// One-based column number, or `0` when unknown.
    pub column: usize,
    /// Zero-based character offset into the source, or `0` when unknown.
    pub offset: usize,
}

impl Pos {
    pub(crate) fn new(line: usize, column: usize, offset: usize) -> Self {
        Self {
            line,
            column,
            offset,
        }
    }
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// A deterministic error produced while lexing, parsing, or running a script.
///
/// The variants are deliberately specific so a host application can react to a
/// budget overrun differently from a syntax error, and so every failure can be
/// reported with a source position. No variant carries data obtained from the
/// filesystem, network, process table, or environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsError {
    /// The source contained a character or literal the lexer cannot scan.
    Lex {
        /// Position of the offending character.
        pos: Pos,
        /// Human-readable explanation.
        message: String,
    },
    /// The token stream did not form a valid program.
    Parse {
        /// Position of the token that broke the grammar.
        pos: Pos,
        /// Human-readable explanation.
        message: String,
    },
    /// A recognized JavaScript feature that the subset intentionally omits.
    UnsupportedSyntax {
        /// Position of the unsupported construct.
        pos: Pos,
        /// Name of the unsupported feature (for example `class` or `=>`).
        feature: String,
    },
    /// A reference to a dangerous global such as `eval` or `process`.
    DangerousGlobal {
        /// Position of the identifier.
        pos: Pos,
        /// The rejected identifier.
        name: String,
    },
    /// A reference to an identifier that is not defined in the sandbox.
    ///
    /// The sandbox has no ambient globals: every name must be declared in the
    /// script, be a documented built-in, or be an explicitly injected host
    /// object.
    AmbientAccess {
        /// Position of the identifier.
        pos: Pos,
        /// The unresolved identifier.
        name: String,
    },
    /// A name was declared twice in the same scope.
    DuplicateDeclaration {
        /// Position of the second declaration.
        pos: Pos,
        /// The duplicated name.
        name: String,
    },
    /// An assignment targeted a `const` binding.
    AssignToConst {
        /// Position of the assignment target.
        pos: Pos,
        /// The constant name.
        name: String,
    },
    /// The deterministic CPU-step budget was exhausted.
    StepBudgetExceeded {
        /// Position of the operation that crossed the limit.
        pos: Pos,
        /// The configured limit.
        limit: u64,
    },
    /// Nested function calls exceeded the call-depth limit.
    CallDepthExceeded {
        /// Position of the call that crossed the limit.
        pos: Pos,
        /// The configured limit.
        limit: usize,
    },
    /// The total number of function calls exceeded the limit.
    CallCountExceeded {
        /// Position of the call that crossed the limit.
        pos: Pos,
        /// The configured limit.
        limit: u64,
    },
    /// A single loop ran for more iterations than the hard cap allows.
    LoopIterationsExceeded {
        /// Position of the loop.
        pos: Pos,
        /// The configured limit.
        limit: u64,
    },
    /// All loops together ran for more iterations than the hard cap allows.
    TotalIterationsExceeded {
        /// Position of the loop that crossed the limit.
        pos: Pos,
        /// The configured limit.
        limit: u64,
    },
    /// A string operation would have produced an over-sized value.
    StringTooLarge {
        /// Position of the operation.
        pos: Pos,
        /// The configured limit in characters.
        limit: usize,
    },
    /// An array operation would have produced an over-sized value.
    ArrayTooLarge {
        /// Position of the operation.
        pos: Pos,
        /// The configured limit in elements.
        limit: usize,
    },
    /// An object operation would have produced an over-sized value.
    ObjectTooLarge {
        /// Position of the operation.
        pos: Pos,
        /// The configured limit in entries.
        limit: usize,
    },
    /// Expressions were nested deeper than the parser allows.
    AstTooDeep {
        /// Position of the deepest node.
        pos: Pos,
        /// The configured limit.
        limit: usize,
    },
    /// A value was nested too deeply while being converted to a string.
    ///
    /// This bounds the conversion walk so a deeply nested array cannot exhaust
    /// the native call stack.
    ValueTooDeep {
        /// Position of the conversion.
        pos: Pos,
        /// The maximum nesting depth.
        limit: usize,
    },
    /// A value contained a cycle while being converted to a string.
    CyclicValue {
        /// Position of the conversion.
        pos: Pos,
    },
    /// The wall-clock budget elapsed while the script was running.
    WallClockExceeded {
        /// Position of the operation that observed the elapsed deadline.
        pos: Pos,
        /// The configured limit in whole milliseconds.
        limit_ms: u128,
    },
    /// A cancellation token was signalled before the script finished.
    Cancelled {
        /// Position where cancellation was observed.
        pos: Pos,
    },
    /// An operand had the wrong runtime type.
    Type {
        /// Position of the offending operation.
        pos: Pos,
        /// Explanation of the expected and actual types.
        message: String,
    },
    /// A division or modulo operation had a zero divisor.
    DivisionByZero {
        /// Position of the operator.
        pos: Pos,
    },
    /// A property read targeted a key that does not exist.
    PropertyNotFound {
        /// Position of the property access.
        pos: Pos,
        /// The missing property name.
        name: String,
    },
    /// A method call targeted a method that does not exist for the receiver.
    MethodNotFound {
        /// Position of the call.
        pos: Pos,
        /// The missing method name.
        name: String,
    },
    /// An out-of-range array index or string offset was used.
    IndexOutOfBounds {
        /// Position of the index expression.
        pos: Pos,
        /// The requested index.
        index: i64,
        /// The length of the receiver.
        len: usize,
    },
    /// A function was called with the wrong number of arguments.
    Arity {
        /// Position of the call.
        pos: Pos,
        /// Number of declared parameters.
        expected: usize,
        /// Number of supplied arguments.
        got: usize,
    },
    /// A value that is not callable was invoked.
    NotCallable {
        /// Position of the call.
        pos: Pos,
    },
    /// A `return` statement was executed outside a function body.
    ReturnOutsideFunction {
        /// Position of the `return`.
        pos: Pos,
    },
    /// A `break` statement was executed outside a loop.
    BreakOutsideLoop {
        /// Position of the `break`.
        pos: Pos,
    },
    /// A `continue` statement was executed outside a loop.
    ContinueOutsideLoop {
        /// Position of the `continue`.
        pos: Pos,
    },
    /// An Acrobat built-in or `event` was used without an injected document
    /// context.
    NoAcrobatContext {
        /// Position of the operation that required a context.
        pos: Pos,
    },
    /// An explicitly injected host object rejected a call.
    Host {
        /// Position of the call, or `0:0` for registry-level failures.
        pos: Pos,
        /// The host object's error message.
        message: String,
    },
    /// A sandbox invariant was violated; this indicates a bug, not bad input.
    Internal {
        /// Position of the operation, if known.
        pos: Pos,
        /// Diagnostic message.
        message: String,
    },
}

impl JsError {
    /// Returns the source position attached to this error.
    pub fn pos(&self) -> Pos {
        match self {
            Self::Lex { pos, .. }
            | Self::Parse { pos, .. }
            | Self::UnsupportedSyntax { pos, .. }
            | Self::DangerousGlobal { pos, .. }
            | Self::AmbientAccess { pos, .. }
            | Self::DuplicateDeclaration { pos, .. }
            | Self::AssignToConst { pos, .. }
            | Self::StepBudgetExceeded { pos, .. }
            | Self::CallDepthExceeded { pos, .. }
            | Self::CallCountExceeded { pos, .. }
            | Self::LoopIterationsExceeded { pos, .. }
            | Self::TotalIterationsExceeded { pos, .. }
            | Self::StringTooLarge { pos, .. }
            | Self::ArrayTooLarge { pos, .. }
            | Self::ObjectTooLarge { pos, .. }
            | Self::AstTooDeep { pos, .. }
            | Self::ValueTooDeep { pos, .. }
            | Self::CyclicValue { pos }
            | Self::WallClockExceeded { pos, .. }
            | Self::Cancelled { pos }
            | Self::Type { pos, .. }
            | Self::DivisionByZero { pos }
            | Self::PropertyNotFound { pos, .. }
            | Self::MethodNotFound { pos, .. }
            | Self::IndexOutOfBounds { pos, .. }
            | Self::Arity { pos, .. }
            | Self::NotCallable { pos }
            | Self::ReturnOutsideFunction { pos }
            | Self::BreakOutsideLoop { pos }
            | Self::ContinueOutsideLoop { pos }
            | Self::NoAcrobatContext { pos }
            | Self::Host { pos, .. }
            | Self::Internal { pos, .. } => *pos,
        }
    }
}

impl fmt::Display for JsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pos = self.pos();
        if pos.line != 0 {
            write!(f, "{pos}: ")?;
        }
        match self {
            Self::Lex { message, .. } => write!(f, "lex error: {message}"),
            Self::Parse { message, .. } => write!(f, "parse error: {message}"),
            Self::UnsupportedSyntax { feature, .. } => {
                write!(
                    f,
                    "unsupported syntax: '{feature}' is not in the sandbox subset"
                )
            }
            Self::DangerousGlobal { name, .. } => {
                write!(
                    f,
                    "dangerous global '{name}' is not available in the sandbox"
                )
            }
            Self::AmbientAccess { name, .. } => {
                write!(
                    f,
                    "unresolved identifier '{name}'; the sandbox has no ambient globals"
                )
            }
            Self::DuplicateDeclaration { name, .. } => {
                write!(f, "'{name}' is already declared in this scope")
            }
            Self::AssignToConst { name, .. } => write!(f, "cannot assign to const '{name}'"),
            Self::StepBudgetExceeded { limit, .. } => {
                write!(f, "step budget exceeded ({limit} steps)")
            }
            Self::CallDepthExceeded { limit, .. } => {
                write!(f, "call depth exceeded ({limit} frames)")
            }
            Self::CallCountExceeded { limit, .. } => {
                write!(f, "call count exceeded ({limit} calls)")
            }
            Self::LoopIterationsExceeded { limit, .. } => {
                write!(f, "loop iteration cap exceeded ({limit} iterations)")
            }
            Self::TotalIterationsExceeded { limit, .. } => {
                write!(f, "total iteration cap exceeded ({limit} iterations)")
            }
            Self::StringTooLarge { limit, .. } => {
                write!(f, "string size limit exceeded ({limit} characters)")
            }
            Self::ArrayTooLarge { limit, .. } => {
                write!(f, "array size limit exceeded ({limit} elements)")
            }
            Self::ObjectTooLarge { limit, .. } => {
                write!(f, "object size limit exceeded ({limit} entries)")
            }
            Self::AstTooDeep { limit, .. } => {
                write!(f, "expression nesting limit exceeded ({limit} levels)")
            }
            Self::ValueTooDeep { limit, .. } => {
                write!(
                    f,
                    "value nesting limit exceeded during string conversion ({limit} levels)"
                )
            }
            Self::CyclicValue { .. } => {
                write!(f, "cyclic value cannot be converted to a string")
            }
            Self::WallClockExceeded { limit_ms, .. } => {
                write!(f, "wall-clock budget exceeded ({limit_ms} ms)")
            }
            Self::Cancelled { .. } => write!(f, "script execution was cancelled"),
            Self::Type { message, .. } => write!(f, "type error: {message}"),
            Self::DivisionByZero { .. } => write!(f, "division or modulo by zero"),
            Self::PropertyNotFound { name, .. } => write!(f, "property '{name}' does not exist"),
            Self::MethodNotFound { name, .. } => {
                write!(f, "method '{name}' is not available for this value")
            }
            Self::IndexOutOfBounds { index, len, .. } => {
                write!(f, "index {index} is out of bounds for length {len}")
            }
            Self::Arity { expected, got, .. } => {
                write!(f, "expected {expected} argument(s), got {got}")
            }
            Self::NotCallable { .. } => write!(f, "value is not callable"),
            Self::ReturnOutsideFunction { .. } => {
                write!(f, "'return' is only valid inside a function")
            }
            Self::BreakOutsideLoop { .. } => write!(f, "'break' is only valid inside a loop"),
            Self::ContinueOutsideLoop { .. } => {
                write!(f, "'continue' is only valid inside a loop")
            }
            Self::NoAcrobatContext { .. } => {
                write!(f, "Acrobat document context is not available in this run")
            }
            Self::Host { message, .. } => write!(f, "host object error: {message}"),
            Self::Internal { message, .. } => write!(f, "sandbox invariant violated: {message}"),
        }
    }
}

impl std::error::Error for JsError {}
