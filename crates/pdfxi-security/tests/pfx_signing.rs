//! End-to-end PKCS#12 import against real Windows-exported containers.
//!
//! The `.pfx` files are **not committed**: they contain private keys. Generate
//! them locally with
//! `crates/pdfxi-security/tests/fixtures/pfx/generate.ps1`, which uses the
//! public test password `pdfxi-test-pfx`, then run this ignored test once:
//!
//! ```text
//! cargo test -p pdfxi-security --test pfx_signing -- --ignored --nocapture
//! ```
//!
//! The default gate must not depend on those files, so the test is `#[ignore]`.
//! The portable PKCS#12 unit tests live in `src/pkcs12` and use only synthetic
//! DER.

use std::path::PathBuf;

use pdfxi_core::Document;
use pdfxi_security::{
    SecurityError, SignOptions, SignatureStatus, SigningIdentity, sign_pdf, verify_signature,
};

/// Public password used by `generate.ps1`; not a secret.
const PASSWORD: &str = "pdfxi-test-pfx";
const SIGNING_TIME: u64 = 1_700_000_000;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pfx")
}

fn sample_pdf() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
    std::fs::read(&path).expect("read the real-world PDF fixture")
}

fn read_pfx(name: &str) -> Vec<u8> {
    let path = fixture_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "read {}: {error}; run tests/fixtures/pfx/generate.ps1 first",
            path.display()
        )
    })
}

fn assert_subject(identity: &SigningIdentity) {
    assert!(
        identity
            .signer_subject()
            .contains("PDFXI Test Signing Identity"),
        "unexpected signer subject: {}",
        identity.signer_subject()
    );
    assert!(identity.chain_len() >= 1);
}

#[test]
#[ignore = "requires local test PFX files from tests/fixtures/pfx/generate.ps1 (public password pdfxi-test-pfx); private keys are never committed"]
fn from_pfx_signs_and_verifies() {
    let pdf = sample_pdf();

    for name in ["leaf-3des.pfx", "leaf-aes256.pfx"] {
        let bytes = read_pfx(name);
        let identity = SigningIdentity::from_pfx(&bytes, PASSWORD)
            .unwrap_or_else(|error| panic!("{name} must import: {error}"));
        assert_subject(&identity);

        let mut document = Document::from_bytes(pdf.clone()).expect("parse the PDF fixture");
        let options = SignOptions {
            signing_time_unix: Some(SIGNING_TIME),
            ..SignOptions::default()
        };
        let signed = sign_pdf(&mut document, &identity, &options)
            .unwrap_or_else(|error| panic!("{name} must sign the fixture: {error}"));
        let verification = verify_signature(signed.bytes()).expect("verify the signed PDF");
        assert_eq!(verification.status(), SignatureStatus::Valid, "{name}");
        assert!(verification.is_fully_valid(), "{name}");
        assert_eq!(verification.signing_time_unix(), Some(SIGNING_TIME));
        assert!(
            verification
                .signer_subject()
                .contains("PDFXI Test Signing Identity"),
            "{name} subject: {}",
            verification.signer_subject()
        );

        // Wrong password: the MAC/ciphertext check reports the ambiguous
        // password error, never a key or bytes leak.
        assert_eq!(
            SigningIdentity::from_pfx(&bytes, "definitely-not-the-password").map(|_| ()),
            Err(SecurityError::PfxPasswordIncorrect),
            "{name} wrong-password mapping"
        );
    }
}

#[test]
#[ignore = "requires local test PFX files from tests/fixtures/pfx/generate.ps1 (public password pdfxi-test-pfx); private keys are never committed"]
fn corrupt_bytes_and_missing_key_are_typed() {
    let bytes = read_pfx("leaf-aes256.pfx");
    assert!(
        matches!(
            SigningIdentity::from_pfx(&bytes[..bytes.len() / 2], PASSWORD),
            Err(SecurityError::PfxMalformed(_))
        ),
        "a truncated container must be malformed, not a wrong password"
    );
    assert!(matches!(
        SigningIdentity::from_pfx(b"not a pfx", PASSWORD),
        Err(SecurityError::PfxMalformed(_))
    ));
    assert_eq!(
        SigningIdentity::from_pfx(&certificate_only_pfx(), PASSWORD).map(|_| ()),
        Err(SecurityError::PfxMissingPrivateKey)
    );
}

// --- Minimal DER encoder for the certificate-only container -----------------

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    if content.len() < 0x80 {
        out.push(content.len() as u8);
    } else {
        let bytes = content.len().to_be_bytes();
        let first = bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len() - 1);
        out.push(0x80 | (bytes.len() - first) as u8);
        out.extend_from_slice(&bytes[first..]);
    }
    out.extend_from_slice(content);
    out
}

fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut content = Vec::new();
    for part in parts {
        content.extend_from_slice(part);
    }
    tlv(0x30, &content)
}

fn explicit(value: &[u8]) -> Vec<u8> {
    tlv(0xa0, value)
}

fn oid(arcs: &[u32]) -> Vec<u8> {
    fn base128(mut value: u64, out: &mut Vec<u8>) {
        let mut buffer = [0_u8; 10];
        let mut index = buffer.len();
        loop {
            index -= 1;
            buffer[index] = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                break;
            }
        }
        let end = buffer.len() - 1;
        for byte in &mut buffer[index..end] {
            *byte |= 0x80;
        }
        out.extend_from_slice(&buffer[index..]);
    }

    let mut content = Vec::new();
    base128(u64::from(arcs[0]) * 40 + u64::from(arcs[1]), &mut content);
    for arc in &arcs[2..] {
        base128(u64::from(*arc), &mut content);
    }
    tlv(0x06, &content)
}

fn integer(value: u32) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len() - 1);
    tlv(0x02, &bytes[first..])
}

fn data_content(payload: &[u8]) -> Vec<u8> {
    sequence(&[
        oid(&[1, 2, 840, 113_549, 1, 7, 1]),
        explicit(&tlv(0x04, payload)),
    ])
}

/// A structurally valid, unencrypted PFX carrying one certificate bag and no
/// key bag. The certificate payload is opaque to the importer.
fn certificate_only_pfx() -> Vec<u8> {
    let certificate = tlv(0x04, b"synthetic certificate payload");
    let cert_bag = sequence(&[
        oid(&[1, 2, 840, 113_549, 1, 9, 22, 1]),
        explicit(&certificate),
    ]);
    let safe_bag = sequence(&[
        oid(&[1, 2, 840, 113_549, 1, 12, 10, 1, 3]),
        explicit(&cert_bag),
    ]);
    let authenticated_safe = sequence(&[data_content(&sequence(&[safe_bag]))]);
    sequence(&[integer(3), data_content(&authenticated_safe)])
}
