use super::*;
use std::io::Write;

fn write_specs(dir: &Path, json: &str) -> PathBuf {
    let path = dir.join("model-specs.json");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(json.as_bytes()).unwrap();
    path
}

#[test]
fn user_override_wins_and_extends() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_specs(
        dir.path(),
        r#"{ "rules": [
            { "match": "claude", "context_window": 999 },
            { "match": "my-local-llm", "context_window": 65536 }
        ] }"#,
    );
    let rules = merged_rules(Some(&path));
    assert_eq!(context_window_in(&rules, "anthropic.claude-opus-4"), 999);
    assert_eq!(context_window_in(&rules, "my-local-llm-v2"), 65536);
    assert_eq!(context_window_in(&rules, "zai-org/GLM-5.2"), 1_048_576);
}

#[test]
fn absent_user_file_falls_back_to_bundled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model-specs.json");
    let rules = merged_rules(Some(&path));
    assert_eq!(
        context_window_in(&rules, "anthropic.claude-opus-4"),
        200_000
    );
}

#[test]
fn bundled_qwen36_window_is_registered_and_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model-specs.json");
    let rules = merged_rules(Some(&path));
    assert_eq!(
        context_window_in(&rules, "Qwen/Qwen3.6-35B-A3B"),
        262_144,
        "qwen3.6 must resolve to its real 256K window"
    );
    assert_eq!(
        context_window_in(&rules, "Qwen/Qwen2.5-7B-Instruct"),
        ff_core::DEFAULT_CONTEXT_WINDOW_TOKENS,
        "the rule must not over-size other Qwen models"
    );
}

#[test]
fn corrupt_user_file_is_quarantined_and_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_specs(dir.path(), "{ this is not valid json");
    let rules = merged_rules(Some(&path));
    assert_eq!(
        context_window_in(&rules, "anthropic.claude-opus-4"),
        200_000
    );
    assert!(!path.exists(), "corrupt file should have been renamed away");
    let preserved: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains("model-specs.corrupt-")
        })
        .collect();
    assert_eq!(
        preserved.len(),
        1,
        "corrupt file should be quarantined once"
    );
}
