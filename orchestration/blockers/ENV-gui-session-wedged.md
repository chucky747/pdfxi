# ENV — the interactive desktop session is wedged (blocks GUI and print evidence)

Status: open, needs the user. Not a code defect. Recorded so no lane wastes time
re-debugging a green tree.

## Symptoms (all reproduced after a clean `git status`)

1. An orphaned viewer process cannot be terminated and does not answer:

```text
Get-Process pdfxi-ui
  id=10060 hwnd=0 responding=True   (survives Stop-Process, Kill, taskkill /F)
  id=7556  hwnd=12320856 responding=False
taskkill /PID 10060 /F
  ERROR: The process with PID 10060 could not be terminated.
  Reason: There is no running instance of the task.
taskkill /PID 7556 /F
  SUCCESS ... but the process stays listed and not responding
```

2. Fresh GUI launches never create a window:

```text
target\lanes\core\debug\pdfxi-ui.exe --light fixtures\minimal.pdf
  pid=24092 cpu=0.015625
  hwnd=0 responding=True
EnumWindows for the pid: no top-level windows, any class, visible or hidden
cpu after 4 s: 0.015625 (idle, not spinning)
```

The same binary and flags produced a full-screen window earlier in this session.

3. Window capture fails in two independent ways:

```text
target\tmp\grab-window.ps1 -ProcId 24092
  window not found
Get-CimInstance-free GDI path: System.Drawing CopyFromScreen  -> never returns
```

4. The print driver writes no file, so the print end-to-end test fails:

```text
cargo xtask verify
  thread 'print::backend_tests::prints_first_page_through_microsoft_print_to_pdf'
  panicked at crates\pdfxi-app\src\print.rs:3094:9:
  the print-to-PDF driver wrote no file
```

## Why this is the host, not the tree

- `cargo test --workspace --all-features -- --skip
  prints_first_page_through_microsoft_print_to_pdf` -> `TEST_EXIT=0`, 78 green
  suites, 421/421 in `pdfxi-app`.
- `cargo fmt --all --check` -> 0, `cargo clippy --workspace --all-targets
  --all-features -- -D warnings` -> 0, `cargo doc --workspace --no-deps` -> 0.
- The print test opens the same window/print subsystem that no longer works; it
  passed `VERIFY OK` earlier today on an unchanged printer code path.

## What the user can do (one action)

Sign out and back in, or restart the Windows session / display driver. The stuck
PID 10060 sits in a GPU/driver call and wedges window creation for every later
process in this console session.

## Effect

GUI PNG evidence and the print-to-PDF test are unavailable until then. Rows are
still advanced through CLI-reachable surfaces meanwhile. `cargo xtask verify` is
expected to report exactly one failure, this print test, and should not be
treated as a regression.
