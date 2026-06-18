use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use ff_agent::{
    flush_due, CancelToken, CompactionContext, CompactionStrategy, ContextPressureEstimator,
    MemoryFlush, ProxyTokenEstimator, DEFAULT_FLUSH_AT_FRACTION,
};
use ff_core::{
    Phenotype, ProviderConfig, ProviderConnection, ProviderKind, ProviderRegistry, SearchConfig,
};
use ff_llm::{OllamaProvider, OpenAiProvider, Provider};
use ff_mcp::{McpConfigWatcher, SupervisorHandle};
use ff_memory::watch::MemoryWatcher;
use ff_memory::{FlushLedger, Fts5Index, Memory, MemoryConfig, MemoryIndex};
use ff_session::SessionStore;
use ff_signals::{SignalStore, SkillAggregate, SkillCompleted};
use ff_skills::{
    default_phenotype, load_phenotypes, SharedRegistry, SkillRegistry, SkillWatcher,
    DEFAULT_PHENOTYPE,
};
use ff_tools::memory::{MemoryGetTool, MemorySearchTool, MemoryWriteTool};
use ff_tools::ToolRegistry;
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
}

/// Builds a fresh [`Provider`] from a [`ProviderConnection`]. Called once per turn
/// so a runtime provider switch takes effect on the next message — there is no
/// shared, mutable provider to swap, only the persisted registry.
fn build_provider(conn: &ProviderConnection) -> Box<dyn Provider> {
    let base_url = conn.resolved_base_url().to_string();
    match conn.kind {
        ProviderKind::CandleVllm => Box::new(OpenAiProvider::new(base_url, None)),
        ProviderKind::Ollama => Box::new(OllamaProvider::new(base_url)),
    }
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
        thinking: config.thinking,
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
    }
}

/// `~/.config/flowforge/provider.json` — the legacy single-provider file. Still
/// read for one-time migration into the registry, and left in place afterward as a
/// backup. `None` only when the OS exposes no config dir.
fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("provider.json"))
}

/// `~/.config/flowforge/provider-registry.json` — the persisted connection registry
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

/// Path-injectable core of [`load_or_migrate_registry`] so tests can drive it with
/// tempdir paths instead of the real config dir.
fn load_or_migrate_registry_at(
    reg_path: Option<PathBuf>,
    cfg_path: Option<PathBuf>,
) -> ProviderRegistry {
    if let Some(registry) = reg_path
        .as_ref()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<ProviderRegistry>(&s).ok())
    {
        return registry;
    }
    if let Some(config) = cfg_path
        .as_ref()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<ProviderConfig>(&s).ok())
    {
        return build_migrated_registry(config);
    }
    ProviderRegistry::default()
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

/// Persists the connection registry. Best-effort: a write failure leaves the
/// in-memory registry authoritative for this session rather than failing the
/// command (mirrors the search-config write path).
fn save_registry(registry: &ProviderRegistry) {
    let Some(path) = registry_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(registry) {
        let _ = fs::write(path, json);
    }
}

/// `~/.config/flowforge/search.json` — persisted, non-secret web-search settings.
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

/// Persists web-search settings. Best-effort, like [`save_registry`].
fn save_search_config(config: &SearchConfig) {
    let Some(path) = search_config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = fs::write(path, json);
    }
}

/// `~/.config/flowforge/phenotype.json` — the name of the active phenotype, persisted
/// so a switch survives a restart (RFC 0001 §7). Separate from the phenotype
/// *definitions* in `~/.flowforge/phenotypes/`; this only records which one is active.
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

/// `~/.flowforge/phenotypes`, where phenotype definition TOML files live.
fn phenotypes_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flowforge")
        .join("phenotypes")
}

/// Resolve a phenotype by name: the built-in `default`, otherwise a definition from
/// `~/.flowforge/phenotypes/`. Returns `None` for an unknown name.
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

pub struct AppState {
    pub store: SessionStore,
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
    /// Per-skill telemetry aggregates (RFC 0001 §8), persisted to
    /// `~/.flowforge/skill_signals.json`. Updated at each turn's start/end; read by
    /// the manual optimize flow's cost estimates.
    signals: Mutex<SignalStore>,
    /// MCP host plumbing (RFC 0003). Populated lazily from Tauri's `setup` closure
    /// (the supervisor needs a live Tokio runtime to spawn its actor) and `None`
    /// when no `mcp.json` is present or the watcher cannot start.
    _mcp_watcher: Mutex<Option<McpConfigWatcher>>,
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
}

impl AppState {
    pub fn new() -> Self {
        Self::with_registry(load_or_migrate_registry())
    }

    pub fn with_registry(registry: ProviderRegistry) -> Self {
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
            store: SessionStore::new(),
            registry: Mutex::new(registry),
            search_config,
            workspace_root: default_workspace_root(),
            skills,
            _skill_watcher: Mutex::new(watcher),
            approvals: Mutex::new(ApprovalRegistry::default()),
            active_skills: Mutex::new(BTreeSet::new()),
            active_phenotype: Mutex::new(default_phenotype()),
            signals: Mutex::new(load_signals()),
            _mcp_watcher: Mutex::new(None),
            mcp: Mutex::new(None),
            mcp_config_path: Mutex::new(None),
            memory,
            memory_index,
            flush_ledger,
            _memory_watcher: Mutex::new(memory_watcher),
        };
        // Restore the persisted phenotype so its active skills survive a restart.
        // An unknown/missing pointer falls back to the built-in default.
        let initial = state
            .persisted_phenotype_name()
            .and_then(|n| resolve_phenotype(n.as_str()))
            .unwrap_or_else(default_phenotype);
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
    fn apply_phenotype(&self, pheno: Phenotype) {
        let known = self.skills.read().unwrap();
        let mut next = BTreeSet::new();
        for name in &pheno.skills {
            if known.get(name).is_some() {
                next.insert(name.clone());
            } else {
                tracing::warn!(skill = %name, phenotype = %pheno.name, "phenotype names unknown skill; skipping");
            }
        }
        drop(known);
        *self.active_skills.lock().unwrap() = next;
        *self.active_phenotype.lock().unwrap() = pheno;
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

        let outcome = MemoryFlush
            .compact(CompactionContext {
                provider,
                store: &self.store,
                registry,
                root: &self.workspace_root,
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

    /// The shared durable-memory store (RFC 0006), used for per-turn ambient
    /// injection and shared with the registered `memory_*` tools.
    pub fn memory(&self) -> Arc<Memory> {
        self.memory.clone()
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
    pub fn build_tool_registry(&self) -> ToolRegistry {
        let mut reg = ToolRegistry::with_defaults();
        reg.register(Box::new(crate::web_search::WebSearchTool::new(
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
        // Bridge MCP tools from currently-running servers (M4.3).
        if let Some(handle) = self.mcp_handle() {
            for tool in ff_mcp::build_bridged_tools(&handle) {
                reg.register(tool);
            }
        }
        reg
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
        self.registry.lock().unwrap().clone()
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
        self.build_provider_for(None)
    }

    /// Build a provider + model snapshot for a specific connection (`None` = the
    /// active one). Used by `list_models(id?)` to probe a connection the user is
    /// editing without switching the active one.
    pub fn build_provider_for(&self, id: Option<&str>) -> (Box<dyn Provider>, String) {
        let conn = {
            let reg = self.registry.lock().unwrap();
            match id {
                Some(id) => reg.connections.iter().find(|c| c.id == id).cloned(),
                None => reg.active_connection().cloned(),
            }
            .unwrap_or_else(|| active_connection_or_default(&reg))
        };
        (build_provider(&conn), conn.model)
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
    /// `~/.flowforge/phenotypes/`, name-sorted. Re-scanned per call (few files).
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
        self.apply_phenotype(pheno.clone());
        save_active_phenotype_name(&pheno.name);
        Ok(pheno)
    }

    /// The active phenotype's persona preamble, if any (prepended to the system
    /// prompt for each turn).
    pub fn active_persona(&self) -> Option<String> {
        self.active_phenotype.lock().unwrap().persona.clone()
    }

    /// The active phenotype's model override, if any (replaces the provider config's
    /// model for the turn).
    pub fn active_model_override(&self) -> Option<String> {
        self.active_phenotype.lock().unwrap().model.clone()
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
/// `memory_*` tools degrade to no-ops rather than failing app start. The initial
/// full reindex happens here (not in the watcher) so a build error is logged once.
fn build_memory() -> (Arc<Memory>, Arc<dyn MemoryIndex>, Option<MemoryWatcher>) {
    let memory = Arc::new(Memory::with_default_root(MemoryConfig::default()));
    let index: Arc<dyn MemoryIndex> = match Fts5Index::open(memory.index_path()) {
        Ok(i) => Arc::new(i),
        Err(e) => {
            tracing::warn!(error = %e, "memory index unavailable on disk; using in-memory");
            match Fts5Index::open_in_memory() {
                Ok(i) => Arc::new(i),
                Err(e) => {
                    tracing::warn!(error = %e, "memory index unavailable; recall disabled");
                    return (memory, Arc::new(NullIndex), None);
                }
            }
        }
    };
    if let Err(e) = index.reindex(&memory.all_chunks()) {
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

fn default_workspace_root() -> PathBuf {
    dirs::home_dir()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
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

    // Approvals are only registered while a turn is live, so a cancel token must
    // exist for the session first — mirrors `send_message` registering the token
    // before the turn (and thus any approval) starts.
    fn arm(state: &AppState, session_id: &str) {
        state.register_cancel(session_id, CancelToken::new());
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
    fn apply_phenotype_records_overrides_and_drops_unknown_skills() {
        let state = AppState::new();
        // No skills are installed in the test environment, so every named skill is
        // unknown and must be dropped — never activated as a phantom.
        let pheno = Phenotype {
            name: "rust".into(),
            skills: vec!["not-installed".into()],
            model: Some("qwen3-coder".into()),
            persona: Some("You are a Rust expert.".into()),
        };
        state.apply_phenotype(pheno);
        assert!(state.active_skills().is_empty());
        assert_eq!(
            state.active_model_override().as_deref(),
            Some("qwen3-coder")
        );
        assert_eq!(
            state.active_persona().as_deref(),
            Some("You are a Rust expert.")
        );
        assert_eq!(state.active_phenotype().name, "rust");
    }

    #[test]
    fn apply_default_clears_overrides() {
        let state = AppState::new();
        state.apply_phenotype(Phenotype {
            name: "rust".into(),
            skills: vec![],
            model: Some("m".into()),
            persona: Some("p".into()),
        });
        state.apply_phenotype(default_phenotype());
        assert!(state.active_model_override().is_none());
        assert!(state.active_persona().is_none());
        assert!(state.active_skills().is_empty());
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
                thinking: true,
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
    fn config_to_connection_uses_kind_slug_and_label() {
        let conn = config_to_connection(ProviderConfig {
            kind: ProviderKind::Ollama,
            base_url: None,
            model: "solo".into(),
            has_key: false,
            thinking: true,
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
            thinking: true,
        });
        assert_eq!(stored.id, "openrouter");
        assert_eq!(state.provider_registry().connections.len(), 3);
        state.remove_connection("openrouter").unwrap();
        assert_eq!(state.provider_registry().connections.len(), 2);
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
}
