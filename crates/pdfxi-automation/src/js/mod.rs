//! A dependency-free, deterministic JavaScript subset for document actions.
//!
//! This module implements a *capability-free* interpreter. Scripts can compute
//! values and manipulate local arrays and objects, but they have no filesystem,
//! network, process, environment, timer, or global-object access. Every run is
//! bounded by explicit CPU-step, call-depth, call-count, loop-iteration,
//! string/array/object-size, wall-clock, and cancellation limits.
//!
//! # Supported syntax
//!
//! - Literals: numbers, single- or double-quoted strings, `true`, `false`,
//!   `null`.
//! - Declarations: `let`, `var`, `const`; assignment `=` plus `+= -= *= /= %=`.
//! - Operators, from lowest to highest precedence: assignment, `||`, `&&`,
//!   `== !=`, `< <= > >=`, `+ -`, `* / %`, unary `! - +`, and postfix `()`,
//!   `.`, `[]`. Binary operators are left-associative; assignment is
//!   right-associative. `&&` and `||` short-circuit and return an operand.
//! - Statements: expression statements, blocks, `if`/`else`, `while`, `for`
//!   (with or without init/condition/update), `function` declarations and
//!   calls, `return`, `break`, `continue`, and empty statements.
//! - Values: arrays and objects with dot/bracket access, index and property
//!   assignment, and the methods listed below.
//!
//! # Built-in whitelist
//!
//! The only predefined free functions are `Number`, `String`, `Boolean`,
//! `parseInt`, `parseFloat`, and `isNaN`. The `Math` namespace exposes `abs`,
//! `floor`, `ceil`, `round`, `min`, `max`, `sqrt`, and `pow`. String values
//! expose `length`, `toString`, `toUpperCase`, `toLowerCase`, `trim`, `charAt`,
//! `indexOf`, `slice`, `substring`, and `split`. Array values expose `length`,
//! `toString`, `join`, `push`, `pop`, `slice`, `indexOf`, `concat`, and
//! `reverse`. These helpers are date-free, random-free, and pure apart from the
//! explicitly documented array mutators.
//!
//! # Rejected constructs
//!
//! Dangerous globals (`eval`, `Function`, `require`, `import`, `export`,
//! `process`, `globalThis`, `global`, `window`, `self`, `this`, `arguments`,
//! `Reflect`, `Proxy`) are rejected with [`JsError::DangerousGlobal`]. Reserved
//! JavaScript features (`new`, `class`, `delete`, `typeof`, `instanceof`,
//! `switch`, `try`/`catch`, `throw`, `async`/`await`, `=>` , template literals,
//! and similar) are rejected with [`JsError::UnsupportedSyntax`]. Any other
//! unresolved name is an [`JsError::AmbientAccess`]: the sandbox has no ambient
//! globals.
//!
//! # Example
//!
//! ```text
//! use pdfxi_automation::{run_script, CancelToken, Limits};
//!
//! let script = "var total = 0; for (var i = 0; i < 4; i = i + 1) { total += i; } total;";
//! let result = run_script(script, &Limits::default(), &CancelToken::new())
//!     .expect("valid script");
//! assert_eq!(result.as_number(), Some(6.0));
//! ```
//!
//! # Dependency note
//!
//! Errors are implemented with direct [`std::fmt::Display`] and
//! [`std::error::Error`] impls rather than a derive macro. The parent crate is
//! deliberately dependency-free (see `docs/ARCHITECTURE.md`), and adding
//! `thiserror` would modify the integrator-owned `Cargo.lock`; this module
//! follows the established [`crate::AutomationError`] convention instead.

mod ast;
mod error;
mod host;
mod interpreter;
mod lexer;
mod parser;
mod value;

pub use error::{JsError, Pos};
pub use host::{HostObject, HostRegistry, NoHostObject};
pub use value::{BuiltinFn, JsFunction, Value};

use crate::CancelToken;
use std::time::Duration;

/// Runtime limits applied to every script execution.
///
/// All counters are checked on every relevant operation. A limit of zero means
/// the very first operation of that kind fails, which is useful for negative
/// tests. The wall clock is optional so ordinary runs remain fully
/// deterministic; when set it is checked on every evaluation step.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Maximum number of interpreter steps (statements and expressions).
    pub max_steps: u64,
    /// Maximum nested function-call depth.
    pub max_call_depth: usize,
    /// Maximum total number of function calls in a run.
    pub max_calls: u64,
    /// Maximum iterations in any single `while` or `for` loop.
    pub max_loop_iterations: u64,
    /// Maximum total iterations across all loops in a run.
    pub max_total_iterations: u64,
    /// Maximum number of characters in any produced string.
    pub max_string_len: usize,
    /// Maximum number of elements in any produced array.
    pub max_array_len: usize,
    /// Maximum number of entries in any produced object.
    pub max_object_entries: usize,
    /// Maximum expression/statement nesting depth accepted by the parser.
    pub max_ast_depth: usize,
    /// Optional wall-clock deadline; `None` keeps runs deterministic.
    pub wall_clock: Option<Duration>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: 100_000,
            max_call_depth: 64,
            max_calls: 10_000,
            max_loop_iterations: 10_000,
            max_total_iterations: 100_000,
            max_string_len: 1_000_000,
            max_array_len: 10_000,
            max_object_entries: 10_000,
            max_ast_depth: 128,
            wall_clock: None,
        }
    }
}

impl Limits {
    /// Sets the CPU-step budget.
    pub fn with_max_steps(mut self, limit: u64) -> Self {
        self.max_steps = limit;
        self
    }

    /// Sets the nested call-depth budget.
    pub fn with_max_call_depth(mut self, limit: usize) -> Self {
        self.max_call_depth = limit;
        self
    }

    /// Sets the total call-count budget.
    pub fn with_max_calls(mut self, limit: u64) -> Self {
        self.max_calls = limit;
        self
    }

    /// Sets the per-loop iteration cap.
    pub fn with_max_loop_iterations(mut self, limit: u64) -> Self {
        self.max_loop_iterations = limit;
        self
    }

    /// Sets the total iteration cap.
    pub fn with_max_total_iterations(mut self, limit: u64) -> Self {
        self.max_total_iterations = limit;
        self
    }

    /// Sets the maximum string length in characters.
    pub fn with_max_string_len(mut self, limit: usize) -> Self {
        self.max_string_len = limit;
        self
    }

    /// Sets the maximum array length.
    pub fn with_max_array_len(mut self, limit: usize) -> Self {
        self.max_array_len = limit;
        self
    }

    /// Sets the maximum object entry count.
    pub fn with_max_object_entries(mut self, limit: usize) -> Self {
        self.max_object_entries = limit;
        self
    }

    /// Sets the parser nesting-depth limit.
    pub fn with_max_ast_depth(mut self, limit: usize) -> Self {
        self.max_ast_depth = limit;
        self
    }

    /// Sets an optional wall-clock deadline.
    pub fn with_wall_clock(mut self, budget: Duration) -> Self {
        self.wall_clock = Some(budget);
        self
    }
}

/// A parsed script that can be run any number of times.
///
/// Parsing applies the lexer and the structural limits from [`Limits`]; running
/// applies the dynamic budgets. Separating the phases lets a host validate a
/// script once and execute it repeatedly with different cancellation tokens.
#[derive(Clone, Debug)]
pub struct Script {
    program: ast::Program,
}

impl Script {
    /// Parses `source` using the default parser limits.
    pub fn parse(source: &str) -> Result<Self, JsError> {
        Self::parse_with_limits(source, &Limits::default())
    }

    /// Parses `source` using `limits` for the parser nesting limit.
    pub fn parse_with_limits(source: &str, limits: &Limits) -> Result<Self, JsError> {
        let tokens = lexer::lex(source)?;
        let mut parser = parser::Parser::new(&tokens, limits.max_ast_depth);
        let program = parser.parse_program()?;
        Ok(Self { program })
    }

    /// Runs the parsed script with the given limits and cancellation token and
    /// no host objects.
    pub fn run(&self, limits: &Limits, cancel: &CancelToken) -> Result<Value, JsError> {
        self.run_with_host(limits, cancel, &HostRegistry::new())
    }

    /// Runs the parsed script with explicitly registered host objects.
    pub fn run_with_host(
        &self,
        limits: &Limits,
        cancel: &CancelToken,
        host: &HostRegistry,
    ) -> Result<Value, JsError> {
        let mut interpreter = interpreter::Interpreter::new(limits, cancel, host);
        interpreter.run(&self.program)
    }
}

/// Parses and immediately runs `source` with no host objects.
///
/// This is the convenience entry point used by tests and simple hosts; callers
/// that need host capabilities should build a [`Script`] and use
/// [`Script::run_with_host`].
pub fn run_script(source: &str, limits: &Limits, cancel: &CancelToken) -> Result<Value, JsError> {
    Script::parse_with_limits(source, limits)?.run(limits, cancel)
}

pub(crate) fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_alphabetic() || ch == '_' || ch == '$' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

pub(crate) fn is_dangerous_name(name: &str) -> bool {
    matches!(
        name,
        "eval"
            | "Function"
            | "require"
            | "import"
            | "export"
            | "process"
            | "globalThis"
            | "global"
            | "window"
            | "self"
            | "this"
            | "arguments"
            | "Reflect"
            | "Proxy"
    )
}

pub(crate) fn is_reserved_name(name: &str) -> bool {
    matches!(
        name,
        "let"
            | "var"
            | "const"
            | "if"
            | "else"
            | "while"
            | "for"
            | "function"
            | "return"
            | "break"
            | "continue"
            | "true"
            | "false"
            | "null"
            | "new"
            | "class"
            | "delete"
            | "typeof"
            | "instanceof"
            | "in"
            | "of"
            | "switch"
            | "case"
            | "default"
            | "try"
            | "catch"
            | "finally"
            | "throw"
            | "do"
            | "async"
            | "await"
            | "yield"
            | "void"
            | "debugger"
            | "with"
            | "super"
            | "extends"
            | "enum"
            | "implements"
            | "interface"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
    )
}

#[cfg(test)]
mod tests;
