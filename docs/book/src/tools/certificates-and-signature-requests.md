# Certificates and signature requests

This tool has two independent halves: a **certificate signature** that proves
byte integrity, and a **signature request** that composes an email in your own
mail client and tracks replies locally. They share the Sign card but are not the
same operation ([T-0245](../../../evidence/ui/T-0245.md)).

## Certificate signing

Open **Tools - Sign** with a document open. The card requires an active, writable
tab; encrypted documents are refused because the session cannot re-encrypt.

1. **Identity** - choose a PKCS#12 bundle (`.pfx` or `.p12`), enter its password,
   and press **Load identity**. The key must be RSA with at least 2048 bits. The
   password is cleared after a successful import and is never written to settings
   or logs; the private key stays in memory.
2. **Options** - reason, location, page, and appearance text. Blank reason and
   location fall back to `Digitally signed` and `PDFXI`.
3. **Sign** - appends the visible `/Text` note, then the CMS/PKCS#7 signature as
   two incremental revisions. The note is inside the signed byte range.

The **E-signed** chip appears only when `Valid && bytes_match &&
covers_entire_file && !appended`. That is an integrity-and-coverage claim, not an
identity claim: chain trust is `None` and the readout says **identity not
verified**. See [Signatures: integrity and identity](../security/signatures.md).

## Verification on open

Opening a signed document verifies its signature once and shows the status,
signer, SHA-256 fingerprint, asserted time, whole-file coverage, appended bytes,
and chain. Unsigned documents show nothing. Any edit, undo, or redo clears the
result until the bytes are checked again, and the readout is scoped to its tab.

## Requesting a signature

The lower half of the card composes a **mailto:** request:

- **Recipient email**, optional **Subject** and **Message**; blank values get a
  default subject and body.
- **Send request** is enabled once the recipient contains `@`. PDFXI hands a
  `mailto:` URL to the platform mail client - it sends no SMTP or API request,
  attaches no file, and never contacts a service.
- A pending row is recorded only after the mail client launches successfully.

**Tracked requests** lists each request as `<recipient> - <document> -
<pending|signed>` in a local TSV file (`sign-requests.tsv` in the platform config
directory). When you later open a returned copy whose signature verifies, a
matching document path or signer-subject substring marks the row **signed**.
A matched row means "matched", not "trusted".

## Limits

- No delivery receipt, read receipt, or recipient identity check.
- A request needs a saved document; an unsaved document is refused.
- The tracker has no expiry, deduplication, delete action, or cross-machine sync.
- No trust anchor, revocation, or RFC 3161 timestamp for the certificate
  ([T-0249](../../../evidence/hardening/T-0249.md), T-0267 in progress).

[Signing workflow](../signing.md) · [Security](../security.md) ·
[Evidence: T-0245](../../../evidence/ui/T-0245.md)
