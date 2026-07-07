use super::*;
use crate::test_support::{lock_mem_store, TestEnv};
use clap::Parser;

fn parse_set(argv: &[&str]) -> SetArgs {
    #[derive(Parser, Debug)]
    #[command(name = "flowforge")]
    struct Wrapper {
        #[command(subcommand)]
        cmd: ConfigCommand,
    }
    let parsed = Wrapper::try_parse_from(argv).expect("parses");
    match parsed.cmd {
        ConfigCommand::Set(args) => args,
        ConfigCommand::List => panic!("expected Set, got List"),
    }
}

#[test]
fn validate_local_provider_is_rejected() {
    for kind in [ProviderKind::CandleVllm, ProviderKind::Ollama] {
        assert!(validate_target(kind, SecretKind::ApiKey).is_err());
    }
}

#[test]
fn validate_secret_matrix_matches_spec() {
    assert!(validate_target(ProviderKind::Bedrock, SecretKind::ApiKey).is_ok());
    assert!(validate_target(ProviderKind::Bedrock, SecretKind::SecretAccessKey).is_ok());
    assert!(validate_target(ProviderKind::Bedrock, SecretKind::SessionToken).is_ok());
    assert!(validate_target(ProviderKind::OpenAi, SecretKind::ApiKey).is_ok());
    assert!(validate_target(ProviderKind::OpenAi, SecretKind::SecretAccessKey).is_err());
    assert!(validate_target(ProviderKind::OpenAi, SecretKind::SessionToken).is_err());
    assert!(validate_target(ProviderKind::SiliconFlow, SecretKind::ApiKey).is_ok());
    assert!(validate_target(ProviderKind::SiliconFlow, SecretKind::SecretAccessKey).is_err());
}

#[test]
fn accepted_secret_kinds_table_is_consistent() {
    assert_eq!(
        accepted_secret_kinds(ProviderKind::Bedrock),
        &SecretKind::ALL
    );
    assert_eq!(
        accepted_secret_kinds(ProviderKind::OpenAi),
        &[SecretKind::ApiKey]
    );
    assert_eq!(
        accepted_secret_kinds(ProviderKind::SiliconFlow),
        &[SecretKind::ApiKey]
    );
    assert!(accepted_secret_kinds(ProviderKind::CandleVllm).is_empty());
    assert!(accepted_secret_kinds(ProviderKind::Ollama).is_empty());
}

#[test]
fn parse_set_with_positional_value() {
    let a = parse_set(&["flowforge", "set", "bedrock", "api-key", "sk-123"]);
    assert!(matches!(a.provider, ProviderKindArg::Bedrock));
    assert!(matches!(a.secret, SecretKindArg::ApiKey));
    assert_eq!(a.value.as_deref(), Some("sk-123"));
    assert!(!a.stdin);
    assert!(!a.clear);
}

#[test]
fn parse_set_with_clear_flag() {
    let a = parse_set(&["flowforge", "set", "openai", "api-key", "--clear"]);
    assert!(matches!(a.provider, ProviderKindArg::OpenAi));
    assert!(a.clear);
    assert!(a.value.is_none());
    assert!(!a.stdin);
}

#[test]
fn parse_set_with_stdin_flag() {
    let a = parse_set(&["flowforge", "set", "siliconflow", "api-key", "--stdin"]);
    assert!(matches!(a.provider, ProviderKindArg::Siliconflow));
    assert!(a.stdin);
    assert!(a.value.is_none());
    assert!(!a.clear);
}

#[test]
fn parse_set_rejects_value_and_clear_together() {
    #[derive(Parser, Debug)]
    #[command(name = "flowforge")]
    struct Wrapper {
        #[command(subcommand)]
        cmd: ConfigCommand,
    }
    let err = Wrapper::try_parse_from([
        "flowforge",
        "set",
        "bedrock",
        "api-key",
        "sk-123",
        "--clear",
    ])
    .expect_err("value + --clear must conflict");
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn parse_set_rejects_stdin_and_clear_together() {
    #[derive(Parser, Debug)]
    #[command(name = "flowforge")]
    struct Wrapper {
        #[command(subcommand)]
        cmd: ConfigCommand,
    }
    let err = Wrapper::try_parse_from([
        "flowforge",
        "set",
        "bedrock",
        "api-key",
        "--stdin",
        "--clear",
    ])
    .expect_err("--stdin + --clear must conflict");
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn parse_list_does_not_require_provider() {
    #[derive(Parser, Debug)]
    #[command(name = "flowforge")]
    struct Wrapper {
        #[command(subcommand)]
        cmd: ConfigCommand,
    }
    Wrapper::try_parse_from(["flowforge", "list"]).expect("list parses without a provider");
}

#[test]
fn parse_set_rejects_unknown_secret_kind() {
    #[derive(Parser, Debug)]
    #[command(name = "flowforge")]
    struct Wrapper {
        #[command(subcommand)]
        cmd: ConfigCommand,
    }
    let err = Wrapper::try_parse_from(["flowforge", "set", "bedrock", "no-such-secret", "sk-123"])
        .expect_err("unknown secret kind must be rejected");
    assert!(
        matches!(
            err.kind(),
            clap::error::ErrorKind::InvalidValue | clap::error::ErrorKind::UnknownArgument
        ),
        "expected InvalidValue or UnknownArgument, got {:?}",
        err.kind()
    );
}

#[test]
fn set_then_list_reflects_has_key() {
    let _lock = lock_mem_store();
    let env = TestEnv::new();
    env.write_registry(&ProviderRegistry::default());

    let args = parse_set(&["flowforge", "set", "bedrock", "api-key", "sk-test"]);
    let code = run_set(&args, Some("sk-test"));
    assert_eq!(code, ExitCode::SUCCESS);

    let reg = registry::load_registry_at(Some(env.registry_path()), Some(env.legacy_path()));
    let conn = reg
        .connections
        .iter()
        .find(|c| c.kind == ProviderKind::Bedrock)
        .expect("auto-created bedrock connection");
    assert!(conn.has_key, "has_key should be flipped to true");
    assert_eq!(
        crate::secrets::get(&conn.id, SecretKind::ApiKey).as_deref(),
        Some("sk-test")
    );
}

#[test]
fn clear_resets_has_key_when_no_other_secrets_remain() {
    let _lock = lock_mem_store();
    let env = TestEnv::new();
    env.write_registry(&ProviderRegistry::default());

    let set_args = parse_set(&["flowforge", "set", "bedrock", "api-key", "sk-test"]);
    run_set(&set_args, Some("sk-test"));
    let clear_args = parse_set(&["flowforge", "set", "bedrock", "api-key", "--clear"]);
    let code = run_set(&clear_args, None);
    assert_eq!(code, ExitCode::SUCCESS);

    let reg = registry::load_registry_at(Some(env.registry_path()), Some(env.legacy_path()));
    let conn = reg
        .connections
        .iter()
        .find(|c| c.kind == ProviderKind::Bedrock)
        .unwrap();
    assert!(!conn.has_key, "has_key should be false after clear");
    assert!(crate::secrets::get(&conn.id, SecretKind::ApiKey).is_none());
}

#[test]
fn set_rejects_invalid_secret_for_provider() {
    let _lock = lock_mem_store();
    let env = TestEnv::new();
    env.write_registry(&ProviderRegistry::default());

    let args = parse_set(&["flowforge", "set", "openai", "session-token", "tok"]);
    let code = run_set(&args, Some("tok"));
    assert_eq!(code, ExitCode::FAILURE);

    let reg = registry::load_registry_at(Some(env.registry_path()), Some(env.legacy_path()));
    assert!(reg
        .connections
        .iter()
        .all(|c| c.kind != ProviderKind::OpenAi));
}

#[test]
fn auto_creates_singleton_connection_for_unseen_kind() {
    let _lock = lock_mem_store();
    let env = TestEnv::new();
    env.write_registry(&ProviderRegistry::default());

    let args = parse_set(&["flowforge", "set", "siliconflow", "api-key", "sk-sf"]);
    let code = run_set(&args, Some("sk-sf"));
    assert_eq!(code, ExitCode::SUCCESS);

    let reg = registry::load_registry_at(Some(env.registry_path()), Some(env.legacy_path()));
    let conn = reg
        .connections
        .iter()
        .find(|c| c.kind == ProviderKind::SiliconFlow)
        .expect("auto-created siliconflow connection");
    assert_eq!(conn.id, "siliconflow");
    assert!(conn.has_key);
}

#[test]
fn set_errors_when_multiple_connections_of_same_kind_exist() {
    let _lock = lock_mem_store();
    let env = TestEnv::new();
    let mut reg = ProviderRegistry::default();
    reg.connections.push(blank_provider_connection(
        "siliconflow",
        ProviderKind::SiliconFlow,
        "Qwen/Qwen2.5-7B-Instruct",
    ));
    reg.connections.push(blank_provider_connection(
        "siliconflow-2",
        ProviderKind::SiliconFlow,
        "Qwen/Qwen2.5-7B-Instruct",
    ));
    env.write_registry(&reg);

    let args = parse_set(&["flowforge", "set", "siliconflow", "api-key", "sk"]);
    let code = run_set(&args, Some("sk"));
    assert_eq!(code, ExitCode::FAILURE);
}

fn blank_provider_connection(id: &str, kind: ProviderKind, model: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.to_string(),
        kind,
        display_name: id.to_string(),
        vendor: None,
        base_url: None,
        model: model.to_string(),
        has_key: false,
        secret_missing: false,
        thinking: kind.default_thinking(),
        reasoning_effort: Default::default(),
        reasoning_visibility: Default::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
    }
}
