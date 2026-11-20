# Package check evidence (AC-10)

Command: `powershell -NoProfile -File scripts/package.ps1 -Check`
Date: 2026-11-19

```text
SOURCE ok=True layout=True notices=True secret_free=True hashes_verified=0
CHECK binary=present D:\Documents\opencode-work\target\lanes\qa\release\pdfxi.exe
STAGING absent (no package built yet)
MANIFEST absent (no package built yet)
CHECK layout_and_notices=PASS
PACKAGE_CHECK_EXIT=0
```

The layout, notice set (`LICENSE`, `NOTICE`, `THIRD_PARTY.md`, `deny.toml`), and
secret/private-file scan pass. No Adobe asset and no PDFium binary is staged;
PDFium is referenced by notice only. A full archive build is recorded in
`docs/evidence/package.md` when the release binary is produced.
