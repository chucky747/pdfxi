//! Deterministic M2 authoring fixtures and their committed manifest.
//!
//! This module extends the M1 corpus in [`crate::fixtures`] with small PDFs that
//! exercise the authoring structures introduced in milestone M2:
//!
//! - all five page boxes (`/MediaBox`, `/CropBox`, `/BleedBox`, `/TrimBox`,
//!   `/ArtBox`);
//! - rotated pages (`/Rotate`);
//! - multi-page documents with a `/PageLabels` number tree;
//! - annotation dictionaries (`/Text`, `/Highlight`, `/Link`, `/Widget`);
//! - an interactive form (`/AcroForm`) with text, checkbox, and choice fields;
//! - an encrypted sample whose *documented, public* test password is the only
//!   credential involved. No real secret is stored in this crate or corpus.
//!
//! [`all`] is the single source of truth shared by the
//! `generate_authoring_fixtures` example and the `authoring_corpus` integration
//! test. Every builder writes a classic-xref PDF with byte-accurate offsets and
//! stream lengths, so regeneration is byte-stable and the files can be parsed
//! by the real `pdfxi_core::Document` (see the integration test, which uses a
//! test-only reverse dependency).
//!
//! # Encrypted sample
//!
//! The encrypted fixture implements the PDF 1.4 *standard security handler*,
//! revision 2, with a 40-bit key (Algorithm 2, 3 and 5 of ISO 32000-1). MD5 and
//! RC4 are implemented locally and only here, so the crate gains no external
//! dependency. MD5 is cryptographically obsolete; it is used solely because R2
//! mandates it and the fixture carries a published test password. The fixture
//! exists so parsers can recognise `/Encrypt`, `/ID`, and an `/O`/`/U` pair —
//! never to protect real data.

use std::path::PathBuf;

use crate::fixtures::toml_quote;
use crate::hash::sha256_hex;

/// Number of committed M2 authoring fixtures.
pub const AUTHORING_FIXTURE_COUNT: usize = 6;

/// Total byte budget for the committed authoring corpus (128 KiB).
pub const AUTHORING_CORPUS_SIZE_BUDGET: usize = 128 * 1024;

/// Documented user password of the encrypted fixture (public test value).
pub const TEST_USER_PASSWORD: &str = "pdfxi-test";

/// Documented owner password of the encrypted fixture (public test value).
pub const TEST_OWNER_PASSWORD: &str = "pdfxi-owner";

/// Standard R2 padding string appended to short passwords (ISO 32000-1,
/// Algorithm 2). It is public and contains no secret.
const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// The one-page font shared by every authoring fixture.
const FONT_OBJECT: &str = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";

/// Header of the generated authoring manifest, including reproduction steps.
const MANIFEST_HEADER: &str = "\
# Committed M2 authoring fixture manifest (T-0064).
#
# Deterministic fixtures for page boxes, rotation, page labels, annotations,
# forms, and a documented-password encrypted sample. Regenerate with:
#   cargo run -p pdfxi-testkit --example generate_authoring_fixtures
# The generator writes every file and computes each size and SHA-256 from the
# bytes it wrote, so this manifest cannot drift from fixtures/authoring/.
# The test tests/authoring_corpus.rs verifies every value.

version = 1
";

/// The authoring feature a fixture primarily demonstrates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthoringFeature {
    /// All five page boxes are present with distinct, documented values.
    PageBoxes,
    /// Pages carry `/Rotate` values of 90, 180, and 270 degrees.
    Rotation,
    /// A multi-page document carries a `/PageLabels` number tree.
    PageLabels,
    /// Text, highlight, link, and widget annotation dictionaries.
    Annotations,
    /// An `/AcroForm` with text, checkbox, and choice fields.
    Forms,
    /// A standard-security-handler R2 encrypted sample.
    Encrypted,
}

impl AuthoringFeature {
    /// Return the stable lower-case manifest token for this feature.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PageBoxes => "page-boxes",
            Self::Rotation => "rotation",
            Self::PageLabels => "page-labels",
            Self::Annotations => "annotations",
            Self::Forms => "forms",
            Self::Encrypted => "encrypted",
        }
    }

    /// Parse a manifest feature token, returning `None` when unknown.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "page-boxes" => Some(Self::PageBoxes),
            "rotation" => Some(Self::Rotation),
            "page-labels" => Some(Self::PageLabels),
            "annotations" => Some(Self::Annotations),
            "forms" => Some(Self::Forms),
            "encrypted" => Some(Self::Encrypted),
            _ => None,
        }
    }
}

/// Description of one committed M2 authoring fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringFixture {
    /// File name relative to `fixtures/authoring/`.
    pub file_name: &'static str,
    /// Primary feature demonstrated by the fixture.
    pub feature: AuthoringFeature,
    /// Additional searchable feature tags mirrored in the manifest.
    pub tags: &'static [&'static str],
    /// Expected number of leaf `/Page` objects.
    pub pages: usize,
}

impl AuthoringFixture {
    /// Regenerate this fixture's bytes from its descriptor.
    pub fn build(&self) -> Vec<u8> {
        build(self)
    }
}

/// All committed M2 authoring fixtures, in manifest order.
pub fn all() -> Vec<AuthoringFixture> {
    vec![
        AuthoringFixture {
            file_name: "authoring-page-boxes.pdf",
            feature: AuthoringFeature::PageBoxes,
            tags: &["geometry", "page-tree", "crop"],
            pages: 1,
        },
        AuthoringFixture {
            file_name: "authoring-rotation.pdf",
            feature: AuthoringFeature::Rotation,
            tags: &["page-tree", "geometry", "rotate"],
            pages: 3,
        },
        AuthoringFixture {
            file_name: "authoring-page-labels.pdf",
            feature: AuthoringFeature::PageLabels,
            tags: &["multipage", "number-tree", "navigation"],
            pages: 3,
        },
        AuthoringFixture {
            file_name: "authoring-annotations.pdf",
            feature: AuthoringFeature::Annotations,
            tags: &["subtypes", "geometry", "review"],
            pages: 1,
        },
        AuthoringFixture {
            file_name: "authoring-forms.pdf",
            feature: AuthoringFeature::Forms,
            tags: &["acroform", "fields", "widgets"],
            pages: 1,
        },
        AuthoringFixture {
            file_name: "authoring-encrypted.pdf",
            feature: AuthoringFeature::Encrypted,
            tags: &["security", "password", "standard-handler"],
            pages: 1,
        },
    ]
}

/// Look up an authoring fixture by file name.
pub fn by_name(name: &str) -> Option<AuthoringFixture> {
    all().into_iter().find(|fixture| fixture.file_name == name)
}

/// Return the workspace root derived from `CARGO_MANIFEST_DIR`, when available.
pub fn workspace_root() -> Option<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
}

/// Return the path of the committed `fixtures/authoring/` directory.
pub fn corpus_dir() -> Option<PathBuf> {
    Some(workspace_root()?.join("fixtures").join("authoring"))
}

/// Return the path of the committed `fixtures/authoring-manifest.toml`.
pub fn manifest_path() -> Option<PathBuf> {
    Some(
        workspace_root()?
            .join("fixtures")
            .join("authoring-manifest.toml"),
    )
}

/// Escape a string for use inside a PDF literal string `(...)`.
fn escape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

/// Build a single content-stream object that draws `text`.
fn stream(text: &str) -> String {
    let content = format!("BT /F1 18 Tf 72 720 Td ({}) Tj ET", escape_literal(text));
    format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        content
    )
}

/// Build a catalog dictionary with optional extra entries.
///
/// `extra` must begin with a space when non-empty, so it is separated from the
/// mandatory `/Pages` entry.
fn catalog(extra: &str) -> String {
    format!("<< /Type /Catalog /Pages 2 0 R{extra} >>")
}

/// Assemble the standard objects for an `n`-page document.
///
/// Returns the object bodies in order (catalog placeholder, pages root, page and
/// content pairs, then the shared font) and the font's object number. Page `i`
/// (zero-based) is object `3 + 2*i`, its content is `4 + 2*i`, and the font is
/// `3 + 2*n`. Callers replace `objects[0]` with their own catalog and append
/// feature-specific objects before calling [`assemble`].
fn base_objects(page_texts: &[&str], page_extras: &[String]) -> (Vec<String>, u32) {
    let count = page_texts.len();
    let font_id = 3 + 2 * count as u32;
    let mut objects = Vec::with_capacity(3 + 2 * count);

    let kids = (0..count)
        .map(|index| format!("{} 0 R", 3 + 2 * index))
        .collect::<Vec<_>>()
        .join(" ");
    objects.push(catalog(""));
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids, count
    ));
    for (index, text) in page_texts.iter().enumerate() {
        let extra = page_extras.get(index).map_or("", String::as_str);
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents {} 0 R /Resources << /Font << /F1 {} 0 R >> >>{} >>",
            4 + 2 * index,
            font_id,
            extra
        ));
        objects.push(stream(text));
    }
    objects.push(FONT_OBJECT.to_string());
    (objects, font_id)
}

/// Assemble a complete classic-xref PDF from raw object bodies.
///
/// Each element of `objects` becomes an indirect object with generation zero.
/// `trailer_extra` is inserted verbatim just before the trailer's closing
/// `>>`; pass an empty string for the common case. Every offset is computed
/// from the actual byte lengths, so the result is structurally valid.
fn assemble(objects: &[String], trailer_extra: &str) -> Vec<u8> {
    let mut out = String::from("%PDF-1.7\n%PDFXI\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
    }
    let xref_at = out.len();
    let size = objects.len() + 1;
    out.push_str(&format!("xref\n0 {size}\n0000000000 65535 f \n"));
    for offset in &offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {size} /Root 1 0 R{trailer_extra} >>\nstartxref\n{xref_at}\n%%EOF\n"
    ));
    out.into_bytes()
}

/// Build the page-boxes fixture.
fn build_page_boxes() -> Vec<u8> {
    let extras = vec![
        " /CropBox [10 10 602 782] /BleedBox [8 8 604 784] /TrimBox [12 12 600 780] \
         /ArtBox [14 14 598 778]"
            .to_string(),
    ];
    let (objects, _) = base_objects(&["All five page boxes"], &extras);
    assemble(&objects, "")
}

/// Build the rotated-pages fixture (90, 180 and 270 degrees).
fn build_rotation() -> Vec<u8> {
    let extras = vec![
        " /Rotate 90".to_string(),
        " /Rotate 180".to_string(),
        " /Rotate 270".to_string(),
    ];
    let texts = ["Rotated page one", "Rotated page two", "Rotated page three"];
    let (objects, _) = base_objects(&texts, &extras);
    assemble(&objects, "")
}

/// Build the multi-page fixture carrying a `/PageLabels` number tree.
fn build_page_labels() -> Vec<u8> {
    let texts = ["Labeled page one", "Labeled page two", "Labeled page three"];
    let (mut objects, font_id) = base_objects(&texts, &[]);
    let labels_id = font_id + 1;
    objects[0] = catalog(&format!(" /PageLabels {labels_id} 0 R"));
    objects.push("<< /Nums [0 << /S /D /> 1 << /S /r /St 1 /P (Sec-) /> ] >>".to_string());
    assemble(&objects, "")
}

/// Build the fixture with text, highlight, link, and widget annotations.
fn build_annotations() -> Vec<u8> {
    let extras = vec![" /Annots [6 0 R 7 0 R 8 0 R 9 0 R]".to_string()];
    let (mut objects, _) = base_objects(&["Annotated page"], &extras);
    objects.push(
        "<< /Type /Annot /Subtype /Text /Rect [100 100 120 120] \
                  /Contents (Sticky note) /T (QA) /Name /Comment /F 4 >>"
            .to_string(),
    );
    objects.push(
        "<< /Type /Annot /Subtype /Highlight /Rect [50 700 300 720] \
                  /QuadPoints [50 720 300 720 50 700 300 700] /C [1 1 0] \
                  /Contents (Highlighted) /F 4 >>"
            .to_string(),
    );
    objects.push(
        "<< /Type /Annot /Subtype /Link /Rect [72 600 240 620] \
                  /Border [0 0 1] /A << /S /URI /URI (https://example.invalid/doc) >> /F 4 >>"
            .to_string(),
    );
    objects.push(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (inline-widget) \
                  /V (widget value) /Rect [72 500 272 520] /F 4 >>"
            .to_string(),
    );
    assemble(&objects, "")
}

/// Build the fixture with an `/AcroForm` and three field types.
fn build_forms() -> Vec<u8> {
    let extras = vec![" /Annots [7 0 R 8 0 R 9 0 R]".to_string()];
    let (mut objects, font_id) = base_objects(&["Form page"], &extras);
    let acroform_id = font_id + 1;
    objects[0] = catalog(&format!(" /AcroForm {acroform_id} 0 R"));
    objects.push(
        "<< /Fields [7 0 R 8 0 R 9 0 R] /DA (/Helv 0 Tf 0 g) \
                  /NeedAppearances false >>"
            .to_string(),
    );
    objects.push(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (text-field) \
                  /V (typed value) /Rect [72 700 272 720] /F 4 >>"
            .to_string(),
    );
    objects.push(
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (checkbox-field) \
                  /V /Yes /AS /Yes /Rect [72 650 92 670] /F 4 >>"
            .to_string(),
    );
    objects.push(
        "<< /Type /Annot /Subtype /Widget /FT /Ch /T (choice-field) \
                  /V (two) /Opt [(one) (two) (three)] /Rect [72 600 272 620] /F 4 >>"
            .to_string(),
    );
    assemble(&objects, "")
}

/// Build the encrypted sample with its documented test password.
fn build_encrypted() -> Vec<u8> {
    let encryption = TestEncryption::documented();
    let (mut objects, font_id) = base_objects(&["Encrypted page"], &[]);
    let encrypt_id = font_id + 1;
    let trailer = format!(
        " /Encrypt {encrypt_id} 0 R /ID [<{}> <{}>]",
        hex(&encryption.document_id),
        hex(&encryption.document_id)
    );
    objects.push(format!(
        "<< /Filter /Standard /V 1 /R 2 /Length 40 /O <{}> /U <{}> /P {} >>",
        hex(&encryption.owner_entry),
        hex(&encryption.user_entry),
        encryption.permissions
    ));
    assemble(&objects, &trailer)
}

/// Regenerate the bytes for any authoring fixture descriptor.
pub fn build(fixture: &AuthoringFixture) -> Vec<u8> {
    match fixture.feature {
        AuthoringFeature::PageBoxes => build_page_boxes(),
        AuthoringFeature::Rotation => build_rotation(),
        AuthoringFeature::PageLabels => build_page_labels(),
        AuthoringFeature::Annotations => build_annotations(),
        AuthoringFeature::Forms => build_forms(),
        AuthoringFeature::Encrypted => build_encrypted(),
    }
}

/// One entry parsed back from `fixtures/authoring-manifest.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringManifestEntry {
    /// File name relative to `fixtures/authoring/`.
    pub name: String,
    /// Primary feature token.
    pub feature: String,
    /// Additional feature tags.
    pub tags: Vec<String>,
    /// Expected page count.
    pub pages: usize,
    /// Size of the committed file in bytes.
    pub size: usize,
    /// Lower-case hexadecimal SHA-256 of the file bytes.
    pub sha256: String,
}

/// Emit the authoring manifest for the supplied fixtures.
///
/// Sizes and digests are computed from the bytes the builders actually emit, so
/// the generated text is the single source of truth checked in under
/// `fixtures/authoring-manifest.toml`.
pub fn emit_manifest(fixtures: &[AuthoringFixture]) -> String {
    let mut out = String::from(MANIFEST_HEADER);
    for fixture in fixtures {
        let bytes = build(fixture);
        out.push_str("\n[[fixture]]\n");
        out.push_str(&format!("name = {}\n", toml_quote(fixture.file_name)));
        out.push_str(&format!(
            "feature = {}\n",
            toml_quote(fixture.feature.as_str())
        ));
        let tags = std::iter::once(fixture.feature.as_str())
            .chain(fixture.tags.iter().copied())
            .map(toml_quote)
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("tags = [{tags}]\n"));
        out.push_str(&format!("pages = {}\n", fixture.pages));
        out.push_str(&format!("size = {}\n", bytes.len()));
        out.push_str(&format!("sha256 = {}\n", toml_quote(&sha256_hex(&bytes))));
    }
    out
}

/// Strip surrounding quotes from a TOML basic string and reverse simple escapes.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return value.to_string();
    }
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
}

/// Parse the flat `[[fixture]]` tables of an authoring manifest.
pub fn parse_manifest(source: &str) -> Vec<AuthoringManifestEntry> {
    let mut entries = Vec::new();
    let mut current: Option<AuthoringManifestEntry> = None;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[fixture]]" {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(AuthoringManifestEntry {
                name: String::new(),
                feature: String::new(),
                tags: Vec::new(),
                pages: 0,
                size: 0,
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
            "feature" => entry.feature = unquote(value),
            "tags" => {
                let inner = value.trim().trim_start_matches('[').trim_end_matches(']');
                entry.tags = inner
                    .split(',')
                    .map(unquote)
                    .filter(|tag| !tag.is_empty())
                    .collect();
            }
            "pages" => {
                if let Ok(pages) = value.trim().parse::<usize>() {
                    entry.pages = pages;
                }
            }
            "size" => {
                if let Ok(size) = value.trim().parse::<usize>() {
                    entry.size = size;
                }
            }
            "sha256" => entry.sha256 = unquote(value),
            _ => {}
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

/// Encode bytes as upper-case hexadecimal, as used by PDF hex strings.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

/// Return `password` padded to the 32-byte block used by the R2 algorithms.
fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut padded = [0u8; 32];
    let copied = password.len().min(32);
    padded[..copied].copy_from_slice(&password[..copied]);
    if copied < 32 {
        let remaining = 32 - copied;
        padded[copied..].copy_from_slice(&PASSWORD_PADDING[..remaining]);
    }
    padded
}

/// The MD5 compression constants (`floor(abs(sin(i + 1)) * 2^32)`).
#[allow(clippy::unreadable_literal)]
const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// Per-round left-rotation amounts for MD5.
const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Compute the 16-byte MD5 digest of `data` (RFC 1321).
///
/// This is a clean-room implementation used only to derive the deterministic
/// R2 encryption keys of the committed test fixture.
fn md5(data: &[u8]) -> [u8; 16] {
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );
    for chunk in message.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (index, word) in m.iter_mut().enumerate() {
            let at = 4 * index;
            *word = u32::from_le_bytes([chunk[at], chunk[at + 1], chunk[at + 2], chunk[at + 3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for index in 0..64 {
            let (f, g) = match index {
                0..=15 => ((b & c) | ((!b) & d), index),
                16..=31 => ((d & b) | ((!d) & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let previous_d = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(MD5_K[index])
                    .wrapping_add(m[g])
                    .rotate_left(MD5_S[index]),
            );
            a = previous_d;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// Apply the RC4 stream cipher with `key` to `data` (no external dependency).
fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut state = [0u8; 256];
    for (index, slot) in state.iter_mut().enumerate() {
        *slot = index as u8;
    }
    if !key.is_empty() {
        let mut j = 0usize;
        for index in 0..256 {
            j = (j + state[index] as usize + key[index % key.len()] as usize) & 0xff;
            state.swap(index, j);
        }
    }
    let mut out = Vec::with_capacity(data.len());
    let (mut i, mut j) = (0usize, 0usize);
    for &byte in data {
        i = (i + 1) & 0xff;
        j = (j + state[i] as usize) & 0xff;
        state.swap(i, j);
        let keystream = state[(state[i] as usize + state[j] as usize) & 0xff];
        out.push(byte ^ keystream);
    }
    out
}

/// Deterministic parameters of the committed R2 encrypted fixture.
///
/// The public passwords in [`TEST_USER_PASSWORD`] and [`TEST_OWNER_PASSWORD`]
/// are the only credentials involved. All fields are derived with the standard
/// algorithm from those constants, so the fixture needs no stored secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestEncryption {
    /// The 32-byte `/O` entry (owner password verification value).
    pub owner_entry: [u8; 32],
    /// The 32-byte `/U` entry (user password verification value).
    pub user_entry: [u8; 32],
    /// The `/P` permission flags, kept at the conventional `-44` value.
    pub permissions: i32,
    /// The first `/ID` element mixed into the R2 key derivation.
    pub document_id: [u8; 16],
}

impl TestEncryption {
    /// Derive the fixture's encryption parameters from the documented passwords.
    pub fn documented() -> Self {
        let document_id = [
            0x50, 0x44, 0x46, 0x58, 0x49, 0x2d, 0x51, 0x41, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05,
            0x06, 0x07,
        ];
        let permissions = -44i32;

        let owner_key = Self::md5_key(&pad_password(TEST_OWNER_PASSWORD.as_bytes()));
        let encrypted_owner = rc4(&owner_key, &pad_password(TEST_USER_PASSWORD.as_bytes()));
        let mut owner_entry = [0u8; 32];
        let copied = encrypted_owner.len().min(32);
        owner_entry[..copied].copy_from_slice(&encrypted_owner[..copied]);

        let file_key = Self::file_key(
            TEST_USER_PASSWORD.as_bytes(),
            &owner_entry,
            permissions,
            &document_id,
        );
        let encrypted_user = rc4(&file_key, &PASSWORD_PADDING);
        let mut user_entry = [0u8; 32];
        let copied = encrypted_user.len().min(32);
        user_entry[..copied].copy_from_slice(&encrypted_user[..copied]);

        Self {
            owner_entry,
            user_entry,
            permissions,
            document_id,
        }
    }

    /// Return the first five MD5 bytes used as a 40-bit R2 key.
    fn md5_key(data: &[u8]) -> [u8; 5] {
        let digest = md5(data);
        let mut key = [0u8; 5];
        key.copy_from_slice(&digest[..5]);
        key
    }

    /// Derive the file key from a user password (ISO 32000-1, Algorithm 2).
    pub fn file_key(
        user_password: &[u8],
        owner_entry: &[u8; 32],
        permissions: i32,
        document_id: &[u8; 16],
    ) -> [u8; 5] {
        let mut input = Vec::with_capacity(32 + 32 + 4 + 16);
        input.extend_from_slice(&pad_password(user_password));
        input.extend_from_slice(owner_entry);
        input.extend_from_slice(&permissions.to_le_bytes());
        input.extend_from_slice(document_id);
        Self::md5_key(&input)
    }

    /// Whether `password` is the documented user password of this fixture.
    ///
    /// Recomputes `/U` with Algorithm 5 and compares it to the stored entry.
    /// The comparison accumulates differences so it does not short-circuit on
    /// the first mismatching byte.
    pub fn password_matches(&self, password: &[u8]) -> bool {
        let key = Self::file_key(
            password,
            &self.owner_entry,
            self.permissions,
            &self.document_id,
        );
        Self::same_entry(&rc4(&key, &PASSWORD_PADDING), &self.user_entry)
    }

    /// Whether `password` is the documented owner password of this fixture.
    ///
    /// Reverses Algorithm 3 by decrypting `/O` with the owner key to recover the
    /// padded user password, then validates that password through
    /// [`Self::password_matches`]. This is the R2 path used by the fixture.
    pub fn owner_password_matches(&self, password: &[u8]) -> bool {
        let owner_key = Self::md5_key(&pad_password(password));
        let recovered_user = rc4(&owner_key, &self.owner_entry);
        self.password_matches(&recovered_user)
    }

    /// Compare two verification entries without short-circuiting on mismatch.
    fn same_entry(computed: &[u8], expected: &[u8; 32]) -> bool {
        if computed.len() != expected.len() {
            return false;
        }
        let mut difference = 0u8;
        for (left, right) in computed.iter().zip(expected.iter()) {
            difference |= left ^ right;
        }
        difference == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_known_vectors() {
        let hex_digest = |input: &str| hex(&md5(input.as_bytes())).to_lowercase();
        assert_eq!(hex_digest(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex_digest("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn rc4_matches_known_vector() {
        let encrypted = rc4(b"Key", b"Plaintext");
        assert_eq!(hex(&encrypted), "BBF316E8D940AF0AD3");
    }

    #[test]
    fn corpus_has_expected_count_and_unique_names() {
        let fixtures = all();
        assert_eq!(fixtures.len(), AUTHORING_FIXTURE_COUNT);
        let mut names: Vec<&str> = fixtures.iter().map(|f| f.file_name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }

    #[test]
    fn builders_are_byte_stable_and_structurally_valid() {
        for fixture in all() {
            let bytes = fixture.build();
            assert_eq!(bytes, fixture.build(), "{}", fixture.file_name);
            assert!(bytes.starts_with(b"%PDF-1.7\n"), "{}", fixture.file_name);
            assert!(bytes.ends_with(b"%%EOF\n"), "{}", fixture.file_name);
            assert!(bytes.len() < AUTHORING_CORPUS_SIZE_BUDGET);
        }
    }

    #[test]
    fn manifest_round_trips_through_parser() {
        let fixtures = all();
        let entries = parse_manifest(&emit_manifest(&fixtures));
        assert_eq!(entries.len(), fixtures.len());
        for (entry, fixture) in entries.iter().zip(fixtures.iter()) {
            assert_eq!(entry.name, fixture.file_name);
            assert_eq!(entry.feature, fixture.feature.as_str());
            assert_eq!(entry.pages, fixture.pages);
            assert_eq!(entry.size, fixture.build().len());
            assert_eq!(entry.sha256, sha256_hex(&fixture.build()));
            assert!(entry.tags.contains(&fixture.feature.as_str().to_string()));
        }
    }

    #[test]
    fn documented_password_is_accepted_and_wrong_one_rejected() {
        let encryption = TestEncryption::documented();
        assert!(encryption.password_matches(TEST_USER_PASSWORD.as_bytes()));
        assert!(encryption.owner_password_matches(TEST_OWNER_PASSWORD.as_bytes()));
        assert!(!encryption.password_matches(b"not-the-password"));
        assert!(!encryption.password_matches(b""));
        assert!(!encryption.owner_password_matches(TEST_USER_PASSWORD.as_bytes()));
        assert!(!encryption.owner_password_matches(b"not-the-owner"));
    }
}
