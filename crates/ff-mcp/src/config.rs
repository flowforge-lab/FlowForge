//! Loading and validating `~/.flowforge/mcp.json` (RFC 0003 §3).
//!
//! The on-disk shape is the de-facto Claude/Cursor `mcpServers` map, where each
//! server's id is the *map key* (not a field):
//!
//! ```json
//! { "mcpServers": { "github": { "command": "github-mcp-server",
//!                                "env": { "GITHUB_TOKEN": "${env:GITHUB_TOKEN}" } } } }
//! ```
//!
//! [`load`] parses that into the flat [`McpServerConfig`] list the rest of the host
//! works with, folding each key into `id` and resolving `${env:VAR}` references from
//! the process environment so secrets never live in the config file (RFC 0003 §9).
//! A missing referenced variable is a hard error: we fail closed rather than spawn a
//! server with a half-populated environment.

use std::collections::BTreeMap;
use std::path::Path;

use ff_core::{McpScope, McpServerConfig};
use serde::{Deserialize, Serialize};

use crate::error::McpError;

/// Top-level `mcp.json` document.
#[derive(Debug, Default, Deserialize, Serialize)]
struct RawConfig {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: BTreeMap<String, RawServerEntry>,
}

/// One server entry as written under `mcpServers` — the id is the surrounding map key,
/// so it is absent here and folded in by [`load`].
///
/// On write-back the optional fields are omitted when empty/false so a managed
/// rewrite stays close to the hand-authored Claude/Cursor shape.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawServerEntry {
    command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_false")]
    disabled: bool,
    #[serde(default, skip_serializing_if = "McpScope::is_global")]
    scope: McpScope,
}

/// `skip_serializing_if` predicate: omit `disabled` when it is the default `false`.
fn is_false(b: &bool) -> bool {
    !*b
}

/// A raw, **un-resolved** server definition supplied by the user (the add/edit form).
///
/// Deliberately distinct from [`McpServerConfig`], which [`load`] fills with `${env:}`
/// references already **resolved** to real secrets. Write-back ([`upsert`]) accepts only
/// this type, so a resolved config can never be passed in and baked back into
/// `mcp.json` by accident — the leak is rejected at compile time, not caught by review.
/// `env` values are stored verbatim (literal strings or hand-written `${env:}`
/// templates); no resolution happens here.
#[derive(Debug, Clone)]
pub struct McpServerInput {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub disabled: bool,
    pub scope: McpScope,
}

/// Parse and validate `mcp.json` at `path`, returning the server set sorted by id.
///
/// A missing file is treated as an empty config (no servers) rather than an error, so
/// the host runs fine before the user has written one.
pub fn load(path: &Path) -> Result<Vec<McpServerConfig>, McpError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(McpError::Config(format!("reading {}: {e}", path.display()))),
    };
    parse(&text, &resolve_from_process_env)
}

/// Enable or disable one server by id, leaving every other entry — and the targeted
/// entry's `${env:...}` templates — byte-for-byte intact.
///
/// Errors if the id is not present, so a stale UI cannot silently no-op.
pub fn set_disabled(path: &Path, id: &str, disabled: bool) -> Result<(), McpError> {
    let mut raw = read_raw(path)?;
    match raw.mcp_servers.get_mut(id) {
        Some(entry) => entry.disabled = disabled,
        None => {
            return Err(McpError::Config(format!(
                "no MCP server '{id}' in {}",
                path.display()
            )))
        }
    }
    write_raw(path, &raw)
}

/// Add a new server definition or replace an existing one with the same id.
///
/// The entry is written verbatim from `def` — the caller (UI add-form) supplies the
/// literal `command`/`args`/`env` it wants on disk. `${env:...}` strings are stored
/// as-is and resolved later by [`load`]; no secret injection happens here.
pub fn upsert(path: &Path, def: &McpServerInput) -> Result<(), McpError> {
    let mut raw = read_raw(path)?;
    raw.mcp_servers.insert(
        def.id.clone(),
        RawServerEntry {
            command: def.command.clone(),
            args: def.args.clone(),
            env: def.env.clone(),
            disabled: def.disabled,
            scope: def.scope,
        },
    );
    write_raw(path, &raw)
}

/// Remove a server definition by id. A no-op (still `Ok`) if the id is absent, so a
/// double-remove from the UI is harmless.
pub fn remove(path: &Path, id: &str) -> Result<(), McpError> {
    let mut raw = read_raw(path)?;
    if raw.mcp_servers.remove(id).is_some() {
        write_raw(path, &raw)?;
    }
    Ok(())
}

/// Read the document **without** resolving `${env:...}` references, so write-back
/// round-trips the raw templates instead of baking resolved secrets back into the
/// file. A missing file is an empty document (the file is created on first write).
fn read_raw(path: &Path) -> Result<RawConfig, McpError> {
    match std::fs::read_to_string(path) {
        Ok(t) => {
            serde_json::from_str(&t).map_err(|e| McpError::Config(format!("invalid mcp.json: {e}")))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RawConfig::default()),
        Err(e) => Err(McpError::Config(format!("reading {}: {e}", path.display()))),
    }
}

/// Serialize the raw document back to `path` (pretty, trailing newline), creating the
/// parent directory if needed. Matches the plain-`fs::write` persistence convention
/// used for the other `~/.flowforge` config files.
fn write_raw(path: &Path, raw: &RawConfig) -> Result<(), McpError> {
    let mut text = serde_json::to_string_pretty(raw)
        .map_err(|e| McpError::Config(format!("serializing mcp.json: {e}")))?;
    text.push('\n');
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| McpError::Config(format!("creating {}: {e}", parent.display())))?;
    }
    std::fs::write(path, text)
        .map_err(|e| McpError::Config(format!("writing {}: {e}", path.display())))
}

/// Resolve a `${env:VAR}` reference from the real process environment.
fn resolve_from_process_env(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

/// Parse `mcp.json` text with an injectable env resolver (so tests need not mutate the
/// real process environment, which is racy under a parallel test runner).
fn parse(
    text: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<McpServerConfig>, McpError> {
    let raw: RawConfig = serde_json::from_str(text)
        .map_err(|e| McpError::Config(format!("invalid mcp.json: {e}")))?;

    let mut servers = Vec::with_capacity(raw.mcp_servers.len());
    for (id, entry) in raw.mcp_servers {
        let command = interpolate(&entry.command, &id, resolve)?;
        let args = entry
            .args
            .iter()
            .map(|a| interpolate(a, &id, resolve))
            .collect::<Result<Vec<_>, _>>()?;
        let mut env = BTreeMap::new();
        for (k, v) in &entry.env {
            env.insert(k.clone(), interpolate(v, &id, resolve)?);
        }
        servers.push(McpServerConfig {
            id,
            command,
            args,
            env,
            disabled: entry.disabled,
            scope: entry.scope,
        });
    }
    // BTreeMap iteration is already id-sorted; keep that contract explicit.
    servers.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(servers)
}

/// Replace every `${env:VAR}` occurrence in `input` with the resolved variable. An
/// unresolved reference is a hard error (fail closed). `server_id` is only for context
/// in the error message.
fn interpolate(
    input: &str,
    server_id: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<String, McpError> {
    const OPEN: &str = "${env:";
    if !input.contains(OPEN) {
        return Ok(input.to_string());
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let end = after.find('}').ok_or_else(|| {
            McpError::Config(format!(
                "server '{server_id}': unterminated '${{env:...}}' reference"
            ))
        })?;
        let var = &after[..end];
        let value = resolve(var).ok_or_else(|| McpError::MissingEnvVar {
            server: server_id.to_string(),
            var: var.to_string(),
        })?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Replace `${workspace}`/`${root}` placeholders in a resolved server config with
/// the session's canonical checkout `root` (#544). Applied at connect for a
/// `Workspace`-scoped instance, *after* load-time `${env:}` resolution -- the root
/// is unknown until a session references the server, so it cannot be resolved in
/// [`load`]. A no-op when `root` is `None` (a `Global` instance has no checkout):
/// the placeholder is left intact rather than blanked, so a misuse is visible.
pub fn substitute_workspace(mut cfg: McpServerConfig, root: Option<&Path>) -> McpServerConfig {
    let Some(root) = root else {
        return cfg;
    };
    let path = root.to_string_lossy();
    let sub = |s: &str| {
        s.replace("${workspace}", path.as_ref())
            .replace("${root}", path.as_ref())
    };
    cfg.command = sub(&cfg.command);
    for a in &mut cfg.args {
        *a = sub(a);
    }
    for v in cfg.env.values_mut() {
        *v = sub(v);
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn map_env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |v| {
            pairs
                .iter()
                .find(|(k, _)| *k == v)
                .map(|(_, val)| val.to_string())
        }
    }

    #[test]
    fn parses_valid_config_with_defaults() {
        let text = r#"{
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/work"]
                },
                "github": { "command": "github-mcp-server", "disabled": true }
            }
        }"#;
        let servers = parse(text, &no_env).unwrap();
        assert_eq!(servers.len(), 2);
        // Sorted by id.
        assert_eq!(servers[0].id, "filesystem");
        assert_eq!(servers[0].args.len(), 3);
        assert!(!servers[0].disabled);
        assert_eq!(servers[1].id, "github");
        assert!(servers[1].disabled);
        assert!(servers[1].args.is_empty());
    }

    #[test]
    fn empty_or_missing_servers_key_is_empty() {
        assert!(parse("{}", &no_env).unwrap().is_empty());
        assert!(parse(r#"{"mcpServers":{}}"#, &no_env).unwrap().is_empty());
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(matches!(
            parse("{ not json", &no_env),
            Err(McpError::Config(_))
        ));
    }

    #[test]
    fn rejects_unknown_fields() {
        let text = r#"{"mcpServers":{"x":{"command":"c","bogus":1}}}"#;
        assert!(matches!(parse(text, &no_env), Err(McpError::Config(_))));
    }

    #[test]
    fn substitute_workspace_resolves_placeholders_in_command_args_env() {
        let cfg = McpServerConfig {
            id: "codegraph".into(),
            command: "${workspace}/bin/cg".into(),
            args: vec!["serve".into(), "--path".into(), "${workspace}".into()],
            env: {
                let mut m = BTreeMap::new();
                m.insert("DB".into(), "${root}/.cg/db".into());
                m
            },
            disabled: false,
            scope: McpScope::Workspace,
        };
        let out = substitute_workspace(cfg, Some(Path::new("/Users/me/projects/repo")));
        assert_eq!(out.command, "/Users/me/projects/repo/bin/cg");
        assert_eq!(out.args[2], "/Users/me/projects/repo");
        assert_eq!(out.env["DB"], "/Users/me/projects/repo/.cg/db");
    }

    #[test]
    fn substitute_workspace_is_noop_and_keeps_placeholder_without_root() {
        let cfg = McpServerConfig {
            id: "codegraph".into(),
            command: "cg".into(),
            args: vec!["--path".into(), "${workspace}".into()],
            env: BTreeMap::new(),
            disabled: false,
            scope: McpScope::Global,
        };
        let out = substitute_workspace(cfg, None);
        assert_eq!(out.args[1], "${workspace}");
    }

    #[test]
    fn rejects_missing_command() {
        let text = r#"{"mcpServers":{"x":{"args":[]}}}"#;
        assert!(matches!(parse(text, &no_env), Err(McpError::Config(_))));
    }

    #[test]
    fn interpolates_env_in_values_and_args() {
        let text = r#"{
            "mcpServers": {
                "github": {
                    "command": "gh",
                    "args": ["--root", "${env:WORK}/repo"],
                    "env": { "GITHUB_TOKEN": "${env:GH_TOKEN}" }
                }
            }
        }"#;
        let env = map_env(&[("WORK", "/home/me"), ("GH_TOKEN", "secret")]);
        let servers = parse(text, &env).unwrap();
        assert_eq!(servers[0].args[1], "/home/me/repo");
        assert_eq!(servers[0].env["GITHUB_TOKEN"], "secret");
    }

    #[test]
    fn missing_env_var_fails_closed() {
        let text = r#"{"mcpServers":{"github":{"command":"gh","env":{"T":"${env:NOPE}"}}}}"#;
        match parse(text, &no_env) {
            Err(McpError::MissingEnvVar { server, var }) => {
                assert_eq!(server, "github");
                assert_eq!(var, "NOPE");
            }
            other => panic!("expected MissingEnvVar, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_reference_errors() {
        let text = r#"{"mcpServers":{"x":{"command":"${env:UNCLOSED"}}}"#;
        assert!(matches!(parse(text, &no_env), Err(McpError::Config(_))));
    }

    #[test]
    fn multiple_references_in_one_value() {
        let text = r#"{"mcpServers":{"x":{"command":"c","env":{"U":"${env:A}-${env:B}"}}}}"#;
        let env = map_env(&[("A", "1"), ("B", "2")]);
        let servers = parse(text, &env).unwrap();
        assert_eq!(servers[0].env["U"], "1-2");
    }

    use tempfile::tempdir;

    const WITH_SECRET: &str = r#"{
        "mcpServers": {
            "github": {
                "command": "github-mcp-server",
                "env": { "GITHUB_TOKEN": "${env:GITHUB_TOKEN}" }
            }
        }
    }"#;

    #[test]
    fn set_disabled_preserves_env_templates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, WITH_SECRET).unwrap();

        set_disabled(&path, "github", true).unwrap();

        // The raw file must still hold the un-resolved template, never the secret.
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("${env:GITHUB_TOKEN}"));
        let raw: RawConfig = serde_json::from_str(&written).unwrap();
        assert!(raw.mcp_servers["github"].disabled);
        assert_eq!(
            raw.mcp_servers["github"].env["GITHUB_TOKEN"],
            "${env:GITHUB_TOKEN}"
        );
    }

    #[test]
    fn set_disabled_unknown_id_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, WITH_SECRET).unwrap();
        assert!(matches!(
            set_disabled(&path, "nope", true),
            Err(McpError::Config(_))
        ));
    }

    #[test]
    fn scope_parses_from_json_and_round_trips_through_upsert() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.json");

        // A hand-authored mcp.json with an explicit workspace scope must parse
        // (RawServerEntry uses deny_unknown_fields, so `scope` had to be added).
        std::fs::write(
            &path,
            r#"{"mcpServers":{"cg":{"command":"codegraph","scope":"workspace"}}}"#,
        )
        .unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].scope, McpScope::Workspace);

        // An absent scope still defaults to Global (back-compat).
        std::fs::write(&path, r#"{"mcpServers":{"g":{"command":"echo"}}}"#).unwrap();
        assert_eq!(load(&path).unwrap()[0].scope, McpScope::Global);

        // upsert preserves scope on write-back.
        upsert(
            &path,
            &McpServerInput {
                id: "ws".into(),
                command: "c".into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                disabled: false,
                scope: McpScope::Workspace,
            },
        )
        .unwrap();
        let ws = load(&path)
            .unwrap()
            .into_iter()
            .find(|s| s.id == "ws")
            .unwrap();
        assert_eq!(ws.scope, McpScope::Workspace);
    }

    #[test]
    fn upsert_adds_then_replaces() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.json");

        let mut def = McpServerInput {
            id: "fs".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "server-filesystem".into()],
            env: BTreeMap::new(),
            disabled: false,
            scope: McpScope::Global,
        };
        upsert(&path, &def).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].command, "npx");

        def.command = "node".into();
        def.disabled = true;
        upsert(&path, &def).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].command, "node");
        assert!(loaded[0].disabled);
    }

    #[test]
    fn upsert_on_missing_file_creates_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("mcp.json");
        let def = McpServerInput {
            id: "x".into(),
            command: "c".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            disabled: false,
            scope: McpScope::Global,
        };
        upsert(&path, &def).unwrap();
        assert!(path.exists());
        assert_eq!(load(&path).unwrap().len(), 1);
    }

    #[test]
    fn upsert_stores_env_template_verbatim() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let mut env = BTreeMap::new();
        env.insert(
            "GITHUB_TOKEN".to_string(),
            "${env:GITHUB_TOKEN}".to_string(),
        );
        upsert(
            &path,
            &McpServerInput {
                id: "github".into(),
                command: "github-mcp-server".into(),
                args: Vec::new(),
                env,
                disabled: false,
                scope: McpScope::Global,
            },
        )
        .unwrap();

        // The literal template lands on disk un-resolved; load() resolves it.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("${env:GITHUB_TOKEN}"));
        let resolved = parse(&raw, &map_env(&[("GITHUB_TOKEN", "secret")])).unwrap();
        assert_eq!(resolved[0].env["GITHUB_TOKEN"], "secret");
    }

    #[test]
    fn remove_deletes_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, WITH_SECRET).unwrap();

        remove(&path, "github").unwrap();
        assert!(load(&path).unwrap().is_empty());

        // Second remove of an absent id leaves the file byte-identical.
        let before = std::fs::read_to_string(&path).unwrap();
        remove(&path, "github").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
}
