//! Test fixture: one `pwd` tool over stdio that returns the server process's current
//! working directory, so the client/supervisor tests can verify a configured `cwd`
//! is applied to the spawned child (#548 W1b).

use rmcp::transport::stdio;
use rmcp::{tool, tool_router, ServiceExt};

#[derive(Debug, Clone)]
struct Cwd;

#[tool_router(server_handler)]
impl Cwd {
    #[tool(description = "Return the server's current working directory")]
    fn pwd(&self) -> String {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = Cwd.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
