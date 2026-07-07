use super::*;

#[test]
fn cargo_test_all_pass() {
    let output = "\
running 162 tests\n\
test foo ... ok\n\
test bar ... ok\n\
test result: ok. 162 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.29s";

    let result = parse_cargo_test(output).unwrap();
    assert_eq!(result.passed, 162);
    assert_eq!(result.failed, 0);
    assert_eq!(result.skipped, 0);
    assert!(result.failures.is_empty());

    let formatted = format_result(&result);
    assert_eq!(formatted, "Tests: 162 passed (total 162)");
}

#[test]
fn cargo_test_with_failures() {
    let output = "running 162 tests\n\
test foo ... ok\n\
test bar ... FAILED\n\
test baz ... FAILED\n\
\n\
failures:\n\
\n\
---- bar stdout ----\n\
thread '\''bar'\'' panicked at '\''assertion failed: left == right\n\
  left: 1\n\
 right: 2'\'', src/lib.rs:42\n\
\n\
---- baz stdout ----\n\
thread '\''baz'\'' panicked at '\''expected true'\'', src/main.rs:10\n\
\n\
failures:\n\
    bar\n\
    baz\n\
\n\
test result: FAILED. 160 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out";

    let result = parse_cargo_test(output).unwrap();
    assert_eq!(result.passed, 160);
    assert_eq!(result.failed, 2);
    assert_eq!(result.failures.len(), 2);
    assert_eq!(result.failures[0].name, "bar");
    assert!(result.failures[0].message.contains("assertion failed"));
    assert_eq!(result.failures[1].name, "baz");

    let formatted = format_result(&result);
    assert!(formatted.contains("160 passed, 2 failed"));
    assert!(formatted.contains("1. bar"));
    assert!(formatted.contains("2. baz"));
}

#[test]
fn cargo_test_multiple_suites() {
    let output = "test result: ok. 50 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n\
test result: ok. 112 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out";

    let result = parse_cargo_test(output).unwrap();
    assert_eq!(result.passed, 162);
    assert_eq!(result.failed, 0);
    assert_eq!(result.skipped, 3);
}

#[test]
fn pytest_all_pass() {
    let output =
        "============================= test session starts ==============================\n\
collected 45 items\n\
tests/test_foo.py ....\n\
============================== 45 passed in 2.31s ==============================";

    let result = parse_pytest(output).unwrap();
    assert_eq!(result.passed, 45);
    assert_eq!(result.failed, 0);

    let formatted = format_result(&result);
    assert_eq!(formatted, "Tests: 45 passed (total 45)");
}

#[test]
fn pytest_with_failures() {
    let output =
        "============================= test session starts ==============================\n\
collected 10 items\n\
\n\
_________________________ test_add _________________________\n\
    def test_add():\n\
>       assert add(1, 2) == 4\n\
E       assert 3 == 4\n\
\n\
tests/test_calc.py:5: AssertionError\n\
=========================== short test summary info ============================\n\
FAILED tests/test_calc.py::test_add\n\
========================= 1 failed, 9 passed in 0.5s ==========================";

    let result = parse_pytest(output).unwrap();
    assert_eq!(result.passed, 9);
    assert_eq!(result.failed, 1);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].name, "test_add");
    assert!(result.failures[0].message.contains("assert 3 == 4"));
}

#[test]
fn vitest_all_pass() {
    let output = " RUN  v4.1.8 /workspace/app\n\
\n\
 ✓ src/lib/foo.test.ts (27 tests) 11ms\n\
\n\
 Test Files  1 passed (1)\n\
      Tests  27 passed (27)\n\
   Start at  10:18:03\n\
   Duration  457ms";

    let result = parse_vitest(output).unwrap();
    assert_eq!(result.passed, 27);
    assert_eq!(result.failed, 0);

    let formatted = format_result(&result);
    assert_eq!(formatted, "Tests: 27 passed (total 27)");
}

#[test]
fn vitest_with_failures() {
    let output = " FAIL  src/lib/foo.test.ts > suite > should add numbers\n\
    × should add numbers\n\
      AssertionError: expected 3 to be 4\n\
\n\
 Test Files  1 failed (1)\n\
      Tests  2 failed | 25 passed (27)\n\
   Start at  10:18:03\n\
   Duration  457ms";

    let result = parse_vitest(output).unwrap();
    assert_eq!(result.passed, 25);
    assert_eq!(result.failed, 2);
}

#[test]
fn unknown_framework_returns_raw() {
    let output = "some random test output\nall good\n";
    let result = parse_test_output(output, true);
    assert!(result.contains("PASSED"));
    assert!(result.contains("some random test output"));
}

#[test]
fn extract_number_handles_variants() {
    assert_eq!(extract_number_before("162 passed", " passed"), Some(162));
    assert_eq!(extract_number_before("ok. 5 passed", " passed"), Some(5));
    assert_eq!(extract_number_before("0 failed", " failed"), Some(0));
    assert_eq!(extract_number_before("no match", " passed"), None);
}

#[test]
fn truncate_keeps_tail() {
    let long = "a\n".repeat(10000);
    let truncated = truncate_output(&long, 100);
    assert!(truncated.len() <= 100);
    assert!(truncated.ends_with("a\n"));
}

#[test]
fn truncate_failure_message_on_multibyte_boundary() {
    let mut msg = "a".repeat(1999);
    msg.push('\u{4e16}'); // 3-byte char at bytes 1999..2002
    msg.push_str(&"b".repeat(100));
    assert!(msg.len() > 2000);

    let result = TestResult {
        passed: 0,
        failed: 1,
        skipped: 0,
        failures: vec![Failure {
            name: "test_utf8".to_string(),
            message: msg,
        }],
        raw_tail: None,
    };
    let formatted = format_result(&result);
    assert!(formatted.contains("...[truncated]"));
}

#[test]
fn truncate_output_on_multibyte_boundary() {
    let chunk = "\u{4e16}".repeat(6000); // 3 bytes each = 18000 bytes
    let result = truncate_output(&chunk, 16_000);
    assert!(result.len() <= 16_000);
    assert!(result.len() < chunk.len());
}
