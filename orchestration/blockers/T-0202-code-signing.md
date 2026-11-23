# Blocker: code signing (T-0202)

Status: open. No certificate exists in this workspace, so nothing built by this
campaign is signed. No task, document, or report may claim otherwise.

## What is unsigned

- `pdfxi-ui.exe`, `pdfxi.exe`, and `pdfium.dll` inside
  `pdfxi-portable-<version>-windows-x86_64.zip`.
- `pdfxi-setup-<version>-windows-x86_64.exe` (Inno Setup),
  when it is built.
- Every `target/lanes/*` and `target/integration` build used as evidence.

## User-visible effect

Windows SmartScreen and Defender can warn on first run ("Windows protected your
PC"), and the publisher reads as unknown in the UAC/properties dialogs. The
per-user installer requests no elevation (`PrivilegesRequired=lowest`), so no
UAC prompt is raised, but SmartScreen still applies to the downloaded setup
executable.

## What is needed

1. A code-signing certificate: an OV/EV code-signing cert from a public CA, or
   an organisation-internal certificate plus a published trust anchor. EV also
   needs a hardware token or cloud HSM.
2. The Windows SDK `signtool.exe` (or an equivalent) on the build machine.
3. The certificate thumbprint or a PFX path supplied through the environment,
   never committed: for example `PDFXI_SIGN_THUMBPRINT` or
   `PDFXI_SIGN_PFX` + `PDFXI_SIGN_PFX_PASSWORD`.

## Where signing hooks in

`scripts/package.ps1`, in this order, after each artefact exists and before its
`.sha256` sidecar is written:

1. `signtool sign /fd SHA256 /tr <timestamp-url> /td SHA256 /sha1 $env:PDFXI_SIGN_THUMBPRINT <exe>`
   for `pdfxi.exe`, `pdfxi-ui.exe`, and the setup executable.
2. `signtool verify /pa /all <exe>` for each one; fail the package on a
   verification error.
3. Only then write the `.sha256` sidecars, so the sidecar covers the signed
   bytes. Note that signing changes the file hash, so any manifest hash of a
   binary must be computed after signing.

`pdfium.dll` is third-party; do not sign it. Ship it unmodified with its hash.

## Until the blocker is cleared

- The release notes must state that the binaries and installer are unsigned.
- SmartScreen warnings are expected and must not be described as a bug.
- Do not add `signtool` calls that no-op silently: a missing certificate must be
  a recorded blocker, not a green check.
## Accepted for 0.1.3 (2026-11-23)

The 0.1.3 build and installer are unsigned and remain so. Release notes and the
installer's final report must state that Windows SmartScreen may warn and that
the publisher is unknown. This blocker is not a reason to withhold the build;
it is a reason not to claim a signed or trusted publisher.