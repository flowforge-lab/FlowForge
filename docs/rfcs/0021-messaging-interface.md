# 0021 — Messaging Interface: multi-platform transport for headless agent

- **Status:** Proposed
- **Milestone:** 0.3.0
- **Author:** tonytan4ever
- **Depends on:** RFC 0020 (goal mode — progress reporting), RFC 0019 (permission matrix — approval in messaging context)
- **Tracking issue:** #809

## 1. Summary & Goals

Ship a **messaging interface** so a FlowForge agent can converse on external
platforms — same `run_turn` / goal-mode loop, different transport. The agent
lives where you already are: Slack for work, Discord for community, WhatsApp
for mobile, WeCom (企业微信) for China.

Goals:

- **Transport-agnostic core** — a `MessageTransport` trait abstracts over
  platforms; the host loop is written once.
- **Four first-class adapters** — Slack (Socket Mode), Discord (Gateway),
  WhatsApp (Cloud API), WeCom (Event Callback).
- **Session continuity** — platform threads map 1:1 to FlowForge sessions;
  history, goal state, and phenotype persist across messages.
- **Streaming responses** — partial output streams to the user as the agent
  thinks (message edits on Slack/Discord, typing indicator + chunked sends
  on WhatsApp/WeCom).
- **Approval in-band** — `Ask` permission cells surface as interactive buttons;
  the loop blocks until the user responds in-platform.
- **Headless deployment** — runs as `flowforge serve` (no GUI, no Tauri); a
  single process can multiplex all configured transports.

Non-goals:

- **Multi-user shared sessions** — one user per session (v1). Shared channels
  with multiple humans addressing the bot is a follow-up.
- **Voice / audio** — text only for now.
- **Running inside the desktop app** — the desktop already has its GUI; this is
  a separate headless process.
- **End-to-end encryption relay** — messages are processed in plaintext on the
  host machine (same trust model as the desktop app).

## 2. Relationship to existing infrastructure

| Need | Reused seam |
|------|-------------|
| Headless turn execution | `run_turn` (`ff-agent`) — already host-agnostic |
| Session + message persistence | `SessionStore` (`ff-session`) |
| Goal-mode self-continue | `drive_goal` (`ff-agent`) — already decoupled from Tauri |
| Tool execution | `ToolContext` + `ff-tools` registry |
| Permission gating | `PermissionMatrix` + `Approver` trait (`ff-agent`) |
| Provider config | `ProviderRegistry` (`ff-core`) |
| MCP servers | `ff-mcp` supervisor (process-level, shareable) |

The **only Tauri-coupled piece** is `spawn_assistant_turn` in
`apps/desktop/src-tauri/src/lib.rs` (~80 lines gluing `run_turn` + event emission
+ cancel registration). This RFC extracts that into a reusable `TurnRunner` in
`ff-agent` or a new `ff-host` crate.

## 3. The `MessageTransport` trait

```rust
#[async_trait]
pub trait MessageTransport: Send + Sync + '\''static {
    /// Human-readable name ("slack", "discord", "whatsapp", "wecom").
    fn name(&self) -> &str;

    /// Connect to the platform and begin receiving events.
    async fn connect(&self) -> Result<()>;

    /// Yield the next inbound user message (blocks until one arrives).
    async fn recv(&self) -> Result<InboundMessage>;

    /// Begin a streaming response (returns a handle to append chunks).
    async fn begin_response(&self, channel: &ChannelId) -> Result<Box<dyn ResponseStream>>;

    /// Present an approval gate and block until the user decides.
    async fn request_approval(
        &self,
        channel: &ChannelId,
        tool: &str,
        safety: PermissionCell,
        args_summary: &str,
    ) -> Result<GateDecision>;

    /// Send a non-streaming notification (goal progress, error, etc.).
    async fn notify(&self, channel: &ChannelId, msg: &Notification) -> Result<()>;
}

#[async_trait]
pub trait ResponseStream: Send {
    /// Append a text chunk (platform edits the message in-place).
    async fn chunk(&mut self, text: &str) -> Result<()>;
    /// Finalize the response.
    async fn finish(self: Box<Self>) -> Result<()>;
}

pub struct InboundMessage {
    pub channel: ChannelId,
    pub user_id: String,
    pub content: String,
    pub attachments: Vec<Attachment>,
}

/// Opaque platform-specific channel identifier that maps to a session.
#[derive(Clone, Hash, Eq, PartialEq)]
pub struct ChannelId {
    pub transport: String,       // "slack", "discord", etc.
    pub platform_id: String,     // Slack: channel+thread_ts; Discord: thread_id; etc.
}
```

## 4. Session mapping

| Platform | Session boundary | New session trigger |
|----------|-----------------|---------------------|
| Slack | Thread (`channel` + `thread_ts`) | New top-level mention; or `/ff new` |
| Discord | Thread (or DM channel) | New thread; or `/ff new` |
| WhatsApp | Chat (phone number pairing) | `/new` command message |
| WeCom | Chat (userId or groupChat thread) | `/new` or 新对话 |

A `ChannelId -> session_id` mapping is persisted in a lightweight store
(`~/.config/flowforge/transports/channel_map.json`).

## 5. Platform-specific adapter notes

### 5.1 Slack (Socket Mode)

- **Auth:** Bot token (`xoxb-`) + App-level token (`xapp-`) for Socket Mode.
- **No public URL needed** — pure WebSocket.
- **Streaming:** Edit the bot'\''s message every ~500ms with accumulated text.
- **Approval:** Interactive message with Approve / Deny buttons -> `action` callback.
- **Rich output:** Tool calls as attachment blocks; code in ``` blocks.
- **Slash commands:** `/ff new`, `/ff goal "objective"`, `/ff status`, `/ff abort`.

### 5.2 Discord (Gateway)

- **Auth:** Bot token, Gateway Intents (MESSAGE_CONTENT).
- **No public URL needed** — WebSocket gateway.
- **Streaming:** Edit message every ~500ms (same as Slack).
- **Approval:** Button components on the message.
- **Rich output:** Embeds for tool calls; markdown code blocks.
- **Slash commands:** Same surface as Slack.

### 5.3 WhatsApp (Cloud API)

- **Auth:** Meta Business account + permanent system user token.
- **Webhook required** — needs a public HTTPS endpoint (Cloudflare Tunnel / ngrok / VPS).
- **24h session window** — user must message first; then bot can reply freely for 24h.
- **Streaming:** Not native. Strategy: typing indicator -> single final message
  (or split into chunks for long responses).
- **Approval:** Interactive button message (max 3 buttons — fits Approve/Deny/Details).
- **Rich output:** Limited markdown. Long tool output truncated with a "see full
  in app" deep link.
- **Rate limit:** 1000 free conversations/month; then ~$0.005/conversation.

### 5.4 WeCom / 企业微信 (Event Callback)

- **Auth:** CorpID + AgentSecret; self-built app (自建应用).
- **Callback URL** — needs a public HTTPS endpoint for receiving messages.
- **Streaming:** Not native. Same strategy as WhatsApp (typing -> final).
- **Approval:** Card messages (卡片消息) with buttons.
- **Rich output:** Markdown messages; card messages for structured output.
- **Registration:** 1-person enterprise is sufficient (个人注册企业微信).
- **No rate limit** on internal messages.

## 6. Headless host architecture

```
flowforge serve [--config transports.toml]

+--------------------------------------------------+
|  ff-host (new crate, or apps/serve binary)       |
|                                                   |
|  +----------+  +----------+  +----------+        |
|  |  Slack   |  | Discord  |  | WhatsApp | ...    |
|  | adapter  |  | adapter  |  | adapter  |        |
|  +----+-----+  +----+-----+  +----+-----+        |
|       +------- ------+------------- +             |
|                      v                            |
|            +------------------+                   |
|            | Router           |                   |
|            | channel->session |                   |
|            +--------+---------+                   |
|                     v                             |
|  +------------------------------------------+    |
|  |  TurnRunner (extracted from desktop)      |    |
|  |  SessionStore . ToolContext . MCP         |    |
|  |  ProviderRegistry . PermissionMatrix      |    |
|  +------------------------------------------+    |
+--------------------------------------------------+
```

### Config (`~/.config/flowforge/transports.toml`)

```toml
[slack]
enabled = true
bot_token = "xoxb-..."
app_token = "xapp-..."          # Socket Mode
channels = ["C0BAA9WSMGB"]      # allowlist (optional)

[discord]
enabled = true
bot_token = "..."
guild_ids = ["123456"]          # allowlist (optional)

[whatsapp]
enabled = false
phone_number_id = "..."
access_token = "..."
verify_token = "..."            # webhook verification
webhook_port = 8443

[wecom]
enabled = false
corp_id = "..."
agent_id = "..."
secret = "..."
callback_token = "..."
callback_aes_key = "..."
webhook_port = 8444
```

## 7. Approval UX in messaging context

The `Approver` trait already abstracts approval decisions. Today only
`DesktopApprover` exists (Tauri dialog). This RFC adds `MessagingApprover`:

```rust
struct MessagingApprover {
    transport: Arc<dyn MessageTransport>,
    channel: ChannelId,
}

#[async_trait]
impl Approver for MessagingApprover {
    async fn request_approval(
        &self,
        tool: &str,
        safety: PermissionCell,
        args: &Value,
    ) -> GateDecision {
        let summary = summarize_args(tool, args);
        self.transport
            .request_approval(&self.channel, tool, safety, &summary)
            .await
            .unwrap_or(GateDecision::Deny) // network failure = deny
    }
}
```

## 8. Goal-mode integration

When a goal is active on a messaging session:

- Each iteration boundary -> post a short progress line:
  `iter 3/25 — "ran cargo test, 312 passed"`
- Budget exhaustion / completion -> post a summary card.
- **Steer:** any user message while active = steer (same as desktop, RFC 0020 §6).
- **Pause/resume/abort:** slash commands or button on the progress message.

## 9. Phased delivery

### Phase 1 — Core extraction + Slack (~2 weeks)
- Extract `TurnRunner` from desktop host into `ff-agent` (or `ff-host`)
- `MessageTransport` trait + `ResponseStream` + `ChannelId` in `ff-core`
- Channel -> session mapping store
- Slack adapter (Socket Mode, streaming edits, approval buttons)
- `flowforge serve --transport slack` binary entry point
- Basic tests (mock transport, round-trip message -> turn -> response)

### Phase 2 — Discord + goal reporting (~1 week)
- Discord adapter (Gateway, threads, embeds, button approval)
- Goal-mode progress posting (iteration boundaries)
- Multi-transport multiplexing (Slack + Discord in one process)

### Phase 3 — WhatsApp (~1 week)
- WhatsApp adapter (Cloud API, webhook receiver, button approval)
- Chunked response strategy (no native streaming)
- Cloudflare Tunnel integration guide / example

### Phase 4 — WeCom + polish (~1 week)
- WeCom adapter (Event Callback, card messages, button approval)
- `/ff new`, `/ff goal`, `/ff status`, `/ff abort` across all platforms
- Rate limiting, user allowlist, audit log
- Docs: setup guide per platform

## 10. Security considerations

- **Token storage:** Platform tokens stored in FlowForge secret store (same as
  provider API keys).
- **User allowlist:** Only configured user IDs can trigger agent turns (prevents
  abuse in public channels).
- **Permission matrix applies equally** — messaging does not bypass safety tiers.
- **Network failure = deny** — if approval request cannot be delivered, default
  to deny.
- **No secrets in responses** — tool results with `SecretKind` are redacted
  before sending to platform.
- **Webhook verification** — WhatsApp/WeCom callbacks verified with
  platform-provided tokens before processing.

## 11. Open questions

1. **Should `flowforge serve` share the desktop'\''s session store?** (Same
   `~/.flowforge/sessions/` dir, or isolated?) Leaning shared — so you can
   start on Slack, pick up on desktop.
2. **Max response length per platform?** (Slack: 3000 chars/block; Discord:
   2000; WhatsApp: 4096; WeCom: 2048) Strategy: truncate + "continued..." for
   tool output; split agent text into multiple messages if needed.
3. **Multi-user channels?** v1 = single user per session. Future: @-mention
   routing so multiple people can each have their own session in one channel.
4. **Crate layout:** new `ff-transport` crate (trait + adapters) vs adapters as
   feature-gated modules inside `ff-host`? Leaning: `ff-transport` for the trait,
   each adapter in its own crate (`ff-transport-slack`, etc.) to keep deps isolated.
