//! Contract tests for the packaging and installer gate.
//!
//! The installer itself is exercised end to end by `scripts/test-installer.ps1`
//! (run as `cargo xtask installer-test`), which builds the smoke-identity setup,
//! installs it into `target\tmp`, checks the payload, the HKCU entry and the
//! running viewer, then uninstalls and verifies the real installation is
//! untouched. These tests only guard that the gate cannot silently disappear:
//! the script, the separate smoke identity and the xtask wiring must stay.

use std::path::{Path, PathBuf};

/// Repository root, two levels above this crate.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/pdfxi-cli lives two levels below the repository root")
        .to_path_buf()
}

/// Read a repository file, failing the test with its path when it is missing.
fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

#[test]
fn installer_test_script_contract() {
    let script = read("scripts/test-installer.ps1");
    for required in [
        "SKIPPED:",
        "/DSmoke=1",
        "unins000.exe",
        "pdfium.dll",
        "HKEY_CURRENT_USER",
        "uninstaller exited 0",
        "INSTALLER TEST PASS",
        "INSTALLER TEST FAILED",
        "target\\tmp",
    ] {
        assert!(
            script.contains(required),
            "scripts/test-installer.ps1 no longer asserts `{required}`"
        );
    }
    let iss = read("installer/pdfxi.iss");
    assert!(
        iss.contains("#if Smoke"),
        "installer/pdfxi.iss lost the Smoke identity branch"
    );
    assert!(
        iss.contains("PDFXI-Smoke"),
        "the smoke installation directory is not separate from the real one"
    );
    assert!(
        iss.contains("PDFXI Smoke"),
        "the smoke application name is not separate from the real one"
    );
    let xtask = read("xtask/src/main.rs");
    assert!(
        xtask.contains("InstallerTest") && xtask.contains("scripts/test-installer.ps1"),
        "`cargo xtask installer-test` is not wired to scripts/test-installer.ps1"
    );
}
