# Product shell evidence

Final visual evidence for the modern Acrobat-style shell:

- `acceptance-b.png` — the recovered acceptance target (image 2), light and dark.
- `light-v16.png` — final light theme, 1920x1200 physical.
- `dark-v16.png` — final dark theme, same viewport.

The review checklist, findings log, and sign-off are in
[`docs/design/UI_REVIEW.md`](../../design/UI_REVIEW.md).

## Capture

```powershell
cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
powershell -NoProfile -File scripts/capture-ui.ps1 `
    -Out docs/evidence/ui/light-v16.png `
    -Exe target/integration/release/pdfxi-ui.exe -Arguments '--light'
powershell -NoProfile -File scripts/capture-ui.ps1 `
    -Out docs/evidence/ui/dark-v16.png `
    -Exe target/integration/release/pdfxi-ui.exe -Arguments '--dark'
```

`scripts/capture-ui.ps1` uses `PrintWindow` with DPI awareness so the capture
is the viewer surface, not the desktop behind it. Earlier review rounds
(v1-v15) were captured during the findings log and are not kept as final
evidence.
