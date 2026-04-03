# ClawPod

Persistent multi-agent runtime daemon for business operations. Receives messages from Slack, Telegram, and Discord, routes them to AI agent teams, and returns responses — all from a single Rust binary running on a VPS.

## Architecture

```
User (Slack / Telegram / Discord)
  │
  ▼
Channel Connector ──► Incoming Queue
                          │
                     Routing (@agent / @team / binding)
                          │
                    ┌─────┴─────┐
                    ▼           ▼
               Single Agent   Team Chain
                    │      (leader → handoff → fan-out)
                    ▼           │
                  Runner ◄──────┘
               (Claude / Codex / mock / custom)
                    │
                    ▼
              Outgoing Queue ──► Channel Connector ──► User
                    │
              State + Event Log
                    │
              Office API (127.0.0.1:3777)
```

## Quick Start

```bash
cd experiments/clawpod

# Diagnostics
cargo run -p runtime -- doctor

# Terminal 1: start daemon with mock provider (no API keys needed)
cargo run -p runtime -- --config examples/local-smoke.toml daemon

# Terminal 2: enqueue a test message
cargo run -p runtime -- --config examples/local-smoke.toml enqueue \
  --channel local \
  --sender alice \
  --sender-id alice_1 \
  --peer-id alice_1 \
  --message "hello from local smoke"
```

Verify:
- `./.clawpod-local/queue/outgoing/*.json` contains the response
- `http://127.0.0.1:3777/office` shows the dashboard
- `curl http://127.0.0.1:3777/api/tasks` returns run history

## Crate Structure

| Crate | Role |
|-------|------|
| `runtime` | CLI binary entry point (`clawpod`) |
| `queue` | File-based message pipeline: incoming → processing → outgoing → dead_letter |
| `routing` | `@agent` / `@team` / binding rule resolution |
| `runner` | Agent execution engine (Anthropic, OpenAI, mock, custom) |
| `team` | Team chain execution with handoff and fan-out |
| `agent` | Workspace bootstrap and session management |
| `server` | Office HTTP API and dashboard (Axum) |
| `domain` | Core types (ProviderKind, ChatType, RunRequest, etc.) |
| `config` | TOML config parsing |
| `store` | JSON file-based state persistence |
| `session` | Session key building and scoping |
| `observer` | Event logging (JSONL) and health tracking |
| `plugins` | Hook system for message transformation |
| `pairing` | Sender authentication via pairing codes |
| `slack` | Slack Socket Mode connector |
| `telegram` | Telegram long-polling connector |
| `discord` | Discord bot connector |

## Configuration

Default config path: `~/.clawpod/clawpod.toml`

```toml
[daemon]
home_dir = "~/.clawpod"
workspace_dir = "~/.clawpod/workspace"
poll_interval_ms = 1000
max_concurrent_runs = 4

[server]
enabled = true
api_port = 3777
host = "127.0.0.1"          # loopback by default
allow_public_bind = false

[runner]
default_provider = "anthropic"
timeout_sec = 120

[heartbeat]
enabled = false
interval_sec = 3600
sender = "Heartbeat"

[queue]
mode = "collect"
max_retries = 3
dead_letter_enabled = true

[session]
dm_scope = "per-channel-peer"

# Agents
[agents.default]
name = "Default"
provider = "anthropic"
model = "claude-sonnet-4-6"

[agents.reviewer]
name = "Reviewer"
provider = "openai"
model = "gpt-5"

# Teams
default_team = "dev"

[teams.dev]
name = "Development"
leader_agent = "default"
agents = ["default", "reviewer"]

# Channels (tokens via env vars)
[channels.slack]
bot_token_env = "SLACK_BOT_TOKEN"
app_token_env = "SLACK_APP_TOKEN"

[channels.telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"

[channels.discord]
bot_token_env = "DISCORD_BOT_TOKEN"
```

### Environment Variables

| Variable | Purpose |
|----------|---------|
| `ANTHROPIC_API_KEY` | Claude API access |
| `SLACK_BOT_TOKEN` | Slack Bot OAuth token |
| `SLACK_APP_TOKEN` | Slack Socket Mode token |
| `TELEGRAM_BOT_TOKEN` | Telegram Bot API token |
| `DISCORD_BOT_TOKEN` | Discord bot token |

## CLI Commands

```bash
clawpod daemon                  # Run the queue daemon
clawpod status                  # Print runtime status
clawpod health                  # Check health from Office server
clawpod logs [--follow]         # Tail event logs
clawpod office                  # Start Office API only
clawpod enqueue --channel ... --message ...  # Manually enqueue a message
clawpod service install|start|stop|status    # Manage background service
clawpod doctor                  # Run diagnostics
clawpod reset --agent <id>      # Reset agent workspace/session
clawpod agent list|show|add|edit|remove      # Manage agents
clawpod team list|show|add-agent|remove-agent|set-leader
clawpod binding list|show|add|update|remove
clawpod access list|allow-channel|deny-channel|remove-channel
clawpod version                 # Print version
```

Examples:

```bash
clawpod agent add researcher \
  --provider openai \
  --model gpt-5 \
  --browser-profile research \
  --prompt-file prompts/research.md \
  --heartbeat-every 30m \
  --heartbeat-target discord \
  --heartbeat-to 1486343914892820500

clawpod agent edit researcher \
  --system-prompt "Stay concise. Escalate blockers early." \
  --heartbeat-direct-policy block \
  --heartbeat-isolated-session false

clawpod team add-agent dev reviewer
clawpod binding add --agent researcher --channel discord --peer-id 1486343914892820500
clawpod access allow-channel --channel discord 1486343914892820500 --require-mention false
clawpod agent remove reviewer --archive-workspace
```

When you administer a remote system over SSH, point the CLI at the same config file the service uses. For a systemd service running as `clawpod`, that usually means:

```bash
sudo -u clawpod -H clawpod --config /home/clawpod/.clawpod/clawpod.toml agent list
```

`clawpod doctor` and `clawpod status` warn when the current CLI config path differs from the runtime home or detected systemd unit.

## Routing

Direct agent routing:

```
@default Investigate this issue
@reviewer Review this PR
```

Team routing (sends to leader, who can hand off to teammates):

```
@dev Fix this bug
```

Agent handoff within a team chain:

```
[@reviewer: Review this diff]
[@ops: Check deployment impact]
```

Multiple handoffs trigger fan-out (parallel execution).

Binding rules fix routing without prefixes:

```toml
[[bindings]]
agent_id = "ops"

[bindings.match]
channel = "slack"
peer_id = "C0123456789"
```

## Runtime Directories

```
~/.clawpod/
├── clawpod.toml          # Config
├── queue/
│   ├── incoming/         # Messages waiting
│   ├── processing/       # Currently executing
│   ├── outgoing/         # Responses ready
│   └── dead_letter/      # Failed after retries
├── workspace/
│   └── <agent_id>/       # Per-agent working directory
├── state/
│   └── clawpod-state.json
├── logs/
│   └── events.jsonl      # Append-only event stream
├── files/                # Attachment storage
└── plugins/              # Optional transform hooks
```

## Office API

Dashboard: `http://127.0.0.1:3777/office`

Key endpoints:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Health check |
| `/api/runs` | GET | Run history |
| `/api/agents` | GET | Agent definitions |
| `/api/teams` | GET | Team definitions |
| `/api/bindings` | GET | Binding rules |
| `/api/queue/status` | GET | Queue state |
| `/api/settings` | GET/PUT | Runtime config |
| `/api/responses` | GET/POST | Pending responses / manual reply |
| `/api/heartbeat/runs` | GET | Recent heartbeat runs |
| `/api/events/stream` | GET | SSE live event stream |
| `/api/chatroom/:team_id` | GET/POST | Team chatroom |
| `/api/logs/events` | GET | Event log |
| `/api/access/senders` | GET | Pairing / sender approval status |

## Providers

| Provider | CLI | Notes |
|----------|-----|-------|
| `anthropic` | `claude` | `claude --dangerously-skip-permissions ...` |
| `openai` | `codex` | `codex exec ...` |
| `mock` | — | Local testing, no API keys |
| `custom` | configurable | Via `custom_providers.<id>` section |

## Deployment

Target: single VPS (Hetzner cpx22 recommended).

```bash
cargo build --release -p runtime
# Binary: target/release/runtime → /usr/local/bin/clawpod
```

- Runs as systemd service with `Restart=on-failure`
- Office stays on `127.0.0.1`, accessed via Tailscale Serve or SSH tunnel
- Secrets in systemd `EnvironmentFile` (`~/.clawpod/env`)
- Firewall: UFW with SSH only

## Design Principles

1. **Runtime First** — Single daemon; agents execute per-message, not as persistent processes
2. **Private By Default** — Office/API binds to loopback; public exposure requires explicit opt-in
3. **Single Node First** — Queue, state, workspace all consistent on one machine
4. **Harness Over Orchestration** — Agent quality comes from workspace structure, not complex scheduling
5. **Observable Operations** — Status, logs, health, event streams are first-class

## License

MIT
