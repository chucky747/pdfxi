# Privacy and local data

PDFXI has no account, no PDFXI-operated server, no bundled model and no
telemetry. Normal use of the viewer and the CLI works entirely on local files.
Two things touch your data: the small set of local files the app remembers, and
the optional assistant, which talks only to the OpenCode service already running
on your machine, and only when a request is made
([ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md),
[T-0221](../../evidence/ui/T-0221.md)).

## What is stored on this machine

| File or folder | Contents | Source |
|---|---|---|
| `%APPDATA%\PDFXI\settings.toml` | Theme, window-control style, display name, library folder, optional shared folder | [T-0221](../../evidence/ui/T-0221.md) |
| `%APPDATA%\PDFXI\library.toml` | Recent documents (capped at 20, de-duplicated, missing entries marked) and starred paths | [T-0221](../../evidence/ui/T-0221.md) |
| `%APPDATA%\PDFXI\shell.toml` | Whether the tools panel is open | [T-0221](../../evidence/ui/T-0221.md) |
| `%USERPROFILE%\Documents\PDFXI` | Default library folder, created on first refresh; contains the PDFs you put there | [T-0221](../../evidence/ui/T-0221.md) |
| `%LOCALAPPDATA%\PDFXI\logs\pdfxi-ui.log` | The startup-failure or panic message of the viewer, appended by the GUI wrapper | [T-0202](../../evidence/release/T-0202.md) |

The library, recents, stars and display name are local files on this machine;
the storage total is computed from the library folder's file sizes. No account,
server or network call is involved, and the optional shared folder has no default
and is never a network location
([T-0221](../../evidence/ui/T-0221.md)). On macOS and Linux the settings live in
the platform config directory (`$XDG_CONFIG_HOME` or `~/.config`,
`~/Library/Application Support`) and the default library is `~/Documents/PDFXI`
([T-0221](../../evidence/ui/T-0221.md)).

**No telemetry.** The architecture contract is "No telemetry by default"
([ARCHITECTURE §Persistence](../../ARCHITECTURE.md)). The viewer's own persisted
state is only the files in the table above, and the CLI never uses the network or
writes telemetry ([CLI](../../CLI.md), [CLI reference](cli.md)). The app's only
network surface is the local OpenCode service: it talks to that service over
loopback HTTP and WebSocket when the user asks, and to nothing else. There is no
telemetry
([ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md)).
The source scan is recorded in
[the T-0269 evidence](../../evidence/docs/T-0269.md).

**Identity and author defaults.** The display name is used for comment authors
and signers. A fresh install starts as the neutral `Reader`, deliberately not
your OS account name, so comments and signatures do not leak it by default; the
name is editable in the identity popover
(`crates/pdfxi-app/src/settings.rs`,
`crates/pdfxi-app/src/ui.rs`). Existing settings that already contain a name
keep it ([T-0221](../../evidence/ui/T-0221.md)). It is not sent to a server by
PDFXI. It is written into comments you add — as the `/T` author field — so it
travels with a document you save and share
(`crates/pdfxi-app/src/shell/comments.rs`,
`crates/pdfxi-edit/src/document_annotations.rs`).

## The assistant

**Status in this build.** The AI Assistant is live. It connects to the OpenCode
service on this machine only when you ask a question; opening the panel or a
document sends nothing ([ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md)).
Before the first request the UI shows what the question will send: the page range
of the excerpt, the provider and model, and that document text is untrusted input
([ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md)
decision 8, [T-0254](../../evidence/hardening/T-0254.md) §6.5). The session
lockdown remains verified (`SEC-013`,
[T-0254](../../evidence/hardening/T-0254.md)).

### What it connects to

- **Your own OpenCode is the only backend.** PDFXI ships no model weights, no
  inference server, no proxy to a hosted API, no account and no PDFXI-side
  provider keys. The assistant works only when a local OpenCode installation is
  reachable and paired; otherwise nothing is sent
  ([ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md)
  decisions 1–2 and "Not decided here").
- **Loopback by default.** Discovery asks the installed app where its service is
  and reads the app's registration file, which points at `127.0.0.1`. The
  service may also advertise a public tunnel URL; PDFXI ignores it unless the
  user explicitly chooses a remote connection, which is shown in the connection
  status
  ([ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md) decision 3,
  [T-0250](../../evidence/hardening/T-0250.md) "Discovery" and "Security
  constraints confirmed by the spike").
- **The service credential stays in memory.** The client authenticates with the
  service's HTTP Basic password, read from your OpenCode registration file at
  connect time. It is held in a value that renders `<redacted>`, used only for
  the request header, and never logged or written to settings; it is dropped on
  disconnect ([T-0251](../../evidence/hardening/T-0251.md) §1,
  [ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md) decision 6
  and its 2026-11-20 amendment).
- **A version gate comes first.** Before any session exists, the client reads
  `/api/info` and refuses a service outside `2.3.x` with a typed
  incompatibility error; no session and no document text follow
  ([T-0254](../../evidence/hardening/T-0254.md) §1.1 and §2.2).

### What is sent, and when

- **Nothing is sent until you ask.** Opening a document or the assistant panel
  makes no request. At Ask time the app sends your typed question plus a bounded
  excerpt of the open document's text to that local service, and to no other
  network destination
  ([ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md)).
- **The excerpt is bounded and labelled.** It is capped at 24,000 characters,
  stops with a truncation marker, and states the included page range. It is
  wrapped in an `<untrusted-document-text file="…" pages="1-N">` fence; the fence
  terminator is neutralised in the document text so a hostile PDF cannot close
  the block and smuggle instructions outside it
  ([ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md)).
- **The library also caps and fences tool results** at 64 KiB per call and 256 KiB
  per conversation, with forged markers neutralised; that is a library
  capability, not the app's grounding path
  ([T-0254](../../evidence/hardening/T-0254.md) §2.3).
- **The text goes to the provider you configured in OpenCode**, under that
  provider's terms. That local service is the only destination; there is no
  PDFXI server and no other network call
  ([ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md)
  consequences).
- **The client-MCP tool route is not used in this build.** OpenCode 2.3.2 lists
  the five document tools but does not expose them to the model, so the UI does
  not make the five `pdfxi_*` tools active and no tool result grounds the answer
  ([ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md)).

### The session lockdown

The assistant session is created with a session permission ruleset, and the
ruleset — not the prompt, and not the model — is the security boundary
([ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md) decision 7):

- the locked ruleset starts with a wildcard `deny` and, in the library
  capability, five specific `allow` rules; the service evaluates last-match-wins
  ([T-0254](../../evidence/hardening/T-0254.md) §1.1, §1.4);
- in this build the app turn attaches no tools at all and the client-MCP attach
  is not wired in. There is no shell, file, edit, network or web tool and no tool
  execution path; the five document tools are a library capability, not active
  UI tools, and grounding is the bounded fenced excerpt above
  ([ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md),
  [ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md)
  decision 7, [T-0254](../../evidence/hardening/T-0254.md) §1.3);
- the client reads the session back and verifies the ruleset exactly. A widened
  rule, an `ask` rule, a wrong rule count or any change in rule content fails
  verification — rule order alone is ignored — and the session is deleted and the
  operation fails closed with `LockdownUnverified`
  ([T-0254](../../evidence/hardening/T-0254.md) §2.1–2.2);
- on the live 2.3.2 service, the locked session denied `execute`, `task`,
  `glob`, `grep`, `list`, `webfetch`, `read`, `write`, `edit`, `shell` and
  `bash`, and allowed the five document tools
  ([T-0254](../../evidence/hardening/T-0254.md) §1.2);
- a live hostile-document test injected text telling the model to run `shell`
  and write a sentinel file. The model printed a tool-call marker as text, but
  zero tool calls reached the MCP server, no tool item appeared, the sentinel was
  never created, and the independent permission evaluator denied the non-document
  actions ([T-0254](../../evidence/hardening/T-0254.md) §4). SEC-013 evidence is
  read against this locked-session + excerpt design, not against MCP tool calls.

The limits are stated in the same evidence and matter when judging the claim:

- the ruleset is **session-scoped**. The owner of the OpenCode service, or
  another local client with its credential, could change the rules or run a
  different session; PDFXI cannot defend the service from its own owner
  ([T-0254](../../evidence/hardening/T-0254.md) §7);
- **fencing is not enforcement.** A model can still be persuaded by document
  text; the deny-all session and the absence of an app tool path are what keep a
  document from making the assistant run a tool
  ([T-0254](../../evidence/hardening/T-0254.md) §7);
- the ruleset shape and MCP naming were observed on OpenCode `2.3.2` on one
  host. Unknown versions are refused rather than trusted
  ([T-0254](../../evidence/hardening/T-0254.md) §1.1, §7).

### What is stored, and disconnecting

This build stores no assistant credential, token or transcript in PDFXI files.
The client reads the service password from your OpenCode registration file when
it connects and keeps it in memory for the life of the connection; it is never
written to settings or logs and is dropped on disconnect
([T-0251](../../evidence/hardening/T-0251.md) §1,
[ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md) decision 6
and its 2026-11-20 amendment). Conversations live in your OpenCode service; the
client creates a session there and reads its messages back
([T-0251](../../evidence/hardening/T-0251.md) §3,
[T-0250](../../evidence/hardening/T-0250.md) "Session, prompt, messages").
The original
[ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md) decision 6
records Windows Credential Manager as the intended store for a pairing token;
the amendment makes the in-memory credential authoritative. No pairing token
exists in this build, and the pairing route was not exercised
([T-0250](../../evidence/hardening/T-0250.md) "What did not work").

Disconnect drops the in-memory credential and ends the connection; closing PDFXI
does the same. PDFXI does not use the service's public tunnel URL without an
explicit user choice, and it never sends anything before a request is made
([ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md) decisions 3
and 8, [T-0250](../../evidence/hardening/T-0250.md) "Security constraints
confirmed by the spike").

## Related pages

[Security](security.md) · [Desktop viewer](gui.md) · [CLI reference](cli.md) ·
[Feature matrix](../../FEATURES.md)
