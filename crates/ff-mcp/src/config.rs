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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reaches_network: Option<bool>,
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
            // McpServerInput (the add-server write-back path) does not carry an
            // egress policy yet; a Settings UI to set it is future work. Written
            // configs omit the field (fail-safe network-capable) until then.
            reaches_network: None,
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
            reaches_network: entry.reaches_network,
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
mod tests;
