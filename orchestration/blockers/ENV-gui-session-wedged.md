# ENV - interactive desktop session wedge (RESOLVED 2026-11-22)

Status: **resolved**. The console session recovered without a code change.
Evidence: `docs/evidence/ui/T-0362.md` (GUI window) and
`docs/evidence/convert/T-0361.md` (print test).

## Resolution

A fresh viewer launch created a real top-level window and terminated cleanly:

```text
> target\integration\debug\pdfxi-ui.exe --light fixtures\minimal.pdf
PID=29308 HWND=10029370 RESPONDING=True EXITED=False
Stop-Process (force-stop) -> process gone
```

No orphaned `pdfxi-ui` process remained. The print-to-PDF end-to-end test also
passes in `cargo xtask verify` and in `cargo test --workspace --all-features`
with no `--skip`:

```text
test print::backend_tests::prints_first_page_through_microsoft_print_to_pdf ... ok
VERIFY OK
NOSKIP_TEST_EXIT=0
```

## Historical symptoms (for re-opening)

The wedge (2026-11-22) presented as:

1. An orphaned viewer PID that survived `Stop-Process`/`taskkill /F`, plus a
   non-responding viewer with a stale window handle.
2. Fresh GUI launches created no top-level window (`hwnd=0`, idle CPU).
3. Window capture failed (`window not found`; a GDI `CopyFromScreen` path never
   returned).
4. The Microsoft Print to PDF driver wrote no file, failing the print test.

The suspected cause was a stuck GPU/driver call in the console session; the
recorded user action was sign-out/restart. The session recovered on its own.

## If it recurs

1. Reproduce the GUI symptom with the exact command above and record `PID`,
   `HWND`, `RESPONDING` and `EXITED`.
2. Reproduce the print symptom with
   `cargo test -p pdfxi-app --features ui,pdfium prints_first_page_through_microsoft_print_to_pdf -- --nocapture`
   and record the panic line.
3. Re-open this blocker; do not add an ad-hoc `--skip` to gate transcripts
   (retired by T-0361).
