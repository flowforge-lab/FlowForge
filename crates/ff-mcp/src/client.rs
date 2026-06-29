//! The client wrapper: one connected MCP server, exposing handshake / list / call in
//! terms of `ff-core` types rather than `rmcp` internals.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

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

/// Common user-level bin directories a login shell puts on `PATH` but a GUI app
/// launched from Finder/Dock/launchd does not inherit. Used to augment a child's
/// `PATH` so a bare `command` resolves in a packaged build (#573). Unix-only; other
/// platforms add nothing (Windows resolves via its own search rules).
#[cfg(unix)]
fn extra_path_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/sbin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

#[cfg(not(unix))]
fn extra_path_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// Append `extra` directories to an inherited `PATH`, preserving order and dropping
/// duplicates so the inherited entries keep priority and a dir already present is not
/// repeated. Falls back to the inherited value unchanged if joining fails (e.g. a dir
/// contains the platform path separator).
fn augment_path(inherited: Option<OsString>, extra: &[PathBuf]) -> OsString {
    use std::collections::HashSet;
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut parts: Vec<PathBuf> = Vec::new();
    if let Some(p) = &inherited {
        for dir in std::env::split_paths(p) {
            if seen.insert(dir.clone()) {
                parts.push(dir);
            }
        }
    }
    for dir in extra {
        if seen.insert(dir.clone()) {
            parts.push(dir.clone());
        }
    }
    std::env::join_paths(&parts).unwrap_or_else(|_| inherited.unwrap_or_default())
}

/// A live connection to one MCP server. Dropping or `shutdown`-ing it ends the child.
pub struct McpClient {
    server_id: String,
    service: RunningService<RoleClient, ListChangedFlag>,
    tools_changed: Arc<AtomicBool>,
    pid: Option<u32>,
}

impl McpClient {
    /// Spawn the server described by `config` and complete the `initialize` handshake.
    ///
    /// Env isolation (RFC 0003 §9.2): the child starts from an **empty** environment.
    /// `env_allowlist` names host variables (e.g. `PATH`, `HOME`) that are passed
    /// through when present so a bare `command` resolves; the config's declared `env`
    /// is applied *after* and wins on collision. Nothing outside the allowlist or the
    /// declared keys reaches the child, so a third-party server can't harvest unrelated
    /// host secrets. The supervisor (M4.2) supplies the allowlist; pass `&[]` for a
    /// fully sealed environment (a server then needs an absolute `command` + declared
    /// `env`).
    pub async fn connect(
        config: &McpServerConfig,
        env_allowlist: &[&str],
        cwd: Option<&Path>,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        // Run the child in `cwd` when set, so a workspace-aware server (e.g. codegraph)
        // indexes the active checkout rather than the app's launch directory (#548).
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.env_clear();
        for key in env_allowlist {
            if *key == "PATH" {
                // Augment the inherited PATH with common user bin dirs so a bare
                // `command` (e.g. "codegraph" in ~/.local/bin) still resolves in a
                // packaged GUI build, which inherits only launchd's minimal PATH
                // (/usr/bin:/bin:/usr/sbin:/sbin) rather than the user's login-shell
                // PATH (#573). Additive: the inherited entries keep priority, and a
                // config-declared `env` PATH below still overrides it wholesale.
                cmd.env(
                    "PATH",
                    augment_path(std::env::var_os("PATH"), &extra_path_dirs()),
                );
            } else if let Ok(value) = std::env::var(key) {
                cmd.env(key, value);
            }
        }
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| McpError::Spawn(config.id.clone(), e.to_string()))?;
        // The transport is consumed by `serve`, so capture the child PID first — it
        // surfaces in `McpServerStatus` and lets the supervisor verify reaping.
        let pid = transport.id();
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
            pid,
        })
    }

    /// The OS process id of the server's child, if known. `None` if the platform did
    /// not report one or the child has already been reaped.
    pub fn pid(&self) -> Option<u32> {
        self.pid
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn augment_path_appends_extra_dirs_in_order() {
        let extra = vec![PathBuf::from("/x/bin"), PathBuf::from("/y/bin")];
        let out = augment_path(Some(OsString::from("/usr/bin:/bin")), &extra);
        let dirs: Vec<PathBuf> = std::env::split_paths(&out).collect();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/x/bin"),
                PathBuf::from("/y/bin"),
            ]
        );
    }

    #[test]
    fn augment_path_dedups_dirs_already_inherited() {
        let extra = vec![PathBuf::from("/usr/bin"), PathBuf::from("/x/bin")];
        let out = augment_path(Some(OsString::from("/usr/bin:/bin")), &extra);
        let dirs: Vec<PathBuf> = std::env::split_paths(&out).collect();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/x/bin"),
            ]
        );
    }

    #[test]
    fn augment_path_handles_no_inherited_path() {
        let out = augment_path(None, &[PathBuf::from("/x/bin")]);
        let dirs: Vec<PathBuf> = std::env::split_paths(&out).collect();
        assert_eq!(dirs, vec![PathBuf::from("/x/bin")]);
    }

    // The point of the augmentation: a child spawned with env_clear + a minimal PATH
    // can still resolve a bare command living in one of the augmented dirs (#573).
    #[cfg(unix)]
    #[tokio::test]
    async fn augmented_path_resolves_bare_command_under_env_clear() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("ff-fake-tool");
        std::fs::write(&bin, "#!/bin/sh\necho ok\n").unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();

        let path = augment_path(
            Some(OsString::from("/usr/bin:/bin")),
            &[tmp.path().to_path_buf()],
        );
        let mut cmd = tokio::process::Command::new("ff-fake-tool");
        cmd.env_clear();
        cmd.env("PATH", &path);
        let out = cmd.output().await.unwrap();
        assert!(
            out.status.success(),
            "bare command should resolve via the augmented PATH"
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }
}
