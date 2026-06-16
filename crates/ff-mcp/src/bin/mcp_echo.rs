//! A minimal MCP server used only by `ff-mcp`'s integration test: one `echo` tool
//! over stdio. Built as a crate bin so `tests/stdio_client.rs` can spawn it via
//! `CARGO_BIN_EXE_mcp_echo` — exercising the real child-process stdio handshake with
//! no network and no external dependency.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_router, ServiceExt};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct EchoRequest {
    /// The message to echo back.
    message: String,
}

#[derive(Debug, Clone)]
struct Echo;

#[tool_router(server_handler)]
impl Echo {
    #[tool(description = "Echo back the provided message")]
    fn echo(&self, Parameters(EchoRequest { message }): Parameters<EchoRequest>) -> String {
        message
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = Echo.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
