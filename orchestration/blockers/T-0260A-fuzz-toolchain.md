# T-0260A fuzz toolchain blocker

- Task: T-0260A (qa) - fuzz targets for the redaction and edit paths
- Recorded: 2026-11-21
- Status: open, host-level, pre-existing for the whole fuzz harness
- Current as of 0.1.4: `cargo-fuzz` is still absent and the bundled C++ runtime
  still does not compile here. The 0.1.4 release notes list the missing
  24-hour campaign as a known limit; no release claim treats fuzzing as done.
- Owned paths: `fuzz/**`, `docs/evidence/acceptance/T-0260A.md`,
  `orchestration/feature-updates/T-0260A.toml`, `orchestration/blockers/**`

## Summary

The `redaction` and `edit` targets are registered, seeded, and exercised through
their real entry functions by a deterministic runner that passes. No libFuzzer
campaign can run on this host: `cargo-fuzz` is absent and `libfuzzer-sys 0.4.13`
(already required by the five pre-existing targets) cannot compile its bundled
C++ runtime with any compiler installed here. No target, seed, or entry function
was skipped, stubbed, or feature-gated to work around it.

## Command 1 - the required build check

```powershell
$env:CARGO_TARGET_DIR="target/lanes/qa"
cargo check --manifest-path fuzz/Cargo.toml --all-targets
```

Exit code 101. The failure is in the third-party C++ build, not in PDFXI code:

```text
warning: libfuzzer-sys@0.4.13: libfuzzer\FuzzerExtFunctionsWindows.cpp:41:11: error: expected constructor, destructor, or type conversion before '(' token
warning: libfuzzer-sys@0.4.13:    41 |   __pragma(comment(linker, "/alternatename:" WIN_SYM_PREFIX STRINGIFY(
...
error occurred in cc-rs: command did not execute successfully (status code exit code: 1): "g++.exe" "-O0" ... "-c" "libfuzzer\\FuzzerExtFunctionsWindows.cpp"
```

Cause: `libfuzzer-sys` compiles the MSVC-only
`__pragma(comment(linker, "/alternatename:..."))` block in its Windows shim.
MinGW g++ 16.2.0 rejects it.

Attempted fix A - WinLibs clang as the C++ compiler:

```powershell
$env:CC="clang"; $env:CXX="clang++"
cargo check --manifest-path fuzz/Cargo.toml --all-targets
```

Exit code 101. `clang++ --target=x86_64-pc-windows-gnu` (clang 23.1.3) rejects the
same `__pragma` block and then the sanitizer declarations:

```text
libfuzzer/FuzzerExtFunctions.def:30:44: error: expected ';' after top level declarator
libfuzzer/FuzzerExtFunctions.def:31:1: error: a type specifier is required for all declarations
```

## Command 2 - the requested campaign

```powershell
cargo +nightly fuzz run redaction -- -runs=2000
```

Exit code 101. rustup auto-installed `nightly-x86_64-pc-windows-msvc`
(1.102.0-nightly, c7d5ba59d 2026-11-20), then:

```text
error: no such command: `fuzz`
help: a command with a similar name exists: `fix`
help: find a package to install `fuzz` with `cargo search cargo-fuzz`
```

Attempted fix B - build the manifest on the MSVC nightly:

```powershell
cargo +nightly check --manifest-path fuzz/Cargo.toml --all-targets
```

Exit code 101: `error: linker `link.exe` not found`. MSVC Build Tools are not
installed, so the MSVC route cannot link either.

## Toolchain inventory (this host)

```text
rustc 1.100.0 (0ef1bc386 2026-11-09), cargo 1.100.0 (3acc79c0c 2026-11-05)
rustup toolchains: stable-x86_64-pc-windows-gnu (active, default),
                   stable-x86_64-pc-windows-msvc,
                   nightly-x86_64-pc-windows-msvc (auto-installed during this task)
g++ 16.2.0 (MinGW-W64 x86_64-ucrt-posix-seh, Brecht Sanders r3)
clang 23.1.3 (WinLibs)
cargo-fuzz: not on PATH
link.exe:   not on PATH
```

## Next viable route

On a fuzz host with a working C++ toolchain for libFuzzer (MSVC Build Tools +
clang-cl, or Linux/macOS), run:

```powershell
rustup toolchain install nightly
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run redaction -- -runs=2000
cargo +nightly fuzz run edit -- -runs=2000
```

Then the 24-hour release runs listed in `fuzz/README.md`. This blocker does not
affect any other lane.

## What was run instead (deterministic equivalent, not a weakened target)

```powershell
$env:CARGO_TARGET_DIR="target/lanes/qa"
cargo test --manifest-path fuzz/entry/Cargo.toml -- --nocapture
```

7 passed, 0 failed; per-seed counts and the full command transcript are in
`docs/evidence/acceptance/T-0260A.md`. The runner calls the exact functions the
two libFuzzer binaries call.

## Pre-existing

`fuzz/Cargo.toml` already declared `libfuzzer-sys = "0.4"` before T-0260A, so the
same `cargo check` failed on the untouched manifest. The new code adds no
dependency that worsens this; `pdfxi-fuzz-entry` deliberately has no
`libfuzzer-sys` dependency so the deterministic runner builds.
## Close-out attempt (2026-11-21): every software path tried, elevation is the limit

The orchestrator installed the missing tooling and retried:

1. `cargo install cargo-fuzz --locked` first failed with `Access is denied
   (os error 5)`; it succeeded after pointing `TEMP`/`TMP` at
   `target\tmp\cargo-install-temp` (the same host temp policy that broke the
   Inno installer). `cargo-fuzz 0.13.2` is now installed.
2. `rustup toolchain install nightly-x86_64-pc-windows-gnu --profile minimal`
   succeeded (rustc 1.102.0-nightly).
3. `CC=clang CXX=clang++ cargo +nightly-x86_64-pc-windows-gnu fuzz build
   redaction` fails in rustc, not in PDFXI:

```text
error: address sanitizer is not supported for this target
Error: failed to build fuzz script: "cargo" "build" ... --target x86_64-pc-windows-gnu
```

AddressSanitizer/libFuzzer on Windows requires the MSVC target.
4. The MSVC target needs `link.exe` and the Windows SDK, and this host has
   neither: no `cl.exe`/`link.exe`, no Visual Studio, no Windows Kits. Installing
   them was attempted:

```text
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
Installer failed with exit code: 5
```

Exit code 5 is access denied: the VS Build Tools installer requires elevation,
which this session does not have and must not request.

Conclusion: the remaining fuzz blocker is **administrator-level toolchain
installation**, not a PDFXI or test problem. The deterministic runner remains the
accepted evidence (7/7 targets pass, 104 seeded invocations) and no target is
weakened. On a host with MSVC Build Tools, `cargo +nightly fuzz build` and
`cargo +nightly fuzz run redaction` should be run before release, as the plan
already requires for the 24-hour campaign.
