//! Acrobat-compatible document context for field/format/validate scripts.
//!
//! This module supplies the *capability surface* that Acrobat-style document
//! scripts expect: a form-field store backing `this.getField(name).value`, an
//! `event` object exposing `value`, `key`, and `rc`, and the in-memory data
//! structures that the whitelisted `AF*` and `util.printf` built-ins operate on.
//!
//! The context is **explicitly injected**: a script never obtains it by name
//! from ambient state. Without an injected [`AcrobatContext`], `this` is a
//! rejected dangerous global and `event` is an unresolved name. All data is
//! in-memory and deterministic; no file, network, process, clock, or random
//! capability is involved, and no real PDF field is mutated by this crate.
//!
//! Host applications construct a context, seed fields and event data, run a
//! script through [`Script::run_with_acrobat`](crate::js::Script::run_with_acrobat),
//! then read the resulting field or event values back.

use super::{JsError, Pos};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Typed error returned by the fallible host accessors when the shared state is
/// already borrowed by an active script evaluation.
fn borrow_conflict(what: &str) -> JsError {
    JsError::Host {
        pos: Pos::default(),
        message: format!("{what} is already borrowed"),
    }
}

/// An in-memory store of form field values, keyed by fully-qualified name.
///
/// Field values are strings, matching the PDF field value model. The store is
/// deliberately simple: it exists so `this.getField(...)` reads and writes can
/// be tested end to end without touching a document.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FieldStore {
    fields: BTreeMap<String, String>,
}

impl FieldStore {
    /// Creates an empty field store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the value of `name`, overwriting any previous value.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.fields.insert(name.into(), value.into());
    }

    /// Returns the value of `name`, or `None` when the field does not exist.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// Returns the value of `name`, or an empty string when it does not exist.
    ///
    /// This mirrors Acrobat's behavior for an empty or missing field value.
    pub fn value(&self, name: &str) -> String {
        self.get(name).unwrap_or("").to_owned()
    }

    /// Returns `true` when `name` has an entry, including an empty-string value.
    pub fn contains(&self, name: &str) -> bool {
        self.fields.contains_key(name)
    }

    /// Returns the field names in stable sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.fields.keys().map(String::as_str)
    }

    /// Returns the number of stored fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns `true` when no fields are stored.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// The Acrobat `event` object exposed to scripts while a context is injected.
///
/// Format scripts read and replace `value`; keystroke scripts inspect `key`
/// and set `rc` to accept or reject the change. `rc` defaults to `true`, as it
/// does in Acrobat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventData {
    /// The value being formatted, validated, or calculated.
    pub value: String,
    /// The keystroke character being tested (empty when not a keystroke).
    pub key: String,
    /// Whether the event is accepted (`true`) or rejected (`false`).
    pub rc: bool,
}

impl Default for EventData {
    fn default() -> Self {
        Self {
            value: String::new(),
            key: String::new(),
            rc: true,
        }
    }
}

/// An explicitly injected Acrobat-style runtime context.
///
/// Cloning shares the same underlying field store and event object, mirroring
/// how an Acrobat document and its current event are single shared instances.
#[derive(Debug, Clone)]
pub struct AcrobatContext {
    fields: Rc<RefCell<FieldStore>>,
    event: Rc<RefCell<EventData>>,
}

impl Default for AcrobatContext {
    fn default() -> Self {
        Self::new()
    }
}

impl AcrobatContext {
    /// Creates a context with an empty field store and a default event.
    pub fn new() -> Self {
        Self {
            fields: Rc::new(RefCell::new(FieldStore::new())),
            event: Rc::new(RefCell::new(EventData::default())),
        }
    }

    /// Seeds a field, returning `self` for chaining.
    pub fn with_field(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.set_field(name, value);
        self
    }

    /// Seeds the event value, returning `self` for chaining.
    pub fn with_event_value(self, value: impl Into<String>) -> Self {
        self.set_event_value(value);
        self
    }

    /// Seeds the event key, returning `self` for chaining.
    pub fn with_event_key(self, key: impl Into<String>) -> Self {
        self.set_event_key(key);
        self
    }

    // The shared state is `Rc<RefCell<_>>`, which is single-threaded. Every
    // borrow taken by the interpreter is confined to a short lexical scope and
    // host callbacks are invoked outside those scopes, so a host accessor can
    // never observe an outstanding borrow. The fallible `try_*` methods expose
    // contention as a typed error; the original infallible accessors delegate to
    // them and use a neutral fallback, so none of them can panic on a
    // `RefCell` borrow conflict. The regression test at the end of this module
    // exercises the only realistic re-entrancy path (a host callback holding a
    // cloned context) and confirms the invariant.

    /// Tries to set a field value.
    ///
    /// Returns a typed [`JsError::Host`] when the field store is already
    /// borrowed; see the module note above for why that cannot happen in normal
    /// use.
    pub fn try_set_field(
        &self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), JsError> {
        self.fields
            .try_borrow_mut()
            .map_err(|_| borrow_conflict("field store"))?
            .set(name, value);
        Ok(())
    }

    /// Sets the value of `name`, overwriting any previous value.
    ///
    /// Infallible convenience wrapper around [`AcrobatContext::try_set_field`];
    /// on the (unreachable-by-invariant) contention path it becomes a no-op
    /// instead of panicking.
    pub fn set_field(&self, name: impl Into<String>, value: impl Into<String>) {
        let _ = self.try_set_field(name, value);
    }

    /// Tries to read a field value, returning an empty string for a missing
    /// field. Returns a typed error if the field store is already borrowed.
    pub fn try_field_value(&self, name: &str) -> Result<String, JsError> {
        let fields = self
            .fields
            .try_borrow()
            .map_err(|_| borrow_conflict("field store"))?;
        Ok(fields.value(name))
    }

    /// Reads a field value, returning an empty string for a missing field.
    ///
    /// Infallible wrapper around [`AcrobatContext::try_field_value`]; returns an
    /// empty string if the invariant is ever broken rather than panicking.
    pub fn field_value(&self, name: &str) -> String {
        self.try_field_value(name).unwrap_or_default()
    }

    /// Tries to report whether a field with `name` exists.
    pub fn try_has_field(&self, name: &str) -> Result<bool, JsError> {
        let fields = self
            .fields
            .try_borrow()
            .map_err(|_| borrow_conflict("field store"))?;
        Ok(fields.contains(name))
    }

    /// Returns `true` when a field with `name` exists.
    ///
    /// Infallible wrapper around [`AcrobatContext::try_has_field`].
    pub fn has_field(&self, name: &str) -> bool {
        self.try_has_field(name).unwrap_or_default()
    }

    /// Tries to return a snapshot of all field values in stable name order.
    pub fn try_field_snapshot(&self) -> Result<Vec<(String, String)>, JsError> {
        let store = self
            .fields
            .try_borrow()
            .map_err(|_| borrow_conflict("field store"))?;
        Ok(store
            .names()
            .map(|name| (name.to_owned(), store.value(name)))
            .collect())
    }

    /// Returns a snapshot of all field values in stable name order.
    ///
    /// Infallible wrapper around [`AcrobatContext::try_field_snapshot`].
    pub fn field_snapshot(&self) -> Vec<(String, String)> {
        self.try_field_snapshot().unwrap_or_default()
    }

    /// Tries to set the event's `value` property.
    pub fn try_set_event_value(&self, value: impl Into<String>) -> Result<(), JsError> {
        self.event
            .try_borrow_mut()
            .map_err(|_| borrow_conflict("event data"))?
            .value = value.into();
        Ok(())
    }

    /// Sets the event's `value` property.
    ///
    /// Infallible wrapper around [`AcrobatContext::try_set_event_value`].
    pub fn set_event_value(&self, value: impl Into<String>) {
        let _ = self.try_set_event_value(value);
    }

    /// Tries to set the event's `key` property.
    pub fn try_set_event_key(&self, key: impl Into<String>) -> Result<(), JsError> {
        self.event
            .try_borrow_mut()
            .map_err(|_| borrow_conflict("event data"))?
            .key = key.into();
        Ok(())
    }

    /// Sets the event's `key` property.
    ///
    /// Infallible wrapper around [`AcrobatContext::try_set_event_key`].
    pub fn set_event_key(&self, key: impl Into<String>) {
        let _ = self.try_set_event_key(key);
    }

    /// Tries to return the event's current `value`.
    pub fn try_event_value(&self) -> Result<String, JsError> {
        let event = self
            .event
            .try_borrow()
            .map_err(|_| borrow_conflict("event data"))?;
        Ok(event.value.clone())
    }

    /// Returns the event's current `value`.
    ///
    /// Infallible wrapper around [`AcrobatContext::try_event_value`].
    pub fn event_value(&self) -> String {
        self.try_event_value().unwrap_or_default()
    }

    /// Tries to return the event's current `key`.
    pub fn try_event_key(&self) -> Result<String, JsError> {
        let event = self
            .event
            .try_borrow()
            .map_err(|_| borrow_conflict("event data"))?;
        Ok(event.key.clone())
    }

    /// Returns the event's current `key`.
    ///
    /// Infallible wrapper around [`AcrobatContext::try_event_key`].
    pub fn event_key(&self) -> String {
        self.try_event_key().unwrap_or_default()
    }

    /// Tries to return the event's acceptance flag.
    pub fn try_event_rc(&self) -> Result<bool, JsError> {
        let event = self
            .event
            .try_borrow()
            .map_err(|_| borrow_conflict("event data"))?;
        Ok(event.rc)
    }

    /// Returns the event's acceptance flag.
    ///
    /// Infallible wrapper around [`AcrobatContext::try_event_rc`].
    pub fn event_rc(&self) -> bool {
        // Acrobat's event.rc defaults to `true` (accept).
        self.try_event_rc().unwrap_or(true)
    }

    /// Returns a shared handle to the field store for interpreter use.
    pub(crate) fn fields_ref(&self) -> Rc<RefCell<FieldStore>> {
        Rc::clone(&self.fields)
    }

    /// Returns a shared handle to the event object for interpreter use.
    pub(crate) fn event_ref(&self) -> Rc<RefCell<EventData>> {
        Rc::clone(&self.event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_store_is_sorted_and_empty_missing_is_empty_string() {
        let mut store = FieldStore::new();
        store.set("b", "2");
        store.set("a", "1");
        assert_eq!(store.names().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(store.get("a"), Some("1"));
        assert_eq!(store.value("missing"), "");
        assert!(!store.contains("missing"));
        assert_eq!(store.len(), 2);
        assert!(!store.is_empty());
    }

    #[test]
    fn context_shares_state_across_clones() {
        let ctx = AcrobatContext::new().with_field("x", "1");
        let clone = ctx.clone();
        clone.set_field("y", "2");
        assert_eq!(ctx.field_value("y"), "2");
        ctx.set_event_key("a");
        assert_eq!(clone.event_key(), "a");
        assert!(clone.event_rc());
        let copy = AcrobatContext::default();
        assert!(copy.field_snapshot().is_empty());
    }

    #[test]
    fn fallible_accessors_report_borrow_contention_as_a_typed_error() {
        let context = AcrobatContext::new();
        // Hold both borrows from inside the module (private fields are visible
        // to this child module) to simulate re-entrant host access.
        let fields = context
            .fields
            .try_borrow_mut()
            .expect("initial field borrow");
        let event = context
            .event
            .try_borrow_mut()
            .expect("initial event borrow");
        assert!(matches!(
            context.try_set_field("x", "1"),
            Err(JsError::Host { .. })
        ));
        assert!(matches!(
            context.try_field_value("x"),
            Err(JsError::Host { .. })
        ));
        assert!(matches!(
            context.try_has_field("x"),
            Err(JsError::Host { .. })
        ));
        assert!(matches!(
            context.try_field_snapshot(),
            Err(JsError::Host { .. })
        ));
        assert!(matches!(
            context.try_set_event_value("v"),
            Err(JsError::Host { .. })
        ));
        assert!(matches!(
            context.try_set_event_key("k"),
            Err(JsError::Host { .. })
        ));
        assert!(matches!(
            context.try_event_value(),
            Err(JsError::Host { .. })
        ));
        assert!(matches!(context.try_event_key(), Err(JsError::Host { .. })));
        assert!(matches!(context.try_event_rc(), Err(JsError::Host { .. })));

        // The infallible wrappers degrade gracefully instead of panicking.
        context.set_field("x", "1");
        context.set_event_value("v");
        assert_eq!(context.field_value("x"), "");
        assert_eq!(context.event_value(), "");

        drop(event);
        drop(fields);
        context.set_field("x", "1");
        assert_eq!(context.try_field_value("x").expect("released"), "1");
        assert_eq!(context.try_event_value().expect("released"), "");
    }

    #[test]
    fn host_callback_can_reenter_the_context_during_a_script_run() {
        use crate::CancelToken;
        use crate::js::{HostObject, HostRegistry, Limits, Script, Value};
        use std::rc::Rc;

        #[derive(Debug)]
        struct ContextTouchingHost(AcrobatContext);

        impl HostObject for ContextTouchingHost {
            fn call(&self, method: &str, _args: &[Value]) -> Result<Value, JsError> {
                match method {
                    "seed" => {
                        self.0.try_set_field("from_host", "42")?;
                        self.0.try_set_event_value("host-event")?;
                        Ok(Value::Bool(true))
                    }
                    _ => Err(JsError::Host {
                        pos: Pos::default(),
                        message: format!("unknown method '{method}'"),
                    }),
                }
            }
        }

        let context = AcrobatContext::new().with_field("seed", "1");
        let mut host = HostRegistry::new();
        host.register("touch", Rc::new(ContextTouchingHost(context.clone())))
            .expect("registration succeeds");
        let script = Script::parse("touch.seed();").expect("valid script");
        let result = script
            .run_with_context(
                &Limits::default(),
                &CancelToken::new(),
                &host,
                Some(&context),
            )
            .expect("script succeeds");
        assert_eq!(result, Value::Bool(true));
        assert_eq!(
            context.try_field_value("from_host").expect("no borrow"),
            "42"
        );
        assert_eq!(context.try_event_value().expect("no borrow"), "host-event");
    }
}
