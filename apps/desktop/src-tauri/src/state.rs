use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use ff_agent::CancelToken;
use ff_core::{ProviderConfig, ProviderKind};
use ff_llm::{OllamaProvider, OpenAiProvider, Provider};
use ff_memory::MemoryStore;
use ff_skills::{SharedRegistry, SkillRegistry, SkillWatcher};
use ff_tools::ToolRegistry;
use tokio::sync::oneshot;

/// One outstanding tool-approval prompt. The shell awaits `tx.send(approved)` (sent
/// by `respond_approval`) or drops the sender on cancel (which denies the call).
struct PendingApproval {
    session_id: String,
    tx: oneshot::Sender<bool>,
}

/// Builds a fresh [`Provider`] from a [`ProviderConfig`]. Called once per turn so a
/// runtime provider switch takes effect on the next message — there is no shared,
/// mutable provider to swap, only the persisted config.
fn build_provider(config: &ProviderConfig) -> Box<dyn Provider> {
    let base_url = config.resolved_base_url().to_string();
    match config.kind {
        ProviderKind::CandleVllm => Box::new(OpenAiProvider::new(base_url, None)),
        ProviderKind::Ollama => Box::new(OllamaProvider::new(base_url)),
    }
}

/// `~/.config/flowforge/provider.json` (platform config dir). `None` only when the
/// OS exposes no config dir, in which case settings stay in-memory for the session.
fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("provider.json"))
}

/// Loads persisted provider settings, falling back to the default (local
/// candle-vllm) when the file is missing or unparseable.
fn load_config() -> ProviderConfig {
    config_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persists provider settings. Best-effort: a write failure leaves the in-memory
/// config authoritative for this session rather than failing the command.
fn save_config(config: &ProviderConfig) {
    let Some(path) = config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = fs::write(path, json);
    }
}

pub struct AppState {
    pub store: MemoryStore,
    /// Persisted, non-secret LLM provider settings. Swapped wholesale by
    /// `set_provider_config`; snapshotted (never held across an await) per turn.
    config: Mutex<ProviderConfig>,
    pub tools: ToolRegistry,
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
    cancels: Mutex<HashMap<String, CancelToken>>,
    /// call_id -> pending UI approval. Removed when the user responds, or dropped
    /// (denying the call) when the turn is cancelled.
    pending: Mutex<HashMap<String, PendingApproval>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::with_config(load_config())
    }

    pub fn with_config(config: ProviderConfig) -> Self {
        let (watcher, skills) = load_skills();
        Self {
            store: MemoryStore::new(),
            config: Mutex::new(config),
            tools: ToolRegistry::with_defaults(),
            workspace_root: default_workspace_root(),
            skills,
            _skill_watcher: Mutex::new(watcher),
            cancels: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Current provider settings (clone — callers never hold the lock).
    pub fn provider_config(&self) -> ProviderConfig {
        self.config.lock().unwrap().clone()
    }

    /// Replace and persist provider settings. Takes effect on the next turn.
    pub fn set_provider_config(&self, config: ProviderConfig) {
        save_config(&config);
        *self.config.lock().unwrap() = config;
    }

    /// Build a provider + model snapshot from the current config for one turn.
    pub fn build_provider(&self) -> (Box<dyn Provider>, String) {
        let config = self.provider_config();
        let provider = build_provider(&config);
        (provider, config.model)
    }

    pub fn register_cancel(&self, session_id: &str, token: CancelToken) {
        self.cancels
            .lock()
            .unwrap()
            .insert(session_id.to_string(), token);
    }

    pub fn take_cancel(&self, session_id: &str) -> Option<CancelToken> {
        self.cancels.lock().unwrap().remove(session_id)
    }

    /// A cheap clone of the current skill set, taken at turn start.
    pub fn skills_snapshot(&self) -> SkillRegistry {
        self.skills.read().unwrap().clone()
    }
}

impl AppState {
    /// Reserve a slot for a UI approval prompt. The caller awaits the returned receiver;
    /// the matching `resolve_approval` (or `cancel_pending_approvals` on cancel) wakes it.
    pub fn register_approval(&self, call_id: &str, session_id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            call_id.to_string(),
            PendingApproval {
                session_id: session_id.to_string(),
                tx,
            },
        );
        rx
    }

    /// Deliver the user's decision. Unknown `call_id` is a no-op (race: cancel raced
    /// the click).
    pub fn resolve_approval(&self, call_id: &str, approved: bool) {
        if let Some(p) = self.pending.lock().unwrap().remove(call_id) {
            // Receiver may have been dropped (cancel raced); ignore the send error.
            let _ = p.tx.send(approved);
        }
    }

    /// Drop every pending approval for this session — dropping the sender resolves
    /// the awaiting receiver with `Err`, which the approver translates to a deny.
    pub fn cancel_pending_approvals(&self, session_id: &str) {
        self.pending
            .lock()
            .unwrap()
            .retain(|_, p| p.session_id != session_id);
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

fn default_workspace_root() -> PathBuf {
    dirs::home_dir()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_approval_delivers_decision() {
        let state = AppState::new();
        let rx = state.register_approval("call-1", "sess");
        state.resolve_approval("call-1", true);
        assert!(rx.await.unwrap());
        // Slot is freed after resolve.
        assert!(state.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancel_pending_denies_via_drop() {
        let state = AppState::new();
        let rx = state.register_approval("call-2", "sess-x");
        state.cancel_pending_approvals("sess-x");
        // Sender was dropped -> RecvError -> caller treats as deny.
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn cancel_pending_only_affects_matching_session() {
        let state = AppState::new();
        let rx_a = state.register_approval("a", "sess-a");
        let rx_b = state.register_approval("b", "sess-b");
        state.cancel_pending_approvals("sess-a");
        // sess-b survives.
        state.resolve_approval("b", true);
        assert!(rx_a.await.is_err());
        assert!(rx_b.await.unwrap());
    }

    #[tokio::test]
    async fn resolve_unknown_call_is_noop() {
        let state = AppState::new();
        // Must not panic.
        state.resolve_approval("nope", true);
    }
}
