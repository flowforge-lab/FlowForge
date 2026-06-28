//! Headless assembly. Builds the same provider + tool registry + skill set the
//! desktop app wires up, but for a terminal. This is the single place runtime
//! dependencies are constructed, so later milestones (M4 MCP tools, M5 SQLite
//! sessions) wire in here rather than in each command.

use std::path::PathBuf;

use ff_core::{ProviderConfig, ProviderKind};
use ff_llm::{
    reasoning_control, wire_dialect, BedrockCreds, BedrockProvider, OllamaProvider, OpenAiProvider,
    Provider,
};
use ff_skills::SkillRegistry;

/// The provider + default model, honoring the same `~/.config/flowforge/provider.json`
/// the desktop app persists, so a provider chosen in the GUI is respected here.
/// Falls back to the default (local candle-vllm) when absent or unreadable.
pub fn load_provider() -> (Box<dyn Provider>, String) {
    let config = load_provider_config();
    let model = config.model.clone();
    (build_provider(&config), model)
}

fn load_provider_config() -> ProviderConfig {
    provider_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn provider_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("provider.json"))
}

fn build_provider(config: &ProviderConfig) -> Box<dyn Provider> {
    let base_url = config.resolved_base_url().to_string();
    // CLI has no per-connection vendor descriptor (#375); the model name is the
    // only signal we have for SiliconFlow GLM/MiniMax detection.
    let dialect = wire_dialect(config.kind, None, &config.model);
    // OpenAI-wire reasoning controls (#394), mirroring the desktop's `build_provider`.
    // A no-op except on the SiliconFlow gateway; the effort dial comes from
    // `provider.json` (`reasoning_effort`), defaulting to Medium for legacy files.
    let reasoning = reasoning_control(config.kind, &config.model, config.reasoning_effort);
    match config.kind {
        ProviderKind::CandleVllm => {
            Box::new(OpenAiProvider::new(base_url, None).with_dialect(dialect))
        }
        ProviderKind::Ollama => Box::new(OllamaProvider::new(base_url)),
        ProviderKind::Bedrock => Box::new(build_bedrock_provider(config)),
        // The CLI has no keychain, so a hosted OpenAI key comes from the
        // OPENAI_API_KEY env var (absent or empty => keyless, for OpenAI-compatible
        // local gateways that need none).
        ProviderKind::OpenAi => Box::new(
            OpenAiProvider::new(base_url, api_key_from_env("OPENAI_API_KEY"))
                .with_dialect(dialect)
                .with_reasoning_control(reasoning),
        ),
        // SiliconFlow is OpenAI-compatible. The CLI has no keychain, so the bearer
        // key comes from SILICONFLOW_API_KEY (empty/unset = anonymous, which the
        // hosted endpoint will reject -- the same env-var pattern as Bedrock above).
        ProviderKind::SiliconFlow => {
            let key = std::env::var("SILICONFLOW_API_KEY")
                .ok()
                .filter(|k| !k.is_empty());
            Box::new(
                OpenAiProvider::new(base_url, key)
                    .with_dialect(dialect)
                    .with_reasoning_control(reasoning),
            )
        }
    }
}

/// Build a Bedrock provider from `config`, mirroring the desktop's `build_provider`.
/// Extracted so the reasoning-effort dial (#394) is assertable without a live Bedrock
/// call — without `with_reasoning_effort`, per-step thinking is invisible through
/// `flowforge run` (same bug surface as desktop #426 acceptance).
fn build_bedrock_provider(config: &ProviderConfig) -> BedrockProvider {
    // The CLI has no keychain or connection registry, so Bedrock here uses the
    // standard AWS credential chain: a bearer token from AWS_BEARER_TOKEN_BEDROCK
    // when set, otherwise a named profile (AWS_PROFILE, default "default").
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_string());
    let creds = match std::env::var("AWS_BEARER_TOKEN_BEDROCK") {
        Ok(token) if !token.is_empty() => BedrockCreds::ApiKey { token },
        _ => BedrockCreds::Profile {
            name: std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".to_string()),
        },
    };
    BedrockProvider::new(region, creds).with_reasoning_effort(config.reasoning_effort)
}

/// A bearer key from `var`, or `None` when the variable is unset *or* empty.
/// An empty string is treated as "no key" (keyless) rather than sent as a blank
/// `Authorization: Bearer`, matching the `!token.is_empty()` guard the Bedrock
/// and SiliconFlow CLI arms use.
fn api_key_from_env(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|k| !k.is_empty())
}

/// `~/.flowforge/skills` — the installed-skills directory shared with the desktop
/// app. Loaded read-only here; install/uninstall stay desktop-side for now.
pub fn skills_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flowforge")
        .join("skills")
}

/// `~/.flowforge/phenos` — the phenotype definitions shared with the desktop
/// app (RFC 0001 §7). Loaded read-only here; editing stays desktop-side.
pub fn phenotypes_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flowforge")
        .join("phenos")
}

/// Resolve a phenotype by name: the built-in `default`, otherwise a definition
/// from `~/.flowforge/phenos/<name>.toml`. Returns `None` for an unknown
/// name. Mirrors the desktop's `resolve_phenotype` so a headless turn sees the
/// same definition the GUI would apply.
pub fn resolve_phenotype(name: &str) -> Option<ff_core::Phenotype> {
    resolve_phenotype_in(name, &phenotypes_root())
}

fn resolve_phenotype_in(name: &str, root: &std::path::Path) -> Option<ff_core::Phenotype> {
    use ff_skills::{default_phenotype, load_phenotypes, DEFAULT_PHENOTYPE};

    if name == DEFAULT_PHENOTYPE {
        return Some(default_phenotype());
    }
    let (mut map, errors) = load_phenotypes(root);
    for e in &errors {
        eprintln!("warning: phenotype load: {e}");
    }
    map.remove(name)
}

/// Load the installed skill set for system-prompt injection. A parse error is
/// reported but never fatal — one bad skill must not block a turn.
pub fn load_skills() -> SkillRegistry {
    let (registry, errors) = SkillRegistry::load_dir(&skills_root());
    for e in &errors {
        eprintln!("warning: skill load: {e}");
    }
    registry
}

/// The directory the agent's file/shell tools are jailed to. For a CLI this is the
/// current working directory — you run `flowforge` where you want it to act.
pub fn workspace_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::resolve_phenotype_in;
    use ff_skills::DEFAULT_PHENOTYPE;
    use std::fs;

    #[test]
    fn default_phenotype_resolves_without_any_files() {
        // The built-in "default" short-circuits the filesystem: it resolves even
        // against an empty (or nonexistent) phenos root, like the desktop does.
        let tmp = tempfile::tempdir().unwrap();
        let p = resolve_phenotype_in(DEFAULT_PHENOTYPE, tmp.path()).unwrap();
        assert_eq!(p.name, "default");
        assert!(p.skills.is_empty());
        assert!(p.model.is_none());
        assert!(p.persona.is_none());
    }

    #[test]
    fn resolves_named_phenotype_from_toml_by_stem() {
        // A `--pheno rust` turn reads `<root>/rust.toml`; the name comes from the
        // file stem, not a field inside the TOML (mirrors the desktop).
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
        // Unique per-test var name so parallel tests never race on process env.
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
        // A var name that is never set anywhere.
        assert_eq!(
            super::api_key_from_env("FF_TEST_OPENAI_KEY_NEVER_SET"),
            None
        );
    }

    // ---- reasoning-control wiring (#394): the CLI must mirror the desktop and emit
    // SiliconFlow's enable_thinking / thinking_budget on the wire. Without the
    // with_reasoning_control hook in build_provider, per-step thinking is invisible
    // through `flowforge run`. ----

    use ff_core::{ProviderConfig, ProviderKind, ReasoningEffort};
    use ff_llm::{ChatMessage, ChatRequest};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build the SiliconFlow CLI provider against `server`, POST one chat turn, and
    /// return the JSON body it sent. Mirrors the body-capture tests in
    /// `crates/ff-llm/src/openai.rs`.
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
        };
        let _ = provider.chat_stream(req).await.expect("send succeeds");
        let reqs = server.received_requests().await.expect("requests recorded");
        serde_json::from_slice(&reqs[0].body).expect("body is json")
    }

    #[tokio::test]
    async fn siliconflow_cli_provider_emits_thinking_budget() {
        // thinking on => the Medium budget cap rides the wire (not the off-switch).
        let body = siliconflow_body(true, ReasoningEffort::Medium).await;
        assert_eq!(body["thinking_budget"], 4096);
        assert!(body.get("enable_thinking").is_none());
    }

    #[tokio::test]
    async fn siliconflow_cli_provider_effort_dial_is_honored() {
        // The reasoning_effort field from provider.json picks the budget.
        let body = siliconflow_body(true, ReasoningEffort::Low).await;
        assert_eq!(body["thinking_budget"], 1024);
    }

    #[tokio::test]
    async fn siliconflow_cli_provider_thinking_off_disables_reasoning() {
        // thinking off => enable_thinking: false, so the model does not reason.
        let body = siliconflow_body(false, ReasoningEffort::Medium).await;
        assert_eq!(body["enable_thinking"], false);
        assert!(body.get("thinking_budget").is_none());
    }

    // ---- Bedrock reasoning-effort wiring (#394/#426): the CLI must mirror the
    // desktop and forward `config.reasoning_effort` to the Bedrock provider's
    // thinking budget. Without `with_reasoning_effort` in build_bedrock_provider,
    // per-step thinking is invisible through `flowforge run`. ----

    #[test]
    fn bedrock_cli_provider_forwards_reasoning_effort() {
        // The reasoning_effort field from provider.json reaches the Bedrock
        // provider — assertable via the public accessor, no live Bedrock call.
        let config = ProviderConfig {
            kind: ProviderKind::Bedrock,
            model: "anthropic.claude-3-5-sonnet-20241022-v2:0".into(),
            reasoning_effort: ReasoningEffort::High,
            ..Default::default()
        };
        let provider = super::build_bedrock_provider(&config);
        assert_eq!(provider.reasoning_effort(), ReasoningEffort::High);
    }

    #[test]
    fn bedrock_cli_provider_default_effort_is_medium() {
        // Without an explicit reasoning_effort, the provider defaults to Medium.
        let config = ProviderConfig {
            kind: ProviderKind::Bedrock,
            model: "anthropic.claude-3-5-sonnet-20241022-v2:0".into(),
            ..Default::default()
        };
        let provider = super::build_bedrock_provider(&config);
        assert_eq!(provider.reasoning_effort(), ReasoningEffort::Medium);
    }
}
