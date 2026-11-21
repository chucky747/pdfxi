# Open, save, and print

This page covers getting a document into the viewer, writing it back, and
sending it to a printer. The library surfaces that list documents are in
[Library: Recent, Starred, Files](library.md).

## Opening a document

There are four entry points, and they all end in the same document session:

- **Open a PDF** on the Home screen, the **+** button in the tab bar, or
  <kbd>Ctrl</kbd>+<kbd>N</kbd> opens the native file picker.
- Drag one or more PDFs from File Explorer onto the window. Non-PDFs are
  ignored.
- Launch `pdfxi-ui.exe <path>` to open that document. This is what the Start
  Menu shortcut and "Open with" pass.

> **Shortcut note.** The bound shortcut is <kbd>Ctrl</kbd>+<kbd>N</kbd>. The
> Home screen and the Help text still say <kbd>Ctrl</kbd>+<kbd>O</kbd>; that hint
> is stale ([T-0261A](../../evidence/acceptance/T-0261A.md)).

Selecting several files in the picker opens each as its own tab. Opening a file
that is already open focuses its existing tab instead of creating a duplicate.
Tabs can be activated and closed; closing the last tab returns to Home.

A file that cannot be opened produces a dismissible error banner instead of a
blank canvas. The same banner covers a corrupt file, a page whose size cannot be
read, and a document that exceeds the input limit.

### Input limit

Documents larger than **2 GiB** are rejected with an error. The viewer does not
read them partially and does not offer a "continue anyway" path.

### Password-protected documents

An encrypted PDF opens through a masked password modal:

- a wrong password keeps the modal open and shows a typed message;
- **Cancel** opens no tab and explains why;
- a document whose permissions deny modification opens **read-only**.

Read-only is enforced by the viewer's own commands, not by cryptography. A
reader that ignores the permission mask can still open the file; see
[Security](security.md). An encrypted document can be read but not saved in this
build, because the session cannot re-encrypt.

## Saving

| Command | Shortcut | Behaviour |
|---|---|---|
| Save | <kbd>Ctrl</kbd>+<kbd>S</kbd> | Writes the current document back to its path |
| Save As | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> | Chooses a new path, then writes |

Saves are **atomic**: the new bytes are written to a temporary file in the same
directory and then renamed over the old file. A failure part-way through a save
therefore does not truncate the previous file.

When the file on disk has changed since it was opened, the viewer shows an
external-change dialog.

### Encrypted documents are refused

Saving an encrypted document stops with the typed message:

```text
saving an encrypted document is not supported yet
```

The tab stays open and the file on disk is untouched. This is the current limit,
not a setting. The same applies to a read-only tab: view operations keep working,
writes are refused.

### What save does not do

- No version history, backup copy, or automatic save is kept.
- Save does not re-encrypt and does not remove protection.
- Save does not flatten annotations or form fields; flattening is a separate,
  confirmed action in [Fill & Sign](fill-and-sign.md).

## Printing

**Print** is always drawn in the command bar. It is not gated on the document
being dirty, and it does not change the file.

The print path is:

1. A print pre-panel confirms the job before the system dialog opens.
2. The Windows `PrintDlgW` dialog runs on a worker thread, so the window stays
   responsive while the system dialog is up.
3. Pages are rasterized and sent to the printer through GDI.
4. Cancellation is honoured **between pages**: pressing Cancel stops the job
   before the next page starts. A page already being rasterized finishes first.

`pdfxi-ui.exe --print <path>` drives the same path for a document passed on the
command line.

### Print limits

- Output is **GDI raster**, not vector PDF. Text is rendered at the print
  resolution chosen by the driver; it is not sent as selectable text.
- Cancellation is between pages, not mid-page.
- The system dialog is `PrintDlgW`, so this flow is Windows-specific. No macOS
  or Linux print flow is verified.

## Related pages

[Library: Recent, Starred, Files](library.md) · [Pages panel](pages-panel.md) ·
[Desktop viewer](gui.md) · [Security](security.md)
