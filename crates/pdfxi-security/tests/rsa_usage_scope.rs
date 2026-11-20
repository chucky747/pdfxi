//! Scope guard for the scoped RUSTSEC-2023-0071 ignore.
//!
//! `deny.toml` ignores RUSTSEC-2023-0071 because `pdfxi-security` uses `rsa`
//! only for signing and verification. The advisory is a PKCS#1 v1.5
//! *decryption* padding-oracle timing issue, so the ignore is only honest while
//! no RSA decryption API enters `crates/pdfxi-security/src`.
//!
//! This test scans every Rust source file under this crate's `src` directory and
//! fails if one of the forbidden tokens appears in code. Text after `//` on a
//! line is treated as a comment and ignored; block comments are not stripped,
//! which only makes the guard stricter. This test file itself lives in `tests`,
//! outside the scanned directory, so the token literals below do not trip it.
//!
//! Evidence and rationale: `orchestration/blockers/T-0241-rsa-advisory.md`.

use std::fs;
use std::path::{Path, PathBuf};

/// RSA decryption entry points that would invalidate the scoped ignore.
const FORBIDDEN_TOKENS: [&str; 4] = [
    "DecryptingKey",
    "Pkcs1v15Encrypt",
    "Oaep",
    "rsa::RsaPrivateKey::decrypt",
];

/// Collect every `*.rs` file below `directory`, depth first.
fn rust_sources(directory: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            found.push(path);
        }
    }
}

#[test]
fn rsa_is_used_for_signing_and_verification_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    assert!(
        !files.is_empty(),
        "guard scanned no Rust sources under {}; the scope check would be vacuous",
        root.display()
    );

    let mut hits = Vec::new();
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            // Ignore line comments: rationale text may legitimately name a
            // forbidden API. Code before `//` is still scanned.
            let code = line.split("//").next().unwrap_or("");
            for token in FORBIDDEN_TOKENS {
                if code.contains(token) {
                    hits.push(format!("{}:{}: {token}", file.display(), index + 1));
                }
            }
        }
    }

    assert!(
        hits.is_empty(),
        "RSA decryption API found in pdfxi-security; the scoped RUSTSEC-2023-0071 \
         ignore in deny.toml is no longer valid. Remove the ignore or remove the \
         decryption path, and update orchestration/blockers/T-0241-rsa-advisory.md.\n{}",
        hits.join("\n")
    );
}
