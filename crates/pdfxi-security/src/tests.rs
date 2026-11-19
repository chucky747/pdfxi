//! Unit tests for the security model.
//!
//! `unwrap`/`expect` are permitted here (test code only). These tests cover
//! permission combinations, owner-versus-user roles, algorithm ID round-trips,
//! signature validity, and wrong-password negative cases.

use crate::encryption::EncryptionMethod;
use crate::error::SecurityError;
use crate::permissions::{PasswordRole, Permission, Permissions};
use crate::protector::{MockProtector, Protector};
use crate::signature::{DigestAlgorithm, SignatureCoverage, SignatureInfo, SignatureStatus};

const ALL_FLAGS: [Permission; 8] = [
    Permission::PRINT,
    Permission::MODIFY,
    Permission::COPY,
    Permission::ANNOTATE,
    Permission::FILL_FORMS,
    Permission::ACCESSIBILITY,
    Permission::ASSEMBLE,
    Permission::PRINT_HIGH_QUALITY,
];

#[test]
fn permission_flags_are_distinct_and_combinable() {
    for (index, left) in ALL_FLAGS.iter().enumerate() {
        for right in ALL_FLAGS.iter().skip(index + 1) {
            assert_ne!(left, right, "flags must be distinct");
            assert_eq!(left.bits() & right.bits(), 0, "flags must not overlap");
        }
    }

    let combined = Permission::PRINT.union(Permission::COPY);
    assert!(combined.contains(Permission::PRINT));
    assert!(combined.contains(Permission::COPY));
    assert!(!combined.contains(Permission::MODIFY));

    let intersection = combined.intersection(Permission::COPY.union(Permission::MODIFY));
    assert_eq!(intersection, Permission::COPY);
    assert!(Permission::NONE.is_empty());
    assert!(!combined.is_empty());
}

#[test]
fn permission_mask_validation_rejects_unknown_bits() {
    let extra = Permission::ALL.bits() | (1 << 20);
    assert_eq!(Permission::from_bits_truncated(extra), Permission::ALL);
    assert_eq!(
        Permission::from_bits_checked(Permission::ALL.bits()),
        Ok(Permission::ALL)
    );
    assert_eq!(
        Permission::from_bits_checked(extra),
        Err(SecurityError::InvalidPermissionBits(extra))
    );
}

#[test]
fn permissions_user_role_only_allows_granted_bits() {
    let granted = Permission::PRINT.union(Permission::COPY);
    let permissions = Permissions::new(granted, PasswordRole::User);
    assert!(permissions.allows(Permission::PRINT));
    assert!(permissions.allows(Permission::COPY));
    assert!(!permissions.allows(Permission::MODIFY));
    assert!(!permissions.allows(Permission::ASSEMBLE));
    assert_eq!(permissions.role(), PasswordRole::User);
    assert_eq!(permissions.bits(), granted);
}

#[test]
fn permissions_owner_role_bypasses_all_restrictions() {
    let permissions = Permissions::new(Permission::NONE, PasswordRole::Owner);
    for flag in ALL_FLAGS {
        assert!(permissions.allows(flag), "owner must allow {flag:?}");
    }
    assert_eq!(permissions.role(), PasswordRole::Owner);
    assert!(permissions.bits().is_empty());
}

#[test]
fn encryption_method_ids_round_trip_for_all_algorithms() {
    for method in EncryptionMethod::ALL {
        let id = method.as_id();
        assert_eq!(EncryptionMethod::from_id(id), Some(method), "id {id}");
    }
    assert_eq!(EncryptionMethod::from_id("bogus"), None);
    assert_eq!(EncryptionMethod::from_id(""), None);
}

#[test]
fn encryption_method_key_lengths_and_families() {
    assert_eq!(EncryptionMethod::None.key_length_bits(), 0);
    assert!(!EncryptionMethod::None.is_encrypted());
    assert_eq!(EncryptionMethod::Rc4_40.key_length_bits(), 40);
    assert_eq!(EncryptionMethod::Rc4_128.key_length_bits(), 128);
    assert_eq!(EncryptionMethod::Aes128.key_length_bits(), 128);
    assert_eq!(EncryptionMethod::Aes256.key_length_bits(), 256);
    assert!(EncryptionMethod::Aes256.is_encrypted());
    assert_eq!(EncryptionMethod::Rc4_40.family(), "RC4");
    assert_eq!(EncryptionMethod::Aes256.family(), "AES");

    assert_eq!(EncryptionMethod::Aes256.accepts_key_length(256), Ok(()));
    assert_eq!(
        EncryptionMethod::Aes256.accepts_key_length(128),
        Err(SecurityError::InvalidKeyLength(128))
    );
}

#[test]
fn digest_algorithm_ids_round_trip() {
    for algorithm in DigestAlgorithm::ALL {
        let id = algorithm.as_id();
        assert_eq!(DigestAlgorithm::from_id(id), Some(algorithm), "id {id}");
    }
    assert_eq!(DigestAlgorithm::from_id("md5"), None);
    assert_eq!(DigestAlgorithm::Sha1.output_len_bytes(), 20);
    assert_eq!(DigestAlgorithm::Sha256.output_len_bytes(), 32);
    assert_eq!(DigestAlgorithm::Sha384.output_len_bytes(), 48);
    assert_eq!(DigestAlgorithm::Sha512.output_len_bytes(), 64);
}

#[test]
fn signature_status_distinguishes_valid_from_failure_states() {
    assert!(SignatureStatus::Valid.is_valid());
    for status in [
        SignatureStatus::Invalid,
        SignatureStatus::Tampered,
        SignatureStatus::Expired,
        SignatureStatus::Revoked,
        SignatureStatus::Unknown,
    ] {
        assert!(!status.is_valid(), "{status} must not be valid");
    }
}

#[test]
fn signature_info_exposes_all_metadata() {
    let signature = SignatureInfo::new(
        "Acme Signing Authority",
        1_700_000_000,
        DigestAlgorithm::Sha256,
        SignatureCoverage::WholeDocument,
        SignatureStatus::Valid,
    );
    assert_eq!(signature.signer(), "Acme Signing Authority");
    assert_eq!(signature.signed_at(), 1_700_000_000);
    assert_eq!(signature.digest_algorithm(), DigestAlgorithm::Sha256);
    assert_eq!(signature.coverage(), SignatureCoverage::WholeDocument);
    assert_eq!(signature.validity(), SignatureStatus::Valid);
    assert!(signature.is_valid());
    assert!(signature.coverage().covers_page(1));
    assert!(signature.coverage().covers_page(10_000));
}

#[test]
fn signature_coverage_enforces_page_ranges() {
    let range = SignatureCoverage::pages(2, 4).expect("valid page range");
    assert!(!range.covers_page(1));
    assert!(range.covers_page(2));
    assert!(range.covers_page(3));
    assert!(range.covers_page(4));
    assert!(!range.covers_page(5));
    assert_eq!(SignatureCoverage::pages(0, 3), None);
    assert_eq!(SignatureCoverage::pages(5, 2), None);
}

#[test]
fn mock_unlock_user_password_grants_user_role() {
    let protector = MockProtector::new(
        EncryptionMethod::Aes128,
        b"user-secret",
        b"owner-secret",
        Permission::PRINT,
    );
    let permissions = protector
        .unlock(b"user-secret")
        .expect("user password unlocks");
    assert_eq!(permissions.role(), PasswordRole::User);
    assert!(permissions.allows(Permission::PRINT));
    assert!(!permissions.allows(Permission::MODIFY));
    assert_eq!(protector.permissions(), permissions);
}

#[test]
fn mock_unlock_owner_password_grants_owner_role() {
    let protector = MockProtector::new(
        EncryptionMethod::Aes256,
        b"user-secret",
        b"owner-secret",
        Permission::NONE,
    );
    let permissions = protector
        .unlock(b"owner-secret")
        .expect("owner password unlocks");
    assert_eq!(permissions.role(), PasswordRole::Owner);
    assert!(permissions.allows(Permission::MODIFY));
    assert!(permissions.allows(Permission::ASSEMBLE));
}

#[test]
fn mock_unlock_rejects_wrong_and_empty_passwords() {
    let protector = MockProtector::new(
        EncryptionMethod::Rc4_128,
        b"user-secret",
        b"owner-secret",
        Permission::ALL,
    );
    assert_eq!(
        protector.unlock(b"wrong"),
        Err(SecurityError::WrongPassword)
    );
    assert_eq!(protector.unlock(b""), Err(SecurityError::WrongPassword));
    assert_eq!(
        protector.unlock(b"user-secreT"),
        Err(SecurityError::WrongPassword)
    );
    assert_eq!(
        protector.unlock(b"owner-secret "),
        Err(SecurityError::WrongPassword)
    );
}

#[test]
fn mock_protector_encrypt_decrypt_round_trip_for_all_methods() {
    let plaintext = b"pdfxi round-trip payload";
    for method in EncryptionMethod::ALL {
        let protector = MockProtector::new(method, b"u", b"o", Permission::NONE);
        assert_eq!(protector.method(), method);
        let ciphertext = protector.encrypt(plaintext).expect("encrypt succeeds");
        let restored = protector.decrypt(&ciphertext).expect("decrypt succeeds");
        assert_eq!(restored, plaintext, "method {method}");
        if method.is_encrypted() {
            assert_ne!(ciphertext.as_slice(), plaintext.as_slice());
        } else {
            assert_eq!(ciphertext.as_slice(), plaintext.as_slice());
        }
    }
}

#[test]
fn protector_trait_is_object_safe() {
    let protector: Box<dyn Protector> = Box::new(
        MockProtector::new(EncryptionMethod::Aes256, b"u", b"o", Permission::PRINT)
            .with_permissions(Permission::COPY),
    );
    assert_eq!(protector.method(), EncryptionMethod::Aes256);
    assert_eq!(
        protector.unlock(b"u").expect("unlock").role(),
        PasswordRole::User
    );
    assert_eq!(protector.permissions().bits(), Permission::COPY);
}

#[test]
fn error_messages_are_actionable() {
    assert!(
        SecurityError::WrongPassword
            .to_string()
            .contains("password")
    );
    assert!(
        SecurityError::CertificateRevoked
            .to_string()
            .contains("revoked")
    );
    assert!(
        SecurityError::SignatureExpired
            .to_string()
            .contains("expired")
    );
    assert!(
        SecurityError::InvalidKeyLength(7)
            .to_string()
            .contains("7 bits")
    );
    assert!(
        SecurityError::Configuration("bad".into())
            .to_string()
            .contains("bad")
    );
}
