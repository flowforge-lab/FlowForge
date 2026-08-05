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
//! - **v1 subset only** — v2 unstable features are not included. If the surface
//!   grows beyond ~50 structs, re-evaluate vendoring
//!   `agent-client-protocol-schema` per the Q4 escape hatch.
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
