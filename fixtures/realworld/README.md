# Real-world PDF fixtures

Unlike `fixtures/*.pdf`, the files in this directory were produced by
mainstream desktop software or by the project's own writer where no external
producer is available. They are committed so parser, text, form, and
document-lifecycle tests exercise structures the generated corpus does not
cover.

All content is original text written for this repository. No third-party
document was downloaded, and no personal data is present.

| File | Producer | Pages | Size (bytes) | SHA-256 |
| --- | --- | --- | --- | --- |
| `browser-print.pdf` | Microsoft Edge 158 headless | 1 | 51,696 | `7a837a076e32ac768538009dca96785f0c25a42a205bd2f898f4d330934cf948` |
| `microsoft-excel-2019-sample.pdf` | Microsoft Excel 2019 | 2 | 92,366 | `bb335a4ca5217eb32ed579cc64eb319055917bb9c5594838a63afeea2797f337` |
| `pdfxi-encrypted-selfmade.pdf` | self-produced (pdfxi) | 1 | 1,088 | `127e88f23535ac56bdc5e5d5232b8357ee540163c497e74e11b08a2cf6101878` |
| `pdfxi-forms-selfmade.pdf` | self-produced (pdfxi) | 1 | 1,195 | `31004820fab4fe929b24a1ba5ac49aea32e6677d9fde7c2ee1ea53e692190190` |
| `powerpoint-slides.pdf` | Microsoft PowerPoint 16.0 | 3 | 119,341 | `a11a98a2d3f9406520726dcff92cdfadb4def4b5c3b79624ffa7fcda224716ac` |

`manifest.toml` is the machine-readable source of truth for these values. The
smoke test `crates/pdfxi-testkit/tests/realworld_corpus.rs` verifies every
SHA-256, opens each file with `pdfxi_core::Document`, checks the page count, and
asserts the marker words (or the recorded current limitation).

## Regeneration

```powershell
pwsh -File fixtures/realworld/generate.ps1
# or, on Windows PowerShell 5.1:
powershell -ExecutionPolicy Bypass -File fixtures/realworld/generate.ps1
```

The script writes intermediates under `target/tmp/realworld`, runs Office and
Edge automation, invokes the `generate_realworld_selfmade` testkit example, and
copies the results here. It reads no network content. Sizes and hashes are
printed at the end; update this file and `manifest.toml` if they change.

## `powerpoint-slides.pdf`

- **Producer:** Microsoft PowerPoint 16.0 (Office 16.0), `Presentation.SaveAs`
  with `ppSaveAsPDF`.
- **Content:** three slides with title and body text. Original text only.
- **Page geometry:** 16:9 landscape, 720 x 405 pt (`/MediaBox [0 0 720 405]`),
  not US Letter portrait.
- **Pages:** 3.
- **Marker words:** `lantern slide marker`, `harbor slide marker`,
  `meadow slide marker`.
- **Why this file:** slides produce a different page size, font subset, and
  content-stream shape than Word or Excel exports, and exercise landscape
  navigation.

## `browser-print.pdf`

- **Producer:** Microsoft Edge 158.0.4370.33,
  `msedge.exe --headless --disable-gpu --no-pdf-header-footer
  --print-to-pdf=... file:///.../browser-print.html`.
- **Content:** a small hand-written HTML page. Original text only.
- **Pages:** 1, PDF 1.4.
- **Links:** two `/Subtype /Link` annotations. The external one targets
  `https://example.com`; the internal one targets the in-page anchor
  `#section-two`.
- **Marker words:** `kestrel browser`, `kestrel anchor`.
- **Current limitation:** `Document::page_text` succeeds but returns glyph
  indices from the Edge font subset instead of Unicode, so the marker words are
  absent. `manifest.toml` records this as `known_failure`; the smoke test
  asserts the markers are still missing and will flip when the ToUnicode CMap is
  applied. The file is kept unchanged.

## `pdfxi-forms-selfmade.pdf`

- **Producer:** self-produced (pdfxi). No installed external producer on the
  reference host can emit AcroForm fields (Word cannot, LibreOffice is absent),
  so this is the committed `fixtures/authoring/authoring-forms.pdf` fixture
  copied under its corpus name by
  `cargo run -p pdfxi-testkit --example generate_realworld_selfmade`.
- **Content:** one page with an `/AcroForm` containing a text field
  (`text-field`, value `typed value`), a checkbox (`checkbox-field`, on) and a
  choice field (`choice-field`, value `two`). Visible marker text: `Form page`.
- **Pages:** 1.
- **Marker words:** `Form page`.
- **Gap:** an externally produced AcroForm PDF is still missing and is listed as
  a corpus gap.

## `pdfxi-encrypted-selfmade.pdf`

- **Producer:** self-produced (pdfxi) by `pdfxi-security`'s encryption writer
  (`EncryptOptions` + `encrypt_document`), AES-128, security revision 4, with a
  fixed document-ID seed for byte-stable regeneration.
- **Open password:** `pdfxi-test` (documented public test value).
- **Owner password:** `pdfxi-owner`.
- **Content:** one page whose visible text is `Encrypted selfmade page marker`.
  The content stream is genuinely encrypted, not merely marked with `/Encrypt`.
- **Pages:** 1.
- **Current limitation:** `pdfxi-core` has no password or decryption layer. It
  opens the file and reports one page, but `Document::page_text(0)` returns
  `Error::UnexpectedToken`. `manifest.toml` records
  `expected_error = "UnexpectedToken"`; the smoke test asserts that typed error.
  The password is recorded here and in the manifest so, once an open-with-
  password API exists, the test can decrypt and assert the marker.
- **Gap:** an externally produced password-protected PDF (Word or LibreOffice)
  is still missing and is listed as a corpus gap.

## `microsoft-excel-2019-sample.pdf`

- **Producer:** Microsoft Excel 2019 (Office 16.0, COM build 19127),
  `Excel.Application` PDF export.
- **Content:** two worksheet pages whose text is original to this repository.
- **Redistribution:** the document contains only text written for this
  repository and a subset of the Calibri font embedded by Excel. It is
  redistributable under this repository's licence.
- **Size:** 92,366 bytes.
- **SHA-256:** `bb335a4ca5217eb32ed579cc64eb319055917bb9c5594838a63afeea2797f337`
- **Pages:** 2.
- **Marker words:** `PDFXI Real-World Sample`, `amber compass lantern`,
  `meadow harmonic`.

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

## Corpus gaps

The Word slots are blocked by a host environment issue, not by a PDFXI defect:
Word COM `SaveAs2` and `ExportAsFixedFormat` hang with a responsive WINWORD
process and no output. See `orchestration/blockers/T-0215-word-com.md` for the
exact repro and attempted fixes. The missing files are:

- `word-multipage.pdf` (3+ pages, headings, bullets, table, inline image).
- `word-tagged-bookmarks.pdf` (outline plus structure tree).
- an externally produced AcroForm PDF.
- an externally produced password-protected PDF.

Once Word is repaired or an interactive first run completes, add those files to
the generator, this README, and `manifest.toml`.
