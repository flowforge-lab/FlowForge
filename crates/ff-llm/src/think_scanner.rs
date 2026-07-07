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
mod tests;
