# Signatures: integrity and identity

The **Sign** card produces a real CMS/PKCS#7 signature over the PDF byte range
and reports what it proves. The short version: **a valid signature proves the
signed bytes are unmodified; it does not prove who signed them.**

This page is the guarantee and limit statement. The workflow (import, options,
readout fields) is in [Signing and verification](../signing.md); the encryption
and permission context is in [Security](../security.md).

## What a valid result proves

A passing check establishes all of the following:

- the file has a signature dictionary and a four-integer `/ByteRange` starting
  at zero;
- `/Contents` decodes to a CMS `SignedData` with one signer using SHA-256 and RSA
  PKCS#1 v1.5;
- the RSA signature verifies over the signed attributes using the embedded
  signer certificate, and the certificate's issuer and serial match;
- the `messageDigest` equals SHA-256 over the declared byte ranges;
- the readout can report whether the ranges cover the whole file, how many bytes
  follow the signature, the subject, and the SHA-256 fingerprint of the signing
  certificate.

These properties are tested on real PDF bytes and independently checked with
.NET `SignedCms` / RSA / SHA-256
([T-0241 §5](../../../evidence/hardening/T-0241.md)).

## What it does not prove

- **Identity.** Certificate-chain trust is `None`: the PDF signature path
  consults no trust anchors. The readout therefore says **identity not
  verified**.
- **Revocation.** No CRL and no OCSP response is checked. A certificate that was
  revoked can still produce a valid signature here.
- **Trusted time.** `signingTime` is asserted by the signer. There is no RFC 3161
  timestamp token, so a backdated signature is not detectable.
- **That the certificate belongs to the person named in the document.** A
  self-signed certificate can be valid and still prove nothing about identity.
  The test identity is `CN=PDFXI Test Signer, O=PDFXI`.
- **A certified or locked document.** There is no `/Perms`/DocMDP certification,
  no field lock, and one signature per revision.
- **A Windows-store identity or HSM key.** The signing identity is the imported
  PKCS#12 bundle; there is no certificate-store picker.

"Identity not verified" is not an error state and it is not a warning that the
signature is bad. It is the honest result of a verification path that has no
trust anchor: integrity was checked, identity was not.

## The E-signed badge

The viewer draws a visible overlay block at the page bottom-right and shows a
green **`✓ E-signed`** chip only when:

```text
Valid && bytes_match && covers_entire_file && !appended
```

In words, the badge asserts:

- the signature is valid over its declared range;
- the digest over that range matches;
- the range covers the entire current file;
- no bytes were appended after the signed revision.

It does **not** assert that the signer is trusted, that the certificate chains to
a root, that the certificate was valid at signing time, or that the time in the
signature is trusted. It also does not turn the typed note into a portable
signature appearance.

The chip is cleared by edits, undo, redo, and switching tabs, because the
verification result no longer describes the current bytes. The readout is scoped
to its tab. Opening a document re-runs verification on its existing signatures.

## The PDF appearance is a typed note

The viewer's overlay block is a viewer drawing. The PDF itself carries a typed
`/Text` annotation as the signature appearance. Another reader may show it as a
sticky note, not as a signature block, and may not draw the `✓ E-signed` chip at
all. The chip is PDFXI's interpretation of its own verification result, not a
standard PDF signature appearance.

## Tamper behaviour

| Change | Result |
|---|---|
| A byte flipped inside the signed range | The digest no longer matches; the signature is not valid and the chip is not shown |
| Bytes appended after the signed revision | The original range may still verify, but `covers_entire_file` is false and `appended` is true, so the chip is not shown; the readout reports the appended bytes |
| `/Contents` corrupted or replaced | The CMS/RSA check fails; the signature is not valid |
| A different certificate embedded | The issuer/serial or signature check fails; the signature is not valid |
| A full rewrite (redaction, protect, optimize, sanitize) | The earlier bytes are replaced; signatures over them no longer verify |
| An incremental edit or form fill | The original range is preserved and still verifies; the new bytes are reported as appended content |

A signature is only as good as the bytes it covers. If the result matters, verify
in an independent reader as well.

## Limits

- **Encrypted documents are refused.** Signing needs a write path, and the
  session cannot re-encrypt the result. The same limit blocks saving an
  encrypted document.
- **No drawn or image signature placement.** The appearance is the typed note
  plus the viewer overlay; there is no freehand ink, signature image, or
  drag-to-place rectangle.
- **No request or tracker workflow.** PDFXI does not send a signature request,
  track recipients, or contact a signing service.
- **No revocation or RFC 3161 timestamp.** Both are future work; this build does
  not claim them.
- **No trust anchor configuration.** The chain result is always unverified in
  this build.
- **One signature per revision**, with no DocMDP certification and no field lock.
- **A self-signed test certificate proves integrity, not identity.** Do not treat
  a valid result as proof that a particular person signed.

## Related pages

[Signing and verification](../signing.md) · [Security](../security.md) ·
[What the assistant sends](assistant-data.md) · [Open, save, and print](../open-save-print.md)
