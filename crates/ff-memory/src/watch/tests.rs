use super::*;
use crate::{Fts5Index, MemoryConfig};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

fn wait_until<F: Fn() -> bool>(cond: F, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    cond()
}

#[test]
fn edit_triggers_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(Memory::new(dir.path(), MemoryConfig::default()));
    let index: Arc<dyn MemoryIndex> = Arc::new(Fts5Index::open_in_memory().unwrap());
    let _watcher = MemoryWatcher::spawn(memory.clone(), index.clone()).unwrap();

    memory
        .write("user prefers rust", crate::WriteTarget::Curated)
        .unwrap();

    let found = wait_until(
        || {
            index
                .search("rust", 10)
                .map(|h| !h.is_empty())
                .unwrap_or(false)
        },
        Duration::from_secs(5),
    );
    assert!(found, "watcher should have reindexed the curated write");
}
