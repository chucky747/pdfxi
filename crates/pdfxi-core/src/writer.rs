//! Serialization of PDF objects, cross-reference tables and trailers.
//!
//! This module is the write-side counterpart to [`crate::lexer::Lexer`]. It
//! emits plain PDF syntax for values held in [`crate::object::Object`] and for
//! the structural tables that make a byte stream a valid PDF. It performs no
//! I/O: callers supply a `Vec<u8>` to append to, which keeps save operations
//! copy-on-write and easy to test.

use std::collections::BTreeMap;

use crate::object::{Dictionary, Object, ObjectId};
use crate::{Error, Result};

/// Append a complete `N G obj ... endobj` definition for `object`.
pub(crate) fn serialize_indirect(id: ObjectId, object: &Object, out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(format!("{} {} obj\n", id.number, id.generation).as_bytes());
    serialize_object(object, out)?;
    out.extend_from_slice(b"\nendobj");
    Ok(())
}

/// Append any PDF object value in syntax accepted by the parser.
///
/// Strings are written as hexadecimal strings, which avoids all escaping
/// ambiguity for arbitrary byte payloads. Reals always carry a decimal point
/// so they reparse as reals rather than integers.
pub(crate) fn serialize_object(object: &Object, out: &mut Vec<u8>) -> Result<()> {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(true) => out.extend_from_slice(b"true"),
        Object::Boolean(false) => out.extend_from_slice(b"false"),
        Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Object::Real(value) => {
            if !value.is_finite() {
                return Err(Error::CannotSerialize(
                    "a real number must be finite".to_owned(),
                ));
            }
            if value.fract() == 0.0 {
                out.extend_from_slice(format!("{value:.1}").as_bytes());
            } else {
                out.extend_from_slice(value.to_string().as_bytes());
            }
        }
        Object::Name(name) => write_name(name.as_bytes(), out),
        Object::String(bytes) => {
            out.push(b'<');
            for byte in bytes {
                out.extend_from_slice(format!("{byte:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Object::Array(items) => {
            out.push(b'[');
            for item in items {
                out.push(b' ');
                serialize_object(item, out)?;
            }
            out.extend_from_slice(b" ]");
        }
        Object::Dictionary(dictionary) => write_dictionary(dictionary, out)?,
        Object::Reference(id) => {
            out.extend_from_slice(format!("{} {} R", id.number, id.generation).as_bytes());
        }
        Object::Stream { dictionary, data } => {
            let mut sized = dictionary.clone();
            let length = i64::try_from(data.len())
                .map_err(|_| Error::CannotSerialize("stream length exceeds i64".to_owned()))?;
            sized.insert("Length".to_owned(), Object::Integer(length));
            write_dictionary(&sized, out)?;
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendstream");
        }
    }
    Ok(())
}

/// Append a dictionary, including the `<<` and `>>` delimiters.
fn write_dictionary(dictionary: &Dictionary, out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(b"<<");
    for (key, value) in dictionary {
        out.push(b' ');
        write_name(key.as_bytes(), out);
        out.push(b' ');
        serialize_object(value, out)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

/// Append a name object, `#`-escaping bytes that are not safe verbatim.
fn write_name(name: &[u8], out: &mut Vec<u8>) {
    out.push(b'/');
    for &byte in name {
        if is_name_byte_safe(byte) {
            out.push(byte);
        } else {
            out.extend_from_slice(format!("#{byte:02X}").as_bytes());
        }
    }
}

/// Return `true` when a byte may appear verbatim inside a PDF name.
fn is_name_byte_safe(byte: u8) -> bool {
    !matches!(
        byte,
        0..=32 | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#'
    )
}

/// Append a classic cross-reference section covering `offsets`.
///
/// When `include_free_zero` is set, object 0 is emitted first as the head of
/// the free-object list, as required by the PDF specification. Consecutive
/// object numbers are grouped into subsections to avoid writing a huge table
/// for documents with sparse object numbers.
pub(crate) fn write_xref_section(
    out: &mut Vec<u8>,
    offsets: &BTreeMap<u32, usize>,
    include_free_zero: bool,
) {
    out.extend_from_slice(b"xref\n");
    let mut entries: Vec<(u32, Option<usize>)> =
        Vec::with_capacity(offsets.len() + usize::from(include_free_zero));
    if include_free_zero {
        entries.push((0, None));
    }
    for (number, offset) in offsets {
        entries.push((*number, Some(*offset)));
    }

    let mut index = 0;
    while index < entries.len() {
        let first = entries[index].0;
        let mut count = 1usize;
        while index + count < entries.len()
            && first.checked_add(count as u32) == Some(entries[index + count].0)
        {
            count += 1;
        }
        out.extend_from_slice(format!("{first} {count}\n").as_bytes());
        for entry in &entries[index..index + count] {
            match entry.1 {
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
                Some(offset) => {
                    out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
                }
            }
        }
        index += count;
    }
}

/// Append a trailer dictionary, `startxref` and `%%EOF` marker.
pub(crate) fn write_trailer(
    out: &mut Vec<u8>,
    trailer: &Dictionary,
    xref_offset: usize,
) -> Result<()> {
    out.extend_from_slice(b"trailer\n");
    write_dictionary(trailer, out)?;
    out.push(b'\n');
    out.extend_from_slice(b"startxref\n");
    out.extend_from_slice(format!("{xref_offset}\n").as_bytes());
    out.extend_from_slice(b"%%EOF\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(object: &Object) -> String {
        let mut out = Vec::new();
        serialize_object(object, &mut out).expect("serializable object");
        String::from_utf8(out).expect("ASCII output")
    }

    #[test]
    fn escapes_unsafe_name_bytes() {
        assert_eq!(text(&Object::Name("A B/c#".to_owned())), "/A#20B#2Fc#23");
    }

    #[test]
    fn writes_hex_strings_and_integral_reals() {
        assert_eq!(text(&Object::String(vec![0x00, 0xAB, 0x10])), "<00AB10>");
        assert_eq!(text(&Object::Real(1.0)), "1.0");
        assert_eq!(text(&Object::Real(2.5)), "2.5");
    }

    #[test]
    fn rejects_non_finite_reals() {
        let mut out = Vec::new();
        assert!(matches!(
            serialize_object(&Object::Real(f64::NAN), &mut out),
            Err(Error::CannotSerialize(_))
        ));
    }

    #[test]
    fn stream_serialization_sets_exact_length() {
        let object = Object::Stream {
            dictionary: Dictionary::new(),
            data: b"abc".to_vec(),
        };
        let rendered = text(&object);
        assert!(rendered.starts_with("<< /Length 3 >>\nstream\nabc\nendstream"));
    }

    #[test]
    fn xref_section_groups_consecutive_objects() {
        let mut offsets = BTreeMap::new();
        offsets.insert(1, 9);
        offsets.insert(2, 50);
        offsets.insert(4, 80);
        let mut out = Vec::new();
        write_xref_section(&mut out, &offsets, true);
        let rendered = String::from_utf8(out).expect("ASCII output");
        assert!(rendered.starts_with("xref\n0 3\n0000000000 65535 f \n"));
        assert!(rendered.contains("4 1\n0000000080 00000 n \n"));
    }

    #[test]
    fn write_trailer_emits_expected_tail() {
        let mut trailer = Dictionary::new();
        trailer.insert("Size".to_owned(), Object::Integer(6));
        let mut out = Vec::new();
        write_trailer(&mut out, &trailer, 123).expect("trailer writes");
        let rendered = String::from_utf8(out).expect("ASCII output");
        assert_eq!(rendered, "trailer\n<< /Size 6 >>\nstartxref\n123\n%%EOF\n");
    }
}
