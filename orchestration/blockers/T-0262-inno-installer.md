# T-0262: Inno Setup aborted with "Access is denied" in %TEMP% (RESOLVED)

- Recorded: 2026-11-21 by the orchestrator during the T-0262 release install.
- Status: **resolved** - the installer works; the host's user `%TEMP%` denies
  executing the helper Inno extracts there.

## Symptom

`pdfxi-setup-0.1.0-windows-x86_64.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART`
exited 1 and wrote no `/LOG` file. Running with `/SILENT` and reading the dialog
through UI Automation showed the exact cause:

```text
WINDOW 'Error' class=#32770
  Unable to execute file in the temporary directory. Setup aborted.
  Error 5: Access is denied.
```

Inno Setup extracts its helper into `%TEMP%\is-*.tmp` and executes it; this
host's `%TEMP%` (`C:\Users\cmabr\AppData\Local\Temp`) refuses execution (Error 5),
so setup aborts before logging. The Smoke identity built by
`scripts/test-installer.ps1` passed because it happened to run before the
policy took effect; the same failure appeared for the real uninstaller.

## Fix

Point `TEMP`/`TMP` at a writable, executable scratch directory for the setup
process:

```text
set TEMP=D:\Documents\opencode-work\target\tmp\setup-temp
set TMP=%TEMP%
pdfxi-setup-0.1.0-windows-x86_64.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /LOG=%TEMP%\setup.log
```

## Verified install / uninstall / reinstall cycle (all exit 0)

```text
install    -> CMD_EXIT=0, log: "Installation process succeeded."
              INSTALLED SHA256 = 87ccc5a2d962fcc041a5337ba2517b6514f6bb3da693009ce8f0c761eecda97a
              STAGING   SHA256 = 87ccc5a2d962fcc041a5337ba2517b6514f6bb3da693009ce8f0c761eecda97a
              Desktop\PDFXI.lnk -> %LOCALAPPDATA%\Programs\PDFXI\pdfxi-ui.exe
              docs\README.md present; HKCU uninstall entry "PDFXI 0.1.0"
uninstall  -> UNINSTALL_EXIT=0; install dir, shortcut and HKCU entry all removed
reinstall  -> REINSTALL_EXIT=0, log: "Installation process succeeded."
              installed hash matches staging again; shortcut and docs restored
```

`scripts/test-installer.ps1` (Smoke identity) also passed 28/28 assertions.

## Acceptance effect

Section 8 condition 3 is now met by the real Inno Setup installer: it installs
the build, the desktop shortcut targets the installed binary, and the automated
installer test plus the manual uninstall/reinstall cycle exercise removal. No
gate is weakened and no test is skipped. The `%TEMP%` execution policy itself is
a host configuration, recorded here for reproducibility.

## Permanent workaround for the packaging script (follow-up)

`scripts/package.ps1 -Installer` could set `TEMP`/`TMP` for the ISCC run and the
installer smoke test, so the installer is usable on hosts with a locked-down
`%TEMP%`. That is a packaging-script change outside T-0262's owns; recorded as
the recommended follow-up rather than made here.
