# Installing PDFXI

PDFXI `0.1.5` is a Windows x64 build. Two delivery forms exist: a per-user
Inno Setup installer and a portable zip. Both are signed with a local
self-signed certificate; read [Signing and trust](#signing-and-trust) before you
run one.

## Installer

The installer is built by `scripts/package.ps1` when Inno Setup 6's `ISCC.exe`
is available. The output is:

```text
target/package/pdfxi-setup-0.1.5-windows-x86_64.exe
```

It installs **for the current user only** (`PrivilegesRequired=lowest`): no
elevation, no UAC prompt, no machine-wide files.

- Default location: `%LOCALAPPDATA%\Programs\PDFXI`.
- Start Menu group **PDFXI**, with the viewer and an uninstall entry.
- A desktop icon is offered as a task (selected once, not forced).
- An optional "Open with" entry can be added. The installer does not claim the
  `.pdf` default handler.
- An uninstall entry is written under
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall`.
- After installation the viewer can be launched from the wizard or the
  shortcut.

The installed payload is the same as the portable archive: `pdfxi-ui.exe`,
`pdfxi.exe`, `pdfium.dll`, `pdfium-licenses\`, `LICENSE`, `NOTICE`,
`README.md`, `THIRD_PARTY.md`, `deny.toml`, `manifest.json`, and `install.ps1`.

### Uninstall

Use **Settings > Apps > PDFXI > Uninstall**, the Start Menu uninstall entry, or
`unins000.exe` in the installation directory. The uninstaller removes the
application directory and the shortcuts it created. It does **not** remove
`%APPDATA%\PDFXI`, so viewer settings survive a reinstall. Delete that folder
yourself to reset the viewer.

If an older uninstaller is broken (the 0.1.2 setup's uninstaller exits 1 after a
portable overlay), run the checked-in recovery script:

```powershell
powershell -NoProfile -File .\scripts\uninstall.ps1
```

It removes only `%LOCALAPPDATA%\Programs\PDFXI`, the matching HKCU uninstall
entry, and shortcuts that point into that directory. Add `-RemoveSettings` to
delete `%APPDATA%\PDFXI` as well.

## Portable zip

The portable archive is:

```text
target/package/pdfxi-portable-0.1.5-windows-x86_64.zip
```

It contains the viewer, the CLI, the pinned `pdfium.dll` with its licences, the
notices, `install.ps1`, and a SHA-256 `manifest.json`. Extract it anywhere and
run `pdfxi-ui.exe`; keep `pdfium.dll` and `pdfium-licenses\` next to the
executable.

The bundled `install.ps1` copies the payload to a directory and creates a Start
Menu group **PDFXI** with viewer and CLI shortcuts, without elevation or
registry writes. It also removes the pre-0.1.5 root-level links that pointed at
the same target, so setup and portable installs no longer duplicate entries:

```powershell
powershell -NoProfile -File .\install.ps1 `
    -TargetDir "$env:LOCALAPPDATA\Programs\PDFXI" -DesktopShortcut
```

`-NoShortcut` skips the Start Menu links. The script installs only the explicit
payload allowlist and copies `pdfium.dll` only from the extracted archive.

## Where PDFium comes from

PDFium is not built by this project. The Windows x64 binary is pinned and
fetched by `scripts/fetch-pdfium.ps1`:

| Item | Value |
|---|---|
| Release tag | `chromium/8165` (PDFium 160.0.8165.0) |
| Asset | `pdfium-win-x64.tgz` from `bblanchon/pdfium-binaries` |
| Archive check | SHA-256 pinned in `scripts/fetch-pdfium.ps1`; mismatch fails closed |
| Cache | `%LOCALAPPDATA%\pdfxi\pdfium\bin\pdfium.dll` by default |
| Licence | BSD-3-Clause (PDFium) plus wrapper notices, shipped in `pdfium-licenses\` |

The script does not use `releases/latest`, does not download an unhashed
archive, and does not modify `Path` or any other persistent environment
variable.

### Loading policy

- **Release builds** load `pdfium.dll` from the directory of the running
  executable only. There is no search of the current directory, `Path`, or an
  environment variable, which prevents DLL planting next to a document.
- **Developer builds** additionally accept `PDFIUM_PATH` and the per-user cache
  above, so tests and `cargo xtask verify` work without copying the DLL.
- Packaging verifies the DLL hash and fails if the bundled copy does not match
  the pin. `pdfium.dll` is third-party and is shipped unmodified, not signed.

PDFium also enforces the input size cap: a document larger than 2 GiB is
rejected before it is loaded. The same cap applies in the viewer and in the
library.

## Signing and trust

Release binaries and the setup executable are signed with a **local self-signed
certificate** generated on the build machine (`CN=PDFXI Local Build, O=PDFXI`).
`scripts/new-signing-cert.ps1` creates it and `scripts/trust-signing-cert.ps1`
trusts it in the current user's Root and TrustedPublisher stores, so the
signature verifies on this machine.

- This is a local publisher, **not a publicly trusted one**. Other machines do
  not trust the certificate, and Windows SmartScreen can still warn there.
- A public OV/EV code-signing certificate is still required for distribution;
  see `orchestration/blockers/T-0202-code-signing.md`.
- The setup executable is signed too, before its `.sha256` sidecar is written.
- `pdfium.dll` stays unsigned by design; it is shipped byte-identical to the
  pinned upstream archive.

Verify a download against the `.sha256` sidecar or `manifest.json`, and check
`Get-AuthenticodeSignature` for `Valid` after trusting the certificate.

## Build from source

Prerequisites: the pinned stable Rust toolchain from `rust-toolchain.toml`
(with `rustfmt` and `clippy`), PowerShell 7 or Windows PowerShell, and a working
Windows linker.

```powershell
# 1. Pinned PDFium for tests and the packaged viewer
powershell -NoProfile -File scripts/fetch-pdfium.ps1

# 2. Viewer and CLI
cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
cargo build --release -p pdfxi-cli

# 3. Package (portable zip, and the installer when ISCC.exe exists)
powershell -NoProfile -File scripts/package.ps1 -SkipBuild
```

`scripts/package.ps1` without `-SkipBuild` builds the workspace first. Set
`ISCC_PATH` if Inno Setup is installed somewhere the script does not find it;
without `ISCC.exe` the portable zip is still produced and the installer step is
skipped with a message.

The viewer binary has no console window in release builds. On a startup failure
it writes to `%LOCALAPPDATA%\PDFXI\logs\pdfxi-ui.log` and shows a message box.

### Resource compiler

The icon, VERSIONINFO resource, and per-monitor-v2 manifest are embedded by
`crates/pdfxi-app/build.rs` through a Windows resource compiler, normally
`windres` from the GNU toolchain. No resource crate is used.

- If no resource compiler is found, the build still succeeds with a warning,
  but the executable has no icon, version resource, or manifest.
- `scripts/package.ps1` rejects such an executable, so a package cannot silently
  ship an unversioned binary.
- `PDFXI_RC` can point at a specific resource compiler.

Only the `x86_64-pc-windows-gnu` target has been built and packaged. The MSVC
toolchain has not been verified.

## Verify a build

```text
cargo xtask verify          # fmt, clippy, tests, docs; includes ui,pdfium tests
cargo xtask docs            # documentation page presence check
```

`cargo xtask verify` requires PDFium. It checks `PDFIUM_PATH`, the per-user
cache, the workspace root, the current directory, and `Path`, and fails with a
message pointing at `scripts/fetch-pdfium.ps1` when none is present.

`cargo xtask goal-verify --all` is green in `0.1.5`: every ADR 0009 row is
`verified` or terminal, and PLAT-002 stays `deferred` because the project is
Windows-only. `cargo xtask features verify` reports 146 rows, 142 `verified`
(58 `gui`, 84 `cli`), 0 `library`, 1 `model`, 1 `deferred` and 3 `replaced`. See
the [feature matrix](../../FEATURES.md), [release notes](../../releases/0.1.5.md)
and ADR 0009.
