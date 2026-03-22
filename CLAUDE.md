# ClawPod

Multi-agent runtime daemon with Slack/Discord/Telegram integration and web-based Office UI.

## Architecture

Rust workspace with crates:

- `runtime` — binary entry point (daemon CLI)
- `server` — Office web UI + REST API (axum)
- `queue` — message queue processor
- `agent` — workspace management + system prompt builder
- `runner` — LLM provider invocation (claude CLI, codex CLI)
- `domain` — shared types (AgentConfig, TeamConfig, etc.)
- `config` — TOML config loading
- `store` — state persistence
- `routing` — message routing / agent resolution
- `session` — session key building
- `plugins` — hook/event dispatch
- `observer` — event sink + health tracking
- `pairing` — sender approval / pairing codes
- `slack` / `discord` / `telegram` — channel adapters

## System Prompt

Built by `SystemPromptBuilder` (zeroclaw-style trait + builder pattern) in `crates/agent/src/prompt.rs`:

- `InstructionsSection` — ClawPod runtime instructions
- `TeammatesSection` — agent roster
- `IdentitySection` — injects `.clawpod/SOUL.md` + `AGENTS.md` from workspace
- `UserPromptSection` — user's custom system_prompt from config

Workspace files are read from disk on every request (no restart needed for changes).

## Build & Test

```sh
cargo check
cargo test -p agent        # prompt builder tests (16 tests)
cargo test                 # full workspace
cargo build --release -p runtime
```

## Deploy

Hetzner server: `REDACTED` (clawpod)
Tailscale: `https://clawpod.taila1d3cf.ts.net/office`

**After pushing changes to main, always deploy to Hetzner:**

```sh
ssh root@REDACTED "source /root/.cargo/env && \
  cd /opt/clawpod-src && git pull && \
  cargo build --release -p runtime && \
  systemctl stop clawpod && \
  sleep 2 && \
  pkill clawpod 2>/dev/null; sleep 1 && \
  rm -f /usr/local/bin/clawpod && \
  cp /opt/clawpod-src/target/release/clawpod /usr/local/bin/clawpod && \
  chmod +x /usr/local/bin/clawpod && \
  systemctl start clawpod && \
  sleep 2 && \
  systemctl status clawpod --no-pager"
```

Or use the `hetzner-deploy` skill.

## Office UI

Single-page app embedded in `crates/server/src/office.html` (included via `include_str!`).

API endpoints for workspace files:
- `GET /api/agents/:id/files` — list editable files
- `GET /api/agents/:id/files/:name` — read file content
- `PUT /api/agents/:id/files/:name` — save file content
