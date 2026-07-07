use super::*;

#[test]
fn tags_in_one_delta() {
    let mut s = ThinkScanner::auto_detect();
    let r = s.push("<think>reasoning here</think>the answer");
    assert_eq!(r.reasoning, "reasoning here");
    assert_eq!(r.content, "the answer");
}

#[test]
fn tags_split_across_deltas() {
    let mut s = ThinkScanner::auto_detect();
    let r1 = s.push("<think>part1");
    assert_eq!(r1.reasoning, "part1");
    assert_eq!(r1.content, "");

    let r2 = s.push(" part2</think>answer");
    assert_eq!(r2.reasoning, " part2");
    assert_eq!(r2.content, "answer");
}

#[test]
fn tag_fragmented_across_deltas() {
    let mut s = ThinkScanner::auto_detect();
    let r1 = s.push("<thi");
    assert_eq!(r1, ScanResult::default());

    let r2 = s.push("nk>reasoning</think>answer");
    assert_eq!(r2.reasoning, "reasoning");
    assert_eq!(r2.content, "answer");
}

#[test]
fn close_tag_fragmented() {
    let mut s = ThinkScanner::auto_detect();
    let r1 = s.push("<think>reasoning</thi");
    assert_eq!(r1.reasoning, "reasoning");
    assert_eq!(r1.content, "");

    let r2 = s.push("nk>answer");
    assert_eq!(r2.reasoning, "");
    assert_eq!(r2.content, "answer");
}

#[test]
fn no_think_tags_passthrough() {
    let mut s = ThinkScanner::auto_detect();
    let r1 = s.push("Hello world");
    assert_eq!(r1.content, "Hello world");
    assert_eq!(r1.reasoning, "");
    assert!(s.is_pass_through());

    let r2 = s.push(" more content");
    assert_eq!(r2.content, " more content");
    assert_eq!(r2.reasoning, "");
}

#[test]
fn leading_whitespace_then_think() {
    let mut s = ThinkScanner::auto_detect();
    let r1 = s.push("\n\n");
    assert_eq!(r1, ScanResult::default());

    let r2 = s.push("<think>reasoning</think>answer");
    assert_eq!(r2.reasoning, "reasoning");
    assert_eq!(r2.content, "answer");
}

#[test]
fn mid_prose_think_not_armed() {
    let mut s = ThinkScanner::auto_detect();
    let r = s.push("Use the <think> tag for reasoning");
    assert_eq!(r.content, "Use the <think> tag for reasoning");
    assert_eq!(r.reasoning, "");
    assert!(s.is_pass_through());
}

#[test]
fn forced_mode_always_splits() {
    let mut s = ThinkScanner::forced();
    let r1 = s.push("reasoning text</think>visible answer");
    assert_eq!(r1.reasoning, "reasoning text");
    assert_eq!(r1.content, "visible answer");
}

#[test]
fn forced_mode_strips_leading_think_tag() {
    let mut s = ThinkScanner::forced();
    let r1 = s.push("<think>reasoning");
    assert_eq!(r1.reasoning, "reasoning");
    assert_eq!(r1.content, "");

    let r2 = s.push("</think>answer");
    assert_eq!(r2.reasoning, "");
    assert_eq!(r2.content, "answer");
}

#[test]
fn unclosed_think_flushes_as_reasoning() {
    let mut s = ThinkScanner::auto_detect();
    let r1 = s.push("<think>reasoning without close");
    assert_eq!(r1.reasoning, "reasoning without close");

    let r2 = s.flush();
    assert_eq!(r2.reasoning, "");
    assert_eq!(r2.content, "");
}

#[test]
fn interleaved_think_answer() {
    let mut s = ThinkScanner::auto_detect();
    let r1 = s.push("<think>thought1</think>answer1<think>thought2</think>answer2");
    assert_eq!(r1.reasoning, "thought1thought2");
    assert_eq!(r1.content, "answer1answer2");
}

#[test]
fn newline_after_close_tag_stripped() {
    let mut s = ThinkScanner::auto_detect();
    let r = s.push("<think>reasoning</think>\n\nthe answer");
    assert_eq!(r.reasoning, "reasoning");
    assert_eq!(r.content, "the answer");
}

#[test]
fn empty_think_block() {
    let mut s = ThinkScanner::auto_detect();
    let r = s.push("<think></think>answer");
    assert_eq!(r.reasoning, "");
    assert_eq!(r.content, "answer");
}

#[test]
fn forced_with_model_emitting_think_open() {
    let mut s = ThinkScanner::forced();
    let r1 = s.push("<think>\nI need to");
    assert_eq!(r1.reasoning, "\nI need to");
    assert_eq!(r1.content, "");

    let r2 = s.push(" think about this\n</think>\n\nHere is my answer");
    assert_eq!(r2.reasoning, " think about this\n");
    assert_eq!(r2.content, "Here is my answer");
}
