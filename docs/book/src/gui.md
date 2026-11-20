# Desktop viewer

`pdfxi-ui` is the native viewer. It is an `egui`/`eframe` window over the
`pdfxi-app` state model. Every control in the current shell either acts on a
real document or changes a real setting; controls that had no implementation
were removed in T-0203 rather than left painted. This page describes the viewer
as shipped in `0.1.0`.

To install or build it, see [Installing PDFXI](install.md).

## Starting the viewer

The viewer opens on an empty **Home** screen: the PDFXI mark, an **Open a PDF**
button, a drop hint, and the `Ctrl+O` shortcut. No document is loaded until you
open one. There is no bundled sample document.

## Opening a PDF

- **Open a PDF**, the **+** button in the tab bar, or `Ctrl+O` opens the native
  Windows file picker. Selecting several files opens them as separate tabs.
- Drag one or more PDFs from Explorer onto the window. Non-PDFs are ignored.
- Launch `pdfxi-ui.exe <path>` to open that document. This is what the Start
  Menu shortcut and "Open with" pass.
- Tabs can be activated and closed. Closing the last tab returns to Home.

A file that cannot be opened produces a dismissible error banner instead of a
blank canvas. The same banner covers a corrupt file, a page whose size cannot
be read, and a document that exceeds the input limit.

Documents larger than 2 GiB are rejected with an error; the viewer does not
read them partially.

Password-protected PDFs open through a masked password modal. A wrong password
keeps the modal open with a typed message, Cancel opens no tab and explains why,
and a document whose permissions deny modification opens read-only. The viewer
does not re-encrypt on save, so saving an encrypted document is refused with a
typed message and the tab stays open — see [Security](security.md).

## Navigating a document

- The thumbnail panel shows real page thumbnails. Selecting one navigates to
  that page.
- The canvas is continuous: scroll to move through the document.
- The floating page/zoom pill shows the current page and the page count.
- Page rasters are produced by PDFium at the current zoom and display scale, so
  text stays crisp at non-default Windows scaling.

## Zoom and rotate

The command bar and the page pill cover:

- zoom in and zoom out;
- fit page, fit width, and actual size;
- rotate the view clockwise.

Rotation changes the view only. The viewer does not rewrite the file.

## Find

Find searches the extracted text of the open document and highlights matches
with per-span boxes on the page. It does not search demo or placeholder text.

Text extraction is progressive for large files: the first 8 pages are indexed
when the document opens, then two more pages are indexed per frame. A match on
a page that has not been indexed yet is found once that page is reached; fully
searching a very large document therefore takes time. This is the current
limitation, not a setting.

## Theme and window controls

The gear menu in the title bar has two settings:

- **Theme**: Light, Dark, or Follow system. Follow system uses the light/dark
  preference reported by the operating system and falls back to Light when
  nothing is reported.
- **Window controls**: Windows or macOS. The Windows style draws minimize,
  maximize/restore, and close at the right edge; the macOS style keeps the
  traffic lights at the left.

Changes apply immediately. Double-clicking the title-bar drag area toggles
maximize.

The caption buttons are painted by `egui`, not by the operating system. Windows
11 Snap Layouts (the flyout on maximize hover) is therefore not available, and
touch or pen hit-testing follows `egui` rather than the native window frame.

## Settings file and launch flags

The viewer stores only explicit user choices in a TOML file:

- Windows: `%APPDATA%\PDFXI\settings.toml`
- Linux: `$XDG_CONFIG_HOME/PDFXI/settings.toml`
  (or `~/.config/PDFXI/settings.toml`)
- macOS: `~/Library/Application Support/PDFXI/settings.toml`

The file holds explicit user choices only:

```toml
theme = "system"             # "light", "dark", or "system"
window_controls = "windows"  # "windows" or "macos"
display_name = "Reader"      # local author name; never the OS account by default
# library_root = "~/Documents/PDFXI"
# shared_folder = "~/Shared/PDFs"
```

`display_name` is the local author shown on comments and signatures; a fresh
install starts as the neutral `Reader`, not your OS account name. Paths expand
`~`, `%VAR%`, `$VAR` and `${VAR}` on load and relative paths are rejected. The
recents and stars live in `%APPDATA%\PDFXI\library.toml`; see
[Privacy and local data](privacy.md) for the full list of local files.

A missing, unreadable, or corrupt file falls back to the defaults (Follow
system, and the platform's control style). Unknown keys are ignored. Writes are
atomic: the new text goes to a temporary file in the same directory and is
renamed over the old one.

These launch flags override the saved settings for one run and are **not**
written back:

```text
pdfxi-ui.exe --light
pdfxi-ui.exe --dark
pdfxi-ui.exe --controls=windows
pdfxi-ui.exe --controls=macos
```

## When something fails

Runtime problems that can be shown in the window use the dismissible error
banner. If the viewer fails before its window exists, the GUI build writes the
message to `%LOCALAPPDATA%\PDFXI\logs\pdfxi-ui.log` and shows a native message
box. Panics take the same path.

Builds produced by this project are unsigned. Windows SmartScreen may warn on
first launch; see [Installing PDFXI](install.md).

## Known limitations

- Find coverage on very large documents is progressive, as described above.
- Saving an encrypted document is refused: the session cannot re-encrypt, so the
  file is opened, and shown read-only when its permissions require it, but not
  written back.
- The native file picker has **not been verified interactively** in the
  development environment; the code path is unit-tested and the picker is the
  platform dialog.
- Snap Layouts and native non-client hit-testing are absent (custom caption).
- Rotation and zoom are view state only; they do not mark the document dirty or
  change the saved file. Byte-changing edits go through the document session,
  and Save / Save As are the only commands that write a file.
- The viewer is Windows-first. The other configuration paths exist in the
  settings module, but no macOS or Linux viewer build is verified.

## Not available in the viewer yet

These commands are present and report that they are not available in this build;
they are not dead controls, and several have a tested document-level
implementation behind them:

- Edit PDF;
- Fill & Sign and Sign (the signing library is real; see [Security](security.md));
- Organize Pages, Add page, and the page menu;
- Export, Convert to PDF, and Combine Files;
- Protect PDF (the protection and redaction libraries are real; see
  [Security](security.md));
- the AI assistant (the client exists; the panel says the assistant appears when
  it is connected — see [Privacy and local data](privacy.md)).

Check the [feature matrix](../../FEATURES.md): `library` and `model` rows are not
end-user features, and only `verified` rows are reachable.
