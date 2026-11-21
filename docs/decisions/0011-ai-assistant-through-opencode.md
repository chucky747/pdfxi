# ADR 0011 — AI Assistant through OpenCode

- Status: accepted — confirmed on OpenCode 2.3.2 by spike T-0250
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

These were answered by T-0250 against OpenCode 2.3.2; the answers and the
redacted transcripts are in "Spike results" below and in
`docs/evidence/hardening/T-0250.md`.

1. Which discovery mechanism works on loopback, and what the pairing endpoints
   require (shape, expiry, single use).
2. Whether the client-MCP attach path (decision 4) exists in the current build,
   and with which authentication.
3. The exact session create / prompt / model switch request and response shapes,
   including how streaming events are addressed to one session.
4. How cancellation is expressed, and what timeout the service applies.
5. The minimum token scope that allows prompts and model switching but nothing
   else, and where Credential Manager storage is keyed.

## Spike results (T-0250)

Probe date: 2026-11-20. One Windows host, one running desktop install.
Result: **accepted**. `/api/info`, `/api/provider`, `/api/model`, session
create, one prompt, and the model switch all worked against the live service,
and the client-MCP WebSocket attach was exercised. Everything below was
observed; fields not observed are marked unverified rather than filled in.

### Pinned versions

| Component | Version | Source |
| --- | --- | --- |
| OpenCode service | `2.3.2` | live `/api/info` |
| Registration file `version` | `2.3.2` | `state/opencode/service.json` |
| `opencode-cli.version` | `2.3.2` | install folder |
| MCP `clientInfo.version` | `2.3.2` | live initialize frame |
| OpenAPI document | `3.1.0`, info version `0.0.1`, title `opencode HttpApi`, 142 paths | live `/openapi.json` |
| Desktop shell `OpenCode.exe` File/ProductVersion | `1.4.4` (Bun runtime metadata, not the service version) | file properties |

A future upgrade is a compatibility event: the client compares
`/api/info.version` and shows an incompatibility message unless it is
`2.3.x`.

### Discovery (how the CLI finds the service)

- `opencode` is not on `PATH` on this host. The binary is
  `%LOCALAPPDATA%\Programs\ai.opencode.desktop\resources\opencode-cli.exe`,
  launched as `opencode-cli.exe serve --service`.
- `opencode-cli.exe service status` prints the endpoint URL
  (`http://127.0.0.1:28894`). That is the supported, version-independent way
  to ask the installed app where its service is.
- The registration file is
  `%USERPROFILE%\.local\state\opencode\service.json` with keys `id`,
  `version`, `url`, `pid`, `password`. It is the standard registration path
  used by `Service.discover()` in `@opencode/client/service`.
- `%USERPROFILE%\.config\opencode\service.json` also exists, holding
  `remote.route`, `password`, and `env`. It carries the tunnel route, not the
  listener URL; its password is byte-equal to the registration password
  (compared in memory, values never printed). `%LOCALAPPDATA%\opencode` and
  `%APPDATA%\opencode` do not exist.
- The listener was confirmed with `Get-NetTCPConnection`:
  `127.0.0.1:28894`, owning process `opencode-cli.exe`. `/api/info` also
  advertises `https://<route>.<host>.opentunnel.xyz`; PDFXI ignores it unless
  the user explicitly chooses a remote connection.

### Authentication

The service uses **HTTP Basic**, username `opencode`, password = the
`password` value in the registration file (43 characters, base64url of 32
bytes).

```text
Authorization: Basic base64("opencode:" + password)
```

Observed against `GET /api/info`: no header -> `401`; `Bearer <password>` ->
`401`; Basic with an empty username -> `401`; `x-opencode-password` -> `401`;
Basic with username `opencode` -> `200`. The value is never written to this
ADR, the evidence file, settings, or logs. PDFXI stores it under the
registration `id` in Windows Credential Manager.

### Server info

```json
{"version":"2.3.2","pid":17164,"hostname":"<host>",
 "urls":["http://127.0.0.1:28894","https://<route>.<host>.opentunnel.xyz"],
 "paths":{"tmp":"<temp>"},"capabilities":{"persistentPty":true}}
```

### Models and providers

`GET /api/provider` -> `{"location":{"directory":...},"data":[...]}` with three
providers: `opencode-go` (enabled), `opencode` (enabled), `claude-agent`
(auto). `GET /api/model` returns the same envelope with 57 models. The test
model is present and enabled:

```json
{"id":"hy4-flash-free","modelID":"hy4-flash-free","providerID":"opencode-go",
 "type":"chat","name":"Hy4 flash Free","status":"active","enabled":true,
 "variants":[{"id":"none"},{"id":"high"}],
 "capabilities":{"tools":true,"input":["text","image"],"output":["text"]},
 "limit":{"context":1048576,"input":983040,"output":65536},
 "cost":[{"input":0,"output":0,"cache":{"read":0,"write":0}}]}
```

The wire form of a model reference is an object, not the `provider/id`
string:

```json
{"providerID":"opencode-go","id":"hy4-flash-free","variant":"default"}
```

`id` and `providerID` are required; `variant` is optional and the service
fills `"default"`. The UI label `opencode-go/hy4-flash-free` maps to
`providerID + "/" + id`.

### Session, prompt, messages

Create (`POST /api/session`):

```json
{"title":"PDFXI spike T-0250",
 "model":{"providerID":"opencode-go","id":"hy4-flash-free"},
 "location":{"directory":"<absolute directory>"}}
```

Response `200 {"data":{"id":"ses_...","projectID":"...","model":{...,"variant":"default"},
"cost":0,"tokens":{...},"time":{"created":...,"updated":...},"title":"...",
"subpath":"...","location":{...}}}`. Session IDs match `^ses`.

Prompt (`POST /api/session/{id}/prompt`):

```json
{"text":"Reply with the single word: pong"}
```

Response `200 {"data":{"id":"msg_...","sessionID":"ses_...","time":{"created":...},
"type":"user","payload":{"text":"..."},"delivery":"steer"}}`. The route
enqueues and returns; it does not return the answer.

Messages (`GET /api/session/{id}/message`) return
`{"data":[...],"cursor":{"previous":...,"next":...}}`, newest first by
default. Observed assistant item:

```json
{"id":"msg_...","type":"assistant",
 "time":{"created":...,"firstToken":...,"streamed":...,"completed":...},
 "agent":"orchestrator",
 "model":{"providerID":"opencode-go","id":"hy4-flash-free","variant":"default"},
 "content":[{"type":"reasoning","text":"..."},{"type":"text","text":"pong"}],
 "finish":"stop","rawFinish":"stop","cost":0,"tokens":{...}}
```

An `{"type":"idle","outcome":"succeeded"}` item follows the turn. Query
parameters: `limit`, `order=asc|desc`, `cursor`, and
`type=user|assistant|idle|...`. First token arrived 2.6 s after the prompt;
the turn completed in 3.1 s on the free model. The client still applies a
60 s timeout and surfaces a timeout as an error, not a hang.

### Model switch

`POST /api/session/{id}/model` with `{"model":{"providerID":"opencode-go",
"id":"hy4-flash-free","variant":"high"}}` -> `204` with an empty body. A
subsequent `GET /api/session/{id}` reports `"variant":"high"`. The route has
no response body, so the UI re-reads the session to confirm.

### Event stream

`GET /api/event` -> `200 text/event-stream`. Raw frames in a 4 s idle window:

```text
data: {"id":"evt_...","type":"server.connected","data":{}}

: heartbeat
```

The schema is `{id?, event, data: V2EventEncoded}` over SSE; observed frames
put the event name inside `data.type` and emit no `event:` line. Subscriptions
are live-only (no replay) and accept `?types=` filters. The catalogue includes
`session.text.delta`, `session.text.ended`, `session.step.*`, `session.idle`,
`session.model.selected`, and `permission.asked`. Correlating an event to a
session uses the location/session fields inside the encoded payload; that was
not observed during a live turn here (idle stream only), so the exact
correlation key stays unverified.

### Client MCP (decision 4, primary path) - verified handshake

`GET /api/experimental/client-mcp` is a WebSocket upgrade endpoint
(`x-websocket: true` in `/openapi.json`), described as: "Upgrade to a
WebSocket over which the client hosts an MCP server named after `name`; its
tools stay available while the socket is open. One JSON-RPC message per text
frame." Query: `name` (required, `^[a-zA-Z0-9_-]+$`), optional `ticket`,
optional `location[directory]`.

Two attach paths were exercised on the live service:

1. **Native app, Basic auth (chosen for PDFXI).** Connect
   `ws://127.0.0.1:<port>/api/experimental/client-mcp?name=<name>` with the
   Basic `Authorization` header. The socket opened and the service sent the
   MCP initialize request:

   ```json
   {"method":"initialize","params":{"protocolVersion":"2025-11-25",
    "capabilities":{"elicitation":{"form":{},"url":{}},"roots":{}},
    "clientInfo":{"name":"opencode","version":"2.3.2"}},
    "jsonrpc":"2.0","id":0}
   ```

   After the client replies with an `initialize` result, the service sends
   `{"jsonrpc":"2.0","method":"notifications/initialized"}`. No `tools/list`
   request arrived within 5 s, so lazy tool discovery is unverified; the
   session had been created before the attach.

2. **Browser-style ticket.** `POST /api/experimental/client-mcp/token?name=<name>`
   with the header `x-opencode-ticket: 1` -> `200 {"location":{...},
   "data":{"ticket":"<36 chars>","expires_in":60}}`. Without that header the
   same call is `403` with an empty body. The ticket is single-use, valid for
   60 s, and the socket URL is
   `.../api/experimental/client-mcp?name=<name>&ticket=<ticket>` with no
   `Authorization` header. This is the browser path only.

   Conclusion: client-MCP **is** the primary design, attached from the native
   app over a loopback WebSocket with Basic auth. PDFXI hosts a read-only MCP
   server on that socket. The ticket route is unnecessary for the desktop app
   and puts a secret in a URL, so PDFXI does not use it.

### RPC fallback (decision 5)

`POST /api/rpc/{rpcID}/{method}` with `{"input":...}` -> `{"output":...}`;
`Rpc.Input` and `Rpc.Output` are `{input?}` / `{output?}` holding any JSON
value. Without an installed plugin the call is `400` with an empty body, so a
plugin must be installed and configured to implement the RPC; the generic
route cannot conjure a handler. Plugin RPC events use the
`rpc.<rpcID>.<event>` envelope. The fallback remains a fallback.

### Cancellation and permissions (schema only)

- `POST /api/session/{id}/interrupt` (`?resume=true|false`) ->
  `{"interrupted":true|false}`. This is the cancellation hook. Not exercised.
- Session create accepts a `permissions` ruleset (`Permission.Ruleset`), and
  the event catalogue has `permission.asked` plus
  `POST /api/session/{id}/permission/{requestID}/reply`. PDFXI creates
  assistant sessions with a ruleset that allows only the PDFXI MCP tools;
  `/api/session/{id}/shell`, `/api/session/{id}/command`, and the file routes
  are never called by the assistant. The exact ruleset shape is unverified.

### What did not work, and what stays unverified

- Pairing (`POST /api/pair`, `GET /auth/connect/{code}`) exists in
  `/openapi.json` but was not exercised; no pairing code was available, and
  the loopback + Basic path made it unnecessary on this host.
- `tools/list` and actual tool invocation through client-MCP were not
  observed; only the initialize handshake was.
- Live event correlation during a generation (`session.text.delta` payload)
  was not captured; the idle stream showed only `server.connected`.
- Cancellation, the permission round-trip, and the `Permission.Ruleset` shape
  are schema-only.
- Exactly one generation call was made (budget rule); no retry, no other
  provider, no paid model.

### Security constraints confirmed by the spike

- Loopback only. The service also advertises a public tunnel URL; PDFXI never
  uses it without an explicit, visible user action.
- The Basic password is the service's full credential. It is read from the
  registration file, held in memory for one request, and stored only in
  Windows Credential Manager keyed by the registration `id`; never in
  settings, documents, logs, evidence, or crash reports.
- Document text is untrusted tool output. Read-only tools, a size cap, and an
  allow-only-PDFXI-tools session ruleset are the enforcement points; nothing
  is sent before the user asks.

## Not decided here

- No hosted service, account, or PDFXI-operated relay (campaign section 7).
- No write or editing tools for the assistant; no filesystem or shell access.
- No local model runtime shipped with PDFXI.

## Amendment (2026-11-20): credential lifetime in the app

Shipped T-0251/T-0252/T-0253 behavior. Where it differs from decision 6 and
"Authentication" above, this section is authoritative.

- Discovery reads the password from `<home>/.local/state/opencode/service.json`
  (the registration file OpenCode itself writes) into the client's `Secret`.
  Explicit pairing puts the user-entered code into the same `Secret`; it uses
  the explicit-endpoint path, not an OpenCode pairing handshake.
- The app keeps the `ServiceEndpoint` and its Basic credential in memory for the
  connection's lifetime, so later prompts reuse the same client/session. It is
  never written to settings, logs, evidence or crash text, and is not displayed:
  the UI shows only the service URL.
- `disconnect()` clears the retained endpoint and pairing input; dropping the
  app drops the secret. The Windows Credential Manager storage in the original
  decision is **not implemented**: the registration file is the only persisted
  credential source, and OpenCode writes it, not PDFXI.
- Consequence: any future remote pairing that must survive a restart needs an
  explicit credential-storage task; it is out of scope for this campaign.
