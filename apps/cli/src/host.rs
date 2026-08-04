//! Headless assembly. Builds the same provider + tool registry + skill set the
//! desktop app wires up, but for a terminal. This is the single place runtime
//! dependencies are constructed, so later milestones (M4 MCP tools, M5 SQLite
//! sessions) wire in here rather than in each command.

use std::path::PathBuf;

use ff_core::{model_supports_documents, ProviderConfig, ProviderKind};
use ff_llm::{
    ollama_num_ctx_from_env, reasoning_control, wire_dialect, BedrockCreds, BedrockProvider,
    OllamaProvider, OpenAiProvider, Provider,
};
use ff_skills::SkillRegistry;

/// The provider + default model, honoring the same `~/.config/flowforge/provider.json`
/// the desktop app persists, so a provider chosen in the GUI is respected here.
/// Falls back to the default (local candle-vllm) when absent or unreadable.
// TODO(#724 follow-up): adopt `provider-registry.json` (loaded via
// `crate::registry::load_registry`) so the chat/run arms see the same
// connections the new `flowforge config` subcommand edits. Kept on the
// legacy singleton for now to keep the chat surface unchanged.
pub fn load_provider() -> (Box<dyn Provider>, String) {
    let config = load_provider_config();
    let model = config.model.clone();
    (build_provider(&config), model)
}

fn load_provider_config() -> ProviderConfig {
    let Some(path) = provider_config_path() else {
        tracing::warn!(
            "no config dir; falling back to the default provider ({})",
            default_provider_desc()
        );
        return ProviderConfig::default();
    };

    // Each failure below is reported separately, and none of them is silent. The
    // default provider is local candle-vllm on port 8000, so when this fell
    // through unnoticed every `serve` turn failed with a bare "transport error:
    // error sending request for url (http://localhost:8000/v1/chat/completions)"
    // — a symptom that names a port the user never chose and does not hint that a
    // config file is missing. Diagnosing it from the outside cost a full session
    // (#1060); the fix is to say so at the point the fallback happens.
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                path = %path.display(),
                default = %default_provider_desc(),
                "no provider config; falling back to the default provider. \
                 Run `flowforge config` to choose one",
            );
            return ProviderConfig::default();
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                default = %default_provider_desc(),
                "provider config unreadable; falling back to the default provider",
            );
            return ProviderConfig::default();
        }
    };

    match serde_json::from_str(&raw) {
        Ok(config) => config,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                default = %default_provider_desc(),
                "provider config malformed; falling back to the default provider",
            );
            ProviderConfig::default()
        }
    }
}

fn provider_config_path() -> Option<PathBuf> {
    // Via `config_dir()`, not `dirs::config_dir()` directly, so this resolves to
    // the same directory as `session_db_path` / `channel_map_path` and honors the
    // test override — without which the fallback warnings below are unassertable.
    config_dir().map(|d| d.join("flowforge").join("provider.json"))
}

/// Describes the fallback provider as `slug @ base-url`, e.g.
/// `candle-vllm @ http://localhost:8000/v1`.
///
/// The URL is the point of this: the failure it explains surfaces as a request
/// error naming a port, so the warning has to name the same port to be
/// recognisable as its cause.
fn default_provider_desc() -> String {
    let config = ProviderConfig::default();
    format!("{} @ {}", config.kind.slug(), config.resolved_base_url())
}

fn build_provider(config: &ProviderConfig) -> Box<dyn Provider> {
    let base_url = config.resolved_base_url().to_string();
    // CLI has no per-connection vendor descriptor (#375); the model name is the
    // only signal we have for SiliconFlow GLM/MiniMax detection.
    let dialect = wire_dialect(config.kind, &config.model);
    // OpenAI-wire reasoning controls (#394), mirroring the desktop's `build_provider`.
    // A no-op except on the SiliconFlow gateway; the effort dial comes from
    // `provider.json` (`reasoning_effort`), defaulting to Medium for legacy files.
    let reasoning = reasoning_control(config.kind, &config.model, config.reasoning_effort);
    // Attachment capability derived from the resolved `(kind, model)` (RFC 0005
    // §11.3), mirroring the desktop's `build_provider`. Documents are universal
    // as of the #338 follow-up (extraction fallback for OpenAI/Ollama, native
    // `DocumentBlock` for Bedrock); vision stays fail-closed in the CLI as
    // before (the CLI has no image-attachment flow).
    let documents = model_supports_documents(config.kind, &config.model);
    match config.kind {
        ProviderKind::CandleVllm => Box::new(
            OpenAiProvider::new(base_url, None)
                .with_documents(documents)
                .with_dialect(dialect)
                // CandleVllm is local (#888): the egress-mismatch warning stays
                // silent even when the phenotype is `egress = local-only`.
                .with_kind(config.kind),
        ),
        ProviderKind::Ollama => Box::new(
            OllamaProvider::new(base_url)
                .with_documents(documents)
                // Per-connection window (#651) wins; env var stays as a global override.
                .with_num_ctx(
                    config
                        .num_ctx
                        .map(u64::from)
                        .or_else(ollama_num_ctx_from_env),
                )
                // Ollama is local (#888); symmetric with the desktop's other arms.
                .with_kind(config.kind),
        ),
        ProviderKind::Bedrock => Box::new(build_bedrock_provider(config, documents)),
        // The CLI has no keychain, so a hosted OpenAI key comes from the
        // OPENAI_API_KEY env var (absent or empty => keyless, for OpenAI-compatible
        // local gateways that need none).
        ProviderKind::OpenAi => Box::new(
            OpenAiProvider::new(base_url, api_key_from_env("OPENAI_API_KEY"))
                .with_documents(documents)
                .with_dialect(dialect)
                .with_reasoning_control(reasoning)
                // OpenAi is hosted (#888): the egress-mismatch warning fires
                // correctly when the phenotype is `egress = local-only`.
                .with_kind(config.kind),
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
                    .with_documents(documents)
                    .with_dialect(dialect)
                    .with_reasoning_control(reasoning)
                    // SiliconFlow is hosted (#888): the egress-mismatch warning
                    // fires correctly when the phenotype is `egress = local-only`.
                    .with_kind(config.kind),
            )
        }
        // OpenRouter is OpenAI-compatible. Bearer key from OPENROUTER_API_KEY.
        ProviderKind::OpenRouter => {
            let key = std::env::var("OPENROUTER_API_KEY")
                .ok()
                .filter(|k| !k.is_empty());
            Box::new(
                OpenAiProvider::new(base_url, key)
                    .with_documents(documents)
                    .with_dialect(dialect)
                    .with_reasoning_control(reasoning)
                    // OpenRouter is hosted (#888): the egress-mismatch warning
                    // fires correctly when the phenotype is `egress = local-only`.
                    .with_kind(config.kind),
            )
        }
    }
}

/// Build a Bedrock provider from `config`, mirroring the desktop's `build_provider`.
/// Extracted so the reasoning-effort dial (#394) is assertable without a live Bedrock
/// call — without `with_reasoning_effort`, per-step thinking is invisible through
/// `flowforge run` (same bug surface as desktop #426 acceptance).
fn build_bedrock_provider(config: &ProviderConfig, documents: bool) -> BedrockProvider {
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
    BedrockProvider::new(region, creds)
        .with_documents(documents)
        .with_reasoning_effort(config.reasoning_effort)
        // Bedrock is hosted (#888): the egress-mismatch warning fires correctly
        // when the phenotype is `egress = local-only`.
        .with_kind(config.kind)
}

/// A bearer key from `var`, or `None` when the variable is unset *or* empty.
/// An empty string is treated as "no key" (keyless) rather than sent as a blank
/// `Authorization: Bearer`, matching the `!token.is_empty()` guard the Bedrock
/// and SiliconFlow CLI arms use.
fn api_key_from_env(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|k| !k.is_empty())
}

/// The on-disk session database (RFC 0012 / #277), mirroring the desktop's
/// `sessions_db_path` so the CLI and GUI share one store — no divergent
/// histories. `None` if the OS exposes no config dir, in which case the store
/// falls back to `:memory:` (sessions are lost on exit). Same path the desktop
/// resolves in `apps/desktop/src-tauri/src/state.rs`.
pub fn session_db_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("flowforge").join("sessions.db"))
}

/// Resolved path of the transport channel→session map (#1060). Shares
/// [`config_dir`] with [`session_db_path`] so the test override redirects it too
/// — otherwise `serve` tests would write into the real `~/.config/flowforge`.
pub fn channel_map_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("flowforge").join("channel-map.json"))
}

/// Resolved config dir: the test override (a thread-local set by
/// `test_support::TestEnv`) when active, else the OS default. Mirrors
/// `registry::config_dir` so session-store tests can point at a tempdir without
/// touching the user's real `~/.config/flowforge/sessions.db`.
fn config_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Some(override_dir) = crate::test_support::config_dir_override() {
            return Some(override_dir);
        }
    }
    dirs::config_dir()
}

/// Open the session store. When `ephemeral` is true (the `--ephemeral` escape
/// hatch for one-shot `run`), an in-memory store is used so nothing is written
/// to disk — matching the pre-#1080 CLI behavior. When false (the default),
/// the store is backed by [`session_db_path`] so conversations survive a
/// restart and show up in `ff sessions list` / `ff chat --resume`. Falls back
/// to in-memory with a warning if the path is unavailable or the file cannot
/// be opened — same resilience as the desktop's `build_session_store`.
pub fn build_session_store(ephemeral: bool) -> ff_session::SessionStore {
    if ephemeral {
        return ff_session::SessionStore::new();
    }
    let Some(path) = session_db_path() else {
        eprintln!("warning: no config dir; sessions will not persist across restarts");
        return ff_session::SessionStore::new();
    };
    ff_session::SessionStore::open(&path).unwrap_or_else(|e| {
        eprintln!(
            "warning: session db unavailable at {} ({}); sessions will not persist",
            path.display(),
            e
        );
        ff_session::SessionStore::new()
    })
    // Note: this fallback covers open-time failures only. Write-time
    // contention (SQLITE_BUSY when the desktop GUI holds the WAL write lock)
    // is mitigated by the busy_timeout(5s) set in SessionStore::from_conn
    // (#1080 follow-up). Short contentions wait; a write blocked for >5s
    // would still hit .expect("insert session") in ff-session and panic.
    // Full graceful degradation would require ff-session to return Result
    // instead of panicking on write errors — out of scope for #1080.
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
mod tests;
