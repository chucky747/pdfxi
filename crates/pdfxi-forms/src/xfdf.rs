//! XFDF import: a bounded XML reader and field-type-aware value mapping.
//!
//! XFDF is the XML sibling of FDF: field values live in `<field name="...">`
//! elements with one or more `<value>` children. Unlike FDF, the XML carries no
//! field-type marker, so importing needs the target [`Form`] to decide whether a
//! value is a boolean state, a choice selection, or ordinary text. [`import_xfdf`]
//! therefore parses the document and maps it against a form in one step, while
//! [`parse_xfdf`] exposes the raw name-to-values document for callers that want
//! to defer mapping.
//!
//! The reader is hand-written and dependency-free. It accepts the XML subset
//! emitted by [`crate::export_xfdf`] plus XML declarations, comments, single- or
//! double-quoted attributes, the five predefined entities, numeric character
//! references, the optional XFDF namespace (default or prefixed), and nested
//! `<field>` elements whose partial names are joined with `.` like the AcroForm
//! loader. Malformed XML, a wrong namespace, and unknown field names are all
//! rejected with structured [`XfdfError`] values rather than panics.
//!
//! Export is provided by [`crate::export_xfdf`]; its output is consumed by this
//! importer, so text, checkbox, radio, choice, number, and signature values all
//! round-trip with the documented type mapping.

use crate::fdf::{FdfDocument, FdfField};
use crate::field::{Field, FieldType, FieldValue};
use crate::form::Form;

/// The XFDF XML namespace published by Adobe.
pub const XFDF_NAMESPACE: &str = "http://ns.adobe.com/xfdf/";

/// Maximum XML element nesting accepted before treating the input as hostile.
const MAX_XML_DEPTH: usize = 64;

/// A structured error produced while parsing or mapping XFDF.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XfdfError {
    /// The document had no root element.
    #[error("XFDF document has no <xfdf> root element")]
    MissingRoot,
    /// The root element was not named `xfdf`.
    #[error("expected an <xfdf> root element but found <{local}>")]
    UnexpectedRoot {
        /// Local name of the unexpected root element.
        local: String,
    },
    /// The XFDF namespace did not match the expected URI.
    #[error("unsupported XML namespace '{namespace}'; expected '{expected}'")]
    UnexpectedNamespace {
        /// Namespace URI that was found.
        namespace: String,
        /// Namespace URI that is required.
        expected: String,
    },
    /// A namespace prefix was used without a matching `xmlns:` declaration.
    #[error("undeclared XML namespace prefix '{prefix}'")]
    UndeclaredPrefix {
        /// The undeclared prefix.
        prefix: String,
    },
    /// The root had no `<fields>` child element.
    #[error("XFDF root has no <fields> element")]
    MissingFields,
    /// A `<field>` element had no `name` attribute.
    #[error("XFDF <field> at byte {offset} has no name attribute")]
    MissingFieldName {
        /// Byte offset of the offending element.
        offset: usize,
    },
    /// Two leaf fields resolved to the same fully qualified name.
    #[error("duplicate XFDF field name '{name}'")]
    DuplicateField {
        /// The duplicated fully qualified name.
        name: String,
    },
    /// A closing tag did not match its opening tag.
    #[error("closing tag '</{found}>' at byte {offset} does not match '</{expected}>'")]
    MismatchedTag {
        /// Name expected from the opening tag.
        expected: String,
        /// Name actually found.
        found: String,
        /// Byte offset of the closing tag.
        offset: usize,
    },
    /// An entity reference was not one of the predefined or numeric entities.
    #[error("unknown XML entity '&{entity};' at byte {offset}")]
    UnknownEntity {
        /// The entity name between `&` and `;`.
        entity: String,
        /// Byte offset of the entity.
        offset: usize,
    },
    /// The XML could not be parsed for a specific structural reason.
    #[error("malformed XFDF XML at byte {offset}: {detail}")]
    Malformed {
        /// Human-readable description of the problem.
        detail: &'static str,
        /// Byte offset where the problem was detected.
        offset: usize,
    },
    /// A field in the XFDF document does not exist in the target form.
    #[error("XFDF field '{name}' does not exist in the target form")]
    UnknownField {
        /// Fully qualified name of the unknown field.
        name: String,
    },
    /// A field carried more `<value>` elements than its type permits.
    #[error("field '{field}' has {count} <value> elements but {expected} accepts at most {max}")]
    TooManyValues {
        /// Fully qualified field name.
        field: String,
        /// Number of `<value>` elements found.
        count: usize,
        /// Field type that rejected the values.
        expected: &'static str,
        /// Maximum number of values the type accepts.
        max: usize,
    },
}

/// One raw XFDF field: a fully qualified name and its `<value>` texts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XfdfField {
    name: String,
    values: Vec<String>,
}

impl XfdfField {
    /// Returns the fully qualified field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the raw `<value>` texts in document order.
    pub fn values(&self) -> &[String] {
        &self.values
    }
}

/// The raw field data parsed from an XFDF document.
///
/// Values are stored as text until [`XfdfDocument::to_fdf`] maps them onto a
/// [`Form`]'s field types. Fields appear in document order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XfdfDocument {
    fields: Vec<XfdfField>,
}

impl XfdfDocument {
    /// Builds a document from raw fields.
    pub fn new(fields: Vec<XfdfField>) -> Self {
        Self { fields }
    }

    /// Returns the fields in document order.
    pub fn fields(&self) -> &[XfdfField] {
        &self.fields
    }

    /// Returns the number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns `true` when the document has no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Maps raw values onto field values using `form` as the type authority.
    ///
    /// Mapping follows the inverse of [`crate::export_xfdf`]:
    ///
    /// - Checkbox: the first value is `On`/`Yes`/`True`/`1` (with an optional
    ///   leading slash) for checked, anything else for unchecked; no value means
    ///   unchecked. At most one value is allowed.
    /// - Radio: the first value is the selected option name when it is one of
    ///   the field's declared states; at most one value.
    /// - Choice: every value that matches a declared choice is selected. Values
    ///   are matched by exact membership, never by spelling, so an option
    ///   literally named `No`, `Off`, `0` or `False` is preserved while an
    ///   undeclared `Off` state is simply not selected. Fields with no declared
    ///   choices keep their values literally.
    /// - Text, signature and unknown fields: the single value is text (or a
    ///   number when the form field already holds a number and the text parses
    ///   as a finite number). At most one value is allowed.
    /// - Pushbutton: always empty.
    ///
    /// A name absent from the form is reported as [`XfdfError::UnknownField`].
    ///
    /// # Errors
    ///
    /// Returns [`XfdfError`] for unknown fields or for scalar fields that carry
    /// more than one `<value>` element.
    pub fn to_fdf(&self, form: &Form) -> Result<FdfDocument, XfdfError> {
        let mut fields = Vec::with_capacity(self.fields.len());
        for raw in &self.fields {
            let field = form
                .field(&raw.name)
                .ok_or_else(|| XfdfError::UnknownField {
                    name: raw.name.clone(),
                })?;
            let value = map_value(field, &raw.name, &raw.values)?;
            fields.push(FdfField::new(raw.name.clone(), value));
        }
        Ok(FdfDocument::new(fields))
    }
}

/// Imports XFDF text, mapping values against `form` into an [`FdfDocument`]
/// that can be applied with [`crate::FormFill::fill_from_fdf`].
///
/// # Errors
///
/// Returns [`XfdfError`] for malformed XML, an unexpected namespace, unknown
/// field names, or values that do not fit the mapped field's type.
pub fn import_xfdf(input: &str, form: &Form) -> Result<FdfDocument, XfdfError> {
    parse_xfdf(input)?.to_fdf(form)
}

/// Parses XFDF text into a raw [`XfdfDocument`] without consulting a form.
///
/// Use [`XfdfDocument::to_fdf`] or [`import_xfdf`] to turn the raw strings into
/// typed field values.
///
/// # Errors
///
/// Returns [`XfdfError`] for malformed XML, an unexpected element name or
/// namespace, or a `<field>` without a `name` attribute.
pub fn parse_xfdf(input: &str) -> Result<XfdfDocument, XfdfError> {
    let mut parser = XmlParser::new(input.as_bytes());
    let root = parser.parse_document()?;
    if root.local.as_str() != "xfdf" {
        return Err(XfdfError::UnexpectedRoot {
            local: root.local.clone(),
        });
    }
    check_namespaces(&root)?;

    let fields_element = root
        .children
        .iter()
        .find_map(|node| match node {
            XmlNode::Element(element) if element.local == "fields" => Some(element),
            _ => None,
        })
        .ok_or(XfdfError::MissingFields)?;

    let mut fields = Vec::new();
    for node in &fields_element.children {
        if let XmlNode::Element(element) = node
            && element.local == "field"
        {
            collect_fields(element, "", &mut fields)?;
        }
    }
    Ok(XfdfDocument { fields })
}

/// Namespace bindings visible at one point in the document tree.
#[derive(Debug, Clone, Default)]
struct NamespaceScope {
    /// Prefixes explicitly bound to the XFDF namespace in this scope.
    prefixes: std::collections::BTreeSet<String>,
}

/// Validates the namespace of every element and attribute, not just the root.
///
/// Every qualified name must use either the built-in `xml` prefix or a prefix
/// bound (on that element or an ancestor) to [`XFDF_NAMESPACE`]. Default
/// namespace declarations must likewise be empty or the XFDF namespace. This
/// rejects foreign-prefixed nodes such as `<f:field f:name="…">` while still
/// accepting them when `f` is explicitly resolved to the XFDF namespace.
fn check_namespaces(root: &XmlElement) -> Result<(), XfdfError> {
    validate_namespaces(root, &NamespaceScope::default())
}

/// Recursively applies [`check_namespaces`] rules with inherited bindings.
fn validate_namespaces(element: &XmlElement, inherited: &NamespaceScope) -> Result<(), XfdfError> {
    let mut scope = inherited.clone();
    for (key, value) in &element.attrs {
        if let Some(prefix) = key.strip_prefix("xmlns:") {
            if value != XFDF_NAMESPACE {
                return Err(XfdfError::UnexpectedNamespace {
                    namespace: value.clone(),
                    expected: XFDF_NAMESPACE.to_owned(),
                });
            }
            scope.prefixes.insert(prefix.to_owned());
        } else if key == "xmlns" && !value.is_empty() && value != XFDF_NAMESPACE {
            return Err(XfdfError::UnexpectedNamespace {
                namespace: value.clone(),
                expected: XFDF_NAMESPACE.to_owned(),
            });
        }
    }
    if let Some(prefix) = element.prefix.as_deref()
        && prefix != "xml"
        && !scope.prefixes.contains(prefix)
    {
        return Err(XfdfError::UndeclaredPrefix {
            prefix: prefix.to_owned(),
        });
    }
    for (key, _) in &element.attrs {
        if key == "xmlns" || key.starts_with("xmlns:") {
            continue;
        }
        if let Some((prefix, _)) = key.split_once(':')
            && prefix != "xml"
            && !scope.prefixes.contains(prefix)
        {
            return Err(XfdfError::UndeclaredPrefix {
                prefix: prefix.to_owned(),
            });
        }
    }
    for node in &element.children {
        if let XmlNode::Element(child) = node {
            validate_namespaces(child, &scope)?;
        }
    }
    Ok(())
}

/// Recursively collects leaf `<field>` elements, joining nested names with `.`.
fn collect_fields(
    element: &XmlElement,
    parent: &str,
    out: &mut Vec<XfdfField>,
) -> Result<(), XfdfError> {
    let partial = element
        .attr_value("name")
        .ok_or(XfdfError::MissingFieldName {
            offset: element.offset,
        })?;
    let name = if parent.is_empty() {
        partial.to_owned()
    } else {
        format!("{parent}.{partial}")
    };

    let mut values = Vec::new();
    let mut children = Vec::new();
    for node in &element.children {
        if let XmlNode::Element(child) = node {
            match child.local.as_str() {
                "value" => {
                    let mut text = String::new();
                    gather_text(child, &mut text);
                    values.push(text);
                }
                "field" => children.push(child),
                _ => {}
            }
        }
    }

    // A leaf field is emitted even with no <value> children so that an empty
    // field round-trips as FieldValue::Empty rather than disappearing.
    if children.is_empty() {
        if out.iter().any(|field| field.name == name) {
            return Err(XfdfError::DuplicateField { name });
        }
        out.push(XfdfField { name, values });
    } else {
        if !values.is_empty() {
            if out.iter().any(|field| field.name == name) {
                return Err(XfdfError::DuplicateField { name });
            }
            out.push(XfdfField {
                name: name.clone(),
                values,
            });
        }
        for child in children {
            collect_fields(child, &name, out)?;
        }
    }
    Ok(())
}

/// Appends every descendant text node of `element` to `out`.
fn gather_text(element: &XmlElement, out: &mut String) {
    for node in &element.children {
        match node {
            XmlNode::Text(text) => out.push_str(text),
            XmlNode::Element(child) => gather_text(child, out),
        }
    }
}

/// Maps raw XFDF strings onto the variant expected by `field`.
fn map_value(field: &Field, name: &str, values: &[String]) -> Result<FieldValue, XfdfError> {
    let too_many = |expected: &'static str, max: usize| XfdfError::TooManyValues {
        field: name.to_owned(),
        count: values.len(),
        expected,
        max,
    };

    match field.field_type() {
        FieldType::CheckBox => {
            if values.len() > 1 {
                return Err(too_many("checkbox", 1));
            }
            Ok(FieldValue::Bool(
                values.first().is_some_and(|value| is_on(value)),
            ))
        }
        FieldType::Radio => {
            if values.len() > 1 {
                return Err(too_many("radio", 1));
            }
            let Some(raw) = values.first() else {
                return Ok(FieldValue::Empty);
            };
            let selected = strip_slash(raw);
            if selected.is_empty() || !is_declared_choice(field, &selected) {
                return Ok(FieldValue::Empty);
            }
            Ok(FieldValue::Choice(vec![selected]))
        }
        FieldType::Choice => {
            let selected: Vec<String> = values
                .iter()
                .map(|value| strip_slash(value))
                .filter(|value| !value.is_empty() && is_declared_choice(field, value))
                .collect();
            if selected.is_empty() {
                Ok(FieldValue::Empty)
            } else {
                Ok(FieldValue::Choice(selected))
            }
        }
        FieldType::Text | FieldType::Signature | FieldType::Unknown => {
            if values.len() > 1 {
                return Err(too_many("text", 1));
            }
            let Some(text) = values.first() else {
                return Ok(FieldValue::Empty);
            };
            if matches!(field.value(), FieldValue::Number(_))
                && let Ok(number) = text.parse::<f64>()
                && number.is_finite()
            {
                return Ok(FieldValue::Number(number));
            }
            Ok(FieldValue::Text(text.clone()))
        }
        FieldType::Button => Ok(FieldValue::Empty),
    }
}

/// Returns `true` for the XFDF tokens that mean a checked/on state.
fn is_on(value: &str) -> bool {
    matches!(
        strip_slash(value).to_ascii_lowercase().as_str(),
        "on" | "yes" | "true" | "1"
    )
}

/// Returns `true` when `value` is selectable for `field`.
///
/// Selection is decided by exact membership in the field's declared choices or
/// radio states, never by the literal spelling of "off"-like tokens. A field
/// with no declared choices has no membership information, so its values are
/// preserved literally. This lets an option actually named `No`, `Off`, `0` or
/// `False` round-trip, while an undeclared `Off` state is not selected.
fn is_declared_choice(field: &Field, value: &str) -> bool {
    let declared = field.choices();
    declared.is_empty() || declared.iter().any(|choice| choice == value)
}

/// Removes surrounding whitespace and all leading slashes from a value.
///
/// `trim_start_matches('/')` is deliberately greedy: both `/On` and `//On`
/// map to `On`. The function is used for XFDF state and option names, which
/// may be written with a leading PDF-name slash by some producers.
fn strip_slash(value: &str) -> String {
    value.trim().trim_start_matches('/').to_owned()
}

// ---------------------------------------------------------------------------
// Minimal XML reader
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum XmlNode {
    Element(XmlElement),
    Text(String),
}

#[derive(Debug, Clone)]
struct XmlElement {
    prefix: Option<String>,
    local: String,
    qualified_name: String,
    attrs: Vec<(String, String)>,
    children: Vec<XmlNode>,
    offset: usize,
}

impl XmlElement {
    /// Finds an attribute by qualified name or by local name.
    fn attr_value(&self, wanted: &str) -> Option<&str> {
        self.attrs.iter().find_map(|(key, value)| {
            if key == wanted {
                return Some(value.as_str());
            }
            if !wanted.contains(':')
                && key
                    .split_once(':')
                    .is_some_and(|(_, local)| local == wanted)
            {
                return Some(value.as_str());
            }
            None
        })
    }
}

struct XmlParser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> XmlParser<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            pos: 0,
            depth: 0,
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn starts_with(&self, needle: &[u8]) -> bool {
        self.src
            .get(self.pos..)
            .is_some_and(|tail| tail.starts_with(needle))
    }

    fn skip_ws(&mut self) {
        while let Some(byte) = self.peek()
            && byte.is_ascii_whitespace()
        {
            self.pos += 1;
        }
    }

    /// Skips whitespace, comments and processing instructions between elements.
    fn skip_misc(&mut self) -> Result<(), XfdfError> {
        loop {
            self.skip_ws();
            if self.starts_with(b"<!--") {
                self.skip_until(b"-->", "unterminated comment")?;
            } else if self.starts_with(b"<?") {
                self.skip_until(b"?>", "unterminated processing instruction")?;
            } else {
                return Ok(());
            }
        }
    }

    fn skip_until(&mut self, marker: &[u8], detail: &'static str) -> Result<(), XfdfError> {
        let start = self.pos;
        loop {
            if self.at_end() {
                return Err(XfdfError::Malformed {
                    detail,
                    offset: start,
                });
            }
            if self.starts_with(marker) {
                self.pos += marker.len();
                return Ok(());
            }
            self.pos += 1;
        }
    }

    fn parse_document(&mut self) -> Result<XmlElement, XfdfError> {
        self.skip_misc()?;
        if self.at_end() {
            return Err(XfdfError::MissingRoot);
        }
        let root = self.parse_element()?;
        self.skip_misc()?;
        if !self.at_end() {
            return Err(XfdfError::Malformed {
                detail: "trailing data after root element",
                offset: self.pos,
            });
        }
        Ok(root)
    }

    fn parse_element(&mut self) -> Result<XmlElement, XfdfError> {
        if self.depth >= MAX_XML_DEPTH {
            return Err(XfdfError::Malformed {
                detail: "element nesting too deep",
                offset: self.pos,
            });
        }
        let offset = self.pos;
        if self.peek() != Some(b'<') {
            return Err(XfdfError::Malformed {
                detail: "expected '<'",
                offset,
            });
        }
        self.pos += 1;
        let qualified_name = self.read_name("element name")?;
        let (prefix, local) = match qualified_name.split_once(':') {
            Some((prefix, local)) => (Some(prefix.to_owned()), local.to_owned()),
            None => (None, qualified_name.clone()),
        };

        let mut attrs = Vec::new();
        let mut self_closing = false;
        loop {
            self.skip_ws();
            if self.at_end() {
                return Err(XfdfError::Malformed {
                    detail: "unclosed start tag",
                    offset,
                });
            }
            if self.starts_with(b"/>") {
                self.pos += 2;
                self_closing = true;
                break;
            }
            if self.peek() == Some(b'>') {
                self.pos += 1;
                break;
            }
            let key = self.read_name("attribute name")?;
            self.skip_ws();
            if self.peek() != Some(b'=') {
                return Err(XfdfError::Malformed {
                    detail: "expected '=' after attribute name",
                    offset: self.pos,
                });
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.read_attribute_value()?;
            attrs.push((key, value));
        }

        let mut element = XmlElement {
            prefix,
            local,
            qualified_name,
            attrs,
            children: Vec::new(),
            offset,
        };
        if self_closing {
            return Ok(element);
        }

        self.depth += 1;
        let result = self.parse_children(&mut element);
        self.depth -= 1;
        result?;
        Ok(element)
    }

    fn parse_children(&mut self, element: &mut XmlElement) -> Result<(), XfdfError> {
        loop {
            if self.at_end() {
                return Err(XfdfError::Malformed {
                    detail: "unclosed element",
                    offset: element.offset,
                });
            }
            if self.starts_with(b"</") {
                self.pos += 2;
                let closing = self.read_name("closing tag name")?;
                self.skip_ws();
                if self.peek() != Some(b'>') {
                    return Err(XfdfError::Malformed {
                        detail: "expected '>' after closing tag",
                        offset: self.pos,
                    });
                }
                self.pos += 1;
                if closing != element.qualified_name {
                    return Err(XfdfError::MismatchedTag {
                        expected: element.qualified_name.clone(),
                        found: closing,
                        offset: self.pos,
                    });
                }
                return Ok(());
            }
            if self.peek() == Some(b'<') {
                if self.starts_with(b"<!--") {
                    self.skip_until(b"-->", "unterminated comment")?;
                } else if self.starts_with(b"<?") {
                    self.skip_until(b"?>", "unterminated processing instruction")?;
                } else {
                    element
                        .children
                        .push(XmlNode::Element(self.parse_element()?));
                }
            } else {
                let start = self.pos;
                while let Some(byte) = self.peek()
                    && byte != b'<'
                {
                    self.pos += 1;
                }
                let text = decode_entities(&self.src[start..self.pos], start)?;
                if !text.is_empty() {
                    element.children.push(XmlNode::Text(text));
                }
            }
        }
    }

    fn read_name(&mut self, detail: &'static str) -> Result<String, XfdfError> {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() || matches!(byte, b'<' | b'>' | b'/' | b'=' | b'\'') {
                break;
            }
            if byte == b'"' && self.pos == start {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(XfdfError::Malformed {
                detail,
                offset: start,
            });
        }
        let raw = &self.src[start..self.pos];
        if !raw
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-' | b'.'))
        {
            return Err(XfdfError::Malformed {
                detail: "invalid XML name",
                offset: start,
            });
        }
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    fn read_attribute_value(&mut self) -> Result<String, XfdfError> {
        let quote = self.peek().ok_or(XfdfError::Malformed {
            detail: "missing attribute value",
            offset: self.pos,
        })?;
        if quote != b'"' && quote != b'\'' {
            return Err(XfdfError::Malformed {
                detail: "attribute value must be quoted",
                offset: self.pos,
            });
        }
        self.pos += 1;
        let start = self.pos;
        while let Some(byte) = self.peek()
            && byte != quote
        {
            self.pos += 1;
        }
        if self.peek() != Some(quote) {
            return Err(XfdfError::Malformed {
                detail: "unterminated attribute value",
                offset: start,
            });
        }
        let raw = &self.src[start..self.pos];
        self.pos += 1;
        decode_entities(raw, start)
    }
}

/// Decodes the five predefined XML entities and numeric character references.
fn decode_entities(raw: &[u8], base: usize) -> Result<String, XfdfError> {
    let mut out = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] != b'&' {
            out.push(raw[index]);
            index += 1;
            continue;
        }
        let entity_start = base + index;
        index += 1;
        let name_start = index;
        while index < raw.len() && raw[index] != b';' {
            if raw[index] == b'&' {
                return Err(XfdfError::Malformed {
                    detail: "unterminated entity",
                    offset: entity_start,
                });
            }
            index += 1;
        }
        if index == raw.len() {
            return Err(XfdfError::Malformed {
                detail: "unterminated entity",
                offset: entity_start,
            });
        }
        let name = &raw[name_start..index];
        index += 1;
        let decoded = match name {
            b"amp" => '&',
            b"lt" => '<',
            b"gt" => '>',
            b"quot" => '"',
            b"apos" => '\'',
            _ => {
                let number =
                    parse_numeric_entity(name).ok_or_else(|| XfdfError::UnknownEntity {
                        entity: String::from_utf8_lossy(name).into_owned(),
                        offset: entity_start,
                    })?;
                let ch = char::from_u32(number).ok_or(XfdfError::Malformed {
                    detail: "invalid numeric entity",
                    offset: entity_start,
                })?;
                let mut buffer = [0_u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                continue;
            }
        };
        let mut buffer = [0_u8; 4];
        out.extend_from_slice(decoded.encode_utf8(&mut buffer).as_bytes());
    }
    String::from_utf8(out).map_err(|_| XfdfError::Malformed {
        detail: "invalid UTF-8 in XML text",
        offset: base,
    })
}

/// Parses `&#123;` and `&#x1F600;` style numeric character references.
fn parse_numeric_entity(name: &[u8]) -> Option<u32> {
    let (digits, radix) = match name.strip_prefix(b"#")? {
        rest @ [b'x' | b'X', ..] => (&rest[1..], 16),
        rest => (rest, 10),
    };
    if digits.is_empty() {
        return None;
    }
    let text = std::str::from_utf8(digits).ok()?;
    u32::from_str_radix(text, radix).ok()
}

#[cfg(test)]
mod tests;
