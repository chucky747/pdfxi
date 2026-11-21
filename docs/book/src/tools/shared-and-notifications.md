# Shared folder, review sync and notifications

These three surfaces turn a local folder into a lightweight review drop and keep
a local activity log. Nothing is sent to a server
([T-0246](../../../evidence/ui/T-0246.md)).

## Shared folder

Choose a folder in the title-bar gear popup: **Choose folder** uses the native
folder picker, **Clear** removes it, and the choice is persisted immediately.
The **Shared** rail item appears only while a folder is configured. It lists the
folder's PDFs non-recursively (capped at 200 entries), sorted by name with byte
sizes; a rejected root, such as one containing `..`, shows a typed error.

## Shared review by XFDF

When the active document's path is inside the shared folder, the panel shows a
**Shared review** block naming `<stem>.xfdf` as present or "not written yet":

- **Share review comments** exports the document's annotations to the sidecar,
  writing a temporary file and renaming it into place.
- **Import review comments** reads the sidecar into a copy of the document and
  commits it, reporting "Imported N review comment(s)". It is enabled only when
  the sidecar exists and the tab is writable.

The separate **Import XFDF** / **Export XFDF** commands exchange *form field
values*; review sync exchanges *annotations*. Only annotations travel in the
sidecar. There is no locking, conflict merge, or author identity check: the last
writer wins.

## Notifications

The bell in the title bar shows the unread count and opens a local list with
`YYYY-MM-DD HH:MM UTC` times, a "No notifications yet." empty state, and **Mark
all read**. Entries are recorded for real events only: document opened, document
saved, export finished, signature requested, signed copy returned, and print
finished. The store is a tab-separated file in the platform config directory,
capped at 100 entries with the oldest dropped; a corrupt file loads empty.

## Limits

- Notifications are a local log, not desktop toasts, and are not synced.
- Shared review is file-based, one sidecar per document.
- Import checks the tab's read-only flag but not the ANNOTATE permission bit.
- No network location is used by default; a network folder is your own choice.

[Evidence: T-0246](../../../evidence/ui/T-0246.md) ·
[Library](../library.md) · [Comments](../comments.md)
