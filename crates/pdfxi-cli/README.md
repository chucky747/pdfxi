# pdfxi-cli

Headless command-line interface for PDFXI. Run `pdfxi --help` or
`pdfxi <command> --help` for the authoritative flag list generated from Clap.

## `pdfxi repair <INPUT> <OUTPUT> [--json]`

Exposes the core `pdfxi-core::repair_document` path (EDIT-015) as a scriptable
command. The command reads a damaged PDF, rebuilds a correct classic
cross-reference table and trailer, writes a verified standalone PDF to a
**distinct** output path, and reports the deterministic typed repairs that were
applied.

Behaviour:

- **The input is never modified.** Passing the same path (or a symlink) as both
  `INPUT` and `OUTPUT` is refused before anything is written.
- **No traversal.** An output path containing a `..` component is rejected.
  Absolute output paths are allowed.
- **Verified output.** The written bytes are re-read and reparsed by
  `pdfxi-core` before success is reported; a page-tree failure is an error.
- **Finite damage set.** Repair handles a missing/misplaced `startxref`, wrong
  xref offsets, broken stream `/Length`, missing `endobj`/`%%EOF` and duplicate
  object revisions. It is not a general-purpose data-recovery tool. This is the
  same documented set as the core `repair` module.

Exit codes:

| Code | Meaning |
| ---- | ------- |
| `0`  | Repaired, written and reparsed successfully. |
| `1`  | CLI, path-validation, I/O or reparse error (see stderr). |
| `3`  | The input could not be repaired at all (`EXIT_REPAIR_FAILED`). |

### `--json`

Emits a stable report on stdout for both success and failure. Success includes
`ok: true`, the `input`/`output` paths, the effective `version`, output `bytes`
and `pages`, and a `warnings` array with one `{code, message}` pair per typed
core warning in deterministic order (with a matching `warning_count` and
`exit_code: 0`). Failure includes `ok: false`, a non-empty `error`,
`exit_code: 3` and an empty `warnings` array.

Example warning codes: `startxref_missing`, `xref_rebuilt_by_scan`,
`wrong_xref_offsets`, `stream_length_corrected`, `truncated_object_end`,
`truncated_eof`, `duplicate_object`, `header_normalized`.
