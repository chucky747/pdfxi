# ADR 0011 — AI Assistant through OpenCode

- Status: proposed — to be confirmed by spike T-0250
- Date: 2026-11-20

## Context

The acceptance image and the owner list both include an AI Assistant: ask about
the open document, summarize it, and show key insights with page citations. The
owner decision of 2026-11-20 is that PDFXI does not build or host a model
backend:

- The backend is the users own OpenCode installation, using whatever providers
  that installation already has configured. Model and provider are switchable
  exactly as in OpenCode; the test model for acceptance is
  `opencode-go/hy4-flash-free`.
- A Connect screen establishes the link, either through MCP or through a small
  plugin.
- No hosted service, no account, no PDFXI-operated server, and no PDFXI-side
  provider keys.

The OpenCode routes involved are documented as experimental: service discovery,
pairing (`POST /api/pair`, `/auth/connect/{code}`), `GET /api/model`,
`GET /api/provider`, session create / prompt / model switch, the event stream,
client-MCP attach and plugin RPC. They may change without notice, so nothing in
this ADR is final until spike T-0250 pins the routes and the OpenCode version it
used against a running service.

A second constraint is hostile documents. Document text is attacker-controlled
input. If the assistant can call arbitrary tools, a PDF containing injected
instructions becomes a remote-code-execution path. The assistant therefore gets
a deliberately narrow capability set.

## Decision

1. **OpenCode is the only backend.** PDFXI ships no model weights, no inference
   server, and no proxy to a hosted API. The assistant works only when a local
   OpenCode is reachable and paired; otherwise the UI shows "Connect to OpenCode"
   and nothing is sent.
2. **The users providers, switchable like OpenCode.** The model picker is fed
   from the OpenCode provider and model list, is changeable per conversation, and
   is remembered. The default and acceptance test model is
   `opencode-go/hy4-flash-free`.
3. **Connection by discovery or pairing code.** "Connect to OpenCode on this PC"
   discovers a running local service on loopback. "Pair with a code" uses the
   OpenCode pairing flow for a service that requires it. Loopback is the default;
   a non-loopback host requires an explicit user action and is shown in the
   connection status.
4. **Primary design: PDFXI attaches as a client MCP server.** PDFXI exposes
   read-only document tools over MCP - document info, page text, search, current
   selection, outline - and the OpenCode session attaches that server. This keeps
   tool execution inside PDFXI, where the document and the permission policy live.
5. **Fallback design: a small bridge plugin.** If a native client cannot attach,
   a small `opencode-pdfxi` plugin bridges plugin RPC to PDFXI over loopback,
   exposing the same read-only tool set. The fallback exists so an experimental
   MCP client path cannot block the feature; it is not a second product surface.
6. **The token lives in Windows Credential Manager.** A pairing token is stored
   there and never in settings files, the document, logs, evidence, or crash
   reports. Settings hold only the service address and the chosen model.
7. **The assistant session can call only the read-only PDFXI tools.** No shell,
   file, edit, network or web tools are attached to an assistant session. Document
   text travels as untrusted tool results with size caps; PDFXI never executes
   anything a document says. A negative test with an injected PDF asserts that no
   other tool is called and no file is written, from the session log.
8. **Nothing is sent before the user asks.** Opening a document sends nothing.
   The UI states what a question will send (which pages, which tool results)
   before the first request in a conversation.
9. **Nothing here is final until T-0250 pins it.** The spike records the exact
   routes, request and response shapes, and the OpenCode version in ADR 0011
   itself as an amendment, with secrets redacted. Until then the client stays
   isolated in `pdfxi-assistant` so a route change is a one-crate change.

## Consequences

- **A new crate, `pdfxi-assistant`.** HTTP client, event stream, timeouts and
  cancellation, loopback by default, credential storage through Windows Credential
  Manager. It is the only crate that knows OpenCode routes.
- **Three UI places, one feature.** The rail, the assistant card and the command
  pill in the acceptance image all drive the same per-document conversation; a
  fourth path would be a defect.
- **The capability registry decides visibility.** Per the campaign rules, the
  assistant controls appear only when a connection exists and the crate reports
  the tool set; the no-dead-controls gate (T-0212) enforces it.
- **Security review is mandatory.** T-0254 is reviewed line by line, not by test
  result alone, and its negative test is a release gate for enabling the feature.
- **Experimental routes are a standing risk.** An OpenCode upgrade may break the
  connection. The spike pins a version and records the date; a break shows a clear
  incompatibility message instead of a hang.
- **No document leaves the machine except as the users own model request.** The
  text that reaches the model is the text the user asked about, sent to the
  provider the user configured in OpenCode, under that providers terms.

## Open questions for spike T-0250

1. Which discovery mechanism works on loopback, and what the pairing endpoints
   require (shape, expiry, single use).
2. Whether the client-MCP attach path (decision 4) exists in the current build,
   and with which authentication.
3. The exact session create / prompt / model switch request and response shapes,
   including how streaming events are addressed to one session.
4. How cancellation is expressed, and what timeout the service applies.
5. The minimum token scope that allows prompts and model switching but nothing
   else, and where Credential Manager storage is keyed.

## Not decided here

- No hosted service, account, or PDFXI-operated relay (campaign section 7).
- No write or editing tools for the assistant; no filesystem or shell access.
- No local model runtime shipped with PDFXI.
