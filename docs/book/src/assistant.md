# Assistant

The AI Assistant answers questions about the open document. It is **not** a
general chat client and it is not a PDFXI-hosted service: it talks only to the
OpenCode installation on this machine, and only after you press **Ask**.

What is stored, and the full privacy argument, are in
[Privacy and local data](privacy.md). The design decisions are
[ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md) and
[ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md).

## Connecting

- The **AssistantConnect** surface connects to the local OpenCode service.
- Connecting calls only `/api/info`, `/api/provider`, and `/api/model`. It does
  **not** create a session and does **not** send document text.
- `/api/info` is a version gate: a service outside `2.3.x` is refused with a
  typed incompatibility error, and no session follows.
- The default and test model is `opencode-go/hy4-flash-free`.
- No credential is persisted by PDFXI. The service password is read from the
  OpenCode registration file at connect time, held in memory, and dropped on
  disconnect. It is never written to settings or logs.

## Nothing is sent until Ask

Opening the panel, opening a document, or connecting sends no document content.
At **Ask** the client sends exactly two things to the local service:

1. a bounded excerpt of the open document's text, wrapped in a fence;
2. your question.

The shape of the prompt is:

```text
<untrusted-document-text file="<name>" pages="1-N">
<excerpt>
</untrusted-document-text>

Question: <your question>
```

The excerpt is:

- capped at **24,000 characters**, and stops with a truncation marker;
- taken from a snapshot bounded to **64 pages / 256 KiB**;
- labelled with its page range in the `pages` attribute and a page-range
  trailer;
- fenced as untrusted input, with the fence terminator **neutralised** in the
  document text so a hostile PDF cannot close the block and smuggle instructions
  outside it.

The panel does **not** preview the excerpt or its page range before the first
request. The page range becomes visible in the sent payload's fence and trailer;
[Privacy and local data](privacy.md) states this correctly.

## The locked session

The assistant session is created with a locked permission ruleset, and the
ruleset — not the prompt and not the model — is the security boundary:

- it starts with a wildcard **deny** and adds five specific `pdfxi_*` **allow**
  rules;
- the app turn attaches **no tool server** in this build, so there is no shell,
  file, network, or web tool and no tool execution path
  ([ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md));
- the five `pdfxi_*` document tools are a library capability, not active UI
  tools, so grounding is the fenced excerpt above;
- the client reads the session back and verifies the ruleset exactly. A widened
  rule, an `ask` rule, a wrong rule count, or any change in rule content fails
  the check; the session is deleted and the operation fails with
  `LockdownUnverified`.

## Limits

- **Fencing is not enforcement.** A model can still be persuaded by text inside
  the document. What prevents a document from making the assistant run a tool is
  the deny-all session and the absence of an app tool path, not the prose in the
  prompt.
- **The ruleset is session-scoped.** The owner of the OpenCode service, or
  another local client with its credential, can change rules or run a different
  session. PDFXI cannot defend the service from its owner.
- **Version-bound.** The ruleset shape and MCP naming were observed on OpenCode
  `2.3.2`. Unknown versions are refused rather than trusted.
- **No local transcript.** Conversations live in your OpenCode service. PDFXI
  stores no assistant credential, token, or transcript in its own files.
- **No PDFXI server, no telemetry.** The only destination is the local service;
  the provider that service uses is the one you configured in OpenCode, under
  that provider's terms.
- The assistant can be wrong. Treat its answer as a draft claim about the
  document, not as an authoritative extraction.

## Related pages

[Privacy and local data](privacy.md) · [Security](security.md) ·
[Search](search.md) · [Desktop viewer](gui.md)
