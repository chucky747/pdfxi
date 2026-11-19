//! Descriptors and byte builders for the committed malformed-PDF corpus.
//!
//! Unlike the valid corpus in [`crate::fixtures`], these files are deliberately
//! hostile: truncated headers, dangling offsets, corrupt objects, and extreme
//! counts. They are small (every file stays below [`MALFORMED_FILE_MAX_BYTES`])
//! so the whole set can be committed and exercised by fast tests.
//!
//! [`all`] is the single source of truth. The `generate_malformed` example uses
//! it to write `fixtures/malformed/` and its `manifest.toml`, and the
//! `crates/pdfxi-core/tests/malformed.rs` integration test reads the on-disk
//! manifest back to verify hashes, budgets, and — most importantly — that
//! `pdfxi_core::Document` never panics on any of these inputs.
//!
//! Each fixture declares a `probe`: the operation whose outcome the manifest's
//! `category` describes. `open` is [`pdfxi_core::Document::from_bytes`],
//! `pages` is the structural accessor `page_count`, `content` is
//! `page_content(0)`, and `save` is `save()`. The core test still runs *every*
//! probe under `catch_unwind`; the declared probe is only the one asserted
//! against the expected category.

/// Number of malformed fixtures in the committed robustness corpus.
pub const MALFORMED_COUNT: usize = 23;

/// Maximum allowed size of one malformed fixture (4 KiB).
pub const MALFORMED_FILE_MAX_BYTES: usize = 4 * 1024;

/// Total committed malformed corpus byte budget (128 KiB).
pub const MALFORMED_CORPUS_SIZE_BUDGET: usize = 128 * 1024;

/// Alias of [`MALFORMED_COUNT`] used by the `pdfxi-core` robustness test.
pub const MALFORMED_FIXTURE_COUNT: usize = MALFORMED_COUNT;

/// Alias of [`MALFORMED_CORPUS_SIZE_BUDGET`] used by the robustness test.
pub const MALFORMED_SIZE_BUDGET: usize = MALFORMED_CORPUS_SIZE_BUDGET;

/// Alias of [`MALFORMED_FILE_MAX_BYTES`] used by the robustness test.
pub const MAX_MALFORMED_FIXTURE_BYTES: usize = MALFORMED_FILE_MAX_BYTES;

/// Which operation's outcome a fixture's `category` describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// `Document::from_bytes`.
    Open,
    /// `Document::page_count` (the structural accessor).
    Pages,
    /// `Document::page_content(0)`.
    Content,
    /// `Document::save`.
    Save,
}

impl Probe {
    /// Return the stable manifest token for this probe.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Pages => "pages",
            Self::Content => "content",
            Self::Save => "save",
        }
    }

    /// Parse a manifest probe token, returning `None` for an unknown token.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "open" => Some(Self::Open),
            "pages" => Some(Self::Pages),
            "content" => Some(Self::Content),
            "save" => Some(Self::Save),
            _ => None,
        }
    }
}

/// A single malformed fixture descriptor (generator source of truth).
#[derive(Debug, Clone, Copy)]
pub struct MalformedBuilder {
    /// File name relative to `fixtures/malformed/`.
    pub name: &'static str,
    /// Why this file exists and what makes it malformed.
    pub intent: &'static str,
    /// Expected error-category token (or `ok` for a documented success).
    pub category: &'static str,
    /// The operation whose outcome `category` describes.
    pub probe: Probe,
    /// Deterministic builder for the fixture bytes.
    pub build: fn() -> Vec<u8>,
}

/// One entry parsed back from `fixtures/malformed/manifest.toml`.
///
/// This is the type the `pdfxi-core` robustness test consumes. It carries both
/// the fine-grained `category`/`probe` pair and a coarse `error_category`
/// family used for family-level assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedFixture {
    /// File name relative to `fixtures/malformed/`.
    pub name: String,
    /// Why this file exists and what makes it malformed.
    pub intent: String,
    /// Fine-grained expected category token (or `ok` for a documented success).
    pub category: String,
    /// The operation whose outcome `category` describes.
    pub probe: String,
    /// Coarse expected error family (`none`, `header`, `startxref`, `xref`,
    /// `object`, `page-tree`, `stream`, `limits`, or `state`).
    pub error_category: String,
    /// Lower-case hexadecimal SHA-256 of the file bytes.
    pub sha256: String,
}

/// Backwards-compatible alias for [`MalformedFixture`].
pub type MalformedEntry = MalformedFixture;

/// Content bytes used by the canonical one-page stream object.
const CONTENT: &[u8] = b"BT /F1 24 Tf 72 720 Td (Hello, PDFXI!) Tj ET";

/// Return the first index of `needle` in `haystack`, if present.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Replace the first occurrence of `from` with `to`, preserving all other bytes.
///
/// Panics (in this generator-only helper) when `from` is absent, which signals a
/// bug in a descriptor rather than a condition the parser must tolerate.
fn replace_once(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let position = find(bytes, from).expect("replacement pattern must exist");
    let mut out = bytes.to_vec();
    out.splice(position..position + from.len(), to.iter().copied());
    out
}

/// Replace the numeric value following `startxref` with `value`.
fn set_startxref(bytes: &[u8], value: &[u8]) -> Vec<u8> {
    let marker = b"startxref\n";
    let start = find(bytes, marker).expect("startxref marker") + marker.len();
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    let mut out = bytes.to_vec();
    out.splice(start..end, value.iter().copied());
    out
}

/// Assemble a structurally valid classic-xref PDF from raw object bodies.
///
/// `trailer_entries` is inserted into the trailer dictionary verbatim; pass an
/// empty string for the default (`/Root 1 0 R` is always emitted). When `eof`
/// is false the closing `%%EOF` marker is omitted.
fn pdf(objects: &[Vec<u8>], trailer_entries: &str, eof: bool) -> Vec<u8> {
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
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R {trailer_entries}>>\nstartxref\n{xref}\n")
            .as_bytes(),
    );
    if eof {
        out.extend_from_slice(b"%%EOF\n");
    }
    out
}

/// Build a stream object body with the declared length `length`.
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

fn empty() -> Vec<u8> {
    Vec::new()
}

fn truncated_header() -> Vec<u8> {
    b"%PDF".to_vec()
}

fn wrong_version() -> Vec<u8> {
    replace_once(&pdf(&valid_objects(), "", true), b"%PDF-1.7", b"%PDF-9.9")
}

fn missing_startxref() -> Vec<u8> {
    replace_once(&pdf(&valid_objects(), "", true), b"startxref", b"XXXXXXXXX")
}

fn startxref_beyond_eof() -> Vec<u8> {
    set_startxref(&pdf(&valid_objects(), "", true), b"999999999")
}

fn negative_startxref() -> Vec<u8> {
    set_startxref(&pdf(&valid_objects(), "", true), b"-1")
}

fn absurd_object_offset() -> Vec<u8> {
    let bytes = pdf(&valid_objects(), "", true);
    let marker = b"0000000000 65535 f \n";
    let start = find(&bytes, marker).expect("free-list entry") + marker.len();
    let mut out = bytes;
    out.splice(start..start + 10, b"9999999999".iter().copied());
    out
}

fn xref_count_mismatch() -> Vec<u8> {
    replace_once(&pdf(&valid_objects(), "", true), b"0 6\n", b"0 3\n")
}

fn obj_without_endobj() -> Vec<u8> {
    let mut out = pdf(&valid_objects(), "", true);
    while let Some(position) = find(&out, b"endobj") {
        out.splice(position..position + 6, b"xxxxxx".iter().copied());
    }
    out
}

fn stream_length_too_long() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[3] = stream(99_999, CONTENT);
    pdf(&objects, "", true)
}

fn stream_length_too_short() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[3] = stream(4, CONTENT);
    pdf(&objects, "", true)
}

fn unterminated_string() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R /Title (unterminated".to_vec();
    pdf(&objects, "", true)
}

fn unterminated_hex() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R /ID <414243".to_vec();
    pdf(&objects, "", true)
}

fn unterminated_dict() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R".to_vec();
    pdf(&objects, "", true)
}

fn deep_nesting() -> Vec<u8> {
    let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
    let mut objects = valid_objects();
    objects[0] = format!("<< /Type /Catalog /Pages 2 0 R /Deep {deep} >>").into_bytes();
    pdf(&objects, "", true)
}

fn huge_count() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[1] = b"<< /Type /Pages /Kids [3 0 R] /Count 2000000000 >>".to_vec();
    pdf(&objects, "", true)
}

fn cyclic_page_tree() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[1] = b"<< /Type /Pages /Kids [2 0 R] /Count 1 >>".to_vec();
    pdf(&objects, "", true)
}

fn non_dict_kids() -> Vec<u8> {
    let mut objects = valid_objects();
    objects[1] = b"<< /Type /Pages /Kids 42 /Count 1 >>".to_vec();
    pdf(&objects, "", true)
}

fn garbage_encrypt() -> Vec<u8> {
    pdf(&valid_objects(), "/Encrypt 99 0 R ", true)
}

fn object_number_zero() -> Vec<u8> {
    replace_once(
        &pdf(&valid_objects(), "", true),
        b"/Root 1 0 R",
        b"/Root 0 0 R",
    )
}

fn missing_eof() -> Vec<u8> {
    pdf(&valid_objects(), "", false)
}

fn binary_garbage() -> Vec<u8> {
    b"\x00PDF-\x01\r\ngarbage\xff\x00\x80binary".to_vec()
}

fn ten_thousand_objects() -> Vec<u8> {
    replace_once(&pdf(&valid_objects(), "", true), b"0 6\n", b"0 10000\n")
}

/// All malformed fixtures, in manifest order.
pub fn all() -> Vec<MalformedBuilder> {
    vec![
        MalformedBuilder {
            name: "empty.pdf",
            intent: "Zero-byte file; header validation must reject without reading further.",
            category: "invalid_header",
            probe: Probe::Open,
            build: empty,
        },
        MalformedBuilder {
            name: "truncated-header.pdf",
            intent: "Header cut off before the '-' sentinel; rejected at open.",
            category: "invalid_header",
            probe: Probe::Open,
            build: truncated_header,
        },
        MalformedBuilder {
            name: "wrong-version.pdf",
            intent: "Unsupported declared version %PDF-9.9 is still parsed structurally.",
            category: "ok",
            probe: Probe::Pages,
            build: wrong_version,
        },
        MalformedBuilder {
            name: "missing-startxref.pdf",
            intent: "startxref keyword removed; structural parse must report the missing marker.",
            category: "missing_startxref",
            probe: Probe::Pages,
            build: missing_startxref,
        },
        MalformedBuilder {
            name: "startxref-beyond-eof.pdf",
            intent: "startxref points far past EOF; offset must be bounds-checked.",
            category: "startxref_out_of_bounds",
            probe: Probe::Pages,
            build: startxref_beyond_eof,
        },
        MalformedBuilder {
            name: "negative-startxref.pdf",
            intent: "Negative startxref value; must fail as an invalid offset, not wrap around.",
            category: "invalid_startxref",
            probe: Probe::Pages,
            build: negative_startxref,
        },
        MalformedBuilder {
            name: "absurd-object-offset.pdf",
            intent: "First in-use xref entry points past EOF; entry must be bounds-checked.",
            category: "malformed_xref",
            probe: Probe::Pages,
            build: absurd_object_offset,
        },
        MalformedBuilder {
            name: "xref-count-mismatch.pdf",
            intent: "Subsection declares fewer entries than objects; subsequent tokens must error.",
            category: "malformed_xref",
            probe: Probe::Pages,
            build: xref_count_mismatch,
        },
        MalformedBuilder {
            name: "obj-without-endobj.pdf",
            intent: "Every endobj keyword removed; full save must fail locating an object range.",
            category: "unexpected_token",
            probe: Probe::Save,
            build: obj_without_endobj,
        },
        MalformedBuilder {
            name: "stream-length-too-long.pdf",
            intent: "Stream /Length exceeds remaining bytes; decoding must report EOF.",
            category: "unexpected_eof",
            probe: Probe::Content,
            build: stream_length_too_long,
        },
        MalformedBuilder {
            name: "stream-length-too-short.pdf",
            intent: "Stream /Length understates the payload; parsing stays bounded.",
            category: "ok",
            probe: Probe::Content,
            build: stream_length_too_short,
        },
        MalformedBuilder {
            name: "unterminated-string.pdf",
            intent: "Catalog literal string has no closing ')'; tokenizer must stop at EOF.",
            category: "unexpected_eof",
            probe: Probe::Pages,
            build: unterminated_string,
        },
        MalformedBuilder {
            name: "unterminated-hex.pdf",
            intent: "Catalog hex string has no closing '>'; tokenizer must reject the bad token.",
            category: "unexpected_token",
            probe: Probe::Pages,
            build: unterminated_hex,
        },
        MalformedBuilder {
            name: "unterminated-dict.pdf",
            intent: "Catalog dictionary has no closing '>>'; tokenizer must reject the bad token.",
            category: "unexpected_token",
            probe: Probe::Pages,
            build: unterminated_dict,
        },
        MalformedBuilder {
            name: "deep-nesting.pdf",
            intent: "100 nested arrays exceed the depth limit; recursion must be bounded.",
            category: "nesting_too_deep",
            probe: Probe::Pages,
            build: deep_nesting,
        },
        MalformedBuilder {
            name: "huge-count.pdf",
            intent: "Absurd /Count is metadata only; real leaf pages are still resolved.",
            category: "ok",
            probe: Probe::Pages,
            build: huge_count,
        },
        MalformedBuilder {
            name: "cyclic-page-tree.pdf",
            intent: "/Pages kid points at itself; walk must detect the cycle.",
            category: "malformed_page_tree",
            probe: Probe::Pages,
            build: cyclic_page_tree,
        },
        MalformedBuilder {
            name: "non-dict-kids.pdf",
            intent: "/Kids is an integer instead of an array; tree walk must reject it.",
            category: "malformed_page_tree",
            probe: Probe::Pages,
            build: non_dict_kids,
        },
        MalformedBuilder {
            name: "garbage-encrypt.pdf",
            intent: "Trailer has a dangling /Encrypt reference; parsing tolerates it but save is refused.",
            category: "unsupported_state",
            probe: Probe::Save,
            build: garbage_encrypt,
        },
        MalformedBuilder {
            name: "object-number-zero.pdf",
            intent: "/Root references object 0 (the free-list head); resolution must reject it.",
            category: "free_object",
            probe: Probe::Pages,
            build: object_number_zero,
        },
        MalformedBuilder {
            name: "missing-eof.pdf",
            intent: "Missing %%EOF marker; the parser does not require it.",
            category: "ok",
            probe: Probe::Pages,
            build: missing_eof,
        },
        MalformedBuilder {
            name: "binary-garbage.pdf",
            intent: "Non-PDF binary bytes; header validation must reject before parsing.",
            category: "invalid_header",
            probe: Probe::Open,
            build: binary_garbage,
        },
        MalformedBuilder {
            name: "ten-thousand-objects.pdf",
            intent: "Xref declares 10,000 empty objects in a tiny file; the count guard rejects it.",
            category: "malformed_xref",
            probe: Probe::Pages,
            build: ten_thousand_objects,
        },
    ]
}

/// Map a fine-grained category to the coarse error family used by the
/// `pdfxi-core` robustness test's family-level assertions.
fn coarse_category(category: &str) -> &'static str {
    match category {
        "invalid_header" => "header",
        "ok" => "none",
        "missing_startxref" | "invalid_startxref" | "startxref_out_of_bounds" => "startxref",
        "malformed_xref" | "missing_xref" | "xref_cycle" | "missing_trailer" => "xref",
        "malformed_page_tree"
        | "free_object"
        | "object_not_found"
        | "missing_root"
        | "missing_pages"
        | "invalid_media_box" => "page-tree",
        "nesting_too_deep" => "limits",
        "unsupported_state" | "unsupported_filter" => "state",
        "unexpected_eof" => "stream",
        _ => "object",
    }
}

/// Emit the manifest TOML for the given fixtures.
pub fn emit_manifest(fixtures: &[MalformedBuilder]) -> String {
    let mut out = String::from(
        "# Committed malformed-PDF robustness corpus (T-0039).\n\
         # Generated by: cargo run -p pdfxi-testkit --example generate_malformed\n\
         # Every file is < 4 KiB. `category` is the expected error token for the\n\
         # declared `probe` (`ok` means a documented successful outcome).\n\n\
         version = 1\n",
    );
    for fixture in fixtures {
        out.push_str("\n[[malformed]]\n");
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
            crate::fixtures::toml_quote(fixture.category)
        ));
        out.push_str(&format!(
            "probe = {}\n",
            crate::fixtures::toml_quote(fixture.probe.as_str())
        ));
        out.push_str(&format!(
            "error_category = {}\n",
            crate::fixtures::toml_quote(coarse_category(fixture.category))
        ));
        out.push_str(&format!(
            "sha256 = {}\n",
            crate::fixtures::toml_quote(&crate::hash::sha256_hex(&(fixture.build)()))
        ));
    }
    out
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

/// Parse the flat `[[malformed]]` tables of a manifest TOML document.
pub fn parse_manifest(source: &str) -> Vec<MalformedEntry> {
    let mut entries = Vec::new();
    let mut current: Option<MalformedEntry> = None;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[malformed]]" {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(MalformedEntry {
                name: String::new(),
                intent: String::new(),
                category: String::new(),
                probe: String::new(),
                error_category: String::new(),
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
            "error_category" => entry.error_category = unquote(value),
            "sha256" => entry.sha256 = unquote(value),
            _ => {}
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

/// Return the path of the committed `fixtures/malformed/` directory.
pub fn corpus_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("workspace root")
        .join("fixtures")
        .join("malformed")
}

/// Read and parse the committed malformed corpus manifest from disk.
pub fn committed_manifest() -> Vec<MalformedFixture> {
    let source = std::fs::read_to_string(corpus_dir().join("manifest.toml"))
        .expect("fixtures/malformed/manifest.toml exists");
    parse_manifest(&source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_has_expected_count() {
        assert_eq!(all().len(), MALFORMED_COUNT);
        assert!((15..=25).contains(&MALFORMED_COUNT));
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
                bytes.len() < MALFORMED_FILE_MAX_BYTES,
                "{} is {} bytes",
                fixture.name,
                bytes.len()
            );
            total += bytes.len();
        }
        assert!(total < MALFORMED_CORPUS_SIZE_BUDGET, "total {total}");
    }

    #[test]
    fn builders_are_byte_stable() {
        for fixture in all() {
            assert_eq!(
                (fixture.build)(),
                (fixture.build)(),
                "{} is not byte-stable",
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
            assert_eq!(entry.category, fixture.category);
            assert_eq!(entry.probe, fixture.probe.as_str());
            assert_eq!(entry.error_category, coarse_category(fixture.category));
            assert_eq!(entry.sha256, crate::hash::sha256_hex(&(fixture.build)()));
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
        assert_eq!(
            on_disk, generated,
            "regenerate the malformed corpus manifest"
        );
    }
}
