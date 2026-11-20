# T-0202 final install - real per-user installer from HEAD

- Date: 2026-11-20
- Build commit: `986c550` (`chore(orchestration): verify T-0201..T-0203, merge feature fragments`)
- Source tree at build time: HEAD `986c550` plus uncommitted docs-writer edits (README,
  docs/book, ACCEPTANCE, T-0192 evidence, UI_REVIEW). None of those touch binaries.
- Toolchain: rustc 1.100.0, cargo 1.100.0; `CARGO_TARGET_DIR=target/integration`.
- Inno Setup 6.7.3 (ISCC found at `%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe`).
- Session quirk applied: every Inno process ran with `%TEMP%`/`%TMP%` set to
  `<repo>\target\tmp\inno-temp` because this session cannot execute files from the
  default temp directory.

The installed binary is built from `986c550`, i.e. the commit **before** this evidence
file was committed. It is not signed; see
`orchestration/blockers/T-0202-code-signing.md`.

## 1. Release build

    cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
    Finished `release` profile [optimized] target(s) in 34.42s
    target/integration/release/pdfxi-ui.exe
      size 6,593,024 bytes
      sha256 B8C82C134CF1428F26EC360C68E93933876DF5EC029D594CC6724975D6BB3ABD

`scripts/package.ps1 -Installer` then rebuilt the workspace release (CLI incl. `pdfxi.exe`)
into the same target directory. One manifest warning was emitted by cargo for the
non-`ui` build: `unused dependency rfd` in `crates/pdfxi-app/Cargo.toml`; it is used only
under the `ui` feature and does not affect the packaged binary.

## 2. Package and installer

    powershell -NoProfile -File scripts/package.ps1 -Installer -TargetDir target/integration

    TARGET <repo>\target\integration
    PDFIUM %LOCALAPPDATA%\pdfxi\pdfium sha256=31d249b8535dd5f351ae7f4b714dc4bb1d76b6069b321a309bc99271dd65f566
    SAFETY ok=True layout=True notices=True secret_free=True hashes_verified=0
    MANIFEST ok=True layout=True notices=True secret_free=True hashes_verified=28
    PACKAGE OK zip=<repo>\target\package\pdfxi-portable-0.1.0-windows-x86_64.zip
    PACKAGE SHA256 1171f797ebd51cc01fd2e0df0e5903cf87b222705013049ce13a624fe1be0fb0
    VIEWER ok=True layout=True notices=True secret_free=True hashes_verified=0
    CHECK pdfium=ok %LOCALAPPDATA%\pdfxi\pdfium
    CHECK layout_and_notices=PASS

(The last three lines are from `scripts/package.ps1 -Check -TargetDir target/integration`
against the same packaged tree.)
    INSTALLER <repo>\target\package\pdfxi-setup-0.1.0-windows-x86_64.exe
    INSTALLER SHA256 72e4a613fe6b68b73cecb96bb774b7e7c18705ae9b3bb1ce82679c9475daf91a

| artefact | size | SHA-256 |
|---|---:|---|
| `pdfxi-portable-0.1.0-windows-x86_64.zip` | 7,696,499 | `1171f797ebd51cc01fd2e0df0e5903cf87b222705013049ce13a624fe1be0fb0` |
| `pdfxi-setup-0.1.0-windows-x86_64.exe` | 7,960,762 | `72e4a613fe6b68b73cecb96bb774b7e7c18705ae9b3bb1ce82679c9475daf91a` |

    powershell -NoProfile -File scripts/package.ps1 -SelfTest
    TEST 1 layout_validation PASS
    TEST 2 missing_notice_failure PASS
    TEST 3 manifest_determinism PASS
    TEST 4 skipbuild_path PASS
    TEST 4b skipbuild_missing_binary_rejected PASS
    TEST 5 secret_private_file_rejection PASS
    TEST 6 source_layout_and_notices PASS
    TEST 7 installer_copy PASS
    TEST 8 payload_bundle PASS ui=True pdfium=True licences=True entries=12
    TEST 9 viewer_binary_check PASS stub_rejected=1 real_accepted=1
    SELFTEST passed=10 failed=0

## 3. Real per-user install

    pdfxi-setup-0.1.0-windows-x86_64.exe /VERYSILENT /SUPPRESSMSGBOXES /CURRENTUSER /NORESTART /TASKS="desktopicon"
    install_exit=0

Installed into `%LOCALAPPDATA%\Programs\PDFXI`, replacing the previous copy-based install
(previous `pdfxi-ui.exe` was 6,221,824 bytes, SHA-256
`64FE7D467CFF3068861BF3DF2A4061C363B9EF04AB659D003BDE9E4039D21810`).

    %LOCALAPPDATA%\Programs\PDFXI
      pdfxi-ui.exe      6,593,024
      pdfxi.exe         1,888,768
      pdfium.dll        7,846,912
      pdfium-licenses\  19 files
      unins000.exe      4,747,010
      unins000.dat          7,755
      install.ps1, LICENSE, NOTICE, README.md, THIRD_PARTY.md, deny.toml, manifest.json

Installed viewer identity:

    sha256           B8C82C134CF1428F26EC360C68E93933876DF5EC029D594CC6724975D6BB3ABD
    equals build     true (target/integration/release/pdfxi-ui.exe)
    PE signature     PE
    PE subsystem     2 (Windows GUI, no console)
    ProductName      PDFXI
    FileVersion      0.1.0
    ProductVersion   0.1.0
    CompanyName      PDFXI contributors

HKCU uninstall entry
(`Software\Microsoft\Windows\CurrentVersion\Uninstall\{4C1F0A46-8E5B-4D2A-9C77-2B6A1E9F3D10}_is1`):

    DisplayName      PDFXI 0.1.0
    DisplayVersion   0.1.0
    DisplayIcon      %LOCALAPPDATA%\Programs\PDFXI\pdfxi-ui.exe
    UninstallString  "%LOCALAPPDATA%\Programs\PDFXI\unins000.exe"
    InstallLocation  %LOCALAPPDATA%\Programs\PDFXI\

Shortcuts (read with WScript.Shell):

    %USERPROFILE%\Desktop\PDFXI.lnk
      target      %LOCALAPPDATA%\Programs\PDFXI\pdfxi-ui.exe
      workingdir  %LOCALAPPDATA%\Programs\PDFXI
      arguments   (none)
    %APPDATA%\Microsoft\Windows\Start Menu\Programs\PDFXI.lnk
      target      %LOCALAPPDATA%\Programs\PDFXI\pdfxi-ui.exe
      workingdir  %LOCALAPPDATA%\Programs\PDFXI
      arguments   (none)

## 4. Launch smoke from the installed build

Started the installed exe with the real-world fixture as an absolute argument, from
`<repo>\target\tmp\smoke-cwd` (a different working directory), with `PDFIUM_PATH`
removed from the process environment:

    PDFIUM_PATH_set = False
    alive      = True after 5 s
    title      = [PDFXI]
    pdfium     = %LOCALAPPDATA%\Programs\PDFXI\pdfium.dll (1 module)
    in_install = True
    closed     = True, exit 0

## 5. Captures from the installed build

`scripts/capture-ui.ps1 -Exe $env:LOCALAPPDATA\Programs\PDFXI\pdfxi-ui.exe`:

    CAPTURED (printwindow) docs/evidence/ui/final-doc-light-windows.png (1920 x 1200)
    CAPTURED (printwindow) docs/evidence/ui/final-doc-dark-windows.png  (1920 x 1200)
    CAPTURED (printwindow) docs/evidence/ui/final-doc-light-macos.png   (1920 x 1200)
    CAPTURED (printwindow) docs/evidence/ui/final-doc-dark-macos.png    (1920 x 1200)
    CAPTURED (printwindow) docs/evidence/ui/final-home-dark-windows.png (1920 x 1200)

The four document captures opened
`<repo>\fixtures\realworld\microsoft-excel-2019-sample.pdf`; the home capture has no
document. Arguments used: `--light|--dark`, `--controls=windows|macos`.

## 6. Licence check

`cargo-deny` was not installed. The first `cargo install cargo-deny --locked` failed with
`Access is denied (os error 5)` building `quote`'s build script from the default
`%TEMP%`; re-running with `%TEMP%`/`%TMP%` set to `<repo>\target\tmp\cargo-temp` installed
cargo-deny 0.22.0.

    cargo deny check
    advisories ok, bans FAILED, licenses ok, sources ok
    exit 2

- **licenses ok**: `rfd` 0.15.4 is MIT, `ashpd` 0.11.1 is MIT, `pollster` 0.4.0 is
  Apache-2.0/MIT, `zbus` 5.19.0 is MIT. All are on the `deny.toml` allowlist.
- **bans FAILED**: wildcard dependencies declared as `path` deps with `*` in several
  crates (`pdfxi-app` 2, `pdfxi-cli` 6, `pdfxi-convert` 4, `pdfxi-testkit` 3, plus one
  each in core/render/security). This is pre-existing workspace style, unrelated to this
  release; `deny.toml` was not edited.
- **advisories ok**, **sources ok**.

## 7. Status

- Unsigned - see `orchestration/blockers/T-0202-code-signing.md`. Nothing in this
  transcript claims code signing.
- Installer row PKG-003 remains `in-progress`: the install above is a manual smoke, and
  there is still no automated installer test.
- Uninstall was not exercised in this round; the uninstaller and HKCU entry exist and
  were created by Inno Setup. A previous isolated `Smoke` AppId install/uninstall cycle is
  recorded in `docs/evidence/release/T-0202.md` R4.2.
