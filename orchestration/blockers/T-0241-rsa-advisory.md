# T-0241 blocker - RUSTSEC-2023-0071 in `rsa 0.9.10`

Date: 2026-11-20. Owner: orchestrator decision, recorded by the integrator.
Status: **mitigated (scoped ignore)**.

## Symptom

`cargo deny check` on the library-wave tree originally failed on advisories:

```text
error[vulnerability]: Marvin Attack: potential key recovery through timing sidechannels
   ┌─ rsa 0.9.10
   ├ ID: RUSTSEC-2023-0071
   ├ Advisory: https://rustsec.org/advisories/RUSTSEC-2023-0071
advisories FAILED, bans ok, licenses ok, sources ok
EXIT=1
```

The advisory arrived with T-0241: `pdfxi-security` now depends on `rsa 0.9.10`
for the real CMS signing path in `src/pdf_signature.rs`.

## Decision (orchestrator, 2026-11-20)

Scoped ignore, documented rather than silent: `deny.toml` now carries
`ignore = ["RUSTSEC-2023-0071"]` under a comment block naming the scope, the
reason, the guard test and this file.

Rationale: RUSTSEC-2023-0071 (Marvin) is a timing side channel in PKCS#1 v1.5
**decryption** padding checks. PDFXI does not decrypt RSA ciphertext. The crate
uses only:

- `RsaPrivateKey`, `SigningKey<Sha256>`, `Pkcs1v15Sign`,
- `RsaPrivateKey::from_pkcs8_pem` / `from_pkcs1_pem` (private-key file decoding),
- `RsaPublicKey::verify(Pkcs1v15Sign::new::<Sha256>(), ...)`.

No `DecryptingKey`, no `Pkcs1v15Encrypt`, no `Oaep`, no decryption call.

## Grep evidence

```text
pdfxi-security src: only SigningKey/Pkcs1v15Sign/RsaPrivateKey::from_*_pem/RsaPublicKey::verify; no decrypt APIs
```

The only `.decrypt(` occurrences in the crate are `protector.decrypt(...)` in
`standard_handler.rs` and its tests, the symmetric AES/RC4 document-encryption
path, not RSA.

## Guard

`crates/pdfxi-security/tests/rsa_usage_scope.rs`:

```text
cargo test -p pdfxi-security --test rsa_usage_scope
test rsa_is_used_for_signing_and_verification_only ... ok
test result: ok. 1 passed; 0 failed
```

It scans every `src/**/*.rs` file for `DecryptingKey`, `Pkcs1v15Encrypt`,
`Oaep` and `rsa::RsaPrivateKey::decrypt` (line comments ignored, `tests/`
outside the scan) and fails with a message pointing at this file if one appears.

## Verification

```text
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
EXIT=0
```

## Remaining risk and revisit trigger

- The ignore is only valid while no RSA decryption path exists. If PDFXI ever
  adds RSA decryption (for example PKCS#1 v1.5 encrypted documents, or a
  compatibility shim that decrypts attacker bytes), the guard fails and the
  ignore must be removed before that code lands.
- Track the advisory: when a fixed `rsa` release ships, upgrade and delete the
  ignore and the guard's rationale, or keep the guard as a permanent scope
  check.
- If signing moves to another backend (ring/CNG/OpenSSL), remove the `rsa`
  dependency, the ignore and the blocker together.
