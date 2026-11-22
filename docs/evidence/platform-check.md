# PLAT-002 - cross-platform build evidence

Date: 2026-11-19. Owner: integrator.

## Local cross-target check (Linux x86_64)

```text
$ rustup target add x86_64-unknown-linux-gnu
$ $env:CARGO_TARGET_DIR='target/cross'
$ cargo check --workspace --all-targets --target x86_64-unknown-linux-gnu
    Checking pdfxi-core v0.1.0 (D:\Documents\opencode-work\crates\pdfxi-core)
    Checking pdfxi-forms v0.1.0 (D:\Documents\opencode-work\crates\pdfxi-forms)
    Checking pdfxi-security v0.1.0 (D:\Documents\opencode-work\crates\pdfxi-security)
    Checking pdfxi-convert v0.1.0 (D:\Documents\opencode-work\crates\pdfxi-convert)
    Checking pdfxi-testkit v0.1.0 (D:\Documents\opencode-work\crates\pdfxi-testkit)
    Checking pdfxi-cli v0.1.0 (D:\Documents\opencode-work\crates\pdfxi-cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.70s
CROSS=0
```

Every workspace crate and every test target compiles for
`x86_64-unknown-linux-gnu` from the Windows host. `cargo check` does not link,
so no Linux linker is required.

## CI matrix

`.github/workflows/ci.yml` defines four jobs:

- `windows-gnu` - fmt, clippy `-D warnings`, tests, docs, feature verify, M0 gate
- `windows-msvc` - tests
- `linux-x11` - GTK/X11/Wayland headers, `cargo check --workspace --all-targets`, tests
- `macos-arm64` - `cargo check --workspace --all-targets`, tests

## Scope

PLAT-002 is the cross-platform build matrix and the local cross-target check;
see `docs/PLATFORMS.md`. Platform-specific shell, registry, and single-instance
behaviour stays a host boundary and is recorded in the PLAT-001/005/006 notes.
The macOS job is CI-only because no macOS host exists in the canonical
workspace; it is not claimed as locally executed.

## Native Linux re-check (T-0354)

The Windows-hosted cross-target check above is superseded for `PLAT-002` by a
native Linux check in WSL Ubuntu 24.04, because `ring`'s build script needs a
Linux C toolchain. Portability-only unused imports were gated and the `ui`-only
dependencies of `pdfxi-app` were made optional, so the default-feature check is
warning-free:

```text
$ RUSTUP_TOOLCHAIN=stable CARGO_TARGET_DIR=$PWD/target/linux-wsl cargo check --workspace --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 25.04s
EXIT:0
```

Full evidence: `docs/evidence/platform/T-0354.md`.