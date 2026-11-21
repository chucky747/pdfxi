# What the assistant sends

The assistant has one destination and one trigger. It talks only to the OpenCode
service on this machine, and it sends document content only after you press
**Ask**. This page is the exact data statement; the privacy argument and the
local-file inventory are in [Privacy and local data](../privacy.md), and the
task workflow is in [Assistant](../assistant.md).

## Before Ask

Opening the panel, opening a document, or connecting sends **no document text**.

Connecting performs a probe only:

| Call | Purpose |
|---|---|
| `/api/info` | Version gate; a service outside `2.3.x` is refused with a typed incompatibility error |
| `/api/provider` | Lists the provider configured in OpenCode |
| `/api/model` | Lists the models available for the default/test model selection |

The probe creates no session and sends no excerpt. No credential is persisted by
PDFXI: the service password is read from the OpenCode registration file at
connect time, held in memory, rendered as `<redacted>`, used only for the request
header, and dropped on disconnect. It is never written to settings or logs.

## At Ask

Pressing **Ask** sends exactly two things to the local service:

1. a bounded excerpt of the open document's text, wrapped in a fence;
2. your question.

The prompt shape is:

```text
<untrusted-document-text file="<name>" pages="1-N">
<excerpt>
</untrusted-document-text>

Question: <your question>
```

The excerpt is bounded before it is sent:

- **24,000 characters** maximum, ending with a truncation marker when clipped;
- taken from a snapshot bounded to **64 pages / 256 KiB**;
- labelled with its page range in the `pages` attribute and a page-range
  trailer;
- fenced as untrusted input, with the fence terminator **neutralised** in the
  document text so a hostile PDF cannot close the block and smuggle instructions
  outside it.

The panel does **not** preview the excerpt or its page range before the first
request. The range first becomes visible in the sent payload's fence and trailer.

## The session is locked

The session is created with a locked permission ruleset, and the ruleset — not
the prompt and not the model — is the security boundary:

- a wildcard **deny** rule comes first;
- five specific `pdfxi_*` **allow** rules follow;
- the app turn attaches **no tool server** in this build. There is no shell,
  file, edit, network, or web tool and no tool execution path
  ([ADR 0013](../../../decisions/0013-assistant-document-grounding-fallback.md));
- the five `pdfxi_*` document tools are a library capability, not active UI
  tools. Grounding is the fenced excerpt, not a tool call;
- the client reads the session back and verifies the ruleset exactly. A widened
  rule, an `ask` rule, a wrong rule count, or any change in rule content fails
  the check; the session is deleted and the operation fails with
  `LockdownUnverified`.

The default and test model is `opencode-go/hy4-flash-free`.

## What is stored

- PDFXI stores **no** assistant credential, token, or transcript in its own
  files.
- The service password lives in memory for the connection and is dropped on
  disconnect or when PDFXI closes.
- Conversations live in your OpenCode service, not in a PDFXI database.
- The provider that service uses is the one you configured in OpenCode, under
  that provider's terms. There is no PDFXI server and no telemetry.

## Limits

- **Fencing is not enforcement.** A model can still be persuaded by text inside
  the document. What prevents a document from making the assistant run a tool is
  the deny-all session and the absence of an app tool path.
- **The ruleset is session-scoped.** The owner of the OpenCode service, or
  another local client with its credential, can change the rules or run a
  different session. PDFXI cannot defend the service from its owner.
- **Version-bound.** The ruleset shape and MCP naming were observed on OpenCode
  `2.3.2`; unknown versions are refused rather than trusted.
- **No tool grounding in this build.** The client-MCP attach route is not wired,
  so no tool result grounds the answer.
- The assistant can be wrong; treat an answer as a draft claim about the
  document, not an authoritative extraction.

## Related pages

[Assistant](../assistant.md) · [Privacy and local data](../privacy.md) ·
[Security](../security.md) · [Signatures](signatures.md)
