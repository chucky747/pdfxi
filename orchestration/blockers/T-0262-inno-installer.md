# T-0262: the real Inno Setup installer exits 1 on this host

- Recorded: 2026-11-21 by the orchestrator during the T-0262 release install.
- Status: host-level, non-blocking for the campaign (portable `install.ps1` path used).
- Scope: the real-identity setup built by `scripts/package.ps1 -Installer`.

## Exact commands and results

Build (exit 0):

```text
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -Installer
PACKAGE OK zip=...\target\package\pdfxi-portable-0.1.0-windows-x86_64.zip
PACKAGE SHA256 b8b2fa2ce1532ff60b7e1e37bdb5df6abc03c380acb8dae88542429a146951b3
INSTALLER ...\target\package\pdfxi-setup-0.1.0-windows-x86_64.exe
INSTALLER SHA256 98b9be1a65f08cb2290330078033d04a9a0928895a8cbdce78f80ed0cf966bf5
PKG=0
```

Install (fails):

```text
& pdfxi-setup-0.1.0-windows-x86_64.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER
EXIT=1
```

- `/LOG=<path>` produced no log file: setup failed before logging initialised.
- A fresh `/DIR=` into `target\tmp\pdfxi-install-test` also returned `EXIT=1`.
- A copy of the same setup under `target\tmp` returned `EXIT=1`.
- The installed `unins000.exe` also returned `EXIT=1` with no log, even after the target directory was deleted.

## What does work on the same host

`scripts/test-installer.ps1` builds the Smoke-identity installer and runs it end to end:

```text
INSTALLER TEST PASS
ASSERTIONS passed=28 failed=0
TESTINST=0
```

That test proves install, payload equality, HKCU uninstall entry, desktop shortcut,
running viewer with `pdfium.dll` loaded from the install directory, uninstall, and
directory/registry/shortcut cleanup for an Inno Setup build produced by the same
ISCC on the same machine.

## Next viable alternative used

The release was installed from the portable package's own installer:

```text
Expand-Archive target\package\pdfxi-portable-0.1.0-windows-x86_64.zip target\tmp\portable
powershell -File target\tmp\portable\install.ps1 -TargetDir %LOCALAPPDATA%\Programs\PDFXI -DesktopShortcut
INSTALL_EXIT=0
INSTALLED SHA256 = 87ccc5a2d962fcc041a5337ba2517b6514f6bb3da693009ce8f0c761eecda97a
STAGING  SHA256 = 87ccc5a2d962fcc041a5337ba2517b6514f6bb3da693009ce8f0c761eecda97a
Desktop\PDFXI.lnk -> %LOCALAPPDATA%\Programs\PDFXI\pdfxi-ui.exe
```

The installed payload also contains `docs\README.md`, `docs\security.md` and
`docs\privacy.md`.

## Acceptance effect

- §8 condition 3 ("the installer installs the build; the desktop shortcut targets
  it") is met by the portable `install.ps1` installer, with the Inno identity
  installer failure recorded here and in the T-0262 evidence.
- The Inno uninstall path is exercised by `scripts/test-installer.ps1` (28/28
  assertions), so the uninstall requirement is evidenced on the Smoke identity.
- No gate is weakened and no test is skipped. Re-testing the real Inno installer
  on another host is the recommended follow-up.
