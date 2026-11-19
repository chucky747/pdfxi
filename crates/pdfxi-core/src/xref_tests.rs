//! Hand-built fixtures and regression tests for xref streams, object streams,
//! mixed `/Prev` chains and hostile cross-reference data.
//!
//! The fixtures are assembled byte by byte (including real zlib streams built
//! from stored DEFLATE blocks) so the tests need no external PDF generator and
//! no dependency on another crate. Every byte offset is computed while writing,
//! which keeps the cross-reference data self-consistent.

use crate::parser;
use crate::{Document, Error, ObjectId};

/// Adler-32 of `data` (RFC 1950), used to build valid zlib fixtures.
fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Wrap `data` in a valid zlib stream made of stored DEFLATE blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(0xffff).peekable();
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        out.push(u8::from(last));
        let length = chunk.len() as u16;
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Encode `data` with the PNG `Up` predictor and a fixed row length.
fn png_up_encode(data: &[u8], columns: usize) -> Vec<u8> {
    assert!(columns > 0 && data.len().is_multiple_of(columns));
    let mut out = Vec::with_capacity(data.len());
    let mut previous = vec![0u8; columns];
    for row in data.chunks_exact(columns) {
        for (index, &byte) in row.iter().enumerate() {
            out.push(byte.wrapping_sub(previous[index]));
        }
        previous.copy_from_slice(row);
    }
    out
}

/// How a single row of an xref stream is filled in by [`build_xref_stream_pdf`].
#[derive(Clone, Copy)]
enum Entry {
    /// A type-0 (free) entry.
    Free,
    /// A type-1 entry pointing at the physical object with this number.
    Object(u32),
    /// A type-1 entry pointing at the xref stream object itself.
    XrefSelf,
    /// A type-2 entry: `(object stream number, index)`.
    Compressed(u32, u32),
}

/// Optional knobs for the xref-stream builder.
struct XrefOptions {
    widths: [u8; 3],
    /// Literal text for `/Index`, or `None` to rely on `/Size`.
    index: Option<String>,
    /// Apply a PNG Up predictor and advertise it in `/DecodeParms`.
    predictor: bool,
    /// Add `/Prev` pointing at the xref stream itself (cycle test).
    self_prev: bool,
}

impl Default for XrefOptions {
    fn default() -> Self {
        Self {
            widths: [1, 4, 2],
            index: None,
            predictor: false,
            self_prev: false,
        }
    }
}

/// Append one big-endian field of `width` bytes to `out`.
fn push_field(out: &mut Vec<u8>, width: u8, value: u64) {
    for shift in (0..width).rev() {
        let amount = 8 * u32::from(shift);
        let byte = if amount >= 64 {
            0
        } else {
            (value >> amount) as u8
        };
        out.push(byte);
    }
}

/// Build a PDF whose newest section is an xref stream.
///
/// `base` may be an existing PDF (for a `/Prev` chain) or empty for a fresh
/// document. Physical `objects` are written first in the given order; the
/// xref stream object is written last. `entries` describe every object number
/// covered by the section's `/W` and `/Index` settings.
fn build_xref_stream_pdf(
    base: &[u8],
    objects: &[(u32, Vec<u8>)],
    xref_number: u32,
    size: u32,
    entries: &[(u32, Entry)],
    extra_dict: &str,
    options: XrefOptions,
) -> Vec<u8> {
    let mut out = if base.is_empty() {
        b"%PDF-1.7\n%PDFXI\n".to_vec()
    } else {
        base.to_vec()
    };
    let mut offsets = std::collections::HashMap::new();
    for (number, body) in objects {
        offsets.insert(*number, out.len());
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = out.len();

    let row_width = options
        .widths
        .iter()
        .map(|&w| usize::from(w))
        .sum::<usize>();
    let mut sorted = entries.to_vec();
    sorted.sort_unstable_by_key(|(number, _)| *number);
    let mut decoded = Vec::with_capacity(sorted.len() * row_width);
    for (_, entry) in sorted {
        let (kind, field_two, field_three) = match entry {
            Entry::Free => (0u64, 0u64, 0u64),
            Entry::Object(number) => (1, offsets[&number] as u64, 0),
            Entry::XrefSelf => (1, xref_offset as u64, 0),
            Entry::Compressed(objstm, index) => (2, u64::from(objstm), u64::from(index)),
        };
        push_field(&mut decoded, options.widths[0], kind);
        push_field(&mut decoded, options.widths[1], field_two);
        push_field(&mut decoded, options.widths[2], field_three);
    }

    let encoded = if options.predictor {
        zlib_stored(&png_up_encode(&decoded, row_width))
    } else {
        zlib_stored(&decoded)
    };

    let mut dictionary = format!(
        "<< /Type /XRef /Size {size} /W [{} {} {}] /Root 1 0 R /Length {}",
        options.widths[0],
        options.widths[1],
        options.widths[2],
        encoded.len()
    );
    if let Some(index) = &options.index {
        dictionary.push_str(" /Index ");
        dictionary.push_str(index);
    }
    if options.predictor {
        dictionary.push_str(&format!(
            " /DecodeParms << /Predictor 12 /Colors 1 /BitsPerComponent 8 /Columns {row_width} >>"
        ));
    }
    if options.self_prev {
        dictionary.push_str(&format!(" /Prev {xref_offset}"));
    }
    dictionary.push_str(extra_dict);
    dictionary.push_str(" /Filter /FlateDecode >>");

    out.extend_from_slice(format!("{xref_number} 0 obj\n{dictionary}\nstream\n").as_bytes());
    out.extend_from_slice(&encoded);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    out
}

/// Build a classic one-page PDF whose text is `text`.
fn classic_one_page_pdf(text: &str) -> Vec<u8> {
    let content = format!("BT ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let mut out = b"%PDF-1.7\n%PDFXI\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Build an `/ObjStm` holding objects 3 (a page) and 4 (its content stream).
///
/// `n_text` is written verbatim as `/N` so tests can inject an absurd count.
fn build_objstm(n_text: &str) -> Vec<u8> {
    let page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>";
    let content = b"BT (From ObjStm) Tj ET";
    let stream = format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        String::from_utf8_lossy(content)
    );
    // Zero-padded offsets give the header a fixed width of 16 bytes, so
    // `/First` can be computed without a fixed-point loop.
    let first = 16usize;
    // Header offsets are relative to `/First`, not absolute positions.
    let second_offset = page.len() + 1;
    assert!(
        second_offset < 100_000,
        "test fixture offsets must fit five digits"
    );
    let header = format!("3 00000 4 {second_offset:05} ");
    assert_eq!(header.len(), first);

    let mut decoded = header.into_bytes();
    decoded.extend_from_slice(page);
    decoded.push(b' ');
    decoded.extend_from_slice(stream.as_bytes());
    decoded.push(b' ');
    let encoded = zlib_stored(&decoded);
    let mut body = format!(
        "<< /Type /ObjStm /N {n_text} /First {first} /Length {} /Filter /FlateDecode >>\nstream\n",
        encoded.len()
    )
    .into_bytes();
    body.extend_from_slice(&encoded);
    body.extend_from_slice(b"\nendstream");
    body
}

/// Return the byte position of the last `needle` occurrence.
fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// Replace ASCII bytes at `position` with `replacement` (equal length).
fn patch_ascii(bytes: &mut [u8], position: usize, replacement: &[u8]) {
    assert_eq!(
        bytes[position..position + replacement.len()].len(),
        replacement.len()
    );
    bytes[position..position + replacement.len()].copy_from_slice(replacement);
}

#[test]
fn xref_stream_parses_page_tree_content_and_decode_parms() {
    let content = b"BT (Stream XRef!) Tj ET";
    let stream_body = format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        String::from_utf8_lossy(content)
    )
    .into_bytes();
    let objects = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_body),
    ];
    let entries = vec![
        (0, Entry::Free),
        (1, Entry::Object(1)),
        (2, Entry::Object(2)),
        (3, Entry::Object(3)),
        (4, Entry::Object(4)),
        (5, Entry::XrefSelf),
    ];
    // The PNG Up predictor and /DecodeParms must be honored: with the
    // predictor byte stream handed to the inflater unchanged, parsing fails.
    let options = XrefOptions {
        predictor: true,
        ..XrefOptions::default()
    };
    let pdf = build_xref_stream_pdf(&[], &objects, 5, 6, &entries, "", options);

    let document = Document::from_bytes(pdf).expect("xref stream fixture parses");
    assert_eq!(document.page_count().expect("page count"), 1);
    assert_eq!(document.root().expect("root"), ObjectId::new(1, 0));
    assert_eq!(document.trailer_size().expect("size"), Some(6));
    assert_eq!(document.declared_page_count().expect("count"), Some(1));
    let page = document.page(0).expect("page");
    assert_eq!(page.id, ObjectId::new(3, 0));
    assert_eq!(page.contents, vec![ObjectId::new(4, 0)]);
    assert_eq!(
        document.page_text(0).expect("decoded page text"),
        "Stream XRef!"
    );
    // Saving an xref-stream input must be declined, not silently rewritten.
    assert!(matches!(
        document.save(),
        Err(Error::UnsupportedState(
            "cross-reference streams are not supported"
        ))
    ));
    assert!(matches!(
        document.save_incremental(),
        Err(Error::UnsupportedState(
            "cross-reference streams are not supported"
        ))
    ));
}

#[test]
fn object_stream_resolves_page_and_content_via_type_two_entries() {
    let objects = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (5, build_objstm("2")),
    ];
    let entries = vec![
        (0, Entry::Free),
        (1, Entry::Object(1)),
        (2, Entry::Object(2)),
        (3, Entry::Compressed(5, 0)),
        (4, Entry::Compressed(5, 1)),
        (5, Entry::Object(5)),
        (6, Entry::XrefSelf),
    ];
    let pdf = build_xref_stream_pdf(&[], &objects, 6, 7, &entries, "", XrefOptions::default());

    let document = Document::from_bytes(pdf).expect("object stream fixture parses");
    assert_eq!(document.page_count().expect("page count"), 1);
    let page = document.page(0).expect("page");
    assert_eq!(page.id, ObjectId::new(3, 0));
    assert_eq!(page.contents, vec![ObjectId::new(4, 0)]);
    assert_eq!(
        document.page_text(0).expect("text from compressed content"),
        "From ObjStm"
    );
}

#[test]
fn absurd_object_stream_count_is_rejected() {
    let objects = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (5, build_objstm("9999999")),
    ];
    let entries = vec![
        (0, Entry::Free),
        (1, Entry::Object(1)),
        (2, Entry::Object(2)),
        (3, Entry::Compressed(5, 0)),
        (4, Entry::Compressed(5, 1)),
        (5, Entry::Object(5)),
        (6, Entry::XrefSelf),
    ];
    let pdf = build_xref_stream_pdf(&[], &objects, 6, 7, &entries, "", XrefOptions::default());
    let document = Document::from_bytes(pdf).expect("header is valid");
    assert!(matches!(
        document.page_count(),
        Err(Error::MalformedObjectStream(_))
    ));
}

#[test]
fn mixed_classic_and_stream_prev_chain_is_newest_wins() {
    let base = classic_one_page_pdf("Original text");
    let previous = parser::startxref_offset(&base).expect("classic startxref");

    let content = b"BT (Newest wins) Tj ET";
    let replacement = format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        String::from_utf8_lossy(content)
    )
    .into_bytes();
    let objects = vec![(4, replacement)];
    // Only a sparse set of object numbers is declared by the new section;
    // objects 1-3 still resolve through the classic /Prev table.
    let entries = vec![
        (0, Entry::Free),
        (4, Entry::Object(4)),
        (5, Entry::XrefSelf),
    ];
    let options = XrefOptions {
        index: Some("[0 1 4 2]".to_owned()),
        ..XrefOptions::default()
    };
    let extra = format!(" /Prev {previous}");
    let pdf = build_xref_stream_pdf(&base, &objects, 5, 6, &entries, &extra, options);

    let document = Document::from_bytes(pdf).expect("mixed chain parses");
    assert_eq!(document.page_count().expect("page count"), 1);
    let page = document.page(0).expect("page from classic /Prev");
    assert_eq!(page.id, ObjectId::new(3, 0));
    assert_eq!(
        document.page_text(0).expect("overridden content"),
        "Newest wins"
    );
}

#[test]
fn truncated_xref_stream_is_a_structured_error() {
    let content = b"BT (Truncate me) Tj ET";
    let stream_body = format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        String::from_utf8_lossy(content)
    )
    .into_bytes();
    let objects = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_body),
    ];
    let entries = vec![
        (0, Entry::Free),
        (1, Entry::Object(1)),
        (2, Entry::Object(2)),
        (3, Entry::Object(3)),
        (4, Entry::Object(4)),
        (5, Entry::XrefSelf),
    ];
    let pdf = build_xref_stream_pdf(&[], &objects, 5, 6, &entries, "", XrefOptions::default());

    let stream_marker = b"\nstream\n";
    let start = rfind_bytes(&pdf, stream_marker).expect("xref stream data");
    let data_start = start + stream_marker.len();
    let end_marker = b"\nendstream";
    let relative_end = pdf[data_start..]
        .windows(end_marker.len())
        .position(|window| window == end_marker)
        .expect("endstream");
    let end = data_start + relative_end;
    // The removed bytes lie inside the stream payload, after the object header
    // that `startxref` addresses, so the offset itself stays valid.
    let removed = 5;
    let mut fixed = Vec::with_capacity(pdf.len() - removed);
    fixed.extend_from_slice(&pdf[..end - removed]);
    fixed.extend_from_slice(&pdf[end..]);

    let document = Document::from_bytes(fixed).expect("header is still valid");
    assert!(matches!(
        document.page_count(),
        Err(Error::UnexpectedEof)
            | Err(Error::MalformedXrefStream(_))
            | Err(Error::InvalidZlibHeader(_))
            | Err(Error::MalformedDeflate(_))
    ));
}

#[test]
fn hostile_xref_field_width_is_rejected() {
    let objects = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
    ];
    let entries = vec![
        (0, Entry::Free),
        (1, Entry::Object(1)),
        (2, Entry::Object(2)),
        (3, Entry::XrefSelf),
    ];
    let options = XrefOptions {
        widths: [1, 9, 2],
        ..XrefOptions::default()
    };
    let pdf = build_xref_stream_pdf(&[], &objects, 3, 4, &entries, "", options);
    let document = Document::from_bytes(pdf).expect("header is valid");
    assert!(matches!(
        document.page_count(),
        Err(Error::MalformedXrefStream(_))
    ));
}

#[test]
fn wrong_width_array_length_is_rejected() {
    let content = b"BT (Wrong W) Tj ET";
    let stream_body = format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        String::from_utf8_lossy(content)
    )
    .into_bytes();
    let objects = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_body),
    ];
    let entries = vec![
        (0, Entry::Free),
        (1, Entry::Object(1)),
        (2, Entry::Object(2)),
        (3, Entry::Object(3)),
        (4, Entry::Object(4)),
        (5, Entry::XrefSelf),
    ];
    let mut pdf = build_xref_stream_pdf(&[], &objects, 5, 6, &entries, "", XrefOptions::default());
    let position = rfind_bytes(&pdf, b"/W [1 4 2]").expect("width array");
    patch_ascii(&mut pdf, position, b"/W [1 4  ]");
    let document = Document::from_bytes(pdf).expect("header is valid");
    assert!(matches!(
        document.page_count(),
        Err(Error::MalformedXrefStream(_))
    ));
}

#[test]
fn cyclic_prev_offset_is_detected() {
    let objects = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
    ];
    let entries = vec![
        (0, Entry::Free),
        (1, Entry::Object(1)),
        (2, Entry::Object(2)),
        (3, Entry::XrefSelf),
    ];
    let options = XrefOptions {
        self_prev: true,
        ..XrefOptions::default()
    };
    let pdf = build_xref_stream_pdf(&[], &objects, 3, 4, &entries, "", options);
    let document = Document::from_bytes(pdf).expect("header is valid");
    assert!(matches!(document.page_count(), Err(Error::XrefCycle(_))));
}
