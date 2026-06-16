//! The client wrapper: one connected MCP server, exposing handshake / list / call in
//! terms of `ff-core` types rather than `rmcp` internals.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ff_core::{McpServerConfig, McpToolInfo};
use rmcp::model::CallToolRequestParams;
use rmcp::service::{NotificationContext, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use tokio::process::Command;

use crate::error::McpError;

/// A `ClientHandler` whose only job is to flip a flag when the server announces
/// `tools/list_changed`, so the caller knows to re-`list_tools` (RFC 0003 §4). Every
/// other notification keeps the trait default (ignored).
#[derive(Clone, Default)]
struct ListChangedFlag(Arc<AtomicBool>);

impl ClientHandler for ListChangedFlag {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A live connection to one MCP server. Dropping or `shutdown`-ing it ends the child.
pub struct McpClient {
    server_id: String,
    service: RunningService<RoleClient, ListChangedFlag>,
    tools_changed: Arc<AtomicBool>,
}

impl McpClient {
    /// Spawn the server described by `config` and complete the `initialize` handshake.
    ///
    /// Env isolation (RFC 0003 §9.2): the child starts from an **empty** environment
    /// with only the declared `env` keys applied, so a third-party server can't
    /// harvest unrelated host secrets. The system-var allowlist (PATH/HOME, needed to
    /// resolve a bare `command`) is layered in by the supervisor (M4.2); until then a
    /// server is reachable by absolute path or via its own declared `env`.
    pub async fn connect(config: &McpServerConfig) -> Result<Self, McpError> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        cmd.env_clear();
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| McpError::Spawn(config.id.clone(), e.to_string()))?;
        let handler = ListChangedFlag::default();
        let tools_changed = handler.0.clone();
        let service = handler
            .serve(transport)
            .await
            .map_err(|e| McpError::Init(config.id.clone(), e.to_string()))?;

        Ok(Self {
            server_id: config.id.clone(),
            service,
            tools_changed,
        })
    }

    /// The id of the server this client is connected to.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Enumerate the server's tools, stamped with this server's id. Mapped into
    /// `ff-core::McpToolInfo` so callers never touch `rmcp` types.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let result = self
            .service
            .list_tools(Default::default())
            .await
            .map_err(|e| McpError::Protocol(format!("list_tools: {e}")))?;

        Ok(result
            .tools
            .into_iter()
            .map(|tool| McpToolInfo {
                server: self.server_id.clone(),
                name: tool.name.to_string(),
                description: tool.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
            })
            .collect())
    }

    /// Call a tool by its bare name with a JSON object of arguments, returning the
    /// collected text content the model will see (RFC 0003 §6).
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, McpError> {
        let params = match arguments {
            serde_json::Value::Null => CallToolRequestParams::new(name.to_string()),
            serde_json::Value::Object(map) => {
                CallToolRequestParams::new(name.to_string()).with_arguments(map)
            }
            _ => return Err(McpError::BadArguments),
        };

        let result = self
            .service
            .call_tool(params)
            .await
            .map_err(|e| McpError::Protocol(format!("call_tool {name}: {e}")))?;

        let mut text = String::new();
        for content in &result.content {
            if let Some(block) = content.as_text() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&block.text);
            }
        }
        Ok(text)
    }

    /// Whether the server has signalled `tools/list_changed` since the last check.
    /// Reading clears the flag, so a caller polls then re-`list_tools` on `true`.
    pub fn take_tools_changed(&self) -> bool {
        self.tools_changed.swap(false, Ordering::SeqCst)
    }

    /// Gracefully end the connection (and the child process). Full lifecycle
    /// supervision — SIGTERM/SIGKILL fallbacks, reaping — is M4.2; this is the clean
    /// path used on a normal close.
    pub async fn shutdown(self) -> Result<(), McpError> {
        self.service
            .cancel()
            .await
            .map_err(|e| McpError::Protocol(format!("shutdown: {e}")))?;
        Ok(())
    }
}
