//! MCP host (`docs/rfcs/0003-mcp-host.md`).
//!
//! - **M4.0** — [`McpClient`]: a JSON-RPC 2.0 client over a child process's stdio
//!   (`initialize` / `list_tools` / `call_tool` / `tools/list_changed`), built on `rmcp`.
//! - **M4.1** — config: [`load`](config::load) parses `~/.flowforge/mcp.json`,
//!   [`McpConfigWatcher`] hot-reloads it, and [`reconcile`] diffs the desired set against
//!   the running set. This layer keeps the desired config current but spawns nothing.
//! - **M4.2** — the supervisor (lifecycle/health/restart) consumes the reconcile actions.
//! - **M4.3** — the `ToolRegistry` bridge.

mod backoff;
mod client;
mod config;
mod error;
mod reconcile;
mod supervisor;
mod watch;

use std::path::PathBuf;

pub use backoff::Backoff;
pub use client::McpClient;
pub use config::load;
pub use error::McpError;
pub use reconcile::{reconcile, ReconcileAction};
pub use supervisor::{spawn as spawn_supervisor, SharedStatus, SupervisorConfig, SupervisorHandle};
pub use watch::{McpConfigWatcher, SharedConfig};

/// Path to the MCP host config, `~/.flowforge/mcp.json`. `None` if the home directory
/// cannot be resolved.
pub fn config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|d| d.join(".flowforge").join("mcp.json"))
}
