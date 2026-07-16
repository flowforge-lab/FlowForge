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

// ----- #978: MiniMax-M3 emits an unpaired trailing </think> that previously
// leaked into visible content. Sequences below are real deltas captured from
// the SiliconFlow API (`.ff-scratch/` in the investigation), replayed here.

#[test]
fn stray_close_tag_after_real_think_is_stripped() {
    // The 8/8 light-repro shape: one real <think>...</think>, then a surplus
    // </think> the model tacks on. The surplus close must be dropped, not shown.
    let mut s = ThinkScanner::forced();
    let r1 = s.push("The user wants me to run a git command. Let me do that.");
    assert_eq!(r1.content, "");
    assert_eq!(
        r1.reasoning,
        "The user wants me to run a git command. Let me do that."
    );

    let r2 = s.push("\n</think>\n\n</think>");
    assert_eq!(
        r2.content, "",
        "surplus </think> must never surface as visible content"
    );
}

#[test]
fn stray_close_then_real_answer_keeps_the_answer() {
    // A stray </think> immediately followed by genuine assistant prose: the tag
    // is stripped, the prose is preserved.
    let mut s = ThinkScanner::forced();
    let _ = s.push("reasoning body");
    let r = s.push("</think>\n\n</think>Here is the real answer.");
    assert_eq!(r.content, "Here is the real answer.");
}

#[test]
fn stray_close_tag_split_across_deltas() {
    // The surplus close tag fragmented across SSE byte-chunks must still be
    // recognized and stripped, not emitted piecemeal.
    let mut s = ThinkScanner::forced();
    let _ = s.push("thinking");
    let r1 = s.push("</think>visible</thi");
    assert_eq!(r1.content, "visible");
    let r2 = s.push("nk>");
    assert_eq!(r2.content, "", "fragmented stray </think> must be stripped");
}

#[test]
fn well_behaved_stream_unaffected_by_stray_close_handling() {
    // Guard: a provider that never opens a <think> stays in pass-through and its
    // content is untouched, even if it happens to contain the substring.
    let mut s = ThinkScanner::auto_detect();
    let r = s.push("Here is plain content with no tags at all.");
    assert_eq!(r.content, "Here is plain content with no tags at all.");
    assert_eq!(r.reasoning, "");
    assert!(s.is_pass_through());
}
