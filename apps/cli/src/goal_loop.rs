use std::io::Write;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Local;
use ff_agent::{
    run_session_turn, AgentEvent, CancelToken, GateDecision, GoalIteration, IterationOutcome,
    ToolContext, UserContext,
};
use ff_core::{Goal, GoalStore, Mode, PermissionMatrix, ReasoningVisibility, Role};
use ff_llm::Provider;
use ff_session::SessionStore;

use crate::host::{self, load_skills};

/// The neutral continuation nudge the desktop uses (#778): the system-prompt goal
/// block (#718) already carries the objective, progress, and the `goal_complete`
/// instruction, so repeating the objective here would duplicate it every iteration.
const GOAL_CONTINUE_NUDGE: &str =
    "Continue toward the goal described in your instructions. Take the next \
     concrete step, or call the `goal_complete` tool if it is fully met and verified.";

pub struct CliGoalIteration {
    provider: Box<dyn Provider>,
    default_model: String,
    session_store: std::sync::Arc<SessionStore>,
    registry: ff_tools::ToolRegistry,
    /// Usage instructions for the MCP servers whose tools are in `registry` (#1173).
    mcp_guidance: Vec<ff_agent::McpGuidance>,
    /// Stops the MCP servers when this iteration is dropped. Held here so the servers
    /// outlive every turn that routes through `registry`, and are reaped even on the
    /// error paths (#1207).
    _mcp_teardown: Option<crate::mcp_host::McpTeardown>,
    workspace: PathBuf,
    cancel_token: Option<CancelToken>,
    goal_store: GoalStore,
}

impl CliGoalIteration {
    pub async fn with_cancel(cancel_token: Option<CancelToken>) -> Self {
        let (provider, default_model) = host::load_provider();
        let workspace = host::workspace_root();
        let store = std::sync::Arc::new(SessionStore::new());

        let (mut registry, _memory_store, _memory_index, mcp_guidance, mcp_teardown) =
            crate::build_registry_with_mcp().await;
        registry.register(Box::new(ff_tools::CompactionRetrieveTool::new(
            store.clone(),
        )));

        Self {
            provider,
            default_model,
            session_store: store,
            registry,
            mcp_guidance,
            _mcp_teardown: mcp_teardown,
            workspace,
            cancel_token,
            goal_store: GoalStore::new(ff_core::goal_store_dir()),
        }
    }
}

/// Tracks per-iteration state extracted from the event stream so the loop can
/// detect `goal_complete`, token consumption, and cancellation without re-reading
/// the session store.
struct IterationState {
    tokens: u64,
    cancelled: bool,
    /// Goal-signal collection (`goal_complete` / `goal_step`), shared with the
    /// desktop host so the two cannot drift (#1226).
    ledger: ff_agent::TurnLedger,
}

impl IterationState {
    fn new() -> Self {
        Self {
            tokens: 0,
            cancelled: false,
            ledger: ff_agent::TurnLedger::new(),
        }
    }

    fn handle_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Done {
                token_count: Some(t),
                ..
            } => {
                self.tokens = *t as u64;
            }
            AgentEvent::Error { message } if message.contains("cancelled") => {
                self.cancelled = true;
            }
            _ => self.ledger.observe(event),
        }
    }
}

#[async_trait]
impl GoalIteration for CliGoalIteration {
    fn gate(&self, _goal: &Goal) -> GateDecision {
        GateDecision::Proceed
    }

    async fn run_once(&self, goal: &Goal) -> IterationOutcome {
        let start = std::time::Instant::now();

        // Seed the continuation turn. A pending steer takes priority; otherwise
        // the neutral nudge. The objective lives in the system-prompt goal block
        // so it is not duplicated here (#778).
        let steer = goal
            .pending_steer
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let steer_consumed = steer.is_some();
        let prompt = match steer {
            Some(s) => s.to_string(),
            None => GOAL_CONTINUE_NUDGE.to_string(),
        };
        self.session_store
            .ensure_session(&goal.session_id, Some(goal.objective.clone()));
        self.session_store
            .add_message(&goal.session_id, Role::User, prompt);

        let matrix = PermissionMatrix::default();
        let approver =
            crate::approver::CliApprover::new(crate::approver::ApprovalMode::Yes, Mode::Auto);
        let tool_ctx = ToolContext::new(
            &self.registry,
            &self.workspace,
            &approver,
            ff_agent::DEFAULT_MAX_ITERATIONS,
            &matrix,
        );
        let user_ctx = UserContext::now().with_working_dir(tool_ctx.root.display().to_string());

        let (memory_store, memory_index) = crate::build_memory_store();
        let (memory, _ambient_keys) = match memory_index {
            Some(idx) => memory_store.ambient_block_filtered_keyed(idx.as_ref()),
            None => (memory_store.ambient_block(), Vec::new()),
        };

        let system_prompt_inputs = ff_agent::SystemPromptInputs {
            persona: None,
            skills: &load_skills(),
            active: &[],
            user: &user_ctx,
            memory: memory.as_deref(),
            extra_instructions: None,
            goal: Some(goal),
            mode: Mode::Auto,
            mcp_guidance: &self.mcp_guidance,
        };

        let cancel = self.cancel_token.clone().unwrap_or_default();

        let mut state = IterationState::new();
        let result = run_session_turn(
            self.provider.as_ref(),
            &self.session_store,
            &tool_ctx,
            &goal.session_id,
            &self.default_model,
            &system_prompt_inputs,
            true,
            ReasoningVisibility::All,
            cancel,
            |event| {
                state.handle_event(&event);
                match event {
                    AgentEvent::Token { delta, .. } => {
                        print!("{}", delta);
                        let _ = std::io::stdout().flush();
                    }
                    AgentEvent::ToolCallStarted { name, .. } => {
                        eprintln!("\n[tool] {} ...", name);
                    }
                    AgentEvent::ToolCallFinished {
                        success, result, ..
                    } => {
                        eprintln!("[tool] -> {}", if success { "ok" } else { "failed" });
                        if !success {
                            let snippet: String = result.chars().take(200).collect();
                            eprintln!("{}", snippet);
                        }
                    }
                    AgentEvent::MemoryFlushed { writes, .. } => {
                        eprintln!(
                            "\n[memory] auto-curated {} durable fact{}",
                            writes,
                            if writes == 1 { "" } else { "s" }
                        );
                    }
                    AgentEvent::ToolOutputChunk { delta, .. } => {
                        eprint!("{}", delta);
                        let _ = std::io::stderr().flush();
                    }
                    AgentEvent::Done { .. } => {}
                    AgentEvent::Error { message } => {
                        eprintln!("\n[error] {}", message);
                    }
                    _ => {}
                }
            },
        )
        .await;

        let elapsed = start.elapsed().as_millis() as i64;

        let mut outcome = IterationOutcome {
            wall_ms: elapsed,
            goal_complete: state.ledger.completed(),
            tokens: state.tokens,
            steer_consumed,
            ledger_steps: state.ledger.into_steps(),
            ..Default::default()
        };

        if let Err(e) = result {
            let err_str = e.to_string();
            if err_str.contains("cancelled") || state.cancelled {
                outcome.cancelled = true;
            } else {
                outcome.failed = true;
            }
        }

        outcome
    }

    fn save(&self, goal: &Goal) {
        if let Err(e) = self.goal_store.save(goal) {
            eprintln!("warning: failed to save goal: {}", e);
        }
    }

    fn now_ms(&self) -> i64 {
        Local::now().timestamp_millis()
    }
}

#[cfg(test)]
mod tests;
