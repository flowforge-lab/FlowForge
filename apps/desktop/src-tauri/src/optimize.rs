//! Manual skill optimize/evolve: gather a skill's body + telemetry + recent
//! transcript, ask the model for a streamlined rewrite, and return the proposed
//! body (M3.5, RFC 0001 §8). The command glue (approval gate, version bump) lives in
//! `lib.rs`; this module owns the prompt and the provider round-trip so the policy is
//! testable without Tauri.

use ff_llm::{ChatMessage, ChatRequest, Provider};
use ff_signals::SkillAggregate;
use futures_util::StreamExt;

/// Build the chat messages for an optimize proposal: a system instruction to
/// streamline while preserving behavior, then the current body, the telemetry
/// summary, and a recent-transcript sample as context.
pub fn build_messages(
    skill: &str,
    body: &str,
    aggregate: Option<&SkillAggregate>,
    transcript: &[String],
) -> Vec<ChatMessage> {
    let system = "You are optimizing a FlowForge skill (a Markdown instruction \
document). Rewrite the skill body to be more streamlined: fewer tokens and fewer \
agent turns to follow, while preserving its behavior, scope, and any concrete \
commands or constraints. Do not invent new capabilities or remove required safety \
notes. Return ONLY the rewritten skill body as Markdown — no preamble, no code \
fence, no commentary.";

    let telemetry = match aggregate {
        Some(a) => format!(
            "Current telemetry for `{skill}`: {} completions, mean {:.0} token-proxy, \
mean {:.1} turns, {:.0}% success.",
            a.completions,
            a.mean_tokens,
            a.mean_turns,
            a.success_rate * 100.0
        ),
        None => format!("No telemetry recorded for `{skill}` yet."),
    };

    let transcript_block = if transcript.is_empty() {
        "No recent transcript available.".to_string()
    } else {
        format!(
            "Recent transcript (most recent last), for context on how the skill is \
used:\n{}",
            transcript.join("\n")
        )
    };

    let user =
        format!("{telemetry}\n\n{transcript_block}\n\nCurrent body of skill `{skill}`:\n\n{body}");

    vec![
        ChatMessage::text("system", system),
        ChatMessage::text("user", user),
    ]
}

/// Strip a surrounding Markdown code fence if the model wrapped its answer in one,
/// so the stored body is the raw instructions.
fn strip_code_fence(s: &str) -> &str {
    let t = s.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    // Drop the optional info string on the opening fence's line.
    let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or("");
    rest.trim_end().strip_suffix("```").unwrap_or(rest).trim()
}

/// Ask the model for a streamlined rewrite of `body`. Returns the proposed body, or
/// an error if the provider fails or yields nothing usable.
pub async fn propose_rewrite(
    provider: &dyn Provider,
    model: &str,
    skill: &str,
    body: &str,
    aggregate: Option<&SkillAggregate>,
    transcript: &[String],
) -> Result<String, String> {
    let req = ChatRequest {
        model: model.to_string(),
        messages: build_messages(skill, body, aggregate, transcript),
        tools: Vec::new(),
        thinking: false,
        max_tokens: None,
    };
    let mut stream = provider.chat_stream(req).await.map_err(|e| e.to_string())?;
    let mut acc = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => {
                acc.push_str(&chunk.delta);
                if chunk.done {
                    break;
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let body = strip_code_fence(&acc).to_string();
    if body.is_empty() {
        return Err("the model returned an empty rewrite".to_string());
    }
    Ok(body)
}

/// Project the post-rewrite mean token cost by scaling the current rolling mean by
/// the body's size change. Returns `(current_mean, estimated_mean)`; `estimated` is
/// `0.0` when there's no telemetry or the original body is empty.
pub fn estimate_cost(aggregate: Option<&SkillAggregate>, before: &str, after: &str) -> (f64, f64) {
    let current = aggregate.map(|a| a.mean_tokens).unwrap_or(0.0);
    let before_chars = before.chars().count();
    let estimated = if current > 0.0 && before_chars > 0 {
        current * (after.chars().count() as f64 / before_chars as f64)
    } else {
        0.0
    };
    (current, estimated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_fenced_output() {
        assert_eq!(
            strip_code_fence("```markdown\nhello\nworld\n```"),
            "hello\nworld"
        );
        assert_eq!(strip_code_fence("```\njust text\n```"), "just text");
        assert_eq!(strip_code_fence("no fence here"), "no fence here");
    }

    #[test]
    fn cost_scales_with_body_size() {
        let agg = SkillAggregate {
            skill: "x".to_string(),
            mean_tokens: 400.0,
            ..Default::default()
        };
        // Halving the body roughly halves the projected cost.
        let (cur, est) = estimate_cost(Some(&agg), "aaaaaaaa", "aaaa");
        assert_eq!(cur, 400.0);
        assert_eq!(est, 200.0);
    }

    #[test]
    fn cost_zero_without_telemetry() {
        let (cur, est) = estimate_cost(None, "aaaa", "aa");
        assert_eq!(cur, 0.0);
        assert_eq!(est, 0.0);
    }

    #[test]
    fn messages_include_body_and_telemetry() {
        let msgs = build_messages("alpha", "do x", None, &[]);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        let user = msgs[1].content.as_deref().unwrap();
        assert!(user.contains("do x"));
        assert!(user.contains("No telemetry"));
    }
}
