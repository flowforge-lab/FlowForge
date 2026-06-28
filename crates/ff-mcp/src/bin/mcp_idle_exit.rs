//! Test fixture: serves one tool over stdio, then exits cleanly (code 0) after a
//! short period -- simulating a stdio MCP server (e.g. codegraph) that idle-exits
//! mid-session. The supervisor must detect the closed transport and restart it
//! without parking it in `Failed` when it had been healthy (#548 W1).

use std::time::Duration;

use rmcp::transport::stdio;
use rmcp::{tool, tool_router, ServiceExt};

#[derive(Debug, Clone)]
struct IdleExit;

#[tool_router(server_handler)]
impl IdleExit {
    #[tool(description = "Return 'pong'")]
    fn ping(&self) -> String {
        "pong".to_string()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = IdleExit.serve(stdio()).await?;
    // Serve briefly so the supervisor reaches Running, then idle-exit cleanly.
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(300)) => {}
        _ = service.waiting() => {}
    }
    std::process::exit(0);
}
