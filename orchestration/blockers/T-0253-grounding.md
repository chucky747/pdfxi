# T-0253 grounding blocker: MCP tools attach but the model never sees them

Status: open (A-16 document grounding blocked; Round A chat UI is unaffected).

## Exact evidence (2026-11-20, OpenCode 2.3.2, hy4-flash-free)

Library live test
`cargo test -p pdfxi-assistant --test lockdown_live -- --ignored --nocapture`:

```text
session: ses_e03c1a20fffeY5O5PlrDFWTrqv
rules: deny *; allow pdfxi_document_info, pdfxi_page_text, pdfxi_search,
       pdfxi_selection, pdfxi_outline
mcp report: McpReport { initialize_seen: true, initialized: true,
  tools_list_requests: 1,
  tools_listed: ["document_info", "page_text", "search", "selection", "outline"],
  tool_calls: [], messages: 3, idle_timed_out: true }
tool names in messages: []
answer: "I don't have a `pdfxi page_text` tool available in this session..."
```

App run (`--assistant-question="What is the verification sentence on page 1?"`)
created session `ses_e03c538a8ffet36YW721ZtK79e` with the exact locked
permissions and the test model, but the model answered from its shell-tool
assumptions (printed Bash markup as text). The session context shows no tool
result and the prompt token count is too small to include five tool results.

## What this is not

- The websocket attach is not failing: the service completed `initialize`,
  `notifications/initialized` and `tools/list`, and our server returned all five
  tools.
- The locked session read-back is exact; `create_locked_session` succeeded.
- No shell, file or write tool ran (the live test's sentinel file is absent).

## Working hypotheses, in order

1. **Ordering.** The session is created before the client MCP attach. If the
   service resolves the agent's tool list at session creation, the pdfxi tools
   are missing from that session forever.
2. **Readiness.** The attach completes asynchronously; the app prompts after a
   300 ms grace, so the first prompt can race the tool registration.
3. **Name mapping.** The service may expose client-MCP tools under the
   unprefixed names (`document_info`) while the locked ruleset allows only
   `pdfxi_*`, so the permission filter drops them.
4. **Agent.** The session defaults to the `build` agent, whose own tool set is
   shell/read/write; client-MCP tools may only be added to a different agent.

## Next step

A diagnostic test attaches before `create_locked_session`, asks twice in one
session, and (if needed) tries a session with unprefixed allows. The fix follows
the result: reorder attach/create, wait for a readiness signal, or amend the
ruleset mapping.

**Experiment in flight (22:59).** The app lane is testing attach-first in the
turn runner: start `attach_client_mcp(ASSISTANT_MCP_NAME, ...)`, wait 1 s, then
`create_locked_session`, then ask. Real-service success is the fixture's page-1
sentence `Verification sentence: amber compass lantern.` in the answer. If that
fails, the next candidates are the unprefixed tool names in the ruleset and a
different session agent.

**Attach-first implemented (23:03).** `ServiceTurnRunner::run` now starts the
client-MCP attach before `create_locked_session`, waits `ATTACH_READY_WAIT`
(1 s), and treats an attach that ends before the session as a failed turn
(`the document MCP server stopped before the assistant turn started`). The
`RecordingAttacher` test records whether `POST /api/session` existed when the
attach started; it must be `Some(false)`. The real-service capture
`target/tmp/t0253-attach-first.png` is the deciding evidence.

**Result: ordering is not the cause (23:07).** Attach-first is unit-proven and
the live attach report is unchanged:
`initialize_seen: true, tools_list_requests: 1, tools_listed: [document_info,
page_text, search, selection, outline], tool_calls: [], idle_timed_out: true`.
The model still answered from shell assumptions, with no `amber compass
lantern`. Remaining causes: the unprefixed tool names in `tools/list` versus
the `pdfxi_*` ruleset actions, or tool registration happening after the prompt
(1 s is a heuristic; the report has no readiness timestamp).

**Result: naming is not the cause (23:18).** T-0270 probe with a session whose
permissions are `deny *` plus unprefixed `document_info`, `page_text`, `search`,
`selection`, `outline` allows: the read-back shows `document_info: allow` and
`pdfxi_document_info: deny`, and the model still answers "I don't have
`document_info` or `page_text` tools available in this session", with
`tool_calls: []` and `provider calls: []`. The service lists the client-MCP
tools but does not expose them to the model in this OpenCode 2.3.2 build.

**Decision (23:19): bounded fenced prompt excerpt.** Since the client-MCP
route cannot ground the model, the app sends a capped excerpt of the open
document as a fenced `<untrusted-document-text>` block in the prompt, with the
locked deny-all session retained (no tools). This preserves the security
properties that matter (nothing sent before the ask; text is untrusted,
bounded, explicitly fenced; no tool execution) and makes A-16 work on
`opencode-go/hy4-flash-free`. The deviation from "document text travels as
tool results" is recorded in an ADR and the privacy/security pages. The MCP
attach path is removed from the turn (the service does not expose it), and the
lockdown library evidence remains as a library capability.

**Fallback if the client-MCP path cannot expose tools to the model.** Send a
bounded, fenced `<untrusted-document-text>` excerpt as part of the prompt
(selected pages, hard cap, explicit instruction to ignore embedded
instructions), keep the locked session and deny-all ruleset, and amend
ADR 0011/T-0254 and the privacy text to say exactly what is sent. This is a
deliberate deviation from "document text travels as tool results" and must be
recorded as a decision, not slipped in.

Do not claim A-16 or SEC-013 until a live answer names the fixture's real text
("Verification sentence: amber compass lantern.").
