# Project memory

PDFXI follows the Anthropic six-command memory model. Agents read the global
index and project file at session start and append a short note to the inbox at
task end; curated memory is owned by the memory-librarian.

- [Project memory notes](../../MEMORY.md)
- Durable project context: `projects/opencode-work/PROJECT.md` in the global
  memory store.

Never store secrets, credentials, raw prompts, or full transcripts in memory.
