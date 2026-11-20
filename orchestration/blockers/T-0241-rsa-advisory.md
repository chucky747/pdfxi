# T-0241 blocker - RUSTSEC-2023-0071 in `rsa 0.9.10`

Date: 2026-11-20. Owner: integrator / lane-security. Status: open, needs an
owner decision.

## Symptom

`cargo deny check` on the library-wave tree (T-0230/T-0234/T-0239/T-0241
applied) fails on advisories:

```text
error[vulnerability]: Marvin Attack: potential key recovery through timing sidechannels
   ┌─ rsa 0.9.10
   ├ ID: RUSTSEC-2023-0071
   ├ Advisory: https://rustsec.org/advisories/RUSTSEC-2023-0071
advisories FAILED, bans ok, licenses ok, sources ok
EXIT=1
```

The advisory is new in this wave: `pdfxi-security` now depends on `rsa 0.9.10`
for the real CMS signing path in `src/pdf_signature.rs` (T-0241). Before this
wave `cargo deny check` reported `advisories ok`.

## Impact

The advisory describes a timing side channel in RSA decryption. PDFXI uses the
crate for signing with a caller-supplied private key, not for decryption, so the
exact attack surface is narrower than the advisory's, but the dependency is real
and the gate is red. RULES.md section 8 requires `cargo deny` in CI, so the
workspace cannot be considered green while this stands.

## Attempts

1. No alternative was substituted in T-0241: the `cms` crate's RSA binding and
   the raw `rsa` crate are the two available Rust paths, and rewriting the
   PKCS#1 v1.5 signature to another provider would change the accepted design
   mid-task.
2. `deny.toml` `[advisories] ignore` is still `[]`; adding an ignore is a
   security decision, not an integrator edit.

## Options for the owner

1. Upgrade or replace: track `rsa` releases for a fixed version, or move
   signing to a backend that does not carry the advisory (ring/OpenSSL/CNG) —
   `pdfxi-security` already has CNG-era certificate work in `trust_manager`.
2. Documented ignore in `deny.toml` with the signing-only rationale and a
   dated re-review, accepting the residual risk.
3. Restrict the dependency to the signing feature and keep default builds
   advisory-clean.

## Next step

Owner decision before the signing shell task T-0245 is opened, because T-0245
ships the first user-facing signature path.
