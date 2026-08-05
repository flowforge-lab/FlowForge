use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use ff_agent::{run_turn, AgentEvent, CancelToken, ToolContext, UserContext};
use ff_core::{PermissionMatrix, ReasoningVisibility, Role, RunStatus, ScheduledTask};
use ff_llm::Provider;
use ff_scheduled::{RunOutcome, TaskRunner};

use crate::host;

pub struct CliTaskRunner {
    provider: Box<dyn Provider>,
    default_model: String,
    session_store: Arc<ff_session::SessionStore>,
    registry: ff_tools::ToolRegistry,
    /// Usage instructions for the MCP servers whose tools are in `registry` (#1173).
    mcp_guidance: Vec<ff_agent::McpGuidance>,
    /// Stops the MCP servers when this runner is dropped; see `CliGoalIteration` (#1207).
    _mcp_teardown: Option<crate::mcp_host::McpTeardown>,
    workspace: std::path::PathBuf,
}

impl CliTaskRunner {
    pub async fn new() -> Self {
        let (provider, default_model) = host::load_provider();
        let workspace = host::workspace_root();
        let store = Arc::new(ff_session::SessionStore::new());

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
        }
    }
}

#[async_trait]
impl TaskRunner for CliTaskRunner {
    async fn fire(&self, task: &ScheduledTask) -> RunOutcome {
        let prompt = match &task.kind {
            ff_core::TaskKind::Prompt(p) => p.clone(),
            ff_core::TaskKind::Builtin(_) => {
                return RunOutcome {
                    session_id: None,
                    status: RunStatus::Error,
                };
            }
        };

        let session = self.session_store.create_session(None);
        self.session_store
            .add_message(&session.id, Role::User, prompt);

        let matrix = PermissionMatrix::default();
        let approver = ff_scheduled::ScheduledApprover::new(task.safety_ceiling);
        let tool_ctx = ToolContext::new(
            &self.registry,
            &self.workspace,
            &approver,
            ff_agent::DEFAULT_MAX_ITERATIONS,
            &matrix,
        );
        let user_ctx = UserContext::now().with_working_dir(tool_ctx.root.display().to_string());

        let (memory_store, memory_index) = build_memory_store_for_task();
        let (memory, _ambient_keys) = match memory_index {
            Some(idx) => memory_store.ambient_block_filtered_keyed(idx.as_ref()),
            None => (memory_store.ambient_block(), Vec::new()),
        };

        let system_prompt = ff_agent::build_system_prompt(&ff_agent::SystemPromptInputs {
            persona: None,
            skills: &host::load_skills(),
            active: &[],
            user: &user_ctx,
            memory: memory.as_deref(),
            extra_instructions: None,
            goal: None,
            mode: ff_core::Mode::Auto,
            mcp_guidance: &self.mcp_guidance,
        });

        let cancel = CancelToken::new();

        let result = run_turn(
            self.provider.as_ref(),
            &self.session_store,
            &tool_ctx,
            &session.id,
            &self.default_model,
            Some(&system_prompt),
            true,
            ReasoningVisibility::All,
            cancel,
            |event| match event {
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
                AgentEvent::Done { .. } => {}
                AgentEvent::Error { message } => {
                    eprintln!("\n[error] {}", message);
                }
                _ => {}
            },
        )
        .await;

        let status = if approver.needs_attention() {
            RunStatus::NeedsAttention
        } else {
            match result {
                Ok(_) => RunStatus::Ok,
                Err(e) => {
                    eprintln!("Task failed: {}", e);
                    RunStatus::Error
                }
            }
        };

        RunOutcome {
            session_id: Some(session.id),
            status,
        }
    }
}

fn build_memory_store_for_task() -> (
    Arc<ff_memory::Memory>,
    Option<Arc<dyn ff_memory::MemoryIndex>>,
) {
    let memory_store = Arc::new(ff_memory::Memory::with_default_root(
        ff_memory::MemoryConfig::default(),
    ));
    let mut memory_index: Option<Arc<dyn ff_memory::MemoryIndex>> = None;
    if let Ok(index) = ff_memory::Fts5Index::open(memory_store.index_path()) {
        let index: Arc<dyn ff_memory::MemoryIndex> = Arc::new(index);
        let _ = ff_memory::MemoryIndex::reindex(index.as_ref(), &memory_store.all_chunks());
        memory_index = Some(index);
    }
    (memory_store, memory_index)
}

#[cfg(test)]
mod tests;
