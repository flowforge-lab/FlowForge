use super::*;
use std::fs;

fn write(dir: &Path, name: &str, body: &str) {
    fs::write(dir.join(name), body).unwrap();
}

#[test]
fn default_has_no_skills_or_overrides() {
    let d = default_phenotype();
    assert_eq!(d.name, DEFAULT_PHENOTYPE);
    assert!(d.skills.is_empty());
    assert!(d.model.is_none());
    assert!(d.persona.is_none());
}

#[test]
fn loads_valid_phenotype() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rust.toml",
        "skills = [\"cargo-help\", \"clippy\"]\nmodel = \"qwen3-coder\"\npersona = \"You are a Rust expert.\"\n",
    );
    let (map, errs) = load_phenotypes(dir.path());
    assert!(errs.is_empty(), "{errs:?}");
    let p = map.get("rust").expect("rust phenotype");
    assert_eq!(p.name, "rust");
    assert_eq!(p.skills, vec!["cargo-help", "clippy"]);
    assert_eq!(p.model.as_deref(), Some("qwen3-coder"));
    assert_eq!(p.persona.as_deref(), Some("You are a Rust expert."));
}

#[test]
fn loads_max_iterations_override() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "codon.toml",
        "skills = []
max_iterations = 40
",
    );
    write(
        dir.path(),
        "plain.toml",
        "skills = []
",
    );
    let (map, errs) = load_phenotypes(dir.path());
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(map["codon"].max_iterations, Some(40));
    assert_eq!(map["plain"].max_iterations, None);
}

#[test]
fn loads_mcp_servers_from_toml() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "codon.toml",
        "skills = [\"codegraph\"]\n\n[[mcp_servers]]\nid = \"codegraph\"\ncommand = \"codegraph\"\nargs = [\"serve\", \"--mcp\"]\nscope = \"workspace\"\n",
    );
    let (map, errs) = load_phenotypes(dir.path());
    assert!(errs.is_empty(), "{errs:?}");
    let p = map.get("codon").expect("codon phenotype");
    assert_eq!(p.mcp_servers.len(), 1);
    let srv = &p.mcp_servers[0];
    assert_eq!(srv.id, "codegraph");
    assert_eq!(srv.command, "codegraph");
    assert_eq!(srv.args, vec!["serve", "--mcp"]);
    assert_eq!(srv.scope, ff_core::McpScope::Workspace);
    // A workspace-scoped server must never pin --path (RFC 0018 section 4.4).
    assert!(!srv.args.iter().any(|a| a == "--path"));
}

#[test]
fn name_comes_from_filename_not_toml() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "writing.toml", "skills = []\n");
    let (map, errs) = load_phenotypes(dir.path());
    assert!(errs.is_empty(), "{errs:?}");
    assert!(map.contains_key("writing"));
    assert_eq!(map["writing"].name, "writing");
}

#[test]
fn malformed_file_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "good.toml", "skills = []\n");
    write(dir.path(), "bad.toml", "skills = \"not a list\"\n");
    let (map, errs) = load_phenotypes(dir.path());
    assert!(map.contains_key("good"));
    assert!(!map.contains_key("bad"));
    assert_eq!(errs.len(), 1);
}

#[test]
fn missing_dir_is_empty_not_error() {
    let dir = tempfile::tempdir().unwrap();
    let (map, errs) = load_phenotypes(&dir.path().join("nope"));
    assert!(map.is_empty());
    assert!(errs.is_empty());
}

#[test]
fn non_toml_files_ignored() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "notes.md", "not a phenotype");
    write(dir.path(), "rust.toml", "skills = []\n");
    let (map, _) = load_phenotypes(dir.path());
    assert_eq!(map.len(), 1);
    assert!(map.contains_key("rust"));
}

fn pheno(name: &str) -> Phenotype {
    Phenotype {
        name: name.to_string(),
        skills: vec!["cargo-help".into()],
        model: Some("qwen3-coder".into()),
        persona: Some("Rust expert.".into()),
        max_iterations: Some(40),
        provider: Some("local-ollama".into()),
        mcp_servers: vec![McpServerConfig {
            id: "codegraph".into(),
            command: "codegraph".into(),
            args: vec!["serve".into(), "--mcp".into()],
            env: Default::default(),
            disabled: false,
            scope: ff_core::McpScope::Workspace,
            reaches_network: None,
            defer: None,
        }],
        egress: ff_core::Egress::Open,
    }
}

#[test]
fn save_round_trips_via_load() {
    let dir = tempfile::tempdir().unwrap();
    save_phenotype(dir.path(), &pheno("rust")).unwrap();
    let (map, errs) = load_phenotypes(dir.path());
    assert!(errs.is_empty(), "{errs:?}");
    let p = map.get("rust").expect("rust phenotype");
    assert_eq!(p, &pheno("rust"));
}

#[test]
fn save_omits_name_and_none_fields() {
    let dir = tempfile::tempdir().unwrap();
    let bare = Phenotype {
        name: "bare".into(),
        skills: Vec::new(),
        model: None,
        persona: None,
        max_iterations: None,
        provider: None,
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    };
    save_phenotype(dir.path(), &bare).unwrap();
    let body = fs::read_to_string(dir.path().join("bare.toml")).unwrap();
    assert!(!body.contains("name"), "name must not be written: {body:?}");
    assert!(!body.contains("model"), "None fields omitted: {body:?}");
    assert!(!body.contains("provider"), "None fields omitted: {body:?}");
    assert!(
        !body.contains("egress"),
        "egress omitted when Open (default): {body:?}"
    );
}

#[test]
fn save_round_trips_local_only_egress() {
    // Regression guard (RFC 0013 Phase 0): the write path used to drop `egress`,
    // so editing+saving a LocalOnly phenotype silently reset it to Open. Round-trip
    // a restricted phenotype and assert the policy survives save->load.
    let dir = tempfile::tempdir().unwrap();
    let enclave = Phenotype {
        name: "enclave".into(),
        skills: Vec::new(),
        model: None,
        persona: Some("local only".into()),
        max_iterations: Some(25),
        provider: None,
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::LocalOnly,
    };
    save_phenotype(dir.path(), &enclave).unwrap();
    let body = fs::read_to_string(dir.path().join("enclave.toml")).unwrap();
    assert!(
        body.contains("egress"),
        "LocalOnly egress must be written: {body:?}"
    );
    let (map, errs) = load_phenotypes(dir.path());
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(
        map.get("enclave").expect("enclave phenotype").egress,
        ff_core::Egress::LocalOnly,
        "egress must survive the save->load round-trip"
    );
}

#[test]
fn save_refuses_default() {
    let dir = tempfile::tempdir().unwrap();
    let err = save_phenotype(dir.path(), &default_phenotype()).unwrap_err();
    assert!(matches!(err, PhenotypeError::Immutable { .. }));
    assert!(!dir.path().join("default.toml").exists());
}

#[test]
fn save_rejects_unsafe_names() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["../evil", "a/b", "", ".hidden", "a\\b"] {
        let err = save_phenotype(dir.path(), &pheno(bad)).unwrap_err();
        assert!(
            matches!(err, PhenotypeError::InvalidName { .. }),
            "{bad:?} should be rejected, got {err:?}"
        );
    }
}

#[test]
fn save_overwrites_existing() {
    let dir = tempfile::tempdir().unwrap();
    save_phenotype(dir.path(), &pheno("rust")).unwrap();
    let mut updated = pheno("rust");
    updated.model = Some("qwen3-max".into());
    save_phenotype(dir.path(), &updated).unwrap();
    let (map, _) = load_phenotypes(dir.path());
    assert_eq!(map["rust"].model.as_deref(), Some("qwen3-max"));
}

#[test]
fn save_leaves_no_tmp_file() {
    let dir = tempfile::tempdir().unwrap();
    save_phenotype(dir.path(), &pheno("rust")).unwrap();
    assert!(!dir.path().join("rust.toml.tmp").exists());
    assert!(dir.path().join("rust.toml").exists());
}

#[test]
fn save_creates_missing_root_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("phenos");
    save_phenotype(&root, &pheno("rust")).unwrap();
    assert!(root.join("rust.toml").exists());
}
