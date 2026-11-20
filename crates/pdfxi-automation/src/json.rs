//! A small, deterministic JSON value, parser, and serializer.
//!
//! The local automation API is line-delimited: one compact JSON value per line.
//! This module implements the subset it needs without an external dependency,
//! matching the rest of this crate's dependency-free style. The parser is
//! strict (RFC 8259 number/string grammar), bounds depth and node count, and
//! never panics on hostile input. The serializer emits a canonical form:
//! object keys are sorted, there is no insignificant whitespace, strings are
//! escaped deterministically, and numbers keep their original spelling. Two
//! equal values therefore always serialize to identical bytes.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;

/// Maximum nesting depth accepted by the parser.
pub const MAX_DEPTH: usize = 64;
/// Maximum number of value nodes accepted by the parser.
pub const MAX_NODES: usize = 65_536;

/// A canonical JSON value.
///
/// Numbers retain their source spelling so serialization is lossless; callers
/// that need a numeric value use [`JsonValue::as_u64`]. Objects use a
/// [`BTreeMap`] so iteration and serialization are sorted by key.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    /// The JSON `null` value.
    Null,
    /// A JSON boolean.
    Bool(bool),
    /// A JSON number, stored exactly as written.
    Number(String),
    /// A JSON string.
    String(String),
    /// An ordered JSON array.
    Array(Vec<JsonValue>),
    /// A JSON object with sorted keys.
    Object(BTreeMap<String, JsonValue>),
}

impl JsonValue {
    /// Parses one complete JSON value from `text`.
    ///
    /// Trailing non-whitespace is rejected. Depth and node count are bounded by
    /// [`MAX_DEPTH`] and [`MAX_NODES`]. Every failure is a typed [`JsonError`].
    pub fn parse(text: &str) -> Result<Self, JsonError> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            pos: 0,
            depth: 0,
            nodes: 0,
        };
        parser.skip_ws();
        let value = parser.parse_value()?;
        parser.skip_ws();
        if parser.pos != parser.bytes.len() {
            return Err(parser.error("trailing characters after JSON value"));
        }
        Ok(value)
    }

    /// Returns the string content when this is a [`JsonValue::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    /// Returns the object entries when this is a [`JsonValue::Object`].
    pub fn as_object(&self) -> Option<&BTreeMap<String, JsonValue>> {
        match self {
            Self::Object(entries) => Some(entries),
            _ => None,
        }
    }

    /// Returns the array elements when this is a [`JsonValue::Array`].
    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            Self::Array(values) => Some(values.as_slice()),
            _ => None,
        }
    }

    /// Returns the value of a key when this is an object.
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.as_object().and_then(|entries| entries.get(key))
    }

    /// Returns `true` when this is [`JsonValue::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Parses a non-negative integer number value (or numeric string) to `u64`.
    ///
    /// Used for the protocol version field, which must be an exact integer.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(text) => text.parse::<u64>().ok(),
            _ => None,
        }
    }

    /// Builds an object from key/value pairs.
    pub fn object<I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (String, JsonValue)>,
    {
        Self::Object(pairs.into_iter().collect())
    }

    /// Builds a non-negative integer number value.
    pub fn number(value: u64) -> Self {
        Self::Number(value.to_string())
    }
}

impl From<&str> for JsonValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for JsonValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<bool> for JsonValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<u64> for JsonValue {
    fn from(value: u64) -> Self {
        Self::number(value)
    }
}

impl From<usize> for JsonValue {
    fn from(value: usize) -> Self {
        Self::number(value as u64)
    }
}

impl From<Vec<String>> for JsonValue {
    fn from(values: Vec<String>) -> Self {
        Self::Array(values.into_iter().map(Self::String).collect())
    }
}

impl From<Vec<JsonValue>> for JsonValue {
    fn from(values: Vec<JsonValue>) -> Self {
        Self::Array(values)
    }
}

impl fmt::Display for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        write_value(self, &mut out);
        f.write_str(&out)
    }
}

/// A parse failure with a byte position and a human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    /// Zero-based byte position where the problem was detected.
    pub position: usize,
    /// Short explanation of the problem.
    pub reason: String,
}

impl JsonError {
    /// Creates an error at `position` with the given reason.
    pub fn new(position: usize, reason: impl Into<String>) -> Self {
        Self {
            position,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid JSON at byte {}: {}", self.position, self.reason)
    }
}

impl std::error::Error for JsonError {}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
    nodes: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, reason: impl Into<String>) -> JsonError {
        JsonError::new(self.pos, reason)
    }

    fn skip_ws(&mut self) {
        while let Some(byte) = self.bytes.get(self.pos) {
            if matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, JsonError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("nesting depth exceeded"));
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(self.error("too many JSON nodes"));
        }
        let result = self.parse_value_inner();
        self.depth -= 1;
        result
    }

    fn parse_value_inner(&mut self) -> Result<JsonValue, JsonError> {
        let byte = match self.bytes.get(self.pos) {
            Some(byte) => *byte,
            None => return Err(self.error("expected a JSON value, found end of input")),
        };
        match byte {
            b'n' => {
                self.expect_literal(b"null")?;
                Ok(JsonValue::Null)
            }
            b't' => {
                self.expect_literal(b"true")?;
                Ok(JsonValue::Bool(true))
            }
            b'f' => {
                self.expect_literal(b"false")?;
                Ok(JsonValue::Bool(false))
            }
            b'"' => Ok(JsonValue::String(self.parse_string()?)),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            other => Err(self.error(format!("unexpected byte 0x{other:02x}"))),
        }
    }

    fn expect_literal(&mut self, literal: &[u8]) -> Result<(), JsonError> {
        if self.bytes.get(self.pos..self.pos + literal.len()) == Some(literal) {
            self.pos += literal.len();
            Ok(())
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        self.pos += 1; // opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = match self.bytes.get(self.pos) {
                Some(byte) => *byte,
                None => return Err(self.error("unterminated string")),
            };
            self.pos += 1;
            match byte {
                b'"' => {
                    return String::from_utf8(out)
                        .map_err(|_| self.error("string is not valid UTF-8"));
                }
                b'\\' => self.parse_escape(&mut out)?,
                0x00..=0x1f => return Err(self.error("unescaped control character in string")),
                other => out.push(other),
            }
        }
    }

    fn parse_escape(&mut self, out: &mut Vec<u8>) -> Result<(), JsonError> {
        let escaped = match self.bytes.get(self.pos) {
            Some(byte) => *byte,
            None => return Err(self.error("unterminated escape sequence")),
        };
        self.pos += 1;
        match escaped {
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            b'/' => out.push(b'/'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'u' => {
                let first = self.parse_hex4()?;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    if self.bytes.get(self.pos) != Some(&b'\\')
                        || self.bytes.get(self.pos + 1) != Some(&b'u')
                    {
                        return Err(self.error("high surrogate without a low surrogate"));
                    }
                    self.pos += 2;
                    let second = self.parse_hex4()?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(self.error("invalid low surrogate"));
                    }
                    0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return Err(self.error("unexpected low surrogate"));
                } else {
                    first
                };
                let ch = char::from_u32(scalar)
                    .ok_or_else(|| self.error("invalid Unicode scalar value"))?;
                let mut buffer = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            }
            other => {
                return Err(self.error(format!("invalid escape '\\{}'", other as char)));
            }
        }
        Ok(())
    }

    fn parse_hex4(&mut self) -> Result<u32, JsonError> {
        let slice = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.error("truncated Unicode escape"))?;
        let mut value = 0u32;
        for (index, byte) in slice.iter().copied().enumerate() {
            let digit = match byte {
                b'0'..=b'9' => (byte - b'0') as u32,
                b'a'..=b'f' => (byte - b'a' + 10) as u32,
                b'A'..=b'F' => (byte - b'A' + 10) as u32,
                _ => return Err(self.error("invalid hex digit in Unicode escape")),
            };
            let _ = index;
            value = value * 16 + digit;
        }
        self.pos += 4;
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<JsonValue, JsonError> {
        let start = self.pos;
        if self.bytes.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        match self.bytes.get(self.pos) {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                    return Err(self.error("leading zero in number"));
                }
            }
            Some(b'1'..=b'9') => {
                self.pos += 1;
                while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.error("invalid number")),
        }
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                return Err(self.error("expected a digit after '.'"));
            }
            while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                return Err(self.error("expected a digit in exponent"));
            }
            while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.error("invalid number encoding"))?;
        Ok(JsonValue::Number(text.to_owned()))
    }

    fn parse_array(&mut self) -> Result<JsonValue, JsonError> {
        self.pos += 1; // '['
        let mut values = Vec::new();
        self.skip_ws();
        if self.bytes.get(self.pos) == Some(&b']') {
            self.pos += 1;
            return Ok(JsonValue::Array(values));
        }
        loop {
            values.push(self.parse_value()?);
            self.skip_ws();
            match self.bytes.get(self.pos) {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(JsonValue::Array(values));
                }
                _ => return Err(self.error("expected ',' or ']' in array")),
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue, JsonError> {
        self.pos += 1; // '{'
        let mut entries = BTreeMap::new();
        self.skip_ws();
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Ok(JsonValue::Object(entries));
        }
        loop {
            self.skip_ws();
            if self.bytes.get(self.pos) != Some(&b'"') {
                return Err(self.error("expected a string key in object"));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.bytes.get(self.pos) != Some(&b':') {
                return Err(self.error("expected ':' after object key"));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.parse_value()?;
            if entries.insert(key.clone(), value).is_some() {
                return Err(self.error(format!("duplicate object key '{key}'")));
            }
            self.skip_ws();
            match self.bytes.get(self.pos) {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(JsonValue::Object(entries));
                }
                _ => return Err(self.error("expected ',' or '}' in object")),
            }
        }
    }
}

/// Appends the canonical compact encoding of `value` to `out`.
fn write_value(value: &JsonValue, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(true) => out.push_str("true"),
        JsonValue::Bool(false) => out.push_str("false"),
        JsonValue::Number(text) => out.push_str(text),
        JsonValue::String(text) => write_string(text, out),
        JsonValue::Array(values) => {
            out.push('[');
            for (index, item) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        JsonValue::Object(entries) => {
            out.push('{');
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(item, out);
            }
            out.push('}');
        }
    }
}

/// Appends a deterministically escaped JSON string literal to `out`.
fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other < '\u{20}' => {
                let _ = write!(out, "\\u{:04x}", other as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_serialize_round_trip_is_canonical() {
        let text = "{\"z\":1,\"a\":[true,null,-2.5e+3,\"x\\ny\"],\"m\":{}}";
        let value = JsonValue::parse(text).expect("valid JSON");
        let canonical = value.to_string();
        assert_eq!(
            canonical,
            "{\"a\":[true,null,-2.5e+3,\"x\\ny\"],\"m\":{},\"z\":1}"
        );
        assert_eq!(JsonValue::parse(&canonical).expect("canonical"), value);
    }

    #[test]
    fn malformed_inputs_are_rejected_without_panicking() {
        for text in [
            "",
            "{",
            "[1,]",
            "{\"a\":1,}",
            "{\"a\":1 \"b\":2}",
            "{\"a\":1,\"a\":2}",
            "01",
            "1.",
            "1e",
            "\"unterminated",
            "\"\\q\"",
            "\"\\uD800\"",
            "true false",
            "{\"a\": 1} trailing",
        ] {
            assert!(
                JsonValue::parse(text).is_err(),
                "expected rejection for {text:?}"
            );
        }
    }

    #[test]
    fn depth_and_trailing_input_are_bounded() {
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        assert!(JsonValue::parse(&deep).is_err(), "depth must be bounded");
        assert!(JsonValue::parse("[1,2,3]").is_ok());
    }

    #[test]
    fn unicode_escapes_and_control_characters_are_handled() {
        let value = JsonValue::parse("\"\\uD83D\\uDE00\\u0001\"").expect("valid escapes");
        assert_eq!(value.as_str(), Some("\u{1f600}\u{1}"));
        assert_eq!(value.to_string(), "\"\u{1f600}\\u0001\"");
    }

    #[test]
    fn numeric_and_string_spelling_are_preserved() {
        let value = JsonValue::parse("{\"i\":1,\"f\":1.0,\"s\":\"1\"}").expect("valid");
        assert_eq!(value.get("i").and_then(JsonValue::as_u64), Some(1));
        assert_eq!(value.get("f").and_then(JsonValue::as_u64), None);
        assert_eq!(value.get("s").and_then(JsonValue::as_str), Some("1"));
    }
}
