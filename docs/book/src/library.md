# Library: Recent, Starred, Files

The library is a set of local views over folders on this machine. It has no
account, no server, and no synchronization. The files it remembers are listed in
[Privacy and local data](privacy.md).

## The library folder

The default library folder is:

```text
~/Documents/PDFXI
```

On Windows that resolves to `%USERPROFILE%\Documents\PDFXI`. On macOS and Linux
it resolves to `~/Documents/PDFXI`. The folder is created on the first refresh if
it does not exist.

You can point the library somewhere else with `library_root` in
`%APPDATA%\PDFXI\settings.toml` (see [Desktop viewer](gui.md) for the other
platform paths). Paths expand `~`, `%VAR%`, `$VAR` and `${VAR}`; relative paths
are rejected. Use a portable value such as `~/Documents/PDFXI` rather than an
absolute path tied to one machine.

## Files

**Files** browses the library folder:

- only `.pdf` files are listed;
- the listing is **non-recursive** — subfolders are not traversed;
- selecting a file opens it in a tab.

A PDF elsewhere on disk is still openable through the picker or by dropping it
on the window; it simply is not part of the library listing.

## Recent

**Recent** lists documents you have opened, newest first. The list is:

- capped at **20** entries;
- de-duplicated, so reopening a file moves it to the top instead of adding a
  second row;
- annotated when an entry no longer exists on disk, rather than failing to open
  or silently disappearing.

Recent entries are stored in `%APPDATA%\PDFXI\library.toml` (or the platform
config directory) and are local to this user.

## Starred

**Starred** lists the paths you have starred. A star is a local bookmark: it
records the path, does not copy the file, and does not follow the file if it is
moved. Removing a star leaves the file untouched.

## Storage meter

The library shows a storage total computed from the sizes of the files in the
library folder. It is a size readout, not a quota: PDFXI does not enforce a
limit, delete old files, or upload anything. A file outside the library folder is
not counted.

## Shared is a placeholder

A **Shared** surface appears only when a shared folder is configured with
`shared_folder` in the settings file. In this build it is a **placeholder**:

- it does not provide shared review, comments, or annotations;
- it does not synchronize files, poll a folder, or contact a server;
- it makes no claim that another person can see your changes.

Do not read the presence of the panel as a shared-review feature. Shared review
is not implemented.

## Limits

- The library folder is one root; there is no multi-root library.
- **Files** does not recurse, filter by tag, or index file contents.
- The storage total covers the library folder's files, not the whole disk.
- Recent and Starred store paths only. Moving or renaming a file breaks the
  entry until you open it again from its new location.
- No network location is used by default; configuring a network folder is your
  choice and is not a PDFXI service.

## Related pages

[Open, save, and print](open-save-print.md) · [Desktop viewer](gui.md) ·
[Privacy and local data](privacy.md)
