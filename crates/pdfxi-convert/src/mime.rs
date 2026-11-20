//! Small, bounded MIME helpers shared by the local capture and email adapters.
//!
//! These helpers are deliberately dependency-free and total for hostile input.
//! They never open a file, resolve a path, or perform I/O themselves: callers
//! supply already-read, UTF-8 text and a byte budget. Unsupported encodings are
//! reported as strings that the public adapters turn into typed errors.

use std::io::Read;
use std::path::Path;

/// Split a MIME-like block into its header text and body at the first blank
/// line. Both CRLF and LF separators are accepted, and the returned body starts
/// immediately after the separator.
pub(crate) fn split_headers(input: &str) -> (&str, &str) {
    let crlf = input.find("\r\n\r\n");
    let lf = input.find("\n\n");
    match (crlf, lf) {
        (Some(a), Some(b)) => {
            if a <= b {
                (&input[..a], &input[a + 4..])
            } else {
                (&input[..b], &input[b + 2..])
            }
        }
        (Some(a), None) => (&input[..a], &input[a + 4..]),
        (None, Some(b)) => (&input[..b], &input[b + 2..]),
        (None, None) => (input, ""),
    }
}

/// Return the first value of a case-insensitive header, joining folded
/// continuation lines with a single space.
pub(crate) fn header_value(headers: &str, name: &str) -> Option<String> {
    let mut current_name = String::new();
    let mut current_value = String::new();
    let mut found: Option<String> = None;

    for line in headers.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if !current_name.is_empty() {
                current_value.push(' ');
                current_value.push_str(line.trim());
            }
            continue;
        }
        if current_name.eq_ignore_ascii_case(name) && found.is_none() {
            found = Some(std::mem::take(&mut current_value));
        } else {
            current_value.clear();
        }
        match line.split_once(':') {
            Some((key, value)) => {
                current_name = key.trim().to_owned();
                current_value = value.trim().to_owned();
            }
            None => {
                current_name.clear();
                current_value.clear();
            }
        }
    }
    if current_name.eq_ignore_ascii_case(name) && found.is_none() {
        found = Some(current_value);
    }
    found
}

/// Return the value of one `;`-separated parameter, stripping optional quotes.
pub(crate) fn param_value(value: &str, name: &str) -> Option<String> {
    for part in value.split(';').skip(1) {
        let Some((key, raw)) = part.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case(name) {
            continue;
        }
        let trimmed = raw.trim();
        let unquoted = trimmed
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(trimmed);
        return Some(unquoted.to_owned());
    }
    None
}

/// Return the lower-cased media type without its parameters.
pub(crate) fn content_essence(value: Option<&str>) -> String {
    value
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Split a multipart body into its component entities using a boundary token.
///
/// Delimiters are recognised only at line starts, so a boundary-like sequence
/// inside a part body is kept. The preamble and the closing delimiter are
/// discarded. At most `limit` parts are returned.
pub(crate) fn split_multipart<'a>(
    body: &'a str,
    boundary: &str,
    limit: usize,
) -> Result<Vec<&'a str>, String> {
    if boundary.is_empty()
        || boundary.len() > 200
        || boundary
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err("invalid MIME boundary".to_owned());
    }
    let delimiter = format!("--{boundary}");
    let closing = format!("{delimiter}--");
    let mut parts: Vec<&str> = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    let mut started = false;
    let mut offset = 0usize;

    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == delimiter || trimmed == closing {
            if let Some((start, end)) = current.take() {
                parts.push(&body[start..end]);
                if parts.len() > limit {
                    return Err(format!("too many MIME parts (limit {limit})"));
                }
            }
            started = true;
            if trimmed == closing {
                break;
            }
        } else if started {
            current = Some((
                current.map_or(offset, |(start, _)| start),
                offset + line.len(),
            ));
        }
        offset += line.len();
    }
    if let Some((start, end)) = current.take() {
        parts.push(&body[start..end]);
        if parts.len() > limit {
            return Err(format!("too many MIME parts (limit {limit})"));
        }
    }
    Ok(parts)
}

/// Decode a `Content-Transfer-Encoding` into raw bytes, enforcing `limit`.
pub(crate) fn decode_transfer(
    encoding: Option<&str>,
    body: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let decoded = match encoding
        .unwrap_or("7bit")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "7bit" | "8bit" | "binary" => {
            if body.len() > limit {
                return Err(format!("MIME part too large (limit {limit})"));
            }
            body.as_bytes().to_vec()
        }
        "quoted-printable" => decode_quoted_printable(body, limit)?,
        "base64" => decode_base64(body, limit)?,
        other => return Err(format!("unsupported Content-Transfer-Encoding '{other}'")),
    };
    Ok(normalize_newlines(&decoded))
}

/// Normalise CRLF and bare CR to LF so decoded text renders deterministically.
fn normalize_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\r' {
            out.push(b'\n');
            index += if bytes.get(index + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

/// Decode quoted-printable bytes. Soft line breaks are removed and all
/// newlines are normalised to LF so text and HTML render deterministically.
fn decode_quoted_printable(body: &str, limit: usize) -> Result<Vec<u8>, String> {
    let bytes = body.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'=' {
            if index + 2 < bytes.len()
                && let (Some(high), Some(low)) =
                    (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                out.push((high << 4) | low);
                index += 3;
            } else if bytes.get(index + 1) == Some(&b'\r') && bytes.get(index + 2) == Some(&b'\n') {
                index += 3;
            } else if bytes.get(index + 1) == Some(&b'\n') {
                index += 2;
            } else {
                out.push(b'=');
                index += 1;
            }
        } else if bytes[index] == b'\r' {
            out.push(b'\n');
            index += if bytes.get(index + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
        } else {
            out.push(bytes[index]);
            index += 1;
        }
        if out.len() > limit {
            return Err(format!("MIME part too large (limit {limit})"));
        }
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decode standard base64, ignoring ASCII whitespace and an optional padding
/// suffix. Accumulators are widened to `u32` so no shift can overflow.
fn decode_base64(body: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut values: Vec<u8> = Vec::with_capacity(body.len());
    for byte in body.bytes() {
        match byte {
            b'A'..=b'Z' => values.push(byte - b'A'),
            b'a'..=b'z' => values.push(byte - b'a' + 26),
            b'0'..=b'9' => values.push(byte - b'0' + 52),
            b'+' => values.push(62),
            b'/' => values.push(63),
            b'=' => break,
            value if value.is_ascii_whitespace() => {}
            _ => return Err("invalid base64 character".to_owned()),
        }
    }
    if values.len() % 4 == 1 {
        return Err("truncated base64 payload".to_owned());
    }
    let mut out = Vec::with_capacity(values.len() / 4 * 3);
    for chunk in values.chunks(4) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map(u32::from).unwrap_or(0);
        let third = chunk.get(2).copied().map(u32::from).unwrap_or(0);
        let fourth = chunk.get(3).copied().map(u32::from).unwrap_or(0);
        let word = (first << 18) | (second << 12) | (third << 6) | fourth;
        if chunk.len() >= 2 {
            out.push((word >> 16) as u8);
        }
        if chunk.len() >= 3 {
            out.push((word >> 8) as u8);
        }
        if chunk.len() >= 4 {
            out.push(word as u8);
        }
        if out.len() > limit {
            return Err(format!("MIME part too large (limit {limit})"));
        }
    }
    Ok(out)
}

/// Strip a UTF-8 byte-order mark, if present.
pub(crate) fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// A rejection produced while reading a caller-supplied local file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoundedReadError {
    /// The file exceeded the caller's byte budget.
    TooLarge {
        /// Number of bytes observed (at least `limit + 1`).
        bytes: usize,
        /// Budget that was exceeded.
        limit: usize,
    },
    /// Opening or reading the file failed.
    Io(String),
}

/// Read at most `limit + 1` bytes from `path`, reporting an over-limit file
/// with its observed size so callers can produce an actionable typed error.
pub(crate) fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, BoundedReadError> {
    let file = std::fs::File::open(path).map_err(|error| {
        BoundedReadError::Io(format!("cannot open '{}': {error}", path.display()))
    })?;
    let mut buffer = Vec::new();
    let allowance = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let read = file
        .take(allowance)
        .read_to_end(&mut buffer)
        .map_err(|error| {
            BoundedReadError::Io(format!("cannot read '{}': {error}", path.display()))
        })?;
    if read > limit {
        return Err(BoundedReadError::TooLarge { bytes: read, limit });
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qp_soft_breaks_and_hex_are_decoded() {
        let decoded = decode_quoted_printable("caf=C3=A9=\r\n line", 64).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "café line");
    }

    #[test]
    fn base64_is_bounded_and_widened() {
        assert_eq!(decode_base64("/w==", 64).unwrap(), vec![0xFF]);
        assert!(decode_base64("////", 2).is_err());
        assert!(decode_base64("A", 64).is_err());
    }
}
