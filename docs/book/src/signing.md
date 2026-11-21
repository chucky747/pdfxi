# Signing and verification

The **Signing** surface applies a certificate signature to the open PDF and
reports what the signature proves. Open it from **Tools → Sign**; the command bar
also reaches it. It is separate from **Fill & Sign**: signing is a cryptographic
operation, not a form fill.

> **Integrity is not identity.** A valid signature proves that the signed bytes
> have not changed since they were signed. It does not prove who the signer is
> unless a trust anchor you chose says so. This build configures **no trust
> anchor**, so the chain result is reported as "identity not verified".

The underlying CMS/PKCS#7 implementation and its evidence are described in
[Security](security.md) and [T-0241](../../evidence/hardening/T-0241.md).

## Importing a certificate

1. Choose a PKCS#12 file: `.pfx` or `.p12`.
2. Enter the file's password in the masked field.
3. PDFXI imports the signing key and certificate.

The password field is masked and is **cleared after a successful import**. The
password is not written to settings or logs.

The signing key must be **RSA with at least 2048 bits**. A smaller key is
rejected rather than silently used.

## Signing

| Option | Meaning |
|---|---|
| Reason | Optional text recorded with the signature |
| Location | Optional text recorded with the signature |
| Page | Optional page for the signature's appearance |
| Appearance | A **typed note placed bottom-right**, plus a visible viewer overlay block |

The signature is written as an **incremental update**: a new revision is
appended to the PDF. The existing bytes are not rewritten, so earlier content and
earlier signatures are not destroyed by this operation.

## What appears on the page

- The viewer draws a **visible overlay block** at the page bottom-right. It is a
  viewer drawing, not part of the PDF.
- The PDF itself carries a **typed `/Text` annotation** as the signature
  appearance. Another reader may show that object as a sticky note rather than as
  a signature block.
- When the badge predicate below holds, the viewer shows a green
  **`✓ E-signed`** chip. The chip is PDFXI's own interpretation of its
  verification result; it is not a standard PDF appearance and another reader
  will not draw it.

## Reading the verification result

The Sign card shows:

| Field | Meaning |
|---|---|
| Status | Whether the signature is valid over the byte range |
| Signer | The certificate subject/common name |
| SHA-256 fingerprint | Digest of the signing certificate |
| Asserted time | The signing time recorded in the signature |
| Whole-file coverage | Whether the signature covers the entire file, not only earlier bytes |
| Appended after signature | Whether bytes were added after the signed revision |
| Chain | Trust result; with no trust anchor configured it reads **identity not verified** |

The **E-signed** badge is shown only when **all** of these are true:

```text
Valid && bytes_match && covers_entire_file && !appended
```

The badge is a statement about integrity and coverage, not about identity. Edit
the document, undo, redo, or switch tabs and the badge is cleared, because the
result no longer describes the current bytes. The readout is scoped to its tab,
and opening a document re-runs verification on its existing signatures. The
guarantees and tamper behaviour are in
[Signatures: integrity and identity](security/signatures.md).

## Limits

- **No drawn or image signature placement.** The appearance is a typed note in
  the bottom-right of the chosen page plus the viewer overlay. There is no
  freehand ink, no signature image, and no drag-to-place rectangle. The PDF
  object is a `/Text` note, so other readers may render it as a note.
- **No request or tracker workflow.** PDFXI does not send a signature request,
  does not track recipients, and does not contact a signing service.
- **No trust anchor.** Chain building has no configured root, so the readout
  cannot establish identity; it says so.
- **No revocation checking and no RFC 3161 timestamp authority.** A signature's
  asserted time is what the signer's software recorded, not a trusted timestamp.
- **Encrypted documents are refused.** A document that needs re-encryption cannot
  be signed in this build; the same session limit that blocks saving encrypted
  documents applies.
- A signature covers the bytes it was created over. Adding annotations or form
  values afterwards is an incremental change and is reported as appended content.
- Verification is only as good as the certificate's own claims. A self-signed
  certificate can produce a valid signature and still prove nothing about
  identity.

## Related pages

[Security](security.md) · [Signatures: integrity and identity](security/signatures.md) ·
[Fill & Sign](fill-and-sign.md) · [Privacy and local data](privacy.md) ·
[Open, save, and print](open-save-print.md)
