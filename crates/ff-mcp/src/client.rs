//! The client wrapper: one connected MCP server, exposing handshake / list / call in
//! terms of `ff-core` types rather than `rmcp` internals.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ff_core::{McpServerConfig, McpToolInfo};
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, ListRootsResult, Root,
};
use rmcp::service::{NotificationContext, RequestContext, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, ErrorData as RmcpErrorData, RoleClient, ServiceExt};
use tokio::process::Command;

use crate::error::McpError;

/// The client-side `ClientHandler` for a supervised MCP server connection. It does
/// two jobs:
///
/// 1. Flips a flag when the server announces `tools/list_changed`, so the caller knows
///    to re-`list_tools` (RFC 0003 §4).
/// 2. Advertises the connection's **workspace roots** (RFC 0018 §4.4): a
///    `Workspace`-scoped server (e.g. codegraph) learns the active checkout from the
///    `roots`/`rootUri` capability rather than a thrashed process cwd. `get_info`
///    declares the `roots` capability and `list_roots` returns the resolved roots set
///    at connect. A `Global`-scoped server passes an empty roots list and behaves as
///    before. Every other notification keeps the trait default (ignored).
#[derive(Clone, Default)]
struct FfClientHandler {
    tools_changed: Arc<AtomicBool>,
    roots: Arc<Vec<Root>>,
}

impl ClientHandler for FfClientHandler {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.tools_changed.store(true, Ordering::SeqCst);
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, RmcpErrorData> {
        Ok(ListRootsResult::new((*self.roots).clone()))
    }

    fn get_info(&self) -> ClientInfo {
        ClientInfo::new(
            ClientCapabilities::builder().enable_roots().build(),
            Implementation::new("flowforge", env!("CARGO_PKG_VERSION")),
        )
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

/// Resolve a bare MCP `command` to an absolute path honoring `PATHEXT`, so Windows cmd
/// shims (`npx.cmd`, `uvx.exe`, `pnpm.cmd`) resolve. Rust's `Command` only appends
/// `.exe` and never consults `PATHEXT`, so a bare `npx` fails "program not found" and
/// every documented MCP config is dead on Windows (#596). Spawning the resolved
/// absolute path directly (rather than wrapping in `cmd /C`) keeps the child PID equal
/// to the real server -- the supervisor surfaces and reaps by that PID -- and, when the
/// path ends in `.cmd`/`.bat`, Rust std (>= 1.77) auto-escapes the args for `cmd.exe`.
///
/// Returns `None` to fall through to spawning the raw command when it already carries a
/// path separator or extension, or is not found on the search path -- letting the real
/// spawn error surface (the #573 loud-failure path) instead of masking it.
///
/// Host-agnostic (takes `path`/`pathext` as parameters and checks `is_file`) so it
/// unit-tests on the non-Windows dev host and the `windows-check` CI leg alike; only
/// the `connect` call site is `#[cfg(windows)]`.
#[cfg_attr(not(windows), allow(dead_code))]
fn resolve_via_pathext(command: &str, path: &str, pathext: &str) -> Option<PathBuf> {
    // Already qualified (absolute, or carries a separator/extension): let `Command`
    // resolve it as-is so we never second-guess an explicit path.
    if Path::new(command).is_absolute()
        || command.contains('/')
        || command.contains('\\')
        || Path::new(command).extension().is_some()
    {
        return None;
    }
    let exts: Vec<&str> = pathext.split(';').filter(|e| !e.is_empty()).collect();
    for dir in std::env::split_paths(path) {
        for ext in &exts {
            // PATHEXT entries include the leading dot (e.g. ".CMD").
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
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

/// Map workspace root paths to MCP [`Root`]s with `file://` URIs (RFC 0018 §4.4),
/// the channel a workspace-aware server reads its checkout from.
fn root_uris(roots: &[&Path]) -> Vec<Root> {
    roots
        .iter()
        .map(|p| Root::new(format!("file://{}", p.display())))
        .collect()
}

/// A live connection to one MCP server. Dropping or `shutdown`-ing it ends the child.
pub struct McpClient {
    server_id: String,
    service: RunningService<RoleClient, FfClientHandler>,
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
    ///
    /// `roots` are advertised to the server via the MCP roots capability (RFC 0018
    /// §4.4) so a workspace-aware server learns its checkout from `rootUri` rather than
    /// a mutable process cwd. `cwd` is still set as a belt-and-braces fallback. Pass an
    /// empty `roots` slice for a `Global`-scoped server.
    pub async fn connect(
        config: &McpServerConfig,
        env_allowlist: &[&str],
        cwd: Option<&Path>,
        roots: &[&Path],
    ) -> Result<Self, McpError> {
        // On Windows, resolve cmd shims (npx.cmd, uvx.exe, ...) via PATHEXT to an
        // absolute path so a bare `command` spawns at all (#596); on every other
        // platform this is byte-identical to `Command::new(&config.command)`.
        let program: OsString = {
            #[cfg(windows)]
            {
                let path = std::env::var("PATH").unwrap_or_default();
                let pathext =
                    std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
                resolve_via_pathext(&config.command, &path, &pathext)
                    .map(Into::into)
                    .unwrap_or_else(|| config.command.clone().into())
            }
            #[cfg(not(windows))]
            {
                config.command.clone().into()
            }
        };
        let mut cmd = Command::new(&program);
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
        let handler = FfClientHandler {
            tools_changed: Arc::new(AtomicBool::new(false)),
            roots: Arc::new(root_uris(roots)),
        };
        let tools_changed = handler.tools_changed.clone();
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

    // #596 — PATHEXT resolution: a bare `npx`/`uvx`/`pnpm` must resolve to its cmd
    // shim, which Rust's `Command` never finds on Windows (it only appends `.exe`).
    // Host-agnostic: synthetic files + a joined PATH, so it runs on the dev host too.
    fn join(dirs: &[&std::path::Path]) -> String {
        std::env::join_paths(dirs.iter().map(|d| d.as_os_str()))
            .unwrap()
            .into_string()
            .unwrap()
    }

    // PATHEXT casing here matches the synthetic file casing so the test is
    // deterministic on a case-sensitive FS (Linux CI) as well as a case-insensitive
    // one (macOS/Windows). On real Windows the OS resolves `is_file()`
    // case-insensitively, which is exactly what makes a lowercase `npx.cmd` match an
    // uppercase `.CMD` PATHEXT entry -- an OS concern, not this function's job.
    #[test]
    fn resolve_via_pathext_finds_a_cmd_shim() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("npx.cmd");
        std::fs::write(&shim, b"").unwrap();
        let path = join(&["/no/such/dir".as_ref(), dir.path()]);
        assert_eq!(resolve_via_pathext("npx", &path, ".exe;.cmd"), Some(shim),);
    }

    #[test]
    fn resolve_via_pathext_respects_pathext_then_path_order() {
        // .exe precedes .cmd in PATHEXT, and the first PATH dir wins. A dir holding
        // both npx.cmd and npx.exe must yield the .exe (earlier PATHEXT entry).
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("npx.exe");
        std::fs::write(&exe, b"").unwrap();
        std::fs::write(dir.path().join("npx.cmd"), b"").unwrap();
        let path = join(&[dir.path()]);
        assert_eq!(resolve_via_pathext("npx", &path, ".exe;.cmd"), Some(exe),);
    }

    #[test]
    fn resolve_via_pathext_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = join(&[dir.path()]);
        assert!(resolve_via_pathext("npx", &path, ".EXE;.CMD").is_none());
    }

    #[test]
    fn resolve_via_pathext_passes_through_qualified_commands() {
        // Absolute paths, anything carrying a separator, or an explicit extension are
        // left for `Command` to resolve as-is — we never second-guess them.
        let path = "/nope";
        assert!(resolve_via_pathext("/usr/bin/node", path, ".EXE;.CMD").is_none());
        assert!(resolve_via_pathext("./local/tool", path, ".EXE;.CMD").is_none());
        assert!(resolve_via_pathext("dir\\tool", path, ".EXE;.CMD").is_none());
        assert!(resolve_via_pathext("server.js", path, ".EXE;.CMD").is_none());
    }

    #[test]
    fn resolve_via_pathext_handles_empty_path_and_pathext() {
        assert!(resolve_via_pathext("npx", "", ".EXE;.CMD").is_none());
        assert!(resolve_via_pathext("npx", "/some/dir", "").is_none());
    }

    #[test]
    fn root_uris_maps_paths_to_file_uris() {
        let roots = root_uris(&[Path::new("/work/A"), Path::new("/work/B")]);
        assert_eq!(
            roots.iter().map(|r| r.uri.clone()).collect::<Vec<_>>(),
            vec!["file:///work/A".to_string(), "file:///work/B".to_string()],
        );
    }

    #[test]
    fn root_uris_empty_for_global() {
        assert!(root_uris(&[]).is_empty());
    }

    #[tokio::test]
    async fn handler_advertises_roots_and_lists_them() {
        let handler = FfClientHandler {
            tools_changed: Arc::new(AtomicBool::new(false)),
            roots: Arc::new(root_uris(&[Path::new("/work/A")])),
        };
        // The client must declare the roots capability so a server knows to query it.
        assert!(handler.get_info().capabilities.roots.is_some());
        // And the stored roots are exactly what was resolved at connect.
        assert_eq!(
            handler
                .roots
                .iter()
                .map(|r| r.uri.clone())
                .collect::<Vec<_>>(),
            vec!["file:///work/A".to_string()],
        );
    }
}
