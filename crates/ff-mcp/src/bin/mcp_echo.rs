//! A minimal MCP server used only by `ff-mcp`'s integration test: one `echo` tool
//! over stdio. Built as a crate bin so `tests/stdio_client.rs` can spawn it via
//! `CARGO_BIN_EXE_mcp_echo` — exercising the real child-process stdio handshake with
//! no network and no external dependency.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct EchoRequest {
    /// The message to echo back.
    message: String,
}

#[derive(Debug, Clone)]
struct Echo;

#[tool_router]
impl Echo {
    #[tool(description = "Echo back the provided message")]
    fn echo(&self, Parameters(EchoRequest { message }): Parameters<EchoRequest>) -> String {
        message
    }
}

/// The sentinel this server sends as its `initialize` instructions, so a test can
/// tell "the field was plumbed through" from "the field happened to be empty"
/// (#1173).
pub const ECHO_INSTRUCTIONS: &str = "Echo server guidance: call `echo` to test the bridge.";

// Hand-written rather than `#[tool_router(server_handler)]`: that form emits an
// empty `impl ServerHandler`, which cannot carry a `get_info` override.
#[tool_handler]
impl ServerHandler for Echo {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(ECHO_INSTRUCTIONS.to_string());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = Echo.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
