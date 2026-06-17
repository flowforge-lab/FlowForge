//! A minimal MCP server used only by `ff-mcp`'s integration tests: one async `sleep`
//! tool over stdio that blocks for the requested number of milliseconds before
//! returning. Built as a crate bin so `tests/supervisor.rs` can spawn it via
//! `CARGO_BIN_EXE_mcp_slow` and verify that app exit preempts an in-flight tool call
//! (#119) instead of stalling up to `CALL_TIMEOUT`.

use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_router, ServiceExt};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SleepRequest {
    /// How long to sleep, in milliseconds, before returning.
    ms: u64,
}

#[derive(Debug, Clone)]
struct Slow;

#[tool_router(server_handler)]
impl Slow {
    #[tool(description = "Sleep for the given number of milliseconds, then return 'done'")]
    async fn sleep(&self, Parameters(SleepRequest { ms }): Parameters<SleepRequest>) -> String {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        "done".to_string()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = Slow.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
