# T-0215: Word COM save/export hang (RESOLVED)

Status: **resolved** - Word COM now produces both DOCX and PDF on this host.
Affects (previously): `word-multipage.pdf`, `word-tagged-bookmarks.pdf`, and any
Word-based route.

## Original symptom

With `TEMP`/`TMP` redirected to a repository scratch directory, `SaveAs2` and
`ExportAsFixedFormat` never returned; WINWORD stayed alive with no window and no
output file after several minutes. PowerPoint COM worked, so it looked
Word-specific.

## What actually fixed it

The failing probe had redirected the process `TEMP`/`TMP` to
`target\tmp\realworld`. Running the same automation with the normal user
`%TEMP%` completes in seconds:

```powershell
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$word.DisplayAlerts = 0
$doc = $word.Documents.Add()
$doc.Content.Text = 'PDFXI Word COM probe'
$doc.SaveAs2('target\tmp\word-com-test.pdf', 17)   # wdFormatPDF
$doc.Close(0); $word.Quit()
```

Result:

```text
COM_OK exists=True size=77607
```

The produced PDF is a valid Word PDF and reads through both readers:

```text
pdfxi info  -> pdf_version 1.7, pages 1
pdfxi text  -> "PDFXI Word COM probe"
PDFium 8125 -> pages=1
```

The earlier hang was therefore an environment effect of the redirected temp
directory (the same class of host policy that made Inno Setup fail in
`T-0262-inno-installer.md`), not a Word or PDFXI defect.

## Consequences

- A Word-produced PDF route is available again. The committed end-to-end
  scenario (T-0260) still records the PowerPoint fixture because that is what it
  executed; the deviation is no longer forced.
- A-27's Office-open check is verified independently with the exported DOCX
  opened in Word (`docs/evidence/ui/final/word-opens-docx.png`).
- No test was skipped or weakened: the deterministic route was already
  accepted; this only removes the environmental restriction.
