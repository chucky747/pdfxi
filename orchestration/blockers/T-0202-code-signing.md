# Blocker: code signing (T-0202)

Status: **local self-signed signing is implemented (2026-11-24)**. Public-CA
trust is still open: no OV/EV certificate exists, so other machines will not
trust the publisher and SmartScreen may still warn there. The 0.1.4 setup and
payload remain unsigned; 0.1.5 is the first release built with the local
certificate.

## Local self-signed signing

`scripts/new-signing-cert.ps1` creates a Code Signing certificate in
`Cert:\CurrentUser\My` with an exportable key. `scripts/trust-signing-cert.ps1`
adds its public part to the current user's Root and TrustedPublisher stores
through the .NET store API, without a UI prompt. Set
`PDFXI_SIGN_THUMBPRINT` and run `scripts/package.ps1`:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/new-signing-cert.ps1
$env:PDFXI_SIGN_THUMBPRINT = '<thumbprint>'
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/trust-signing-cert.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1
```

The package signs `pdfxi.exe` and `pdfxi-ui.exe` before staging, so the manifest
and zip contain the signed bytes, then signs the Inno Setup executable before
writing its `.sha256` sidecar. Each signature is checked with
`Get-AuthenticodeSignature`; a non-`Valid` status fails the package.
`PDFXI_SIGN_TIMESTAMP_URL` overrides the default RFC 3161 URL (DigiCert);
set it to an empty string for an offline build without a timestamp. With no
thumbprint the build stays unsigned and prints
`SIGNING skipped: PDFXI_SIGN_THUMBPRINT not set`.

Scope of the self-signed certificate:

- It proves the bytes were signed by this key and detects tampering.
- It is `Valid` on this machine after the trust step.
- It is **not** a publicly trusted publisher. Machines without the certificate
  see an untrusted publisher, and SmartScreen reputation is not granted.
- A public OV/EV certificate (or an organisation certificate plus a published
  trust anchor) is still required for distribution to other users.


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