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
    assert_eq!(s("run_all"), Safety::Dangerous);
    assert_eq!(s("restart"), Safety::Dangerous);
    assert_eq!(s("status"), Safety::ReadOnly);
    assert_eq!(s("inspect"), Safety::ReadOnly);
    assert_eq!(s("stop"), Safety::Write);
    // Unknown / missing action is conservatively Dangerous.
    assert_eq!(s("bogus"), Safety::Dangerous);
    // min_safety is ReadOnly (status/inspect advertised in Plan); max is Dangerous.
    assert_eq!(tool.min_safety(), Safety::ReadOnly);
    assert_eq!(tool.max_safety(), Safety::Dangerous);
}

// --- Marker extraction + meta trailer (pure; no python) ---

#[test]
fn extract_markers_pulls_images_and_vars_out_of_output() {
    use super::kernel::extract_markers;
    let buf = "line one\n__FF_IMAGE__/tmp/k/fig-0.png\nline two\n__FF_VARS__[{\"name\":\"a\"}]\n";
    let (clean, images, vars) = extract_markers(buf);
    assert_eq!(clean, "line one\nline two");
    assert_eq!(images, vec!["/tmp/k/fig-0.png".to_string()]);
    assert_eq!(vars.as_deref(), Some("[{\"name\":\"a\"}]"));
}

#[test]
fn extract_markers_ignores_midline_lookalikes() {
    use super::kernel::extract_markers;
    // A marker substring that is NOT at the start of a line stays in output.
    let buf = "print this __FF_IMAGE__not-a-marker\n";
    let (clean, images, vars) = extract_markers(buf);
    assert_eq!(clean, "print this __FF_IMAGE__not-a-marker");
    assert!(images.is_empty());
    assert!(vars.is_none());
}

#[test]
fn meta_trailer_absent_when_nothing_to_report() {
    assert!(super::meta_trailer(&[], None).is_none());
    assert!(super::meta_trailer(&[], Some("[]")).is_none());
    assert!(super::meta_trailer(&[], Some("")).is_none());
}

#[test]
fn meta_trailer_carries_images_and_vars_as_json() {
    let images = vec!["/tmp/k/fig-0.png".to_string()];
    let trailer = super::meta_trailer(
        &images,
        Some("[{\"name\":\"a\",\"type\":\"int\",\"repr\":\"1\"}]"),
    )
    .expect("trailer present");
    assert!(trailer.contains("<<<FF_NB_META"));
    assert!(trailer.trim_end().ends_with("FF_NB_META"));
    // The JSON body is parseable and carries both arrays.
    let body = trailer
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("json line");
    let v: serde_json::Value = serde_json::from_str(body).expect("valid JSON");
    assert_eq!(v["images"][0]["path"], "/tmp/k/fig-0.png");
    assert_eq!(v["images"][0]["mediaType"], "image/png");
    assert_eq!(v["variables"][0]["name"], "a");
}

#[test]
fn format_variables_renders_table_or_empty() {
    assert!(super::format_variables("[]").contains("no user variables"));
    let out = super::format_variables("[{\"name\":\"a\",\"type\":\"int\",\"repr\":\"5\"}]");
    assert!(out.contains("1 variable(s)"));
    assert!(out.contains("a: int = 5"));
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
    let r1 = sup
        .run_cell(sid, None, "x = 41", 30)
        .await
        .expect("cell 1 runs");
    assert!(!r1.errored, "assignment shouldn't error: {r1:?}");

    let r2 = sup
        .run_cell(sid, None, "print(x + 1)", 30)
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
        .run_cell(sid, None, "raise ValueError('boom')", 30)
        .await
        .expect("cell 3 runs");
    assert!(r3.errored);
    assert!(r3.output.contains("ValueError"));

    // status reflects a running kernel with the cells we ran
    let status = sup.status(sid).await;
    assert!(status.contains("running"), "status: {status}");

    // stop removes it
    sup.stop(sid, None).await.expect("stop succeeds");
    assert!(sup.status(sid).await.contains("no kernel"));
}

#[tokio::test]
async fn start_allows_up_to_three_then_caps() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    // Three kernels per session are allowed.
    let k1 = sup.start("s", dir.path()).await.expect("1st starts");
    let k2 = sup.start("s", dir.path()).await.expect("2nd starts");
    let k3 = sup.start("s", dir.path()).await.expect("3rd starts");
    assert!(k1 != k2 && k2 != k3 && k1 != k3, "ids are distinct");
    // The fourth is refused by the cap.
    let err = sup.start("s", dir.path()).await.unwrap_err();
    assert!(err.contains("cap"), "got: {err}");
    assert_eq!(sup.reap_session("s").await, 3, "all three reaped");
}

#[tokio::test]
async fn kernels_are_isolated_and_selected_by_id() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    let a = sup.start("s", dir.path()).await.unwrap();
    let b = sup.start("s", dir.path()).await.unwrap();

    // Define a var only in kernel a.
    sup.run_cell("s", Some(&a), "secret = 123", 30)
        .await
        .unwrap();
    // b has no such var → NameError.
    let rb = sup
        .run_cell("s", Some(&b), "print(secret)", 30)
        .await
        .unwrap();
    assert!(rb.errored && rb.output.contains("NameError"), "got: {rb:?}");
    // a still has it.
    let ra = sup
        .run_cell("s", Some(&a), "print(secret)", 30)
        .await
        .unwrap();
    assert!(ra.output.contains("123"), "got: {ra:?}");

    // Omitting `kernel` with two live kernels is an ambiguity error.
    let amb = sup.run_cell("s", None, "pass", 30).await.unwrap_err();
    assert!(amb.contains("multiple kernels"), "got: {amb}");

    sup.reap_session("s").await;
}

#[tokio::test]
async fn restart_clears_state_and_changes_id() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    let id1 = sup.start("s", dir.path()).await.unwrap();
    sup.run_cell("s", None, "x = 1", 30).await.unwrap();

    let id2 = sup.restart("s", None, dir.path()).await.expect("restart");
    assert_ne!(id1, id2, "restart assigns a fresh kernel id");

    // The old namespace is gone.
    let r = sup.run_cell("s", None, "print(x)", 30).await.unwrap();
    assert!(r.errored && r.output.contains("NameError"), "got: {r:?}");

    sup.stop("s", None).await.unwrap();
}

#[tokio::test]
async fn inspect_dumps_user_variables() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    sup.start("s", dir.path()).await.unwrap();
    sup.run_cell("s", None, "a = 5\nb = 'hi'\nimport os", 30)
        .await
        .unwrap();

    let vars_json = sup.inspect("s", None, 30).await.expect("inspect");
    // Parse the dump (avoids brittle whitespace assumptions in json.dumps output).
    let vars: Vec<serde_json::Value> = serde_json::from_str(&vars_json).expect("valid JSON dump");
    let by_name = |n: &str| vars.iter().find(|v| v["name"] == n).cloned();
    // User vars present with type; imported module and dunders excluded.
    assert_eq!(by_name("a").unwrap()["type"], "int", "got: {vars_json}");
    assert_eq!(by_name("b").unwrap()["type"], "str", "got: {vars_json}");
    assert!(by_name("os").is_none(), "module excluded; got: {vars_json}");

    // inspect must not count as a user cell.
    let status = sup.status("s").await;
    assert!(status.contains("cells executed=1"), "status: {status}");

    sup.stop("s", None).await.unwrap();
}

fn matplotlib_available() -> bool {
    std::process::Command::new("python3")
        .args(["-c", "import matplotlib"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn run_cell_detects_matplotlib_image() {
    if !python3_available() || !matplotlib_available() {
        eprintln!("skipping: python3 + matplotlib not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = Arc::new(KernelSupervisor::new());
    let tool = NotebookTool::new(Arc::clone(&sup));
    let sid = "img";
    tool.run_with_session(serde_json::json!({"action": "start"}), dir.path(), sid)
        .await;

    let code = "import matplotlib\nmatplotlib.use('Agg')\nimport matplotlib.pyplot as plt\nplt.plot([1,2,3])";
    let res = tool
        .run_with_session(
            serde_json::json!({"action": "run_cell", "code": code}),
            dir.path(),
            sid,
        )
        .await;
    assert!(res.success, "run_cell failed: {:?}", res);
    // The result carries the FF_NB_META trailer with an image path.
    assert!(
        res.content.contains("<<<FF_NB_META"),
        "got: {}",
        res.content
    );
    assert!(res.content.contains("image/png"), "got: {}", res.content);
    let path_line = res
        .content
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("meta json line");
    let meta: serde_json::Value = serde_json::from_str(path_line).expect("valid meta JSON");
    let img_path = meta["images"][0]["path"].as_str().expect("image path");
    assert!(
        std::path::Path::new(img_path).exists(),
        "saved figure should exist on disk: {img_path}"
    );

    // Stopping the kernel cleans up its image temp dir.
    sup.stop(sid, None).await.unwrap();
    assert!(
        !std::path::Path::new(img_path).exists(),
        "image temp dir should be cleaned on stop: {img_path}"
    );
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
async fn snapshot_projects_session_state_for_the_panel() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();

    // No kernel → empty snapshot.
    let none = sup.snapshot("s").await;
    assert!(!none.has_kernel);
    assert_eq!(none.state, None);
    assert_eq!(none.kernel_id, None);
    assert_eq!(none.pid, None);
    assert_eq!(none.execution_count, 0);
    assert!(none.raw.is_empty());
    assert!(none.kernels.is_none(), "no kernels → None");

    // One live kernel → the representative describes it.
    let id1 = sup.start("s", dir.path()).await.unwrap();
    sup.run_cell("s", None, "x = 1", 30).await.unwrap();
    let one = sup.snapshot("s").await;
    assert!(one.has_kernel);
    assert_eq!(one.state, Some(KernelLiveState::Running));
    assert_eq!(one.kernel_id.as_deref(), Some(id1.as_str()));
    assert!(one.pid.is_some());
    assert_eq!(one.execution_count, 1);
    assert!(
        one.raw.contains(&id1),
        "raw carries the canonical status line"
    );
    // The structured list carries the one kernel (FE shows tabs only when > 1).
    let one_kernels = one.kernels.expect("kernels present when a kernel exists");
    assert_eq!(one_kernels.len(), 1);
    assert_eq!(one_kernels[0].kernel_id, id1);
    assert_eq!(one_kernels[0].state, KernelLiveState::Running);
    assert_eq!(one_kernels[0].execution_count, 1);

    // A second kernel → raw lists both; representative stays a live kernel.
    let id2 = sup.start("s", dir.path()).await.unwrap();
    let multi = sup.snapshot("s").await;
    assert!(multi.has_kernel);
    assert_eq!(multi.state, Some(KernelLiveState::Running));
    assert!(multi.raw.contains(&id1) && multi.raw.contains(&id2));
    assert_eq!(multi.raw.lines().count(), 2, "one status line per kernel");
    // Structured list: both kernels, sorted by id (stable FE tab order), one
    // KernelInfo per kernel.
    let multi_kernels = multi.kernels.expect("kernels present");
    assert_eq!(multi_kernels.len(), 2, "kernels[] lists every kernel");
    let mut ids: Vec<&str> = multi_kernels.iter().map(|k| k.kernel_id.as_str()).collect();
    let mut want = vec![id1.as_str(), id2.as_str()];
    ids.sort_unstable();
    want.sort_unstable();
    assert_eq!(ids, want, "kernels[] carries both ids");
    assert!(
        multi_kernels
            .iter()
            .all(|k| k.state == KernelLiveState::Running),
        "both kernels live"
    );

    sup.reap_session("s").await;
}

#[tokio::test]
async fn stop_removes_one_kernel_and_leaves_the_rest() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    let a = sup.start("s", dir.path()).await.unwrap();
    let b = sup.start("s", dir.path()).await.unwrap();
    let c = sup.start("s", dir.path()).await.unwrap();
    assert_eq!(sup.snapshot("s").await.kernels.unwrap().len(), 3);

    // Per-kernel stop (switcher's per-tab Stop): remove just `b`.
    sup.stop("s", Some(&b)).await.unwrap();
    let after = sup.snapshot("s").await;
    let after_kernels = after.kernels.expect("kernels remain");
    assert_eq!(
        after_kernels.len(),
        2,
        "only the targeted kernel is removed"
    );
    let ids: Vec<&str> = after_kernels.iter().map(|k| k.kernel_id.as_str()).collect();
    assert!(ids.contains(&a.as_str()) && ids.contains(&c.as_str()));
    assert!(!ids.contains(&b.as_str()), "stopped kernel is gone");
    assert!(after.has_kernel, "session still has kernels");

    // Stopping the remaining two empties the session.
    sup.stop("s", Some(&a)).await.unwrap();
    sup.stop("s", Some(&c)).await.unwrap();
    let empty = sup.snapshot("s").await;
    assert!(!empty.has_kernel, "no kernels left");
    assert!(empty.kernels.is_none());
}

#[tokio::test]
async fn run_cell_handles_non_ascii_source() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    // Regression for #880: the driver used `sys.stdin.read(n)` which reads
    // characters, but Rust frames cells with a UTF-8 byte count. Any non-ASCII
    // source made the driver under-read, desync from the next length header,
    // and hang until the per-cell timeout killed the kernel.
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    sup.start("unicode", dir.path()).await.unwrap();

    let r = sup
        .run_cell("unicode", None, "print(\"café\")", 5)
        .await
        .expect("non-ASCII cell should run before timeout");
    assert!(!r.errored, "non-ASCII cell should not error: {r:?}");
    assert!(r.output.contains("café"), "got: {:?}", r.output);

    // A follow-up cell proves the framing stayed synchronized after the
    // non-ASCII payload.
    let r2 = sup
        .run_cell("unicode", None, "print(\"still alive\")", 5)
        .await
        .expect("kernel remains synchronized after non-ASCII cell");
    assert!(!r2.errored, "follow-up cell should not error: {r2:?}");
    assert!(r2.output.contains("still alive"), "got: {:?}", r2.output);

    sup.stop("unicode", None).await.unwrap();
}

#[tokio::test]
async fn run_cell_handles_large_burst_without_truncation() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    // Regression for the partial-write bug: `_w` used to discard the return
    // value of `os.write()`, so once a single burst exceeded the host's pipe
    // capacity (PIPE_BUF ≈ 64 KiB on Linux, 4 KiB on macOS, plus the kernel's
    // own pipe sizing) the tail was silently dropped — corrupting the cell
    // output the model would later see. We emit a payload larger than the
    // visible cap (16 KiB) carrying a unique marker at the very end; after
    // truncation the visible tail must still contain the marker, proving
    // every byte round-tripped through the pipe.
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    sup.start("burst", dir.path()).await.unwrap();

    let r = sup
        .run_cell(
            "burst",
            None,
            r#"import sys
payload = "x" * 100_000 + "TAIL_MARKER_ABCDEF"
sys.stdout.write(payload + "\n")"#,
            30,
        )
        .await
        .expect("burst cell should run before timeout");
    assert!(!r.errored, "burst cell should not error: {r:?}");
    assert!(
        r.truncated,
        "burst should exceed MAX_CELL_OUTPUT and be truncated: len={}",
        r.output.len()
    );
    assert!(
        r.output.contains("TAIL_MARKER_ABCDEF"),
        "tail marker must survive partial writes; got len={}",
        r.output.len()
    );

    sup.stop("burst", None).await.unwrap();
}

#[tokio::test]
async fn stdout_exposes_fileno_for_libraries_that_grab_it() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    // Regression: the driver's `_Std` proxy deliberately replaces
    // `sys.stdout`, so without an explicit `fileno()` libraries that reach
    // for the underlying fd (tqdm, rich, click, pytest capture) crash with
    // AttributeError instead of falling back. Asserting fd 1 also pins the
    // contract — `fileno()` MUST return the real stdout, not a fake.
    let dir = tempfile::tempdir().unwrap();
    let sup = KernelSupervisor::new();
    sup.start("fileno", dir.path()).await.unwrap();

    let r = sup
        .run_cell(
            "fileno",
            None,
            r#"import sys
fd = sys.stdout.fileno()
# Touch it through os.fstat to prove it's a real, valid fd (not a stub that
# returns an int) — same call pattern tqdm/rich use when they decide
# whether to bypass their own buffer.
import os
os.fstat(fd)
print("FILENO_OK", fd)"#,
            10,
        )
        .await
        .expect("fileno cell should run before timeout");
    assert!(!r.errored, "fileno cell should not error: {r:?}");
    assert!(
        r.output.contains("FILENO_OK 1"),
        "stdout.fileno() must return 1 (real stdout fd); got: {:?}",
        r.output
    );

    sup.stop("fileno", None).await.unwrap();
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
    let res = sup.run_cell("s", None, "while True: pass", 1).await;
    match res {
        // Preferred outcome: SIGINT raised KeyboardInterrupt, the driver caught it
        // and emitted the error sentinel, so the cell is reported errored and the
        // kernel SURVIVES (usable for the next cell).
        Ok(cell) => {
            assert!(cell.errored, "interrupted cell should be errored: {cell:?}");
            assert!(cell.output.contains("KeyboardInterrupt"), "got: {cell:?}");
            assert!(sup.status("s").await.contains("running"));
            // and it still works afterward
            let r = sup.run_cell("s", None, "print(1+1)", 30).await.unwrap();
            assert!(r.output.contains("2"));
            sup.stop("s", None).await.unwrap();
        }
        // Fallback: SIGINT didn't land in the grace window, so the kernel was
        // killed and the call reports a timeout.
        Err(e) => {
            assert!(e.contains("timeout"), "got: {e}");
            assert!(sup.status("s").await.contains("dead"));
        }
    }
}

// --- ipynb parsing (Phase 2) ---

use super::parse::{parse_notebook, NotebookCell};

#[test]
fn parse_notebook_extracts_code_cells_only() {
    let ipynb = r##"{
        "cells": [
            {"cell_type": "markdown", "source": ["# Title"]},
            {"cell_type": "code", "source": ["x = 1\n", "y = 2"]},
            {"cell_type": "raw", "source": ["raw stuff"]},
            {"cell_type": "code", "source": ["print(x + y)"]}
        ],
        "metadata": {},
        "nbformat": 4,
        "nbformat_minor": 5
    }"##;
    let cells = parse_notebook(ipynb).unwrap();
    assert_eq!(cells.len(), 2);
    assert_eq!(
        cells[0],
        NotebookCell {
            index: 1,
            source: "x = 1\ny = 2".to_string()
        }
    );
    assert_eq!(
        cells[1],
        NotebookCell {
            index: 3,
            source: "print(x + y)".to_string()
        }
    );
}

#[test]
fn parse_notebook_source_as_single_string() {
    let ipynb = r##"{
        "cells": [
            {"cell_type": "code", "source": "print('hello')"}
        ],
        "nbformat": 4
    }"##;
    let cells = parse_notebook(ipynb).unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].source, "print('hello')");
}

#[test]
fn parse_notebook_skips_empty_code_cells() {
    let ipynb = r##"{
        "cells": [
            {"cell_type": "code", "source": []},
            {"cell_type": "code", "source": "  \n  "},
            {"cell_type": "code", "source": ["real code"]}
        ],
        "nbformat": 4
    }"##;
    let cells = parse_notebook(ipynb).unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].index, 2);
}

#[test]
fn parse_notebook_invalid_json() {
    let result = parse_notebook("not json at all");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("invalid JSON"));
}

#[test]
fn parse_notebook_missing_cells() {
    let result = parse_notebook(r##"{"metadata": {}}"##);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("cells"));
}

// --- run_all integration (Phase 2) ---

#[tokio::test]
async fn run_all_executes_cells_sequentially() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();

    // Write a notebook with 3 code cells.
    let ipynb = r##"{
        "cells": [
            {"cell_type": "code", "source": ["x = 10"]},
            {"cell_type": "markdown", "source": ["# skip me"]},
            {"cell_type": "code", "source": ["y = x * 2"]},
            {"cell_type": "code", "source": ["print(y)"]}
        ],
        "nbformat": 4
    }"##;
    let nb_path = dir.path().join("test.ipynb");
    std::fs::write(&nb_path, ipynb).unwrap();

    let sup = Arc::new(KernelSupervisor::new());
    let tool = NotebookTool::new(Arc::clone(&sup));
    let sid = "run-all-test";

    // Start kernel
    let start_args = serde_json::json!({"action": "start"});
    let res = tool.run_with_session(start_args, dir.path(), sid).await;
    assert!(res.success, "start failed: {:?}", res);

    // Run all
    let args = serde_json::json!({
        "action": "run_all",
        "notebook": nb_path.to_str().unwrap()
    });
    let res = tool.run_with_session(args, dir.path(), sid).await;
    assert!(res.success, "run_all failed: {:?}", res);
    assert!(
        res.content.contains("3/3"),
        "should report 3/3 cells; got: {}",
        res.content
    );
    assert!(
        res.content.contains("20"),
        "y=20 should appear in output; got: {}",
        res.content
    );

    sup.stop(sid, None).await.unwrap();
}

#[tokio::test]
async fn run_all_stops_on_error() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();

    let ipynb = r##"{
        "cells": [
            {"cell_type": "code", "source": ["x = 1"]},
            {"cell_type": "code", "source": ["raise ValueError('boom')"]},
            {"cell_type": "code", "source": ["print('should not run')"]}
        ],
        "nbformat": 4
    }"##;
    let nb_path = dir.path().join("err.ipynb");
    std::fs::write(&nb_path, ipynb).unwrap();

    let sup = Arc::new(KernelSupervisor::new());
    let tool = NotebookTool::new(Arc::clone(&sup));
    let sid = "run-all-err";

    let start_args = serde_json::json!({"action": "start"});
    tool.run_with_session(start_args, dir.path(), sid).await;

    let args = serde_json::json!({
        "action": "run_all",
        "notebook": nb_path.to_str().unwrap(),
        "stop_on_error": true
    });
    let res = tool.run_with_session(args, dir.path(), sid).await;
    assert!(res.success, "run_all itself should not error: {:?}", res);
    // Only 2 of 3 cells ran (stopped at second).
    assert!(res.content.contains("2/3"), "got: {}", res.content);
    assert!(
        res.content.contains("stopped on error"),
        "got: {}",
        res.content
    );
    assert!(
        !res.content.contains("should not run"),
        "third cell should not have run"
    );

    sup.stop(sid, None).await.unwrap();
}
