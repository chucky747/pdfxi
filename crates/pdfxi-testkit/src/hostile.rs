//! Deterministic malformed/adversarial PDF fixtures for hostile-input coverage.
//!
//! This module is the single source of truth for the small `fixtures/hostile/`
//! corpus. It is deliberately separate from [`crate::malformed`]: that earlier
//! corpus is consumed by a hardcoded-count integration test in `pdfxi-core`, so
//! growing it would break a lane this crate does not own. The hostile corpus has
//! its own count ([`HOSTILE_COUNT`]), builder set ([`all`]), and manifest
//! ([`emit_manifest`]) so QA can extend coverage without touching other crates.
//!
//! Every builder is a pure `fn() -> Vec<u8>`: no randomness, no host state, no
//! network, and no checked-in binary blobs. Every file stays below
//! [`HOSTILE_FILE_MAX_BYTES`]. The manifest records a lowercase SHA-256 digest
//! and the expected error category/probe ("expected structure") for each file,
//! so `cargo xtask corpus smoke` can audit the whole set.
//!
//! Some constructs (recursive annotation `/P`, cyclic form `/Kids`, structure
//! tree parent cycles, and lone UTF-16 surrogates) are tolerated by the current
//! parser entry points: they are data-shape hazards rather than grammar errors.
//! Those fixtures declare [`Category::Tolerated`]; their tests assert the byte
//! shape and the no-panic/no-hang contract instead of inventing a typed error
//! that the parser does not produce. This is recorded honestly in the manifest
//! `intent` text and in the task evidence.

use crate::hash::sha256_hex;

/// Number of hostile fixtures in the committed corpus.
///
/// This is the "replacement count" for `corpus_contains_fifty_fixtures`: the
/// valid 50-fixture corpus is unchanged, and this constant tracks the separate
/// adversarial set in both the generator and the tests.
pub const HOSTILE_COUNT: usize = 16;

/// Maximum allowed size of one hostile fixture (4 KiB).
pub const HOSTILE_FILE_MAX_BYTES: usize = 4 * 1024;

/// Total committed hostile corpus byte budget (96 KiB).
pub const HOSTILE_CORPUS_SIZE_BUDGET: usize = 96 * 1024;

/// Manifest schema version for `fixtures/hostile/manifest.toml`.
pub const HOSTILE_MANIFEST_VERSION: u32 = 1;

/// The operation whose outcome a fixture's declared `category` describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// `pdfxi_core::Document::from_bytes`.
    Open,
    /// `pdfxi_core::Document::page_count`.
    Pages,
    /// `pdfxi_core::Document::page_content(0)`.
    Content,
}

impl Probe {
    /// Return the stable lowercase manifest token for this probe.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Pages => "pages",
            Self::Content => "content",
        }
    }

    /// Parse a manifest probe token, returning `None` when it is unknown.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "open" => Some(Self::Open),
            "pages" => Some(Self::Pages),
            "content" => Some(Self::Content),
            _ => None,
        }
    }
}

/// Expected parser outcome for a hostile fixture at its declared [`Probe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Input does not start with `%PDF-`.
    InvalidHeader,
    /// The `startxref` keyword is absent.
    MissingStartXref,
    /// The `startxref` value is not a valid offset.
    InvalidStartXref,
    /// The `startxref` value lies outside the file.
    StartXrefOutOfBounds,
    /// Classic xref table is structurally malformed.
    MalformedXref,
    /// A `/Type /XRef` stream is structurally malformed.
    MalformedXrefStream,
    /// A `/Type /ObjStm` is structurally malformed.
    MalformedObjectStream,
    /// A token appeared where the grammar disallows it.
    UnexpectedToken,
    /// Input ended before a value was complete.
    UnexpectedEof,
    /// A nesting or container-size limit was exceeded.
    NestingTooDeep,
    /// The `/Pages` tree could not be resolved.
    MalformedPageTree,
    /// The trailer has no `/Root`.
    MissingRoot,
    /// The catalog has no `/Pages`.
    MissingPages,
    /// A stream filter could not be decoded.
    UnsupportedFilter,
    /// `/DecodeParms` contained an out-of-range value.
    InvalidDecodeParameters,
    /// The construct is tolerated at this entry point; verify byte shape and
    /// no-panic/no-hang instead of a typed rejection.
    Tolerated,
}

impl Category {
    /// Return the stable lowercase manifest token for this category.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidHeader => "invalid_header",
            Self::MissingStartXref => "missing_startxref",
            Self::InvalidStartXref => "invalid_startxref",
            Self::StartXrefOutOfBounds => "startxref_out_of_bounds",
            Self::MalformedXref => "malformed_xref",
            Self::MalformedXrefStream => "malformed_xref_stream",
            Self::MalformedObjectStream => "malformed_object_stream",
            Self::UnexpectedToken => "unexpected_token",
            Self::UnexpectedEof => "unexpected_eof",
            Self::NestingTooDeep => "nesting_too_deep",
            Self::MalformedPageTree => "malformed_page_tree",
            Self::MissingRoot => "missing_root",
            Self::MissingPages => "missing_pages",
            Self::UnsupportedFilter => "unsupported_filter",
            Self::InvalidDecodeParameters => "invalid_decode_parameters",
            Self::Tolerated => "tolerated",
        }
    }

    /// Parse a manifest category token, returning `None` when it is unknown.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "invalid_header" => Some(Self::InvalidHeader),
            "missing_startxref" => Some(Self::MissingStartXref),
            "invalid_startxref" => Some(Self::InvalidStartXref),
            "startxref_out_of_bounds" => Some(Self::StartXrefOutOfBounds),
            "malformed_xref" => Some(Self::MalformedXref),
            "malformed_xref_stream" => Some(Self::MalformedXrefStream),
            "malformed_object_stream" => Some(Self::MalformedObjectStream),
            "unexpected_token" => Some(Self::UnexpectedToken),
            "unexpected_eof" => Some(Self::UnexpectedEof),
            "nesting_too_deep" => Some(Self::NestingTooDeep),
            "malformed_page_tree" => Some(Self::MalformedPageTree),
            "missing_root" => Some(Self::MissingRoot),
            "missing_pages" => Some(Self::MissingPages),
            "unsupported_filter" => Some(Self::UnsupportedFilter),
            "invalid_decode_parameters" => Some(Self::InvalidDecodeParameters),
            "tolerated" => Some(Self::Tolerated),
            _ => None,
        }
    }

    /// Return `true` when `error` is the typed error this category expects.
    ///
    /// `Tolerated` never matches an error: those fixtures are checked by byte
    /// shape and the fuzz no-panic contract instead.
    pub fn matches(&self, error: &pdfxi_core::Error) -> bool {
        use pdfxi_core::Error;
        match self {
            Self::InvalidHeader => matches!(error, Error::InvalidHeader),
            Self::MissingStartXref => matches!(error, Error::MissingStartXref),
            Self::InvalidStartXref => matches!(error, Error::InvalidStartXref(_)),
            Self::StartXrefOutOfBounds => matches!(error, Error::StartXrefOutOfBounds { .. }),
            Self::MalformedXref => matches!(error, Error::MalformedXref(_)),
            Self::MalformedXrefStream => matches!(error, Error::MalformedXrefStream(_)),
            Self::MalformedObjectStream => matches!(error, Error::MalformedObjectStream(_)),
            Self::UnexpectedToken => matches!(error, Error::UnexpectedToken(_)),
            Self::UnexpectedEof => matches!(error, Error::UnexpectedEof),
            Self::NestingTooDeep => matches!(error, Error::NestingTooDeep(_)),
            Self::MalformedPageTree => matches!(error, Error::MalformedPageTree(_)),
            Self::MissingRoot => matches!(error, Error::MissingRoot),
            Self::MissingPages => matches!(error, Error::MissingPages),
            Self::UnsupportedFilter => {
                matches!(
                    error,
                    Error::UnsupportedFilter(_) | Error::FilteredContent { .. }
                )
            }
            Self::InvalidDecodeParameters => matches!(error, Error::InvalidDecodeParameters(_)),
            Self::Tolerated => false,
        }
    }
}

/// One hostile fixture descriptor (generator source of truth).
#[derive(Debug, Clone, Copy)]
pub struct HostileBuilder {
    /// File name relative to `fixtures/hostile/`.
    pub name: &'static str,
    /// Why this file exists and what makes it hostile.
    pub intent: &'static str,
    /// Expected parser outcome token.
    pub category: Category,
    /// The operation whose outcome `category` describes.
    pub probe: Probe,
    /// A byte substring that must be present (the "expected structure" marker).
    pub shape: &'static [u8],
    /// Deterministic builder for the fixture bytes.
    pub build: fn() -> Vec<u8>,
}

/// One entry parsed back from `fixtures/hostile/manifest.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostileFixture {
    /// File name relative to `fixtures/hostile/`.
    pub name: String,
    /// Why this file exists and what makes it hostile.
    pub intent: String,
    /// Expected parser outcome token.
    pub category: String,
    /// The operation whose outcome `category` describes.
    pub probe: String,
    /// Expected byte substring, hex-encoded (the "expected structure" marker).
    pub shape: String,
    /// Lowercase hexadecimal SHA-256 of the file bytes.
    pub sha256: String,
}

/// Content bytes used by the canonical one-page stream object.
const CONTENT: &[u8] = b"BT /F1 24 Tf 72 720 Td (Hello, PDFXI!) Tj ET";

/// Options for the classic-xref assembly helper.
struct Assemble<'a> {
    trailer_extra: &'a str,
    eof: bool,
    root: bool,
    startxref: Option<&'a str>,
    xref_count: Option<&'a str>,
    omit_xref_entries: bool,
}

impl Default for Assemble<'_> {
    fn default() -> Self {
        Self {
            trailer_extra: "",
            eof: true,
            root: true,
            startxref: None,
            xref_count: None,
            omit_xref_entries: false,
        }
    }
}

/// Assemble a classic-xref PDF from raw object bodies with hostile overrides.
fn assemble(objects: &[Vec<u8>], options: Assemble<'_>) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%PDFXI\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = objects.len() + 1;
    let size_text = size.to_string();
    let xref_text = xref.to_string();
    let count = options.xref_count.unwrap_or(&size_text);
    out.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
    if !options.omit_xref_entries {
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
    }
    let root = if options.root { "/Root 1 0 R " } else { "" };
    let startxref = options.startxref.unwrap_or(&xref_text);
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} {root}{}>>\nstartxref\n{startxref}\n",
            options.trailer_extra
        )
        .as_bytes(),
    );
    if options.eof {
        out.extend_from_slice(b"%%EOF\n");
    }
    out
}

/// Return a stream object body with the declared length `length`.
fn stream(length: usize, content: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {length} >>\nstream\n").into_bytes();
    out.extend_from_slice(content);
    out.extend_from_slice(b"\nendstream");
    out
}

/// Canonical five-object one-page document bodies.
fn valid_objects() -> Vec<Vec<u8>> {
    vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Contents 4 0 R >>"
            .to_vec(),
        stream(CONTENT.len(), CONTENT),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ]
}

/// A deterministic zlib stream using only uncompressed (stored) DEFLATE blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut position = 0usize;
    loop {
        let end = (position + 65535).min(data.len());
        let chunk = &data[position..end];
        let len = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
        out.push(u8::from(end == data.len()));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
        if end == data.len() {
            break;
        }
        position = end;
    }
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&(((b << 16) | a) as u32).to_be_bytes());
    out
}

/// Write one big-endian field of `width` bytes into `out`.
fn push_field(out: &mut Vec<u8>, width: usize, value: u64) {
    for shift in (0..width).rev() {
        out.push(((value >> (shift * 8)) & 0xFF) as u8);
    }
}

/// Build an object stream holding a page (object 3) and its content (object 4).
///
/// `n_text` and `first_text` are written verbatim so callers can inject hostile
/// `/N` and `/First` values while keeping every other byte deterministic.
fn build_objstm(n_text: &str, first_text: &str) -> Vec<u8> {
    let page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>";
    let content = b"BT (From ObjStm) Tj ET";
    let content_body = format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        String::from_utf8_lossy(content)
    );
    let second_offset = page.len() + 1;
    let header = format!("3 00000 4 {second_offset:05} ");
    let mut decoded = header.into_bytes();
    decoded.extend_from_slice(page);
    decoded.push(b' ');
    decoded.extend_from_slice(content_body.as_bytes());
    decoded.push(b' ');
    let encoded = zlib_stored(&decoded);
    let mut body = format!(
        "<< /Type /ObjStm /N {n_text} /First {first_text} /Length {} \
         /Filter /FlateDecode >>\nstream\n",
        encoded.len()
    )
    .into_bytes();
    body.extend_from_slice(&encoded);
    body.extend_from_slice(b"\nendstream");
    body
}

/// Build an xref-stream PDF whose page and content live in an ObjStm.
///
/// `n_text` and `first_text` are written verbatim into the object stream
/// dictionary, which lets the hostile fixtures inject out-of-range values.
pub fn compressed_pdf(n_text: &str, first_text: &str) -> Vec<u8> {
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (5, build_objstm(n_text, first_text)),
    ];
    let mut out = b"%PDF-1.7\n%PDFXI\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (number, body) in &objects {
        offsets.insert(*number, out.len());
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = out.len();
    // Objects 3 and 4 are compressed into object 5; object 6 is the xref stream
    // itself (a type-1 self entry, matching the core's known-good layout).
    let rows: [(u64, u64, u64); 7] = [
        (0, 0, 0),
        (1, offsets[&1] as u64, 0),
        (1, offsets[&2] as u64, 0),
        (2, 5, 0),
        (2, 5, 1),
        (1, offsets[&5] as u64, 0),
        (1, xref_offset as u64, 0),
    ];
    let mut decoded = Vec::new();
    for (kind, field_two, field_three) in rows {
        push_field(&mut decoded, 1, kind);
        push_field(&mut decoded, 4, field_two);
        push_field(&mut decoded, 2, field_three);
    }
    let encoded = zlib_stored(&decoded);
    out.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /XRef /Size 7 /Index [0 7] /W [1 4 2] /Root 1 0 R \
             /Length {} /Filter /FlateDecode >>\nstream\n",
            encoded.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&encoded);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    out
}

fn xref_truncated() -> Vec<u8> {
    assemble(
        &valid_objects(),
        Assemble {
            omit_xref_entries: true,
            ..Assemble::default()
        },
    )
}

fn xref_past_eof() -> Vec<u8> {
    assemble(
        &valid_objects(),
        Assemble {
            startxref: Some("999999999"),
            ..Assemble::default()
        },
    )
}

fn objstm_bad_n() -> Vec<u8> {
    compressed_pdf("999999", "16")
}

fn objstm_bad_first() -> Vec<u8> {
    compressed_pdf("2", "999999")
}

fn cyclic_page_tree() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[1] = b"<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 2 >>".to_vec();
    assemble(&objects, Assemble::default())
}

fn page_tree_missing_kids() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[1] = b"<< /Type /Pages /Count 1 >>".to_vec();
    assemble(&objects, Assemble::default())
}

fn catalog_missing_pages() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[0] = b"<< /Type /Catalog >>".to_vec();
    assemble(&objects, Assemble::default())
}

fn stream_absurd_length() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[3] = stream(999_999_999, CONTENT);
    assemble(&objects, Assemble::default())
}

fn nested_filter_parms() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[3] = format!(
        "<< /Length {} /Filter [/FlateDecode /ASCIIHexDecode] \
         /DecodeParms << /Predictor -7 /Colors -5 /BitsPerComponent -1 \
         /Columns 999999999 >> >>\nstream\n{}\nendstream",
        CONTENT.len(),
        String::from_utf8_lossy(CONTENT)
    )
    .into_bytes();
    assemble(&objects, Assemble::default())
}

fn nested_arrays() -> Vec<u8> {
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    let mut objects = valid_objects();
    objects[0] = format!("<< /Type /Catalog /Pages 2 0 R /Deep {deep} >>").into_bytes();
    assemble(&objects, Assemble::default())
}

fn nested_dicts() -> Vec<u8> {
    let deep = format!("{} /Leaf 1 {}", "<< /A ".repeat(200), ">>".repeat(200));
    let mut objects = valid_objects();
    objects[0] = format!("<< /Type /Catalog /Pages 2 0 R /Deep {deep} >>").into_bytes();
    assemble(&objects, Assemble::default())
}

fn utf16_lone_surrogate() -> Vec<u8> {
    let mut objects = valid_objects();
    // Valid hex containing an unpaired high surrogate, a normal code unit and
    // an unpaired low surrogate, plus an intentionally odd-length hex token.
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R /Title <D8000041DC00DFFF> /Odd <D80> >>".to_vec();
    assemble(&objects, Assemble::default())
}

fn annot_recursive_p() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[2] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                   /Contents 4 0 R /Annots [5 0 R] >>"
        .to_vec();
    objects.push(b"<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /P 5 0 R".to_vec());
    assemble(&objects, Assemble::default())
}

fn form_field_cyclic_kids() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R \
                   /AcroForm << /Fields [6 0 R] >> >>"
        .to_vec();
    objects.push(b"<< /FT /Tx /T (field) /Kids [6 0 R".to_vec());
    assemble(&objects, Assemble::default())
}

fn struct_tree_parent_cycle() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>".to_vec();
    objects.push(b"<< /Type /StructTreeRoot /ParentTree 7 0 R /K [7 0 R".to_vec());
    assemble(&objects, Assemble::default())
}

fn trailer_missing_root() -> Vec<u8> {
    assemble(
        &valid_objects(),
        Assemble {
            root: false,
            ..Assemble::default()
        },
    )
}

/// All hostile fixtures, in manifest order.
pub fn all() -> Vec<HostileBuilder> {
    vec![
        HostileBuilder {
            name: "xref-truncated.pdf",
            intent: "Classic xref declares six entries but only the free head is present; \
                     the in-bounds offset must still reject the truncated table.",
            category: Category::MalformedXref,
            probe: Probe::Pages,
            shape: b"xref\n0 6",
            build: xref_truncated,
        },
        HostileBuilder {
            name: "xref-past-eof.pdf",
            intent: "startxref points far past EOF; the offset must be bounds-checked.",
            category: Category::StartXrefOutOfBounds,
            probe: Probe::Pages,
            shape: b"startxref\n999999999",
            build: xref_past_eof,
        },
        HostileBuilder {
            name: "objstm-bad-n.pdf",
            intent: "Compressed page tree resolved from an /ObjStm whose /N is absurd.",
            category: Category::MalformedObjectStream,
            probe: Probe::Pages,
            shape: b"/Type /ObjStm /N 999999",
            build: objstm_bad_n,
        },
        HostileBuilder {
            name: "objstm-bad-first.pdf",
            intent: "Compressed page tree resolved from an /ObjStm whose /First is past EOF.",
            category: Category::MalformedObjectStream,
            probe: Probe::Pages,
            shape: b"/First 999999",
            build: objstm_bad_first,
        },
        HostileBuilder {
            name: "cyclic-page-tree.pdf",
            intent: "The /Pages root lists itself in /Kids; the tree walk must detect the cycle.",
            category: Category::MalformedPageTree,
            probe: Probe::Pages,
            shape: b"/Kids [2 0 R 3 0 R]",
            build: cyclic_page_tree,
        },
        HostileBuilder {
            name: "page-tree-missing-kids.pdf",
            intent: "/Pages node has no /Kids array; the tree walk must reject it.",
            category: Category::MalformedPageTree,
            probe: Probe::Pages,
            shape: b"<< /Type /Pages /Count 1 >>",
            build: page_tree_missing_kids,
        },
        HostileBuilder {
            name: "catalog-missing-pages.pdf",
            intent: "Catalog has no /Pages reference; the trailer root is structurally incomplete.",
            category: Category::MissingPages,
            probe: Probe::Pages,
            shape: b"<< /Type /Catalog >>",
            build: catalog_missing_pages,
        },
        HostileBuilder {
            name: "stream-absurd-length.pdf",
            intent: "Content stream /Length is far larger than the file; decoding must stop at EOF.",
            category: Category::UnexpectedEof,
            probe: Probe::Content,
            shape: b"/Length 999999999",
            build: stream_absurd_length,
        },
        HostileBuilder {
            name: "nested-filter-parms.pdf",
            intent: "Nested filter chain with negative predictor/colors/bits and a huge column count; \
                     the decoder rejects the hostile DecodeParms with a typed error.",
            category: Category::InvalidDecodeParameters,
            probe: Probe::Content,
            shape: b"/Filter [/FlateDecode /ASCIIHexDecode]",
            build: nested_filter_parms,
        },
        HostileBuilder {
            name: "deep-arrays.pdf",
            intent: "200 nested arrays in the catalog exceed the bounded nesting limit.",
            category: Category::NestingTooDeep,
            probe: Probe::Pages,
            shape: b"[[[[",
            build: nested_arrays,
        },
        HostileBuilder {
            name: "deep-dicts.pdf",
            intent: "200 nested dictionaries in the catalog exceed the bounded nesting limit.",
            category: Category::NestingTooDeep,
            probe: Probe::Pages,
            shape: b"<< /A << /A << /A ",
            build: nested_dicts,
        },
        HostileBuilder {
            name: "utf16-lone-surrogate.pdf",
            intent: "Unpaired UTF-16 surrogates plus an odd-length hex token in the catalog; \
                     text decoding is total and the odd token is tolerated, so the test asserts \
                     byte shape and no-panic instead of a typed error.",
            category: Category::Tolerated,
            probe: Probe::Pages,
            shape: b"<D8000041DC00DFFF>",
            build: utf16_lone_surrogate,
        },
        HostileBuilder {
            name: "annot-recursive-p.pdf",
            intent: "Annotation /P points back at its own object; the parser tolerates the cycle, \
                     so the test asserts byte shape and no-panic instead of a typed error.",
            category: Category::Tolerated,
            probe: Probe::Open,
            shape: b"/P 5 0 R",
            build: annot_recursive_p,
        },
        HostileBuilder {
            name: "form-field-cyclic-kids.pdf",
            intent: "AcroForm field /Kids points back at itself; tolerated at this entry point, \
                     verified by byte shape and no-panic.",
            category: Category::Tolerated,
            probe: Probe::Open,
            shape: b"/Kids [6 0 R",
            build: form_field_cyclic_kids,
        },
        HostileBuilder {
            name: "struct-tree-parent-cycle.pdf",
            intent: "Structure tree /ParentTree and /K both point at the root; tolerated here, \
                     verified by byte shape and no-panic.",
            category: Category::Tolerated,
            probe: Probe::Open,
            shape: b"/ParentTree 7 0 R",
            build: struct_tree_parent_cycle,
        },
        HostileBuilder {
            name: "trailer-missing-root.pdf",
            intent: "Trailer has no /Root reference; the document root cannot be resolved.",
            category: Category::MissingRoot,
            probe: Probe::Pages,
            shape: b"<< /Size 6",
            build: trailer_missing_root,
        },
    ]
}

/// Emit the manifest TOML for the given hostile fixtures.
pub fn emit_manifest(fixtures: &[HostileBuilder]) -> String {
    let mut out = format!(
        "# Committed hostile-input corpus (T-0183).\n\
         # Generated by: cargo run -p pdfxi-testkit --example generate_hostile\n\
         # Every file is < 4 KiB. `category` is the typed parser outcome expected\n\
         # for `probe`; `shape` is a hex-encoded byte substring (expected structure).\n\
         # `tolerated` means no typed-error path is reachable at this entry point.\n\n\
         version = {}\n",
        HOSTILE_MANIFEST_VERSION
    );
    for fixture in fixtures {
        out.push_str("\n[[hostile]]\n");
        out.push_str(&format!(
            "name = {}\n",
            crate::fixtures::toml_quote(fixture.name)
        ));
        out.push_str(&format!(
            "intent = {}\n",
            crate::fixtures::toml_quote(fixture.intent)
        ));
        out.push_str(&format!(
            "category = {}\n",
            crate::fixtures::toml_quote(fixture.category.as_str())
        ));
        out.push_str(&format!(
            "probe = {}\n",
            crate::fixtures::toml_quote(fixture.probe.as_str())
        ));
        out.push_str(&format!(
            "shape = {}\n",
            crate::fixtures::toml_quote(&hex_encode(fixture.shape))
        ));
        out.push_str(&format!(
            "sha256 = {}\n",
            crate::fixtures::toml_quote(&sha256_hex(&(fixture.build)()))
        ));
    }
    out
}

/// Return the lowercase hexadecimal encoding of `bytes`.
fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0F)]));
    }
    out
}

/// Decode a lowercase/uppercase hexadecimal string, returning `None` if invalid.
fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push(((high << 4) | low) as u8);
    }
    Some(out)
}

/// Strip surrounding quotes from a TOML basic-string scalar.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let inner = &value[1..value.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        out
    } else {
        value.to_string()
    }
}

/// Parse the flat `[[hostile]]` tables of a manifest TOML document.
pub fn parse_manifest(source: &str) -> Vec<HostileFixture> {
    let mut entries = Vec::new();
    let mut current: Option<HostileFixture> = None;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[hostile]]" {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(HostileFixture {
                name: String::new(),
                intent: String::new(),
                category: String::new(),
                probe: String::new(),
                shape: String::new(),
                sha256: String::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(entry) = current.as_mut() else {
            continue;
        };
        match key.trim() {
            "name" => entry.name = unquote(value),
            "intent" => entry.intent = unquote(value),
            "category" => entry.category = unquote(value),
            "probe" => entry.probe = unquote(value),
            "shape" => entry.shape = unquote(value),
            "sha256" => entry.sha256 = unquote(value),
            _ => {}
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

/// Return the path of the committed `fixtures/hostile/` directory.
pub fn corpus_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .map(|root| root.join("fixtures").join("hostile"))
        .unwrap_or_else(|| std::path::PathBuf::from("fixtures/hostile"))
}

/// Read and parse the committed hostile corpus manifest from disk.
pub fn committed_manifest() -> Vec<HostileFixture> {
    std::fs::read_to_string(corpus_dir().join("manifest.toml"))
        .map(|source| parse_manifest(&source))
        .unwrap_or_default()
}

/// Decode the hex-encoded `shape` field of a manifest entry.
pub fn manifest_shape(entry: &HostileFixture) -> Option<Vec<u8>> {
    hex_decode(&entry.shape)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_has_expected_count() {
        assert_eq!(all().len(), HOSTILE_COUNT);
        assert!((10..=32).contains(&HOSTILE_COUNT));
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = all().iter().map(|f| f.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }

    #[test]
    fn every_fixture_fits_the_size_budget() {
        let mut total = 0usize;
        for fixture in all() {
            let bytes = (fixture.build)();
            assert!(
                bytes.len() < HOSTILE_FILE_MAX_BYTES,
                "{} is {} bytes",
                fixture.name,
                bytes.len()
            );
            total += bytes.len();
        }
        assert!(total < HOSTILE_CORPUS_SIZE_BUDGET, "total {total}");
    }

    #[test]
    fn builders_are_byte_stable() {
        for fixture in all() {
            assert_eq!((fixture.build)(), (fixture.build)(), "{}", fixture.name);
        }
    }

    #[test]
    fn shape_marker_is_present_in_every_fixture() {
        for fixture in all() {
            let bytes = (fixture.build)();
            assert!(
                find_bytes(&bytes, fixture.shape),
                "{} missing shape marker",
                fixture.name
            );
        }
    }

    #[test]
    fn manifest_round_trips_through_the_parser() {
        let fixtures = all();
        let emitted = emit_manifest(&fixtures);
        let parsed = parse_manifest(&emitted);
        assert_eq!(parsed.len(), fixtures.len());
        for (entry, fixture) in parsed.iter().zip(fixtures.iter()) {
            assert_eq!(entry.name, fixture.name);
            assert_eq!(entry.intent, fixture.intent);
            assert_eq!(entry.category, fixture.category.as_str());
            assert_eq!(entry.probe, fixture.probe.as_str());
            assert_eq!(manifest_shape(entry).as_deref(), Some(fixture.shape));
            assert_eq!(entry.sha256, sha256_hex(&(fixture.build)()));
            assert_eq!(entry.sha256.len(), 64);
            assert!(
                entry
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
        }
    }

    #[test]
    fn committed_manifest_matches_generated_descriptors() {
        let on_disk = committed_manifest();
        let generated = parse_manifest(&emit_manifest(&all()));
        assert_eq!(on_disk, generated, "regenerate the hostile corpus manifest");
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() {
            return true;
        }
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}
