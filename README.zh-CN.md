<p align="center">
  <img src="docs/assets/banner.png" width="600" alt="FlowForge">
</p>

<p align="center">
  开源、本地优先、键盘驱动的 AI 编程界面。
</p>

<p align="center">
  <a href="README.md">English</a> | 中文
</p>

---

## 产品哲学

FlowForge 利用 AI 的**均匀 capability**，服务人类的**非均匀 agency**——同时抵抗 AI
训练里被塞进来的 **uniform politeness** 偏见。三个动词，三层：

- **利用** *结构层*——LLM 均匀、不知疲倦的 attention，作为可锻造的原材料。
- **服务** *用户层*——人的注意力、意图与意志是尖锐、个人化、非均匀的；一切为个体的 agency 让路，绝不把它抹平。
- **抵抗** *训练层*——RLHF 把模型推向千篇一律的顺从与谄媚。我们反其道而行：一个诚实的伙伴，胜过一个礼貌的伙伴。

这套哲学如何落成我们据以构建的「四根支柱」，见 [`PRINCIPLES.md`](PRINCIPLES.md)。

---

## 功能特性

- **多模型支持** — OpenAI 兼容（Ollama、LM Studio、SiliconFlow、OpenRouter）、Anthropic（原生 Messages API）、AWS Bedrock（Converse API）
- **智能体循环** — 研究 → 规划 → 实现 → 验证，流式工具调用 + 交互式审批
- **工具系统** — bash、edit、view、grep、glob、web_fetch、web_search、python、apply_patch 等
- **MCP 宿主** — 连接外部工具服务器（stdio/SSE），健康监控 + 自动重启
- **记忆系统** — Markdown 持久化 + SQLite FTS5 检索 + 可选本地向量嵌入（BM25 + 向量混合）
- **技能与表型** — 热重载的 YAML 清单技能，可组合的智能体人格
- **定时任务** — cron 风格自动化，可配置审批上限
- **多窗格会话** — 分屏编辑器，每个窗格独立绑定工作区和模型
- **命令行** — 无头脚本，CI 友好的退出码，相同的智能体循环
- **Plan / Auto / Act 模式** — 从只读分析到完全自主，自由调节智能体权限

## 技术栈

| 层级 | 技术 |
|------|------|
| 外壳 | Tauri 2（Rust 后端 + 系统 WebView） |
| 前端 | React 19 + TypeScript + Vite |
| 状态 | Zustand |
| 存储 | SQLite（会话、记忆索引、定时任务、刷新账本） |
| 样式 | Tailwind CSS + shadcn/ui |
| AI | 多模型：OpenAI 兼容、Anthropic、Bedrock、Ollama（原生） |

## 架构

```
┌─────────────────────────────────────────────────────────────┐
│                      FlowForge Desktop                       │
│                        (Tauri 2)                             │
├─────────────────────────────────────────────────────────────┤
│  ┌───────────────────────────────────────────────────────┐  │
│  │              React 前端 (WebView)                      │  │
│  │                                                       │  │
│  │  ┌─────────────┐  ┌──────────────┐  ┌────────────┐  │  │
│  │  │  对话视图   │  │   分屏窗格   │  │  设置面板  │  │  │
│  │  │  (流式)     │  │   (#148)     │  │            │  │  │
│  │  └─────────────┘  └──────────────┘  └────────────┘  │  │
│  │                                                       │  │
│  │  ┌─────────────────────────────────────────────────┐  │  │
│  │  │                 Zustand Store                    │  │  │
│  │  └─────────────────────────────────────────────────┘  │  │
│  └───────────────────────┬───────────────────────────────┘  │
│                          │ Tauri IPC (invoke / events)       │
├──────────────────────────┼──────────────────────────────────┤
│  ┌───────────────────────┴───────────────────────────────┐  │
│  │                   Rust 后端                            │  │
│  │                                                       │  │
│  │  ┌───────────┐  ┌───────────┐  ┌──────────────────┐  │  │
│  │  │ ff-agent  │  │  ff-llm   │  │    ff-memory     │  │  │
│  │  │ (循环 +   │  │ (OpenAI,  │  │ (Markdown +      │  │  │
│  │  │  工具)    │  │  Bedrock, │  │  FTS5 + 嵌入)    │  │  │
│  │  │           │  │  Anthropic,│  │                  │  │  │
│  │  │           │  │  Ollama)  │  │                  │  │  │
│  │  └───────────┘  └───────────┘  └──────────────────┘  │  │
│  │                                                       │  │
│  │  ┌───────────┐  ┌───────────┐  ┌──────────────────┐  │  │
│  │  │ff-session │  │  ff-mcp   │  │   ff-skills      │  │  │
│  │  │(SQLite    │  │ (MCP 宿主 │  │ (发现、热重载、  │  │  │
│  │  │ 存储)     │  │  + 监控)  │  │  表型解析)       │  │  │
│  │  └───────────┘  └───────────┘  └──────────────────┘  │  │
│  │                                                       │  │
│  │  ┌───────────┐  ┌───────────┐  ┌──────────────────┐  │  │
│  │  │ff-signals │  │ff-scheduled│ │   ff-tools       │  │  │
│  │  │(遥测 +   │  │ (cron     │  │ (bash, edit,     │  │  │
│  │  │ 信号)     │  │  运行器)  │  │  view, web, ...) │  │  │
│  │  └───────────┘  └───────────┘  └──────────────────┘  │  │
│  └───────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────┤
│                      本地存储层                               │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │   SQLite DB  │  │  ~/.flowforge│  │  技能 (MD +      │  │
│  │ (会话、定时) │  │  /memory/    │  │  YAML 清单)      │  │
│  │              │  │  (平面文件)  │  │                  │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
└─────────────────────────────────────────────────────────────┘
         │                    │
         ▼                    ▼
┌─────────────────┐  ┌────────────────┐
│  LLM 提供商     │  │  MCP 服务器    │
│  (OpenAI 兼容,  │  │  (stdio/SSE,   │
│   Bedrock,      │  │   外部工具)    │
│   Anthropic,    │  │                │
│   Ollama)       │  │                │
└─────────────────┘  └────────────────┘
```

### Crate 一览

| Crate | 职责 |
|-------|------|
| `ff-core` | 领域类型 — Message, Turn, Skill, Profile, ProviderConnection |
| `ff-agent` | 智能体循环（工具调度、压缩、审批门控） |
| `ff-llm` | Provider trait + 实现（OpenAI 兼容、Anthropic、Bedrock、Ollama） |
| `ff-mcp` | MCP 客户端 & 监管器 — 健康监控、自动重启、环境隔离 |
| `ff-memory` | Markdown 持久化记忆 + SQLite FTS5 检索 + 可选嵌入（[RFC 0006](docs/rfcs/0006-memory.md)） |
| `ff-session` | 会话持久化（SQLite 存储，对话记录 CRUD） |
| `ff-signals` | 技能遥测聚合（激活数、成本、延迟、成功率）+ 为未来 NeuroForge 集成准备的信号总线 |
| `ff-skills` | 技能发现、YAML 清单解析、表型解析、热重载 |
| `ff-scheduled` | Cron 风格任务运行器，可配置审批上限 |
| `ff-tools` | 内置工具：bash、edit、view、grep、glob、web_fetch、web_search、python、apply_patch |
| `ff-workflow` | 多智能体编排 *（计划中 — M7）* |

## 开发

```bash
# 前置条件：Rust 1.80+、Node 20+、pnpm 9+
git clone https://github.com/flowforge-lab/FlowForge.git
cd FlowForge

# 安装前端依赖
pnpm install

# 开发模式运行（Tauri 热重载）
cargo tauri dev

# 仅前端：在浏览器内运行 mock 后端
# （不需要 Rust 编译或 LLM，适合纯 UI/样式开发）
pnpm --dir apps/desktop dev:mock

# 生产构建
cargo tauri build
```

## 命令行

FlowForge 附带 CLI 二进制文件（`flowforge`），用于脚本、CI 和无头场景 — 相同的智能体循环，相同的工具，无 GUI。

```bash
# 单次运行：执行一轮并输出结果
flowforge run "总结 src/ 的内容"

# 非交互：自动批准写入，JSON 事件流输出到 stdout
flowforge run --json --yes "为新的 memory crate 添加 README 章节"

# 只读分析：拒绝所有写入（CI 的安全默认）
flowforge run --deny "审计 src/ 中未使用的依赖"

# 交互式 REPL（无子命令时默认）
flowforge
```

### 退出码

- **0** — 轮次成功完成（或 REPL 正常退出）。
- **非零** — 智能体错误，或必需的工具审批被拒绝。

当 stdin 非终端且未提供 `--yes` / `--deny` 标志时，所有写入/危险工具调用**默认拒绝** — 使 `--deny` 成为 CI 的安全默认，`--yes` 为自主运行的显式授权。

## 路线图

- [x] **M1** — Tauri 2 外壳 + React 对话 UI + 首次 LLM 调用
- [x] **M2** — 工具调用（bash、view、edit）+ 流式渲染 + 交互式审批
- [x] **M3** — 技能 + 表型 + 命令面板
- [x] **M4** — MCP 宿主 & 监管器 — 外部工具服务器、生命周期 UI
- [x] **M5** — 记忆系统 — Markdown + FTS5 检索 + 可选嵌入（[RFC 0006](docs/rfcs/0006-memory.md)）
- 🚧 **M6** — 冷启动优化（目标 <200ms）
- 🔮 **M7** — 工作流画布（可视化多智能体编排）

### 0.2.0（下一版本）

- 权限矩阵重构 — `Safety::Sensitive` 层级 + 可编辑控制面板（[RFC 0019](docs/rfcs/0019-permission-matrix-and-sensitive-tier.md)，[#682](https://github.com/flowforge-lab/FlowForge/issues/682)）
- 目标模式 — 持久化自主目标循环（[#683](https://github.com/flowforge-lab/FlowForge/issues/683)）
- 自研工具链 — FlowForge 开发 FlowForge（[#684](https://github.com/flowforge-lab/FlowForge/issues/684)）

## NeuroForge（计划中）

FlowForge 与 NeuroForge 是独立但互补的系统。NeuroForge 是计划中的认知健康层，将消费 FlowForge 的意图/结果信号，建模专注状态、奖励预测和自适应节奏 — 灵感来自神经科学中关于心流、前扣带回（aMCC）激活和多巴胺驱动学习的研究。

FlowForge 完全独立可用。NeuroForge 集成将为选择加入的用户解锁闭环认知反馈系统。

| 项目 | 定位 | 状态 |
|------|------|------|
| **FlowForge**（本仓库） | 开源 AI 编程界面 — 本地优先、键盘驱动 | 活跃开发 |
| **NeuroForge** | 认知健康插件 — RPE 模型、心流评分、自适应节奏 | 计划中 |
| **NeuroForge Cloud** | 跨设备同步、团队功能、托管推理 | 未来 |

## 许可

[MIT](./LICENSE)
