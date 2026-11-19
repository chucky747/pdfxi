# ADR 0001 — Windows bootstrap toolchain

## Status

Accepted.

## Context

The autonomous shell cannot elevate. `winget` installs for Git and Visual Studio
Build Tools request administrator approval and fail with exit codes 1 and 5. A
working linker is required for tests, not just `cargo check`.

## Decision

Use user-local fallbacks on this host:

- MinGit under `%LOCALAPPDATA%\Programs\MinGit`.
- WinLibs MinGW under `%LOCALAPPDATA%\Programs\winlibs`.
- Rust toolchain `stable-x86_64-pc-windows-gnu`.

CI will add an MSVC job with Visual Studio Build Tools so the codebase remains
portable. The repository does not require MSVC for development.

## Consequences

- Tests and builds work without elevation.
- The host toolchain differs from the eventual MSVC release toolchain.
- `scripts/bootstrap.ps1` must keep both paths working.
