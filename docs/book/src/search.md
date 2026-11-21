# Search

Search runs over the extracted text of the **open document**. It does not search
the library, other tabs, or placeholder text.

## Options

| Option | Effect |
|---|---|
| Match case | Makes the search case-sensitive; off by default |
| Whole word | Matches only whole words instead of substrings |

## Moving through matches

- **Next** and **Previous** move through matches in document order.
- <kbd>F3</kbd> repeats Next; <kbd>Shift</kbd>+<kbd>F3</kbd> repeats Previous.
- The canvas scrolls to the match and draws per-span highlight boxes over the
  matched text.

The result list is capped at **10,000** matches. When the cap is reached, the
count is shown with a **+** to say that more matches exist; the list is not
silently truncated to look complete.

## Progressive extraction

Text extraction is progressive for large files:

- the first 8 pages are indexed when the document opens;
- two more pages are indexed per frame after that.

A match on a page that has not been indexed yet is found once that page is
reached. Fully searching a very large document therefore takes time as indexing
catches up. This is the current behaviour, not a setting.

Extraction is text only. A scanned page with no text layer produces no matches;
OCR is not part of the viewer's search path in this build.

## Limits

- No regular expressions, wildcards, or proximity operators.
- No replace, and no search-and-redact. Redaction is a separate, unavailable
  command in this build; see [Security](security.md).
- No cross-document or folder-wide search.
- No persistent search history.
- Because indexing is progressive, a match may appear after you start searching
  rather than at the instant you press Enter.

## Related pages

[Pages panel](pages-panel.md) · [Comments](comments.md) ·
[Desktop viewer](gui.md)
