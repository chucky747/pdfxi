//! Integration tests for the committed M2 authoring corpus.
//!
//! These tests read `fixtures/authoring-manifest.toml`, verify every byte on
//! disk against the manifest, parse every fixture through the real
//! `pdfxi_core::Document`, and assert the feature-specific structure of each
//! fixture. They also exercise the golden snapshot helpers. The suite is
//! deterministic, offline, and completes in well under a second.
//!
//! `pdfxi-core` is a *test-only* dependency here; Cargo permits the resulting
//! dev-dependency cycle because `pdfxi-core` likewise dev-depends on testkit.

use std::fs;
use std::path::PathBuf;

use pdfxi_core::Rectangle;
use pdfxi_testkit::authoring;
use pdfxi_testkit::golden::{ContentSnapshot, PageTreeSnapshot};
use pdfxi_testkit::hash::sha256_hex;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn manifest_source() -> String {
    let path = authoring::manifest_path().expect("authoring manifest path");
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    authoring::by_name(name)
        .unwrap_or_else(|| panic!("unknown authoring fixture {name}"))
        .build()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

#[test]
fn authoring_manifest_is_integral() {
    let fixtures = authoring::all();
    let entries = authoring::parse_manifest(&manifest_source());
    assert_eq!(fixtures.len(), authoring::AUTHORING_FIXTURE_COUNT);
    assert_eq!(entries.len(), fixtures.len());

    let mut names: Vec<&str> = fixtures.iter().map(|fixture| fixture.file_name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(names.len(), before, "fixture names must be unique");

    for entry in &entries {
        let fixture = authoring::by_name(&entry.name)
            .unwrap_or_else(|| panic!("manifest names unknown fixture {}", entry.name));
        assert_eq!(entry.feature, fixture.feature.as_str());
        assert!(authoring::AuthoringFeature::parse(&entry.feature).is_some());
        assert_eq!(entry.pages, fixture.pages);
        assert!(entry.tags.contains(&entry.feature));
        assert_eq!(entry.size, fixture.build().len());
        assert!(entry.size > 0);
        assert_eq!(entry.sha256.len(), 64);
        assert!(
            entry
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "hash for {} must be lower-case hex",
            entry.name
        );
    }
}

#[test]
fn authoring_checksums_are_stable() {
    let entries = authoring::parse_manifest(&manifest_source());
    for fixture in authoring::all() {
        let first = fixture.build();
        let second = fixture.build();
        assert_eq!(first, second, "{} must be byte-stable", fixture.file_name);

        let entry = entries
            .iter()
            .find(|entry| entry.name == fixture.file_name)
            .unwrap_or_else(|| panic!("manifest missing {}", fixture.file_name));
        assert_eq!(sha256_hex(&first), entry.sha256, "{}", fixture.file_name);
        assert_eq!(first.len(), entry.size, "{}", fixture.file_name);
    }
}

#[test]
fn every_authoring_fixture_parses_through_pdfxi_core() {
    for fixture in authoring::all() {
        let document = pdfxi_core::Document::from_bytes(fixture.build())
            .unwrap_or_else(|error| panic!("{}: {error}", fixture.file_name));
        assert_eq!(document.version(), "1.7", "{}", fixture.file_name);
        assert_eq!(
            document
                .page_count()
                .unwrap_or_else(|error| { panic!("{} page count: {error}", fixture.file_name) }),
            fixture.pages,
            "{}",
            fixture.file_name
        );
        assert_eq!(document.pages().expect("pages").len(), fixture.pages);
        for index in 0..fixture.pages {
            assert!(
                !document
                    .page_text(index)
                    .unwrap_or_else(|error| {
                        panic!("{} page {index} text: {error}", fixture.file_name)
                    })
                    .is_empty(),
                "{} page {index} has no text",
                fixture.file_name
            );
        }
    }
}

#[test]
fn page_box_fixture_exposes_all_five_boxes() {
    let bytes = fixture_bytes("authoring-page-boxes.pdf");
    let document = pdfxi_core::Document::from_bytes(bytes.clone()).expect("page-box document");
    let boxes = document.page_boxes(0).expect("page boxes");

    assert_eq!(boxes.media(), Rectangle::new(0.0, 0.0, 612.0, 792.0));
    assert_eq!(boxes.crop(), Rectangle::new(10.0, 10.0, 602.0, 782.0));
    assert_eq!(boxes.bleed(), Rectangle::new(8.0, 8.0, 604.0, 784.0));
    assert_eq!(boxes.trim(), Rectangle::new(12.0, 12.0, 600.0, 780.0));
    assert_eq!(boxes.art(), Rectangle::new(14.0, 14.0, 598.0, 778.0));
    for kind in pdfxi_core::BoxKind::ALL {
        assert!(boxes.is_explicit(kind), "{kind:?} should be explicit");
    }

    let snapshot = PageTreeSnapshot::from_pdf(&bytes).expect("page-tree snapshot");
    assert_eq!(snapshot.pages().len(), 1);
    assert_eq!(snapshot.pages()[0].boxes.len(), 5);
}

#[test]
fn rotation_fixture_carries_expected_rotations() {
    let bytes = fixture_bytes("authoring-rotation.pdf");
    let document = pdfxi_core::Document::from_bytes(bytes.clone()).expect("rotation document");
    assert_eq!(document.page_count().expect("pages"), 3);

    let effective: Vec<u16> = (0..3)
        .map(|index| pdfxi_core::pages::effective_rotation(&document, index).expect("rotation"))
        .collect();
    assert_eq!(effective, vec![90, 180, 270]);

    let snapshot = PageTreeSnapshot::from_pdf(&bytes).expect("page-tree snapshot");
    let recorded: Vec<i64> = snapshot.pages().iter().map(|page| page.rotation).collect();
    assert_eq!(recorded, vec![90, 180, 270]);
}

#[test]
fn annotation_fixture_has_all_four_subtypes() {
    let bytes = fixture_bytes("authoring-annotations.pdf");
    let text = String::from_utf8(bytes).expect("ASCII fixture");
    for subtype in ["Text", "Highlight", "Link", "Widget"] {
        assert!(
            text.contains(&format!("/Subtype /{subtype}")),
            "missing /Subtype /{subtype}"
        );
    }
    assert!(text.contains("/Annots [6 0 R 7 0 R 8 0 R 9 0 R]"));
    assert!(text.contains("/QuadPoints [50 720 300 720 50 700 300 700]"));
    let document =
        pdfxi_core::Document::from_bytes(text.into_bytes()).expect("annotation document");
    assert_eq!(document.page_count().expect("pages"), 1);
}

#[test]
fn form_fixture_has_text_checkbox_and_choice_fields() {
    let bytes = fixture_bytes("authoring-forms.pdf");
    let text = String::from_utf8(bytes).expect("ASCII fixture");
    assert!(text.contains("/AcroForm 6 0 R"), "missing /AcroForm");
    assert!(
        text.contains("/Fields [7 0 R 8 0 R 9 0 R]"),
        "missing /Fields"
    );
    for field_type in ["Tx", "Btn", "Ch"] {
        assert!(
            text.contains(&format!("/FT /{field_type}")),
            "missing /FT /{field_type}"
        );
    }
    assert!(
        text.contains("/Opt [(one) (two) (three)]"),
        "missing choices"
    );
    let document = pdfxi_core::Document::from_bytes(text.into_bytes()).expect("form document");
    assert_eq!(document.page_count().expect("pages"), 1);
}

#[test]
fn encrypted_fixture_handles_documented_password() {
    let bytes = fixture_bytes("authoring-encrypted.pdf");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Filter /Standard"));
    assert!(text.contains("/V 1"));
    assert!(text.contains("/R 2"));
    assert!(text.contains("/Encrypt 6 0 R"));
    assert!(text.contains("/ID [<"));

    let encryption = authoring::TestEncryption::documented();
    assert!(text.contains(&format!("/O <{}>", to_hex(&encryption.owner_entry))));
    assert!(text.contains(&format!("/U <{}>", to_hex(&encryption.user_entry))));
    assert_eq!(encryption.permissions, -44);
    assert!(encryption.password_matches(authoring::TEST_USER_PASSWORD.as_bytes()));
    assert!(encryption.owner_password_matches(authoring::TEST_OWNER_PASSWORD.as_bytes()));
    assert!(!encryption.password_matches(b"wrong-password"));
    assert!(!encryption.password_matches(b""));
    assert!(!encryption.owner_password_matches(authoring::TEST_USER_PASSWORD.as_bytes()));
    assert!(!authoring::TEST_USER_PASSWORD.is_empty());
    assert!(!authoring::TEST_OWNER_PASSWORD.is_empty());

    // A parsed encrypted document still exposes its structure; saving is
    // intentionally out of scope for this fixture.
    let document = pdfxi_core::Document::from_bytes(bytes).expect("encrypted document");
    assert_eq!(document.page_count().expect("pages"), 1);
}

#[test]
fn page_label_fixture_has_a_number_tree() {
    let bytes = fixture_bytes("authoring-page-labels.pdf");
    let text = String::from_utf8(bytes).expect("ASCII fixture");
    assert!(text.contains("/PageLabels 10 0 R"), "missing /PageLabels");
    assert!(
        text.contains("/Nums [0 << /S /D /> 1 << /S /r /St 1 /P (Sec-) /> ]"),
        "missing number tree"
    );
    let document = pdfxi_core::Document::from_bytes(text.into_bytes()).expect("labels document");
    assert_eq!(document.page_count().expect("pages"), 3);
}

#[test]
fn generators_reproduce_committed_authoring_bytes() {
    let root = workspace_root();
    for fixture in authoring::all() {
        let path = root
            .join("fixtures")
            .join("authoring")
            .join(fixture.file_name);
        let on_disk =
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert_eq!(
            on_disk,
            fixture.build(),
            "{} is stale; regenerate the authoring corpus",
            fixture.file_name
        );
    }
}

#[test]
fn golden_snapshots_are_stable_and_content_normalized() {
    let bytes = fixture_bytes("authoring-page-boxes.pdf");
    let first = PageTreeSnapshot::from_pdf(&bytes).expect("first snapshot");
    let second = PageTreeSnapshot::from_pdf(&bytes).expect("second snapshot");
    assert!(first.matches(&second));
    assert_eq!(first.fingerprint(), second.fingerprint());

    let content = ContentSnapshot::new(b"BT /F1 18 Tf 72 720 Td (Hello) Tj ET\n\n");
    let same = ContentSnapshot::new(b"BT /F1 18 Tf 72 720 Td (Hello) Tj ET");
    assert!(content.matches(&same));
    assert_eq!(content.fingerprint(), same.fingerprint());
}
