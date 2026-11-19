//! Runtime values, built-in function identities, and lexical scopes.
//!
//! Values are plain Rust data with no ambient capabilities. Arrays and objects
//! use `Rc<RefCell<..>>` so the subset can mutate them through index and dot
//! assignment; every borrow in this module uses `try_borrow`/`try_borrow_mut`
//! and converts a failed borrow into a typed error instead of panicking.

use crate::js::acrobat::{EventData, FieldStore};
use crate::js::ast::Stmt;
use crate::js::error::{JsError, Pos};
use crate::js::host::HostObject;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

/// Identities of the whitelisted built-in functions.
///
/// These are the only free functions and namespace methods a script can call.
/// The full whitelist is documented in the JavaScript sandbox module overview
/// that is re-exported from the crate root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinFn {
    /// The `Number(value)` coercion function.
    Number,
    /// The `String(value)` coercion function.
    String,
    /// The `Boolean(value)` coercion function.
    Boolean,
    /// The `parseInt(string)` helper.
    ParseInt,
    /// The `parseFloat(string)` helper.
    ParseFloat,
    /// The `isNaN(value)` predicate.
    IsNaN,
    /// `Math.abs`.
    MathAbs,
    /// `Math.floor`.
    MathFloor,
    /// `Math.ceil`.
    MathCeil,
    /// `Math.round`.
    MathRound,
    /// `Math.min`.
    MathMin,
    /// `Math.max`.
    MathMax,
    /// `Math.sqrt`.
    MathSqrt,
    /// `Math.pow`.
    MathPow,
    /// Acrobat `AFNumber_Format`.
    AfNumberFormat,
    /// Acrobat `AFNumber_Keystroke`.
    AfNumberKeystroke,
    /// Acrobat `AFPercent_Format`.
    AfPercentFormat,
    /// Acrobat `AFPercent_Keystroke`.
    AfPercentKeystroke,
    /// Acrobat `AFDate_FormatEx`.
    AfDateFormatEx,
    /// Acrobat `AFDate_KeystrokeEx`.
    AfDateKeystrokeEx,
    /// Acrobat `AFDate_Format` (legacy numeric format codes).
    AfDateFormat,
    /// Acrobat `AFDate_Keystroke` (legacy numeric format codes).
    AfDateKeystroke,
    /// Acrobat `util.printf` (documented subset).
    UtilPrintf,
}

/// A runtime value produced or consumed by the sandbox interpreter.
///
/// Values carry no ambient capabilities: a script can only obtain a
/// [`Value::Host`] by reading a name that the embedding application explicitly
/// registered. Internal function and built-in representations are exposed so a
/// [`HostObject`](crate::js::HostObject) can inspect arguments and return
/// results without depending on private types.
#[derive(Clone)]
pub enum Value {
    /// The JavaScript `undefined` value.
    Undefined,
    /// The JavaScript `null` literal.
    Null,
    /// A boolean literal.
    Bool(bool),
    /// An IEEE-754 double-precision number.
    Number(f64),
    /// A UTF-8 string.
    Str(String),
    /// A mutable array of values.
    Array(Rc<RefCell<Vec<Value>>>),
    /// A mutable string-keyed object.
    Object(Rc<RefCell<BTreeMap<String, Value>>>),
    /// A user-defined function closure.
    Function(Rc<JsFunction>),
    /// A whitelisted built-in function.
    BuiltinFn(BuiltinFn),
    /// The `Math` namespace object.
    MathNamespace,
    /// The Acrobat `util` namespace object (`util.printf`).
    UtilNamespace,
    /// The injected Acrobat `event` object.
    AcrobatEvent(Rc<RefCell<EventData>>),
    /// The injected Acrobat document context exposed as `this`.
    DocumentContext(Rc<RefCell<FieldStore>>),
    /// A field reference produced by `this.getField(name)`.
    Field(Rc<RefCell<FieldStore>>, String),
    /// An explicitly injected host object.
    Host(Rc<dyn HostObject>),
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Undefined => f.write_str("Undefined"),
            Self::Null => f.write_str("Null"),
            Self::Bool(value) => write!(f, "Bool({value})"),
            Self::Number(value) => write!(f, "Number({value})"),
            Self::Str(value) => write!(f, "Str({value:?})"),
            Self::Array(items) => match items.try_borrow() {
                Ok(items) => write!(f, "Array({} items)", items.len()),
                Err(_) => f.write_str("Array(<borrowed>)"),
            },
            Self::Object(entries) => match entries.try_borrow() {
                Ok(entries) => write!(f, "Object({} entries)", entries.len()),
                Err(_) => f.write_str("Object(<borrowed>)"),
            },
            Self::Function(function) => write!(f, "Function({})", function.name),
            Self::BuiltinFn(function) => write!(f, "BuiltinFn({function:?})"),
            Self::MathNamespace => f.write_str("MathNamespace"),
            Self::UtilNamespace => f.write_str("UtilNamespace"),
            Self::AcrobatEvent(_) => f.write_str("AcrobatEvent(<shared>)"),
            Self::DocumentContext(_) => f.write_str("DocumentContext(<shared>)"),
            Self::Field(_, name) => write!(f, "Field({name})"),
            Self::Host(_) => f.write_str("Host(<opaque>)"),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Undefined, Self::Undefined) | (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => match (a.try_borrow(), b.try_borrow()) {
                (Ok(a), Ok(b)) => a.as_slice() == b.as_slice(),
                _ => false,
            },
            (Self::Object(a), Self::Object(b)) => match (a.try_borrow(), b.try_borrow()) {
                (Ok(a), Ok(b)) => *a == *b,
                _ => false,
            },
            (Self::Function(a), Self::Function(b)) => Rc::ptr_eq(a, b),
            (Self::BuiltinFn(a), Self::BuiltinFn(b)) => a == b,
            (Self::MathNamespace, Self::MathNamespace)
            | (Self::UtilNamespace, Self::UtilNamespace) => true,
            (Self::AcrobatEvent(a), Self::AcrobatEvent(b)) => Rc::ptr_eq(a, b),
            (Self::DocumentContext(a), Self::DocumentContext(b)) => Rc::ptr_eq(a, b),
            (Self::Field(a, an), Self::Field(b, bn)) => an == bn && Rc::ptr_eq(a, b),
            (Self::Host(a), Self::Host(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Value {
    /// Returns the JavaScript-style type name of this value.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Undefined => "undefined",
            Self::Null => "object",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::Str(_) => "string",
            Self::Array(_) => "object",
            Self::Object(_) => "object",
            Self::Function(_) | Self::BuiltinFn(_) => "function",
            Self::MathNamespace | Self::UtilNamespace => "object",
            Self::AcrobatEvent(_)
            | Self::DocumentContext(_)
            | Self::Field(_, _)
            | Self::Host(_) => "object",
        }
    }

    /// Returns the wrapped number, or `None` for any other type.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the wrapped string slice, or `None` for any other type.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value.as_str()),
            _ => None,
        }
    }

    /// Returns the wrapped boolean, or `None` for any other type.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns `true` when this is the `null` literal.
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Returns `true` when this is the `undefined` value.
    pub fn is_undefined(&self) -> bool {
        matches!(self, Self::Undefined)
    }

    /// Returns a deterministic, human-readable rendering of the value.
    ///
    /// Nesting is capped at 16 levels so a cyclic array or object cannot
    /// produce an unbounded string. This helper is intended for diagnostics and
    /// tests; the interpreter uses a budget-checked conversion internally.
    pub fn display(&self) -> String {
        self.display_inner(0)
    }

    fn display_inner(&self, depth: usize) -> String {
        if depth > 16 {
            return "…".to_owned();
        }
        match self {
            Self::Undefined => "undefined".to_owned(),
            Self::Null => "null".to_owned(),
            Self::Bool(value) => value.to_string(),
            Self::Number(value) => format_number(*value),
            Self::Str(value) => value.clone(),
            Self::Array(items) => match items.try_borrow() {
                Ok(items) => {
                    let parts: Vec<String> = items
                        .iter()
                        .map(|item| item.display_inner(depth + 1))
                        .collect();
                    format!("[{}]", parts.join(","))
                }
                Err(_) => "[<borrowed>]".to_owned(),
            },
            Self::Object(entries) => match entries.try_borrow() {
                Ok(entries) => {
                    let parts: Vec<String> = entries
                        .iter()
                        .map(|(key, value)| format!("{key}:{}", value.display_inner(depth + 1)))
                        .collect();
                    format!("{{{}}}", parts.join(","))
                }
                Err(_) => "{<borrowed>}".to_owned(),
            },
            Self::Function(_) | Self::BuiltinFn(_) => "[function]".to_owned(),
            Self::MathNamespace => "[object Math]".to_owned(),
            Self::UtilNamespace => "[object util]".to_owned(),
            Self::AcrobatEvent(_) => "[object event]".to_owned(),
            Self::DocumentContext(_) => "[object document]".to_owned(),
            Self::Field(_, name) => format!("[field {name}]"),
            Self::Host(_) => "[host object]".to_owned(),
        }
    }
}

/// Formats a number deterministically without exponent noise for small values.
pub(crate) fn format_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == f64::INFINITY {
        return "Infinity".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_owned();
    }
    if value.fract() == 0.0 && value.abs() < 1.0e21 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// A user-defined function value closure over its defining environment.
///
/// The fields are crate-private; host code interacts with functions only through
/// [`Value`] and calls them by invoking the value from a script.
pub struct JsFunction {
    pub(crate) name: String,
    pub(crate) params: Vec<String>,
    pub(crate) body: Vec<Stmt>,
    pub(crate) env: Env,
}

impl fmt::Debug for JsFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JsFunction({})", self.name)
    }
}

/// A lexical scope chain. Clone handles share the same underlying scope data.
#[derive(Clone)]
pub(crate) struct Env(Rc<RefCell<EnvData>>);

struct EnvData {
    vars: BTreeMap<String, Binding>,
    parent: Option<Env>,
}

#[derive(Clone)]
struct Binding {
    mutable: bool,
    value: Value,
}

impl Env {
    /// Creates the root scope.
    pub(crate) fn root() -> Self {
        Self(Rc::new(RefCell::new(EnvData {
            vars: BTreeMap::new(),
            parent: None,
        })))
    }

    /// Creates a child scope that delegates lookups to `parent`.
    pub(crate) fn child(parent: &Env) -> Self {
        Self(Rc::new(RefCell::new(EnvData {
            vars: BTreeMap::new(),
            parent: Some(parent.clone()),
        })))
    }

    /// Declares a new binding in this scope only.
    pub(crate) fn declare(
        &self,
        name: &str,
        mutable: bool,
        value: Value,
        pos: Pos,
    ) -> Result<(), JsError> {
        let mut data = self.try_mut(pos)?;
        if data.vars.contains_key(name) {
            return Err(JsError::DuplicateDeclaration {
                pos,
                name: name.to_owned(),
            });
        }
        data.vars
            .insert(name.to_owned(), Binding { mutable, value });
        Ok(())
    }

    /// Reads a binding from this scope or an ancestor.
    pub(crate) fn get(&self, name: &str, pos: Pos) -> Result<Value, JsError> {
        let mut scope = self.clone();
        loop {
            let (found, parent) = {
                let data = scope.try_borrow(pos)?;
                (
                    data.vars.get(name).map(|binding| binding.value.clone()),
                    data.parent.clone(),
                )
            };
            if let Some(value) = found {
                return Ok(value);
            }
            match parent {
                Some(parent) => scope = parent,
                None => {
                    return Err(JsError::AmbientAccess {
                        pos,
                        name: name.to_owned(),
                    });
                }
            }
        }
    }

    /// Reassigns an existing binding, rejecting writes to `const` bindings.
    pub(crate) fn set(&self, name: &str, value: Value, pos: Pos) -> Result<(), JsError> {
        let mut scope = self.clone();
        loop {
            let mut data = scope.try_mut(pos)?;
            if let Some(binding) = data.vars.get_mut(name) {
                if binding.mutable {
                    binding.value = value;
                    return Ok(());
                }
                return Err(JsError::AssignToConst {
                    pos,
                    name: name.to_owned(),
                });
            }
            let parent = data.parent.clone();
            drop(data);
            match parent {
                Some(parent) => scope = parent,
                None => {
                    return Err(JsError::AmbientAccess {
                        pos,
                        name: name.to_owned(),
                    });
                }
            }
        }
    }

    fn try_borrow(&self, pos: Pos) -> Result<std::cell::Ref<'_, EnvData>, JsError> {
        self.0.try_borrow().map_err(|_| JsError::Internal {
            pos,
            message: "scope was already mutably borrowed".to_owned(),
        })
    }

    fn try_mut(&self, pos: Pos) -> Result<std::cell::RefMut<'_, EnvData>, JsError> {
        self.0.try_borrow_mut().map_err(|_| JsError::Internal {
            pos,
            message: "scope was already borrowed".to_owned(),
        })
    }
}
