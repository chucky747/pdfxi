# Security

Security lives in `pdfxi-security`, with content-stream removal in
`pdfxi-edit::redaction`. The cryptographic primitives and several handlers are
verified against published vectors; document-open integration, a full UI, and
trusted timestamping are still tracked as in-progress rows. No secret is logged
and no code uses `unsafe`.

## Passwords and AES-256

- `SEC-001` (in-progress): the standard security handler covers PDF revisions 2
  through 6 — Algorithm 2 MD5 key derivation, Algorithms 3–7 `/O` and `/U`
  computation and owner recovery, revision 5 SHA-256, and revision 6 Algorithm
  2.B hardened hashing. User then owner passwords are tried with constant-time
  comparison; owner role bypasses permission bits. Per-object keys use RC4
  `min(n+5,16)`, AESV2 with `sALt`, and AESV3 file-key rules. Opening an encrypted
  document end-to-end is the remaining work.
- `SEC-002` (verified): dependency-free FIPS 197 AES-128/192/256 with CBC and
  PKCS#7, RC4 40/128, MD5, and SHA-256/384/512 are checked against FIPS and NIST
  vectors. Complete AES-256 (AESV3) object encryption and decryption for
  revisions 5 and 6 includes the hardened hash, consistent `/O`, `/U`, `/OE`,
  `/UE`, and `/Perms` values, per-stream IVs, and `EncryptMetadata` handling.
  Regression tests include an independent .NET AES-256-CBC vector and an
  independent ISO 32000-2 revision 6 known-answer vector. Evidence is in
  `../../evidence/security/T-0061.md`.

`EncryptionMethod` in `pdfxi-security::encryption` names none, RC4-40/128, and
AES-128/256.

## CMS signatures

`SEC-005` (verified): `build_signed_data` and `verify_signed_data` implement a
deterministic CMS/PKCS#7 SignedData subset with `encapContentInfo`,
`signerInfos`, and signed attributes (`contentType`, `messageDigest`,
`signingTime`). A `SignatureProvider` trait keeps keys and certificates out of
the crate. Verification distinguishes tampered content, tampered signed
attributes, bad signatures, truncation, and unsupported algorithms; detached
verification accepts supplied content. The strict DER/ASN.1 subset lives in
`pdfxi-security::der`.

`SEC-006` (in-progress): the CMS validation subset is in place, but RFC 3161
timestamps and trust/revocation management are not implemented. A `signingTime`
attribute is encoded and decoded; no trusted timestamp token is requested.
`SEC-003` (certificate encryption) remains planned.

## X.509 digital IDs

`SEC-004` (in-progress): `pdfxi-security::x509` provides strict DER certificate
parsing, deterministic name display, the four common v3 extensions,
caller-clocked validity, and caller-trusted chain verification through a
`CertificateVerifier` with a `TrustStore`. A `DigitalIdStore` keeps private keys
behind an opaque `KeyHandle` and never logs secrets. Twenty-one tests cover
parsing, malformed DER, time boundaries, trust decisions, key usage, SAN/EKU,
duplicate IDs, opaque handles, and negative signature cases. PKCS#12/PFX import
and a desktop management UI are follow-on work. Evidence is in
`../../evidence/security/T-0070.md`.

## Redaction

`EDIT-013` (in-progress): `pdfxi-security::redaction` validates rectangle and
literal-text plans (finite, non-empty, in page bounds), orders them canonically,
and verifies removal with strict or permissive residual detection that reports
page plus byte offsets.

Actual content rewriting is in `pdfxi-edit::redaction`: matching
`Tj`/`TJ`/`'`/`"` operators are removed, Form XObjects are recursed into,
intersecting annotations are stripped, an opaque rectangle is painted, and text
is re-extracted to verify that no residual remains. Redaction removes content
rather than only covering it, per RULES.md section 8. Raster image removal from
redacted areas is owned by later edit/render work.

## Sanitization

- `SEC-008` (in-progress): category-based plans for metadata, embedded files,
  annotations, JavaScript, and form values, with deterministic ordered actions
  and strict/permissive verification against a caller-supplied inventory.
- `SEC-009` (in-progress): Info dictionary and XMP scrubbing are modelled as two
  deterministic `Remove` actions. Real stream deletion in the core writer is not
  wired yet.

## Related pages

[Getting started](getting-started.md) · [Forms](forms.md) ·
[Standards and preflight](standards.md) · [Feature matrix](../../FEATURES.md)
