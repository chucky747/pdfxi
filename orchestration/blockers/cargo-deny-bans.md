# Blocker - cargo-deny bans check fails on wildcard path dependencies

- Recorded: 2026-11-20
- Task: T-0206 (release gate)
- Severity: non-blocking for the build; blocks a clean `cargo deny check`.

## Command

    cargo deny check

Installation note: `cargo install cargo-deny --locked` first failed with
`Access is denied (os error 5)` because the session could not execute build-script
output from the default `%TEMP%`; it succeeded after setting `%TEMP%`/`%TMP%` to a
directory inside the repository.

## Output summary

    advisories ok, bans FAILED, licenses ok, sources ok
    exit 2

- `error[wildcard]` for path dependencies declared as `*`:
  - `pdfxi-app`: 2
  - `pdfxi-cli`: 6
  - `pdfxi-convert`: 4
  - `pdfxi-testkit`: 3
  - `pdfxi-core`, `pdfxi-render`, `pdfxi-security`: 1 each
- Licences are fine for the dependencies added in this campaign: `rfd` 0.15.4
  (MIT), `ashpd` 0.11.1 (MIT), `pollster` 0.4.0 (Apache-2.0/MIT), `zbus` 5.19.0
  (MIT).
- `advisories ok` and `sources ok`.
- Only duplicate-version warnings otherwise.

`deny.toml` was **not** changed.

## Viable fixes (not applied)

1. Give every intra-workspace path dependency an explicit `version` alongside
   `path`, so the requirement is no longer a wildcard. This is the strict fix and
   keeps the bans check meaningful.
2. Allow wildcards only for path dependencies in `deny.toml`, for example a
   `[bans]` section with `wildcards = "allow"` plus `allow-wildcard-paths = true`
   (the exact key depends on the cargo-deny version), or a targeted exception.
   This weakens the check, so it needs an ADR.

Choose (1) unless there is a reason to keep wildcards; then document (2) in an
ADR under `docs/decisions/`.
