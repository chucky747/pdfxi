//! Protect / unprotect a real-world PDF through the public library API.
//!
//! These tests use `fixtures/realworld/microsoft-excel-2019-sample.pdf`
//! (a hybrid cross-reference-stream source with object streams) as the proof
//! that protection works on a document the user actually has, not on a
//! synthetic one-page fixture. They also leave the protected and unprotected
//! bytes under `target/lanes/security2/t0238-out/` for the reviewer.

use std::fs;
use std::path::{Path, PathBuf};

use pdfxi_core::Document;
use pdfxi_security::{
    DecryptionReport, EncryptionMethod, PasswordRole, Permission, ProtectOptions,
    ProtectPermissions, SecurityError, open_encrypted, protect_document, remove_protection,
};

const FIXTURE: &str = "microsoft-excel-2019-sample.pdf";
const USER_PASSWORD: &[u8] = b"pdfxi-test";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn fixture(name: &str) -> PathBuf {
    workspace_root()
        .join("fixtures")
        .join("realworld")
        .join(name)
}

fn artifact(name: &str) -> PathBuf {
    workspace_root()
        .join("target")
        .join("lanes")
        .join("security2")
        .join("t0238-out")
        .join(name)
}

fn write_artifact(name: &str, bytes: &[u8]) {
    let path = artifact(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create artifact directory");
    }
    fs::write(&path, bytes).expect("write artifact");
    assert!(path.is_file(), "artifact missing: {}", path.display());
}

/// A consistent "print and fill, but do not copy or change" grant.
fn restricted() -> ProtectPermissions {
    ProtectPermissions {
        print: true,
        print_high_quality: true,
        modify: false,
        copy: false,
        annotate: false,
        fill_forms: true,
        accessibility: true,
        assemble: false,
    }
}

fn assert_permissions(report: DecryptionReport, expected: ProtectPermissions) {
    let bits = report.permissions.bits();
    assert_eq!(bits, expected.to_permission().expect("consistent grants"));
    assert_eq!(report.permissions.allows(Permission::PRINT), expected.print);
    assert_eq!(
        report.permissions.allows(Permission::PRINT_HIGH_QUALITY),
        expected.print_high_quality
    );
    assert_eq!(
        report.permissions.allows(Permission::MODIFY),
        expected.modify
    );
    assert_eq!(report.permissions.allows(Permission::COPY), expected.copy);
    assert_eq!(
        report.permissions.allows(Permission::ANNOTATE),
        expected.annotate
    );
    assert_eq!(
        report.permissions.allows(Permission::FILL_FORMS),
        expected.fill_forms
    );
    assert_eq!(
        report.permissions.allows(Permission::ACCESSIBILITY),
        expected.accessibility
    );
    assert_eq!(
        report.permissions.allows(Permission::ASSEMBLE),
        expected.assemble
    );
}

#[test]
fn real_world_aes256_protect_opens_with_password_and_round_trips() {
    let source_bytes = fs::read(fixture(FIXTURE)).expect("read real-world fixture");
    let source = Document::from_bytes(source_bytes.clone()).expect("fixture parses");
    let source_pages = source.page_count().expect("fixture page count");
    let source_text = source.page_text(0).expect("fixture first-page text");
    assert!(source_pages > 1, "fixture should be multi-page");
    assert!(!source_text.is_empty(), "fixture first page has no text");

    let protected = protect_document(
        &source,
        ProtectOptions::new(USER_PASSWORD).with_permissions(restricted()),
    )
    .expect("protect with AES-256");

    assert!(
        protected
            .windows(b"/Encrypt".len())
            .any(|window| window == b"/Encrypt"),
        "protected output must carry an /Encrypt entry"
    );
    assert_ne!(
        protected, source_bytes,
        "output must differ from the source"
    );
    write_artifact("protected.pdf", &protected);

    // Opening with the user password yields the user role and the masked bits.
    let (opened, report) =
        open_encrypted(&protected, Some(USER_PASSWORD)).expect("open with the user password");
    assert_eq!(report.method, EncryptionMethod::Aes256);
    assert_eq!(report.revision, 6, "AES-256 defaults to revision 6");
    assert_eq!(report.role, PasswordRole::User);
    assert_permissions(report, restricted());
    assert_eq!(opened.page_count().expect("protected pages"), source_pages);
    assert_eq!(
        opened.page_text(0).expect("protected first-page text"),
        source_text,
        "page text must survive protection"
    );

    // The open password is required and a wrong one is typed.
    assert_eq!(
        open_encrypted(&protected, None).err(),
        Some(SecurityError::MissingPassword)
    );
    assert_eq!(
        open_encrypted(&protected, Some(b"not-the-password")).err(),
        Some(SecurityError::WrongPassword)
    );
    assert_eq!(
        remove_protection(&protected, b"not-the-password").err(),
        Some(SecurityError::WrongPassword)
    );

    // Removing protection writes plaintext with no /Encrypt entry.
    let unprotected = remove_protection(&protected, USER_PASSWORD)
        .expect("remove protection with the user password");
    assert!(
        !unprotected
            .windows(b"/Encrypt".len())
            .any(|window| window == b"/Encrypt"),
        "unprotected output must not carry an /Encrypt entry"
    );
    assert_ne!(unprotected, protected);
    write_artifact("unprotected.pdf", &unprotected);

    let plain = Document::from_bytes(unprotected.clone()).expect("unprotected parses");
    assert_eq!(plain.page_count().expect("unprotected pages"), source_pages);
    assert_eq!(
        plain.page_text(0).expect("unprotected first-page text"),
        source_text,
        "page text must survive protection removal"
    );
    assert_eq!(
        open_encrypted(&unprotected, Some(USER_PASSWORD)).err(),
        Some(SecurityError::EncryptedDocumentInvalid(
            "no standard /Encrypt dictionary was found".to_owned()
        )),
        "unprotected bytes carry no encryption dictionary"
    );
}

#[test]
fn owner_password_only_opens_without_a_prompt_and_keeps_read_only_rights() {
    let source_bytes = fs::read(fixture(FIXTURE)).expect("read real-world fixture");
    let source = Document::from_bytes(source_bytes).expect("fixture parses");
    let source_text = source.page_text(0).expect("fixture first-page text");

    let options = ProtectOptions::new(b"")
        .with_owner_password(b"owner-pdfxi-test")
        .with_permissions(ProtectPermissions::READ_ONLY);
    let protected =
        protect_document(&source, options).expect("protect with an owner password only");

    // An empty user password opens without a prompt but stays in the user role.
    let (opened, report) =
        open_encrypted(&protected, Some(b"")).expect("open with an empty user password");
    assert_eq!(report.method, EncryptionMethod::Aes256);
    assert_eq!(report.revision, 6);
    assert_eq!(report.role, PasswordRole::User);
    assert_permissions(report, ProtectPermissions::READ_ONLY);
    assert_eq!(
        opened.page_text(0).expect("protected first-page text"),
        source_text
    );
    assert_eq!(
        open_encrypted(&protected, None).err(),
        Some(SecurityError::MissingPassword)
    );

    // The owner password bypasses the mask, as PDF owner semantics require.
    let (_, owner) = open_encrypted(&protected, Some(b"owner-pdfxi-test"))
        .expect("open with the owner password");
    assert_eq!(owner.role, PasswordRole::Owner);
    assert!(owner.permissions.allows(Permission::MODIFY));
    assert!(owner.permissions.allows(Permission::COPY));

    // Either password can remove protection; the empty open password works.
    let unprotected =
        remove_protection(&protected, b"").expect("remove with the empty user password");
    assert!(
        !unprotected
            .windows(b"/Encrypt".len())
            .any(|window| window == b"/Encrypt")
    );
    let plain = Document::from_bytes(unprotected).expect("unprotected parses");
    assert_eq!(
        plain.page_text(0).expect("unprotected first-page text"),
        source_text
    );
}

#[test]
fn protection_errors_are_typed_and_never_silent() {
    let source_bytes = fs::read(fixture(FIXTURE)).expect("read real-world fixture");
    let source = Document::from_bytes(source_bytes.clone()).expect("fixture parses");

    // Contradictory grants never reach the writer.
    let contradictory = ProtectPermissions {
        annotate: true,
        modify: false,
        ..ProtectPermissions::NONE
    };
    assert!(matches!(
        protect_document(
            &source,
            ProtectOptions::new(USER_PASSWORD).with_permissions(contradictory)
        ),
        Err(SecurityError::PermissionConflict(_))
    ));

    // Encryption with no effective password is refused.
    assert!(matches!(
        protect_document(&source, ProtectOptions::new(b"")),
        Err(SecurityError::Configuration(_))
    ));

    // Passwords longer than the revision 6 limit are refused, not truncated.
    let long = vec![b'x'; pdfxi_security::MAX_PASSWORD_BYTES + 1];
    assert!(matches!(
        protect_document(&source, ProtectOptions::new(&long)),
        Err(SecurityError::Configuration(_))
    ));

    // A plaintext document has no protection to remove.
    assert_eq!(
        remove_protection(&source_bytes, USER_PASSWORD).err(),
        Some(SecurityError::DocumentNotEncrypted)
    );

    // Protecting an already-encrypted document is refused, not double-encrypted.
    let protected = protect_document(
        &source,
        ProtectOptions::new(USER_PASSWORD).with_permissions(restricted()),
    )
    .expect("protect");
    let (decrypted, _) = open_encrypted(&protected, Some(USER_PASSWORD)).expect("decrypt");

    // A `Document` parsed directly from encrypted bytes keeps the `/Encrypt`
    // trailer and must be rejected.
    let encrypted_document = Document::from_bytes(protected).expect("ciphertext still parses");
    assert_eq!(
        protect_document(&encrypted_document, ProtectOptions::new(USER_PASSWORD)).err(),
        Some(SecurityError::DocumentAlreadyEncrypted)
    );
    assert_eq!(
        decrypted.page_text(0).expect("decrypted text"),
        source.page_text(0).expect("source text")
    );
}

#[test]
fn explicit_aes128_and_rc4_revisions_are_covered() {
    let source_bytes = fs::read(fixture(FIXTURE)).expect("read real-world fixture");
    let source = Document::from_bytes(source_bytes).expect("fixture parses");

    for (method, revision) in [
        (EncryptionMethod::Aes128, 4),
        (EncryptionMethod::Rc4_128, 3),
        (EncryptionMethod::Rc4_40, 2),
    ] {
        let protected = protect_document(
            &source,
            ProtectOptions::new(USER_PASSWORD)
                .with_encryption(method)
                .with_permissions(restricted()),
        )
        .expect("protect with an explicit method");
        let (opened, report) =
            open_encrypted(&protected, Some(USER_PASSWORD)).expect("open the protected file");
        assert_eq!(report.method, method);
        assert_eq!(report.revision, revision);
        assert_eq!(report.role, PasswordRole::User);
        assert!(opened.page_text(0).is_ok());
    }

    // AES-256 revision 5 is accepted explicitly; revision 4 is not.
    assert!(
        protect_document(
            &source,
            ProtectOptions::new(USER_PASSWORD)
                .with_encryption(EncryptionMethod::Aes256)
                .with_revision(5)
        )
        .is_ok()
    );
    assert!(matches!(
        protect_document(
            &source,
            ProtectOptions::new(USER_PASSWORD)
                .with_encryption(EncryptionMethod::Aes256)
                .with_revision(4)
        ),
        Err(SecurityError::Configuration(_))
    ));
}
