//! MCP host client (M4.0). A JSON-RPC 2.0 client speaking to an external MCP server
//! over a child process's stdio, built on the official `rmcp` SDK. Scope here is one
//! client + one server: `initialize` / `list_tools` / `call_tool` / `tools/list_changed`.
//! Config loading (M4.1), the supervisor (M4.2), and the `ToolRegistry` bridge (M4.3)
//! build on top. See `docs/rfcs/0003-mcp-host.md`.

mod client;
mod error;

pub use client::McpClient;
pub use error::McpError;
