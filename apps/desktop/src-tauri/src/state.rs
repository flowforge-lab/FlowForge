use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use ff_agent::CancelToken;
use ff_llm::{OllamaProvider, OpenAiProvider, Provider};
use ff_memory::MemoryStore;
use ff_skills::{SharedRegistry, SkillRegistry, SkillWatcher};
use ff_tools::ToolRegistry;
use std::path::PathBuf;
use tokio::sync::oneshot;

/// One outstanding tool-approval prompt. The shell awaits `tx.send(approved)` (sent
/// by `respond_approval`) or drops the sender on cancel (which denies the call).
struct PendingApproval {
    session_id: String,
    tx: oneshot::Sender<bool>,
}

/// Default local model served by candle-vllm. Swappable once provider settings
/// land (M3+). Matches the `id` candle-vllm reports at `/v1/models`.
const DEFAULT_MODEL: &str = "Qwen3-4B-Instruct-2507";

/// Selects which LLM backend `AppState` talks to. M1 hard-defaults to
/// [`ProviderKind::CandleVllm`]; the enum exists so M3 settings can switch at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderKind {
    /// Local candle-vllm, OpenAI-compatible SSE on `:8000/v1`.
    #[default]
    CandleVllm,
    /// Local Ollama, native NDJSON `/api/chat` on `:11434`.
    #[allow(dead_code)] // constructed by M3 provider settings
    OllamaNative,
    /// Hosted OpenAI API (reads `OPENAI_API_KEY`).
    #[allow(dead_code)] // constructed by M3 provider settings
    OpenAi,
}

impl ProviderKind {
    fn build(self) -> Box<dyn Provider> {
        match self {
            ProviderKind::CandleVllm => Box::new(OpenAiProvider::candle_vllm()),
            ProviderKind::OllamaNative => Box::new(OllamaProvider::default()),
            ProviderKind::OpenAi => {
                let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
                Box::new(OpenAiProvider::openai(key))
            }
        }
    }
}

pub struct AppState {
    pub store: MemoryStore,
    pub provider: Box<dyn Provider>,
    pub model: String,
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
        Self::with_provider(ProviderKind::default())
    }

    pub fn with_provider(kind: ProviderKind) -> Self {
        let (watcher, skills) = load_skills();
        Self {
            store: MemoryStore::new(),
            provider: kind.build(),
            model: DEFAULT_MODEL.to_string(),
            tools: ToolRegistry::with_defaults(),
            workspace_root: default_workspace_root(),
            skills,
            _skill_watcher: Mutex::new(watcher),
            cancels: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
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
