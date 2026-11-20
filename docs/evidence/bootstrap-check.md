# Bootstrap check evidence (AC-01)

Command: `powershell -NoProfile -File scripts/bootstrap.ps1 -Check`
Date: 2026-11-19

```text
BOOTSTRAP CHECK (non-destructive) root=D:\Documents\opencode-work
CANDIDATE cargo=C:\Users\cmabr\.cargo\bin git=C:\Users\cmabr\AppData\Local\Programs\MinGit\cmd linker=C:\Users\cmabr\AppData\Local\Programs\winlibs\bin
TOOL git found=True path=C:\Users\cmabr\AppData\Local\Programs\MinGit\cmd\git.exe version=git version 2.56.0.windows.4
TOOL rustup found=True path=C:\Users\cmabr\.cargo\bin\rustup.exe version=rustup 1.29.3 (edbc831ce 2026-11-02)
TOOL cargo found=True path=C:\Users\cmabr\.cargo\bin\cargo.exe version=cargo 1.100.0 (3acc79c0c 2026-11-05)
TOOL rustc found=True path=C:\Users\cmabr\.cargo\bin\rustc.exe version=rustc 1.100.0 (0ef1bc386 2026-11-09)
TOOL gcc found=True path=C:\Users\cmabr\AppData\Local\Programs\winlibs\bin\gcc.exe version=gcc.exe (MinGW-W64 x86_64-ucrt-posix-seh, built by Brecht Sanders, r3) 16.2.0
TOOLCHAIN stable-x86_64-pc-windows-gnu installed=True
CHECK result=PASS missing=0 verified_tools=5
CHECK_EXIT=0
```

Toolchain pin (`rust-toolchain.toml`) is `stable-x86_64-pc-windows-gnu`, matching
`docs/decisions/0001-toolchain.md`. All five tools resolve from user-local
installs with no elevation. The winget elevation path remains unexercised and is
recorded as an assumption, not a blocker.
