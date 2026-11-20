# Blocker - chrono is unmaintained (RUSTSEC-2026-0543)

Date: 2026-11-20. Owner: integrator / orchestrator. Status: **resolved**.

## Symptom

`cargo deny check` on the T-0256 tree originally reported:

```text
error[unmaintained]: chrono is unmaintained
   ┌─ chrono 0.4.45 registry+https://github.com/rust-lang/crates.io-index
   ├ ID: RUSTSEC-2026-0543
   ├ Advisory: https://rustsec.org/advisories/RUSTSEC-2026-0543
advisories FAILED, bans ok, licenses ok, sources ok
EXIT=1
```

It was not caused by T-0256: `chrono` entered through the root workspace
dependency table. Earlier runs passed because the advisory database had not yet
picked up RUSTSEC-2026-0543.

## Resolution (2026-11-20)

Facts verified on the current tree:

- `crates/pdfxi-app/Cargo.toml` no longer depends on `chrono`; the app lane
  replaced it with `time`.
- `cargo deny check` exits 0 in a clean shell (`cmd /c`): `advisories ok, bans
  ok, licenses ok, sources ok`.
- `cargo tree -i chrono` still shows `chrono 0.4.45` as a transitive dependency
  of `pdfium-render 0.9.5`.
- RUSTSEC-2026-0543 is not currently reported by the advisory database, and
  `deny.toml` has no chrono ignore (none was added).

No direct dependency on chrono remains, so no migration was needed beyond the
app lane's move to `time`.

## Residual risk

chrono remains transitively through `pdfium-render`, which has no replacement
backend in the workspace: PDFium is the renderer. If the advisory reappears, the
fallback is a scoped, documented `[advisories]` ignore for that crate path or a
forked/patched pdfium-render backend — **not** a direct-dependency migration,
because there is no direct dependency left to migrate. Re-check at final
packaging (T-0262).

## Next step

Closed. T-0262's notes carry the re-check instruction at packaging time.
