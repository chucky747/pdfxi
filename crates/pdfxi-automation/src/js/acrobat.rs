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

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

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

    /// Sets a field value.
    ///
    /// # Panics
    ///
    /// Never in practice: the field store is borrowed only inside a synchronous
    /// script run, so no outstanding borrow can exist when the host calls this.
    pub fn set_field(&self, name: impl Into<String>, value: impl Into<String>) {
        self.fields.borrow_mut().set(name, value);
    }

    /// Reads a field value, returning an empty string for a missing field.
    ///
    /// # Panics
    ///
    /// Never in practice; see [`AcrobatContext::set_field`].
    pub fn field_value(&self, name: &str) -> String {
        self.fields.borrow().value(name)
    }

    /// Returns `true` when a field with `name` exists.
    ///
    /// # Panics
    ///
    /// Never in practice; see [`AcrobatContext::set_field`].
    pub fn has_field(&self, name: &str) -> bool {
        self.fields.borrow().contains(name)
    }

    /// Returns a snapshot of all field values in stable name order.
    ///
    /// # Panics
    ///
    /// Never in practice; see [`AcrobatContext::set_field`].
    pub fn field_snapshot(&self) -> Vec<(String, String)> {
        let store = self.fields.borrow();
        store
            .names()
            .map(|name| (name.to_owned(), store.value(name)))
            .collect()
    }

    /// Sets the event's `value` property.
    ///
    /// # Panics
    ///
    /// Never in practice; see [`AcrobatContext::set_field`].
    pub fn set_event_value(&self, value: impl Into<String>) {
        self.event.borrow_mut().value = value.into();
    }

    /// Sets the event's `key` property.
    ///
    /// # Panics
    ///
    /// Never in practice; see [`AcrobatContext::set_field`].
    pub fn set_event_key(&self, key: impl Into<String>) {
        self.event.borrow_mut().key = key.into();
    }

    /// Returns the event's current `value`.
    ///
    /// # Panics
    ///
    /// Never in practice; see [`AcrobatContext::set_field`].
    pub fn event_value(&self) -> String {
        self.event.borrow().value.clone()
    }

    /// Returns the event's current `key`.
    ///
    /// # Panics
    ///
    /// Never in practice; see [`AcrobatContext::set_field`].
    pub fn event_key(&self) -> String {
        self.event.borrow().key.clone()
    }

    /// Returns the event's acceptance flag.
    ///
    /// # Panics
    ///
    /// Never in practice; see [`AcrobatContext::set_field`].
    pub fn event_rc(&self) -> bool {
        self.event.borrow().rc
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
}
