# Desktop viewer

`pdfxi-ui` is the native viewer. It is an `egui`/`eframe` window over the
`pdfxi-app` state model. Every control in the current shell either acts on a
real document or changes a real setting; controls that had no implementation
were removed rather than left painted. This page is the map of the shell. Each
tool has its own page.

To install or build it, see [Installing PDFXI](install.md).

## What is on screen

The title bar carries the gear menu, the document tabs, and the caption buttons.
Below it, the left rail switches between the panel surfaces:

| Surface | State in this build |
|---|---|
| Documents | Open tabs and the current document |
| Files | Folder browser rooted at the library folder |
| Recent | Recent documents, newest first |
| Starred | Starred documents |
| Comments | Notes and highlights for the current document |
| Forms | Form fields for the current document |
| Signing | The certificate Sign card |
| AssistantConnect | The AI Assistant panel |
| Shared | Only when a shared folder is configured; placeholder, no review features |

**Edit**, **Export**, and **Protect** capabilities are off: their commands are
present and report that they are not available in this build. **Print** is always
drawn and works.

The viewer opens on an empty **Home** screen: the PDFXI mark, an **Open a PDF**
button, a drop hint, and a keyboard hint. No document is loaded until you open
one. There is no bundled sample document.

## Opening and saving

The native picker, drag and drop, multi-select, duplicate handling, the password
modal, the 2 GiB input cap, atomic save, the encrypted-save refusal, and the
external-change dialog are described in
[Open, save, and print](open-save-print.md).

**Shortcut note.** The shell binds <kbd>Ctrl</kbd>+<kbd>N</kbd> to Open. The
Home screen and the Help text still say <kbd>Ctrl</kbd>+<kbd>O</kbd>; that hint
is stale. Use <kbd>Ctrl</kbd>+<kbd>N</kbd>, the **Open a PDF** button, or the
**+** button in the tab bar.

## Navigating

- The thumbnail panel shows real page thumbnails; selecting one navigates to
  that page. See [Pages panel](pages-panel.md).
- The canvas is continuous: scroll to move through the document.
- The floating page/zoom pill shows the current page and the page count.
- Page rasters are produced by PDFium at the current zoom and display scale, so
  text stays crisp at non-default Windows scaling.
- The command bar and the page pill cover zoom in, zoom out, fit page, fit
  width, actual size, and rotate clockwise. Rotation changes the view only; it
  does not rewrite the file.

## Search

Find searches the extracted text of the open document and highlights matches
with per-span boxes. Options, match navigation, and the result cap are in
[Search](search.md).

## Comments, forms, and signing

- [Comments](comments.md) — text notes, highlights, replies, and resolve.
- [Fill & Sign](fill-and-sign.md) — fill fields, reset, flatten, and exchange
  form data.
- [Signing and verification](signing.md) — the certificate Sign card, its
  visible overlay block, the green `✓ E-signed` chip, and what its readout
  proves.

## Assistant

The AI Assistant is live and connects only to the OpenCode service on this
machine, and only when you ask a question. What is sent, and the session
lockdown around it, are in [Assistant](assistant.md) and
[Privacy and local data](privacy.md).

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
pdfxi-ui.exe --print <path>
```

## When something fails

Runtime problems that can be shown in the window use the dismissible error
banner. If the viewer fails before its window exists, the GUI build writes the
message to `%LOCALAPPDATA%\PDFXI\logs\pdfxi-ui.log` and shows a native message
box. Panics take the same path.

Builds produced by this project are unsigned. Windows SmartScreen may warn on
first launch; see [Installing PDFXI](install.md).

## Known limitations

- Find coverage on very large documents is progressive; see [Search](search.md).
- Saving an encrypted document is refused: the session cannot re-encrypt, so the
  file is opened, and shown read-only when its permissions require it, but not
  written back.
- The native file picker has **not been verified interactively** in the
  development environment; the code path is unit-tested and the picker is the
  platform dialog.
- Snap Layouts and native non-client hit-testing are absent (custom caption).
- Rotation and zoom are view state only; they do not mark the document dirty or
  change the saved file.
- The viewer is Windows-first. The other configuration paths exist in the
  settings module, but no macOS or Linux viewer build is verified.
- The Shared surface is a placeholder. It is not a review workflow and does not
  synchronize anything.

## Not available in the viewer

These commands are present and report that they are not available in this build;
they are not dead controls, and several have a tested document-level
implementation behind them:

- Edit PDF;
- Organize Pages, including Add page and the page menu;
- Export, Convert to PDF, and Combine Files;
- Protect PDF, including Redact (the protection and redaction libraries are
  real; T-0263 is pending — see [Security](security.md)).

Check the [feature matrix](../../FEATURES.md): `library` and `model` rows are not
end-user features, and only `verified` rows are reachable.
