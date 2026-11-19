//! Bounds-checked tokenizer for PDF object syntax.
//!
//! Every read goes through [`Lexer::byte`], so malformed or truncated input
//! produces an error rather than a panic. Recursion is bounded by `MAX_DEPTH`
//! and container sizes are bounded so hostile files cannot exhaust memory.

use crate::object::{Dictionary, Object, ObjectId};
use crate::{Error, Result};

/// Maximum nesting depth for arrays, dictionaries and literal strings.
const MAX_DEPTH: usize = 64;
/// Maximum number of entries accepted in a single array or dictionary.
const MAX_CONTAINER_ITEMS: usize = 1_000_000;

/// A cursor over a byte slice that can parse PDF object values.
pub(crate) struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    /// Create a lexer positioned at `pos`.
    pub(crate) fn new(data: &'a [u8], pos: usize) -> Result<Self> {
        if pos > data.len() {
            return Err(Error::StartXrefOutOfBounds {
                offset: pos,
                len: data.len(),
            });
        }
        Ok(Self { data, pos })
    }

    /// Return the byte at the cursor, if any.
    pub(crate) fn byte(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Advance past white space and `%` comments.
    pub(crate) fn skip_ws(&mut self) {
        while let Some(byte) = self.byte() {
            if is_whitespace(byte) {
                self.pos += 1;
            } else if byte == b'%' {
                while let Some(next) = self.byte() {
                    self.pos += 1;
                    if next == b'\n' || next == b'\r' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    /// Read a run of regular (non-delimiter) characters.
    pub(crate) fn read_regular(&mut self) -> Option<&'a [u8]> {
        let start = self.pos;
        while let Some(byte) = self.byte() {
            if is_regular(byte) {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            None
        } else {
            Some(&self.data[start..self.pos])
        }
    }

    /// Peek at the next regular token without consuming it.
    fn peek_regular(&self) -> Option<&'a [u8]> {
        let mut probe = Self {
            data: self.data,
            pos: self.pos,
        };
        probe.read_regular()
    }

    /// Consume a keyword, returning an error when it is absent.
    pub(crate) fn expect_keyword(&mut self, keyword: &[u8]) -> Result<()> {
        self.skip_ws();
        match self.read_regular() {
            Some(token) if token == keyword => Ok(()),
            Some(token) => Err(Error::UnexpectedToken(format!(
                "expected '{}', found '{}'",
                String::from_utf8_lossy(keyword),
                String::from_utf8_lossy(token)
            ))),
            None => Err(Error::UnexpectedEof),
        }
    }

    /// Parse one object value at the cursor.
    pub(crate) fn parse_value(&mut self, depth: usize) -> Result<Object> {
        if depth > MAX_DEPTH {
            return Err(Error::NestingTooDeep(MAX_DEPTH));
        }
        self.skip_ws();
        match self.byte() {
            None => Err(Error::UnexpectedEof),
            Some(b'/') => self.parse_name().map(Object::Name),
            Some(b'(') => self.parse_literal_string().map(Object::String),
            Some(b'[') => self.parse_array(depth),
            Some(b'<') if self.data.get(self.pos + 1) == Some(&b'<') => {
                self.parse_dictionary(depth)
            }
            Some(b'<') => self.parse_hex_string().map(Object::String),
            Some(b']') | Some(b'>') | Some(b')') | Some(b'}') => {
                Err(Error::UnexpectedToken("unbalanced delimiter".to_owned()))
            }
            Some(_) => self.parse_atom(),
        }
    }

    fn parse_name(&mut self) -> Result<String> {
        self.pos += 1; // '/'
        let start = self.pos;
        while let Some(byte) = self.byte() {
            if is_regular(byte) {
                self.pos += 1;
            } else {
                break;
            }
        }
        let raw = &self.data[start..self.pos];
        let mut decoded = Vec::with_capacity(raw.len());
        let mut index = 0;
        while index < raw.len() {
            if raw[index] == b'#'
                && index + 2 < raw.len()
                && let (Some(high), Some(low)) =
                    (hex_value(raw[index + 1]), hex_value(raw[index + 2]))
            {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
            decoded.push(raw[index]);
            index += 1;
        }
        Ok(String::from_utf8_lossy(&decoded).into_owned())
    }

    fn parse_literal_string(&mut self) -> Result<Vec<u8>> {
        self.pos += 1; // '('
        let mut out = Vec::new();
        let mut nesting = 1usize;
        while let Some(byte) = self.byte() {
            self.pos += 1;
            match byte {
                b'(' => {
                    nesting += 1;
                    if nesting > MAX_DEPTH {
                        return Err(Error::NestingTooDeep(MAX_DEPTH));
                    }
                    out.push(byte);
                }
                b')' => {
                    nesting -= 1;
                    if nesting == 0 {
                        return Ok(out);
                    }
                    out.push(byte);
                }
                b'\\' => self.read_escape(&mut out)?,
                _ => out.push(byte),
            }
        }
        Err(Error::UnexpectedEof)
    }

    fn read_escape(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let byte = self.byte().ok_or(Error::UnexpectedEof)?;
        self.pos += 1;
        match byte {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'(' | b')' | b'\\' => out.push(byte),
            b'\r' => {
                if self.byte() == Some(b'\n') {
                    self.pos += 1;
                }
            }
            b'\n' => {}
            b'0'..=b'7' => {
                let mut value = (byte - b'0') as u16;
                for _ in 0..2 {
                    match self.byte() {
                        Some(next @ b'0'..=b'7') => {
                            value = value * 8 + (next - b'0') as u16;
                            self.pos += 1;
                        }
                        _ => break,
                    }
                }
                out.push(value as u8);
            }
            _ => out.push(byte),
        }
        Ok(())
    }

    fn parse_hex_string(&mut self) -> Result<Vec<u8>> {
        self.pos += 1; // '<'
        let mut out = Vec::new();
        let mut high = None;
        while let Some(byte) = self.byte() {
            self.pos += 1;
            if byte == b'>' {
                if let Some(digit) = high {
                    out.push(digit << 4);
                }
                return Ok(out);
            }
            if is_whitespace(byte) {
                continue;
            }
            let digit = hex_value(byte)
                .ok_or_else(|| Error::UnexpectedToken("invalid hexadecimal digit".to_owned()))?;
            if let Some(previous) = high.take() {
                out.push((previous << 4) | digit);
            } else {
                high = Some(digit);
            }
        }
        Err(Error::UnexpectedEof)
    }

    fn parse_array(&mut self, depth: usize) -> Result<Object> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.byte() {
                None => return Err(Error::UnexpectedEof),
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Object::Array(items));
                }
                _ => {
                    if items.len() >= MAX_CONTAINER_ITEMS {
                        return Err(Error::NestingTooDeep(MAX_CONTAINER_ITEMS));
                    }
                    items.push(self.parse_value(depth + 1)?);
                }
            }
        }
    }

    fn parse_dictionary(&mut self, depth: usize) -> Result<Object> {
        self.pos += 2; // '<<'
        let mut dictionary = Dictionary::new();
        loop {
            self.skip_ws();
            match self.byte() {
                None => return Err(Error::UnexpectedEof),
                Some(b'>') if self.data.get(self.pos + 1) == Some(&b'>') => {
                    self.pos += 2;
                    break;
                }
                Some(b'/') => {
                    if dictionary.len() >= MAX_CONTAINER_ITEMS {
                        return Err(Error::NestingTooDeep(MAX_CONTAINER_ITEMS));
                    }
                    let key = self.parse_name()?;
                    let value = self.parse_value(depth + 1)?;
                    dictionary.insert(key, value);
                }
                _ => {
                    return Err(Error::UnexpectedToken(
                        "expected a name key in dictionary".to_owned(),
                    ));
                }
            }
        }

        let after_dictionary = self.pos;
        self.skip_ws();
        if self.starts_with(b"stream") && is_stream_delimiter(self.data.get(self.pos + 6).copied())
        {
            self.pos += 6;
            self.consume_line_ending();
            let data = self.read_stream_data(&dictionary)?;
            self.skip_ws();
            if self.starts_with(b"endstream") {
                self.pos += 9;
            }
            return Ok(Object::Stream { dictionary, data });
        }
        self.pos = after_dictionary;
        Ok(Object::Dictionary(dictionary))
    }

    fn consume_line_ending(&mut self) {
        if self.byte() == Some(b'\r') {
            self.pos += 1;
            if self.byte() == Some(b'\n') {
                self.pos += 1;
            }
        } else if self.byte() == Some(b'\n') {
            self.pos += 1;
        }
    }

    fn read_stream_data(&mut self, dictionary: &Dictionary) -> Result<Vec<u8>> {
        let remaining = self.data.len().saturating_sub(self.pos);
        let declared = match dictionary.get("Length") {
            Some(Object::Integer(length)) if *length >= 0 => usize::try_from(*length).ok(),
            _ => None,
        };
        let end = match declared {
            Some(length) if length <= remaining => self.pos + length,
            Some(_) => return Err(Error::UnexpectedEof),
            None => {
                let tail = &self.data[self.pos..];
                match tail.windows(9).position(|window| window == b"endstream") {
                    Some(index) => self.pos + index,
                    None => return Err(Error::UnexpectedEof),
                }
            }
        };
        let payload = self.data[self.pos..end].to_vec();
        self.pos = end;
        Ok(payload)
    }

    fn parse_atom(&mut self) -> Result<Object> {
        let token = self.read_regular().ok_or(Error::UnexpectedEof)?;
        match token {
            b"true" => return Ok(Object::Boolean(true)),
            b"false" => return Ok(Object::Boolean(false)),
            b"null" => return Ok(Object::Null),
            _ => {}
        }

        if let Ok(number) = parse_integer(token) {
            if let Some(id) = self.try_reference(number) {
                return Ok(Object::Reference(id));
            }
            return Ok(Object::Integer(number));
        }

        let text = std::str::from_utf8(token)
            .map_err(|_| Error::UnexpectedToken("non-UTF-8 token".to_owned()))?;
        match text.parse::<f64>() {
            Ok(value) => Ok(Object::Real(value)),
            Err(_) => Err(Error::UnexpectedToken(format!("unknown token: {text}"))),
        }
    }

    fn try_reference(&mut self, number: i64) -> Option<ObjectId> {
        let after_number = self.pos;
        let found = (|| {
            self.skip_ws();
            let generation_token = self.read_regular()?;
            let generation = parse_integer(generation_token).ok()?;
            if !(0..=i64::from(u16::MAX)).contains(&generation) {
                return None;
            }
            self.skip_ws();
            if self.peek_regular()? != b"R" {
                return None;
            }
            let _ = self.read_regular()?;
            let number = u32::try_from(number).ok()?;
            Some(ObjectId::new(number, generation as u16))
        })();
        if found.is_none() {
            self.pos = after_number;
        }
        found
    }

    fn starts_with(&self, prefix: &[u8]) -> bool {
        self.data
            .get(self.pos..)
            .is_some_and(|rest| rest.starts_with(prefix))
    }
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_regular(byte: u8) -> bool {
    !matches!(
        byte,
        0 | 9
            | 10
            | 12
            | 13
            | 32
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

fn is_stream_delimiter(byte: Option<u8>) -> bool {
    matches!(byte, Some(b'\r') | Some(b'\n') | Some(b' ') | Some(b'\t'))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_integer(token: &[u8]) -> Result<i64> {
    let text = std::str::from_utf8(token)
        .map_err(|_| Error::UnexpectedToken("non-UTF-8 number token".to_owned()))?;
    text.parse::<i64>()
        .map_err(|_| Error::UnexpectedToken(format!("invalid integer: {text}")))
}
