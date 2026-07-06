//! Stateful streaming scanner that splits `<think>...</think>` tags from content
//! into the reasoning stream (#729).
//!
//! Two activation layers share one implementation:
//! - **Layer 1 (explicit)**: `think_tags: true` in `WireDialect` — always arms the
//!   splitter for known models (MiniMax on SiliconFlow).
//! - **Layer 2 (auto-detect)**: when the flag is off, arms the splitter if the
//!   stream'\''s first non-whitespace content begins with `<think>`. Catches new/
//!   unflagged models without code changes; mid-prose `<think>` does NOT trigger it.

/// Result of feeding one delta through the scanner.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanResult {
    /// Visible content (goes to `Chunk::delta`).
    pub content: String,
    /// Reasoning content (goes to `Chunk::reasoning_delta`).
    pub reasoning: String,
}

/// Scanner state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Layer 2: waiting for the first non-whitespace token to decide whether to arm.
    Undecided,
    /// Scanner is armed and we are inside a `<think>` block.
    InsideThink,
    /// Scanner is armed and we are outside any `<think>` block (visible content).
    Outside,
    /// Scanner is not armed — pass everything through as content verbatim.
    PassThrough,
}

/// A stateful streaming scanner owned by a single SSE stream. Fed each `content`
/// delta; splits output between visible content and reasoning.
#[derive(Debug)]
pub struct ThinkScanner {
    state: State,
    /// Partial bytes buffered when a tag might be split across deltas.
    buf: String,
    /// If true, strip a leading `<think>` on first content (forced mode where
    /// the model redundantly emits the open tag we already know about).
    strip_leading_open: bool,
}

const OPEN_TAG: &str = "<think>";
const CLOSE_TAG: &str = "</think>";

impl ThinkScanner {
    /// Create a scanner that forces splitting (Layer 1: known think-tag model).
    /// Starts armed — first content goes to reasoning. If the model redundantly
    /// emits a leading `<think>` tag, it is consumed (not echoed to reasoning).
    pub fn forced() -> Self {
        Self {
            state: State::InsideThink,
            buf: String::new(),
            strip_leading_open: true,
        }
    }

    /// Create a scanner in auto-detect mode (Layer 2: arm only if leading `<think>`).
    pub fn auto_detect() -> Self {
        Self {
            state: State::Undecided,
            buf: String::new(),
            strip_leading_open: false,
        }
    }

    /// Returns true if the scanner is a no-op pass-through (never armed).
    pub fn is_pass_through(&self) -> bool {
        self.state == State::PassThrough
    }

    /// Feed one content delta. Returns the split result.
    pub fn push(&mut self, delta: &str) -> ScanResult {
        if delta.is_empty() {
            return ScanResult::default();
        }

        // In forced mode, strip a redundant leading <think> the model may emit.
        let delta = if self.strip_leading_open {
            self.strip_leading_open = false;
            let trimmed = delta.trim_start();
            if let Some(rest) = trimmed.strip_prefix(OPEN_TAG) {
                rest
            } else if OPEN_TAG.starts_with(trimmed) {
                // Partial <think> — buffer and wait for more.
                self.buf.push_str(delta);
                return ScanResult::default();
            } else {
                delta
            }
        } else {
            delta
        };

        if delta.is_empty() {
            return ScanResult::default();
        }

        match self.state {
            State::PassThrough => ScanResult {
                content: delta.to_string(),
                reasoning: String::new(),
            },
            State::Undecided => self.handle_undecided(delta),
            State::InsideThink => self.handle_inside(delta),
            State::Outside => self.handle_outside(delta),
        }
    }

    /// Flush any buffered content at end-of-stream. An unclosed `<think>` must
    /// never swallow the answer — flush buffered bytes as reasoning if inside,
    /// or as content if outside.
    pub fn flush(&mut self) -> ScanResult {
        let buf = std::mem::take(&mut self.buf);
        if buf.is_empty() {
            return ScanResult::default();
        }
        match self.state {
            State::InsideThink => ScanResult {
                content: String::new(),
                reasoning: buf,
            },
            State::Undecided => {
                // Never armed — treat buffer as content.
                ScanResult {
                    content: buf,
                    reasoning: String::new(),
                }
            }
            _ => ScanResult {
                content: buf,
                reasoning: String::new(),
            },
        }
    }

    fn handle_undecided(&mut self, delta: &str) -> ScanResult {
        self.buf.push_str(delta);
        // Check if we have enough bytes to decide.
        let trimmed = self.buf.trim_start();
        if trimmed.is_empty() {
            // All whitespace so far — keep buffering.
            return ScanResult::default();
        }
        // Check if the trimmed content starts with (or is a prefix of) <think>.
        if trimmed.starts_with(OPEN_TAG) {
            // Confirmed: arm the scanner, consume the open tag.
            let after_tag_pos = self.buf.find(OPEN_TAG).unwrap() + OPEN_TAG.len();
            let remainder = self.buf[after_tag_pos..].to_string();
            self.buf.clear();
            self.state = State::InsideThink;
            // Process remainder (might contain content or even </think>).
            if remainder.is_empty() {
                ScanResult::default()
            } else {
                self.handle_inside(&remainder)
            }
        } else if OPEN_TAG.starts_with(trimmed) || trimmed.len() < OPEN_TAG.len() {
            // Could still be a partial `<think>` — keep buffering.
            // But only if what we have is a valid prefix of the tag.
            let check = &trimmed[..trimmed.len().min(OPEN_TAG.len())];
            if OPEN_TAG.starts_with(check) {
                ScanResult::default()
            } else {
                // Not a prefix of <think> — pass through.
                let buf = std::mem::take(&mut self.buf);
                self.state = State::PassThrough;
                ScanResult {
                    content: buf,
                    reasoning: String::new(),
                }
            }
        } else {
            // Does not start with <think> — pass through.
            let buf = std::mem::take(&mut self.buf);
            self.state = State::PassThrough;
            ScanResult {
                content: buf,
                reasoning: String::new(),
            }
        }
    }

    fn handle_inside(&mut self, delta: &str) -> ScanResult {
        self.buf.push_str(delta);
        let mut reasoning = String::new();
        let mut content = String::new();

        if let Some(pos) = self.buf.find(CLOSE_TAG) {
            // Found </think> — everything before it is reasoning.
            reasoning.push_str(&self.buf[..pos]);
            let after = pos + CLOSE_TAG.len();
            let remainder = self.buf[after..].to_string();
            self.buf.clear();
            self.state = State::Outside;
            // Strip leading newlines after </think>.
            let remainder = remainder.trim_start_matches('\n');
            if !remainder.is_empty() {
                let r = self.handle_outside(remainder);
                content.push_str(&r.content);
                reasoning.push_str(&r.reasoning);
            }
        } else if could_be_partial_close(&self.buf) {
            // Buffer might end with a partial </think> — keep it buffered.
            let safe = safe_emit_len(&self.buf, CLOSE_TAG);
            reasoning.push_str(&self.buf[..safe]);
            let keep = self.buf[safe..].to_string();
            self.buf = keep;
        } else {
            // No close tag possible — emit all as reasoning.
            reasoning.push_str(&self.buf);
            self.buf.clear();
        }

        ScanResult { content, reasoning }
    }

    fn handle_outside(&mut self, delta: &str) -> ScanResult {
        self.buf.push_str(delta);
        let mut content = String::new();
        let mut reasoning = String::new();

        if let Some(pos) = self.buf.find(OPEN_TAG) {
            // Found <think> — everything before it is content.
            content.push_str(&self.buf[..pos]);
            let after = pos + OPEN_TAG.len();
            let remainder = self.buf[after..].to_string();
            self.buf.clear();
            self.state = State::InsideThink;
            if !remainder.is_empty() {
                let r = self.handle_inside(&remainder);
                content.push_str(&r.content);
                reasoning.push_str(&r.reasoning);
            }
        } else if could_be_partial_open(&self.buf) {
            let safe = safe_emit_len(&self.buf, OPEN_TAG);
            content.push_str(&self.buf[..safe]);
            let keep = self.buf[safe..].to_string();
            self.buf = keep;
        } else {
            content.push_str(&self.buf);
            self.buf.clear();
        }

        ScanResult { content, reasoning }
    }
}

/// Check if the buffer ends with a partial match of `tag`.
fn could_be_partial_close(buf: &str) -> bool {
    could_be_partial(buf, CLOSE_TAG)
}

fn could_be_partial_open(buf: &str) -> bool {
    could_be_partial(buf, OPEN_TAG)
}

fn could_be_partial(buf: &str, tag: &str) -> bool {
    // Check if any suffix of buf is a prefix of tag.
    for i in 1..tag.len() {
        if buf.ends_with(&tag[..i]) {
            return true;
        }
    }
    false
}

/// How many bytes from the start of `buf` can be safely emitted without risking
/// splitting a partial tag at the boundary.
fn safe_emit_len(buf: &str, tag: &str) -> usize {
    // Find the longest suffix that is a prefix of the tag.
    for i in (1..tag.len()).rev() {
        if buf.ends_with(&tag[..i]) {
            return buf.len() - i;
        }
    }
    buf.len()
}

#[cfg(test)]
mod tests {
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
        // <think> split as "<thi" + "nk>reasoning</think>answer"
        let r1 = s.push("<thi");
        assert_eq!(r1, ScanResult::default()); // buffered, undecided

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
        // In forced mode, content goes to reasoning until </think>
        let r1 = s.push("reasoning text</think>visible answer");
        assert_eq!(r1.reasoning, "reasoning text");
        assert_eq!(r1.content, "visible answer");
    }

    #[test]
    fn forced_mode_strips_leading_think_tag() {
        let mut s = ThinkScanner::forced();
        // MiniMax sends "<think>reasoning</think>answer" — forced mode strips
        // the redundant leading <think>.
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
        // Real MiniMax behavior: model emits "<think>\nreasoning\n</think>\n\nanswer"
        let mut s = ThinkScanner::forced();
        // forced() strips the leading <think>, remainder goes to reasoning.
        let r1 = s.push("<think>\nI need to");
        assert_eq!(r1.reasoning, "\nI need to");
        assert_eq!(r1.content, "");

        let r2 = s.push(" think about this\n</think>\n\nHere is my answer");
        assert_eq!(r2.reasoning, " think about this\n");
        assert_eq!(r2.content, "Here is my answer");
    }
}
