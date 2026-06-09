use std::collections::HashMap;
use std::sync::Mutex;

use ff_agent::CancelToken;
use ff_llm::{OllamaProvider, Provider};
use ff_memory::MemoryStore;

/// Default local model. Swappable once provider settings land (M3+).
const DEFAULT_MODEL: &str = "qwen3:4b";

pub struct AppState {
    pub store: MemoryStore,
    pub provider: Box<dyn Provider>,
    pub model: String,
    cancels: Mutex<HashMap<String, CancelToken>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            store: MemoryStore::new(),
            provider: Box::new(OllamaProvider::default()),
            model: DEFAULT_MODEL.to_string(),
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
