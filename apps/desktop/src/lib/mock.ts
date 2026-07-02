// In-browser mock backend. Fulfils the FfIpc contract with canned data and a faked
// token stream, so the frontend runs standalone via `VITE_FF_MOCK=1 pnpm dev`.
//
// Set `VITE_FF_MOCK_SLOW=1` to stream at 300 ms/word instead of 40 ms/word —
// gives you enough time to hit Stop and verify the cancelTurn path.

import type {
  Attachment,
  Message,
  ProviderConfig,
  ProviderConnection,
  ProviderRegistry,
  ModelSelection,
  ResolvedModel,
  ProviderKind,
  SearchConfig,
  ApprovalSafety,
  SearchBackend,
  Session,
  SessionWorkspace,
  TokenEvent,
  ReasoningEvent,
  TurnDoneEvent,
  TurnErrorEvent,
  IntentionSignal,
  ToolApprovalRequestEvent,
  ToolAskRequestEvent,
  ToolCallEvent,
  ToolResultEvent,
  SkillInfo,
  SkillAggregate,
  SkillsChangedEvent,
  SkillEvolveApprovalRequestEvent,
  Phenotype,
  McpServerStatus,
  McpServerConfig,
  McpStatusChangedEvent,
  MemoryChunkStat,
  MemoryFileInfo,
  MemoryOverview,
  MemoryFlushedEvent,
  UpdateProgressEvent,
} from "../bindings";
import type { Format } from "../bindings/Format";
import type { SecretKind } from "../bindings/SecretKind";
import type { BedrockAuth } from "../bindings/BedrockAuth";
import type { FfIpc, Unlisten } from "./ipc";
import type { MarketplaceSkill } from "./marketplace";
import type { MarketplaceProfile } from "./profile-marketplace";
import type {
  ScheduledTask,
  CreateScheduledTaskInput,
  RunRecord,
} from "@/bindings";
import { CONTROL_DEFAULTS, type ControlConfig } from "./control";
import {
  APP_VERSION_FALLBACK,
  type UpdateStatus,
  type BackupResult,
} from "./about";
import { autoTitle } from "./auto-title";
import type { PhenotypeMcpUnavailableEvent } from "@/bindings";

type Listener<T> = (e: T) => void;

const uid = () => crypto.randomUUID();
const now = () => Date.now();

// `SecretKind::ALL` order (#320) — `provider_secret_presence` returns kinds in
// this order so the mock matches the backend's stable presence ordering.
const SECRET_KIND_ALL: readonly SecretKind[] = [
  "apiKey",
  "secretAccessKey",
  "sessionToken",
];

// 300 ms/word in slow mode — long enough to see the Stop button and click it.
const TOKEN_INTERVAL_MS = import.meta.env.VITE_FF_MOCK_SLOW === "1" ? 300 : 40;

// Mirrors the backend `SECRET_ANSWER_PLACEHOLDER` (#562): a secret `ask_user`
// answer is redacted at the source, so the `tool:result` the FE receives carries
// this placeholder, never the typed value.
const SECRET_ANSWER_PLACEHOLDER = "[secret provided by user]";

// A small Markdown document so the renderer's features (headings, lists,
// emphasis, inline code, a fenced + highlighted code block, a table, a link)
// are all exercised under `VITE_FF_MOCK=1`. Uses single spaces between words so
// the word-by-word fake stream reconstructs it faithfully.
const MOCK_REPLY = `### Mocked assistant reply

This is a **mocked assistant reply** streamed token by token so the UI can be built without a running backend. It now renders _Markdown_ — including inline \`code\` and the block below.

- First a short list
- Then some \`inline code\`
- And a [link](https://tauri.app)

\`\`\`ts
// fenced code block with syntax highlighting
export function greet(name: string): string {
  return \`hello, \${name}\`;
}
\`\`\`

| Feature | Status |
| --- | --- |
| Headings | done |
| Code blocks | done |`;

const MOCK_REASONING =
  "Let me scan the README for FlowForge references before suggesting an edit.";

// Canned installed skills so the command palette skill source (#11/#16) is
// exercisable offline. `active`/`score` are placeholders — the methods below
// overlay live active state and search scores.
const MOCK_SKILLS: SkillInfo[] = [
  {
    name: "rust-debugging",
    description: "Systematic Rust debugging with bash, view, and edit.",
    version: "0.1.0",
    keywords: ["rust", "debug"],
    active: false,
    score: 0,
  },
  {
    name: "create-pr",
    description: "Open a GitHub pull request following CONTRIBUTING.",
    version: "0.2.0",
    keywords: ["git", "github", "pr"],
    active: false,
    score: 0,
  },
  {
    name: "write-tests",
    description: "Generate unit tests with coverage analysis.",
    version: "0.1.0",
    keywords: ["test", "coverage"],
    active: false,
    score: 0,
  },
  // Bundled with the Codon phenotype (#235/#304); seeded on first run.
  {
    name: "codegraph",
    description:
      "Code-aware navigation: definitions, callers, callees, impact.",
    version: "0.1.0",
    keywords: ["code", "graph", "navigation"],
    active: false,
    score: 0,
  },
];

// Required MCP server ids per skill, mirroring `SkillManifest.mcp` (which the
// `SkillInfo` binding deliberately omits, so the FE can't read it). Lets the mock
// reproduce PR #296's `missing_skill_mcp_servers` diff and emit the #301 event:
// `codegraph` wants the `codegraph` server, which is absent from `MOCK_MCP`, so
// switching to the `codon` phenotype surfaces the unavailable-tools toast.
const SKILL_MCP_REQUIREMENTS: Record<string, string[]> = {
  codegraph: ["codegraph"],
};

// Mirrors `ff_skills::search_skills` scoring so the mock ranks like the backend:
// exact keyword (4) > name prefix (3) > name substring (2) > description (1).
function scoreSkill(skill: SkillInfo, q: string): number {
  const name = skill.name.toLowerCase();
  if (skill.keywords.some((k) => k.toLowerCase() === q)) return 4;
  if (name.startsWith(q)) return 3;
  if (name.includes(q)) return 2;
  if (skill.description.toLowerCase().includes(q)) return 1;
  return 0;
}

// Canned marketplace catalog so the Skills → Marketplace search (SET.5) is
// exercisable offline. FE-only (`MarketplaceSkill`); no backend catalog yet.
const MOCK_MARKETPLACE: MarketplaceSkill[] = [
  {
    name: "docx-author",
    description: "Create and edit Word documents with headings and tables.",
    version: "1.4.0",
    author: "flowforge-labs",
    keywords: ["docx", "word", "document"],
    installs: 12400,
  },
  {
    name: "pdf-toolkit",
    description: "Merge, split, and extract text from PDF files.",
    version: "2.0.1",
    author: "flowforge-labs",
    keywords: ["pdf", "merge", "ocr"],
    installs: 9800,
  },
  {
    name: "sql-explain",
    description: "Explain and optimize SQL queries against a schema.",
    version: "0.6.2",
    author: "community",
    keywords: ["sql", "database", "query"],
    installs: 4300,
  },
  {
    name: "k8s-debug",
    description: "Diagnose Kubernetes pods, events, and rollout failures.",
    version: "0.3.0",
    author: "community",
    keywords: ["kubernetes", "k8s", "debug"],
    installs: 2100,
  },
];

// Ranks a marketplace skill like `scoreSkill` (keyword > name prefix > name
// substring > description), so the mock search orders results sensibly.
function scoreMarketplace(skill: MarketplaceSkill, q: string): number {
  const name = skill.name.toLowerCase();
  if (skill.keywords.some((k) => k.toLowerCase() === q)) return 4;
  if (name.startsWith(q)) return 3;
  if (name.includes(q)) return 2;
  if (skill.description.toLowerCase().includes(q)) return 1;
  return 0;
}

// The built-in phenotype: no skills, no overrides (RFC 0001 §7). Always present.
const DEFAULT_PHENOTYPE: Phenotype = {
  name: "default",
  skills: [],
  mcpServers: [],
};

// Canned phenotypes so the `pheno` palette (#28) is exercisable offline. `default`
// is prepended by `listPhenotypes`, matching the backend.
const MOCK_PHENOTYPES: Phenotype[] = [
  // The out-of-box default phenotype (#298), seeded on first run (#304). Listed
  // first after `default` so the Phenos → Installed star/reset target it.
  // Keep in sync with docs/examples/codon/phenos/codon.toml (#304) — the seed is
  // the source of truth; persona is summarized here.
  {
    name: "codon",
    skills: ["codegraph"],
    persona:
      "A disciplined software engineer: Research → Plan → Implement → Verify, codegraph-first navigation.",
    maxIterations: 50,
    // codegraph travels with codon as a workspace-scoped MCP server (RFC 0018 C3).
    mcpServers: [
      {
        id: "codegraph",
        command: "codegraph",
        args: ["serve", "--mcp"],
        env: {},
        disabled: false,
        scope: "workspace",
      },
    ],
  },
  {
    name: "rust",
    skills: ["rust-debugging", "write-tests"],
    persona: "You are a meticulous Rust engineer.",
    mcpServers: [],
  },
  {
    name: "reviewer",
    skills: ["create-pr"],
    // Binds a provider + model (RFC 0005 §11.2, Phase C) so the per-pane model
    // chip's phenotype tier is exercisable offline: a session on this phenotype
    // (no session override) resolves to openai/gpt-4o, not the global default.
    provider: "openai",
    model: "gpt-4o",
    mcpServers: [],
  },
];

// Canned profile catalog so the Profiles → Marketplace search (SET.7) is
// exercisable offline. FE-only (`MarketplaceProfile`); no backend catalog yet.
const MOCK_PROFILE_MARKETPLACE: MarketplaceProfile[] = [
  {
    id: "data-science",
    name: "Data Science",
    description: "Notebooks, dataframes, and plotting helpers.",
    skillCount: 6,
    author: "flowforge-labs",
    installs: 8200,
  },
  {
    id: "web-frontend",
    name: "Web Frontend",
    description: "React, TypeScript, and CSS-focused working set.",
    skillCount: 5,
    author: "flowforge-labs",
    installs: 7100,
  },
  {
    id: "devops",
    name: "DevOps",
    description: "Terraform, Kubernetes, and CI pipeline skills.",
    skillCount: 4,
    author: "community",
    installs: 3400,
  },
  {
    id: "technical-writer",
    name: "Technical Writer",
    description: "Docs, changelogs, and API reference authoring.",
    skillCount: 3,
    author: "community",
    installs: 1900,
  },
];

// Ranks a marketplace profile by name/description match, mirroring the skill
// marketplace scorer so results order sensibly.
function scoreProfile(profile: MarketplaceProfile, q: string): number {
  const name = profile.name.toLowerCase();
  const id = profile.id.toLowerCase();
  if (id === q || name === q) return 4;
  if (id.startsWith(q) || name.startsWith(q)) return 3;
  if (id.includes(q) || name.includes(q)) return 2;
  if (profile.description.toLowerCase().includes(q)) return 1;
  return 0;
}

const HOUR_MS = 60 * 60 * 1000;

// Seed scheduled tasks (RFC 0017) with times relative to load so "Next/Last" read
// sensibly offline. One built-in "Memory Organizer" (RFC 0006 consolidation) that
// runs daily, plus a weekly digest, so the list shows running + paused states.
function initialScheduledTasks(): ScheduledTask[] {
  const now = Date.now();
  return [
    {
      id: "memory-organizer",
      name: "Memory Organizer",
      kind: { kind: "builtin", value: "memory_consolidate" },
      cron: "0 0 17 * * *",
      safetyCeiling: "read_only",
      cadenceLabel: "Daily at 5:00 PM",
      nextRun: now + 6 * HOUR_MS,
      lastRun: now - 18 * HOUR_MS,
      paused: false,
      catchUp: false,
    },
    {
      id: "weekly-digest",
      name: "Weekly Digest",
      kind: { kind: "prompt", value: "Summarise my week." },
      cron: "0 0 9 * * 1",
      safetyCeiling: "read_only",
      cadenceLabel: "Mondays at 9:00 AM",
      paused: true,
      lastRun: now - 4 * 24 * HOUR_MS,
      catchUp: false,
    },
  ];
}

// Canned MCP server statuses so the servers panel (#91) is exercisable offline.
// One running with tools, one that restarted once, one failed-to-spawn.
const MOCK_MCP: McpServerStatus[] = [
  { id: "github", state: "running", toolCount: 8, restarts: 0, pid: 4242 },
  { id: "filesystem", state: "running", toolCount: 3, restarts: 1, pid: 4243 },
  {
    id: "playwright",
    state: "failed",
    toolCount: 0,
    lastError: "spawn npx ENOENT",
    restarts: 5,
  },
];

interface ActiveTurn {
  // All pending interval/timeout handles for this turn, cleared on cancel.
  timers: ReturnType<typeof setInterval>[];
  messageId: string;
  // callIds emitted but not yet resolved. On cancel these are backfilled with a
  // "[cancelled]" result, mirroring the real backend's tool-result backfill so a
  // cancelled step never spins forever in the UI.
  pendingToolCalls: string[];
}

const uidShort = () => crypto.randomUUID().slice(0, 8);

// Composite key mirroring the backend's `(session_id, call_id)` pending key.
const approvalKey = (sessionId: string, callId: string) =>
  `${sessionId}\u0000${callId}`;

// Mirrors `ff_skills::bump_patch`: increments the trailing numeric segment, or
// appends `.1` when it isn't a plain integer.
const bumpPatch = (version: string): string => {
  const idx = version.lastIndexOf(".");
  const head = idx === -1 ? "" : version.slice(0, idx);
  const last = idx === -1 ? version : version.slice(idx + 1);
  if (/^\d+$/.test(last)) {
    const next = Number(last) + 1;
    return head === "" ? String(next) : `${head}.${next}`;
  }
  return `${version}.1`;
};

// Tool output beyond this many chars is folded in the Markdown export so a noisy
// step doesn't dominate the document (mirrors the backend's truncation).
const TOOL_FOLD_LIMIT = 2000;

/** Human-readable Markdown for a session: title + metadata, then `## You` /
 *  `## Assistant` / `## Tool` sections. `Role::System` is skipped; assistant tool
 *  calls render as `**Tool call:** name(args)`; long tool output is folded. */
function renderSessionMarkdown(session: Session, messages: Message[]): string {
  const lines: string[] = [
    `# ${session.title?.trim() || "Untitled session"}`,
    "",
  ];
  const meta: string[] = [];
  if (session.goal) meta.push(`- **Goal:** ${session.goal}`);
  meta.push(`- **Created:** ${new Date(session.createdAt).toISOString()}`);
  meta.push(`- **Updated:** ${new Date(session.updatedAt).toISOString()}`);
  if (session.phenotype) meta.push(`- **Phenotype:** ${session.phenotype}`);
  if (meta.length) lines.push(...meta, "");

  for (const m of messages) {
    if (m.role === "system") continue;
    lines.push(
      m.role === "user"
        ? "## You"
        : m.role === "assistant"
          ? "## Assistant"
          : "## Tool",
      "",
    );
    if (m.content.trim()) {
      const body =
        m.role === "tool" && m.content.length > TOOL_FOLD_LIMIT
          ? `${m.content.slice(0, TOOL_FOLD_LIMIT)}\n… (${m.content.length - TOOL_FOLD_LIMIT} more chars truncated)`
          : m.content.trim();
      lines.push(body, "");
    }
    if (m.role === "assistant" && m.toolCalls?.length) {
      for (const c of m.toolCalls) {
        lines.push(`**Tool call:** ${c.name}(${c.arguments})`, "");
      }
    }
  }
  return `${lines.join("\n").trimEnd()}\n`;
}

export class MockIpc implements FfIpc {
  private sessions = new Map<string, Session>();
  private messages = new Map<string, Message[]>();
  // Per-session working directory (slice 3b, #200). Mirrors the backend's
  // in-memory session_cwd map; unset sessions report the default root.
  private workspaces = new Map<string, string>();
  private readonly defaultWorkspace = "/Users/you/projects/flowforge";
  /** Per-session current git branch, set by `checkoutBranch` (#628). The mock has
   *  no real repo, so this stands in for `.git/HEAD` and backs `getSessionWorkspace`. */
  private mockBranches = new Map<string, string>();
  /** Local branches the mock offers in the switch picker (#628). */
  private readonly mockBranchList = ["main", "develop", "feature/demo"];
  // One active timer per session so cancelTurn can stop it.
  private activeTimers = new Map<string, ActiveTurn>();

  // Provider connection registry (Issue #138, RFC 0005 Phase A). Mirrors the
  // backend default (local candle-vLLM active + a keyless Ollama), plus a hosted
  // AWS Bedrock connection (#202 PR-3b) so the provider-card surface and the
  // Bedrock credentials form are exercisable offline. Persistence is in-memory.
  private registry: ProviderRegistry = {
    schemaVersion: 1,
    active: "candle-vllm",
    connections: [
      {
        id: "candle-vllm",
        kind: "candleVllm",
        displayName: "candle-vLLM",
        model: "Qwen3-4B-Instruct-2507",
        hasKey: false,
        secretMissing: false,
        thinking: false,
        reasoningEffort: "medium",
        reasoningVisibility: "all",
        warmupEnabled: true,
      },
      {
        id: "ollama",
        kind: "ollama",
        displayName: "Ollama",
        model: "llama3.2",
        hasKey: false,
        secretMissing: false,
        thinking: false,
        reasoningEffort: "medium",
        reasoningVisibility: "all",
        warmupEnabled: true,
      },
      {
        id: "bedrock",
        kind: "bedrock",
        displayName: "AWS Bedrock",
        model: "us.anthropic.claude-sonnet-4-0",
        hasKey: false,
        secretMissing: false,
        thinking: true,
        reasoningEffort: "low",
        reasoningVisibility: "all",
        warmupEnabled: true,
        region: "us-east-1",
        authMode: "profile",
        awsProfile: "bedrock-profile",
      },
      // SiliconFlow (#329): a hosted, OpenAI-compatible connection so the
      // hosted-key provider card (editable base URL + bearer API key) is
      // exercisable offline. Keyless by default → the Test probe fails until a
      // key is stored, mirroring the real `list_models`-based probe. `baseUrl`
      // unset defaults to the global `.com` endpoint.
      {
        id: "siliconflow",
        kind: "siliconFlow",
        displayName: "SiliconFlow",
        model: "deepseek-ai/DeepSeek-V3",
        hasKey: false,
        secretMissing: false,
        thinking: true,
        reasoningEffort: "high",
        reasoningVisibility: "all",
        warmupEnabled: true,
      },
      // OpenAI (#311 PR-3b): a hosted, OpenAI-compatible connection so the
      // hosted-key card is exercisable offline. Keyless by default → the Test
      // probe fails until a key is stored, mirroring the real `list_models`
      // probe (401 on a missing/bad key). `baseUrl` unset defaults to
      // https://api.openai.com/v1.
      {
        id: "openai",
        kind: "openai",
        displayName: "OpenAI",
        model: "gpt-4o",
        hasKey: false,
        secretMissing: false,
        thinking: true,
        reasoningEffort: "medium",
        reasoningVisibility: "all",
        warmupEnabled: true,
      },
    ],
  };

  // Canned per-connection model lists so the picker is exercisable offline. The
  // Bedrock list mirrors a ListInferenceProfiles response (cross-region + on-demand
  // inference-profile ids).
  private modelsByConnection: Record<string, string[]> = {
    "candle-vllm": ["Qwen3-4B-Instruct-2507", "Qwen3-8B-Instruct"],
    ollama: ["llama3.2", "qwen2.5", "mistral"],
    bedrock: [
      "us.anthropic.claude-opus-4-0",
      "us.anthropic.claude-sonnet-4-0",
      "us.anthropic.claude-haiku-3-5",
      "us.meta.llama4-maverick-17b-instruct",
      "amazon.nova-pro-v1:0",
      "amazon.nova-lite-v1:0",
    ],
    // SiliconFlow catalog sample (#329) — a representative slice of the real
    // catalog so the model picker populates from `list_models` and overflows its
    // scroll area offline (the real endpoint returns ~70 ids).
    siliconflow: [
      "deepseek-ai/DeepSeek-R1",
      "deepseek-ai/DeepSeek-V3",
      "deepseek-ai/DeepSeek-V3.1",
      "deepseek-ai/deepseek-vl2",
      "Qwen/Qwen3-8B",
      "Qwen/Qwen3-14B",
      "Qwen/Qwen3-32B",
      "Qwen/Qwen3-235B-A22B",
      "Qwen/Qwen3-30B-A3B-Instruct-2507",
      "Qwen/Qwen3-Coder-30B-A3B-Instruct",
      "Qwen/Qwen3-Coder-480B-A35B-Instruct",
      "Qwen/Qwen2.5-7B-Instruct",
      "Qwen/Qwen2.5-14B-Instruct",
      "Qwen/Qwen2.5-32B-Instruct",
      "Qwen/Qwen2.5-72B-Instruct",
      "Qwen/Qwen2.5-VL-7B-Instruct",
      "THUDM/GLM-4-9B-0414",
      "THUDM/GLM-4-32B-0414",
      "THUDM/GLM-Z1-32B-0414",
      "zai-org/GLM-4.5",
      "zai-org/GLM-4.5-Air",
      "zai-org/GLM-4.6",
      "moonshotai/Kimi-K2-Instruct",
      "moonshotai/Kimi-K2-Thinking",
      "meta-llama/Meta-Llama-3.1-8B-Instruct",
      "meta-llama/Llama-3.3-70B-Instruct",
      "tencent/Hunyuan-A13B-Instruct",
      "baidu/ERNIE-4.5-300B-A47B",
      "ByteDance-Seed/Seed-OSS-36B-Instruct",
      "openai/gpt-oss-120b",
      "openai/gpt-oss-20b",
    ],
    // OpenAI catalog sample (#311) so the picker populates from `list_models`.
    openai: ["gpt-4o", "gpt-4o-mini", "gpt-4.1", "o3", "o4-mini"],
  };

  // Per-connection secret material stored "in the keychain" (Issue #202 PR-3).
  // The mock only tracks *which* `SecretKind`s are set so it can recompute
  // `hasKey`; the values themselves are written then discarded, mirroring the
  // backend's write-only contract (a secret value is never read back over IPC).
  private secretsByConnection = new Map<string, Set<SecretKind>>();

  // Web-search settings (Issue #43). Defaults mirror the backend: SearXNG with no
  // endpoint configured. Persistence is in-memory for the mock session.
  private searchConfig: SearchConfig = {
    backend: "searxNg",
    hasKey: false,
  };

  // Control settings (Issue #127). In-memory for the mock session; structurally
  // cloned on read/write so callers can't mutate the stored copy.
  private controlConfig: ControlConfig = structuredClone(CONTROL_DEFAULTS);

  // Scheduled tasks (SET.9). In-memory for the mock session — created tasks survive
  // a section reopen but reset on app reload (no real scheduler).
  private scheduledTasks: ScheduledTask[] = initialScheduledTasks();

  private tokenListeners = new Set<Listener<TokenEvent>>();
  private reasoningListeners = new Set<Listener<ReasoningEvent>>();
  private doneListeners = new Set<Listener<TurnDoneEvent>>();
  private errorListeners = new Set<Listener<TurnErrorEvent>>();
  private intentionListeners = new Set<Listener<IntentionSignal>>();
  private toolCallListeners = new Set<Listener<ToolCallEvent>>();
  private toolResultListeners = new Set<Listener<ToolResultEvent>>();
  private approvalRequestListeners = new Set<
    Listener<ToolApprovalRequestEvent>
  >();
  /** `(sessionId, callId)` -> resume callback. Set when a write tool emits an
   *  approval request; the matching `respondApproval` resolves it. Keyed by both
   *  so colliding call ids across sessions stay isolated (mirrors the backend). */
  private pendingApprovals = new Map<string, (approved: boolean) => void>();
  // Four-option approval (#229). Real in-memory sets so the mock reproduces the
  // backend's short-circuit: a tool approved "this session" / "always" skips the
  // approval-request entirely on the next call. `sessionApproved` is cleared on
  // session delete (mirrors `clear_session_approvals`); `alwaysApproved` persists
  // for the mock's lifetime (mirrors `tool_permissions.json`).
  private sessionApproved = new Map<string, Set<string>>();
  private alwaysApproved = new Set<string>();
  private askRequestListeners = new Set<Listener<ToolAskRequestEvent>>();
  /** `(sessionId, callId)` -> resume callback for an `ask_user` (#44). The matching
   *  `respondAsk` resolves it; cancel deletes it (mirrors backend cancel-via-drop). */
  private pendingAsks = new Map<string, (answer: string) => void>();
  private skillsChangedListeners = new Set<Listener<SkillsChangedEvent>>();
  private evolveApprovalListeners = new Set<
    Listener<SkillEvolveApprovalRequestEvent>
  >();
  /** Per-skill current-version overrides applied by `optimizeSkill`/`rollbackSkill`;
   *  falls back to the static `MOCK_SKILLS` version when absent. */
  private skillVersions = new Map<string, string>();
  /** Per-skill archived versions, newest-first (mirrors the backend history tree). */
  private archivedVersions = new Map<string, string[]>();
  private activeSkills = new Set<string>();
  private activePhenotype: Phenotype = DEFAULT_PHENOTYPE;
  // User phenotypes (everything but the built-in `default`). Mutable so the editor
  // write path (`updatePhenotype`, #530) can upsert; seeded from the canned set.
  private phenotypes: Phenotype[] = MOCK_PHENOTYPES.map((p) => ({ ...p }));
  // Per-session model overrides (RFC 0005 §11.2, Phase D; #499). Absent → the
  // session inherits the phenotype/global tiers; see resolveModelSelection.
  private sessionModelSelections = new Map<string, ModelSelection>();

  // MCP server statuses, keyed by id (mutated by enable/disable/restart/add/remove).
  private mcpServers = new Map<string, McpServerStatus>(
    MOCK_MCP.map((s) => [s.id, { ...s }]),
  );
  private mcpStatusListeners = new Set<Listener<McpStatusChangedEvent>>();
  private memoryFlushedListeners = new Set<Listener<MemoryFlushedEvent>>();
  // Scheduled-task firing (#543). `scheduled:fired` carries the run a fire created
  // (taskId → sessionId linkage); `scheduled:changed` carries a full task snapshot
  // that replaces the store wholesale (mirrors `mcp:status-changed`).
  private scheduledFiredListeners = new Set<Listener<RunRecord>>();
  private scheduledChangedListeners = new Set<Listener<ScheduledTask[]>>();
  /** Monotonic run-id source for the `RunRecord`s `runScheduledTaskNow` mints. */
  private scheduledRunSeq = 0;
  /** Per-task fire history (newest pushed last), backing `listScheduledRuns`. */
  private scheduledRuns: RunRecord[] = [];
  /** Global pause-all kill-switch state (RFC 0017 §8.3). */
  private scheduledPausedAll = false;
  private phenoMcpUnavailableListeners = new Set<
    Listener<PhenotypeMcpUnavailableEvent>
  >();
  private updateProgressListeners = new Set<Listener<UpdateProgressEvent>>();
  private updateDownloadFinishedListeners = new Set<Listener<void>>();
  private workspaceBranchListeners = new Set<Listener<SessionWorkspace>>();
  // Sessions that have already simulated a context-pressure flush (#283). The
  // real flush only fires once a session crosses the budget; the mock stands in
  // by flushing once per session so the provenance surface is exercisable.
  private flushedSessions = new Set<string>();

  async createSession(goal?: string): Promise<Session> {
    const ts = now();
    const session: Session = {
      id: uid(),
      goal: goal ?? null,
      title: null,
      summary: null,
      status: "active",
      createdAt: ts,
      updatedAt: ts,
    };
    this.sessions.set(session.id, session);
    this.messages.set(session.id, []);
    if (goal) {
      this.emit(this.intentionListeners, { sessionId: session.id, goal });
    }
    return session;
  }

  // Fork (#149): clone a session's transcript into a fresh session. The new
  // session carries the source's goal + a `" (copy)"`-suffixed title, and a deep
  // copy of its messages re-keyed to the new session id. Mirrors what the real
  // backend command must do server-side.
  async forkSession(sessionId: string): Promise<Session> {
    const source = this.sessions.get(sessionId);
    if (!source) throw new Error(`unknown session: ${sessionId}`);
    const ts = now();
    const forked: Session = {
      ...source,
      id: uid(),
      title: source.title ? `${source.title} (copy)` : null,
      createdAt: ts,
      updatedAt: ts,
    };
    const clonedMessages: Message[] = (this.messages.get(sessionId) ?? []).map(
      (m) => ({ ...m, id: uid(), sessionId: forked.id }),
    );
    this.sessions.set(forked.id, forked);
    this.messages.set(forked.id, clonedMessages);
    return { ...forked };
  }

  async getSessionWorkspace(sessionId: string): Promise<SessionWorkspace> {
    // The mock has no real filesystem; the branch is whatever `checkoutBranch`
    // last set for this session (null until then), standing in for `.git/HEAD`.
    return {
      path: this.workspaces.get(sessionId) ?? this.defaultWorkspace,
      gitBranch: this.mockBranches.get(sessionId) ?? null,
    };
  }

  async setSessionWorkspace(sessionId: string, path: string): Promise<string> {
    const trimmed = path.trim();
    if (!trimmed) throw new Error("cannot resolve directory: empty path");
    this.workspaces.set(sessionId, trimmed);
    this.emitBranchChanged(trimmed);
    return trimmed;
  }

  async listBranches(_sessionId: string): Promise<string[]> {
    return [...this.mockBranchList];
  }

  async checkoutBranch(
    sessionId: string,
    branch: string,
  ): Promise<SessionWorkspace> {
    if (!this.mockBranchList.includes(branch)) {
      throw new Error(`unknown branch: ${branch}`);
    }
    this.mockBranches.set(sessionId, branch);
    const path = this.workspaces.get(sessionId) ?? this.defaultWorkspace;
    // Mirror the real command: route the switch through `workspace:branch-changed`
    // so the chip updates + flashes (#627) via the same reactive path an external
    // checkout uses -- no extra FE wiring needed for the confirmation.
    setTimeout(() => {
      this.emit(this.workspaceBranchListeners, { path, gitBranch: branch });
    }, 0);
    return { path, gitBranch: branch };
  }

  async listSessions(): Promise<Session[]> {
    return [...this.sessions.values()].sort(
      (a, b) => b.updatedAt - a.updatedAt,
    );
  }

  async renameSession(sessionId: string, title: string): Promise<void> {
    const s = this.sessions.get(sessionId);
    if (s) {
      s.title = title;
      s.updatedAt = now();
    }
  }

  // Delete (#168): permanently drop a session and its transcript. Idempotent —
  // deleting an unknown id is a no-op. Mirrors the real backend command.
  async deleteSession(sessionId: string): Promise<void> {
    this.sessions.delete(sessionId);
    this.messages.delete(sessionId);
    // Session-scoped approvals expire with the session (backend clears them in
    // `clear_session_approvals` on delete).
    this.sessionApproved.delete(sessionId);
  }

  async getMessages(sessionId: string): Promise<Message[]> {
    return [...(this.messages.get(sessionId) ?? [])];
  }

  // Export (#278). Mirrors the backend `export_session`: a lossless JSON envelope
  // ({ session, messages }, pretty-printed) or a human-readable Markdown render.
  // Rejects an unknown id (the backend returns None → the command errors).
  async exportSession(sessionId: string, format: Format): Promise<string> {
    const session = this.sessions.get(sessionId);
    if (!session) throw new Error(`unknown session: ${sessionId}`);
    const messages = this.messages.get(sessionId) ?? [];
    if (format === "json") {
      return JSON.stringify({ session, messages }, null, 2);
    }
    return renderSessionMarkdown(session, messages);
  }

  async sendMessage(
    sessionId: string,
    content: string,
    attachments?: Attachment[],
  ): Promise<string> {
    const user = this.append(sessionId, "user", content, attachments);
    // Dev-only: `/cap` reproduces a turn that ends at the tool-call limit with no
    // streamed answer — the agent loop's empty-content finalizer writes a
    // `[stopped: …]` notice server-side without emitting tokens (#513). It's stored
    // as history (surfaced on the next getMessages refetch) and the turn is closed
    // with `turn:done`, so the one-click Continue affordance can be exercised here.
    if (content.trim() === "/cap") {
      const stopped = this.append(
        sessionId,
        "assistant",
        "[stopped: reached tool-call limit]",
      );
      this.emit(this.doneListeners, {
        sessionId,
        messageId: stopped.id,
        tokenCount: null,
      });
      return user.id;
    }
    // Dev-only (#636): `/cancel` reproduces a user Stop during the pending window —
    // the backend replaces the empty assistant message with a bare `[stopped]`
    // marker (no tokens) and closes the turn. Surfaced on the next getMessages
    // refetch so the Cancelled banner + one-click Continue can be exercised here.
    if (content.trim() === "/cancel") {
      const stopped = this.append(sessionId, "assistant", "[stopped]");
      this.emit(this.doneListeners, {
        sessionId,
        messageId: stopped.id,
        tokenCount: null,
      });
      return user.id;
    }
    // Dev-only (#562): a message mentioning a secret/password drives the
    // `ask_user` step's masked variant, so the `<input type="password">` +
    // at-rest redaction path is demoable under VITE_FF_MOCK=1.
    const secret = /\b(secret|password|passphrase)\b/i.test(content);
    this.streamAssistant(sessionId, secret);
    return user.id;
  }

  async editMessage(
    sessionId: string,
    messageId: string,
    content: string,
    attachments?: Attachment[],
  ): Promise<string> {
    const list = this.messages.get(sessionId);
    const idx = list?.findIndex((m) => m.id === messageId) ?? -1;
    if (!list || idx < 0) throw new Error("no such message");
    if (list[idx].role !== "user") throw new Error("not a user message");
    // Mirror the backend: cancel any in-flight turn before mutating the
    // transcript, so a running turn can't append past the cut we're making.
    await this.cancelTurn(sessionId);
    // Replace the edited message in place and truncate everything after it.
    const edited: Message = {
      ...list[idx],
      content,
      ...(attachments?.length ? { attachments } : { attachments: undefined }),
    };
    this.messages.set(sessionId, [...list.slice(0, idx), edited]);
    const s = this.sessions.get(sessionId);
    if (s) s.updatedAt = now();
    this.streamAssistant(sessionId);
    return edited.id;
  }

  async cancelTurn(sessionId: string): Promise<void> {
    const active = this.activeTimers.get(sessionId);
    if (!active) return;
    active.timers.forEach((t) => clearInterval(t));
    this.activeTimers.delete(sessionId);
    // Any tool call still in flight needs a matching result, or its step would
    // spin forever — the real backend backfills "[cancelled]" the same way.
    for (const callId of active.pendingToolCalls) {
      this.emit(this.toolResultListeners, {
        sessionId,
        messageId: active.messageId,
        callId,
        success: false,
        result: "[cancelled]",
      });
      // Drop any awaiting-approval entry — its tool:result was just emitted.
      this.pendingApprovals.delete(approvalKey(sessionId, callId));
      // Drop any pending ask_user resume too, or a later respondAsk would fire
      // the surviving callback and emit a duplicate tool:result for a dead turn.
      this.pendingAsks.delete(approvalKey(sessionId, callId));
    }
    // Emit done with whatever partial content was accumulated — mirrors what
    // the real backend does when a CancellationToken fires.
    const cancelledContent =
      this.messages.get(sessionId)?.find((m) => m.id === active.messageId)
        ?.content ?? "";
    this.emit(this.doneListeners, {
      sessionId,
      messageId: active.messageId,
      // Rough estimate (~4 chars/token) — mirrors the real backend supplying a
      // context-usage token count on Done.
      tokenCount: Math.ceil(cancelledContent.length / 4),
    });
  }

  onToken(cb: Listener<TokenEvent>): Promise<Unlisten> {
    return this.subscribe(this.tokenListeners, cb);
  }
  onReasoning(cb: Listener<ReasoningEvent>): Promise<Unlisten> {
    return this.subscribe(this.reasoningListeners, cb);
  }
  onTurnDone(cb: Listener<TurnDoneEvent>): Promise<Unlisten> {
    return this.subscribe(this.doneListeners, cb);
  }
  onTurnError(cb: Listener<TurnErrorEvent>): Promise<Unlisten> {
    return this.subscribe(this.errorListeners, cb);
  }
  onIntention(cb: Listener<IntentionSignal>): Promise<Unlisten> {
    return this.subscribe(this.intentionListeners, cb);
  }
  onToolCall(cb: Listener<ToolCallEvent>): Promise<Unlisten> {
    return this.subscribe(this.toolCallListeners, cb);
  }
  onToolResult(cb: Listener<ToolResultEvent>): Promise<Unlisten> {
    return this.subscribe(this.toolResultListeners, cb);
  }
  onApprovalRequest(cb: Listener<ToolApprovalRequestEvent>): Promise<Unlisten> {
    return this.subscribe(this.approvalRequestListeners, cb);
  }
  onSkillsChanged(cb: Listener<SkillsChangedEvent>): Promise<Unlisten> {
    return this.subscribe(this.skillsChangedListeners, cb);
  }
  onEvolveApprovalRequest(
    cb: Listener<SkillEvolveApprovalRequestEvent>,
  ): Promise<Unlisten> {
    return this.subscribe(this.evolveApprovalListeners, cb);
  }
  onAskRequest(cb: Listener<ToolAskRequestEvent>): Promise<Unlisten> {
    return this.subscribe(this.askRequestListeners, cb);
  }
  onMcpStatusChanged(cb: Listener<McpStatusChangedEvent>): Promise<Unlisten> {
    return this.subscribe(this.mcpStatusListeners, cb);
  }

  onMemoryFlushed(cb: Listener<MemoryFlushedEvent>): Promise<Unlisten> {
    return this.subscribe(this.memoryFlushedListeners, cb);
  }

  onScheduledFired(cb: Listener<RunRecord>): Promise<Unlisten> {
    return this.subscribe(this.scheduledFiredListeners, cb);
  }

  onScheduledChanged(cb: Listener<ScheduledTask[]>): Promise<Unlisten> {
    return this.subscribe(this.scheduledChangedListeners, cb);
  }

  onPhenotypeMcpUnavailable(
    cb: Listener<PhenotypeMcpUnavailableEvent>,
  ): Promise<Unlisten> {
    return this.subscribe(this.phenoMcpUnavailableListeners, cb);
  }
  onWorkspaceBranchChanged(cb: Listener<SessionWorkspace>): Promise<Unlisten> {
    return this.subscribe(this.workspaceBranchListeners, cb);
  }

  onUpdateProgress(cb: Listener<UpdateProgressEvent>): Promise<Unlisten> {
    return this.subscribe(this.updateProgressListeners, cb);
  }

  onUpdateDownloadFinished(cb: Listener<void>): Promise<Unlisten> {
    return this.subscribe(this.updateDownloadFinishedListeners, cb);
  }

  async listMcpServers(): Promise<McpServerStatus[]> {
    return [...this.mcpServers.values()]
      .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
      .map((s) => ({ ...s }));
  }

  async restartMcpServer(id: string): Promise<void> {
    const s = this.mcpServers.get(id);
    if (!s) return;
    s.restarts += 1;
    s.state = "running";
    s.lastError = undefined;
    this.emitMcpStatusChanged();
  }

  async setMcpServerEnabled(id: string, enabled: boolean): Promise<void> {
    const s = this.mcpServers.get(id);
    if (!s) return;
    s.state = enabled ? "running" : "disabled";
    if (enabled) s.lastError = undefined;
    this.emitMcpStatusChanged();
  }

  async addMcpServer(def: McpServerConfig): Promise<void> {
    this.mcpServers.set(def.id, {
      id: def.id,
      state: def.disabled ? "disabled" : "running",
      toolCount: 0,
      restarts: 0,
    });
    this.emitMcpStatusChanged();
  }

  async removeMcpServer(id: string): Promise<void> {
    if (this.mcpServers.delete(id)) this.emitMcpStatusChanged();
  }

  async respondApproval(
    sessionId: string,
    callId: string,
    approved: boolean,
  ): Promise<void> {
    const key = approvalKey(sessionId, callId);
    const resume = this.pendingApprovals.get(key);
    if (!resume) return;
    this.pendingApprovals.delete(key);
    resume(approved);
  }

  async respondAsk(
    sessionId: string,
    callId: string,
    answer: string,
  ): Promise<void> {
    const key = approvalKey(sessionId, callId);
    const resume = this.pendingAsks.get(key);
    if (!resume) return;
    this.pendingAsks.delete(key);
    resume(answer);
  }

  async setSessionApprove(sessionId: string, tool: string): Promise<void> {
    let set = this.sessionApproved.get(sessionId);
    if (!set) {
      set = new Set<string>();
      this.sessionApproved.set(sessionId, set);
    }
    set.add(tool);
  }

  async setAlwaysApprove(tool: string): Promise<void> {
    this.alwaysApproved.add(tool);
  }

  async removeAlwaysApprove(tool: string): Promise<void> {
    this.alwaysApproved.delete(tool);
  }

  async listAlwaysApproved(): Promise<string[]> {
    return [...this.alwaysApproved].sort();
  }

  private activeConnection(): ProviderConnection {
    const active = this.registry.connections.find(
      (c) => c.id === this.registry.active,
    );
    // Registry invariants guarantee the active pointer resolves.
    if (!active)
      throw new Error(`active connection missing: ${this.registry.active}`);
    return active;
  }

  /** Slug derived from vendor/displayName/kind, deduped with a `-N` suffix. */
  private deriveConnectionId(conn: ProviderConnection): string {
    const base =
      (conn.vendor || conn.displayName || conn.kind)
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/^-+|-+$/g, "") || "connection";
    let id = base;
    let n = 2;
    while (this.registry.connections.some((c) => c.id === id)) {
      id = `${base}-${n++}`;
    }
    return id;
  }

  private cloneRegistry(): ProviderRegistry {
    return {
      schemaVersion: this.registry.schemaVersion,
      active: this.registry.active,
      connections: this.registry.connections.map((c) => ({ ...c })),
    };
  }

  // getProviderConfig/setProviderConfig stay as active-connection shims during
  // the frontend cutover to the registry (Issue #126).
  async getProviderConfig(): Promise<ProviderConfig> {
    const c = this.activeConnection();
    return {
      kind: c.kind,
      baseUrl: c.baseUrl,
      model: c.model,
      hasKey: c.hasKey,
      thinking: c.thinking,
      reasoningEffort: c.reasoningEffort,
      reasoningVisibility: c.reasoningVisibility,
      warmupEnabled: true,
    };
  }

  async setProviderConfig(
    kind: ProviderKind,
    baseUrl: string | undefined,
    model: string,
    thinking: boolean,
  ): Promise<ProviderConfig> {
    const trimmed = baseUrl?.trim();
    const c = this.activeConnection();
    c.kind = kind;
    c.baseUrl = trimmed ? trimmed : undefined;
    c.model = model;
    c.thinking = thinking;
    // Secrets are a later phase; the mock never has a key.
    c.hasKey = false;
    return {
      kind: c.kind,
      baseUrl: c.baseUrl,
      model: c.model,
      hasKey: c.hasKey,
      thinking: c.thinking,
      reasoningEffort: c.reasoningEffort,
      reasoningVisibility: c.reasoningVisibility,
      warmupEnabled: true,
    };
  }

  async getProviderRegistry(): Promise<ProviderRegistry> {
    return this.cloneRegistry();
  }

  async setActiveConnection(id: string): Promise<void> {
    if (!this.registry.connections.some((c) => c.id === id)) {
      throw new Error(`unknown connection: ${id}`);
    }
    this.registry.active = id;
  }

  async upsertConnection(
    conn: ProviderConnection,
  ): Promise<ProviderConnection> {
    const id = conn.id?.trim() ? conn.id.trim() : this.deriveConnectionId(conn);
    const stored: ProviderConnection = { ...conn, id };
    const idx = this.registry.connections.findIndex((c) => c.id === id);
    if (idx >= 0) this.registry.connections[idx] = stored;
    else this.registry.connections.push(stored);
    return { ...stored };
  }

  async removeConnection(id: string): Promise<void> {
    if (this.registry.connections.length <= 1) {
      throw new Error("cannot remove the last connection");
    }
    const idx = this.registry.connections.findIndex((c) => c.id === id);
    if (idx < 0) return;
    this.registry.connections.splice(idx, 1);
    if (this.registry.active === id) {
      this.registry.active = this.registry.connections[0].id;
    }
  }

  async listModels(id?: string): Promise<string[]> {
    const target = id ?? this.registry.active;
    // Hosted OpenAI-compatible kinds (#311/#329) need a stored API key — the real
    // `list_models` probe 401s without a valid bearer, mirroring `testConnection`
    // above. Return empty (not the catalog) until a key is set, so the FE's
    // post-credentials recovery (#486) is exercisable offline.
    const conn = this.registry.connections.find((c) => c.id === target);
    if (conn && (conn.kind === "openai" || conn.kind === "siliconFlow")) {
      if (!this.secretsByConnection.get(conn.id)?.has("apiKey")) return [];
    }
    return this.modelsByConnection[target] ?? [];
  }

  // Provider secrets (Issue #202 PR-3). Write-only: the mock records which kinds
  // are set (to recompute `hasKey`) and drops the value, mirroring the backend's
  // keychain write that never returns the secret over IPC.
  async setProviderSecret(
    connectionId: string,
    kind: SecretKind,
    value: string,
  ): Promise<void> {
    const conn = this.registry.connections.find((c) => c.id === connectionId);
    if (!conn) throw new Error(`unknown connection: ${connectionId}`);
    // No empty-value guard: the `set_provider_secret` contract doesn't forbid an
    // empty string, so the mock must not be stricter than the real backend.
    void value;
    let set = this.secretsByConnection.get(connectionId);
    if (!set) {
      set = new Set<SecretKind>();
      this.secretsByConnection.set(connectionId, set);
    }
    set.add(kind);
    conn.hasKey = set.size > 0;
  }

  async clearProviderSecret(
    connectionId: string,
    kind: SecretKind,
  ): Promise<void> {
    const conn = this.registry.connections.find((c) => c.id === connectionId);
    if (!conn) throw new Error(`unknown connection: ${connectionId}`);
    const set = this.secretsByConnection.get(connectionId);
    set?.delete(kind);
    conn.hasKey = (set?.size ?? 0) > 0;
  }

  // Which secret kinds are stored for a connection (#320), in `SecretKind::ALL`
  // order, so each Bedrock field shows its own Stored/Clear state. Presence only —
  // no value is returned; rejects on an unknown id (mirrors the backend command).
  async providerSecretPresence(connectionId: string): Promise<SecretKind[]> {
    if (!this.registry.connections.some((c) => c.id === connectionId)) {
      throw new Error(`unknown connection: ${connectionId}`);
    }
    const set = this.secretsByConnection.get(connectionId);
    return SECRET_KIND_ALL.filter((k) => set?.has(k));
  }

  // The Bedrock auth a connection resolves to right now (#320): the explicit pin,
  // or the `Auto` precedence winner. `null` for non-Bedrock / unknown connections.
  async resolvedBedrockAuth(connectionId: string): Promise<BedrockAuth | null> {
    const conn = this.registry.connections.find((c) => c.id === connectionId);
    if (!conn || conn.kind !== "bedrock") return null;
    const mode = conn.authMode ?? "auto";
    return mode === "auto" ? this.resolveBedrockAuto(conn) : mode;
  }

  // `BedrockAuth::resolve_auto` mirror: API key > profile > IAM keys, falling back
  // to profile so the probe surfaces the real auth failure. A profile counts only
  // when non-empty; IAM keys only when both the access key id and the secret access
  // key are present.
  private resolveBedrockAuto(conn: ProviderConnection): BedrockAuth {
    const set = this.secretsByConnection.get(conn.id);
    const hasApiKey = set?.has("apiKey") ?? false;
    const hasProfile = !!conn.awsProfile?.trim();
    const hasIamKeys =
      !!conn.accessKeyId?.trim() && (set?.has("secretAccessKey") ?? false);
    if (hasApiKey) return "apiKey";
    if (hasProfile) return "profile";
    if (hasIamKeys) return "iamKeys";
    return "profile";
  }

  // Test Connection (Issue #202 PR-3a). `id` defaults to the active connection.
  // Resolves on a successful probe; rejects with a message the UI surfaces. Local
  // kinds always pass; the hosted OpenAI-compatible kinds (SiliconFlow #329,
  // OpenAI #311) need a stored API key (mirroring the real `list_models` probe,
  // which 401s without a valid bearer key); Bedrock passes only when the active
  // auth mode has the credentials it needs (profile resolves from ~/.aws;
  // IAM/API-Key need a stored secret) — exercising both result paths offline.
  async testConnection(id?: string): Promise<void> {
    const target = id ?? this.registry.active;
    const conn = this.registry.connections.find((c) => c.id === target);
    if (!conn) throw new Error(`unknown connection: ${target}`);
    if (conn.kind === "openai" || conn.kind === "siliconFlow") {
      if (!this.secretsByConnection.get(conn.id)?.has("apiKey")) {
        const name = conn.kind === "openai" ? "OpenAI" : "SiliconFlow";
        throw new Error(`No ${name} API key configured.`);
      }
      return;
    }
    if (conn.kind !== "bedrock") return;
    const secrets = this.secretsByConnection.get(conn.id);
    // `Auto` (the default) resolves to a concrete mode by precedence before the
    // probe validates it, mirroring the backend — so the probe checks the same
    // credential the next run will use.
    const mode =
      !conn.authMode || conn.authMode === "auto"
        ? this.resolveBedrockAuto(conn)
        : conn.authMode;
    switch (mode) {
      case "iamKeys":
        if (!conn.accessKeyId?.trim() || !secrets?.has("secretAccessKey")) {
          throw new Error(
            "IAM keys incomplete — set an access key id and secret access key.",
          );
        }
        return;
      case "apiKey":
        if (!secrets?.has("apiKey")) {
          throw new Error("No Bedrock API key configured.");
        }
        return;
      default:
        // Profile: assume the named/default profile resolves.
        return;
    }
  }

  async getSearchConfig(): Promise<SearchConfig> {
    return { ...this.searchConfig };
  }

  async setSearchConfig(
    backend: SearchBackend,
    baseUrl: string | undefined,
  ): Promise<SearchConfig> {
    const trimmed = baseUrl?.trim();
    this.searchConfig = {
      backend,
      baseUrl: trimmed ? trimmed : undefined,
      // Secrets are a later phase; the mock never has a key.
      hasKey: false,
    };
    return { ...this.searchConfig };
  }

  async getControlConfig(): Promise<ControlConfig> {
    return structuredClone(this.controlConfig);
  }

  async setControlConfig(config: ControlConfig): Promise<ControlConfig> {
    this.controlConfig = structuredClone(config);
    return structuredClone(this.controlConfig);
  }

  async listScheduledTasks(): Promise<ScheduledTask[]> {
    return this.scheduledTasks.map((t) => ({ ...t }));
  }

  async toggleScheduledTask(id: string): Promise<ScheduledTask> {
    const task = this.scheduledTasks.find((t) => t.id === id);
    if (!task) throw new Error(`unknown scheduled task: ${id}`);
    task.paused = !task.paused;
    // A paused task has no next run; resuming schedules it ~a day out.
    task.nextRun = task.paused ? undefined : Date.now() + 24 * HOUR_MS;
    return { ...task };
  }

  async createScheduledTask(
    input: CreateScheduledTaskInput,
  ): Promise<ScheduledTask> {
    const task: ScheduledTask = {
      id: uid(),
      name: input.name,
      kind: input.kind,
      cron: input.cron,
      workspace: input.workspace,
      profile: input.profile,
      safetyCeiling: input.safetyCeiling ?? "read_only",
      // Derived server-side in the real backend; the mock fakes a label.
      cadenceLabel: input.name,
      nextRun: Date.now() + 24 * HOUR_MS,
      paused: false,
      catchUp: input.catchUp ?? false,
    };
    this.scheduledTasks.push(task);
    return { ...task };
  }

  async deleteScheduledTask(id: string): Promise<void> {
    const task = this.scheduledTasks.find((t) => t.id === id);
    if (task && task.kind.kind === "builtin") {
      throw new Error("built-in tasks cannot be deleted");
    }
    this.scheduledTasks = this.scheduledTasks.filter((t) => t.id !== id);
  }

  async previewCadence(cron: string): Promise<string> {
    // The mock cannot parse cron; echo a stable placeholder.
    return `Runs on \`${cron}\``;
  }

  async runScheduledTaskNow(id: string): Promise<RunRecord> {
    const task = this.scheduledTasks.find((t) => t.id === id);
    if (!task) throw new Error(`unknown scheduled task: ${id}`);
    if (this.scheduledPausedAll) {
      throw new Error("scheduled tasks are globally paused");
    }
    // A manual fire spawns a session the run is attached to (the real runner does
    // the same), so the UI's ↗ open-session jump has somewhere to land.
    const session = await this.createSession(`Scheduled: ${task.name}`);
    const firedMs = Date.now();
    // The fire updates the derived stamps: last run is now; an active task's next
    // run rolls forward (a paused task still has none).
    task.lastRun = firedMs;
    task.nextRun = task.paused ? undefined : firedMs + 24 * HOUR_MS;
    const run: RunRecord = {
      id: (this.scheduledRunSeq += 1),
      taskId: id,
      sessionId: session.id,
      firedMs,
      status: "ok",
    };
    this.scheduledRuns.push(run);
    // Fire first (carries the session linkage), then the snapshot that live-updates
    // Next / Last in the list without a reload.
    this.emit(this.scheduledFiredListeners, run);
    this.emit(
      this.scheduledChangedListeners,
      this.scheduledTasks.map((t) => ({ ...t })),
    );
    return run;
  }

  async listScheduledRuns(id: string): Promise<RunRecord[]> {
    // Newest first, capped at 50 — mirrors the backend `runs(id, 50)` reader.
    return this.scheduledRuns
      .filter((r) => r.taskId === id)
      .slice(-50)
      .reverse();
  }

  async setScheduledPausedAll(paused: boolean): Promise<boolean> {
    this.scheduledPausedAll = paused;
    this.emit(
      this.scheduledChangedListeners,
      this.scheduledTasks.map((t) => ({ ...t })),
    );
    return paused;
  }

  async warmup(): Promise<void> {
    // No-op: there is no real server behind the mock.
  }

  // Memory (RFC 0006, M5.1e) — canned read-only store mirroring the Rust layout:
  // a curated MEMORY.md plus daily/<date>.md files, daily newest-first.
  private memoryFiles: Array<MemoryFileInfo & { body: string }> = [
    {
      name: "MEMORY.md",
      relPath: "MEMORY.md",
      kind: "curated",
      sizeBytes: 332,
      modifiedMs: 1_718_000_000_000,
      // Canonical RFC 0008 headings (## Identity / ## Patterns / ## Focus) so the
      // SET.8 category cards render meaningful demo content under VITE_FF_MOCK=1.
      body: `# Memory

## Identity
Abid owns the React frontend; Tony owns the Rust backend. Prefers concise answers and dark mode.

## Patterns
One PR per issue, verified under the in-browser mock before pushing. Reuses existing Tailwind/shadcn primitives.

## Focus
Shipping the Settings redesign — currently the Memory browser (SET.8).
`,
    },
    {
      name: "2026-06-18.md",
      relPath: "daily/2026-06-18.md",
      kind: "daily",
      sizeBytes: 41,
      modifiedMs: 1_718_700_000_000,
      body: "- Shipped the memory IPC contract today.\n",
    },
    {
      name: "2026-06-17.md",
      relPath: "daily/2026-06-17.md",
      kind: "daily",
      sizeBytes: 33,
      modifiedMs: 1_718_600_000_000,
      body: "- Reviewed RFC 0006 memory.\n",
    },
  ];

  async listMemoryFiles(): Promise<MemoryFileInfo[]> {
    return this.memoryFiles.map(({ body: _body, ...info }) => ({ ...info }));
  }

  async readMemoryFile(relPath: string): Promise<string> {
    const f = this.memoryFiles.find((m) => m.relPath === relPath);
    if (!f) throw new Error("invalid memory path");
    return f.body;
  }

  async memoryOverview(): Promise<MemoryOverview> {
    return {
      enabled: true,
      fileCount: this.memoryFiles.length,
      totalBytes: this.memoryFiles.reduce((sum, f) => sum + f.sizeBytes, 0),
      rootPath: "/mock/memory",
    };
  }

  // Salience surface (M6.2, #293). Seeds one live, one dormant, one pinned, and
  // one never-recalled chunk so the Salience UI is exercisable under
  // VITE_FF_MOCK=1 independently of the backend. `dormant`/`weight` are the
  // backend-authoritative values the FE renders verbatim (no threshold here).
  private memoryChunks: MemoryChunkStat[] = [
    {
      chunkKey: "curated:identity#0",
      relPath: "MEMORY.md",
      heading: "Identity",
      preview: "Abid owns the React frontend; Tony owns the Rust backend.",
      weight: 0.92,
      accessCount: 7,
      lastAccessedMs: 1_718_700_000_000,
      dormant: false,
      pinned: false,
    },
    {
      chunkKey: "curated:patterns#0",
      relPath: "MEMORY.md",
      heading: "Patterns",
      preview: "One PR per issue, verified under the in-browser mock.",
      weight: 1.0,
      accessCount: 3,
      lastAccessedMs: 1_718_600_000_000,
      dormant: false,
      pinned: true,
    },
    {
      chunkKey: "curated:focus#0",
      relPath: "MEMORY.md",
      heading: "Focus",
      preview: "Shipping the Settings redesign — the Memory browser.",
      weight: 0.12,
      accessCount: 1,
      lastAccessedMs: 1_715_000_000_000,
      dormant: true,
      pinned: false,
    },
    {
      chunkKey: "daily:2026-06-18#0",
      relPath: "daily/2026-06-18.md",
      heading: null,
      preview: "Shipped the memory IPC contract today.",
      weight: 1.0,
      accessCount: 0,
      lastAccessedMs: null,
      dormant: false,
      pinned: false,
    },
  ];

  async listMemoryChunks(): Promise<MemoryChunkStat[]> {
    return this.memoryChunks.map((c) => ({ ...c }));
  }

  async resetMemoryChunk(chunkKey: string): Promise<void> {
    const c = this.memoryChunks.find((m) => m.chunkKey === chunkKey);
    if (c) {
      c.weight = 1.0;
      c.dormant = false;
      c.lastAccessedMs = Date.now();
    }
  }

  async setMemoryChunkPinned(chunkKey: string, pinned: boolean): Promise<void> {
    const c = this.memoryChunks.find((m) => m.chunkKey === chunkKey);
    if (c) {
      c.pinned = pinned;
      if (pinned) {
        c.weight = 1.0;
        c.dormant = false;
      }
    }
  }

  async listSkills(): Promise<SkillInfo[]> {
    return [...MOCK_SKILLS]
      .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
      .map((s) => ({ ...s, active: this.activeSkills.has(s.name), score: 0 }));
  }

  async searchSkills(query: string): Promise<SkillInfo[]> {
    const q = query.trim().toLowerCase();
    return MOCK_SKILLS.map((s) => ({
      s,
      score: q === "" ? 0 : scoreSkill(s, q),
    }))
      .filter(({ score }) => q === "" || score > 0)
      .sort(
        (a, b) =>
          b.score - a.score ||
          (a.s.name < b.s.name ? -1 : a.s.name > b.s.name ? 1 : 0),
      )
      .map(({ s, score }) => ({
        ...s,
        active: this.activeSkills.has(s.name),
        score,
      }));
  }

  // Marketplace search (SET.5). Empty query lists the full catalog, install-count
  // descending; a query ranks by relevance and drops non-matches.
  async searchSkillMarketplace(query: string): Promise<MarketplaceSkill[]> {
    const q = query.trim().toLowerCase();
    if (q === "") {
      return [...MOCK_MARKETPLACE].sort((a, b) => b.installs - a.installs);
    }
    return MOCK_MARKETPLACE.map((s) => ({ s, score: scoreMarketplace(s, q) }))
      .filter(({ score }) => score > 0)
      .sort((a, b) => b.score - a.score || b.s.installs - a.s.installs)
      .map(({ s }) => ({ ...s }));
  }

  async activateSkill(name: string): Promise<void> {
    if (!MOCK_SKILLS.some((s) => s.name === name)) {
      throw new Error(`unknown skill: ${name}`);
    }
    this.activeSkills.add(name);
    this.emitSkillsChanged();
  }

  async deactivateSkill(name: string): Promise<void> {
    this.activeSkills.delete(name);
    this.emitSkillsChanged();
  }

  // Telemetry (RFC 0001 §8). The mock returns a deterministic canned aggregate for
  // any installed skill (derived from the name so it's stable across calls) and null
  // for an unknown skill — mirroring the backend's `Option<SkillAggregate>`.
  async getSkillTelemetry(skill: string): Promise<SkillAggregate | null> {
    if (!MOCK_SKILLS.some((s) => s.name === skill)) return null;
    const seed = skill.length;
    const completions = seed;
    const successes = Math.max(1, seed - 1);
    return {
      skill,
      activations: seed + 1,
      completions,
      successes,
      meanTokens: seed * 100,
      meanTurns: 2,
      meanLatencyMs: seed * 250,
      successRate: successes / completions,
    };
  }

  // Skill evolution (Issue #29, M3.5). `optimizeSkill` emits a canned proposal and
  // blocks on approval via the *same* `pendingApprovals` map the backend reuses
  // (keyed by `requestId` as both session and call id). Approval bumps the patch
  // version and archives the prior one; decline rejects.
  async optimizeSkill(_sessionId: string, skill: string): Promise<string> {
    const info = MOCK_SKILLS.find((s) => s.name === skill);
    if (!info) throw new Error(`unknown skill: ${skill}`);
    const currentVersion = this.skillVersions.get(skill) ?? info.version;
    const newVersion = bumpPatch(currentVersion);
    const requestId = uid();
    const beforeBody = `# ${skill}\n\n${info.description}\n\nVerbose original body with redundant guidance.`;
    const afterBody = `# ${skill}\n\n${info.description}\n\nStreamlined body.`;
    const approved = await new Promise<boolean>((resolve) => {
      this.pendingApprovals.set(approvalKey(requestId, requestId), resolve);
      this.emit(this.evolveApprovalListeners, {
        requestId,
        skill,
        currentVersion,
        newVersion,
        beforeBody,
        afterBody,
        costEstimate: {
          currentMeanTokens: beforeBody.length / 4,
          estimatedMeanTokens: afterBody.length / 4,
        },
      });
    });
    if (!approved) throw new Error("optimize declined");
    const archived = this.archivedVersions.get(skill) ?? [];
    this.archivedVersions.set(skill, [currentVersion, ...archived]);
    this.skillVersions.set(skill, newVersion);
    this.emitSkillsChanged();
    return newVersion;
  }

  async rollbackSkill(skill: string, version: string): Promise<void> {
    const info = MOCK_SKILLS.find((s) => s.name === skill);
    if (!info) throw new Error(`unknown skill: ${skill}`);
    const archived = this.archivedVersions.get(skill) ?? [];
    if (!archived.includes(version)) {
      throw new Error(`version not found: ${version}`);
    }
    const current = this.skillVersions.get(skill) ?? info.version;
    this.archivedVersions.set(skill, [
      current,
      ...archived.filter((v) => v !== version),
    ]);
    this.skillVersions.set(skill, version);
    this.emitSkillsChanged();
  }

  async listSkillVersions(skill: string): Promise<string[]> {
    return [...(this.archivedVersions.get(skill) ?? [])];
  }

  async listPhenotypes(): Promise<Phenotype[]> {
    return [DEFAULT_PHENOTYPE, ...this.phenotypes].map((p) => ({ ...p }));
  }

  async getPhenotype(): Promise<Phenotype> {
    return this.activePhenotype;
  }

  async switchPhenotype(name: string): Promise<Phenotype> {
    const pheno =
      name === DEFAULT_PHENOTYPE.name
        ? DEFAULT_PHENOTYPE
        : this.phenotypes.find((p) => p.name === name);
    if (!pheno) throw new Error(`unknown phenotype: ${name}`);
    // Replace the active set with the phenotype's installed skills (dropping any
    // that aren't installed), mirroring the backend.
    this.activeSkills = new Set(
      pheno.skills.filter((n) => MOCK_SKILLS.some((s) => s.name === n)),
    );
    this.activePhenotype = pheno;
    this.emitSkillsChanged();
    // Mirror the backend warn (#296) as a UI signal (#301): a phenotype skill may
    // declare an MCP server that isn't available.
    this.emitPhenoMcpUnavailable(pheno.name, this.activeSkills);
    return pheno;
  }

  // Per-session phenotype binding (#246). Mirrors the backend: validate the name
  // (unless clearing), then store it on the session record. Other sessions are
  // untouched. `null` clears the binding so the session inherits the global active
  // phenotype again.
  async setSessionPhenotype(
    sessionId: string,
    name: string | null,
  ): Promise<void> {
    if (name !== null) {
      const pheno =
        name === DEFAULT_PHENOTYPE.name
          ? DEFAULT_PHENOTYPE
          : this.phenotypes.find((p) => p.name === name);
      if (!pheno) throw new Error(`unknown phenotype: ${name}`);
      // Mirror the backend warn (#296/#301) on the per-pane path too, scoped to
      // the bound phenotype's skills (the global active set is left untouched).
      this.emitPhenoMcpUnavailable(
        pheno.name,
        pheno.skills.filter((n) => MOCK_SKILLS.some((s) => s.name === n)),
      );
    }
    const s = this.sessions.get(sessionId);
    if (s) {
      s.phenotype = name ?? undefined;
      s.updatedAt = now();
    }
  }

  // Persist an edited phenotype (RFC 0005 Phase D / #525, #530). Mirrors
  // state.rs::update_phenotype: reject the immutable built-in `default`, validate any
  // `provider` against the live registry, then upsert by name (lossless whole-record
  // write). When the edited phenotype is the active one, re-apply its skills and emit
  // skills:changed so the FE active set updates.
  async updatePhenotype(pheno: Phenotype): Promise<Phenotype> {
    if (pheno.name === DEFAULT_PHENOTYPE.name) {
      throw new Error(`phenotype '${pheno.name}' is immutable`);
    }
    if (pheno.provider !== undefined) {
      const conn = this.registry.connections.find(
        (c) => c.id === pheno.provider,
      );
      if (!conn) throw new Error(`unknown connection: ${pheno.provider}`);
    }
    const saved: Phenotype = { ...pheno };
    const idx = this.phenotypes.findIndex((p) => p.name === saved.name);
    if (idx >= 0) this.phenotypes[idx] = saved;
    else this.phenotypes.push(saved);

    if (saved.name === this.activePhenotype.name) {
      this.activePhenotype = saved;
      this.activeSkills = new Set(
        saved.skills.filter((n) => MOCK_SKILLS.some((s) => s.name === n)),
      );
      this.emitSkillsChanged();
      this.emitPhenoMcpUnavailable(saved.name, this.activeSkills);
    }
    return { ...saved };
  }

  // Per-session model selection (RFC 0005 §11.2, Phase D; #499). Mirrors the
  // backend: validate the connection (unless clearing) then store/drop the
  // per-session override. Other sessions are untouched.
  async setSessionModelSelection(
    sessionId: string,
    selection: ModelSelection | null,
  ): Promise<void> {
    if (selection !== null) {
      const conn = this.registry.connections.find(
        (c) => c.id === selection.connection,
      );
      if (!conn) throw new Error(`unknown connection: ${selection.connection}`);
      this.sessionModelSelections.set(sessionId, { ...selection });
    } else {
      this.sessionModelSelections.delete(sessionId);
    }
  }

  async getSessionModelSelection(
    sessionId: string,
  ): Promise<ModelSelection | null> {
    const sel = this.sessionModelSelections.get(sessionId);
    return sel ? { ...sel } : null;
  }

  // Mirror of the backend capability derivation (RFC 0005 §11.3): attachment caps
  // come from the resolved `(kind, model)`, never a stored connection flag. As of
  // the #338 follow-up, documents are universal — Bedrock carries a native
  // `DocumentBlock`, OpenAI/Ollama extract document text client-side into the
  // prompt — so `supportsDocuments` is true for every known kind. Vision is a
  // deliberately small, mock-faithful subset of the data-driven model-specs
  // (#466) — enough to exercise the composer gate offline (gpt-4o and any
  // `*-vision` model un-gate images), and fail-closed for an unknown kind. The
  // `!kind` early return below is a connection-missing guard (no connection ->
  // no caps at all), distinct from the per-kind vision fail-closed.
  private modelCaps(
    kind: ProviderKind | undefined,
    model: string,
  ): { supportsVision: boolean; supportsDocuments: boolean } {
    if (!kind) return { supportsVision: false, supportsDocuments: false };
    const m = model.toLowerCase();
    const supportsVision =
      m.includes("vision") || (kind === "openai" && m.startsWith("gpt-4o"));
    return { supportsVision, supportsDocuments: true };
  }

  // Authoritative resolver mirror (§11.2), most-specific wins: session override →
  // the session's phenotype (its `provider`/`model`, each inheriting when None) →
  // the global active connection + its model. Caps are derived from the resolved
  // `(kind, model)` and folded into the `ResolvedModel` response (§11.3).
  async resolveModelSelection(sessionId: string): Promise<ResolvedModel> {
    const override = this.sessionModelSelections.get(sessionId);
    let connection: string;
    let model: string;
    if (override) {
      connection = override.connection;
      model = override.model;
    } else {
      const bound = this.sessions.get(sessionId)?.phenotype;
      const pheno =
        (bound === DEFAULT_PHENOTYPE.name
          ? DEFAULT_PHENOTYPE
          : bound
            ? this.phenotypes.find((p) => p.name === bound)
            : undefined) ?? this.activePhenotype;

      const connId = pheno.provider ?? this.registry.active;
      const conn =
        this.registry.connections.find((c) => c.id === connId) ??
        this.activeConnection();
      connection = conn.id;
      model = pheno.model ?? conn.model;
    }

    const kind = this.registry.connections.find(
      (c) => c.id === connection,
    )?.kind;
    // The mock has no live Ollama to probe, so the served-window contract
    // (#602) carries nulls; the model-chip readout / amber dot stays hidden in
    // mock mode by design.
    return {
      connection,
      model,
      ...this.modelCaps(kind, model),
      contextWindow: null,
      trainedContextWindow: null,
      contextWindowSource: null,
    };
  }

  // Profile marketplace search (SET.7). Empty query lists the full catalog,
  // install-count descending; a query ranks by relevance and drops non-matches.
  async searchProfileMarketplace(query: string): Promise<MarketplaceProfile[]> {
    const q = query.trim().toLowerCase();
    if (q === "") {
      return [...MOCK_PROFILE_MARKETPLACE].sort(
        (a, b) => b.installs - a.installs,
      );
    }
    return MOCK_PROFILE_MARKETPLACE.map((p) => ({
      p,
      score: scoreProfile(p, q),
    }))
      .filter(({ score }) => score > 0)
      .sort((a, b) => b.score - a.score || b.p.installs - a.p.installs)
      .map(({ p }) => ({ ...p }));
  }

  // About section (SET.11). Structured stubs — real updater/backup lands later.
  async checkForUpdates(): Promise<UpdateStatus> {
    return { kind: "upToDate", version: APP_VERSION_FALLBACK };
  }

  // The real backend streams `update:progress` per downloaded chunk then relaunches
  // (#362/#363/#566). The mock has no real updater, so it simulates the chunk stream:
  // ~5 cumulative progress ticks up to a fixed total, then `update:download-finished`,
  // so the determinate progress bar is exercisable under VITE_FF_MOCK=1. Resolves once
  // the stream completes (the real path never resolves — it relaunches).
  async installUpdate(): Promise<void> {
    const total = 5 * 1024 * 1024; // 5 MiB
    const chunks = 5;
    const step = total / chunks;
    for (let i = 1; i <= chunks; i++) {
      await new Promise((r) => setTimeout(r, 220));
      this.emit(this.updateProgressListeners, {
        downloaded: BigInt(Math.round(step * i)),
        total: BigInt(total),
      });
    }
    await new Promise((r) => setTimeout(r, 220));
    this.emit(this.updateDownloadFinishedListeners, undefined);
  }

  async exportBackup(): Promise<BackupResult> {
    return { path: "~/Downloads/flowforge-backup.json" };
  }

  async restoreBackup(): Promise<BackupResult> {
    return { path: "~/Downloads/flowforge-backup.json" };
  }

  // --- internals ---

  private append(
    sessionId: string,
    role: Message["role"],
    content: string,
    attachments?: Attachment[],
  ): Message {
    const msg: Message = {
      id: uid(),
      sessionId,
      role,
      content,
      ...(attachments?.length ? { attachments } : {}),
      createdAt: now(),
    };
    const list = this.messages.get(sessionId);
    const isFirstUserMsg =
      role === "user" && !(list ?? []).some((m) => m.role === "user");
    list?.push(msg);
    const s = this.sessions.get(sessionId);
    if (s) {
      s.updatedAt = msg.createdAt;
      // Mirror the backend: first user message seeds an untitled session's title.
      if (isFirstUserMsg && !s.title) s.title = autoTitle(content);
    }
    return msg;
  }

  // Emit a single intermediate-prose assistant message: the narration the real
  // backend persists on each per-iteration assistant message (#415). Appended to
  // history and streamed as one token so the frontend materializes it live.
  private emitProse(sessionId: string, text: string): string {
    const msg = this.append(sessionId, "assistant", text);
    this.emit(this.tokenListeners, {
      sessionId,
      messageId: msg.id,
      delta: text,
    });
    return msg.id;
  }

  private streamAssistant(sessionId: string, secret = false): void {
    const turn: ActiveTurn = {
      timers: [],
      messageId: "",
      pendingToolCalls: [],
    };
    this.activeTimers.set(sessionId, turn);

    // The backend mints one assistant message per tool-calling iteration, each
    // carrying its own prose; we mirror that so the turn exercises the StepGroup
    // fold (#17), cross-iteration step aggregation, and interleaved prose rows
    // (#415) — intermediate narration folded between the steps it describes.

    // Iteration 1: a line of prose, then a planning checklist (todo render, #42)
    // and a couple of auto-resolving reads.
    const a1 = this.emitProse(
      sessionId,
      "I'll read the README and find the FlowForge references, then update the title.",
    );
    turn.messageId = a1;
    this.emitAutoStep(
      sessionId,
      a1,
      "todo",
      {
        items: [
          { content: "Read the README", status: "completed" },
          { content: "Find the FlowForge references", status: "in_progress" },
          { content: "Update the title", status: "pending" },
        ],
      },
      "[x] Read the README\n[~] Find the FlowForge references\n[ ] Update the title",
    );
    this.emitAutoStep(
      sessionId,
      a1,
      "view",
      { path: "README.md" },
      "(mocked) read 42 lines from README.md",
    );
    this.emitAutoStep(
      sessionId,
      a1,
      "grep",
      { pattern: "FlowForge", path: "." },
      "(mocked) 7 matches across 3 files",
    );

    // Iteration 2: more prose, then the interactive `ask_user` step (#44) and the
    // approval-gated write — exercising the tool:call -> ask-request -> respondAsk
    // and the approve round-trips under VITE_FF_MOCK=1.
    const a2 = this.emitProse(
      sessionId,
      "Found 7 references across 3 files. I'll confirm the file, then make the edit.",
    );
    turn.messageId = a2;
    this.emitAskStep(sessionId, a2, turn, secret, () => {
      this.emitApprovalStep(sessionId, a2, turn, () => {
        // Final iteration: the answer lands on its own assistant message, so the
        // intermediate prose above renders as folded rows rather than the answer.
        const a3 = this.append(sessionId, "assistant", "").id;
        turn.messageId = a3;
        this.streamWords(sessionId, turn);
      });
    });
  }

  /** Emit a `tool:call` + `tool:ask-request` and register a `respondAsk` resume
   *  that emits the matching `tool:result` (the answer) and runs `next`. A turn
   *  cancel drops the resume via `cancelTurn`, which fires the standard
   *  cancellation backfill. */
  private emitAskStep(
    sessionId: string,
    messageId: string,
    turn: ActiveTurn,
    secret: boolean,
    next: () => void,
  ): void {
    const callId = uidShort();
    turn.pendingToolCalls.push(callId);
    const args = secret
      ? { question: "Enter your sudo password to continue:", secret: true }
      : { question: "Which file should I update?" };
    this.emit(this.toolCallListeners, {
      sessionId,
      messageId,
      callId,
      tool: "ask_user",
      args,
    });
    this.emit(this.askRequestListeners, {
      sessionId,
      messageId,
      callId,
      question: args.question,
      secret,
    });
    this.pendingAsks.set(approvalKey(sessionId, callId), (answer) => {
      turn.pendingToolCalls = turn.pendingToolCalls.filter(
        (id) => id !== callId,
      );
      this.emit(this.toolResultListeners, {
        sessionId,
        messageId,
        callId,
        success: true,
        // Backend redacts a secret answer at the source (#562), so the result the
        // FE sees is the placeholder — never the typed value.
        result: secret ? SECRET_ANSWER_PLACEHOLDER : answer,
      });
      next();
    });
  }

  /** The original write-with-approval demo step. Extracted so other steps (the
   *  `ask_user` step above) can chain into it. */
  private emitApprovalStep(
    sessionId: string,
    messageId: string,
    turn: ActiveTurn,
    next: () => void = () => this.streamWords(sessionId, turn),
  ): void {
    const callId = uidShort();
    // The demo write step's trust level; gates the short-circuit below exactly
    // as the backend's `allowlist_covers` does (Dangerous is never covered). The
    // `as` keeps the union type so the dangerous guard isn't narrowed away to a
    // tautology — the demo emits "write", but the check models the full contract.
    const safety = "write" as ApprovalSafety;
    turn.pendingToolCalls.push(callId);
    this.emit(this.toolCallListeners, {
      sessionId,
      messageId,
      callId,
      tool: "edit",
      args: { path: "README.md", old_str: "FlowForge", new_str: "FlowForge!" },
    });
    // Backend-owned short-circuit (#229): a tool the user marked "always" or
    // "this session" is approved before any event is emitted — no prompt, no
    // flicker. Mirrors `allowlist_covers`: Dangerous calls are never covered, so
    // they fall through to the prompt even with a grant. Resolve straight to the
    // result otherwise.
    if (
      safety !== "dangerous" &&
      (this.alwaysApproved.has("edit") ||
        this.sessionApproved.get(sessionId)?.has("edit"))
    ) {
      turn.pendingToolCalls = turn.pendingToolCalls.filter(
        (id) => id !== callId,
      );
      this.emit(this.toolResultListeners, {
        sessionId,
        messageId,
        callId,
        success: true,
        result: "(mocked) edited README.md",
      });
      next();
      return;
    }
    this.emit(this.approvalRequestListeners, {
      sessionId,
      messageId,
      callId,
      tool: "edit",
      args: { path: "README.md", old_str: "FlowForge", new_str: "FlowForge!" },
      safety,
    });
    this.pendingApprovals.set(approvalKey(sessionId, callId), (approved) => {
      turn.pendingToolCalls = turn.pendingToolCalls.filter(
        (id) => id !== callId,
      );
      this.emit(this.toolResultListeners, {
        sessionId,
        messageId,
        callId,
        success: approved,
        result: approved
          ? "(mocked) edited README.md"
          : "call to `edit` was not approved",
      });
      next();
    });
  }

  // Emit a read-only tool step that resolves immediately (no approval gate).
  // Used to pad a turn to multiple steps so the StepGroup fold is exercised.
  private emitAutoStep(
    sessionId: string,
    messageId: string,
    tool: string,
    args: unknown,
    result: string,
  ): void {
    const callId = uidShort();
    this.emit(this.toolCallListeners, {
      sessionId,
      messageId,
      callId,
      tool,
      args,
    });
    this.emit(this.toolResultListeners, {
      sessionId,
      messageId,
      callId,
      success: true,
      result,
    });
  }

  private streamWords(sessionId: string, turn: ActiveTurn): void {
    const thinking = this.activeConnection().thinking;
    if (thinking) {
      this.streamReasoning(sessionId, turn, () =>
        this.streamAnswer(sessionId, turn),
      );
    } else {
      this.streamAnswer(sessionId, turn);
    }
  }

  private streamReasoning(
    sessionId: string,
    turn: ActiveTurn,
    next: () => void,
  ): void {
    const words = MOCK_REASONING.split(" ");
    let i = 0;
    const timer = setInterval(() => {
      if (i >= words.length) {
        clearInterval(timer);
        next();
        return;
      }
      const delta = (i === 0 ? "" : " ") + words[i];
      i += 1;
      this.emit(this.reasoningListeners, {
        sessionId,
        messageId: turn.messageId,
        delta,
      });
    }, TOKEN_INTERVAL_MS);
    turn.timers.push(timer);
  }

  private streamAnswer(sessionId: string, turn: ActiveTurn): void {
    const stored = this.messages
      .get(sessionId)
      ?.find((m) => m.id === turn.messageId);
    const words = MOCK_REPLY.split(" ");
    let i = 0;
    const timer = setInterval(() => {
      if (i >= words.length) {
        clearInterval(timer);
        this.activeTimers.delete(sessionId);
        // Simulate a one-time context-pressure memory flush (#283) so the memory
        // browser's provenance surface is exercisable under the mock.
        if (!this.flushedSessions.has(sessionId)) {
          this.flushedSessions.add(sessionId);
          this.emit(this.memoryFlushedListeners, {
            sessionId,
            messageId: turn.messageId,
            writes: 2,
          });
        }
        this.emit(this.doneListeners, {
          sessionId,
          messageId: turn.messageId,
          // Rough estimate (~4 chars/token) — mirrors the real backend supplying
          // a context-usage token count on Done.
          tokenCount: Math.ceil((stored?.content.length ?? 0) / 4),
        });
        return;
      }
      const delta = (i === 0 ? "" : " ") + words[i];
      i += 1;
      if (stored) stored.content += delta;
      this.emit(this.tokenListeners, {
        sessionId,
        messageId: turn.messageId,
        delta,
      });
    }, TOKEN_INTERVAL_MS);
    turn.timers.push(timer);
  }

  private subscribe<T>(
    set: Set<Listener<T>>,
    cb: Listener<T>,
  ): Promise<Unlisten> {
    set.add(cb);
    return Promise.resolve(() => set.delete(cb));
  }

  private emitSkillsChanged(): void {
    this.emit(this.skillsChangedListeners, {
      active: [...this.activeSkills].sort(),
    });
  }

  private emitMcpStatusChanged(): void {
    this.emit(this.mcpStatusListeners, {
      servers: [...this.mcpServers.values()]
        .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
        .map((s) => ({ ...s })),
    });
  }

  // Required-but-unavailable MCP server ids for a set of active skills (#301),
  // mirroring the backend's `unavailable_required_servers`: collect each skill's
  // declared servers, drop those present AND running, name-sort + dedupe.
  private unavailableSkillMcp(skills: Iterable<string>): string[] {
    const required = new Set<string>();
    for (const name of skills) {
      for (const server of SKILL_MCP_REQUIREMENTS[name] ?? []) {
        required.add(server);
      }
    }
    const running = new Set(
      [...this.mcpServers.values()]
        .filter((s) => s.state === "running")
        .map((s) => s.id),
    );
    return [...required].filter((id) => !running.has(id)).sort();
  }

  // Emit `phenotype:mcp-unavailable` only when something is actually unavailable
  // (matches the backend warn, which is per-missing-server). No-op otherwise.
  private emitPhenoMcpUnavailable(
    phenotype: string,
    skills: Iterable<string>,
  ): void {
    const servers = this.unavailableSkillMcp(skills);
    if (servers.length === 0) return;
    this.emit(this.phenoMcpUnavailableListeners, { phenotype, servers });
  }

  // The mock has no filesystem, so it can't observe `.git/HEAD`. Stand in for
  // the backend's debounced `GitHeadWatcher` by synthesizing a branch from the
  // path and emitting `workspace:branch-changed` on the next macrotask — after
  // the store's `set` runs `load` (which resets `gitBranch` to null), so the
  // reactive patch is observable in `pnpm dev:mock`. Dev/mock only; a production
  // build always runs inside Tauri, so MockIpc is dead-code-eliminated there.
  private emitBranchChanged(path: string): void {
    const parts = path.replace(/\/+$/, "").split("/");
    const name = parts[parts.length - 1] || path;
    const branch = `mock-${name}`;
    setTimeout(() => {
      this.emit(this.workspaceBranchListeners, { path, gitBranch: branch });
    }, 0);
  }

  private emit<T>(set: Set<Listener<T>>, payload: T): void {
    set.forEach((cb) => cb(payload));
  }
}
