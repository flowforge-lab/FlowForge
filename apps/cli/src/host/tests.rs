use super::{channel_map_path, resolve_phenotype_in, session_db_path};
use ff_skills::DEFAULT_PHENOTYPE;
use std::fs;

#[test]
fn default_phenotype_resolves_without_any_files() {
    let tmp = tempfile::tempdir().unwrap();
    let p = resolve_phenotype_in(DEFAULT_PHENOTYPE, tmp.path()).unwrap();
    assert_eq!(p.name, "default");
    assert!(p.skills.is_empty());
    assert!(p.model.is_none());
    assert!(p.persona.is_none());
}

#[test]
fn resolves_named_phenotype_from_toml_by_stem() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("rust.toml"),
        "skills = [\"cargo-check\", \"clippy\"]\n\
         model = \"qwen3-coder\"\n\
         persona = \"You are a Rust expert.\"\n",
    )
    .unwrap();

    let p = resolve_phenotype_in("rust", tmp.path()).unwrap();
    assert_eq!(p.name, "rust");
    assert_eq!(p.skills, vec!["cargo-check", "clippy"]);
    assert_eq!(p.model.as_deref(), Some("qwen3-coder"));
    assert_eq!(p.persona.as_deref(), Some("You are a Rust expert."));
}

#[test]
fn unknown_name_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(resolve_phenotype_in("nope", tmp.path()).is_none());
}

#[test]
fn api_key_from_env_reads_a_set_key() {
    let var = "FF_TEST_OPENAI_KEY_SET";
    std::env::set_var(var, "sk-abc123");
    assert_eq!(super::api_key_from_env(var), Some("sk-abc123".to_string()));
    std::env::remove_var(var);
}

#[test]
fn api_key_from_env_treats_empty_as_keyless() {
    let var = "FF_TEST_OPENAI_KEY_EMPTY";
    std::env::set_var(var, "");
    assert_eq!(super::api_key_from_env(var), None);
    std::env::remove_var(var);
}

#[test]
fn api_key_from_env_unset_is_none() {
    assert_eq!(
        super::api_key_from_env("FF_TEST_OPENAI_KEY_NEVER_SET"),
        None
    );
}

use ff_core::{ProviderConfig, ProviderKind, ReasoningEffort};
use ff_llm::{ChatMessage, ChatRequest, Provider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn siliconflow_body(thinking: bool, effort: ReasoningEffort) -> serde_json::Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("data: [DONE]\n\n"))
        .mount(&server)
        .await;

    let config = ProviderConfig {
        kind: ProviderKind::SiliconFlow,
        base_url: Some(server.uri()),
        model: "zai-org/GLM-5.2".into(),
        reasoning_effort: effort,
        ..Default::default()
    };
    let provider = super::build_provider(&config);
    let req = ChatRequest {
        model: config.model.clone(),
        messages: vec![ChatMessage::text("user", "hi")],
        tools: Vec::new(),
        thinking,
        max_tokens: None,
        cache_messages: false,
    };
    let _ = provider.chat_stream(req).await.expect("send succeeds");
    let reqs = server.received_requests().await.expect("requests recorded");
    serde_json::from_slice(&reqs[0].body).expect("body is json")
}

#[tokio::test]
async fn siliconflow_cli_provider_emits_thinking_budget() {
    let body = siliconflow_body(true, ReasoningEffort::Medium).await;
    assert_eq!(body["thinking_budget"], 4096);
    assert!(body.get("enable_thinking").is_none());
}

#[tokio::test]
async fn siliconflow_cli_provider_effort_dial_is_honored() {
    let body = siliconflow_body(true, ReasoningEffort::Low).await;
    assert_eq!(body["thinking_budget"], 1024);
}

#[tokio::test]
async fn siliconflow_cli_provider_thinking_off_disables_reasoning() {
    let body = siliconflow_body(false, ReasoningEffort::Medium).await;
    assert_eq!(body["enable_thinking"], false);
    assert!(body.get("thinking_budget").is_none());
}

#[test]
fn bedrock_cli_provider_forwards_reasoning_effort() {
    let config = ProviderConfig {
        kind: ProviderKind::Bedrock,
        model: "anthropic.claude-3-5-sonnet-20241022-v2:0".into(),
        reasoning_effort: ReasoningEffort::High,
        ..Default::default()
    };
    let provider = super::build_bedrock_provider(&config, true);
    assert_eq!(provider.reasoning_effort(), ReasoningEffort::High);
    assert!(
        provider.supports_documents(),
        "documents flag forwarded to the Bedrock provider"
    );
}

#[test]
fn bedrock_cli_provider_default_effort_is_medium() {
    let config = ProviderConfig {
        kind: ProviderKind::Bedrock,
        model: "anthropic.claude-3-5-sonnet-20241022-v2:0".into(),
        ..Default::default()
    };
    let provider = super::build_bedrock_provider(&config, true);
    assert_eq!(provider.reasoning_effort(), ReasoningEffort::Medium);
}

/// The channel→session map must live beside the session DB (#1060).
///
/// `serve` shares its store with the desktop app so a Slack conversation shows up
/// in the UI; if the map resolved to a different root, the mapping and the
/// sessions it names would drift apart and every restart would strand the
/// channel's history. Deriving both from `config_dir` is what prevents that, and
/// this asserts it rather than trusting the two call sites to stay in step.
#[test]
fn channel_map_sits_beside_the_session_db() {
    let _env = crate::test_support::TestEnv::new();

    let map = channel_map_path().expect("override provides a config dir");
    let db = session_db_path().expect("override provides a config dir");

    assert_eq!(
        map.parent(),
        db.parent(),
        "channel map and session DB must share a directory"
    );
    assert_eq!(map.file_name().unwrap(), "channel-map.json");
}

/// #1060: a missing `provider.json` must not resolve silently.
///
/// The default is local candle-vllm on port 8000, so a silent fallback made every
/// `serve` turn fail with `error sending request for url
/// (http://localhost:8000/v1/chat/completions)` — an error naming a port the user
/// never chose, with nothing connecting it to an absent config file.
#[test]
fn missing_provider_config_falls_back_to_the_default() {
    let env = crate::test_support::TestEnv::new();
    assert!(
        !env.legacy_path().exists(),
        "precondition: no provider.json in the test env"
    );

    let config = super::load_provider_config();

    assert_eq!(config.kind, ProviderKind::default());
    assert_eq!(
        super::provider_config_path().unwrap(),
        env.legacy_path(),
        "must read the overridden dir, not the real ~/.config"
    );
}

/// A malformed file falls back rather than panicking — and is distinguished from
/// the missing-file case, since the remedies differ (fix the file vs create it).
#[test]
fn malformed_provider_config_falls_back_to_the_default() {
    let env = crate::test_support::TestEnv::new();
    fs::write(env.legacy_path(), "{ not json").unwrap();

    let config = super::load_provider_config();

    assert_eq!(config.kind, ProviderKind::default());
}

/// The complement, and the one that stops the warnings becoming noise: a valid
/// config is honored and must NOT hit any fallback path.
#[test]
fn valid_provider_config_is_honored() {
    let env = crate::test_support::TestEnv::new();
    fs::write(
        env.legacy_path(),
        r#"{"kind":"ollama","model":"qwen3.6:35b-a3b","hasKey":false}"#,
    )
    .unwrap();

    let config = super::load_provider_config();

    assert_eq!(config.kind, ProviderKind::Ollama);
    assert_eq!(config.model, "qwen3.6:35b-a3b");
    assert_ne!(
        config.kind,
        ProviderKind::default(),
        "guards the test itself: a default-kind fixture could not detect a fallback"
    );
}

/// The fallback description must name the port that appears in the resulting
/// request error, which is the only string tying symptom to cause.
#[test]
fn default_provider_desc_names_the_url_from_the_error() {
    let desc = super::default_provider_desc();
    assert!(desc.contains("localhost:8000"), "got {desc}");
    assert!(desc.contains("candle-vllm"), "got {desc}");
}
