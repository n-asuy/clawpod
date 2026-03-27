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

Local deploy via SSH (server builds from source). Requires `DEPLOY_HOST` env var:

```sh
export DEPLOY_HOST=<server-ip>        # or set in .env / shell profile
./scripts/deploy.sh                   # deploy main
./scripts/deploy.sh feat/xxx          # deploy a specific branch
```

Optional env vars: `DEPLOY_USER` (default: root), `DEPLOY_SRC_DIR` (default: /opt/clawpod-src).

For initial server setup, use the `hetzner-deploy` skill.

## Office UI

Single-page app embedded in `crates/server/src/office.html` (included via `include_str!`).

API endpoints for workspace files:
- `GET /api/agents/:id/files` — list editable files
- `GET /api/agents/:id/files/:name` — read file content
- `PUT /api/agents/:id/files/:name` — save file content
