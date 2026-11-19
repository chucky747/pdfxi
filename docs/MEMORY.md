# MEMORY

PDFXI uses the Anthropic memory model: a scoped `/memories` directory with six
operations (`view`, `create`, `str_replace`, `insert`, `delete`, `rename`) and a
small `MEMORY.md` index. The system is installed globally so every repository
inherits it.

## Global layout

```text
~/.config/opencode/
├── AGENTS.md
├── skills/memory/SKILL.md
├── bin/memory.ps1
└── memories/
    ├── MEMORY.md
    ├── global/
    ├── projects/<repo-slug>/
    ├── inbox/
    └── archive/
```

## Protocol

1. At session start, `view /memories/MEMORY.md` and the current project's
   `PROJECT.md`.
2. Load only the files needed for the task.
3. At task end, `create /memories/inbox/<date>-<agent>-<id>.md` with a ≤10-line
   note: task, decisions, evidence, next step.
4. The memory-librarian folds inbox notes into curated files and prunes the
   index.
5. Never store secrets, credentials, raw prompts, or full transcripts.

## Concurrency

Agents write unique inbox files. Curated files (`MEMORY.md`, `PROJECT.md`,
`DECISIONS.md`, `LESSONS.md`) are written only by the memory-librarian. This
avoids lost updates while preserving Anthropic's file-based model.

## Budgets

- `MEMORY.md` ≤ 150 lines / 8 KB.
- Individual memory files ≤ 300 lines.
- Inbox notes ≤ 20 lines.
- Archive entries older than 90 days unless they are decisions.

## Project namespace

For this repository: `~/.config/opencode/memories/projects/opencode-work/`.

- `PROJECT.md` — what PDFXI is, current campaign, invariants.
- `STATE.md` — current milestone, green gates, open blockers.
- `DECISIONS.md` — cross-session decisions not yet in ADRs.
- `LESSONS.md` — recurring mistakes and toolchain quirks.

## Privacy

Memory is local. It is excluded from the repository. Do not write customer
documents, keys, tokens, or personal data. Redact paths when they contain a
username.
