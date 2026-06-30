use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use ff_agent::{
    flush_due, AbstractiveConfig, CancelToken, CompactionContext, CompactionStrategy,
    ContextPressureEstimator, MemoryFlush, ProxyTokenEstimator, DEFAULT_FLUSH_AT_FRACTION,
};
use ff_core::{
    model_supports_documents, model_supports_vision, BedrockAuth, ConnectionId, McpScope,
    McpServerConfig, McpServerState, McpServerStatus, Mode, ModelSelection, Phenotype,
    ProviderConfig, ProviderConnection, ProviderKind, ProviderRegistry, ResolvedModel,
    SearchConfig, SecretKind, SessionWorkspace,
};
use ff_llm::{
    ollama_num_ctx_from_env, reasoning_control, wire_dialect, BedrockCreds, BedrockProvider,
    OllamaProvider, OpenAiProvider, Provider, ServedWindowProbe,
};
use ff_mcp::{McpConfigWatcher, SupervisorHandle};

use crate::git_watch::GitHeadWatcher;
use ff_memory::watch::MemoryWatcher;
use ff_memory::{
    EmbeddingProvider, FlushLedger, Fts5Index, HybridIndex, Memory, MemoryConfig, MemoryIndex,
    NoopEmbedder, OpenAiEmbedder,
};
use ff_scheduled::ScheduledStore;
use ff_session::SessionStore;
use ff_signals::{SignalStore, SkillAggregate, SkillCompleted};
use ff_skills::{
    default_phenotype, load_phenotypes, save_phenotype, SharedRegistry, SkillRegistry,
    SkillWatcher, DEFAULT_PHENOTYPE,
};
use ff_tools::memory::{MemoryConsolidateTool, MemoryGetTool, MemorySearchTool, MemoryWriteTool};
use ff_tools::process::{ProcessManagerTool, ProcessSupervisor};
use ff_tools::{Safety, ToolRegistry};
use tokio::sync::oneshot;

/// Registry of in-flight turn cancellation tokens and tool-approval prompts, kept
/// behind a single lock so a cancel and an approval registration can never race
/// (TOCTOU): `register_approval` checks for a live cancel token and inserts the
/// pending slot under the same guard.
#[derive(Default)]
struct ApprovalRegistry {
    /// session_id -> live turn cancellation token. Present only while a turn runs.
    cancels: HashMap<String, CancelToken>,
    /// (session_id, call_id) -> pending UI approval. Keyed by both so colliding
    /// LLM-supplied `call_id`s across concurrent sessions never overwrite each
    /// other. Removed when the user responds, or dropped (denying the call) when
    /// the turn is cancelled. The sender wakes the awaiting approver.
    pending: HashMap<(String, String), oneshot::Sender<bool>>,
    /// (session_id, call_id) -> pending `ask_user` question (#44). Same keying,
    /// liveness, and cancel-via-drop semantics as `pending`, but carries the typed
    /// answer string instead of an approve/deny bool.
    pending_asks: HashMap<(String, String), oneshot::Sender<String>>,
    /// session_id -> set of tool names approved for this session only (#229).
    session_approved: HashMap<String, HashSet<String>>,
    /// Tools approved globally across all sessions (persisted to tool_permissions.json).
    always_approved: HashSet<String>,
}

/// On-disk shape of `tool_permissions.json` (#229).
#[derive(serde::Serialize, serde::Deserialize)]
struct ToolPermissions {
    always_approved: Vec<String>,
}

impl ApprovalRegistry {
    fn set_session_approve(&mut self, session_id: &str, tool: &str) {
        self.session_approved
            .entry(session_id.to_string())
            .or_default()
            .insert(tool.to_string());
    }

    fn is_session_approved(&self, session_id: &str, tool: &str) -> bool {
        self.session_approved
            .get(session_id)
            .is_some_and(|s| s.contains(tool))
    }

    fn set_always_approve(&mut self, tool: &str) {
        self.always_approved.insert(tool.to_string());
    }

    fn remove_always_approve(&mut self, tool: &str) {
        self.always_approved.remove(tool);
    }

    fn is_always_approved(&self, tool: &str) -> bool {
        self.always_approved.contains(tool)
    }

    fn list_always_approved(&self) -> Vec<String> {
        let mut v: Vec<String> = self.always_approved.iter().cloned().collect();
        v.sort();
        v
    }

    fn load_always_approved(path: &Path) -> HashSet<String> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<ToolPermissions>(&s).ok())
            .map(|tp| tp.always_approved.into_iter().collect())
            .unwrap_or_default()
    }

    fn save_always_approved(path: &Path, set: &HashSet<String>) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut sorted: Vec<String> = set.iter().cloned().collect();
        sorted.sort();
        let perms = ToolPermissions {
            always_approved: sorted,
        };
        let json = serde_json::to_string_pretty(&perms).map_err(io::Error::other)?;
        // Atomic write: temp file + rename
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &json)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Builds a fresh [`Provider`] from a [`ProviderConnection`]. Called once per turn
/// so a runtime provider switch takes effect on the next message — there is no
/// shared, mutable provider to swap, only the persisted registry.
fn build_provider(conn: &ProviderConnection, model: &str) -> Box<dyn Provider> {
    let base_url = conn.resolved_base_url().to_string();
    // Per-gateway wire-dialect choices (#375). Resolved once here so the per-turn
    // hot path only carries a `Copy` struct; defaults are no-ops for vanilla
    // OpenAI / candle-vllm / Ollama / LM Studio.
    let dialect = wire_dialect(conn.kind, conn.vendor.as_deref(), model);
    // Reasoning depth dial (#394/#395). The per-connection user override now
    // drives it: it both caps SiliconFlow's auto-`max` escalation and bounds
    // Bedrock/Anthropic extended thinking. Medium for pre-#395 registries.
    let effort = conn.reasoning_effort;
    // OpenAI-wire reasoning controls (#394). No-op except for the SiliconFlow
    // gateway; native providers take the effort dial directly below.
    let reasoning = reasoning_control(conn.kind, model, effort);
    // Attachment capabilities are derived from the resolved `(kind, model)` (RFC
    // 0005 §11.3), never a stored connection flag, so a per-session model override
    // is gated by the model actually running. Fail-closed on unknown models.
    let vision = model_supports_vision(conn.kind, model);
    let documents = model_supports_documents(conn.kind, model);
    match conn.kind {
        ProviderKind::CandleVllm => Box::new(
            OpenAiProvider::new(base_url, None)
                .with_vision(vision)
                .with_dialect(dialect)
                .with_reasoning_control(reasoning),
        ),
        ProviderKind::Ollama => Box::new(
            OllamaProvider::new(base_url)
                .with_vision(vision)
                .with_num_ctx(ollama_num_ctx_from_env()),
        ),
        // Bedrock resolves credentials by auth mode, pulling secret material from the
        // OS keychain here so the provider crate stays keychain-free (#202 PR-2).
        ProviderKind::Bedrock => {
            let region = conn
                .region
                .clone()
                .unwrap_or_else(|| "us-east-1".to_string());
            let id = conn.id.as_str();
            // Auto (the default) resolves to a concrete mode by precedence
            // (API key > profile > IAM keys, #320); explicit modes pin themselves.
            // The probe path (build_provider_for) flows through here too, so it
            // validates the exact credential a run will use.
            let mode = match conn.auth_mode.unwrap_or(BedrockAuth::Auto) {
                BedrockAuth::Auto => resolve_bedrock_auth(conn),
                other => other,
            };
            let creds = match mode {
                BedrockAuth::Auto => unreachable!("Auto resolved above"),
                BedrockAuth::Profile => BedrockCreds::Profile {
                    name: conn
                        .aws_profile
                        .clone()
                        .unwrap_or_else(|| "default".to_string()),
                },
                BedrockAuth::IamKeys => BedrockCreds::IamKeys {
                    access_key_id: conn.access_key_id.clone().unwrap_or_default(),
                    secret_access_key: crate::secrets::get(id, SecretKind::SecretAccessKey)
                        .unwrap_or_default(),
                    session_token: crate::secrets::get(id, SecretKind::SessionToken),
                },
                BedrockAuth::ApiKey => BedrockCreds::ApiKey {
                    token: crate::secrets::get(id, SecretKind::ApiKey).unwrap_or_default(),
                },
            };
            Box::new(
                BedrockProvider::new(region, creds)
                    .with_vision(vision)
                    .with_documents(documents)
                    .with_reasoning_effort(effort),
            )
        }
        // Hosted OpenAI (-compatible). Bearer key pulled from the keychain here so
        // the provider crate stays keychain-free, mirroring the Bedrock arm (#311).
        ProviderKind::OpenAi => {
            let key = crate::secrets::get(conn.id.as_str(), SecretKind::ApiKey);
            Box::new(
                OpenAiProvider::new(base_url, key)
                    .with_vision(vision)
                    .with_dialect(dialect)
                    .with_reasoning_control(reasoning),
            )
        }
        // SiliconFlow is OpenAI-compatible; the bearer key is pulled from the OS
        // keychain here so the provider crate stays keychain-free (mirrors Bedrock).
        ProviderKind::SiliconFlow => {
            let key = crate::secrets::get(conn.id.as_str(), SecretKind::ApiKey);
            Box::new(
                OpenAiProvider::new(base_url, key)
                    .with_vision(vision)
                    .with_dialect(dialect)
                    .with_reasoning_control(reasoning),
            )
        }
    }
}

/// Concrete auth a Bedrock connection in `Auto` mode resolves to, reading live
/// keychain presence (#320). A profile counts only when its name is non-empty;
/// IAM keys count only when both the access key id and the secret access key are
/// present. Pure delegation to [`BedrockAuth::resolve_auto`] for the precedence.
fn resolve_bedrock_auth(conn: &ProviderConnection) -> BedrockAuth {
    let id = conn.id.as_str();
    BedrockAuth::resolve_auto(
        crate::secrets::get(id, SecretKind::ApiKey).is_some(),
        conn.aws_profile.as_deref().is_some_and(|p| !p.is_empty()),
        conn.access_key_id.is_some()
            && crate::secrets::get(id, SecretKind::SecretAccessKey).is_some(),
    )
}

/// The active connection, or the built-in default when the `active` pointer dangles
/// (registry invariants forbid this, but turns must still resolve a provider).
fn active_connection_or_default(registry: &ProviderRegistry) -> ProviderConnection {
    registry.active_connection().cloned().unwrap_or_else(|| {
        ProviderRegistry::default()
            .connections
            .into_iter()
            .next()
            .expect("default registry is non-empty")
    })
}

/// Human-facing label for a bare local kind, used when wrapping a legacy
/// [`ProviderConfig`] as a connection (migration / `with_config`).
fn display_name_for(kind: ProviderKind) -> String {
    match kind {
        ProviderKind::CandleVllm => "candle-vLLM",
        ProviderKind::Ollama => "Ollama",
        ProviderKind::Bedrock => "Amazon Bedrock",
        ProviderKind::OpenAi => "OpenAI",
        ProviderKind::SiliconFlow => "SiliconFlow",
    }
    .to_string()
}

/// Wrap a legacy single [`ProviderConfig`] as a [`ProviderConnection`]. The id is
/// the kind slug so it is stable across migrations.
fn config_to_connection(config: ProviderConfig) -> ProviderConnection {
    ProviderConnection {
        id: config.kind.slug().to_string(),
        kind: config.kind,
        display_name: display_name_for(config.kind),
        vendor: None,
        base_url: config.base_url,
        model: config.model,
        has_key: config.has_key,
        secret_missing: false,
        thinking: config.thinking,
        // Carry the depth dial through migration; a legacy `provider.json` without
        // the field deserializes to Medium (`#[serde(default)]`), same as before.
        reasoning_effort: config.reasoning_effort,
        reasoning_visibility: config.reasoning_visibility,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
    }
}

/// Project a connection back to the legacy [`ProviderConfig`] shape for the
/// `get/set_provider_config` shims kept during the frontend cutover (#126).
fn connection_to_config(conn: &ProviderConnection) -> ProviderConfig {
    ProviderConfig {
        kind: conn.kind,
        base_url: conn.base_url.clone(),
        model: conn.model.clone(),
        has_key: conn.has_key,
        thinking: conn.thinking,
        reasoning_effort: conn.reasoning_effort,
        reasoning_visibility: conn.reasoning_visibility,
    }
}

/// `<config dir>/flowforge/provider.json` — the legacy single-provider file
/// (`~/Library/Application Support` on macOS, `~/.config` on Linux). Still
/// read for one-time migration into the registry, and left in place afterward as a
/// backup. `None` only when the OS exposes no config dir.
fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("provider.json"))
}

/// `<config dir>/flowforge/provider-registry.json` — the persisted connection registry
/// (replaces `provider.json`). `None` only when the OS exposes no config dir, in
/// which case settings stay in-memory for the session.
fn registry_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("provider-registry.json"))
}

/// Build the registry to start from: the saved registry if present, else a one-time
/// migration of a legacy `provider.json` (the saved provider becomes the *active*
/// connection, with the other local vendor seeded keyless + inactive), else the
/// built-in default. Pure and idempotent — persistence happens lazily on the first
/// mutation, so construction (including in tests) never writes to the config dir.
fn load_or_migrate_registry() -> ProviderRegistry {
    load_or_migrate_registry_at(registry_path(), config_path())
}

/// Outcome of reading the persisted registry file: cleanly loaded, genuinely
/// absent, or present-but-unreadable. Distinguishing the last case is what keeps
/// a corrupt or half-written file from silently masquerading as "no config" and
/// wiping the user's connections back to the factory default.
enum RegistryRead {
    Loaded(ProviderRegistry),
    Absent,
    Corrupt,
}

/// Read and parse the registry file without ever destroying data on failure. A
/// file that exists but cannot be read or parsed (e.g. truncated by a crash
/// mid-write) is renamed to a `*.corrupt-<unix>.json` sibling and reported as
/// [`RegistryRead::Corrupt`] so the caller seeds a fresh default without
/// overwriting the preserved bytes.
fn read_registry_file(path: Option<&Path>) -> RegistryRead {
    let Some(path) = path else {
        return RegistryRead::Absent;
    };
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return RegistryRead::Absent,
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e,
                "provider registry unreadable; quarantining and seeding default");
            quarantine_registry(path);
            return RegistryRead::Corrupt;
        }
    };
    match serde_json::from_str::<ProviderRegistry>(&raw) {
        Ok(registry) => RegistryRead::Loaded(registry),
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e,
                "provider registry unparseable; quarantining and seeding default");
            quarantine_registry(path);
            RegistryRead::Corrupt
        }
    }
}

/// Preserve an unreadable registry file by renaming it alongside the original
/// rather than letting the next save truncate it. Best-effort: a rename failure
/// is logged but never fatal.
fn quarantine_registry(path: &Path) {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let preserved = path.with_extension(format!("corrupt-{unix}.json"));
    match fs::rename(path, &preserved) {
        Ok(()) => {
            tracing::warn!(preserved = %preserved.display(), "preserved unreadable provider registry")
        }
        Err(e) => tracing::warn!(path = %path.display(), error = %e,
            "could not preserve unreadable provider registry"),
    }
}

/// Path-injectable core of [`load_or_migrate_registry`] so tests can drive it with
/// tempdir paths instead of the real config dir.
fn load_or_migrate_registry_at(
    reg_path: Option<PathBuf>,
    cfg_path: Option<PathBuf>,
) -> ProviderRegistry {
    let registry = match read_registry_file(reg_path.as_deref()) {
        RegistryRead::Loaded(registry) => registry,
        // Only a genuinely absent registry falls through to legacy migration; a
        // quarantined (corrupt) one seeds a clean default so stale legacy state is
        // never re-migrated over a registry the user was actively using.
        RegistryRead::Absent => cfg_path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<ProviderConfig>(&s).ok())
            .map(build_migrated_registry)
            .unwrap_or_default(),
        RegistryRead::Corrupt => ProviderRegistry::default(),
    };
    registry
}

/// Migrate a legacy single [`ProviderConfig`] into a registry: it becomes the
/// active connection, and the *other* built-in local vendor is added keyless and
/// inactive so the user can still switch (#139 review nit 2).
fn build_migrated_registry(config: ProviderConfig) -> ProviderRegistry {
    let active = config_to_connection(config);
    let mut connections = vec![active.clone()];
    for seed in ProviderRegistry::default().connections {
        if seed.kind != active.kind {
            connections.push(seed);
        }
    }
    ProviderRegistry {
        active: active.id,
        connections,
    }
}

/// Atomically write `contents` to `path`: write a sibling `.tmp` file, then
/// rename it over the target. Rename is atomic on the same filesystem, so a
/// crash or kill mid-write leaves the previous (valid) file intact instead of a
/// truncated one — the root cause behind config silently resetting to default.
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

/// Persists the connection registry. Best-effort: a write failure leaves the
/// in-memory registry authoritative for this session rather than failing the
/// command (mirrors the search-config write path). Written atomically so an
/// interrupted save never corrupts the existing file.
fn save_registry(registry: &ProviderRegistry) {
    let Some(path) = registry_path() else {
        return;
    };
    let Ok(json) = serde_json::to_string_pretty(registry) else {
        return;
    };
    if let Err(e) = write_atomic(&path, &json) {
        tracing::warn!(path = %path.display(), error = %e,
            "provider registry save failed; in-memory state authoritative this session");
    }
}

/// `<config dir>/flowforge/search.json` — persisted, non-secret web-search settings.
/// `None` only when the OS exposes no config dir (settings stay in-memory then).
fn search_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("search.json"))
}

/// Loads persisted web-search settings, falling back to the default (SearXNG, no
/// endpoint) when the file is missing or unparseable.
fn load_search_config() -> SearchConfig {
    search_config_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persists web-search settings. Best-effort and atomic, like [`save_registry`].
fn save_search_config(config: &SearchConfig) {
    let Some(path) = search_config_path() else {
        return;
    };
    let Ok(json) = serde_json::to_string_pretty(config) else {
        return;
    };
    if let Err(e) = write_atomic(&path, &json) {
        tracing::warn!(path = %path.display(), error = %e,
            "search config save failed; in-memory state authoritative this session");
    }
}

/// `~/.config/flowforge/tool_permissions.json` — the persistent tool allowlist (#229).
/// `None` only when the OS exposes no config dir.
fn tool_permissions_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("tool_permissions.json"))
}

/// The on-disk session database (RFC 0012 / #277). `None` if no config dir resolves,
/// in which case the store falls back to `:memory:`.
fn sessions_db_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("sessions.db"))
}

/// Open the persistent session store, falling back to an ephemeral in-memory store
/// (with a warning) if the path is unavailable or the file cannot be opened — same
/// resilience as the FlushLedger open.
fn build_session_store() -> SessionStore {
    // Tests must never write to the real config dir (see load_or_migrate_registry).
    // An on-disk session db is exercised directly in `session_db_survives_restart`.
    if cfg!(test) {
        return SessionStore::new();
    }
    let Some(path) = sessions_db_path() else {
        tracing::warn!("no config dir; sessions will not persist across restarts");
        return SessionStore::new();
    };
    SessionStore::open(&path).unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %path.display(), "session db unavailable; sessions will not persist");
        SessionStore::new()
    })
}

/// `~/.config/flowforge/scheduled.db` — the durable scheduled-task store.
fn scheduled_db_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("scheduled.db"))
}

/// Open the scheduled-task store, falling back to an ephemeral in-memory store (with
/// a warning) if the path is unavailable — same resilience as `build_session_store`.
fn build_scheduled_store() -> ScheduledStore {
    let store = build_scheduled_store_inner();
    // Seed the app's built-in tasks (e.g. Memory Organizer) on first run.
    // Idempotent, so it is safe to call on every startup (RFC 0017 §6.3, #544).
    store.seed_builtins();
    store
}

fn build_scheduled_store_inner() -> ScheduledStore {
    if cfg!(test) {
        return ScheduledStore::open_in_memory().expect("in-memory scheduled store");
    }
    let Some(path) = scheduled_db_path() else {
        tracing::warn!("no config dir; scheduled tasks will not persist across restarts");
        return ScheduledStore::open_in_memory().expect("in-memory scheduled store");
    };
    ScheduledStore::open(&path).unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %path.display(), "scheduled db unavailable; tasks will not persist");
        ScheduledStore::open_in_memory().expect("in-memory scheduled store")
    })
}

/// `~/.config/flowforge/phenotype.json` — the name of the active phenotype, persisted
/// so a switch survives a restart (RFC 0001 §7). Separate from the phenotype
/// *definitions* in `~/.flowforge/phenos/`; this only records which one is active.
fn active_phenotype_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("phenotype.json"))
}

/// The persisted active phenotype name, or `None` if never set / unreadable. Falls
/// back to the built-in default at the call site.
fn load_active_phenotype_name() -> Option<String> {
    let raw = active_phenotype_path().and_then(|p| fs::read_to_string(p).ok())?;
    serde_json::from_str::<ActivePhenotypeFile>(&raw)
        .ok()
        .map(|f| f.active)
}

/// Persist the active phenotype name. Best-effort, like `save_registry`.
fn save_active_phenotype_name(name: &str) {
    let Some(path) = active_phenotype_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = ActivePhenotypeFile {
        active: name.to_string(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&file) {
        let _ = fs::write(path, json);
    }
}

/// On-disk shape of `phenotype.json`.
#[derive(serde::Serialize, serde::Deserialize)]
struct ActivePhenotypeFile {
    active: String,
}

/// `~/.config/flowforge/mode.json` — the default agent autonomy mode (RFC 0011 P2,
/// #265), persisted so the user's choice survives a restart. New sessions with no
/// explicit binding inherit this; the factory value is [`Mode::Auto`].
fn default_mode_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("mode.json"))
}

/// On-disk shape of `mode.json`.
#[derive(serde::Serialize, serde::Deserialize)]
struct DefaultModeFile {
    default: Mode,
}

/// The persisted default mode, or [`Mode::Auto`] if never set / unreadable.
fn load_default_mode() -> Mode {
    default_mode_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str::<DefaultModeFile>(&raw).ok())
        .map(|f| f.default)
        .unwrap_or(Mode::Auto)
}

/// Persist the default mode. Best-effort, like `save_active_phenotype_name`.
fn save_default_mode(mode: Mode) {
    let Some(path) = default_mode_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&DefaultModeFile { default: mode }) {
        let _ = fs::write(path, json);
    }
}

/// `~/.flowforge/phenos`, where phenotype definition TOML files live.
fn phenotypes_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flowforge")
        .join("phenos")
}

/// The Codon phenotype, bundled verbatim from the committed `docs/examples/codon/`
/// tree (the single source of truth) so the shipped binary and the documented
/// example can never drift.
const CODON_PHENOTYPE_TOML: &str =
    include_str!("../../../../docs/examples/codon/phenos/codon.toml");
/// The codegraph skill Codon depends on, bundled from the same example tree.
const CODEGRAPH_SKILL_MD: &str =
    include_str!("../../../../docs/examples/codon/skills/codegraph/SKILL.md");

/// Seed the built-in content (the Codon phenotype and the codegraph skill it
/// requires) into the real `~/.flowforge/` tree. Runs once at startup, before the
/// skill watcher spawns and the persisted phenotype resolves, so a user who has
/// selected Codon finds its skill already present.
#[cfg(not(test))]
fn seed_builtin_content() {
    seed_builtin_content_at(
        &phenotypes_root(),
        &skills_root(),
        ff_mcp::config_path().as_deref(),
    );
}

/// Path-injectable core of [`seed_builtin_content`] so tests can drive it against
/// a tempdir instead of the real home. Each built-in is written only when absent,
/// leaving a user-edited copy untouched; the codegraph skill body is written at
/// `skills/codegraph/SKILL.md` (the layout [`SkillRegistry`] scans). When an
/// `mcp.json` path is known we retire a previously seeded, unmodified disabled
/// codegraph entry (RFC 0018 C3 #590) -- codegraph now travels with the codon
/// phenotype, not the global file; a user-edited entry is left intact. `None` skips
/// it (no home dir).
fn seed_builtin_content_at(phenotypes_root: &Path, skills_root: &Path, mcp_path: Option<&Path>) {
    seed_if_absent(&phenotypes_root.join("codon.toml"), CODON_PHENOTYPE_TOML);
    seed_if_absent(
        &skills_root.join("codegraph").join("SKILL.md"),
        CODEGRAPH_SKILL_MD,
    );
    if let Some(mcp_path) = mcp_path {
        retire_seeded_codegraph_if_unmodified(mcp_path);
    }
}

/// The exact shape the pre-C3 seed wrote for codegraph. A live `mcp.json` entry that
/// matches this is an unmodified seed safe to retire; any difference (a user-set
/// command/args/env, or an enabled entry the user turned on) means the user owns it
/// and it is left alone.
fn is_unmodified_codegraph_seed(srv: &McpServerConfig) -> bool {
    srv.id == "codegraph"
        && srv.command == "codegraph"
        && srv.args == ["serve", "--mcp"]
        && srv.env.is_empty()
        && srv.disabled
        && srv.scope == McpScope::Workspace
}

/// Remove the pre-C3 seeded codegraph entry from the global `mcp.json` now that
/// codegraph travels with the codon phenotype (RFC 0018 C3 #590). Removes ONLY an
/// unmodified, disabled seed ([`is_unmodified_codegraph_seed`]); a user-edited entry
/// (e.g. the #573 absolute-`command` workaround) keeps working as a global-tier
/// override. Best-effort: a parse or write failure is logged and skipped so a
/// hand-managed or read-only `mcp.json` is never clobbered or blocks startup.
fn retire_seeded_codegraph_if_unmodified(mcp_path: &Path) {
    let servers = match ff_mcp::load(mcp_path) {
        Ok(servers) => servers,
        Err(e) => {
            tracing::warn!(error = %e, "retire codegraph mcp seed: read existing mcp.json");
            return;
        }
    };
    let is_seed = servers
        .iter()
        .find(|s| s.id == "codegraph")
        .is_some_and(is_unmodified_codegraph_seed);
    if !is_seed {
        return;
    }
    if let Err(e) = ff_mcp::remove(mcp_path, "codegraph") {
        tracing::warn!(error = %e, "retire codegraph mcp seed: write");
    }
}

/// Write `contents` to `path` only if it does not already exist. Best-effort: a
/// failure (read-only home, racing process) is logged and skipped so startup is
/// never blocked -- the affected built-in simply will not appear until a later
/// successful launch.
fn seed_if_absent(path: &Path, contents: &str) {
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            tracing::warn!(path = %path.display(), error = %e, "seed built-in: create dir");
            return;
        }
    }
    if let Err(e) = fs::write(path, contents) {
        tracing::warn!(path = %path.display(), error = %e, "seed built-in: write");
    }
}

/// Resolve a phenotype by name: the built-in `default`, otherwise a definition from
/// `~/.flowforge/phenos/`. Returns `None` for an unknown name.
fn resolve_phenotype(name: &str) -> Option<Phenotype> {
    if name == DEFAULT_PHENOTYPE {
        return Some(default_phenotype());
    }
    let (mut map, errors) = load_phenotypes(&phenotypes_root());
    for e in &errors {
        tracing::warn!(error = %e, "phenotype load");
    }
    map.remove(name)
}

/// The out-of-box default phenotype name (#298, a "for now" default until we revisit
/// defaults). Seeded on first run by #304; a compiled-in, deletion-proof built-in is
/// tracked as #306.
const CODON_PHENOTYPE: &str = "codon";

/// First-run phenotype selection. A persisted user choice always wins; otherwise we
/// prefer the out-of-box `codon` default (seeded into `~/.flowforge/phenos/` on first
/// run), falling back to the built-in `default` when codon isn't installed (e.g. a
/// read-only home where the seed couldn't land). Pure over its inputs so the branch
/// matrix is unit-testable without touching `~/.flowforge`.
fn initial_phenotype(
    persisted: Option<String>,
    resolve: impl Fn(&str) -> Option<Phenotype>,
) -> Phenotype {
    persisted
        .and_then(|n| resolve(n.as_str()))
        .or_else(|| resolve(CODON_PHENOTYPE))
        .unwrap_or_else(default_phenotype)
}

pub struct AppState {
    pub store: Arc<SessionStore>,
    /// Durable scheduled-task store (RFC 0017, #539/#540). Shared (via `Arc`) so a
    /// later headless runner (#542) can read the due set without rebuilding state.
    pub scheduled: Arc<ScheduledStore>,
    /// Persisted, non-secret LLM provider connection registry (RFC 0005 Phase A).
    /// The active connection drives each turn; snapshotted (never held across an
    /// await) per turn. Mutated by the connection commands and the legacy
    /// `set_provider_config` shim.
    registry: Mutex<ProviderRegistry>,
    /// Persisted, non-secret web-search settings, shared (via `Arc`) with the
    /// registered `web_search` tool so a runtime backend switch is visible without
    /// rebuilding the registry.
    search_config: Arc<Mutex<SearchConfig>>,
    /// Per-session workspace roots are an M3 concern (folder picker). For M2 every
    /// session shares one default root; the field is threaded so the picker is
    /// purely additive later.
    pub workspace_root: PathBuf,
    /// Installed skills, kept current by `_skill_watcher`. Snapshotted per turn
    /// (`skills_snapshot`) so a mid-turn reload never races (RFC 0001 §9).
    skills: SharedRegistry,
    /// Owns the filesystem watcher; dropping it stops hot-reload. `Mutex` keeps
    /// `AppState` `Sync` (the `notify` watcher is `Send` but not `Sync`). `Option`
    /// covers the fallback when the watcher cannot start.
    _skill_watcher: Mutex<Option<SkillWatcher>>,
    /// Turn cancellation tokens + pending approvals under one lock (see
    /// [`ApprovalRegistry`]).
    approvals: Mutex<ApprovalRegistry>,
    /// Globally active skills, whose bodies are injected into the system prompt
    /// (RFC 0001 §4). A `BTreeSet` keeps the set deduplicated and name-sorted.
    /// Replaced wholesale by `switch_phenotype`; tweaked individually by
    /// `activate_skill`/`deactivate_skill`.
    active_skills: Mutex<BTreeSet<String>>,
    /// The active phenotype, resolved at startup from the persisted pointer (RFC
    /// 0001 §7). Supplies the model and persona overrides for each turn.
    active_phenotype: Mutex<Phenotype>,
    /// The default agent autonomy mode (RFC 0011 P2, #265), loaded at startup from
    /// `mode.json`. A session with no explicit mode binding inherits this.
    default_mode: Mutex<Mode>,
    /// Per-skill telemetry aggregates (RFC 0001 §8), persisted to
    /// `~/.flowforge/skill_signals.json`. Updated at each turn's start/end; read by
    /// the manual optimize flow's cost estimates.
    signals: Mutex<SignalStore>,
    /// MCP host plumbing (RFC 0003). Populated lazily from Tauri's `setup` closure
    /// (the supervisor needs a live Tokio runtime to spawn its actor) and `None`
    /// when no `mcp.json` is present or the watcher cannot start.
    _mcp_watcher: Mutex<Option<McpConfigWatcher>>,
    /// Owns the git HEAD watcher (#561); dropping it stops live branch sync. `Mutex`
    /// for `Sync` (the `notify` watcher is `Send` but not `Sync`); `None` when the
    /// watcher could not start. Re-pointed per active session by [`align_git_watcher`](Self::align_git_watcher).
    _git_watcher: Mutex<Option<GitHeadWatcher>>,
    mcp: Mutex<Option<SupervisorHandle>>,
    /// Path to the watched `mcp.json`, captured when [`init_mcp_at`](Self::init_mcp_at)
    /// runs. The MCP control commands write back to this exact file so their edits flow
    /// through the same watcher that drives reconcile. `None` until MCP is initialized.
    mcp_config_path: Mutex<Option<PathBuf>>,
    /// Durable agent memory (RFC 0006): the Markdown store at `~/.flowforge/memory`
    /// and its FTS5 recall index, shared (via `Arc`) with the registered
    /// `memory_*` tools. The index is a derived cache rebuilt from the Markdown on
    /// startup and kept current by `_memory_watcher`.
    memory: Arc<Memory>,
    memory_index: Arc<dyn MemoryIndex>,
    /// Durable per-session flush ledger (RFC 0006 §7.2). `None` if its sidecar db
    /// could not be opened — the flush then degrades to off rather than failing.
    flush_ledger: Option<Arc<FlushLedger>>,
    /// Owns the memory filesystem watcher; dropping it stops debounced reindex.
    _memory_watcher: Mutex<Option<MemoryWatcher>>,
    /// App-global table of agent-started background processes (#218), shared (via
    /// `Arc`) with the registered `process_manager` tool so a process started in
    /// one turn can be polled or stopped in a later one. Children are killed when
    /// the last `Arc` drops at app exit.
    process_supervisor: Arc<ProcessSupervisor>,
    /// Short-TTL cache for the Ollama served-window probe (#602), keyed by the
    /// resolved `(connection, model)`. The chip resolves on every render, but the
    /// served window changes only when the model is (re)loaded, so a probe per
    /// resolve would spam `/api/ps`. Entries expire after [`SERVED_WINDOW_TTL`].
    served_window_cache: Mutex<HashMap<(ConnectionId, String), (Instant, ServedWindowProbe)>>,
}

/// How long a probed served window stays fresh before the next resolve re-probes.
const SERVED_WINDOW_TTL: Duration = Duration::from_secs(10);

impl AppState {
    pub fn new() -> Self {
        Self::with_registry(load_or_migrate_registry())
    }

    pub fn with_registry(registry: ProviderRegistry) -> Self {
        // Seed the bundled built-ins (Codon + codegraph) before the watcher loads
        // the skills dir, so the codegraph skill is present when a persisted Codon
        // phenotype resolves below. Gated out of tests, which must not write to the
        // real `~/.flowforge/`; the seed core is exercised directly via tempdirs.
        #[cfg(not(test))]
        seed_builtin_content();
        let (watcher, skills) = load_skills();
        // The installer tools are agent-callable, so they own the skills root and a
        // handle to the shared registry to refresh it on a successful install.
        // Shared so the registered `web_search` tool and `set_search_config` see the
        // same cell; a settings change takes effect on the next call.
        let search_config = Arc::new(Mutex::new(load_search_config()));
        let (memory, memory_index, memory_watcher) = build_memory();
        let flush_ledger = FlushLedger::open(memory.root().join("flush.db"))
            .map(Arc::new)
            .map_err(
                |e| tracing::warn!(error = %e, "flush ledger unavailable; memory flush disabled"),
            )
            .ok();
        let state = Self {
            store: Arc::new(build_session_store()),
            scheduled: Arc::new(build_scheduled_store()),
            registry: Mutex::new(registry),
            search_config,
            workspace_root: default_workspace_root(),
            skills,
            _skill_watcher: Mutex::new(watcher),
            approvals: Mutex::new({
                let mut reg = ApprovalRegistry::default();
                if let Some(path) = tool_permissions_path() {
                    reg.always_approved = ApprovalRegistry::load_always_approved(&path);
                }
                reg
            }),
            active_skills: Mutex::new(BTreeSet::new()),
            active_phenotype: Mutex::new(default_phenotype()),
            default_mode: Mutex::new(load_default_mode()),
            signals: Mutex::new(load_signals()),
            _mcp_watcher: Mutex::new(None),
            _git_watcher: Mutex::new(None),
            mcp: Mutex::new(None),
            mcp_config_path: Mutex::new(None),
            memory,
            memory_index,
            flush_ledger,
            _memory_watcher: Mutex::new(memory_watcher),
            process_supervisor: Arc::new(ProcessSupervisor::new()),
            served_window_cache: Mutex::new(HashMap::new()),
        };
        // Restore the persisted phenotype so its active skills survive a restart.
        // With no persisted choice, prefer the out-of-box `codon` default (#298),
        // falling back to the built-in `default` when codon isn't installed.
        let initial = initial_phenotype(state.persisted_phenotype_name(), resolve_phenotype);
        state.apply_phenotype(initial);
        state
    }

    /// The persisted active phenotype name, if any. Indirection so tests can observe
    /// the load path.
    fn persisted_phenotype_name(&self) -> Option<String> {
        load_active_phenotype_name()
    }

    /// Make `pheno` the active phenotype: replace the active-skill set with its
    /// (registry-validated) skills, recording the resolved phenotype for model and
    /// persona overrides. Unknown skill names are dropped with a warning rather than
    /// failing — the installed set can drift from a phenotype definition.
    fn apply_phenotype(&self, pheno: Phenotype) -> BTreeSet<String> {
        let next = self.resolve_skills(&pheno);
        *self.active_skills.lock().unwrap() = next.clone();
        *self.active_phenotype.lock().unwrap() = pheno;
        next
    }

    /// Resolve a phenotype's declared skills against the installed registry,
    /// dropping (with a warning) any name that is not installed — the installed
    /// set can drift from a phenotype definition, and a missing skill must not
    /// fail the turn. Returns a name-sorted, deduplicated set. Shared by
    /// [`apply_phenotype`](Self::apply_phenotype) (global switch) and the
    /// per-session resolver (#246) so both paths validate identically.
    pub(crate) fn resolve_skills(&self, pheno: &Phenotype) -> BTreeSet<String> {
        let known = self.skills.read().unwrap();
        let mut next = BTreeSet::new();
        for name in &pheno.skills {
            if known.get(name).is_some() {
                next.insert(name.clone());
            } else {
                tracing::warn!(skill = %name, phenotype = %pheno.name, "phenotype names unknown skill; skipping");
            }
        }
        next
    }

    /// MCP server ids declared by `skills` (via their manifests) whose tools are NOT
    /// currently available — the server is either absent from `~/.flowforge/mcp.json`
    /// or present but not `Running` (a `Failed`/`Disabled`/`Restarting`/`Starting`
    /// server advertises no tools).
    ///
    /// A phenotype carries its MCP servers as "DNA" (#235): a skill declares the
    /// servers it needs, and a phenotype that lists the skill expects those servers
    /// available. The first cut *requires* the server to already exist in `mcp.json`
    /// and reports the unavailable ones; it never injects a server definition on
    /// activation (that would mutate the `mcp.json` source-of-truth — a follow-up).
    ///
    /// Name-sorted and deduplicated. Empty only when every required server is present
    /// AND running, or no listed skill requires one. When MCP is uninitialized every
    /// required server is reported (none can be running).
    fn missing_skill_mcp_servers(&self, skills: &BTreeSet<String>) -> Vec<String> {
        let required: BTreeSet<String> = {
            let registry = self.skills.read().unwrap();
            skills
                .iter()
                .filter_map(|name| registry.get(name))
                .flat_map(|skill| skill.manifest.mcp.iter().cloned())
                .collect()
        };
        if required.is_empty() {
            return Vec::new();
        }
        let snapshot = self
            .mcp_handle()
            .map(|handle| handle.status_snapshot())
            .unwrap_or_default();
        Self::unavailable_required_servers(&required, &snapshot)
    }

    /// Pure diff of `required` server ids against a supervisor `snapshot`: an id is
    /// unavailable unless a server with that id is present and in the `Running` state.
    /// Name-sorted and deduplicated (`required` is a `BTreeSet`).
    fn unavailable_required_servers(
        required: &BTreeSet<String>,
        snapshot: &[McpServerStatus],
    ) -> Vec<String> {
        let running: BTreeSet<&str> = snapshot
            .iter()
            .filter(|s| s.state == McpServerState::Running)
            .map(|s| s.id.as_str())
            .collect();
        required
            .iter()
            .filter(|id| !running.contains(id.as_str()))
            .cloned()
            .collect()
    }

    /// Warn (once per server) when an activated phenotype lists skills whose declared
    /// MCP servers are unavailable — absent from `mcp.json` or present but not running
    /// (#235). Best-effort and non-fatal: this must not block activation — the skill's
    /// grep/glob fallbacks still work; only the bridged MCP tools are unavailable until
    /// the user adds/repairs the server. No server is injected.
    fn warn_missing_skill_mcp(&self, phenotype: &str, skills: &BTreeSet<String>) {
        for server in self.missing_skill_mcp_servers(skills) {
            tracing::warn!(
                phenotype = %phenotype,
                server = %server,
                "phenotype skill requires an MCP server whose tools are unavailable (not present in mcp.json, or present but not running); add or repair it to enable them (no server is injected)"
            );
        }
    }

    /// The required-but-unavailable MCP servers for `phenotype_name`'s resolved skills
    /// (#301). Same list [`warn_missing_skill_mcp`](Self::warn_missing_skill_mcp) logs,
    /// surfaced read-only so the command layer (which holds the `AppHandle`) can emit
    /// `phenotype:mcp-unavailable` alongside the warn. Name-sorted and deduplicated;
    /// empty for an unknown phenotype or when every required server is present and
    /// running.
    pub fn unavailable_skill_mcp_servers(&self, phenotype_name: &str) -> Vec<String> {
        match resolve_phenotype(phenotype_name) {
            Some(pheno) => self.missing_skill_mcp_servers(&self.resolve_skills(&pheno)),
            None => Vec::new(),
        }
    }

    /// Run a pre-compaction memory flush if the session is now under context
    /// pressure (RFC 0006 §7.2). Best-effort and silent: it persists durable facts
    /// to memory before older detail is summarized away, and never touches the
    /// visible transcript. No-op when memory is disabled or the flush ledger is
    /// unavailable.
    ///
    /// Cycle policy (v1): flush the first time a session crosses the budget, then at
    /// most once per [`REFLUSH_INTERVAL_MESSAGES`] of further growth while still over
    /// budget. A real auto-compaction trigger (its own work) will later advance the
    /// cycle marker; the estimator/strategy seams in `ff-agent` already accommodate
    /// per-model windows and richer strategies.
    pub async fn maybe_flush_memory(
        &self,
        provider: &dyn Provider,
        registry: &ToolRegistry,
        session_id: &str,
        model: &str,
        cancel: CancelToken,
    ) {
        let Some(ledger) = self.flush_ledger.as_ref() else {
            return;
        };
        if !self.memory.is_enabled() {
            return;
        }
        let history = self.store.get_messages(session_id);
        let pressure = ProxyTokenEstimator::default().assess(&history, model);
        let message_count = history.len() as u64;
        let last_flush_count = match ledger.last_flush(session_id) {
            Ok(rec) => rec.map(|r| r.message_count),
            Err(e) => {
                tracing::warn!(error = %e, "flush ledger read failed; skipping flush");
                return;
            }
        };
        if !flush_due(
            pressure,
            message_count,
            last_flush_count,
            DEFAULT_FLUSH_AT_FRACTION,
            REFLUSH_INTERVAL_MESSAGES,
        ) {
            return;
        }

        let session_root = self.session_root(session_id);
        let outcome = MemoryFlush
            .compact(CompactionContext {
                provider,
                store: self.store.as_ref(),
                registry,
                root: &session_root,
                session_id,
                model,
                cancel,
            })
            .await;
        match outcome {
            Ok(o) => {
                tracing::info!(?o, session = %session_id, "pre-compaction memory flush");
                if let Err(e) = ledger.record_flush(session_id, message_count, now_ms()) {
                    tracing::warn!(error = %e, "flush ledger write failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "memory flush failed"),
        }
    }

    /// The working directory for `session_id`'s tools this turn. Returns the
    /// session's persisted cwd if one has been set (#200/#279), otherwise the
    /// default [`workspace_root`](Self::workspace_root). A stored path that no
    /// longer resolves to a directory (e.g. deleted between runs) also falls back,
    /// so a restored session never runs its tools against a missing root.
    pub fn session_root(&self, session_id: &str) -> PathBuf {
        self.store
            .session_workspace(session_id)
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| self.workspace_root.clone())
    }

    /// Set `session_id`'s working directory, persisted in the session row so it
    /// survives a restart (#279). Called by the `set_session_workspace` command
    /// (#200) and exercised by tests. No-op for an unknown session.
    pub fn set_session_cwd(&self, session_id: &str, dir: PathBuf) {
        self.store
            .set_session_workspace(session_id, Some(dir.display().to_string()));
    }

    /// The shared durable-memory store (RFC 0006), used for per-turn ambient
    /// injection and shared with the registered `memory_*` tools.
    pub fn memory(&self) -> Arc<Memory> {
        self.memory.clone()
    }

    /// The shared recall index, so per-turn ambient injection can skip dormant
    /// chunks (RFC 0007 §M6.1). Same `Arc` the `memory_*` tools hold.
    pub fn index(&self) -> Arc<dyn MemoryIndex> {
        self.memory_index.clone()
    }

    /// The directory installed skills live in.
    pub fn skills_root(&self) -> PathBuf {
        skills_root()
    }

    /// The retained-version history tree for skill evolution (RFC 0001 §8). Kept
    /// outside `skills_root` so the registry never sees version copies as skills.
    pub fn skill_history_root(&self) -> PathBuf {
        skill_history_root()
    }

    /// Re-scan the skills directory into the shared registry. Called after an
    /// install/uninstall so the change is visible without waiting on the watcher.
    pub fn reload_skills(&self) {
        reload_registry(&skills_root(), &self.skills);
    }

    /// Build the per-turn tool registry: built-in tools + MCP-bridged tools from
    /// running servers (RFC 0003 §6). Snapshotted per turn so a hot-reload mid-turn
    /// never races an in-flight tool call — same discipline as skill snapshots.
    pub fn build_tool_registry(&self, session_root: &Path) -> ToolRegistry {
        let mut reg = ToolRegistry::with_defaults();
        reg.register(Box::new(ff_tools::WebSearchTool::new(
            self.search_config.clone(),
        )));
        reg.register(Box::new(crate::tools::InstallSkillTool::new(
            skills_root(),
            self.skills.clone(),
        )));
        reg.register(Box::new(crate::tools::UninstallSkillTool::new(
            skills_root(),
            self.skills.clone(),
        )));
        reg.register(Box::new(crate::tools::SearchSkillsTool::new(
            self.skills.clone(),
        )));
        reg.register(Box::new(crate::tools::SkillsTool::new(self.skills.clone())));
        // Durable-memory recall tools (RFC 0006 §6-7). Share the long-lived store
        // and index so a `memory_write` is searchable within the same turn.
        reg.register(Box::new(MemorySearchTool::new(
            self.memory.clone(),
            self.memory_index.clone(),
        )));
        reg.register(Box::new(MemoryGetTool::new(self.memory.clone())));
        reg.register(Box::new(MemoryWriteTool::new(
            self.memory.clone(),
            self.memory_index.clone(),
        )));
        // Background-process control (#218). App-global supervisor injected here so
        // a process started in one turn survives into later turns.
        reg.register(Box::new(ProcessManagerTool::new(
            self.process_supervisor.clone(),
        )));
        reg.register(Box::new(MemoryConsolidateTool::new(
            self.memory.clone(),
            self.memory_index.clone(),
        )));
        // Reversible tool-result compaction retrieve (M7.1a, RFC 0016 Tier 1).
        // Shares the live session store so it can read originals stashed at ingest.
        reg.register(Box::new(ff_tools::CompactionRetrieveTool::new(
            self.store.clone(),
        )));
        // Bridge MCP tools from the instances this session resolves to (M4.3): every
        // global instance plus the workspace instances rooted at `session_root` (RFC
        // 0018 §4.6). Routing is by instance key, so a concurrent turn on another
        // workspace binds the same tool name to its own instance.
        if let Some(handle) = self.mcp_handle() {
            for tool in ff_mcp::build_bridged_tools(&handle, session_root) {
                reg.register(tool);
            }
        }
        reg
    }

    /// Align the MCP supervisor's live instance set for `session_id`, now rooted at
    /// `root` (RFC 0018 §4.3/§4.5). Each `Workspace`-scoped server (e.g. codegraph) gets
    /// a per-root instance, ref-counted by session; a referenced instance not currently
    /// `Running` is proactively (re)started for this turn -- the fix for a codegraph
    /// parked in `Failed` (#557). Best-effort: a no-op when MCP is uninitialized.
    pub async fn align_session_mcp(&self, session_id: &str, root: &Path) {
        if let Some(sup) = self.mcp_handle() {
            let servers = self.resolve_mcp_servers(session_id);
            sup.align_session(session_id, root.to_path_buf(), servers)
                .await;
        }
    }

    /// Release `session_id`'s references to per-workspace MCP instances, evicting any
    /// whose ref-list empties (RFC 0018 §4.3). Called on session close/delete so a
    /// per-workspace codegraph is reaped once no live session references its path.
    pub async fn release_session_mcp(&self, session_id: &str) {
        if let Some(sup) = self.mcp_handle() {
            sup.release_session(session_id).await;
        }
    }

    /// Start the git HEAD watcher (#561 BE half) and store it. Returns the receiver
    /// that yields a `SessionWorkspace` after each changed branch resolution so the
    /// caller (the Tauri `setup` hook) can forward it as `workspace:branch-changed`.
    /// `None` when the watcher could not start -- live branch sync then degrades to
    /// off rather than failing app startup. Mirrors [`init_mcp`](Self::init_mcp): the
    /// watcher is parked in `AppState` and re-pointed per turn by
    /// [`align_git_watcher`](Self::align_git_watcher).
    pub fn init_git_watcher(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<SessionWorkspace>> {
        match GitHeadWatcher::spawn() {
            Ok((watcher, rx)) => {
                *self._git_watcher.lock().unwrap() = Some(watcher);
                Some(rx)
            }
            Err(e) => {
                tracing::warn!(error = %e, "git head watcher unavailable");
                None
            }
        }
    }

    /// Aim the git HEAD watcher at the active session's `root`, mirroring
    /// [`align_session_mcp`](Self::align_session_mcp). Idempotent and
    /// best-effort: a no-op when the watcher could not start or `root` is unchanged.
    pub fn align_git_watcher(&self, root: &Path) {
        if let Some(w) = self._git_watcher.lock().unwrap().as_mut() {
            w.re_point(root);
        }
    }

    /// Stop and remove all background processes owned by `session_id` (#218).
    /// Fire-and-forget: the session is already gone from the store; process
    /// cleanup is best-effort and must not block the UI thread.
    pub fn reap_session_processes(&self, session_id: &str) {
        let sup = self.process_supervisor.clone();
        let id = session_id.to_owned();
        // `tauri::async_runtime::spawn` (not bare `tokio::spawn`) so this is safe to
        // call from the synchronous `delete_session` command, which Tauri runs off
        // the reactor on macOS (#117). A bare `tokio::spawn` there panics with "no
        // reactor running", and the unwind through the command FFI boundary takes
        // the whole app down (#471).
        tauri::async_runtime::spawn(async move {
            let n = sup.reap_session(&id).await;
            if n > 0 {
                tracing::info!(session_id = %id, reaped = n, "reaped session processes");
            }
        });
    }

    /// Start a periodic background reaper that drives
    /// [`ProcessSupervisor::reap_idle`] to remove finished processes and stop
    /// abandoned ones (started by the agent but never polled again) across all
    /// sessions. Fire-and-forget: the task lives for the app's lifetime on the
    /// shared Tokio runtime. Like [`init_mcp`](Self::init_mcp), it enters the
    /// runtime itself, so it's safe to call from Tauri's `setup` — which runs
    /// off-reactor on macOS (issue #117).
    pub fn start_process_reaper(&self) {
        let sup = self.process_supervisor.clone();
        // `tokio::spawn` needs an entered reactor; `setup` may not have one.
        let rt = tauri::async_runtime::handle();
        let _guard = rt.inner().enter();
        tokio::spawn(async move {
            // Scan every minute; reap processes unpolled for ten minutes. A
            // finished process is reaped on the first tick regardless of the idle
            // budget; a running one is only stopped once it has gone untouched.
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            let idle = Duration::from_secs(10 * 60);
            loop {
                interval.tick().await;
                let n = sup.reap_idle(idle).await;
                if n > 0 {
                    tracing::info!(reaped = n, "periodic process reaper cleaned up");
                }
            }
        });
    }

    /// Start the MCP host: begin watching `~/.flowforge/mcp.json` and spawn the
    /// lifecycle supervisor. Idempotent — a second call is a no-op so a re-`setup`
    /// can't double-spawn. Best-effort: a missing config dir or watcher failure is
    /// logged and leaves MCP disabled rather than failing app start (RFC 0003 §3,5).
    /// Safe to call from any thread: it enters the shared Tokio runtime itself, so
    /// callers (e.g. Tauri's `setup`, which runs outside an entered reactor on
    /// macOS) need not establish a runtime context.
    pub fn init_mcp(&self) {
        let Some(path) = ff_mcp::config_path() else {
            tracing::warn!("no home dir; mcp host disabled");
            return;
        };
        self.init_mcp_at(path);
    }

    /// Path-injectable core of [`init_mcp`](Self::init_mcp). Owns the runtime-enter
    /// so the supervisor's `tokio::spawn` always has a live reactor regardless of
    /// the calling thread (see issue #117). Separated so tests can drive it with a
    /// tempdir config path from a non-runtime thread.
    fn init_mcp_at(&self, path: PathBuf) {
        if self.mcp.lock().unwrap().is_some() {
            return;
        }
        // The supervisor actor is `tokio::spawn`'d; guarantee an entered reactor
        // regardless of the calling thread's context.
        let rt = tauri::async_runtime::handle();
        let _guard = rt.inner().enter();
        let (watcher, shared, change_rx) = match McpConfigWatcher::spawn(path.clone()) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "mcp config watcher unavailable");
                return;
            }
        };
        let handle =
            ff_mcp::spawn_supervisor(shared, change_rx, ff_mcp::SupervisorConfig::default());
        *self._mcp_watcher.lock().unwrap() = Some(watcher);
        *self.mcp.lock().unwrap() = Some(handle);
        *self.mcp_config_path.lock().unwrap() = Some(path);
    }

    /// Path to the watched `mcp.json`, or `None` before [`init_mcp`](Self::init_mcp)
    /// has run. The MCP control commands write back to this file (its edits then ride
    /// the existing config watcher into a reconcile).
    pub fn mcp_config_path(&self) -> Option<PathBuf> {
        self.mcp_config_path.lock().unwrap().clone()
    }

    /// A clone of the supervisor handle if MCP was successfully initialized; `None`
    /// when [`init_mcp`](Self::init_mcp) hasn't run or the watcher couldn't start.
    pub fn mcp_handle(&self) -> Option<SupervisorHandle> {
        self.mcp.lock().unwrap().clone()
    }

    /// The full connection registry (clone — callers never hold the lock).
    pub fn provider_registry(&self) -> ProviderRegistry {
        let mut reg = self.registry.lock().unwrap().clone();
        // Cross-check `has_key` against the live OS keychain. An app rebuild can
        // change the code-signing identity, after which the keychain ACL denies
        // the new binary and secrets written by the old build read back as
        // `None` — so `has_key` still says "key present" while auth fails. Flag
        // those connections so the UI can prompt to re-enter the key instead of
        // failing silently. Read-only probe over the clone; the persisted
        // registry is untouched, so `secret_missing` is never saved as `true`.
        for conn in &mut reg.connections {
            if conn.has_key && crate::secrets::present(conn.id.as_str()).is_empty() {
                conn.secret_missing = true;
            }
        }
        reg
    }

    /// Select the active connection by id; persists. `Err` on an unknown id.
    pub fn set_active_connection(&self, id: &str) -> Result<(), String> {
        let snapshot = {
            let mut reg = self.registry.lock().unwrap();
            reg.set_active(id)?;
            reg.clone()
        };
        save_registry(&snapshot);
        Ok(())
    }

    /// Add or update a connection (keyed by id, deriving one when blank); persists.
    /// Returns the stored connection so the caller sees the resolved id.
    pub fn upsert_connection(&self, conn: ProviderConnection) -> ProviderConnection {
        let (stored, snapshot) = {
            let mut reg = self.registry.lock().unwrap();
            let stored = reg.upsert(conn);
            (stored, reg.clone())
        };
        save_registry(&snapshot);
        stored
    }

    /// Remove a connection by id; persists. `Err` when removing the last one.
    pub fn remove_connection(&self, id: &str) -> Result<(), String> {
        let snapshot = {
            let mut reg = self.registry.lock().unwrap();
            reg.remove(id)?;
            reg.clone()
        };
        save_registry(&snapshot);
        Ok(())
    }

    /// Store a provider secret of `kind` for connection `conn_id` in the OS keychain,
    /// then flip that connection's `has_key` flag and persist. The secret value never
    /// enters the registry — only the coarse flag does. `Err` (without writing the
    /// secret) on an unknown connection id.
    pub fn set_connection_secret(
        &self,
        conn_id: &str,
        kind: SecretKind,
        value: &str,
    ) -> Result<(), String> {
        // Validate the id before any keychain write, but never hold the registry
        // lock across keychain I/O (#202 follow-up).
        {
            let reg = self.registry.lock().unwrap();
            if !reg.connections.iter().any(|c| c.id == conn_id) {
                return Err(format!("unknown connection: {conn_id}"));
            }
        }
        crate::secrets::set(conn_id, kind, value)?;
        let snapshot = {
            let mut reg = self.registry.lock().unwrap();
            if let Some(conn) = reg.connections.iter_mut().find(|c| c.id == conn_id) {
                conn.has_key = true;
            }
            reg.clone()
        };
        save_registry(&snapshot);
        Ok(())
    }

    /// Remove the secret of `kind` for `conn_id`, recompute `has_key` from the
    /// remaining stored secrets, and persist. Idempotent. `Err` on an unknown id.
    pub fn clear_connection_secret(&self, conn_id: &str, kind: SecretKind) -> Result<(), String> {
        // Validate the id before touching the keychain (#202 follow-up, mirrors set).
        {
            let reg = self.registry.lock().unwrap();
            if !reg.connections.iter().any(|c| c.id == conn_id) {
                return Err(format!("unknown connection: {conn_id}"));
            }
        }
        crate::secrets::clear(conn_id, kind)?;
        let has_key = SecretKind::ALL
            .iter()
            .any(|k| crate::secrets::get(conn_id, *k).is_some());
        let snapshot = {
            let mut reg = self.registry.lock().unwrap();
            if let Some(conn) = reg.connections.iter_mut().find(|c| c.id == conn_id) {
                conn.has_key = has_key;
            }
            reg.clone()
        };
        save_registry(&snapshot);
        Ok(())
    }

    /// The secret kinds currently stored for a connection (#320), so the UI can
    /// render Stored/Clear per field instead of off the coarse `has_key` flag.
    /// Presence only — no secret value leaves the backend. `Err` on an unknown id.
    pub fn connection_secret_presence(&self, conn_id: &str) -> Result<Vec<SecretKind>, String> {
        {
            let reg = self.registry.lock().unwrap();
            if !reg.connections.iter().any(|c| c.id == conn_id) {
                return Err(format!("unknown connection: {conn_id}"));
            }
        }
        Ok(crate::secrets::present(conn_id))
    }

    /// The concrete Bedrock auth a connection resolves to right now (#320): the
    /// explicit mode if pinned, otherwise the `Auto` precedence winner against live
    /// keychain presence. `None` for non-Bedrock connections or an unknown id, so
    /// the UI can flag which credential field is actually "active".
    pub fn resolved_bedrock_auth(&self, conn_id: &str) -> Option<BedrockAuth> {
        let conn = {
            let reg = self.registry.lock().unwrap();
            reg.connections.iter().find(|c| c.id == conn_id).cloned()?
        };
        if conn.kind != ProviderKind::Bedrock {
            return None;
        }
        Some(match conn.auth_mode.unwrap_or(BedrockAuth::Auto) {
            BedrockAuth::Auto => resolve_bedrock_auth(&conn),
            other => other,
        })
    }

    /// Current provider settings projected from the active connection (clone —
    /// callers never hold the lock). Legacy shim kept during the FE cutover (#126).
    pub fn provider_config(&self) -> ProviderConfig {
        let reg = self.registry.lock().unwrap();
        connection_to_config(&active_connection_or_default(&reg))
    }

    /// Apply legacy provider settings onto the active connection in place, then
    /// persist. Legacy shim kept during the FE cutover (#126).
    pub fn set_provider_config(&self, config: ProviderConfig) {
        let snapshot = {
            let mut reg = self.registry.lock().unwrap();
            let active_id = reg.active.clone();
            if let Some(conn) = reg.connections.iter_mut().find(|c| c.id == active_id) {
                conn.kind = config.kind;
                conn.base_url = config.base_url;
                conn.model = config.model;
                conn.has_key = config.has_key;
                conn.thinking = config.thinking;
            }
            reg.clone()
        };
        save_registry(&snapshot);
    }

    /// Current web-search settings (clone — callers never hold the lock).
    pub fn search_config(&self) -> SearchConfig {
        self.search_config.lock().unwrap().clone()
    }

    /// Replace and persist web-search settings. Visible to the `web_search` tool on
    /// its next call (they share the `Arc`).
    pub fn set_search_config(&self, config: SearchConfig) {
        save_search_config(&config);
        *self.search_config.lock().unwrap() = config;
    }

    /// Build a provider + model snapshot from the active connection for one turn.
    pub fn build_provider(&self) -> (Box<dyn Provider>, String) {
        self.build_provider_for(None, None)
    }

    /// Build a provider + model snapshot for a specific connection (`None` = the
    /// active one) running `model` (`None` = the connection's own default model).
    /// `send_message` passes the *resolved* model so the provider's wire-strip
    /// capabilities match the model actually running (RFC 0005 §11.3); `list_models`
    /// probes a connection with `None`. Returns the provider + the model it runs.
    pub fn build_provider_for(
        &self,
        id: Option<&str>,
        model: Option<&str>,
    ) -> (Box<dyn Provider>, String) {
        let conn = {
            let reg = self.registry.lock().unwrap();
            match id {
                Some(id) => reg.connections.iter().find(|c| c.id == id).cloned(),
                None => reg.active_connection().cloned(),
            }
            .unwrap_or_else(|| active_connection_or_default(&reg))
        };
        let resolved_model = model.unwrap_or(&conn.model).to_string();
        (build_provider(&conn, &resolved_model), resolved_model)
    }

    /// Resolve the `(connection, model)` for a turn using RFC 0005 §11.2 three-tier
    /// precedence: session > phenotype > global. An explicit per-session selection
    /// (set via [`set_session_model`](Self::set_session_model)) wins outright -- it is
    /// already a coherent `(connection, model)` pair. Absent that, this resolves the
    /// phenotype binding over the globally active connection. `model` falls back to the
    /// *resolved connection's* own model, never a foreign tier's, so a phenotype model
    /// override can never ride the wrong endpoint -- the latent bug in RFC 0005 §11.1.
    pub fn resolve_model_selection(&self, session_id: &str) -> ResolvedModel {
        let (connection, model) = if let Some(sel) = self.store.session_model(session_id) {
            (sel.connection, sel.model)
        } else {
            let pheno = self.session_phenotype(session_id);
            let reg = self.registry.lock().unwrap();
            let connection = pheno.provider.clone().unwrap_or_else(|| reg.active.clone());
            let model = pheno.model.clone().unwrap_or_else(|| {
                reg.connections
                    .iter()
                    .find(|c| c.id == connection)
                    .map(|c| c.model.clone())
                    .unwrap_or_else(|| active_connection_or_default(&reg).model)
            });
            (connection, model)
        };
        // Derive attachment caps from the resolved `(kind, model)` (RFC 0005 §11.3),
        // single-sourcing them at the resolution point. Fail-closed when the
        // connection dangles (no kind -> no caps), matching the gate's fail-closed.
        let kind = {
            let reg = self.registry.lock().unwrap();
            reg.connections
                .iter()
                .find(|c| c.id == connection)
                .map(|c| c.kind)
        };
        let supports_vision = kind.is_some_and(|k| model_supports_vision(k, &model));
        let supports_documents = kind.is_some_and(|k| model_supports_documents(k, &model));
        ResolvedModel {
            connection,
            model,
            supports_vision,
            supports_documents,
            // Served-window fields populated in the async IPC command via
            // [`served_window`](Self::served_window); the sync resolver leaves them
            // None so internal callers (turn building, tests) keep the cheap path.
            context_window: None,
            trained_context_window: None,
            context_window_source: None,
        }
    }

    /// Probe the served context window for the model `session_id` will run on
    /// (#602), memoized for [`SERVED_WINDOW_TTL`] per `(connection, model)`. The
    /// sync `resolve_model_selection` cannot do this -- the probe is HTTP -- so the
    /// IPC command awaits this and folds the result onto the `ResolvedModel` it
    /// returns. Ollama only; other kinds (and a dangling connection) yield an empty
    /// probe so the chip simply hides the readout.
    pub async fn served_window(&self, session_id: &str) -> ServedWindowProbe {
        let resolved = self.resolve_model_selection(session_id);
        let (kind, base_url) = {
            let reg = self.registry.lock().unwrap();
            match reg.connections.iter().find(|c| c.id == resolved.connection) {
                Some(c) => (c.kind, c.resolved_base_url().to_string()),
                None => return ServedWindowProbe::default(),
            }
        };
        if kind != ProviderKind::Ollama {
            return ServedWindowProbe::default();
        }
        let key = (resolved.connection.clone(), resolved.model.clone());
        // Fresh cache hit short-circuits the HTTP round-trip.
        if let Some((at, probe)) = self.served_window_cache.lock().unwrap().get(&key) {
            if at.elapsed() < SERVED_WINDOW_TTL {
                return probe.clone();
            }
        }
        let provider = OllamaProvider::new(base_url).with_num_ctx(ollama_num_ctx_from_env());
        let probe = provider.served_window(&resolved.model).await;
        self.served_window_cache
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), probe.clone()));
        probe
    }

    /// Resolve this session's MCP server set for a turn using RFC 0018 §3.2 three-tier
    /// precedence: session > phenotype > global. The sibling of
    /// [`resolve_model_selection`](Self::resolve_model_selection) -- one mental model
    /// for both. Composition is **whole-record override-by-id** (RFC §11.5): a later
    /// tier's entry replaces a lower tier's entry with the same id as a unit (command +
    /// args + env + scope), never a field-level merge. A tier may set `disabled: true`
    /// to suppress an inherited server for this turn; suppressed entries are filtered
    /// out of the resolved set. Insertion order is preserved (global first) for stable
    /// reconcile/bridge ordering.
    pub fn resolve_mcp_servers(&self, session_id: &str) -> Vec<McpServerConfig> {
        let mut resolved: Vec<McpServerConfig> = self
            .mcp_config_path()
            .and_then(|p| ff_mcp::load(&p).ok())
            .unwrap_or_default();

        let overlay = |resolved: &mut Vec<McpServerConfig>, srv: McpServerConfig| match resolved
            .iter_mut()
            .find(|e| e.id == srv.id)
        {
            Some(existing) => *existing = srv,
            None => resolved.push(srv),
        };

        for srv in self.session_phenotype(session_id).mcp_servers {
            overlay(&mut resolved, srv);
        }
        if let Some(session_servers) = self.store.session_mcp_servers(session_id) {
            for srv in session_servers {
                overlay(&mut resolved, srv);
            }
        }

        resolved.retain(|s| !s.disabled);
        resolved
    }

    pub fn register_cancel(&self, session_id: &str, token: CancelToken) {
        self.approvals
            .lock()
            .unwrap()
            .cancels
            .insert(session_id.to_string(), token);
    }

    pub fn take_cancel(&self, session_id: &str) -> Option<CancelToken> {
        self.approvals.lock().unwrap().cancels.remove(session_id)
    }

    /// Remove this session's cancel token only when it is still `expected` — the
    /// token THIS turn registered. A successor turn (the re-run that
    /// `edit_message` spawns after cancelling the in-flight one) may have already
    /// replaced it via `register_cancel`; removing unconditionally would strip
    /// the live turn's token, killing its Stop button and auto-denying its tool
    /// approvals. Identity is by shared flag (`CancelToken::ptr_eq`), not value.
    pub fn take_cancel_if(&self, session_id: &str, expected: &CancelToken) -> Option<CancelToken> {
        let mut reg = self.approvals.lock().unwrap();
        match reg.cancels.get(session_id) {
            Some(tok) if tok.ptr_eq(expected) => reg.cancels.remove(session_id),
            _ => None,
        }
    }

    /// A cheap clone of the current skill set, taken at turn start.
    pub fn skills_snapshot(&self) -> SkillRegistry {
        self.skills.read().unwrap().clone()
    }

    /// The active skill names, name-sorted (BTreeSet order).
    pub fn active_skills(&self) -> Vec<String> {
        self.active_skills.lock().unwrap().iter().cloned().collect()
    }

    /// Record that `skill` was active at the start of a turn (RFC 0001 §8).
    pub fn record_skill_activated(&self, skill: &str) {
        self.signals.lock().unwrap().record_activated(skill);
    }

    /// Fold a finished turn's metrics into `skill`'s aggregate (RFC 0001 §8).
    pub fn record_skill_completed(&self, ev: &SkillCompleted) {
        self.signals.lock().unwrap().record_completed(ev);
    }

    /// The telemetry aggregate for one skill, if any signals have been recorded.
    pub fn skill_telemetry(&self, skill: &str) -> Option<SkillAggregate> {
        self.signals.lock().unwrap().aggregate(skill)
    }

    /// Persist the telemetry aggregates once, at turn end. Snapshots the serialized
    /// payload under the lock, then drops the lock before the synchronous file write
    /// so `get_skill_telemetry` and concurrent turns never block on I/O (addresses
    /// #77 review nit 1). Best-effort: a write failure is logged and ignored.
    pub fn persist_signals(&self) {
        let payload = self.signals.lock().unwrap().snapshot_payload();
        SignalStore::persist_payload(payload);
    }

    /// Add a skill to the active set. Errors if no installed skill has this name
    /// so the UI/agent can't activate a phantom. Idempotent for an already-active
    /// skill.
    pub fn activate_skill(&self, name: &str) -> Result<(), String> {
        if self.skills.read().unwrap().get(name).is_none() {
            return Err(format!("unknown skill: {name}"));
        }
        self.active_skills.lock().unwrap().insert(name.to_string());
        Ok(())
    }

    /// Remove a skill from the active set. Idempotent — deactivating a skill that
    /// isn't active is a no-op.
    pub fn deactivate_skill(&self, name: &str) {
        self.active_skills.lock().unwrap().remove(name);
    }

    /// Drop active entries that are no longer installed (e.g. after an uninstall),
    /// so the active set never names a missing skill. Called after a reload.
    pub fn prune_active_skills(&self) {
        let known: BTreeSet<String> = self
            .skills
            .read()
            .unwrap()
            .names()
            .into_iter()
            .map(str::to_string)
            .collect();
        self.active_skills
            .lock()
            .unwrap()
            .retain(|n| known.contains(n));
    }

    /// All selectable phenotypes: the built-in `default` plus every definition in
    /// `~/.flowforge/phenos/`, name-sorted. Re-scanned per call (few files).
    pub fn list_phenotypes(&self) -> Vec<Phenotype> {
        let (map, errors) = load_phenotypes(&phenotypes_root());
        for e in &errors {
            tracing::warn!(error = %e, "phenotype load");
        }
        let mut out = vec![default_phenotype()];
        out.extend(map.into_values().filter(|p| p.name != DEFAULT_PHENOTYPE));
        out
    }

    /// The active phenotype (clone — never hold the lock).
    pub fn active_phenotype(&self) -> Phenotype {
        self.active_phenotype.lock().unwrap().clone()
    }

    /// Switch to `name`: applies its skills and overrides, then persists the choice.
    /// Errors on an unknown phenotype. Returns the resolved phenotype so the caller
    /// can report what is now active.
    pub fn switch_phenotype(&self, name: &str) -> Result<Phenotype, String> {
        let pheno = resolve_phenotype(name).ok_or_else(|| format!("unknown phenotype: {name}"))?;
        let skills = self.apply_phenotype(pheno.clone());
        self.warn_missing_skill_mcp(&pheno.name, &skills);
        save_active_phenotype_name(&pheno.name);
        Ok(pheno)
    }

    /// Persist an edited phenotype and, when it is the one currently active, re-apply
    /// it so the change takes effect immediately (RFC 0005 Phase D / #525). The
    /// built-in `default` is immutable and rejected by [`save_phenotype`]. A
    /// `provider` binding is validated against the live registry up front -- mirroring
    /// [`set_session_model`](Self::set_session_model) -- so the editor cannot pin a
    /// phantom connection. Returns the saved phenotype.
    pub fn update_phenotype(&self, pheno: Phenotype) -> Result<Phenotype, String> {
        if let Some(ref provider) = pheno.provider {
            let reg = self.registry.lock().unwrap();
            if !reg.connections.iter().any(|c| &c.id == provider) {
                return Err(format!("unknown connection: {provider}"));
            }
        }
        save_phenotype(&phenotypes_root(), &pheno).map_err(|e| e.to_string())?;
        if pheno.name == self.active_phenotype().name {
            let skills = self.apply_phenotype(pheno.clone());
            self.warn_missing_skill_mcp(&pheno.name, &skills);
        }
        Ok(pheno)
    }

    /// The active phenotype's model override, if any (replaces the provider config's
    /// model for the turn).
    pub fn active_model_override(&self) -> Option<String> {
        self.active_phenotype.lock().unwrap().model.clone()
    }

    /// Resolve the phenotype a *session* runs as (#246). A session with an explicit
    /// binding (set via [`set_session_phenotype`](Self::set_session_phenotype)) gets
    /// that phenotype; an unbound session — or one whose bound name no longer exists
    /// — inherits the global active phenotype. This is the single source of truth a
    /// turn uses to derive persona / skills / model / `max_iterations`, so two
    /// panes can run different Phenos simultaneously.
    pub fn session_phenotype(&self, session_id: &str) -> Phenotype {
        match self.store.session_phenotype(session_id) {
            Some(name) => resolve_phenotype(&name).unwrap_or_else(|| {
                tracing::warn!(
                    phenotype = %name,
                    session = %session_id,
                    "session bound to unknown phenotype; inheriting global active"
                );
                self.active_phenotype()
            }),
            None => self.active_phenotype(),
        }
    }

    /// Active skills for a *turn* (#246). Resolution differs from the persona/model
    /// path: only a session with an **explicit** phenotype binding uses that
    /// phenotype's declared skills. An unbound session keeps the global active set
    /// (`active_skills`) so the command palette (`activate_skill` /
    /// `deactivate_skill`) still takes effect on the next turn — otherwise a manual
    /// activation would silently stop reaching the agent while the palette still
    /// showed the skill active.
    pub fn turn_active_skills(&self, session_id: &str) -> Vec<String> {
        if self.store.session_phenotype(session_id).is_some() {
            self.resolve_skills(&self.session_phenotype(session_id))
                .into_iter()
                .collect()
        } else {
            self.active_skills()
        }
    }

    /// Bind `session_id` to a phenotype by name, or clear it (`None`) so the session
    /// inherits the global active one again (#246). Validates the name against the
    /// phenotype registry up front so a stale UI cannot bind a phantom — unknown
    /// names error rather than silently falling back. No-op-safe for the binding
    /// itself: clearing always succeeds.
    pub fn set_session_phenotype(
        &self,
        session_id: &str,
        name: Option<String>,
    ) -> Result<(), String> {
        if let Some(ref n) = name {
            let pheno = resolve_phenotype(n).ok_or_else(|| format!("unknown phenotype: {n}"))?;
            let skills = self.resolve_skills(&pheno);
            self.warn_missing_skill_mcp(&pheno.name, &skills);
        }
        self.store.set_session_phenotype(session_id, name);
        Ok(())
    }

    /// The global default mode (#265). Factory value [`Mode::Auto`].
    pub fn default_mode(&self) -> Mode {
        *self.default_mode.lock().unwrap()
    }

    /// Set and persist the global default mode (#265).
    pub fn set_default_mode(&self, mode: Mode) {
        *self.default_mode.lock().unwrap() = mode;
        save_default_mode(mode);
    }

    /// Resolve the mode a turn for `session_id` runs as (#265): an explicit per-pane
    /// binding, else the global [`default_mode`](Self::default_mode). Mirrors
    /// [`session_phenotype`](Self::session_phenotype).
    pub fn session_mode(&self, session_id: &str) -> Mode {
        self.store
            .session_mode(session_id)
            .unwrap_or_else(|| self.default_mode())
    }

    /// Bind `session_id` to a mode, or clear it (`None`) so the session inherits the
    /// global default again (#265).
    pub fn set_session_mode(&self, session_id: &str, mode: Option<Mode>) {
        self.store.set_session_mode(session_id, mode);
    }

    /// The session's explicit per-pane model selection, or `None` if it inherits the
    /// phenotype's model (#499). Raw passthrough; resolution lives in
    /// [`resolve_model_selection`](Self::resolve_model_selection).
    pub fn session_model(&self, session_id: &str) -> Option<ModelSelection> {
        self.store.session_model(session_id)
    }

    /// Bind `session_id` to an explicit `(connection, model)` selection, or clear it
    /// (`None`) so the session inherits its phenotype's model again (#499). Validates
    /// the connection id against the live registry up front so a stale UI cannot pin a
    /// phantom endpoint -- unknown ids error rather than silently riding the wrong
    /// connection. Clearing always succeeds.
    pub fn set_session_model(
        &self,
        session_id: &str,
        selection: Option<ModelSelection>,
    ) -> Result<(), String> {
        if let Some(ref sel) = selection {
            let reg = self.registry.lock().unwrap();
            if !reg.connections.iter().any(|c| c.id == sel.connection) {
                return Err(format!("unknown connection: {}", sel.connection));
            }
        }
        self.store.set_session_model(session_id, selection);
        Ok(())
    }
}

impl AppState {
    /// Reserve a slot for a UI approval prompt. The caller awaits the returned
    /// receiver; the matching `resolve_approval` (or `cancel_pending_approvals` on
    /// cancel) wakes it.
    ///
    /// If the session has no live cancel token — the turn was already cancelled, or
    /// is being torn down — the prompt is refused: the sender is dropped before
    /// returning, so `rx.await` yields `Err`, which the approver treats as a deny.
    /// This closes the TOCTOU where a cancel lands between the agent loop's
    /// cancellation check and this registration, which would otherwise orphan the
    /// sender and hang the awaiting approver.
    pub fn register_approval(&self, session_id: &str, call_id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        let mut reg = self.approvals.lock().unwrap();
        if reg.cancels.contains_key(session_id) {
            reg.pending
                .insert((session_id.to_string(), call_id.to_string()), tx);
        }
        // else: `tx` drops here -> `rx` resolves to `Err` -> deny.
        rx
    }

    /// Deliver the user's decision. An unknown `(session_id, call_id)` is a no-op
    /// (race: cancel raced the click).
    pub fn resolve_approval(&self, session_id: &str, call_id: &str, approved: bool) {
        let key = (session_id.to_string(), call_id.to_string());
        if let Some(tx) = self.approvals.lock().unwrap().pending.remove(&key) {
            // Receiver may have been dropped (cancel raced); ignore the send error.
            let _ = tx.send(approved);
        }
    }

    /// Drop every pending approval AND pending question for this session — dropping
    /// the sender resolves the awaiting future with `Err`, which the approver
    /// translates to a deny / a dismissed question. Called on turn cancel so neither
    /// an open approval nor an open `ask_user` can hang the turn.
    pub fn cancel_pending_approvals(&self, session_id: &str) {
        let mut reg = self.approvals.lock().unwrap();
        reg.pending.retain(|(sid, _), _| sid != session_id);
        reg.pending_asks.retain(|(sid, _), _| sid != session_id);
    }

    /// Clear a session's "Allow this session" allowlist (#229). Called ONLY on
    /// session delete -- never on turn cancel, which would silently revoke a
    /// grant the user expects to last until the session ends.
    pub fn clear_session_approvals(&self, session_id: &str) {
        self.approvals
            .lock()
            .unwrap()
            .session_approved
            .remove(session_id);
    }

    /// Reserve a slot for an `ask_user` question (#44). Mirrors `register_approval`:
    /// the same TOCTOU liveness guard refuses (drops the sender -> `Err` -> `None`
    /// dismissed) when the session has no live turn, so a cancel racing registration
    /// can never orphan the sender.
    pub fn register_ask(&self, session_id: &str, call_id: &str) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        let mut reg = self.approvals.lock().unwrap();
        if reg.cancels.contains_key(session_id) {
            reg.pending_asks
                .insert((session_id.to_string(), call_id.to_string()), tx);
        }
        // else: `tx` drops here -> `rx` resolves to `Err` -> the question is dismissed.
        rx
    }

    /// Deliver the user's answer to an awaiting `ask_user`. An unknown
    /// `(session_id, call_id)` is a no-op (race: cancel raced the submit).
    pub fn resolve_ask(&self, session_id: &str, call_id: &str, answer: String) {
        let key = (session_id.to_string(), call_id.to_string());
        if let Some(tx) = self.approvals.lock().unwrap().pending_asks.remove(&key) {
            let _ = tx.send(answer);
        }
    }

    // --- Four-option approval (#229) ---

    /// Check if a tool is always-approved (global persistent allowlist).
    pub fn is_always_approved(&self, tool: &str) -> bool {
        self.approvals.lock().unwrap().is_always_approved(tool)
    }

    /// Check if a tool is session-approved for the given session.
    pub fn is_session_approved(&self, session_id: &str, tool: &str) -> bool {
        self.approvals
            .lock()
            .unwrap()
            .is_session_approved(session_id, tool)
    }

    /// Mark a tool as approved for this session only.
    pub fn set_session_approve(&self, session_id: &str, tool: &str) {
        self.approvals
            .lock()
            .unwrap()
            .set_session_approve(session_id, tool);
    }

    /// Add a tool to the global always-approved set and persist.
    pub fn set_always_approve(&self, tool: &str) {
        let set = {
            let mut reg = self.approvals.lock().unwrap();
            reg.set_always_approve(tool);
            reg.always_approved.clone()
        };
        if let Some(path) = tool_permissions_path() {
            if let Err(e) = ApprovalRegistry::save_always_approved(&path, &set) {
                tracing::warn!(error = %e, "failed to persist tool_permissions.json");
            }
        }
    }

    /// Remove a tool from the global always-approved set and persist.
    pub fn remove_always_approve(&self, tool: &str) {
        let set = {
            let mut reg = self.approvals.lock().unwrap();
            reg.remove_always_approve(tool);
            reg.always_approved.clone()
        };
        if let Some(path) = tool_permissions_path() {
            if let Err(e) = ApprovalRegistry::save_always_approved(&path, &set) {
                tracing::warn!(error = %e, "failed to persist tool_permissions.json");
            }
        }
    }

    /// List all globally always-approved tools, sorted.
    pub fn list_always_approved(&self) -> Vec<String> {
        self.approvals.lock().unwrap().list_always_approved()
    }

    /// Whether the #229 allowlist pre-approves this call without a prompt. The
    /// allowlist is keyed by tool name only, so it never covers `Dangerous`
    /// invocations -- those always re-prompt regardless of any "Always Allow" or
    /// "Allow this session" grant on the tool.
    pub fn allowlist_covers(&self, session_id: &str, tool: &str, safety: Safety) -> bool {
        safety != Safety::Dangerous
            && (self.is_always_approved(tool) || self.is_session_approved(session_id, tool))
    }
}

/// The default workspace root for M2: the user's home directory, falling back to the
/// process CWD. Replaced by a per-session, user-chosen folder in M3.
/// `~/.flowforge/skills`, the directory the skill watcher loads and watches.
fn skills_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flowforge")
        .join("skills")
}

/// `~/.flowforge/skill_history`, the retained-version tree for skill evolution (RFC
/// 0001 §8). Deliberately a sibling of `skills/`, not a child: the registry scans
/// every top-level dir under `skills/`, so version copies must live elsewhere.
fn skill_history_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flowforge")
        .join("skill_history")
}

/// `~/.flowforge/skill_signals.json`, the per-skill telemetry aggregate store (RFC
/// 0001 §8). Lives under `~/.flowforge/` with the skills it describes, not the
/// platform config dir (that's for user *settings* like the provider/phenotype).
fn signals_path() -> Option<PathBuf> {
    dirs::home_dir().map(|d| d.join(".flowforge").join("skill_signals.json"))
}

/// Load the persisted telemetry aggregates, or an in-memory-only store when there is
/// no home dir. Best-effort: a missing/corrupt file starts empty.
fn load_signals() -> SignalStore {
    match signals_path() {
        Some(path) => SignalStore::load(path),
        None => SignalStore::new(),
    }
}

/// Start the skill watcher, falling back to a one-shot load (no hot-reload) if the
/// OS watcher cannot start (e.g. the skills dir does not exist yet).
fn load_skills() -> (Option<SkillWatcher>, SharedRegistry) {
    let root = skills_root();
    match SkillWatcher::spawn(root.clone()) {
        Ok((watcher, shared, errors)) => {
            for e in &errors {
                tracing::warn!(error = %e, "skill load");
            }
            (Some(watcher), shared)
        }
        Err(e) => {
            tracing::warn!(error = %e, "skill watcher unavailable; loading once");
            let (reg, _) = SkillRegistry::load_dir(&root);
            (None, Arc::new(RwLock::new(reg)))
        }
    }
}

/// How much the transcript must grow (in messages) before a session that is still
/// over budget is flushed again. Keeps a long over-budget session from flushing
/// every turn while still capturing durable facts stated much later (RFC 0006 §7.2).
const REFLUSH_INTERVAL_MESSAGES: u64 = 40;

/// Current wall-clock time as Unix epoch milliseconds.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Build the durable-memory store, its recall index, and a debounced reindex
/// watcher (RFC 0006). Best-effort: the index is a derived cache, so if it can't
/// open on disk we fall back to an in-memory index, and if even that fails the
/// Memory config sourced from the environment. Persisted Settings (the Memory
/// pane, issue #131) is not wired yet, so until it lands semantic recall is opt-in
/// via `FF_MEMORY_EMBEDDINGS=1` (truthy). Everything else keeps `MemoryConfig`
/// defaults: memory on, embeddings off -> pure FTS5/BM25.
fn memory_config_from_env() -> MemoryConfig {
    let mut config = MemoryConfig::default();
    if env_flag("FF_MEMORY_EMBEDDINGS") {
        config.embeddings.enabled = true;
        config.embeddings.provider = EmbeddingProvider::Local;
    }
    config
}

/// Tier-2 abstractive cold-tail summary config from the environment (RFC 0016
/// M7.0). Default-off; opt in with `FF_COMPACT_ABSTRACTIVE`. The summarizer model
/// defaults to the session model and is overridable (same provider/connection)
/// with `FF_COMPACT_ABSTRACTIVE_MODEL`; the fire fraction is tunable via
/// `FF_COMPACT_ABSTRACTIVE_AT` (a 0..1 budget fraction).
pub(crate) fn abstractive_config_from_env() -> AbstractiveConfig {
    let mut config = AbstractiveConfig {
        enabled: env_flag("FF_COMPACT_ABSTRACTIVE"),
        ..AbstractiveConfig::default()
    };
    if let Some(model) = std::env::var("FF_COMPACT_ABSTRACTIVE_MODEL")
        .ok()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
    {
        config.model = Some(model);
    }
    if let Some(at) = std::env::var("FF_COMPACT_ABSTRACTIVE_AT")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
    {
        config.fire_at_fraction = at;
    }
    config
}

/// The `(base_url, model, api_key)` for a local embedder, or `None` to stay on the
/// BM25 floor. Returns `Some` only when embeddings are enabled, the provider is
/// `Local`, and a model is configured (`FF_MEMORY_EMBEDDINGS_MODEL`) -- an
/// embedding endpoint needs a real embedding model, so without one we log once and
/// fall back rather than spamming a chat-only server. Base URL defaults to the
/// local candle-vLLM endpoint; the API key is reserved for the M5.3.2 cloud path.
fn local_embedder_from_env(config: &MemoryConfig) -> Option<(String, String, Option<String>)> {
    if !config.embeddings.enabled || config.embeddings.provider != EmbeddingProvider::Local {
        return None;
    }
    let model = std::env::var("FF_MEMORY_EMBEDDINGS_MODEL")
        .ok()
        .filter(|m| !m.trim().is_empty());
    let Some(model) = model else {
        tracing::warn!(
            "memory embeddings enabled but FF_MEMORY_EMBEDDINGS_MODEL is unset; staying on BM25"
        );
        return None;
    };
    let base = std::env::var("FF_MEMORY_EMBEDDINGS_BASE_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "http://localhost:8000/v1".to_string());
    let key = std::env::var("FF_MEMORY_EMBEDDINGS_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty());
    tracing::info!(base_url = %base, model = %model, "memory semantic recall enabled (local embedder)");
    Some((base, model, key))
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// `memory_*` tools degrade to no-ops rather than failing app start. The initial
/// full reindex happens here (not in the watcher) so a build error is logged once.
fn build_memory() -> (Arc<Memory>, Arc<dyn MemoryIndex>, Option<MemoryWatcher>) {
    let config = memory_config_from_env();
    let embedder = local_embedder_from_env(&config);
    let decay = config.decay.clone();
    let memory = Arc::new(Memory::with_default_root(config));
    let wrap = |i: Fts5Index| -> Arc<dyn MemoryIndex> {
        match &embedder {
            Some((base, model, key)) => Arc::new(HybridIndex::new(
                i,
                OpenAiEmbedder::new(base, model.clone(), key.clone()),
            )),
            None => Arc::new(HybridIndex::new(i, NoopEmbedder)),
        }
    };
    let index: Arc<dyn MemoryIndex> = match Fts5Index::open(memory.index_path()) {
        Ok(i) => wrap(i.with_decay(decay.clone())),
        Err(e) => {
            tracing::warn!(error = %e, "memory index unavailable on disk; using in-memory");
            match Fts5Index::open_in_memory() {
                Ok(i) => wrap(i.with_decay(decay.clone())),
                Err(e) => {
                    tracing::warn!(error = %e, "memory index unavailable; recall disabled");
                    return (memory, Arc::new(NullIndex), None);
                }
            }
        }
    };
    let chunks = memory.all_chunks();
    if embedder.is_some() {
        // With embeddings on, the full reindex is a serial blocking-HTTP loop
        // (one embed call per chunk), so doing it inline would stall app launch
        // on the embedding server. Run it off the startup path: recall stays
        // available from the persisted on-disk FTS index and embeddings backfill
        // shortly after (PR #215 review, nit 1).
        let bg = index.clone();
        std::thread::spawn(move || match bg.reindex(&chunks) {
            Ok(()) => tracing::info!("memory embeddings reindex complete"),
            Err(e) => tracing::warn!(error = %e, "background memory reindex failed"),
        });
    } else if let Err(e) = index.reindex(&chunks) {
        tracing::warn!(error = %e, "initial memory reindex failed");
    }
    let watcher = MemoryWatcher::spawn(memory.clone(), index.clone())
        .map_err(|e| tracing::warn!(error = %e, "memory watcher unavailable"))
        .ok();
    (memory, index, watcher)
}

/// A do-nothing recall index used only when SQLite is entirely unavailable, so the
/// `memory_*` tools return empty results instead of erroring the turn.
struct NullIndex;

impl MemoryIndex for NullIndex {
    fn reindex(&self, _chunks: &[ff_memory::MemoryChunk]) -> ff_memory::Result<()> {
        Ok(())
    }
    fn reindex_path(
        &self,
        _path: &Path,
        _chunks: &[ff_memory::MemoryChunk],
    ) -> ff_memory::Result<()> {
        Ok(())
    }
    fn remove_path(&self, _path: &Path) -> ff_memory::Result<()> {
        Ok(())
    }
    fn search(&self, _query: &str, _k: usize) -> ff_memory::Result<Vec<ff_memory::ScoredChunk>> {
        Ok(Vec::new())
    }
}

/// The default working directory for a session whose cwd is unset: `~/.flowforge/
/// workspaces/`, created on first use. Falls back to the home directory if the
/// directory cannot be created, and to the process CWD when there is no home dir.
/// A user-chosen folder (the per-session picker, #200) overrides this.
fn default_workspace_root() -> PathBuf {
    default_workspace_root_in(dirs::home_dir())
}

/// Testable core of [`default_workspace_root`] with the home dir injected.
fn default_workspace_root_in(home: Option<PathBuf>) -> PathBuf {
    if let Some(home) = home {
        let workspaces = home.join(".flowforge").join("workspaces");
        if fs::create_dir_all(&workspaces).is_ok() {
            return workspaces;
        }
        return home;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Re-scan root and swap the shared registry's contents. Shared by the installer
/// tools and the install/uninstall commands so a change is visible immediately,
/// independent of the filesystem watcher.
pub fn reload_registry(root: &Path, registry: &SharedRegistry) {
    let (next, errors) = SkillRegistry::load_dir(root);
    for e in &errors {
        tracing::warn!(error = %e, "skill reload after install");
    }
    if let Ok(mut guard) = registry.write() {
        *guard = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ff_core::{ReasoningEffort, ReasoningVisibility};
    use ff_llm::{ChatMessage, ChatRequest};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Approvals are only registered while a turn is live, so a cancel token must
    // exist for the session first — mirrors `send_message` registering the token
    // before the turn (and thus any approval) starts.
    fn arm(state: &AppState, session_id: &str) {
        state.register_cancel(session_id, CancelToken::new());
    }

    // #277: the desktop store persists across a "restart". `build_session_store`
    // delegates to `SessionStore::open`, so this exercises the same path-backed
    // contract over an explicit temp db (without touching the real config dir).
    #[test]
    fn session_db_survives_restart() {
        use ff_core::Role;
        use ff_session::SessionStore;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");

        let session_id = {
            let store = SessionStore::open(&path).unwrap();
            let s = store.create_session(Some("persist me".into()));
            store.add_message(&s.id, Role::User, "still here?".into());
            s.id
        };

        // A fresh store over the same path == an app restart.
        let store = SessionStore::open(&path).unwrap();
        let sessions = store.list_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);
        let msgs = store.get_messages(&session_id);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "still here?");
    }

    #[tokio::test]
    async fn resolve_approval_delivers_decision() {
        let state = AppState::new();
        arm(&state, "sess");
        let rx = state.register_approval("sess", "call-1");
        state.resolve_approval("sess", "call-1", true);
        assert!(rx.await.unwrap());
        // Slot is freed after resolve.
        assert!(state.approvals.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn cancel_pending_denies_via_drop() {
        let state = AppState::new();
        arm(&state, "sess-x");
        let rx = state.register_approval("sess-x", "call-2");
        state.cancel_pending_approvals("sess-x");
        // Sender was dropped -> RecvError -> caller treats as deny.
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn cancel_pending_only_affects_matching_session() {
        let state = AppState::new();
        arm(&state, "sess-a");
        arm(&state, "sess-b");
        let rx_a = state.register_approval("sess-a", "a");
        let rx_b = state.register_approval("sess-b", "b");
        state.cancel_pending_approvals("sess-a");
        // sess-b survives.
        state.resolve_approval("sess-b", "b", true);
        assert!(rx_a.await.is_err());
        assert!(rx_b.await.unwrap());
    }

    #[tokio::test]
    async fn resolve_unknown_call_is_noop() {
        let state = AppState::new();
        // Must not panic.
        state.resolve_approval("nope", "nope", true);
    }

    #[tokio::test]
    async fn register_without_live_cancel_denies_immediately() {
        let state = AppState::new();
        // No `register_cancel` -> the turn is gone (or never started). The TOCTOU
        // guard refuses the prompt: the receiver resolves to Err -> deny, and no
        // orphaned sender is left behind to hang the approver.
        let rx = state.register_approval("ghost", "call-x");
        assert!(rx.await.is_err());
        assert!(state.approvals.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn standalone_op_with_liveness_token_can_be_approved() {
        // Mirrors the install_skill flow: a non-turn op registers a liveness token
        // under its request_id so the TOCTOU guard admits it, keyed (id, id).
        let state = AppState::new();
        state.register_cancel("op", CancelToken::new());
        let rx = state.register_approval("op", "op");
        state.resolve_approval("op", "op", true);
        assert!(rx.await.unwrap());
        // The flow releases the liveness token afterward.
        assert!(state.take_cancel("op").is_some());
    }

    #[test]
    fn take_cancel_if_removes_only_its_own_token() {
        // The clean single-turn finish: the token a turn registered is still the
        // live one, so its epilogue removes it.
        let state = AppState::new();
        let token = CancelToken::new();
        state.register_cancel("sess", token.clone());
        assert!(
            state.take_cancel_if("sess", &token).is_some(),
            "a turn drops its own still-registered token"
        );
        assert!(state.take_cancel("sess").is_none(), "map is now empty");
    }

    #[test]
    fn take_cancel_if_leaves_a_successor_turns_token() {
        // The edit-during-live-turn race (#464/#468 blocker): turn A is cancelled
        // and a re-run turn B registers its token before A's epilogue runs. A must
        // NOT strip B's token, or B loses its Stop button and auto-denies tools.
        let state = AppState::new();
        let token_a = CancelToken::new();
        state.register_cancel("sess", token_a.clone());

        // Turn B replaces the session's token (mirrors edit_message -> spawn).
        let token_b = CancelToken::new();
        state.register_cancel("sess", token_b.clone());

        // Turn A's epilogue: identity check fails, B's token survives.
        assert!(
            state.take_cancel_if("sess", &token_a).is_none(),
            "A must not remove B's token"
        );
        // B's token is still live and removable by B's own epilogue.
        assert!(
            state.take_cancel_if("sess", &token_b).is_some(),
            "B drops its own token cleanly"
        );
    }

    #[tokio::test]
    async fn colliding_call_ids_across_sessions_are_isolated() {
        let state = AppState::new();
        arm(&state, "sess-1");
        arm(&state, "sess-2");
        // Same LLM-supplied call_id in two concurrent sessions must not collide.
        let rx1 = state.register_approval("sess-1", "dup");
        let rx2 = state.register_approval("sess-2", "dup");
        // Resolving one leaves the other intact.
        state.resolve_approval("sess-1", "dup", true);
        state.resolve_approval("sess-2", "dup", false);
        assert!(rx1.await.unwrap());
        assert!(!rx2.await.unwrap());
    }

    #[tokio::test]
    async fn ask_delivers_answer() {
        let state = AppState::new();
        arm(&state, "sess");
        let rx = state.register_ask("sess", "ask-1");
        state.resolve_ask("sess", "ask-1", "main.rs".to_string());
        assert_eq!(rx.await.unwrap(), "main.rs");
    }

    #[tokio::test]
    async fn cancel_dismisses_pending_ask_via_drop() {
        let state = AppState::new();
        arm(&state, "sess-x");
        let rx = state.register_ask("sess-x", "ask-2");
        state.cancel_pending_approvals("sess-x");
        // Sender dropped -> RecvError -> caller treats as a dismissed question.
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn register_ask_without_live_turn_dismisses_immediately() {
        let state = AppState::new();
        // No `arm` -> no live cancel token -> the TOCTOU guard refuses the slot.
        let rx = state.register_ask("ghost", "ask-3");
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn cancel_dismisses_ask_only_for_matching_session() {
        let state = AppState::new();
        arm(&state, "sess-a");
        arm(&state, "sess-b");
        let rx_a = state.register_ask("sess-a", "a");
        let rx_b = state.register_ask("sess-b", "b");
        state.cancel_pending_approvals("sess-a");
        state.resolve_ask("sess-b", "b", "kept".to_string());
        assert!(rx_a.await.is_err());
        assert_eq!(rx_b.await.unwrap(), "kept");
    }

    #[tokio::test]
    async fn resolve_unknown_ask_is_noop() {
        let state = AppState::new();
        state.resolve_ask("nope", "nope", "x".to_string());
    }

    #[test]
    fn activate_unknown_skill_errors() {
        let state = AppState::new();
        // `new()` restores the persisted phenotype from the real config dir, which
        // can pre-populate the active set; this test exercises only the activate
        // guard, so start from a known-empty baseline (test-isolation, not behavior).
        state.active_skills.lock().unwrap().clear();
        let err = state
            .activate_skill("definitely-not-installed-skill-xyz")
            .unwrap_err();
        assert!(err.contains("unknown skill"), "{err}");
        assert!(state.active_skills().is_empty());
    }

    #[test]
    fn active_skills_is_sorted_and_deduped() {
        let state = AppState::new();
        {
            // Bypass the install-check guard: this test exercises the accessor and
            // deactivate, not the registry lookup (covered elsewhere).
            let mut guard = state.active_skills.lock().unwrap();
            // Clear the phenotype `new()` restored from the real config dir so the
            // assertions below own the full expected set (test-isolation).
            guard.clear();
            guard.insert("zeta".into());
            guard.insert("alpha".into());
            guard.insert("alpha".into());
        }
        assert_eq!(state.active_skills(), vec!["alpha", "zeta"]);
        state.deactivate_skill("alpha");
        assert_eq!(state.active_skills(), vec!["zeta"]);
        // Deactivating an absent skill is a no-op.
        state.deactivate_skill("nope");
        assert_eq!(state.active_skills(), vec!["zeta"]);
    }

    // First-run phenotype selection (#298). A fake resolver lets us cover the whole
    // branch matrix without touching `~/.flowforge`. `codon` and `default` resolve;
    // anything else is unknown.
    fn fake_resolve(name: &str) -> Option<Phenotype> {
        match name {
            "codon" | "default" | "rust" => Some(Phenotype {
                name: name.to_string(),
                skills: vec![],
                model: None,
                persona: None,
                max_iterations: None,
                provider: None,
                mcp_servers: Vec::new(),
            }),
            _ => None,
        }
    }

    #[test]
    fn initial_phenotype_prefers_persisted_choice() {
        let pheno = initial_phenotype(Some("rust".to_string()), fake_resolve);
        assert_eq!(pheno.name, "rust");
    }

    #[test]
    fn initial_phenotype_defaults_to_codon_when_no_persisted_choice() {
        let pheno = initial_phenotype(None, fake_resolve);
        assert_eq!(pheno.name, "codon");
    }

    #[test]
    fn initial_phenotype_unknown_persisted_falls_through_to_codon() {
        let pheno = initial_phenotype(Some("ghost".to_string()), fake_resolve);
        assert_eq!(pheno.name, "codon");
    }

    #[test]
    fn initial_phenotype_falls_back_to_default_when_codon_absent() {
        // Codon not installed (rare seed-failure): resolver only knows `default`.
        let resolve = |name: &str| (name == "default").then(default_phenotype);
        let pheno = initial_phenotype(None, resolve);
        assert_eq!(pheno.name, DEFAULT_PHENOTYPE);
    }

    #[test]
    fn switch_to_unknown_phenotype_errors() {
        let state = AppState::new();
        let err = state
            .switch_phenotype("definitely-not-a-phenotype-xyz")
            .unwrap_err();
        assert!(err.contains("unknown phenotype"), "{err}");
    }

    #[test]
    fn default_phenotype_is_always_listed() {
        let state = AppState::new();
        let names: Vec<String> = state
            .list_phenotypes()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert!(names.contains(&DEFAULT_PHENOTYPE.to_string()), "{names:?}");
    }

    #[test]
    fn update_phenotype_rejects_immutable_default() {
        // Guard fires before any disk I/O -- the built-in `default` is never written.
        let state = AppState::new();
        let err = state.update_phenotype(default_phenotype()).unwrap_err();
        assert!(err.contains("immutable"), "{err}");
    }

    #[test]
    fn update_phenotype_rejects_unknown_provider() {
        // Provider binding is validated against the live registry before saving, so a
        // stale editor cannot pin a phantom connection (mirrors set_session_model).
        let state = AppState::new();
        let pheno = Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: None,
            persona: None,
            max_iterations: None,
            provider: Some("definitely-not-a-connection-xyz".into()),
            mcp_servers: Vec::new(),
        };
        let err = state.update_phenotype(pheno).unwrap_err();
        assert!(err.contains("unknown connection"), "{err}");
    }

    #[test]
    fn apply_phenotype_records_overrides_and_drops_unknown_skills() {
        let state = AppState::new();
        // No skills are installed in the test environment, so every named skill is
        // unknown and must be dropped — never activated as a phantom.
        let pheno = Phenotype {
            name: "rust".into(),
            skills: vec!["not-installed".into()],
            model: Some("qwen3-coder".into()),
            persona: Some("You are a Rust expert.".into()),
            max_iterations: None,
            provider: None,
            mcp_servers: Vec::new(),
        };
        state.apply_phenotype(pheno);
        assert!(state.active_skills().is_empty());
        assert_eq!(
            state.active_model_override().as_deref(),
            Some("qwen3-coder")
        );
        assert_eq!(
            state.active_phenotype().persona.as_deref(),
            Some("You are a Rust expert.")
        );
        assert_eq!(state.active_phenotype().name, "rust");
    }

    /// Build a `SkillRegistry` from a tempdir of `SKILL.md` fixtures and install it
    /// on `state`. Each spec is `(skill_name, declared_mcp_servers)`. The returned
    /// `TempDir` is only kept to satisfy ownership; `load_dir` reads eagerly.
    fn install_skills(state: &AppState, specs: &[(&str, &[&str])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, mcp) in specs {
            let skill_dir = dir.path().join(name);
            std::fs::create_dir_all(&skill_dir).unwrap();
            let mut fm = format!("---\nname: {name}\ndescription: test\nversion: 0.1.0\n");
            if !mcp.is_empty() {
                fm.push_str("mcp:\n");
                for s in *mcp {
                    fm.push_str(&format!("  - {s}\n"));
                }
            }
            fm.push_str("---\nbody\n");
            std::fs::write(skill_dir.join("SKILL.md"), fm).unwrap();
        }
        let (reg, errs) = SkillRegistry::load_dir(dir.path());
        assert!(errs.is_empty(), "skill fixtures failed to load: {errs:?}");
        *state.skills.write().unwrap() = reg;
        dir
    }

    #[test]
    fn skill_declared_mcp_servers_are_required() {
        let state = AppState::new();
        let _dir = install_skills(&state, &[("codegraph", &["codegraph"]), ("plain", &[])]);
        let skills: BTreeSet<String> = ["codegraph".into(), "plain".into()].into();
        // No MCP supervisor is initialized in tests, so a required server is always
        // reported missing — exactly the "server not in mcp.json" warn path.
        assert_eq!(
            state.missing_skill_mcp_servers(&skills),
            vec!["codegraph".to_string()]
        );
    }

    #[test]
    fn skill_without_mcp_requires_no_server() {
        let state = AppState::new();
        let _dir = install_skills(&state, &[("plain", &[])]);
        let skills: BTreeSet<String> = ["plain".into()].into();
        assert!(state.missing_skill_mcp_servers(&skills).is_empty());
    }

    #[test]
    fn unknown_active_skill_contributes_no_mcp_requirement() {
        let state = AppState::new();
        let skills: BTreeSet<String> = ["not-installed".into()].into();
        assert!(state.missing_skill_mcp_servers(&skills).is_empty());
    }

    #[test]
    fn missing_mcp_servers_are_deduped_and_sorted() {
        let state = AppState::new();
        let _dir = install_skills(
            &state,
            &[("a", &["zeta", "codegraph"]), ("b", &["codegraph"])],
        );
        let skills: BTreeSet<String> = ["a".into(), "b".into()].into();
        assert_eq!(
            state.missing_skill_mcp_servers(&skills),
            vec!["codegraph".to_string(), "zeta".to_string()]
        );
    }

    #[test]
    fn unavailable_skill_mcp_servers_is_empty_for_unknown_phenotype() {
        let state = AppState::new();
        // The query resolves the phenotype by name first; an unknown name yields no
        // requirement (the command never emits in that case).
        assert!(state
            .unavailable_skill_mcp_servers("definitely-not-a-phenotype")
            .is_empty());
    }

    #[test]
    fn unavailable_skill_mcp_servers_resolves_and_delegates_for_known_phenotype() {
        let state = AppState::new();
        // The default phenotype always resolves; no skills are installed in the test
        // environment, so it requires no MCP server. This exercises the resolve +
        // delegate path (the non-empty list building is covered by the
        // missing_skill_mcp_servers / unavailable_required_servers tests above).
        assert!(state
            .unavailable_skill_mcp_servers(DEFAULT_PHENOTYPE)
            .is_empty());
    }

    #[test]
    fn present_but_not_running_server_is_reported_unavailable() {
        let status = |id: &str, state: McpServerState| McpServerStatus {
            id: id.into(),
            state,
            tool_count: 0,
            last_error: None,
            restarts: 0,
            pid: None,
            scope_key: None,
        };
        let required: BTreeSet<String> =
            ["running".into(), "failed".into(), "disabled".into()].into();
        let snapshot = [
            status("running", McpServerState::Running),
            status("failed", McpServerState::Failed),
            status("disabled", McpServerState::Disabled),
        ];
        // Only the Running server provides tools; Failed/Disabled are present in the
        // snapshot yet still reported, since their tools are unavailable.
        assert_eq!(
            AppState::unavailable_required_servers(&required, &snapshot),
            vec!["disabled".to_string(), "failed".to_string()]
        );
    }

    #[test]
    fn apply_phenotype_returns_resolved_active_skills() {
        let state = AppState::new();
        let _dir = install_skills(&state, &[("codegraph", &["codegraph"])]);
        let resolved = state.apply_phenotype(Phenotype {
            name: "codon".into(),
            skills: vec!["codegraph".into(), "not-installed".into()],
            model: None,
            persona: None,
            max_iterations: None,
            provider: None,
            mcp_servers: Vec::new(),
        });
        // Unknown skills are dropped; the returned set mirrors the active set so the
        // caller can warn about MCP requirements without re-resolving.
        assert_eq!(
            resolved,
            ["codegraph".to_string()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        assert_eq!(state.active_skills(), vec!["codegraph"]);
    }

    #[test]
    fn apply_default_clears_overrides() {
        let state = AppState::new();
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: Some("m".into()),
            persona: Some("p".into()),
            max_iterations: None,
            provider: None,
            mcp_servers: Vec::new(),
        });
        state.apply_phenotype(default_phenotype());
        assert!(state.active_model_override().is_none());
        assert!(state.active_phenotype().persona.is_none());
        assert!(state.active_skills().is_empty());
    }

    #[test]
    fn unbound_session_inherits_global_active_phenotype() {
        let state = AppState::new();
        // Make the global active phenotype non-default so inheritance is observable.
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: Some("qwen3-coder".into()),
            persona: Some("You are a Rust expert.".into()),
            max_iterations: Some(40),
            provider: None,
            mcp_servers: Vec::new(),
        });
        let s = state.store.create_session(None);
        let resolved = state.session_phenotype(&s.id);
        assert_eq!(resolved.name, "rust");
        assert_eq!(resolved.model.as_deref(), Some("qwen3-coder"));
        assert_eq!(resolved.persona.as_deref(), Some("You are a Rust expert."));
        // The per-session resolver carries the loop cap (#244-R3 x #246), so a bound
        // pane can run a different iteration budget than the global default.
        assert_eq!(resolved.max_iterations, Some(40));
    }

    #[test]
    fn bound_session_resolves_to_its_own_phenotype() {
        let state = AppState::new();
        // Global active is the built-in default...
        let s = state.store.create_session(None);
        // ...but bind this session explicitly to `default` and confirm it resolves
        // to a real, named phenotype (the built-in is always resolvable).
        state
            .set_session_phenotype(&s.id, Some(DEFAULT_PHENOTYPE.into()))
            .unwrap();
        let resolved = state.session_phenotype(&s.id);
        assert_eq!(resolved.name, DEFAULT_PHENOTYPE);
    }

    #[test]
    fn session_bound_to_unknown_phenotype_falls_back_to_global() {
        let state = AppState::new();
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: None,
            persona: None,
            max_iterations: None,
            provider: None,
            mcp_servers: Vec::new(),
        });
        let s = state.store.create_session(None);
        // Inject a dangling binding directly through the store (the validated
        // setter would reject it) to exercise the resolver's graceful fallback.
        state
            .store
            .set_session_phenotype(&s.id, Some("ghost-phenotype".into()));
        let resolved = state.session_phenotype(&s.id);
        assert_eq!(
            resolved.name, "rust",
            "unknown binding inherits global active"
        );
    }

    // RFC 0005 Phase C (#498): three-tier model resolution. `with_registry` seeds the
    // default registry (active `candle-vllm` @ Qwen3-4B + `ollama` @ llama3.2), so a
    // phenotype `provider` binding is observable against a real second connection.
    #[test]
    fn unbound_session_resolves_to_active_connection_and_its_model() {
        let state = AppState::with_registry(ProviderRegistry::default());
        let s = state.store.create_session(None);
        let sel = state.resolve_model_selection(&s.id);
        assert_eq!(sel.connection, "candle-vllm");
        assert_eq!(sel.model, "Qwen3-4B-Instruct-2507");
    }

    #[test]
    fn phenotype_model_override_without_provider_rides_active_connection() {
        let state = AppState::with_registry(ProviderRegistry::default());
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: Some("custom-model".into()),
            persona: None,
            max_iterations: None,
            provider: None,
            mcp_servers: Vec::new(),
        });
        let s = state.store.create_session(None);
        let sel = state.resolve_model_selection(&s.id);
        // No provider binding -> stays on the active connection (backward compatible),
        // but now the override model is routed through it explicitly.
        assert_eq!(sel.connection, "candle-vllm");
        assert_eq!(sel.model, "custom-model");
    }

    #[test]
    fn phenotype_provider_binding_routes_to_that_connections_model() {
        let state = AppState::with_registry(ProviderRegistry::default());
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: None,
            persona: None,
            max_iterations: None,
            provider: Some("ollama".into()),
            mcp_servers: Vec::new(),
        });
        let s = state.store.create_session(None);
        let sel = state.resolve_model_selection(&s.id);
        // Provider bound, no explicit model -> the BOUND connection's own model, never
        // the active connection's (the RFC 0005 §11.1 cross-endpoint bug).
        assert_eq!(sel.connection, "ollama");
        assert_eq!(sel.model, "llama3.2");
    }

    #[test]
    fn phenotype_provider_and_model_are_both_honored() {
        let state = AppState::with_registry(ProviderRegistry::default());
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: Some("llama3.2:70b".into()),
            persona: None,
            max_iterations: None,
            provider: Some("ollama".into()),
            mcp_servers: Vec::new(),
        });
        let s = state.store.create_session(None);
        let sel = state.resolve_model_selection(&s.id);
        assert_eq!(sel.connection, "ollama");
        assert_eq!(sel.model, "llama3.2:70b");
    }

    // RFC 0005 §11.2 Phase D (#499): an explicit per-session selection is the top tier.
    #[test]
    fn session_model_override_wins_over_phenotype() {
        let state = AppState::with_registry(ProviderRegistry::default());
        // A phenotype binding that would otherwise route to ollama/llama3.2...
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: None,
            persona: None,
            max_iterations: None,
            provider: Some("ollama".into()),
            mcp_servers: Vec::new(),
        });
        let s = state.store.create_session(None);
        // ...is overridden by an explicit session pin to the active connection.
        state
            .set_session_model(
                &s.id,
                Some(ModelSelection {
                    connection: "candle-vllm".into(),
                    model: "pinned-model".into(),
                }),
            )
            .unwrap();
        let sel = state.resolve_model_selection(&s.id);
        assert_eq!(sel.connection, "candle-vllm");
        assert_eq!(sel.model, "pinned-model");
    }

    // RFC 0005 §11.3 (#525 PR C): attachment caps are derived from the *resolved*
    // `(kind, model)`, so a per-session model override is gated by the model it runs.
    #[test]
    fn resolved_caps_follow_session_override_model() {
        let state = AppState::with_registry(ProviderRegistry::default());
        let s = state.store.create_session(None);
        // Unbound: active candle-vllm @ Qwen3-4B is text-only.
        let base = state.resolve_model_selection(&s.id);
        assert!(!base.supports_vision);
        // Pin the session to a vision-capable Ollama model; caps follow the override.
        state
            .set_session_model(
                &s.id,
                Some(ModelSelection {
                    connection: "ollama".into(),
                    model: "llama3.2-vision".into(),
                }),
            )
            .unwrap();
        let sel = state.resolve_model_selection(&s.id);
        assert_eq!(sel.connection, "ollama");
        assert!(sel.supports_vision, "vision tag => derived supports_vision");
        assert!(!sel.supports_documents, "ollama wire has no document block");
    }

    #[test]
    fn resolved_caps_fail_closed_when_connection_missing() {
        let state = AppState::with_registry(ProviderRegistry::default());
        // A phenotype bound to a connection that does not exist (e.g. since removed),
        // with a model name that *would* be vision-capable on a real connection.
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: Some("llama3.2-vision".into()),
            persona: None,
            max_iterations: None,
            provider: Some("ghost-conn".into()),
            mcp_servers: Vec::new(),
        });
        let s = state.store.create_session(None);
        let sel = state.resolve_model_selection(&s.id);
        assert_eq!(sel.connection, "ghost-conn");
        // No kind to scope the capability lookup -> fail closed, matching the gate.
        assert!(!sel.supports_vision);
        assert!(!sel.supports_documents);
    }

    #[test]
    fn set_session_model_rejects_unknown_connection() {
        let state = AppState::with_registry(ProviderRegistry::default());
        let s = state.store.create_session(None);
        let err = state
            .set_session_model(
                &s.id,
                Some(ModelSelection {
                    connection: "ghost-conn".into(),
                    model: "whatever".into(),
                }),
            )
            .unwrap_err();
        assert!(err.contains("unknown connection"), "{err}");
        // The rejected selection must NOT have been written.
        assert!(state.session_model(&s.id).is_none());
    }

    #[test]
    fn clearing_session_model_falls_back_to_phenotype() {
        let state = AppState::with_registry(ProviderRegistry::default());
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: None,
            persona: None,
            max_iterations: None,
            provider: Some("ollama".into()),
            mcp_servers: Vec::new(),
        });
        let s = state.store.create_session(None);
        state
            .set_session_model(
                &s.id,
                Some(ModelSelection {
                    connection: "candle-vllm".into(),
                    model: "pinned-model".into(),
                }),
            )
            .unwrap();
        // Clearing the pin returns resolution to the phenotype tier.
        state.set_session_model(&s.id, None).unwrap();
        let sel = state.resolve_model_selection(&s.id);
        assert_eq!(sel.connection, "ollama");
        assert_eq!(sel.model, "llama3.2");
    }

    // --- RFC 0018 C3 (#590): tiered MCP resolution (session > phenotype > global) ---

    fn mcp_cfg(id: &str, command: &str, scope: McpScope) -> McpServerConfig {
        McpServerConfig {
            id: id.into(),
            command: command.into(),
            args: vec!["serve".into(), "--mcp".into()],
            env: Default::default(),
            disabled: false,
            scope,
        }
    }

    fn pheno_with_mcp(name: &str, servers: Vec<McpServerConfig>) -> Phenotype {
        Phenotype {
            name: name.into(),
            skills: vec![],
            model: None,
            persona: None,
            max_iterations: None,
            provider: None,
            mcp_servers: servers,
        }
    }

    #[test]
    fn unbound_session_resolves_no_mcp_servers_by_default() {
        let state = AppState::new();
        let s = state.store.create_session(None);
        // No global file, default phenotype, no session override -> empty (today's
        // behavior preserved).
        assert!(state.resolve_mcp_servers(&s.id).is_empty());
    }

    #[test]
    fn global_mcp_json_feeds_resolution() {
        let state = AppState::new();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mcp.json");
        fs::write(&path, r#"{"mcpServers":{"github":{"command":"gh-mcp"}}}"#).unwrap();
        state.init_mcp_at(path);

        let s = state.store.create_session(None);
        let resolved = state.resolve_mcp_servers(&s.id);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "github");
        // Absent `scope` deserializes to Global (back-compat).
        assert_eq!(resolved[0].scope, McpScope::Global);
    }

    #[test]
    fn phenotype_mcp_servers_resolve_for_unbound_session() {
        let state = AppState::new();
        state.apply_phenotype(pheno_with_mcp(
            "codon",
            vec![mcp_cfg("codegraph", "codegraph", McpScope::Workspace)],
        ));
        let s = state.store.create_session(None);
        let resolved = state.resolve_mcp_servers(&s.id);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "codegraph");
        assert_eq!(resolved[0].scope, McpScope::Workspace);
    }

    #[test]
    fn phenotype_overrides_global_by_id_whole_record() {
        let state = AppState::new();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mcp.json");
        // Global declares codegraph as a global-scoped server.
        fs::write(
            &path,
            r#"{"mcpServers":{"codegraph":{"command":"old-codegraph"}}}"#,
        )
        .unwrap();
        state.init_mcp_at(path);
        // The phenotype's codegraph wins as a WHOLE record (command + scope), not a
        // field-level merge (RFC 0018 section 11.5).
        state.apply_phenotype(pheno_with_mcp(
            "codon",
            vec![mcp_cfg("codegraph", "codegraph", McpScope::Workspace)],
        ));

        let s = state.store.create_session(None);
        let resolved = state.resolve_mcp_servers(&s.id);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].command, "codegraph");
        assert_eq!(resolved[0].scope, McpScope::Workspace);
    }

    #[test]
    fn session_tier_overrides_phenotype_by_id() {
        let state = AppState::new();
        state.apply_phenotype(pheno_with_mcp(
            "codon",
            vec![mcp_cfg("codegraph", "pheno-codegraph", McpScope::Workspace)],
        ));
        let s = state.store.create_session(None);
        state.store.set_session_mcp_servers(
            &s.id,
            Some(vec![mcp_cfg(
                "codegraph",
                "session-codegraph",
                McpScope::Workspace,
            )]),
        );

        let resolved = state.resolve_mcp_servers(&s.id);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].command, "session-codegraph");
    }

    #[test]
    fn disabled_at_a_tier_suppresses_an_inherited_server() {
        let state = AppState::new();
        state.apply_phenotype(pheno_with_mcp(
            "codon",
            vec![mcp_cfg("codegraph", "codegraph", McpScope::Workspace)],
        ));
        let s = state.store.create_session(None);
        // The session suppresses the phenotype's codegraph for this turn.
        let mut off = mcp_cfg("codegraph", "codegraph", McpScope::Workspace);
        off.disabled = true;
        state.store.set_session_mcp_servers(&s.id, Some(vec![off]));

        assert!(state.resolve_mcp_servers(&s.id).is_empty());
    }

    #[test]
    fn set_session_phenotype_validates_name() {
        let state = AppState::new();
        let s = state.store.create_session(None);
        let err = state
            .set_session_phenotype(&s.id, Some("not-a-real-pheno".into()))
            .unwrap_err();
        assert!(err.contains("unknown phenotype"), "{err}");
        // The rejected name must NOT have been written.
        assert!(state.store.session_phenotype(&s.id).is_none());
        // Clearing always succeeds.
        state.set_session_phenotype(&s.id, None).unwrap();
    }

    // An unbound session runs as the global default mode (#265). We compare against
    // the live `default_mode()` rather than a literal so the test stays hermetic
    // regardless of any persisted `mode.json` in the dev environment.
    #[test]
    fn unbound_session_inherits_default_mode() {
        let state = AppState::new();
        let s = state.store.create_session(None);
        assert_eq!(state.session_mode(&s.id), state.default_mode());
    }

    #[test]
    fn bound_session_resolves_to_its_own_mode() {
        let state = AppState::new();
        let s = state.store.create_session(None);
        state.set_session_mode(&s.id, Some(Mode::Plan));
        assert_eq!(state.session_mode(&s.id), Mode::Plan);
        // Clearing the binding inherits the global default again.
        state.set_session_mode(&s.id, None);
        assert_eq!(state.session_mode(&s.id), state.default_mode());
    }

    #[test]
    fn unknown_session_resolves_to_default_mode() {
        let state = AppState::new();
        assert_eq!(state.session_mode("ghost-session"), state.default_mode());
    }

    // Regression guard for the #246 review blocker: per-session phenotype binding
    // must not silently disconnect the manual skill-activation palette. An UNBOUND
    // session keeps the global active set (so palette toggles reach the turn); only
    // an EXPLICITLY bound session swaps in its phenotype's declared skills.
    #[test]
    fn turn_active_skills_unbound_uses_palette_bound_uses_phenotype() {
        let state = AppState::new();
        // Simulate a command-palette activation: a skill toggled into the global
        // active set (bypassing the install-check guard, as the dedup test does).
        state
            .active_skills
            .lock()
            .unwrap()
            .insert("palette-skill".into());

        let s = state.store.create_session(None);

        // Unbound: the turn must still see the palette activation.
        assert!(
            state
                .turn_active_skills(&s.id)
                .contains(&"palette-skill".to_string()),
            "an unbound session must inherit the global palette-activated skills"
        );

        // Explicitly bound: the phenotype's declared skills govern the turn, so the
        // manually-activated palette skill drops out (no installed skills in tests).
        state
            .set_session_phenotype(&s.id, Some(DEFAULT_PHENOTYPE.into()))
            .unwrap();
        assert!(
            !state
                .turn_active_skills(&s.id)
                .contains(&"palette-skill".to_string()),
            "an explicitly bound session uses its phenotype's declared skills, not the palette"
        );
    }

    // Regression guard for issue #117: the supervisor actor is `tokio::spawn`'d, so
    // `init_mcp` must establish a reactor context itself. This is intentionally a
    // plain `#[test]` (no `#[tokio::test]`) to mirror Tauri's `setup`, which runs
    // off-runtime on macOS — pre-fix this panicked with "there is no reactor running".
    #[test]
    fn migration_makes_saved_candle_provider_active_and_seeds_ollama_inactive() {
        let config = ProviderConfig {
            kind: ProviderKind::CandleVllm,
            base_url: Some("http://localhost:9001/v1".into()),
            model: "my-candle-model".into(),
            has_key: false,
            ..Default::default()
        };
        let reg = build_migrated_registry(config);
        assert_eq!(reg.active, "candle-vllm");
        let active = reg.active_connection().unwrap();
        assert_eq!(active.kind, ProviderKind::CandleVllm);
        assert_eq!(active.model, "my-candle-model");
        assert_eq!(active.base_url.as_deref(), Some("http://localhost:9001/v1"));
        // The other local vendor is seeded, keyless, and NOT active.
        let ollama = reg
            .connections
            .iter()
            .find(|c| c.kind == ProviderKind::Ollama)
            .unwrap();
        assert_ne!(reg.active, ollama.id);
        assert!(!ollama.has_key);
        assert_eq!(reg.connections.len(), 2);
    }

    #[test]
    fn migration_makes_saved_ollama_provider_active_and_seeds_candle_inactive() {
        let config = ProviderConfig {
            kind: ProviderKind::Ollama,
            base_url: None,
            model: "qwen2.5".into(),
            has_key: false,
            ..Default::default()
        };
        let reg = build_migrated_registry(config);
        assert_eq!(reg.active, "ollama");
        assert_eq!(reg.active_connection().unwrap().model, "qwen2.5");
        assert!(reg
            .connections
            .iter()
            .any(|c| c.kind == ProviderKind::CandleVllm));
        assert_eq!(reg.connections.len(), 2);
    }

    #[test]
    fn load_uses_existing_registry_file_over_legacy_config() {
        let tmp = tempfile::tempdir().unwrap();
        let reg_path = tmp.path().join("provider-registry.json");
        let cfg_path = tmp.path().join("provider.json");
        // A registry file with a single ollama connection.
        let existing = ProviderRegistry {
            active: "ollama".into(),
            connections: vec![ProviderConnection {
                id: "ollama".into(),
                kind: ProviderKind::Ollama,
                display_name: "Ollama".into(),
                vendor: None,
                base_url: None,
                model: "saved".into(),
                has_key: false,
                secret_missing: false,
                thinking: true,
                reasoning_effort: ReasoningEffort::default(),
                reasoning_visibility: ReasoningVisibility::default(),
                region: None,
                auth_mode: None,
                aws_profile: None,
                access_key_id: None,
            }],
        };
        fs::write(&reg_path, serde_json::to_string(&existing).unwrap()).unwrap();
        // A legacy config that must be ignored when the registry file exists.
        fs::write(
            &cfg_path,
            serde_json::to_string(&ProviderConfig::default()).unwrap(),
        )
        .unwrap();
        let loaded = load_or_migrate_registry_at(Some(reg_path), Some(cfg_path));
        assert_eq!(loaded, existing);
    }

    #[test]
    fn load_migrates_when_only_legacy_config_present() {
        let tmp = tempfile::tempdir().unwrap();
        let reg_path = tmp.path().join("provider-registry.json");
        let cfg_path = tmp.path().join("provider.json");
        let config = ProviderConfig {
            kind: ProviderKind::Ollama,
            base_url: None,
            model: "legacy".into(),
            has_key: false,
            ..Default::default()
        };
        fs::write(&cfg_path, serde_json::to_string(&config).unwrap()).unwrap();
        let loaded = load_or_migrate_registry_at(Some(reg_path.clone()), Some(cfg_path));
        assert_eq!(loaded.active, "ollama");
        assert_eq!(loaded.active_connection().unwrap().model, "legacy");
        // Pure load: migration does not write the registry file (lazy persist).
        assert!(!reg_path.exists());
    }

    /// Regression (#487): a real on-disk registry holding a profile-auth Bedrock
    /// connection -- captured verbatim from a user's `provider-registry.json` -- must
    /// survive load with every Bedrock field intact. This exercises the exact bytes,
    /// including the pre-#395 absence of `reasoningEffort` (must default to Medium) and
    /// the camelCase Bedrock fields (`authMode`, `awsProfile`). Proves the persistence
    /// layer never silently drops the connection on load.
    #[test]
    fn load_preserves_profile_auth_bedrock_connection_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let reg_path = tmp.path().join("provider-registry.json");
        // Exact shape of the field-reported file (active Bedrock, profile auth, no key).
        let on_disk = r#"{
          "connections": [
            {
              "id": "ollama",
              "kind": "ollama",
              "displayName": "Ollama",
              "model": "llama3.2",
              "hasKey": false,
              "thinking": true,
              "supportsVision": false
            },
            {
              "id": "aws-bedrock",
              "kind": "bedrock",
              "displayName": "AWS Bedrock",
              "model": "us.anthropic.claude-opus-4-8",
              "hasKey": false,
              "thinking": false,
              "supportsVision": false,
              "region": "us-east-2",
              "authMode": "profile",
              "awsProfile": "bedrock-profile"
            }
          ],
          "active": "aws-bedrock"
        }"#;
        fs::write(&reg_path, on_disk).unwrap();

        let loaded = load_or_migrate_registry_at(Some(reg_path), None);

        // Neither connection is dropped, and Bedrock stays active.
        assert_eq!(
            loaded.connections.len(),
            2,
            "both connections must survive load"
        );
        assert_eq!(loaded.active, "aws-bedrock");

        let bedrock = loaded
            .connections
            .iter()
            .find(|c| c.id == "aws-bedrock")
            .expect("the Bedrock connection must not be dropped on load");
        assert_eq!(bedrock.kind, ProviderKind::Bedrock);
        assert_eq!(bedrock.region.as_deref(), Some("us-east-2"));
        assert_eq!(bedrock.auth_mode, Some(BedrockAuth::Profile));
        assert_eq!(bedrock.aws_profile.as_deref(), Some("bedrock-profile"));
        assert_eq!(bedrock.model, "us.anthropic.claude-opus-4-8");
        // A file written before #395 carries no reasoningEffort -> defaults to Medium.
        assert_eq!(bedrock.reasoning_effort, ReasoningEffort::default());
    }

    async fn siliconflow_connection_body(
        mut conn: ProviderConnection,
        thinking: bool,
    ) -> serde_json::Value {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("data: [DONE]\n\n"))
            .mount(&server)
            .await;

        conn.base_url = Some(server.uri());
        let model = conn.model.clone();
        let provider = super::build_provider(&conn, &conn.model);
        let req = ChatRequest {
            model,
            messages: vec![ChatMessage::text("user", "hi")],
            tools: Vec::new(),
            thinking,
            max_tokens: None,
        };
        let _ = provider.chat_stream(req).await.expect("send succeeds");
        let reqs = server.received_requests().await.expect("requests recorded");
        serde_json::from_slice(&reqs[0].body).expect("body is json")
    }

    fn siliconflow_conn(id: &str, effort: ReasoningEffort) -> ProviderConnection {
        ProviderConnection {
            id: id.into(),
            kind: ProviderKind::SiliconFlow,
            display_name: "SiliconFlow".into(),
            vendor: None,
            base_url: None,
            model: "zai-org/GLM-5.2".into(),
            has_key: false,
            secret_missing: false,
            thinking: true,
            reasoning_effort: effort,
            reasoning_visibility: ReasoningVisibility::default(),
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
        }
    }

    #[tokio::test]
    async fn build_provider_threads_high_connection_effort_to_siliconflow_request() {
        let body = siliconflow_connection_body(
            siliconflow_conn("sf-high-effort", ReasoningEffort::High),
            true,
        )
        .await;

        assert_eq!(body["thinking_budget"], 8192);
        assert!(body.get("enable_thinking").is_none());
    }

    #[tokio::test]
    async fn legacy_connection_without_reasoning_effort_emits_medium_budget() {
        let legacy = r#"{
          "id": "sf-legacy-effort",
          "kind": "siliconFlow",
          "displayName": "SiliconFlow",
          "model": "zai-org/GLM-5.2",
          "hasKey": false,
          "thinking": true,
          "supportsVision": false
        }"#;
        let conn: ProviderConnection = serde_json::from_str(legacy).unwrap();
        assert_eq!(conn.reasoning_effort, ReasoningEffort::Medium);

        let body = siliconflow_connection_body(conn, true).await;
        assert_eq!(body["thinking_budget"], 4096);
    }

    /// Editing one connection (the legacy active-config shim, or a per-connection
    /// upsert) must never drop a sibling -- the failure mode behind "my Bedrock
    /// connection vanished after I touched Ollama".
    #[test]
    fn editing_one_connection_preserves_siblings() {
        let mut reg = ProviderRegistry {
            active: "aws-bedrock".into(),
            connections: vec![
                ProviderConnection {
                    id: "ollama".into(),
                    kind: ProviderKind::Ollama,
                    display_name: "Ollama".into(),
                    vendor: None,
                    base_url: None,
                    model: "llama3.2".into(),
                    has_key: false,
                    secret_missing: false,
                    thinking: true,
                    reasoning_effort: ReasoningEffort::default(),
                    reasoning_visibility: ReasoningVisibility::default(),
                    region: None,
                    auth_mode: None,
                    aws_profile: None,
                    access_key_id: None,
                },
                ProviderConnection {
                    id: "aws-bedrock".into(),
                    kind: ProviderKind::Bedrock,
                    display_name: "AWS Bedrock".into(),
                    vendor: None,
                    base_url: None,
                    model: "us.anthropic.claude-opus-4-8".into(),
                    has_key: false,
                    secret_missing: false,
                    thinking: false,
                    reasoning_effort: ReasoningEffort::default(),
                    reasoning_visibility: ReasoningVisibility::default(),
                    region: Some("us-east-2".into()),
                    auth_mode: Some(BedrockAuth::Profile),
                    aws_profile: Some("bedrock-profile".into()),
                    access_key_id: None,
                },
            ],
        };
        // Per-connection upsert of an existing id edits in place, keeps the sibling.
        reg.upsert(ProviderConnection {
            model: "llama3.3".into(),
            ..reg.connections[0].clone()
        });
        assert_eq!(
            reg.connections.len(),
            2,
            "upsert must not drop the Bedrock sibling"
        );
        assert!(reg.connections.iter().any(|c| c.id == "aws-bedrock"));
        assert_eq!(reg.connections[0].model, "llama3.3");
    }

    #[test]
    fn load_falls_back_to_default_when_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = load_or_migrate_registry_at(
            Some(tmp.path().join("provider-registry.json")),
            Some(tmp.path().join("provider.json")),
        );
        assert_eq!(loaded, ProviderRegistry::default());
    }

    #[test]
    fn corrupt_registry_is_quarantined_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let reg_path = tmp.path().join("provider-registry.json");
        // A truncated / half-written file (the crash-mid-write failure mode).
        fs::write(&reg_path, "{ \"connections\": [ {").unwrap();

        let loaded = load_or_migrate_registry_at(Some(reg_path.clone()), None);

        // A corrupt file seeds a clean default rather than silently nuking config.
        assert_eq!(loaded, ProviderRegistry::default());
        // The unreadable bytes are preserved in a sibling, never destroyed...
        let preserved: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("provider-registry.corrupt-")
            })
            .collect();
        assert_eq!(
            preserved.len(),
            1,
            "corrupt file should be quarantined once"
        );
        // ...and the original path is moved aside (so the next save starts clean).
        assert!(!reg_path.exists());
    }

    #[test]
    fn corrupt_registry_does_not_trigger_legacy_migration() {
        let tmp = tempfile::tempdir().unwrap();
        let reg_path = tmp.path().join("provider-registry.json");
        let cfg_path = tmp.path().join("provider.json");
        fs::write(&reg_path, "not json at all").unwrap();
        // A legacy config exists, but a *corrupt* registry must not re-migrate it
        // over the user's (now quarantined) active registry — seed default instead.
        fs::write(
            &cfg_path,
            serde_json::to_string(&ProviderConfig {
                kind: ProviderKind::Ollama,
                model: "legacy".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();

        let loaded = load_or_migrate_registry_at(Some(reg_path), Some(cfg_path));
        assert_eq!(loaded, ProviderRegistry::default());
    }

    #[test]
    fn write_atomic_replaces_existing_and_leaves_no_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("provider-registry.json");
        write_atomic(&path, "first").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        // A second write fully replaces the contents (no append / partial state)...
        write_atomic(&path, "second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        // ...and never leaves the intermediate .tmp file behind.
        assert!(!tmp.path().join("provider-registry.tmp").exists());
    }

    #[test]
    fn config_to_connection_uses_kind_slug_and_label() {
        let conn = config_to_connection(ProviderConfig {
            kind: ProviderKind::Ollama,
            base_url: None,
            model: "solo".into(),
            has_key: false,
            thinking: true,
            ..Default::default()
        });
        assert_eq!(conn.id, "ollama");
        assert_eq!(conn.display_name, "Ollama");
        assert_eq!(conn.model, "solo");
        // Legacy shim projects it back to a ProviderConfig faithfully.
        let cfg = connection_to_config(&conn);
        assert_eq!(cfg.kind, ProviderKind::Ollama);
        assert_eq!(cfg.model, "solo");
    }

    #[test]
    fn set_provider_config_shim_mutates_active_connection_in_place() {
        let state = AppState::with_registry(ProviderRegistry::default());
        state.set_provider_config(ProviderConfig {
            kind: ProviderKind::CandleVllm,
            base_url: Some("http://localhost:9100/v1".into()),
            model: "edited".into(),
            has_key: false,
            thinking: true,
            ..Default::default()
        });
        let reg = state.provider_registry();
        // No new connection; the active one is edited in place.
        assert_eq!(reg.connections.len(), 2);
        let active = reg.active_connection().unwrap();
        assert_eq!(active.id, "candle-vllm");
        assert_eq!(active.model, "edited");
        assert_eq!(active.base_url.as_deref(), Some("http://localhost:9100/v1"));
    }

    #[test]
    fn session_root_defaults_to_workspace_root_when_unset() {
        let state = AppState::new();
        assert_eq!(state.session_root("sess-unset"), state.workspace_root);
    }

    #[test]
    fn session_root_returns_set_cwd() {
        let state = AppState::new();
        let sess = state.store.create_session(None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        state.set_session_cwd(&sess.id, path.clone());
        assert_eq!(state.session_root(&sess.id), path);
    }

    #[test]
    fn session_cwd_persists_across_a_fresh_state_over_the_same_db() {
        // #279: the cwd lives in the session row, so a restart (a new store over
        // the same db file) restores it rather than dropping it.
        let db = tempfile::tempdir().unwrap();
        let db_path = db.path().join("sessions.db");
        let work = tempfile::tempdir().unwrap();
        let work_path = work.path().to_path_buf();

        let sess_id = {
            let store = ff_session::SessionStore::open(&db_path).unwrap();
            let s = store.create_session(None);
            store.set_session_workspace(&s.id, Some(work_path.display().to_string()));
            s.id
        };
        // A fresh store over the same file == an app restart.
        let store = ff_session::SessionStore::open(&db_path).unwrap();
        assert_eq!(
            store.session_workspace(&sess_id),
            Some(work_path.display().to_string())
        );
    }

    #[test]
    fn default_workspace_root_creates_flowforge_workspaces() {
        let home = tempfile::tempdir().unwrap();
        let root = default_workspace_root_in(Some(home.path().to_path_buf()));
        assert_eq!(root, home.path().join(".flowforge").join("workspaces"));
        assert!(root.is_dir());
    }

    #[test]
    fn default_workspace_root_falls_back_to_cwd_without_home() {
        let root = default_workspace_root_in(None);
        assert_eq!(root, std::env::current_dir().unwrap());
    }

    #[test]
    fn session_cwd_is_isolated_per_session() {
        let state = AppState::new();
        let sess_a = state.store.create_session(None);
        let sess_b = state.store.create_session(None);
        let a = tempfile::tempdir().unwrap();
        let a_path = a.path().to_path_buf();
        state.set_session_cwd(&sess_a.id, a_path.clone());
        assert_eq!(state.session_root(&sess_a.id), a_path);
        // A different session is unaffected and still falls back to the default.
        assert_eq!(state.session_root(&sess_b.id), state.workspace_root);
    }

    #[test]
    fn set_active_connection_rejects_unknown_id() {
        let state = AppState::with_registry(ProviderRegistry::default());
        assert!(state.set_active_connection("ghost").is_err());
        assert_eq!(state.provider_registry().active, "candle-vllm");
        state.set_active_connection("ollama").unwrap();
        assert_eq!(state.provider_registry().active, "ollama");
    }

    #[test]
    fn upsert_and_remove_connection_round_trip() {
        let state = AppState::with_registry(ProviderRegistry::default());
        let stored = state.upsert_connection(ProviderConnection {
            id: String::new(),
            kind: ProviderKind::CandleVllm,
            display_name: "OpenRouter".into(),
            vendor: Some("openrouter".into()),
            base_url: Some("https://openrouter.ai/api/v1".into()),
            model: "x".into(),
            has_key: false,
            secret_missing: false,
            thinking: true,
            reasoning_effort: ReasoningEffort::default(),
            reasoning_visibility: ReasoningVisibility::default(),
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
        });
        assert_eq!(stored.id, "openrouter");
        assert_eq!(state.provider_registry().connections.len(), 3);
        state.remove_connection("openrouter").unwrap();
        assert_eq!(state.provider_registry().connections.len(), 2);
    }

    fn has_key_of(state: &AppState, id: &str) -> bool {
        state
            .provider_registry()
            .connections
            .into_iter()
            .find(|c| c.id == id)
            .unwrap()
            .has_key
    }

    #[test]
    fn set_connection_secret_flips_has_key_without_leaking_value() {
        let state = AppState::with_registry(ProviderRegistry::default());
        let id = "candle-vllm";
        assert!(!has_key_of(&state, id));
        state
            .set_connection_secret(id, SecretKind::ApiKey, "sk-secret-aaa")
            .unwrap();
        assert!(has_key_of(&state, id));
        // The value lands in the keychain (MemStore under cfg(test))...
        assert_eq!(
            crate::secrets::get(id, SecretKind::ApiKey).as_deref(),
            Some("sk-secret-aaa")
        );
        // ...but never in the registry the frontend receives.
        let json = serde_json::to_string(&state.provider_registry()).unwrap();
        assert!(!json.contains("sk-secret-aaa"));
    }

    #[test]
    fn set_connection_secret_unknown_id_errors_and_writes_nothing() {
        let state = AppState::with_registry(ProviderRegistry::default());
        let err = state
            .set_connection_secret("nonexistent-xyz", SecretKind::ApiKey, "nope")
            .unwrap_err();
        assert!(err.contains("nonexistent-xyz"));
        assert!(crate::secrets::get("nonexistent-xyz", SecretKind::ApiKey).is_none());
    }

    #[test]
    fn clear_connection_secret_recomputes_has_key() {
        let state = AppState::with_registry(ProviderRegistry::default());
        let id = "ollama";
        state
            .set_connection_secret(id, SecretKind::SecretAccessKey, "aws-secret")
            .unwrap();
        state
            .set_connection_secret(id, SecretKind::SessionToken, "aws-token")
            .unwrap();
        assert!(has_key_of(&state, id));
        // One of two secrets remains => has_key stays true.
        state
            .clear_connection_secret(id, SecretKind::SessionToken)
            .unwrap();
        assert!(has_key_of(&state, id));
        // Last secret cleared => has_key flips false.
        state
            .clear_connection_secret(id, SecretKind::SecretAccessKey)
            .unwrap();
        assert!(!has_key_of(&state, id));
    }

    // ---- #311 PR-2: OpenAI connection secret lifecycle ----

    /// A hosted-OpenAI connection (keychain `ApiKey`, no AWS fields). Unique id so
    /// the process-global MemStore keychain stays isolated across parallel tests.
    fn openai_conn(id: &str) -> ProviderConnection {
        ProviderConnection {
            id: id.into(),
            kind: ProviderKind::OpenAi,
            display_name: "OpenAI".into(),
            vendor: None,
            base_url: None,
            model: "gpt-4o".into(),
            has_key: false,
            secret_missing: false,
            thinking: false,
            reasoning_effort: ReasoningEffort::default(),
            reasoning_visibility: ReasoningVisibility::default(),
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
        }
    }

    #[test]
    fn openai_connection_api_key_lifecycle() {
        // Proves PR-1's "works end-to-end once a key is stored" claim for the
        // OpenAI kind: the generic ApiKey plumbing flips has_key, keeps the value
        // out of the registry, reports presence, and clears cleanly.
        let id = "openai-lifecycle";
        let state = AppState::with_registry(ProviderRegistry::default());
        state.upsert_connection(openai_conn(id));

        assert!(!has_key_of(&state, id));
        assert!(state.connection_secret_presence(id).unwrap().is_empty());

        state
            .set_connection_secret(id, SecretKind::ApiKey, "sk-openai-xyz")
            .unwrap();

        assert!(has_key_of(&state, id));
        assert_eq!(
            crate::secrets::get(id, SecretKind::ApiKey).as_deref(),
            Some("sk-openai-xyz")
        );
        assert_eq!(
            state.connection_secret_presence(id).unwrap(),
            vec![SecretKind::ApiKey]
        );
        // The secret value never enters the registry the frontend receives.
        let json = serde_json::to_string(&state.provider_registry()).unwrap();
        assert!(!json.contains("sk-openai-xyz"));

        state
            .clear_connection_secret(id, SecretKind::ApiKey)
            .unwrap();
        assert!(!has_key_of(&state, id));
        assert!(state.connection_secret_presence(id).unwrap().is_empty());
    }

    // ---- #320: per-kind presence + Auto auth resolution ----

    /// Build + insert a Bedrock connection with the given id and auth mode, leaving
    /// secret material to the caller (keychain is a process-global MemStore in tests,
    /// so each test uses a unique id to stay isolated).
    fn bedrock_conn(id: &str, auth_mode: Option<BedrockAuth>) -> ProviderConnection {
        ProviderConnection {
            id: id.into(),
            kind: ProviderKind::Bedrock,
            display_name: "Amazon Bedrock".into(),
            vendor: None,
            base_url: None,
            model: "anthropic.claude-3-5-sonnet".into(),
            has_key: false,
            secret_missing: false,
            thinking: false,
            reasoning_effort: ReasoningEffort::default(),
            reasoning_visibility: ReasoningVisibility::default(),
            region: Some("us-east-1".into()),
            auth_mode,
            aws_profile: None,
            access_key_id: None,
        }
    }

    #[test]
    fn connection_secret_presence_lists_stored_kinds_and_errors_on_unknown_id() {
        let state = AppState::with_registry(ProviderRegistry::default());
        // Unique, self-registered id: the keychain MemStore is process-global in
        // tests, so sharing a default connection id (e.g. `candle-vllm`) with
        // another secret-storing test makes the initial emptiness assertion
        // order-dependent.
        let id = "secret-presence-listing";
        state.upsert_connection(bedrock_conn(id, None));
        assert!(state.connection_secret_presence(id).unwrap().is_empty());
        state
            .set_connection_secret(id, SecretKind::ApiKey, "sk-1")
            .unwrap();
        state
            .set_connection_secret(id, SecretKind::SessionToken, "tok-1")
            .unwrap();
        assert_eq!(
            state.connection_secret_presence(id).unwrap(),
            vec![SecretKind::ApiKey, SecretKind::SessionToken]
        );
        assert!(state.connection_secret_presence("ghost").is_err());
    }

    #[test]
    fn resolved_auth_auto_prefers_api_key_over_iam_keys() {
        let id = "bedrock-auto-api-wins";
        let state = AppState::with_registry(ProviderRegistry::default());
        let mut conn = bedrock_conn(id, Some(BedrockAuth::Auto));
        conn.access_key_id = Some("AKIA...".into());
        state.upsert_connection(conn);
        // Both an IAM secret and an API key are stored => API key wins.
        state
            .set_connection_secret(id, SecretKind::SecretAccessKey, "iam-secret")
            .unwrap();
        state
            .set_connection_secret(id, SecretKind::ApiKey, "br-bearer")
            .unwrap();
        assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::ApiKey));
    }

    #[test]
    fn resolved_auth_auto_prefers_profile_over_iam_keys() {
        let id = "bedrock-auto-profile-wins";
        let state = AppState::with_registry(ProviderRegistry::default());
        let mut conn = bedrock_conn(id, Some(BedrockAuth::Auto));
        conn.aws_profile = Some("dev".into());
        conn.access_key_id = Some("AKIA...".into());
        state.upsert_connection(conn);
        state
            .set_connection_secret(id, SecretKind::SecretAccessKey, "iam-secret")
            .unwrap();
        // No API key => profile beats IAM keys.
        assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::Profile));
    }

    #[test]
    fn resolved_auth_auto_falls_back_to_iam_keys_then_profile() {
        let id = "bedrock-auto-iam-only";
        let state = AppState::with_registry(ProviderRegistry::default());
        let mut conn = bedrock_conn(id, Some(BedrockAuth::Auto));
        conn.access_key_id = Some("AKIA...".into());
        state.upsert_connection(conn);
        state
            .set_connection_secret(id, SecretKind::SecretAccessKey, "iam-secret")
            .unwrap();
        // Only IAM keys configured.
        assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::IamKeys));
        // Nothing configured at all => Profile fallback so the probe surfaces it.
        let bare = "bedrock-auto-bare";
        state.upsert_connection(bedrock_conn(bare, Some(BedrockAuth::Auto)));
        assert_eq!(
            state.resolved_bedrock_auth(bare),
            Some(BedrockAuth::Profile)
        );
    }

    #[test]
    fn resolved_auth_explicit_pin_wins_over_auto_preference() {
        let id = "bedrock-pinned-iam";
        let state = AppState::with_registry(ProviderRegistry::default());
        let mut conn = bedrock_conn(id, Some(BedrockAuth::IamKeys));
        conn.access_key_id = Some("AKIA...".into());
        state.upsert_connection(conn);
        state
            .set_connection_secret(id, SecretKind::SecretAccessKey, "iam-secret")
            .unwrap();
        // An API key is ALSO stored, but the explicit IamKeys pin is honored.
        state
            .set_connection_secret(id, SecretKind::ApiKey, "br-bearer")
            .unwrap();
        assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::IamKeys));
    }

    #[test]
    fn resolved_auth_legacy_none_defaults_to_auto() {
        // A pre-#320 Bedrock connection persisted with auth_mode: None and only a
        // profile resolves to Profile under the new Auto default -> no regression.
        let id = "bedrock-legacy-none";
        let state = AppState::with_registry(ProviderRegistry::default());
        let mut conn = bedrock_conn(id, None);
        conn.aws_profile = Some("default".into());
        state.upsert_connection(conn);
        assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::Profile));
    }

    #[test]
    fn resolved_auth_is_none_for_non_bedrock_or_unknown() {
        let state = AppState::with_registry(ProviderRegistry::default());
        assert_eq!(state.resolved_bedrock_auth("candle-vllm"), None);
        assert_eq!(state.resolved_bedrock_auth("ghost"), None);
    }

    #[test]
    fn init_mcp_spawns_supervisor_without_an_entered_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::new();
        // Parent dir exists (tempdir); mcp.json is absent => empty config, but the
        // supervisor still spawns. Exercises the exact path that aborted at boot.
        state.init_mcp_at(tmp.path().join("mcp.json"));
        assert!(
            state.mcp_handle().is_some(),
            "supervisor must spawn (and not panic) when init runs off-runtime"
        );
    }

    #[test]
    fn init_mcp_captures_config_path_for_write_back() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mcp.json");
        let state = AppState::new();
        assert!(
            state.mcp_config_path().is_none(),
            "path is None before init"
        );
        state.init_mcp_at(path.clone());
        assert_eq!(
            state.mcp_config_path(),
            Some(path),
            "control commands must write back to the watched file"
        );
    }

    #[test]
    fn start_process_reaper_spawns_without_an_entered_runtime() {
        // Same off-reactor `setup` path as `init_mcp` (issue #117): the method
        // must enter the runtime itself so a caller with no entered reactor
        // doesn't panic. The loop is detached and dies with the test process.
        let state = AppState::new();
        state.start_process_reaper();
        // No panic == the spawn succeeded.
    }

    #[test]
    fn reap_session_processes_spawns_without_an_entered_runtime() {
        // #471: `delete_session` is a synchronous Tauri command, which runs off the
        // reactor on macOS (issue #117). `reap_session_processes` must therefore use
        // a reactor-safe spawn -- a bare `tokio::spawn` here panics with "no reactor
        // running" and the unwind through the command FFI takes the whole app down.
        // Plain `#[test]` (no `#[tokio::test]`) so there is no ambient runtime.
        let state = AppState::new();
        state.reap_session_processes("any-session");
        // No panic == the spawn succeeded.
    }

    // --- Four-option approval tests (#229) ---

    #[test]
    fn session_approved_scoped_by_session_and_tool() {
        let mut reg = ApprovalRegistry::default();
        reg.set_session_approve("s1", "t1");
        assert!(reg.is_session_approved("s1", "t1"));
        assert!(!reg.is_session_approved("s2", "t1"));
        assert!(!reg.is_session_approved("s1", "t2"));
    }

    #[test]
    fn turn_cancel_keeps_session_approved_but_delete_clears_it() {
        let state = AppState::new();
        arm(&state, "s1");
        arm(&state, "s2");
        state.set_session_approve("s1", "bash");
        state.set_session_approve("s2", "bash");

        // Turn cancel (Stop button) must NOT revoke "Allow this session" (#229).
        state.cancel_pending_approvals("s1");
        assert!(state.is_session_approved("s1", "bash"));
        assert!(state.is_session_approved("s2", "bash"));

        // Session delete is the only thing that clears the session allowlist,
        // and only for the targeted session.
        state.clear_session_approvals("s1");
        assert!(!state.is_session_approved("s1", "bash"));
        assert!(state.is_session_approved("s2", "bash"));
    }

    #[test]
    fn allowlist_never_covers_dangerous() {
        let state = AppState::new();
        arm(&state, "s1");
        // Session grant (in-memory only; avoids touching the real persisted file).
        state.set_session_approve("s1", "bash");

        // Write / ReadOnly: the grant pre-approves.
        assert!(state.allowlist_covers("s1", "bash", Safety::Write));
        assert!(state.allowlist_covers("s1", "bash", Safety::ReadOnly));

        // Dangerous: always re-prompts despite the grant.
        assert!(!state.allowlist_covers("s1", "bash", Safety::Dangerous));

        // An ungranted tool is never covered regardless of safety.
        assert!(!state.allowlist_covers("s1", "ungranted_tool_xyz", Safety::Write));
    }

    #[test]
    fn always_approved_set_remove_list() {
        let mut reg = ApprovalRegistry::default();
        reg.set_always_approve("read_file");
        reg.set_always_approve("write_file");
        assert!(reg.is_always_approved("read_file"));
        assert!(!reg.is_always_approved("exec"));
        assert_eq!(reg.list_always_approved(), vec!["read_file", "write_file"]);
        reg.remove_always_approve("read_file");
        assert!(!reg.is_always_approved("read_file"));
        assert_eq!(reg.list_always_approved(), vec!["write_file"]);
    }

    #[test]
    fn always_approved_save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tool_permissions.json");
        let mut set = HashSet::new();
        set.insert("bash".to_string());
        set.insert("read_file".to_string());
        ApprovalRegistry::save_always_approved(&path, &set).unwrap();
        let loaded = ApprovalRegistry::load_always_approved(&path);
        assert_eq!(loaded, set);
    }

    #[test]
    fn load_always_approved_missing_file_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent.json");
        let loaded = ApprovalRegistry::load_always_approved(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn seed_builtin_content_writes_codon_and_codegraph_when_absent() {
        let phenos = tempfile::tempdir().unwrap();
        let skills = tempfile::tempdir().unwrap();
        seed_builtin_content_at(phenos.path(), skills.path(), None);

        // The Codon phenotype landed and parses through the real loader.
        let codon = phenos.path().join("codon.toml");
        assert!(codon.exists(), "codon.toml must be seeded");
        let (map, errors) = load_phenotypes(phenos.path());
        assert!(
            errors.is_empty(),
            "seeded codon.toml must parse: {errors:?}"
        );
        let pheno = map.get("codon").expect("codon phenotype present");
        assert!(
            pheno.skills.iter().any(|s| s == "codegraph"),
            "codon must declare the codegraph skill"
        );

        // The codegraph skill body landed in the layout the registry scans and parses.
        let skill_md = skills.path().join("codegraph").join("SKILL.md");
        assert!(skill_md.exists(), "codegraph SKILL.md must be seeded");
        let (registry, errors) = SkillRegistry::load_dir(skills.path());
        assert!(errors.is_empty(), "seeded SKILL.md must parse: {errors:?}");
        assert!(
            registry.get("codegraph").is_some(),
            "codegraph skill must be loadable so resolve_skills keeps it"
        );
    }

    #[test]
    fn seed_builtin_content_does_not_clobber_user_edits() {
        let phenos = tempfile::tempdir().unwrap();
        let skills = tempfile::tempdir().unwrap();
        let codon = phenos.path().join("codon.toml");
        fs::write(&codon, "# user-edited\nskills = []\n").unwrap();

        seed_builtin_content_at(phenos.path(), skills.path(), None);

        assert_eq!(
            fs::read_to_string(&codon).unwrap(),
            "# user-edited\nskills = []\n",
            "an existing phenotype must never be overwritten"
        );
        // The absent codegraph skill is still seeded alongside the kept edit.
        assert!(skills.path().join("codegraph").join("SKILL.md").exists());
    }

    #[test]
    fn seed_builtin_content_is_idempotent() {
        let phenos = tempfile::tempdir().unwrap();
        let skills = tempfile::tempdir().unwrap();
        seed_builtin_content_at(phenos.path(), skills.path(), None);
        let first = fs::read_to_string(phenos.path().join("codon.toml")).unwrap();

        seed_builtin_content_at(phenos.path(), skills.path(), None);
        let second = fs::read_to_string(phenos.path().join("codon.toml")).unwrap();

        assert_eq!(first, second, "a second seed run must be a no-op");
    }

    #[test]
    fn retire_removes_an_unmodified_disabled_codegraph_seed() {
        let mcp = tempfile::tempdir().unwrap();
        let path = mcp.path().join("mcp.json");
        // The exact shape the pre-C3 seed wrote (disabled, workspace, serve --mcp).
        let seeded = r#"{"mcpServers":{"codegraph":{"command":"codegraph","args":["serve","--mcp"],"disabled":true,"scope":"workspace"}}}"#;
        fs::write(&path, seeded).unwrap();

        retire_seeded_codegraph_if_unmodified(&path);

        let servers = ff_mcp::load(&path).expect("mcp.json must still parse");
        assert!(
            !servers.iter().any(|s| s.id == "codegraph"),
            "an unmodified disabled seed must be retired (codegraph now lives in the codon phenotype)"
        );
    }

    #[test]
    fn retire_leaves_a_user_edited_codegraph_entry() {
        let mcp = tempfile::tempdir().unwrap();
        let path = mcp.path().join("mcp.json");
        // The #573 workaround: an absolute command, enabled. The user owns this.
        let user =
            r#"{"mcpServers":{"codegraph":{"command":"my-codegraph","args":["--port","9000"]}}}"#;
        fs::write(&path, user).unwrap();

        retire_seeded_codegraph_if_unmodified(&path);

        let servers = ff_mcp::load(&path).unwrap();
        let codegraph = servers
            .iter()
            .find(|s| s.id == "codegraph")
            .expect("user entry must survive");
        assert_eq!(codegraph.command, "my-codegraph");
        assert_eq!(
            codegraph.args,
            vec!["--port".to_string(), "9000".to_string()]
        );
    }

    #[test]
    fn retire_leaves_an_enabled_seed_the_user_turned_on() {
        let mcp = tempfile::tempdir().unwrap();
        let path = mcp.path().join("mcp.json");
        // Seed args/command/scope, but the user enabled it -> no longer an unmodified
        // seed, so it is the user's and must be left intact.
        let enabled = r#"{"mcpServers":{"codegraph":{"command":"codegraph","args":["serve","--mcp"],"disabled":false,"scope":"workspace"}}}"#;
        fs::write(&path, enabled).unwrap();

        retire_seeded_codegraph_if_unmodified(&path);

        let servers = ff_mcp::load(&path).unwrap();
        let codegraph = servers
            .iter()
            .find(|s| s.id == "codegraph")
            .expect("an enabled (user-owned) entry must survive");
        assert!(!codegraph.disabled);
    }

    #[test]
    fn retire_is_a_noop_when_no_codegraph_entry() {
        let mcp = tempfile::tempdir().unwrap();
        let path = mcp.path().join("mcp.json");
        let other = r#"{"mcpServers":{"github":{"command":"gh-mcp"}}}"#;
        fs::write(&path, other).unwrap();

        retire_seeded_codegraph_if_unmodified(&path);

        let servers = ff_mcp::load(&path).unwrap();
        assert!(
            servers.iter().any(|s| s.id == "github"),
            "unrelated entries untouched"
        );
        assert_eq!(servers.len(), 1);
    }
}
