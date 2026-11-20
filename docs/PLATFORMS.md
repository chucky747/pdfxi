# Platform support

PDFXI is developed Windows-first. The canonical workspace host is Windows 11
x64 with the GNU toolchain pinned in `rust-toolchain.toml`.

| Platform | Build | Test | Notes |
|---|---|---|---|
| Windows x86_64 GNU | yes | yes | canonical development host |
| Windows x86_64 MSVC | CI job | CI job | `.github/workflows/ci.yml` |
| Linux x86_64 | `cargo check --workspace --all-targets --target x86_64-unknown-linux-gnu` | CI job (`ubuntu-latest`) | windowing crates need GTK/X11/Wayland headers |
| macOS aarch64 | CI job | CI job (`macos-14`) | no local host available in the canonical workspace |

## Local cross-target check

```powershell
rustup target add x86_64-unknown-linux-gnu
$env:CARGO_TARGET_DIR='target/cross'
cargo check --workspace --all-targets --target x86_64-unknown-linux-gnu
```

`cargo check` does not link, so no Linux linker is required. The check still
compiles every crate and test target for the foreign target, which catches
`cfg`-gated and pointer-width problems that a Windows-only build hides.

## Renderer and OCR boundaries

PDFium and Tesseract are loaded dynamically at runtime through `pdfium.dll`,
`pdfium.so`, or `libpdfium.dylib`, and a Tesseract sidecar binary discovered by
the host. The libraries compile without either present; a missing renderer
returns a typed error.

## Platform integration rows

PLAT-001, PLAT-004, PLAT-005, PLAT-006, and PLAT-007 are delivered as
host-agnostic library contracts. PLAT-002 is delivered as the cross-platform
build matrix above plus the local cross-target check. Windows-specific
registry, shell, and single-instance behaviour stays a host boundary and is
listed verbatim in the affected row notes.
