# ADR 0013 — Assistant grounding via fenced prompt excerpt

- Status: accepted (2026-11-20)
- Date: 2026-11-20
- Supersedes the grounding path in ADR 0011 decision 4; see T-0253 and T-0254.

## Context

ADR 0011 chose client-MCP as the way the assistant reads the open document: PDFXI
hosts five read-only tools (document_info, page_text, search, selection, outline)
in a locked OpenCode session. T-0254 proved that lock-down is enforceable — the
ruleset denies all but those five actions, is read back and verified, and a
hostile-document test ran no tool and wrote no sentinel.

Live evidence on OpenCode 2.3.2 with the test model `opencode-go/hy4-flash-free`
shows the model never sees those tools. The attach completes (`initialize`,
`initialized`, `tools/list` returning all five) and the evaluator allows them, yet
`tool_names=[]` and `tool_calls=[]`: the model answers that it has no PDF tools.
This held with the `pdfxi_*` rules, unprefixed allows, attach-before-session, and
a second prompt in the same session. The service lists the tools but does not
expose them to this build, so A-16 (ask / summarize / key insights) cannot be
grounded through client-MCP.

## Decision

1. At Ask time, send the open document in the prompt as a bounded, fenced
   `<untrusted-document-text file="…" pages="1-N">` excerpt: cap 24,000 characters,
   stop with a truncation marker, and state the pages included. Neutralize the
   fence terminator in the document text so a hostile PDF cannot close the block
   and smuggle instructions outside it.
2. Nothing is sent before the user asks; the excerpt is built from the active
   document at Ask time.
3. Keep the locked deny-all session (no shell, file, network, or edit tools); do
   not wire the client-MCP attach into the app turn.

## Consequences

- A-16 works on the acceptance model: document text is untrusted, bounded and
  fenced, and no tool execution path exists.
- This differs from T-0254's "document text as tool results": the BudgetedTools
  capability stays as a library capability, but the app path does not use it.
  Context is capped, so very large documents are only partially grounded, and the
  answer says so.
- Privacy text must state that the question and bounded excerpt go to the local
  OpenCode service and no other network. SEC-013 evidence is read against the
  locked-session + excerpt design, not against MCP tool calls.
