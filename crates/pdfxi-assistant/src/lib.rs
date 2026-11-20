//! Client for the user's local OpenCode service, per ADR 0011
//! (`docs/decisions/0011-ai-assistant-through-opencode.md`).
//!
//! PDFXI does not host a model backend. The assistant talks to the OpenCode
//! service the user already runs on this machine: read the service
//! registration, authenticate with HTTP Basic, list providers and models,
//! create a session, prompt, switch models, consume the event stream, and
//! attach as a client MCP server. Routes are pinned by the T-0250 spike; the
//! service password is read at runtime and never logged or stored by this
//! crate.
//!
//! This crate is a scaffold only. Discovery, authentication, the typed
//! request/response shapes, the event stream and the MCP attach are not
//! implemented yet; task T-0251 fills in this body.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
