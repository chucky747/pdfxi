# T-0215 blocker: Word COM save/export hangs

Status: open (environment, not a PDFXI defect).
Affects: `word-multipage.pdf`, `word-tagged-bookmarks.pdf`, and any Word-based
route to a password-protected PDF.

## Symptom

Microsoft Word 16.0 automation starts and edits documents, but every save or
export call blocks forever:

```powershell
$env:TEMP = '<repo>\target\tmp\realworld'; $env:TMP = $env:TEMP
$w = New-Object -ComObject Word.Application
$w.Visible = $false; $w.DisplayAlerts = 0
$d = $w.Documents.Add()
$d.Content.Text = 'probe'
$d.SaveAs2("$env:TEMP\probe.docx", 16)          # hangs
$d.ExportAsFixedFormat("$env:TEMP\probe.pdf", 17, $false, 1)  # hangs
```

`Documents.Add` and text insertion return normally. `SaveAs2` (DOCX and PDF)
and `ExportAsFixedFormat` never return: the WINWORD process stays responsive,
owns no top-level window, and no output file appears. A 60-second timeout is
not enough; the call is still pending after several minutes. PowerPoint COM
export on the same host works (a 3-slide PDF was produced in seconds), so this
is Word-specific.

## What was tried

- `Visible = $false` and `Visible = $true`; `DisplayAlerts = 0`.
- `SaveAs2` to `%TEMP%` on both the repository drive and the system drive.
- `ExportAsFixedFormat` with 2, 4, and 15 parameters.
- Resetting `%APPDATA%\Microsoft\Templates\Normal.dotm` (renamed aside).
- Disabling the Dropbox and Acrobat PDFMaker COM add-ins (`COMAddIns` and
  `HKCU\Software\Microsoft\Office\Word\Addins`).
- Changing the default printer from the Brother MFC-L2750DW to
  "Microsoft Print to PDF", and setting `Application.ActivePrinter`
  explicitly. `Options.BackgroundSave = $false`.

None of these changed the hang. The host was restored afterwards (add-in
`LoadBehavior`, `Normal.dotm`, and default printer).

## Conclusion

The Word installation on the reference host needs an interactive first run
(complete any activation/privacy dialog and save one document manually) or an
Office repair. Until then Word-based fixtures cannot be produced by this
automation session.

## Corpus gap

`powerpoint-slides.pdf`, `browser-print.pdf`, `pdfxi-forms-selfmade.pdf`, and
`pdfxi-encrypted-selfmade.pdf` are committed. The following remain gaps:

- `word-multipage.pdf` (3+ pages, headings, bullets, table, image)
- `word-tagged-bookmarks.pdf` (outline and structure tree)
- an externally produced AcroForm PDF and an externally produced
  password-protected PDF (LibreOffice is not installed)
