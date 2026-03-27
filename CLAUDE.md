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

Pushing to `main` triggers GitHub Actions (`.github/workflows/deploy.yml`):
1. `cargo test`
2. `cargo build --release -p runtime`
3. Transfer binary to server via SCP
4. Restart systemd service

Server credentials are stored in GitHub Secrets (`HETZNER_SSH_KEY`, `DEPLOY_HOST`).

For initial server setup, use the `hetzner-deploy` skill.

## Office UI

Single-page app embedded in `crates/server/src/office.html` (included via `include_str!`).

API endpoints for workspace files:
- `GET /api/agents/:id/files` — list editable files
- `GET /api/agents/:id/files/:name` — read file content
- `PUT /api/agents/:id/files/:name` — save file content
