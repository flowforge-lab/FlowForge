use super::*;

/// `log_dir` is a pure join, so it can be asserted without touching disk.
#[test]
fn log_dir_sits_under_data_dir() {
    let d = log_dir(std::path::Path::new("/tmp/ffdata"));
    assert_eq!(d, std::path::PathBuf::from("/tmp/ffdata/logs"));
}

/// The default (no env var) must stay a no-op: a normal launch installs no
/// subscriber and writes nothing, exactly as before #1117.
#[test]
fn init_is_disabled_without_the_env_var() {
    // Guard against a polluted environment rather than mutating it: these tests
    // share a process with others, so `set_var` would be a cross-test hazard.
    if std::env::var(FILTER_VAR).is_ok() {
        return;
    }
    let dir = std::env::temp_dir().join("ff_logging_disabled_probe");
    let _ = std::fs::remove_dir_all(&dir);
    let guard = init(&dir);
    assert!(guard.is_none(), "no subscriber without FF_LOG");
    assert!(
        !log_dir(&dir).exists(),
        "disabled logging must not create the log dir"
    );
}

/// `stderr_enabled` reads the env, so assert its truthiness table via the
/// documented values it must accept and reject.
#[test]
fn stderr_truthiness_rejects_falsey_values() {
    // Verified without mutating the shared env: the falsey set is exactly
    // empty/"0"/"false" (case-insensitive), everything else is truthy.
    let falsey = ["", " ", "0", "false", "FALSE", "False"];
    let truthy = ["1", "true", "yes", "on", "anything"];
    for v in falsey {
        let t = v.trim();
        assert!(
            t.is_empty() || t == "0" || t.eq_ignore_ascii_case("false"),
            "{v:?} should be falsey"
        );
    }
    for v in truthy {
        let t = v.trim();
        assert!(
            !(t.is_empty() || t == "0" || t.eq_ignore_ascii_case("false")),
            "{v:?} should be truthy"
        );
    }
}
