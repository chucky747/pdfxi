# Real-world PDF fixtures

Unlike `fixtures/*.pdf`, the files in this directory were produced by mainstream
desktop software rather than by `pdfxi-testkit`. They are committed so parser
and document-lifecycle tests exercise structures the generated corpus does not
cover.

## `microsoft-excel-2019-sample.pdf`

- **Producer:** Microsoft Excel 2019 (Office 16.0, COM build 19127),
  `Excel.Application` PDF export.
- **Content:** two worksheet pages whose text is original to this repository.
  The workbook was created locally for this fixture; no third-party document
  was downloaded or copied.
- **Redistribution:** the document contains only text written for this
  repository and a subset of the Calibri font embedded by Excel. It is
  redistributable under this repository's licence.
- **Size:** 92,366 bytes.
- **SHA-256:** `bb335a4ca5217eb32ed579cc64eb319055917bb9c5594838a63afeea2797f337`

### Why this file

It covers features the generated fixtures do not have:

- FlateDecode-compressed content streams (`/Filter/FlateDecode`).
- A cross-reference stream (`/Type/XRef`) reached through a hybrid
  `/XRefStm` trailer.
- An object stream (`/Type/ObjStm`).
- An embedded subset TrueType font (`/Subtype/TrueType`,
  `/BaseFont/BCDEEE+Calibri`, `/FontFile2`).

### How it was produced

The following PowerShell command is the complete generator. It writes only to
the system temp directory; the committed file was copied from there.

```powershell
$out = Join-Path $env:TEMP 'pdfxi-realworld-excel.pdf'
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Add()
while ($wb.Worksheets.Count -lt 2) { $wb.Worksheets.Add() | Out-Null }
$s1 = $wb.Worksheets.Item(1)
$s1.Name = 'Overview'
$s1.Cells.Item(1, 1) = 'PDFXI Real-World Sample'
$s1.Cells.Item(2, 1) = 'The orchestra rehearsed the quarantine protocol beside the lighthouse while the cinnamon tea cooled.'
$s1.Cells.Item(3, 1) = 'Verification sentence: amber compass lantern.'
$s2 = $wb.Worksheets.Item(2)
$s2.Name = 'Details'
$s2.Cells.Item(1, 1) = 'Second page marker: meadow harmonic.'
$s2.Cells.Item(2, 1) = 'This worksheet exists so the sample has two pages and a real page tree.'
$wb.ExportAsFixedFormat(0, $out)
$wb.Close($false)
$xl.Quit()
```
### Verification

```text
cargo run -p pdfxi-cli -- text fixtures/realworld/microsoft-excel-2019-sample.pdf
PDFXI Real-World SampleThe orchestra rehearsed the quarantine protocol beside the lighthouse while the cinnamon tea cooled.Verification sentence: amber compass lantern.
Second page marker: meadow harmonic.This worksheet exists so the sample has two pages and a real page tree.
```

`crates/pdfxi-core/tests/realworld.rs` asserts the marker words, the two-page
count, and the current incremental-save behaviour. The source uses
cross-reference streams, so `Document::save_incremental()` returns
`Error::UnsupportedState("cross-reference streams are not supported")` today;
the test keeps that limitation honest instead of weakening the assertions.
