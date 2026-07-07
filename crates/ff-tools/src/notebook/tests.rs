use super::kernel::{parse_sentinel, sentinel_line};
use super::*;
use crate::registry::Tool;

// --- Sentinel framing (pure) ---

#[test]
fn sentinel_parse_extracts_output_and_ok_status() {
    let nonce = "abc123";
    let buf = format!("hello\nworld\n{}\n", sentinel_line(nonce, "ok"));
    let (output, errored) = parse_sentinel(&buf, nonce).expect("sentinel present");
    assert_eq!(output, "hello\nworld");
    assert!(!errored);
}

#[test]
fn sentinel_parse_reports_error_status() {
    let nonce = "abc123";
    let buf = format!("Traceback...\n{}\n", sentinel_line(nonce, "error"));
    let (_output, errored) = parse_sentinel(&buf, nonce).expect("sentinel present");
    assert!(errored);
}

#[test]
fn sentinel_parse_none_until_the_line_arrives() {
    let nonce = "abc123";
    assert!(parse_sentinel("partial output, no end yet\n", nonce).is_none());
}

#[test]
fn sentinel_parse_ignores_a_wrong_nonce_lookalike() {
    // A cell prints a sentinel-SHAPED line, but with a different nonce. It must be
    // treated as ordinary output, not the delimiter — the collision guard.
    let real = "realnonce";
    let fake_line = sentinel_line("OTHERNONCE", "ok");
    let buf = format!("{fake_line}\nmore output\n{}\n", sentinel_line(real, "ok"));
    let (output, errored) = parse_sentinel(&buf, real).expect("real sentinel present");
    assert!(output.contains(&fake_line), "lookalike stays in output");
    assert!(output.contains("more output"));
    assert!(!errored);
}

// --- Safety classification ---

#[test]
fn safety_matches_action() {
    let tool = NotebookTool::new(std::sync::Arc::new(KernelSupervisor::new()));
    let s = |action: &str| tool.safety(&serde_json::json!({ "action": action }));
    assert_eq!(s("start"), Safety::Dangerous);
    assert_eq!(s("run_cell"), Safety::Dangerous);
    assert_eq!(s("status"), Safety::ReadOnly);
    assert_eq!(s("stop"), Safety::Write);
    // Unknown / missing action is conservatively Dangerous.
    assert_eq!(s("bogus"), Safety::Dangerous);
    // min_safety is ReadOnly (status is advertised in Plan); max is Dangerous.
    assert_eq!(tool.min_safety(), Safety::ReadOnly);
    assert_eq!(tool.max_safety(), Safety::Dangerous);
}

// --- Kernel round-trip (real python3; skips gracefully when absent) ---

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn kernel_round_trip_persists_state() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    let sid = "s1";

    // start
    sup.start(sid, dir.path()).await.expect("kernel starts");

    // state persists across cells: define in one, use in the next.
    let r1 = sup.run_cell(sid, "x = 41", 30).await.expect("cell 1 runs");
    assert!(!r1.errored, "assignment shouldn't error: {r1:?}");

    let r2 = sup
        .run_cell(sid, "print(x + 1)", 30)
        .await
        .expect("cell 2 runs");
    assert!(!r2.errored, "print shouldn't error: {r2:?}");
    assert!(
        r2.output.contains("42"),
        "state should persist across cells; got: {:?}",
        r2.output
    );

    // an exception is reported as errored, kernel survives
    let r3 = sup
        .run_cell(sid, "raise ValueError('boom')", 30)
        .await
        .expect("cell 3 runs");
    assert!(r3.errored);
    assert!(r3.output.contains("ValueError"));

    // status reflects a running kernel with the cells we ran
    let status = sup.status(sid).await;
    assert!(status.contains("running"), "status: {status}");

    // stop removes it
    sup.stop(sid).await.expect("stop succeeds");
    assert!(sup.status(sid).await.contains("no kernel"));
}

#[tokio::test]
async fn start_refuses_to_clobber_a_live_kernel() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    sup.start("s", dir.path()).await.unwrap();
    let err = sup.start("s", dir.path()).await.unwrap_err();
    assert!(err.contains("already running"), "got: {err}");
    sup.stop("s").await.unwrap();
}

#[tokio::test]
async fn reap_session_kills_the_kernel() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    sup.start("s", dir.path()).await.unwrap();
    assert_eq!(sup.reap_session("s").await, 1);
    assert_eq!(sup.reap_session("s").await, 0, "idempotent");
}

#[tokio::test]
#[ignore = "timing-sensitive; run locally with a python3 present"]
async fn run_cell_times_out_and_interrupts() {
    if !python3_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    sup.start("s", dir.path()).await.unwrap();
    let res = sup.run_cell("s", "while True: pass", 1).await;
    match res {
        // Preferred outcome: SIGINT raised KeyboardInterrupt, the driver caught it
        // and emitted the error sentinel, so the cell is reported errored and the
        // kernel SURVIVES (usable for the next cell).
        Ok(cell) => {
            assert!(cell.errored, "interrupted cell should be errored: {cell:?}");
            assert!(cell.output.contains("KeyboardInterrupt"), "got: {cell:?}");
            assert!(sup.status("s").await.contains("running"));
            // and it still works afterward
            let r = sup.run_cell("s", "print(1+1)", 30).await.unwrap();
            assert!(r.output.contains("2"));
            sup.stop("s").await.unwrap();
        }
        // Fallback: SIGINT didn't land in the grace window, so the kernel was
        // killed and the call reports a timeout.
        Err(e) => {
            assert!(e.contains("timeout"), "got: {e}");
            assert!(sup.status("s").await.contains("dead"));
        }
    }
}
