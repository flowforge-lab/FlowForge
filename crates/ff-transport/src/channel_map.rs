use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::ChannelId;

/// Persistent mapping from transport channels to FlowForge session IDs.
/// New channels get a fresh session on first contact; the map is persisted
/// to disk so sessions survive restarts.
#[derive(Debug, Clone)]
pub struct ChannelMap {
    /// Keyed by `"{transport}:{platform_id}"` for JSON-friendly serialization.
    entries: HashMap<String, String>,
    path: Option<PathBuf>,
}

impl ChannelMap {
    /// Create an in-memory channel map (for tests).
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            path: None,
        }
    }

    /// Load from disk, or create empty if the file doesn't exist.
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let entries: HashMap<String, String> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            entries,
            path: Some(path),
        }
    }

    /// Look up the session ID for a channel.
    pub fn get(&self, channel: &ChannelId) -> Option<&str> {
        self.entries.get(&Self::key(channel)).map(|s| s.as_str())
    }

    /// Bind a channel to a session ID.
    pub fn insert(&mut self, channel: ChannelId, session_id: String) {
        self.entries.insert(Self::key(&channel), session_id);
        self.persist();
    }

    fn key(channel: &ChannelId) -> String {
        format!("{}:{}", channel.transport, channel.platform_id)
    }

    /// Number of mapped channels.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn persist(&self) {
        if let Some(path) = &self.path {
            let path = path.clone();
            let json = serde_json::to_string_pretty(&self.entries).unwrap_or_default();
            // When inside a tokio runtime, offload the blocking write so the
            // async event path is never stalled by filesystem latency.
            // Outside a runtime (tests, sync callers), write synchronously.
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::task::spawn_blocking(move || {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&path, &json);
                });
            } else {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&path, &json);
            }
        }
    }
}

impl Default for ChannelMap {
    fn default() -> Self {
        Self::new()
    }
}
