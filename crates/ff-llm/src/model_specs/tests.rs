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

// Redirects the module's rule loader at a temporary override file for the current
// thread. Without this the capability lookups read a process-global cache keyed to
// the real config dir, so they could not be tested at all -- and a test that calls
// `merged_rules` directly instead would pass even with the #1137 bug present,
// because it never exercises the lookup that chooses the rule source.
thread_local! {
    static OVERRIDE_PATH: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn override_path() -> Option<PathBuf> {
    OVERRIDE_PATH.with(|p| p.borrow().clone())
}

/// Run `f` with the loader pointed at `path`, restoring the previous value after.
fn with_override<T>(path: Option<PathBuf>, f: impl FnOnce() -> T) -> T {
    let prev = OVERRIDE_PATH.with(|p| p.replace(path));
    let out = f();
    OVERRIDE_PATH.with(|p| *p.borrow_mut() = prev);
    out
}

#[test]
fn vision_lookup_honours_the_user_override_both_ways() {
    // Exercises `supports_vision` itself -- the function that had the bug -- not the
    // merge helper underneath it. Reverting the fix must make this fail.
    let dir = tempfile::tempdir().unwrap();
    let path = write_specs(
        dir.path(),
        r#"{ "rules": [
            { "match": "vendor/brand-new-omni", "provider": "siliconFlow", "supports_vision": true },
            { "match": "Qwen/Qwen3-VL-8B-Instruct", "provider": "siliconFlow", "supports_vision": false }
        ] }"#,
    );

    with_override(Some(path), || {
        // Granted: a capable model the bundled name matching cannot express.
        assert!(super::supports_vision(
            ProviderKind::SiliconFlow,
            "vendor/brand-new-omni-v1"
        ));
        // Revoked: a model the bundled defaults grant.
        assert!(!super::supports_vision(
            ProviderKind::SiliconFlow,
            "Qwen/Qwen3-VL-8B-Instruct"
        ));
        // Untouched families keep the bundled verdict.
        assert!(super::supports_vision(
            ProviderKind::SiliconFlow,
            "moonshotai/Kimi-K3"
        ));
        assert!(!super::supports_vision(
            ProviderKind::SiliconFlow,
            "zai-org/GLM-5.2"
        ));
    });
}

#[test]
fn vision_lookup_is_provider_scoped_through_the_override() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_specs(
        dir.path(),
        r#"{ "rules": [
            { "match": "shared-name", "provider": "siliconFlow", "supports_vision": true }
        ] }"#,
    );
    with_override(Some(path), || {
        assert!(super::supports_vision(
            ProviderKind::SiliconFlow,
            "vendor/shared-name-x"
        ));
        assert!(!super::supports_vision(
            ProviderKind::OpenAi,
            "vendor/shared-name-x"
        ));
    });
}

#[test]
fn window_only_override_does_not_strip_vision() {
    // Overriding a model's context window must not silently revoke its vision, and
    // must still apply the window -- both lookups share one rule source.
    let dir = tempfile::tempdir().unwrap();
    let path = write_specs(
        dir.path(),
        r#"{ "rules": [
            { "match": "Qwen/Qwen3-VL-8B-Instruct", "context_window": 12345 }
        ] }"#,
    );
    with_override(Some(path), || {
        assert_eq!(super::lookup("Qwen/Qwen3-VL-8B-Instruct"), 12345);
        assert!(super::supports_vision(
            ProviderKind::SiliconFlow,
            "Qwen/Qwen3-VL-8B-Instruct"
        ));
    });
}

#[test]
fn corrupt_override_leaves_vision_on_the_bundled_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_specs(dir.path(), "{ this is not json");
    with_override(Some(path), || {
        assert!(super::supports_vision(
            ProviderKind::SiliconFlow,
            "moonshotai/Kimi-K3"
        ));
        assert!(!super::supports_vision(
            ProviderKind::SiliconFlow,
            "zai-org/GLM-5.2"
        ));
    });
}

#[test]
fn absent_override_keeps_bundled_vision_verdicts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model-specs.json");
    with_override(Some(path), || {
        for m in ["moonshotai/Kimi-K3", "zai-org/GLM-5V-Turbo"] {
            assert!(super::supports_vision(ProviderKind::SiliconFlow, m));
        }
        assert!(!super::supports_vision(
            ProviderKind::SiliconFlow,
            "deepseek-ai/DeepSeek-V3.2"
        ));
    });
}
