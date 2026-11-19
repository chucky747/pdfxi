//! Audited host-object boundary for the JavaScript sandbox.
//!
//! A script can never reach the filesystem, network, process table, or
//! environment on its own. The embedding application deliberately registers a
//! named object implementing [`HostObject`], and only that object can service
//! method calls. This task adds the boundary and test doubles; no real PDF
//! mutation is wired up, so the safe default [`NoHostObject`] rejects every
//! call.

use crate::js::error::{JsError, Pos};
use crate::js::value::Value;
use crate::js::{is_dangerous_name, is_reserved_name, is_valid_identifier};
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

/// A fixed, auditable capability that a script may call through a registered
/// name.
///
/// Implementors receive a method name and already-evaluated arguments and
/// return a [`Value`] or a typed error. The interpreter has already applied all
/// budgets before a call reaches the host, so an implementation only needs to
/// validate its own domain-specific inputs. Implementations must be
/// deterministic and must not perform I/O in the sandbox context.
pub trait HostObject: fmt::Debug {
    /// Handles one method call from a script.
    ///
    /// `method` is the property name used at the call site and `args` are the
    /// evaluated arguments in source order.
    fn call(&self, method: &str, args: &[Value]) -> Result<Value, JsError>;
}

/// A host object that rejects every call.
///
/// This is the default capability surface: with no registrations, scripts can
/// only use pure built-ins and cannot affect the outside world.
#[derive(Debug, Clone, Copy)]
pub struct NoHostObject;

impl HostObject for NoHostObject {
    fn call(&self, method: &str, _args: &[Value]) -> Result<Value, JsError> {
        Err(JsError::Host {
            pos: Pos::default(),
            message: format!("no host object implements method '{method}'"),
        })
    }
}

/// A name-to-object registry that defines exactly which capabilities exist in a
/// sandbox run.
///
/// Names must be valid identifiers, must not collide with reserved words, and
/// must not be one of the rejected dangerous globals (for example `process` or
/// `eval`). This makes the audited surface explicit and prevents a host from
/// accidentally shadowing a built-in or a security-sensitive name.
#[derive(Debug, Default)]
pub struct HostRegistry {
    objects: BTreeMap<String, Rc<dyn HostObject>>,
}

impl HostRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `object` under `name`.
    ///
    /// Returns a typed error when the name is invalid, reserved, dangerous, or
    /// already taken. The registry is intentionally single-threaded and shares
    /// objects through [`Rc`].
    pub fn register(
        &mut self,
        name: impl Into<String>,
        object: Rc<dyn HostObject>,
    ) -> Result<(), JsError> {
        let name = name.into();
        if is_dangerous_name(&name) {
            return Err(JsError::DangerousGlobal {
                pos: Pos::default(),
                name,
            });
        }
        if !is_valid_identifier(&name) || is_reserved_name(&name) {
            return Err(JsError::Host {
                pos: Pos::default(),
                message: format!("'{name}' is not a valid host object name"),
            });
        }
        if self.objects.contains_key(&name) {
            return Err(JsError::Host {
                pos: Pos::default(),
                message: format!("host object '{name}' is already registered"),
            });
        }
        self.objects.insert(name, object);
        Ok(())
    }

    /// Looks up a registered object by name.
    pub fn get(&self, name: &str) -> Option<&Rc<dyn HostObject>> {
        self.objects.get(name)
    }

    /// Returns `true` when no objects are registered.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Returns the registered names in stable sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.objects.keys().map(String::as_str)
    }
}
