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
        reaches_network: None,
        defer: None,
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
        reaches_network: None,
        defer: None,
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

    std::fs::write(
        &path,
        r#"{"mcpServers":{"cg":{"command":"codegraph","scope":"workspace"}}}"#,
    )
    .unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].scope, McpScope::Workspace);

    std::fs::write(&path, r#"{"mcpServers":{"g":{"command":"echo"}}}"#).unwrap();
    assert_eq!(load(&path).unwrap()[0].scope, McpScope::Global);

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

    let before = std::fs::read_to_string(&path).unwrap();
    remove(&path, "github").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}
