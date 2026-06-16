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

use ff_core::McpServerConfig;
use serde::Deserialize;

use crate::error::McpError;

/// Top-level `mcp.json` document.
#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: BTreeMap<String, RawServerEntry>,
}

/// One server entry as written under `mcpServers` — the id is the surrounding map key,
/// so it is absent here and folded in by [`load`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServerEntry {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    disabled: bool,
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
}
