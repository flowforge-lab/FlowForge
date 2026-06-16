use thiserror::Error;

/// A small, owned error surface so callers (and the future supervisor) never have to
/// match on `rmcp`'s internal error enums — the underlying cause is rendered into the
/// message string.
#[derive(Debug, Error)]
pub enum McpError {
    /// The child process could not be spawned (bad command, missing binary).
    #[error("failed to spawn MCP server '{0}': {1}")]
    Spawn(String, String),
    /// The `initialize` handshake failed.
    #[error("MCP server '{0}' failed to initialize: {1}")]
    Init(String, String),
    /// A request (`list_tools` / `call_tool`) or shutdown failed.
    #[error("MCP protocol error: {0}")]
    Protocol(String),
    /// A tool was called with arguments that are not a JSON object.
    #[error("tool arguments must be a JSON object")]
    BadArguments,
    /// `mcp.json` could not be read, parsed, or watched (M4.1).
    #[error("MCP config error: {0}")]
    Config(String),
    /// A `${env:VAR}` reference in `mcp.json` had no matching process-environment
    /// variable. We fail closed rather than spawn with a missing secret (M4.1).
    #[error("server '{server}': environment variable '{var}' referenced in mcp.json is not set")]
    MissingEnvVar { server: String, var: String },
}
