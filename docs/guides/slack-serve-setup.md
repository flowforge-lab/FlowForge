# Serving a Slack channel with `flowforge serve`

`flowforge serve` routes messages from one Slack channel into agent turns and
asks for approval in-channel via Block Kit buttons (RFC 0021).

This guide is the setup path for a first run. It was written by reading the
transport, so every claim below is traceable to source — but the end-to-end run
against a real Slack workspace is the thing it cannot verify for you.

## Socket Mode, not a callback URL

There is no Request URL to configure and no public endpoint to expose.
`SlackTransport::connect` POSTs to `apps.connections.open` with an app-level
token, receives a single-use `wss://` URL, and opens an **outbound** WebSocket
that Slack pushes events down.

Practically: this runs from a laptop behind NAT with no tunnel, no ngrok, and no
inbound firewall rule. The tradeoff is that it is tokens-only — there is no
HTTP-callback mode in this implementation.

You need **two** tokens, because they authenticate different things:

| Token | Prefix | Authenticates | Where to get it |
| --- | --- | --- | --- |
| App-level | `xapp-` | `apps.connections.open` (opening the socket) | Basic Information → App-Level Tokens |
| Bot | `xoxb-` | `chat.postMessage`, `chat.update` (replies, approval prompts) | OAuth & Permissions (after install) |

## 1. Create the app

api.slack.com/apps → **Create New App** → **From scratch**. Name it, pick your
workspace.

## 2. Enable Socket Mode

**Socket Mode** → toggle **Enable Socket Mode**. Slack prompts you to create an
app-level token; give it the `connections:write` scope.

Copy the `xapp-…` value now — Slack shows it once.

## 3. Add bot scopes

**OAuth & Permissions** → **Bot Token Scopes**. The transport only ever calls
`chat.postMessage` and `chat.update`, so it needs:

- `chat:write` — post replies and approval prompts, and edit them in place
- `channels:history` — receive messages in a **public** channel
  (use `groups:history` instead for a **private** channel)

## 4. Subscribe to message events

**Event Subscriptions** → toggle on. With Socket Mode enabled it will *not* ask
for a Request URL.

Under **Subscribe to bot events**, add:

- `message.channels` — for a public channel
- `message.groups` — for a private channel

`parse_envelope` accepts only `events_api` and `interactive` frames. Within
`events_api` it requires `event.type == "message"` and drops anything carrying a
`bot_id` or a `subtype` (edits, joins, channel-topic changes). So:

- **`app_mention` alone will not work** — the bot needs plain channel messages.
- Editing a message will not re-trigger the agent; it arrives as
  `message.message_changed` and is dropped.

## 5. Enable interactivity (for approval buttons)

**Interactivity & Shortcuts** → toggle on. Under Socket Mode there is again no
URL to supply. Without this, approval prompts render but clicks never arrive, and
every `Ask`-tier tool call will hang until it times out (default 10 minutes).

## 6. Install and invite

**Install App** → install to the workspace → copy the `xoxb-…`.

Then, in Slack, invite the bot to the channel:

```
/invite @your-app-name
```

Skipping this yields `not_in_channel` on the first reply.

## 7. Collect the IDs

- **Channel ID** — right-click the channel → *Copy link*; the ID is the trailing
  `C…` segment.
- **Your user ID** — profile → ⋮ → *Copy member ID*; a `U…` value.

## 8. Write the config

`serve` reads `<config-dir>/flowforge/transports.toml`, where `<config-dir>` is
`dirs::config_dir()`:

| OS | Path |
| --- | --- |
| macOS | `~/Library/Application Support/flowforge/transports.toml` |
| Linux | `~/.config/flowforge/transports.toml` |
| Windows | `%APPDATA%\flowforge\transports.toml` |

```toml
[slack]
app_token = "xapp-1-…"
bot_token = "xoxb-…"
allowed_users = ["U_YOUR_USER_ID"]
```

The file holds two live credentials in plaintext — `chmod 600` it, and keep it
out of any repo.

### Precedence and strictness

- **Environment variables win over the file.** `SLACK_APP_TOKEN` and
  `SLACK_BOT_TOKEN` are consulted first, falling back to `[slack]`. A stale
  exported token silently shadows a correct file, which is worth checking first
  when a good-looking config still fails to authenticate.
- **`allowed_users` is the reverse:** the `--allow-user` flag wins, and the file
  is the fallback.
- The `[slack]` table is parsed with `deny_unknown_fields`, so a mistyped key is
  a hard error rather than a silently ignored line.

## 9. Run

```
cargo run -p ff-cli --bin flowforge -- serve --channel C0123456789
```

`--channel` is the only required flag; `--mode` defaults to `auto`.

If `allowed_users` is empty in **both** the file and `--allow-user`, `serve`
refuses to start. That is deliberate: an empty allowlist would otherwise boot a
bot that acks every message and answers nobody.

Post a message in the channel. `Ctrl-C` shuts down cleanly — in-flight messages
already accepted are drained before exit rather than dropped.

## Security notes

`allowed_users` is the only thing between a Slack message and an agent turn
running in your workspace. Non-allowlisted senders are still *acked* — Slack
redelivers unacked envelopes, so refusing to ack would make the message return
rather than go away — but the message is dropped before it can become a turn, and
the rejection is logged at `warn!`.

Two consequences worth internalising before pointing this at a shared channel:

- **A channel button is a shared authorization surface.** Anyone who can see the
  message can click it. Approvals are therefore not equivalent to a local
  approval prompt, which only you can answer.
- **Buttons can never authorize `Publish` or `Dangerous` tiers**, regardless of
  what the permission matrix says for the current mode. `serve` overrides the
  matrix here specifically because `Act`/`Publish` is otherwise `Allow` — this
  check is the only thing standing between a channel button and a remote publish.

Start with a private channel you control and your own user ID alone in the
allowlist.

## Troubleshooting

Turn logging on first. `serve` writes to `<config dir>/flowforge/logs/` (macOS:
`~/Library/Application Support/flowforge/logs/`, next to `sessions.db`), and
`FF_LOG_STDERR=1` mirrors it to the terminal:

```bash
FF_LOG=info,ff_transport_slack=debug FF_LOG_STDERR=1 \
  flowforge serve --channel C0123456789
```

The filter variable is **`FF_LOG`**, not `RUST_LOG`. A healthy start logs
`router started`; Ctrl-C logs `router stopped`. If you see neither, you are
running a build from before #1060 — the CLI installed no subscriber then, and
every log line was silently discarded.

| Symptom | Likely cause |
| --- | --- |
| `invalid_auth` on startup | App token is not `xapp-`, lacks `connections:write`, or an exported `SLACK_APP_TOKEN` is shadowing the file |
| Starts, but messages do nothing | **Most likely: Event Subscriptions has no bot events.** Enabling Socket Mode does *not* subscribe you — add `message.channels` (public) / `message.groups` (private) under *Subscribe to bot events*, then reinstall. Note the `channels:history` scope appearing in your token's scope list does **not** mean the subscription exists; they are separate settings. Otherwise: bot not invited, or sender not in `allowed_users` (logged at `warn!`) |
| `not_in_channel` on reply | `/invite @your-app` not done |
| Replies work, approvals hang | Interactivity not enabled |
| Refuses to start, complains about the allowlist | `allowed_users` empty in both file and flag — this is the intended fail-closed behaviour |

To confirm Slack is delivering events at all, independently of `serve`: a
message posted while nothing is connected produces no event, and Slack delivers
a given event to only **one** open Socket Mode connection — so stop `serve`
before testing with a separate client, or the two will compete for frames.
