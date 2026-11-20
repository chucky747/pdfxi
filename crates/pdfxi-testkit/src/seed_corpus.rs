//! Deterministic fuzz seed corpus: generator, descriptor manifest and checks.
//!
//! The five libFuzzer targets in `fuzz/fuzz_targets/` are backed by the plain
//! entry points in [`crate::fuzz`]. Those entry points are also exercised over the
//! committed seeds in `fuzz/corpus/<target>/`. This module makes that corpus
//! reproducible without committing binary blobs by hand: [`seeds`] returns the
//! complete, byte-stable seed set, [`write_corpus`] writes it to disk when an
//! example is invoked with the [`GENERATE_ENV`] flag, and [`manifest_text`]
//! renders a SHA-256 descriptor so the committed bytes can be audited.
//!
//! Generation uses no randomness beyond a fixed-seed xorshift and no dependence
//! on the host, so every invocation produces identical bytes on every machine.
//! The corpus stays small (well under 1 KiB per seed) and contains no secrets or
//! customer data.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::hash::sha256_hex;
use crate::{minimal_pdf, multipage_pdf};

/// Manifest schema version, bumped only when the field layout changes.
pub const MANIFEST_VERSION: u32 = 1;

/// Environment variable that gates on-disk corpus generation.
///
/// The generator example is a no-op unless this variable is set to `1`, so an
/// accidental `cargo run` cannot rewrite committed seeds.
pub const GENERATE_ENV: &str = "PDFXI_GENERATE_FUZZ_SEEDS";

/// Fuzz targets covered by the generated corpus, in stable report order.
pub const TARGETS: [&str; 5] = ["parser", "filters", "content", "forms", "crypto"];

/// One generated seed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    /// Target directory name under `fuzz/corpus/`.
    pub target: &'static str,
    /// File name of the seed.
    pub name: &'static str,
    /// Seed bytes.
    pub bytes: Vec<u8>,
}

/// One parsed row from `fuzz/corpus/manifest.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Target directory name.
    pub target: String,
    /// Seed file name.
    pub name: String,
    /// Expected byte length.
    pub bytes: usize,
    /// Expected lower-case SHA-256 digest.
    pub sha256: String,
}

/// Why a manifest could not be parsed or verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// The descriptor text is missing a required field or is malformed.
    Malformed(String),
    /// The descriptor version is unsupported.
    UnsupportedVersion(u32),
    /// A listed seed file is missing from the corpus.
    Missing(String),
    /// A listed seed file has a different length than recorded.
    SizeMismatch(String),
    /// A listed seed file has a different SHA-256 than recorded.
    HashMismatch(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(formatter, "malformed manifest: {why}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported manifest version: {version}")
            }
            Self::Missing(what) => write!(formatter, "missing seed: {what}"),
            Self::SizeMismatch(what) => write!(formatter, "size mismatch: {what}"),
            Self::HashMismatch(what) => write!(formatter, "hash mismatch: {what}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Advance a fixed-seed xorshift generator by one byte.
fn next_byte(state: &mut u64) -> u8 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u8
}

/// Produce `len` deterministic bytes from `seed`.
fn pseudo_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        out.push(next_byte(&mut state));
    }
    out
}

/// Build a seed whose body is a short textual prefix plus deterministic bytes.
fn prefixed(prefix: &[u8], seed: u64, len: usize) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(&pseudo_bytes(seed, len));
    out
}

fn parser_seeds() -> Vec<Seed> {
    let full = minimal_pdf();
    let truncated_len = full.len() / 2;
    vec![
        Seed {
            target: "parser",
            name: "valid-1page.pdf",
            bytes: full.clone(),
        },
        Seed {
            target: "parser",
            name: "valid-3page.pdf",
            bytes: multipage_pdf(3),
        },
        Seed {
            target: "parser",
            name: "header-junk.bin",
            bytes: prefixed(b"%PDF-1.7\n%PDFXI\n", 0xA1, 96),
        },
        Seed {
            target: "parser",
            name: "xref-truncated.pdf",
            bytes: full[..truncated_len].to_vec(),
        },
    ]
}

fn filter_seeds() -> Vec<Seed> {
    // The filter fuzz entry point derives PNG/TIFF predictor parameters from the
    // first four bytes, so these seeds deliberately vary those four bytes.
    vec![
        Seed {
            target: "filters",
            name: "predictor-png-up.bin",
            bytes: prefixed(&[139, 3, 8, 16], 0xB1, 80),
        },
        Seed {
            target: "filters",
            name: "predictor-tiff.bin",
            bytes: prefixed(&[130, 1, 8, 8], 0xB2, 64),
        },
        Seed {
            target: "filters",
            name: "flate-chain.bin",
            bytes: prefixed(&[1, 1, 8, 1], 0xB3, 64),
        },
        Seed {
            target: "filters",
            name: "empty-params.bin",
            bytes: vec![0, 0, 0, 0],
        },
    ]
}

fn content_seeds() -> Vec<Seed> {
    vec![
        Seed {
            target: "content",
            name: "text-operators.bin",
            bytes: b"BT /F1 12 Tf 72 720 Td (Hello Seed) Tj ET\n".to_vec(),
        },
        Seed {
            target: "content",
            name: "escaped-string.bin",
            bytes: b"BT /F1 12 Tf (a\\(b\\)c\\\\d) Tj ET\n".to_vec(),
        },
        Seed {
            target: "content",
            name: "random.bin",
            bytes: pseudo_bytes(0xC3, 128),
        },
    ]
}

fn form_seeds() -> Vec<Seed> {
    let xfdf = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><fields>",
        "<field name=\"seed\"><value>Deterministic</value></field>",
        "</fields></xfdf>\n",
    );
    let fdf = concat!(
        "%FDF-1.7\n1 0 obj\n",
        "<< /FDF << /Fields [<< /T (seed) /V (Deterministic) >>] >> >>\n",
        "endobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n",
    );
    vec![
        Seed {
            target: "forms",
            name: "simple.xfdf",
            bytes: xfdf.as_bytes().to_vec(),
        },
        Seed {
            target: "forms",
            name: "simple.fdf",
            bytes: fdf.as_bytes().to_vec(),
        },
        Seed {
            target: "forms",
            name: "bad-utf8.bin",
            bytes: pseudo_bytes(0xD3, 64),
        },
    ]
}

fn crypto_seeds() -> Vec<Seed> {
    let mut oid = vec![0x06, 0x03, 0x2A, 0x03, 0x04];
    oid.extend_from_slice(&pseudo_bytes(0xE2, 32));
    vec![
        Seed {
            target: "crypto",
            name: "der-sequence.bin",
            bytes: prefixed(&[0x30, 0x00], 0xE1, 32),
        },
        Seed {
            target: "crypto",
            name: "oid-variants.bin",
            bytes: oid,
        },
        Seed {
            target: "crypto",
            name: "random-variants.bin",
            bytes: pseudo_bytes(0xE3, 96),
        },
    ]
}

/// Return the complete, byte-stable generated seed set.
///
/// The order is fixed: targets follow [`TARGETS`] and seeds are grouped by
/// target in the order shown here. Two calls always return equal vectors.
pub fn seeds() -> Vec<Seed> {
    let mut out = Vec::new();
    out.extend(parser_seeds());
    out.extend(filter_seeds());
    out.extend(content_seeds());
    out.extend(form_seeds());
    out.extend(crypto_seeds());
    out
}

/// Return the absolute path of the `fuzz/corpus/` directory.
pub fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fuzz")
        .join("corpus")
}

/// Return the absolute path of the generated seed manifest.
pub fn manifest_path() -> PathBuf {
    corpus_root().join("manifest.toml")
}

/// Strip surrounding double quotes from a TOML basic-string scalar.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// Render the descriptor manifest for [`seeds`].
///
/// The output is deterministic: field order, comments and hashes are stable, and
/// only the recorded digests/sizes vary if a seed body changes.
pub fn manifest_text() -> String {
    let mut out = String::new();
    out.push_str("# T-0144 deterministic fuzz seed descriptor.\n");
    out.push_str("# Generated by: PDFXI_GENERATE_FUZZ_SEEDS=1 cargo run -p pdfxi-testkit --example generate_fuzz_seeds\n");
    out.push_str("# Pre-existing seeds under each target directory are not listed here; this\n");
    out.push_str("# manifest covers exactly the seeds produced by `seeds()`.\n\n");
    out.push_str(&format!("version = {MANIFEST_VERSION}\n\n"));
    for seed in seeds() {
        out.push_str("[[seed]]\n");
        out.push_str(&format!("target = \"{}\"\n", seed.target));
        out.push_str(&format!("name = \"{}\"\n", seed.name));
        out.push_str(&format!("bytes = {}\n", seed.bytes.len()));
        out.push_str(&format!("sha256 = \"{}\"\n\n", sha256_hex(&seed.bytes)));
    }
    out
}

/// Parse a seed descriptor with a small dependency-free line reader.
pub fn parse_manifest(text: &str) -> Result<Vec<ManifestEntry>, ManifestError> {
    let mut version: Option<u32> = None;
    let mut entries = Vec::new();
    let mut current: Option<(String, String, Option<usize>, Option<String>)> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("version") {
            let value = value
                .split_once('=')
                .ok_or_else(|| ManifestError::Malformed("version has no '='".to_owned()))?;
            version =
                Some(
                    value.1.trim().parse().map_err(|_| {
                        ManifestError::Malformed("version is not numeric".to_owned())
                    })?,
                );
            continue;
        }
        if line == "[[seed]]" {
            if let Some((target, name, bytes, sha256)) = current.take() {
                entries.push(finish_entry(target, name, bytes, sha256)?);
            }
            current = Some((String::new(), String::new(), None, None));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(entry) = current.as_mut() else {
            continue;
        };
        match key.trim() {
            "target" => entry.0 = unquote(value),
            "name" => entry.1 = unquote(value),
            "bytes" => {
                entry.2 = Some(value.trim().parse().map_err(|_| {
                    ManifestError::Malformed("seed bytes is not numeric".to_owned())
                })?);
            }
            "sha256" => entry.3 = Some(unquote(value)),
            _ => {}
        }
    }
    if let Some((target, name, bytes, sha256)) = current.take() {
        entries.push(finish_entry(target, name, bytes, sha256)?);
    }

    let version = version.ok_or_else(|| ManifestError::Malformed("no version field".to_owned()))?;
    if version != MANIFEST_VERSION {
        return Err(ManifestError::UnsupportedVersion(version));
    }
    if entries.is_empty() {
        return Err(ManifestError::Malformed("no seed entries".to_owned()));
    }
    Ok(entries)
}

fn finish_entry(
    target: String,
    name: String,
    bytes: Option<usize>,
    sha256: Option<String>,
) -> Result<ManifestEntry, ManifestError> {
    let label = if name.is_empty() {
        target.clone()
    } else {
        name.clone()
    };
    if target.is_empty() || name.is_empty() {
        return Err(ManifestError::Malformed(format!(
            "seed missing target or name ({label})"
        )));
    }
    let bytes =
        bytes.ok_or_else(|| ManifestError::Malformed(format!("seed {label} has no bytes")))?;
    let sha256 =
        sha256.ok_or_else(|| ManifestError::Malformed(format!("seed {label} has no sha256")))?;
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ManifestError::Malformed(format!(
            "seed {label} sha256 is not 64 lower-case hex chars"
        )));
    }
    Ok(ManifestEntry {
        target,
        name,
        bytes,
        sha256,
    })
}

/// Verify every manifest entry against its committed seed file.
///
/// Returns one `(target, name, sha256)` triple per verified seed in manifest
/// order. This is the audit trail printed by the corpus smoke test.
pub fn verify_corpus(
    root: &Path,
    manifest: &[ManifestEntry],
) -> Result<Vec<(String, String, String)>, ManifestError> {
    let mut audit = Vec::with_capacity(manifest.len());
    for entry in manifest {
        let path = root.join(&entry.target).join(&entry.name);
        let label = format!("{}/{}", entry.target, entry.name);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ManifestError::Missing(label));
            }
            Err(error) => {
                return Err(ManifestError::Malformed(format!(
                    "cannot read {label}: {error}"
                )));
            }
        };
        if bytes.len() != entry.bytes {
            return Err(ManifestError::SizeMismatch(label));
        }
        let digest = sha256_hex(&bytes);
        if digest != entry.sha256 {
            return Err(ManifestError::HashMismatch(label));
        }
        audit.push((entry.target.clone(), entry.name.clone(), digest));
    }
    Ok(audit)
}

/// Write every generated seed and the descriptor manifest under `root`.
///
/// Existing files with the same names are overwritten, which makes regeneration
/// idempotent. Parent directories are created as needed. No path outside `root`
/// is written because target and file names come from the fixed [`seeds`] list.
pub fn write_corpus(root: &Path) -> io::Result<usize> {
    for seed in seeds() {
        let directory = root.join(seed.target);
        fs::create_dir_all(&directory)?;
        fs::write(directory.join(seed.name), &seed.bytes)?;
    }
    fs::write(root.join("manifest.toml"), manifest_text())?;
    Ok(seeds().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_are_byte_stable_across_calls() {
        assert_eq!(seeds(), seeds());
        assert_eq!(manifest_text(), manifest_text());
    }

    #[test]
    fn seeds_cover_every_target_with_non_empty_bodies() {
        let found = seeds();
        for target in TARGETS {
            assert!(
                found.iter().any(|seed| seed.target == target),
                "no seeds for {target}"
            );
        }
        assert!(found.iter().all(|seed| !seed.bytes.is_empty()));
    }

    #[test]
    fn manifest_round_trips_the_generated_seeds() {
        let text = manifest_text();
        let parsed = parse_manifest(&text).expect("generated manifest parses");
        assert_eq!(parsed.len(), seeds().len());
        for (entry, seed) in parsed.iter().zip(seeds().iter()) {
            assert_eq!(entry.target, seed.target);
            assert_eq!(entry.name, seed.name);
            assert_eq!(entry.bytes, seed.bytes.len());
            assert_eq!(entry.sha256, sha256_hex(&seed.bytes));
        }
    }

    #[test]
    fn parse_manifest_rejects_bad_input() {
        assert!(matches!(
            parse_manifest("version = 1\n"),
            Err(ManifestError::Malformed(_))
        ));
        let wrong_version = concat!(
            "version = 9\n",
            "[[seed]]\n",
            "target = \"x\"\n",
            "name = \"y\"\n",
            "bytes = 1\n",
            "sha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
        );
        assert!(matches!(
            parse_manifest(wrong_version),
            Err(ManifestError::UnsupportedVersion(9))
        ));
        assert!(matches!(
            parse_manifest("[[seed]]\ntarget = \"x\"\n"),
            Err(ManifestError::Malformed(_))
        ));
    }
}
