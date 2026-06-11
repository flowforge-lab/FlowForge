use std::collections::HashMap;
use std::sync::Mutex;

use ff_agent::CancelToken;
use ff_llm::{OllamaProvider, OpenAiProvider, Provider};
use ff_memory::MemoryStore;
use ff_tools::ToolRegistry;
use std::path::PathBuf;

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
    cancels: Mutex<HashMap<String, CancelToken>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::with_provider(ProviderKind::default())
    }

    pub fn with_provider(kind: ProviderKind) -> Self {
        Self {
            store: MemoryStore::new(),
            provider: kind.build(),
            model: DEFAULT_MODEL.to_string(),
            tools: ToolRegistry::with_defaults(),
            workspace_root: default_workspace_root(),
            cancels: Mutex::new(HashMap::new()),
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
}

/// The default workspace root for M2: the user's home directory, falling back to the
/// process CWD. Replaced by a per-session, user-chosen folder in M3.
fn default_workspace_root() -> PathBuf {
    dirs::home_dir()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}
