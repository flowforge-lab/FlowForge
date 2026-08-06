//! Wire types for the Agent Client Protocol v1 (ACP).
//!
//! This crate defines the serde `Serialize`/`Deserialize` types for all ACP v1
//! method payloads — 12 client→agent methods, 9 agent→client methods, their
//! responses, and the shared types (content blocks, tool calls, capabilities,
//! permissions, session updates).
//!
//! # Design
//!
//! - **Leaf crate** — no dependency on `ff-mcp`, `ff-core`, or anything else in
//!   the workspace. Keeps mutation-testing fast (~25s) per `AGENTS.md`.
//! - **Transport-agnostic** — no JSON-RPC plumbing, no session state machine.
//!   Those belong to the ACP integration crates.
//! - **v1 subset only** — v2 unstable features are not included.
//!
//! # Size vs. the vendoring escape hatch
//!
//! #1200's Q4 set a trigger to re-evaluate vendoring `agent-client-protocol-schema`
//! if the surface grows beyond ~50 structs. This crate ships **119 types
//! (103 structs + 16 enums)** — over 2× that estimate. The decision to hand-write
//! anyway is recorded on #1200 and rests on:
//!
//! 1. **The count is nesting, not surface.** The v1 method set is only 21 methods;
//!    the bulk of the 119 types is the deeply-nested capability tree
//!    (`ClientCapabilities` alone pulls in 8 nested types) and the `session/update`
//!    notification tree. Both are flat wire records with no logic to carry.
//! 2. **The schema crate is not a free win.** `agent-client-protocol-schema`
//!    ships generated types with custom serde attributes (e.g.
//!    `x-deserialize-default-on-error`) that differ from the attributes we want
//!    on our own types, and its version is decoupled from the wire protocol
//!    version, so pinning the crate does not pin the protocol.
//! 3. **Maintenance when v1 gains fields is additive.** New fields are optional
//!    and unknown fields are tolerated on every inbound type, so an added field
//!    never breaks deserialization. Adding it to the struct is a mechanical edit.
//!
//! If a future ACP version adds complex session-lifecycle types rather than flat
//! records, revisit the escape hatch.
//!
//! # Module layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`rpc`] | JSON-RPC 2.0 envelope (`RequestId`, `RpcError`, `ErrorCode`) |
//! | [`content`] | `ContentBlock` tagged union + `TextContent`, `ImageContent`, etc. |
//! | [`tool`] | `ToolCallUpdate`, `ToolKind`, `ToolCallContent`, `Diff`, `Terminal` |
//! | [`capabilities`] | `ClientCapabilities`, `AgentCapabilities` and nested types |
//! | [`permission`] | `PermissionOption`, `RequestPermissionOutcome`, `SessionId` |
//! | [`session`] | `SessionUpdate` tagged union, `Plan`, `SessionInfo`, `McpServer` |
//! | [`client`] | 12 client→agent method payloads + responses + `CancelNotification` |
//! | [`agent`] | 9 agent→client method payloads + responses + notifications |

pub mod agent;
pub mod capabilities;
pub mod client;
pub mod content;
pub mod permission;
pub mod rpc;
pub mod session;
pub mod tool;
