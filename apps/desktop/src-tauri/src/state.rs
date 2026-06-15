use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use ff_agent::CancelToken;
use ff_core::{ProviderConfig, ProviderKind};
use ff_llm::{OllamaProvider, OpenAiProvider, Provider};
use ff_memory::MemoryStore;
use ff_skills::{SharedRegistry, SkillRegistry, SkillWatcher};
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
    /// Turn cancellation tokens + pending approvals under one lock (see
    /// [`ApprovalRegistry`]).
    approvals: Mutex<ApprovalRegistry>,
    /// Globally active skills, whose bodies are injected into the system prompt
    /// (RFC 0001 §4). Global and in-memory for M3.3; per-phenotype persistence is
    /// M3.4. A `BTreeSet` keeps the set deduplicated and name-sorted.
    active_skills: Mutex<BTreeSet<String>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::with_config(load_config())
    }

    pub fn with_config(config: ProviderConfig) -> Self {
        let (watcher, skills) = load_skills();
        // The installer tools are agent-callable, so they own the skills root and a
        // handle to the shared registry to refresh it on a successful install.
        let mut tools = ToolRegistry::with_defaults();
        tools.register(Box::new(crate::tools::InstallSkillTool::new(
            skills_root(),
            skills.clone(),
        )));
        tools.register(Box::new(crate::tools::UninstallSkillTool::new(
            skills_root(),
            skills.clone(),
        )));
        tools.register(Box::new(crate::tools::SearchSkillsTool::new(
            skills.clone(),
        )));
        Self {
            store: MemoryStore::new(),
            config: Mutex::new(config),
            tools,
            workspace_root: default_workspace_root(),
            skills,
            _skill_watcher: Mutex::new(watcher),
            approvals: Mutex::new(ApprovalRegistry::default()),
            active_skills: Mutex::new(BTreeSet::new()),
        }
    }

    /// The directory installed skills live in.
    pub fn skills_root(&self) -> PathBuf {
        skills_root()
    }

    /// Re-scan the skills directory into the shared registry. Called after an
    /// install/uninstall so the change is visible without waiting on the watcher.
    pub fn reload_skills(&self) {
        reload_registry(&skills_root(), &self.skills);
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

    /// Drop every pending approval for this session — dropping the sender resolves
    /// the awaiting receiver with `Err`, which the approver translates to a deny.
    pub fn cancel_pending_approvals(&self, session_id: &str) {
        self.approvals
            .lock()
            .unwrap()
            .pending
            .retain(|(sid, _), _| sid != session_id);
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
}
