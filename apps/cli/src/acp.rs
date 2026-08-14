//! `flowforge acp` — serve FlowForge as an ACP agent over stdio.
//!
//! An ACP client (e.g. Zed) spawns this process and drives it through the
//! agent half of the Agent Client Protocol. The protocol wiring lives in
//! [`ff_acp::agent`]; this module assembles the *host* (LLM provider, tool
//! registry, session store, permission matrix, system-prompt inputs) exactly as
//! [`crate::task_runner`] does for scheduled tasks, and implements
//! [`ff_acp::agent::AcpHost`] over it.

use std::process::ExitCode;
use std::sync::Arc;

use ff_acp::agent::AcpHost;
use ff_agent::{Approver, McpGuidance, SystemPromptInputs, ToolContext, UserContext};
use ff_core::{Mode, PermissionMatrix};
use ff_llm::Provider;
use ff_session::SessionStore;
use ff_skills::SkillRegistry;

/// Owns every resource a turn borrows, so the borrows in
/// [`SystemPromptInputs`] and [`ToolContext`] outlive each `session/prompt`.
struct CliAcpHost {
    provider: Box<dyn Provider>,
    default_model: String,
    store: SessionStore,
    registry: ff_tools::ToolRegistry,
    matrix: PermissionMatrix,
    skills: SkillRegistry,
    user: UserContext,
    workspace: std::path::PathBuf,
    mcp_guidance: Vec<McpGuidance>,
    _mcp_teardown: Option<crate::mcp_host::McpTeardown>,
}

impl AcpHost for CliAcpHost {
    fn provider(&self) -> &dyn Provider {
        self.provider.as_ref()
    }

    fn store(&self) -> &SessionStore {
        &self.store
    }

    fn model(&self) -> &str {
        &self.default_model
    }

    fn tool_context<'a>(&'a self, _mode: Mode, approver: &'a dyn Approver) -> ToolContext<'a> {
        ToolContext::new(
            &self.registry,
            &self.workspace,
            approver,
            ff_agent::DEFAULT_MAX_ITERATIONS,
            &self.matrix,
        )
    }

    fn prompt_inputs(&self, mode: Mode) -> SystemPromptInputs<'_> {
        SystemPromptInputs {
            persona: None,
            skills: &self.skills,
            active: &[],
            user: &self.user,
            memory: None,
            extra_instructions: None,
            goal: None,
            mode,
            mcp_guidance: &self.mcp_guidance,
        }
    }
}

/// Boot the ACP agent server and run it until the client disconnects.
pub async fn run() -> ExitCode {
    let (provider, default_model) = crate::host::load_provider();
    let workspace = crate::host::workspace_root();
    let store = SessionStore::new();

    let (registry, _memory_store, _memory_index, mcp_guidance, mcp_teardown) =
        crate::build_registry_with_mcp().await;

    let user = UserContext::now().with_working_dir(workspace.display().to_string());

    let host = Arc::new(CliAcpHost {
        provider,
        default_model,
        store,
        registry,
        matrix: PermissionMatrix::default(),
        skills: crate::host::load_skills(),
        user,
        workspace,
        mcp_guidance,
        _mcp_teardown: mcp_teardown,
    });

    match ff_acp::agent::serve(host).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ACP server error: {e}");
            ExitCode::FAILURE
        }
    }
}
