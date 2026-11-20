//! Small, deterministic XML helpers shared by the Office writers.
//!
//! The adapters generate every XML part themselves from a caller-supplied
//! model. This module centralises the two operations that are easy to get
//! subtly wrong: escaping element/attribute text into well-formed XML 1.0, and
//! composing spreadsheet cell references.

/// Returns the fixed XML declaration used at the start of every part.
#[must_use]
pub const fn declaration() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>"
}

/// Returns true when a Unicode scalar is permitted literally in XML 1.0 text.
fn is_xml_char(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\r')
        || ('\u{20}'..='\u{D7FF}').contains(&ch)
        || ('\u{E000}'..='\u{FFFD}').contains(&ch)
        || ('\u{10000}'..='\u{10FFFF}').contains(&ch)
}

/// Escapes `value` for use in XML element content or a double-quoted attribute.
///
/// The five predefined entities are expanded. XML-illegal control characters
/// are replaced with U+FFFD so hostile extracted text can never produce a
/// malformed part. The replacement is deterministic and preserves length in
/// characters, not bytes.
#[must_use]
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if is_xml_char(c) => out.push(c),
            _ => out.push('\u{fffd}'),
        }
    }
    out
}

/// Returns the zero-based spreadsheet column as uppercase letters (`0 -> A`).
#[must_use]
pub fn column_letters(mut column: usize) -> String {
    let mut letters = Vec::new();
    loop {
        let remainder = column % 26;
        letters.push(b'A' + remainder as u8);
        if column < 26 {
            break;
        }
        column = column / 26 - 1;
    }
    letters.reverse();
    // Every byte pushed above is an ASCII uppercase letter.
    String::from_utf8(letters).unwrap_or_default()
}

/// Returns an A1-style cell reference for a zero-based `(column, row)` pair.
#[must_use]
pub fn cell_ref(column: usize, row: usize) -> String {
    let mut reference = column_letters(column);
    reference.push_str(&(row + 1).to_string());
    reference
}

/// Formats a finite `f64` for an Excel numeric cell without exponent notation
/// surprises. Non-finite inputs are mapped to zero; validation rejects them
/// before this is called, so this is only a defensive fallback.
#[must_use]
pub fn number(value: f64) -> String {
    if value.is_finite() {
        let mut text = value.to_string();
        if text.contains('E') || text.contains('e') {
            text = format!("{value:.15}");
        }
        text
    } else {
        "0".to_owned()
    }
}

/// Converts a finite PDF point length to whole EMU units (1 pt = 12700 EMU).
///
/// Values are clamped to the positive i64 range so an oversized but finite
/// coordinate cannot overflow the OOXML drawing fields.
#[must_use]
pub fn points_to_emu(value: f64) -> i64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    let scaled = value * 12_700.0;
    if scaled >= i64::MAX as f64 {
        i64::MAX
    } else {
        scaled as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_expands_all_predefined_entities() {
        assert_eq!(escape("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
        assert_eq!(escape("plain"), "plain");
    }

    #[test]
    fn escape_replaces_illegal_control_characters() {
        assert_eq!(escape("a\0b\u{1f}c"), "a\u{fffd}b\u{fffd}c");
        assert_eq!(escape("line\nbreak\t tab"), "line\nbreak\t tab");
    }

    #[test]
    fn column_letters_and_cell_ref_follow_spreadsheet_rules() {
        assert_eq!(column_letters(0), "A");
        assert_eq!(column_letters(25), "Z");
        assert_eq!(column_letters(26), "AA");
        assert_eq!(column_letters(27), "AB");
        assert_eq!(column_letters(701), "ZZ");
        assert_eq!(cell_ref(0, 0), "A1");
        assert_eq!(cell_ref(26, 9), "AA10");
    }

    #[test]
    fn number_and_points_to_emu_are_deterministic_and_safe() {
        assert_eq!(number(12.5), "12.5");
        assert_eq!(number(f64::NAN), "0");
        assert_eq!(points_to_emu(72.0), 914_400);
        assert_eq!(points_to_emu(-1.0), 0);
        assert_eq!(points_to_emu(f64::INFINITY), 0);
    }
}
