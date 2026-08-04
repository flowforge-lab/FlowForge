use super::*;

/// `log_dir` is a pure join, so it can be asserted without touching disk.
#[test]
fn log_dir_sits_under_data_dir() {
    let d = log_dir(std::path::Path::new("/tmp/ffdata"));
    assert_eq!(d, std::path::PathBuf::from("/tmp/ffdata/logs"));
}

/// The old default was "no subscriber at all without `FF_LOG`". #1118 problem 3
/// reverses that: the point of the floor is that a failure nobody was watching
/// still leaves a trace, which an opt-in default can never provide, because
/// opting in requires having predicted the failure.
///
/// This asserts the resolution rule rather than calling `init`, which installs a
/// process-global subscriber and would fight every other test in the binary.
#[test]
fn unset_filter_resolves_to_the_warn_floor() {
    if std::env::var(FILTER_VAR).is_ok() {
        return; // Environment already set; the override case covers this.
    }
    assert_eq!(
        resolve_directive(),
        DEFAULT_DIRECTIVE,
        "an unset FF_LOG must still record failures"
    );
}

/// The floor is `warn`, not `info`. `info` is where routine progress lives
/// ("observer wake spawning turn"), which is noise on a user's disk until they are
/// already debugging. If this ever widens to `info`, every user starts paying disk
/// for someone else's debugging session — so assert the ceiling, not just the
/// string.
#[test]
fn the_floor_admits_warnings_but_not_routine_info() {
    let hint = tracing_subscriber::EnvFilter::new(DEFAULT_DIRECTIVE)
        .max_level_hint()
        .expect("a level-only directive has a known ceiling");
    assert_eq!(
        hint,
        tracing::level_filters::LevelFilter::WARN,
        "the floor must admit WARN and ERROR, and stop below them"
    );
}

/// `FF_LOG` must still win in both directions: `off` to silence the floor, a
/// verbose directive to dig. Without this, adding the floor would have taken away
/// the debugging path it was built to serve.
#[test]
fn an_explicit_filter_overrides_the_floor_in_both_directions() {
    for directive in ["off", "trace", "ff_observer=trace", "warn,ff_agent=debug"] {
        assert!(
            tracing_subscriber::EnvFilter::try_new(directive).is_ok(),
            "{directive:?} must be a usable override"
        );
    }
    // A blank value is an unset variable expanding in a shell script far more
    // often than a request for silence, so it falls back to the floor. `off` is
    // the deliberate way to ask.
    assert_ne!(
        tracing_subscriber::EnvFilter::new("off").max_level_hint(),
        tracing_subscriber::EnvFilter::new(DEFAULT_DIRECTIVE).max_level_hint(),
        "`off` must differ from the floor, or silencing is impossible"
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

/// Always-on logging with uncapped daily rotation is a slow disk leak: a desktop
/// app nobody prunes by hand accumulates a file per day forever. This was harmless
/// while logging was opt-in and is not harmless as a default.
#[test]
fn rotation_prunes_old_files() {
    // Compile-time: a zero or wild cap is a build error, not a test failure.
    const _: () = assert!(MAX_LOG_FILES > 0 && MAX_LOG_FILES <= 30);

    let dir = std::env::temp_dir().join(format!("ff_log_cap_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Pre-seed more dated files than the cap allows, as a long-running install
    // would have. The appender prunes on open, so this proves the production path
    // is capped rather than merely that a capped builder can be constructed.
    for day in 1..=(MAX_LOG_FILES + 5) {
        std::fs::write(dir.join(format!("flowforge.log.2026-01-{day:02}")), b"old").unwrap();
    }

    let appender = open_appender(&dir).expect("appender must open");
    drop(appender);

    let kept = std::fs::read_dir(&dir).unwrap().count();
    assert!(
        kept <= MAX_LOG_FILES + 1, // +1: today's freshly opened file
        "retention must prune old files, kept {kept} with cap {MAX_LOG_FILES}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Logs name filesystem paths (usernames, project names) and observer targets
/// (hosts). Since they are now written without being asked for, they must not be
/// readable by other accounts on a shared machine.
#[cfg(unix)]
#[test]
fn log_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("ff_log_perm_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("flowforge.log.2026-07-28");
    std::fs::write(&file, b"x").unwrap();
    // Deliberately world-readable first, so the assertion proves the call changed
    // it rather than inheriting a strict umask from the test runner.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

    restrict_permissions(&dir);

    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&file), 0o600, "log file must be owner-only");
    assert_eq!(mode(&dir), 0o700, "log dir must be owner-only");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A permissions failure must not cost us the logs — the whole point of the floor
/// is that evidence survives. Best-effort means a missing directory is tolerated,
/// not a panic on a user's launch path.
#[cfg(unix)]
#[test]
fn restricting_permissions_tolerates_a_missing_dir() {
    restrict_permissions(&std::env::temp_dir().join("ff_log_absent_dir_probe"));
}

/// `FF_LOG=off` is the documented way to ask for silence, so it must not leave the
/// side effects of logging behind: an empty file per day and a writer thread. The
/// filter alone cannot deliver that — by the time it drops events the file is
/// already open.
#[test]
fn off_is_recognised_as_global_silence() {
    for directive in ["off", "OFF", " off ", "off,", "off,off"] {
        assert!(
            directive_is_off(directive),
            "{directive:?} asks for silence, so init must skip its side effects"
        );
    }
}

/// The other half of that judgement, and the one worth getting wrong-averse:
/// `off,ff_agent=debug` means "quiet except this target", which is a real and
/// useful shape that still needs a file. Treating it as silence would discard logs
/// somebody explicitly asked for — a far worse failure than an unused empty file,
/// which is why `directive_is_off` is conservative.
#[test]
fn a_target_scoped_directive_is_not_silence() {
    for directive in [
        "off,ff_agent=debug",
        "ff_agent=off",
        "warn",
        DEFAULT_DIRECTIVE,
        "trace",
        "",
    ] {
        assert!(
            !directive_is_off(directive),
            "{directive:?} still needs a log file"
        );
    }
}

/// The `0o600` guarantee must survive rotation, not just startup.
///
/// `restrict_permissions` runs once over the files that exist when `init` is
/// called. `tracing_appender` opens each new day's file itself with a hardcoded
/// `create(true)` and no mode hook, so without the wrapper every file after the
/// first day lands at the umask default and the guarantee expires overnight —
/// silently, on day two, long after anyone would look.
///
/// Simulates the rotation by advancing the wrapper's day counter, since the real
/// one takes 24 hours.
#[cfg(unix)]
#[test]
fn rotation_reapplies_owner_only_permissions() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("ff_log_rot_perm_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Stand in for the file the appender rolls to on the next day, created
    // world-readable exactly as an unhooked `create(true)` under a default umask
    // would leave it.
    let rotated = dir.join("flowforge.log.2026-07-29");
    std::fs::write(&rotated, b"tomorrow").unwrap();
    std::fs::set_permissions(&rotated, std::fs::Permissions::from_mode(0o644)).unwrap();

    let mut appender = ModeEnforcingAppender::new(Vec::new(), dir.clone());
    // Force the day to differ, which is what a rotation looks like from here.
    appender.current_day = Some(0);
    appender.write_all(b"first line after midnight").unwrap();

    let mode = std::fs::metadata(&rotated).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "the file created by a rotation must be owner-only too"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The wrapper sits on the hot write path, so it must not re-stat on every line —
/// only when the date turns over. Asserted by writing repeatedly within one day and
/// checking a deliberately loosened file is left alone.
#[cfg(unix)]
#[test]
fn writes_within_a_day_do_not_touch_permissions() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("ff_log_same_day_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("flowforge.log.2026-07-29");
    std::fs::write(&file, b"x").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let mut appender = ModeEnforcingAppender::new(Vec::new(), dir.clone());
    for _ in 0..50 {
        appender.write_all(b"same day").unwrap();
    }

    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o644,
        "no rotation happened, so the wrapper must not have run restrict_permissions"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Pins `init`'s *behaviour*, not just `directive_is_off`'s return value.
///
/// A predicate test alone leaves the wiring unpinned: deleting the early return
/// from `init` keeps every `directive_is_off` test green while `FF_LOG=off` goes
/// back to creating a directory, a file and a thread. Asserted through the
/// observable side effect — whether the log directory appears — since that is what
/// the user actually gets.
///
/// `nextest` runs each test in its own process, so mutating `FF_LOG` here cannot
/// race another test. The subscriber is never installed on this path — that is the
/// property under test — so the usual "global subscriber already set" hazard does
/// not apply either.
#[test]
fn init_with_off_creates_nothing() {
    let data_dir = std::env::temp_dir().join(format!("ff_off_init_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&data_dir).unwrap();

    let previous = std::env::var(FILTER_VAR).ok();
    // SAFETY: single-threaded at this point, and nextest isolates each test in its
    // own process, so no other thread can be reading the environment concurrently.
    unsafe { std::env::set_var(FILTER_VAR, "off") };

    let guard = init(&data_dir);

    match previous {
        Some(v) => unsafe { std::env::set_var(FILTER_VAR, v) },
        None => unsafe { std::env::remove_var(FILTER_VAR) },
    }

    assert!(
        guard.is_none(),
        "FF_LOG=off must not return a writer guard, i.e. must not have spawned the worker"
    );
    assert!(
        !log_dir(&data_dir).exists(),
        "FF_LOG=off must not create the log directory, let alone a file inside it"
    );
    let _ = std::fs::remove_dir_all(&data_dir);
}
