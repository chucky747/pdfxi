# ENV - the interactive desktop session is wedged (GUI evidence)

Status: partially resolved 2026-11-22. The **print path is healthy again**; the
GUI window symptom is still under observation. Not a code defect.

## Resolved: the print-to-PDF test (2026-11-22)

`cargo xtask verify` now runs the test with no skip and it passes:

```text
test print::backend_tests::prints_first_page_through_microsoft_print_to_pdf ... ok
VERIFY OK
```

`cargo test --workspace --all-features` with **no `--skip`** also passes:

```text
test print::backend_tests::prints_first_page_through_microsoft_print_to_pdf ... ok
green_suites=79  tests_passed=4130
NOSKIP_TEST_EXIT=0
```

The campaign's manual `--skip` transcripts are retired; see
`docs/evidence/convert/T-0361.md`. If the driver wedges again, reproduce with
the feature command above and record the exact error before adding any skip.

## Still observed: no GUI window from a fresh launch

1. An orphaned viewer process cannot be terminated and does not answer:

```text
Get-Process pdfxi-ui
  id=10060 hwnd=0 responding=True   (survives Stop-Process, Kill, taskkill /F)
  id=7556  hwnd=12320856 responding=False
```

2. Fresh GUI launches have not created a window in this console session:

```text
target\lanes\core\debug\pdfxi-ui.exe --light fixtures\minimal.pdf
  pid=24092 cpu=0.015625  hwnd=0 responding=True
EnumWindows for the pid: no top-level windows, any class, visible or hidden
```

The same binary and flags produced a full-screen window earlier in the campaign.

3. Window capture failed in this session (`window not found`; a GDI
   `CopyFromScreen` path never returned).

## What the user can do (one action)

Sign out and back in, or restart the Windows session / display driver. The stuck
PID 10060 sits in a GPU/driver call and can wedge window creation for later
processes in the same console session.

## Effect

GUI PNG evidence is unavailable until then; CLI-reachable surfaces remain the
verified path and `cargo xtask verify` is green. The print test is no longer
affected.
