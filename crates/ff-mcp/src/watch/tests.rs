use super::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn event_touches_matches_only_the_config_file() {
    let name = Some(OsStr::new("mcp.json"));
    assert!(event_touches(
        &[PathBuf::from("/home/u/.flowforge/mcp.json")],
        name
    ));
    assert!(!event_touches(
        &[PathBuf::from("/home/u/.flowforge/skill_signals.json")],
        name
    ));
    assert!(event_touches(
        &[
            PathBuf::from("/home/u/.flowforge/skill_signals.json"),
            PathBuf::from("/home/u/.flowforge/mcp.json"),
        ],
        name
    ));
}

#[test]
fn event_touches_is_conservative_when_paths_empty_or_no_name() {
    assert!(event_touches(&[], Some(OsStr::new("mcp.json"))));
    assert!(event_touches(&[PathBuf::from("/x/mcp.json")], None));
}

const ONE: &str = r#"{"mcpServers":{"a":{"command":"x"}}}"#;
const TWO: &str = r#"{"mcpServers":{"a":{"command":"x"},"b":{"command":"y"}}}"#;

#[test]
fn reload_swaps_in_new_config() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    let shared: SharedConfig = Arc::new(RwLock::new(Vec::new()));

    fs::write(&path, ONE).unwrap();
    reload(&path, &shared);
    assert_eq!(shared.read().unwrap().len(), 1);

    fs::write(&path, TWO).unwrap();
    reload(&path, &shared);
    assert_eq!(shared.read().unwrap().len(), 2);
}

#[test]
fn reload_keeps_last_good_on_parse_error() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    let shared: SharedConfig = Arc::new(RwLock::new(Vec::new()));

    fs::write(&path, ONE).unwrap();
    reload(&path, &shared);
    assert_eq!(shared.read().unwrap().len(), 1);

    fs::write(&path, "{ broken").unwrap();
    reload(&path, &shared);
    assert_eq!(
        shared.read().unwrap().len(),
        1,
        "bad parse must not clobber"
    );
}

#[test]
fn spawn_missing_file_starts_empty() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    let (_w, shared, _rx) = McpConfigWatcher::spawn(path).unwrap();
    assert!(shared.read().unwrap().is_empty());
}

#[test]
fn spawn_loads_initial_and_watches() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    fs::write(&path, ONE).unwrap();

    let (_w, shared, _rx) = McpConfigWatcher::spawn(path.clone()).unwrap();
    assert_eq!(shared.read().unwrap().len(), 1);

    fs::write(&path, TWO).unwrap();
    for _ in 0..40 {
        if shared.read().unwrap().len() == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if shared.read().unwrap().len() != 2 {
        reload(&path, &shared);
    }
    assert_eq!(shared.read().unwrap().len(), 2);
}
