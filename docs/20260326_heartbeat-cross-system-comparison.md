# Heartbeat Design: Cross-System Comparison

Status: reference

## Purpose

Document how periodic heartbeat functionality is designed across the Claw family
of agent runtimes, clarifying where ClawPod aligns with or diverges from each
system's approach.

## Systems Compared

| System | Language | Config Format | Primary Use Case |
|--------|----------|---------------|------------------|
| OpenClaw | TypeScript | JSON5 | Multi-channel comms agent |
| ZeroClaw | Rust + Python | TOML | Single-agent task runner |
| TinyClaw | TypeScript + Shell | JSON | Lightweight prototype |
| Clawith | Python (FastAPI) | Database (SQLAlchemy) | Multi-agent with shared plaza |
| ClawPod | Rust | TOML | Multi-agent daemon with channel adapters |

## Per-Agent Heartbeat Configuration

### Supported

**OpenClaw**, **ClawPod**, **TinyClaw**, and **Clawith** all support per-agent
heartbeat configuration. Each agent can define its own interval, prompt, delivery
target, and behavioral flags independently.

### Not Supported

**ZeroClaw** uses a single global `[heartbeat]` block. All agents share the same
interval, prompt, and delivery target. There is no per-agent override mechanism.

### Resolution Precedence

OpenClaw and ClawPod share the same three-tier precedence model:

1. `agents.<id>.heartbeat` (highest)
2. `agent_defaults.heartbeat`
3. Hardcoded defaults (lowest)

If any agent declares an explicit heartbeat config, only agents with explicit
config participate in scheduled heartbeats.

TinyClaw checks per-agent enable/interval, falling back to global
`monitoring.heartbeat_interval`. Clawith stores config per-agent in the database
with column-level defaults.

## Heartbeat Prompt

### Custom Prompt Sources

| System | Config Field | File-Based | Default Prompt |
|--------|-------------|------------|----------------|
| OpenClaw | `heartbeat.prompt` | `HEARTBEAT.md` in workspace | "Read HEARTBEAT.md if it exists..." |
| ClawPod | `heartbeat.prompt` | `heartbeat.md` in agent root | "Review your current context..." |
| ZeroClaw | `heartbeat.message` | `HEARTBEAT.md` in workspace | None (file required or message fallback) |
| TinyClaw | — | `heartbeat.md` per agent | "Quick status check..." |
| Clawith | — | `HEARTBEAT.md` per agent | Multi-phase exploration prompt |

OpenClaw and ClawPod support both a config-level `prompt` field and a workspace
file. The config field takes precedence; the file provides workspace-local
context.

ZeroClaw's `heartbeat.md` file supports YAML frontmatter for `target` and `to`
overrides, which ClawPod also adopted.

### Effectively Empty Detection

OpenClaw and ClawPod both skip scheduled heartbeats when the file contains only
whitespace, HTML comments, markdown headers, or empty checkboxes. This prevents
wasted LLM calls when no heartbeat work is defined.

ZeroClaw does not perform this check. TinyClaw uses a simple hardcoded fallback.

## Interval Configuration

| System | Format | Default | Minimum |
|--------|--------|---------|---------|
| OpenClaw | Duration string (`"30m"`, `"1h"`) | 30m | — |
| ClawPod | Duration string (`"30m"`, `"1h"`) | 30m | 10s |
| ZeroClaw | Integer minutes | 5m | — |
| TinyClaw | Integer seconds | Configurable | 10s |
| Clawith | Integer minutes | 240m (4h) | — |

OpenClaw and ClawPod use human-readable duration strings. ZeroClaw and Clawith
use integer minutes. TinyClaw uses integer seconds.

## Delivery Targets

### Target Types

| Target | OpenClaw | ClawPod | ZeroClaw | TinyClaw | Clawith |
|--------|----------|---------|----------|----------|---------|
| `none` (run, don't send) | Yes | Yes | — | — | — |
| `last` (last external contact) | Yes | Yes | — | — | — |
| Specific channel (`slack`, `discord`, `telegram`, etc.) | Yes | Yes | Single channel | API pseudo-channel | — |
| `chatroom` (internal team chat) | — | Yes | — | — | Plaza |
| `to` (explicit recipient) | Yes | Yes | Yes | — | — |
| `account_id` (multi-account) | Yes | Yes | — | — | — |

### Direct Message Policy

OpenClaw and ClawPod support `direct_policy` (`allow` | `block`) to control
whether heartbeat output can be delivered as a DM. This is absent from the other
systems.

### Channel Visibility Controls

OpenClaw and ClawPod define per-channel visibility flags:

- `show_ok` — deliver OK-only acks to this channel
- `show_alerts` — deliver non-OK alerts to this channel
- `use_indicator` — emit status indicator events

These flags are configured under `channels.defaults.heartbeat` and
`channels.<name>.heartbeat`, and are evaluated independently of delivery
targeting.

ZeroClaw, TinyClaw, and Clawith have no equivalent.

## HEARTBEAT_OK Suppression

### Token-Based ACK

OpenClaw, ClawPod, and Clawith recognize the `HEARTBEAT_OK` token in the LLM
response:

- **OpenClaw / ClawPod**: Strip `HEARTBEAT_OK` when it appears at the start or
  end of the response. If the remaining text is `<= ack_max_chars` (default:
  300), suppress delivery entirely. `HEARTBEAT_OK` in the middle of a response
  is treated as normal text.
- **Clawith**: Detect `HEARTBEAT_OK` (case-insensitive, whitespace-normalized)
  and skip activity logging. No character threshold.

### Two-Phase Mode (ZeroClaw Only)

ZeroClaw takes a different approach: instead of running the full heartbeat and
then suppressing the output, it asks the LLM in a cheap first phase whether work
exists. The full heartbeat only runs if Phase 1 indicates work is needed. This
reduces API costs but requires two LLM calls when work does exist.

### No Suppression

TinyClaw has no suppression mechanism. All heartbeat responses are logged as-is.

## Session Strategy

### Session Isolation

| System | Default Session | Isolated Mode | Light Context |
|--------|----------------|---------------|---------------|
| OpenClaw | Agent main session | `isolatedSession: true` | `lightContext: true` |
| ClawPod | Agent main session | `isolated_session: true` | `light_context: true` |
| ZeroClaw | Stateless | `load_session_context: true` (opt-in) | — |
| TinyClaw | Fresh per run | — | — |
| Clawith | Multi-turn with context injection | — | — |

OpenClaw and ClawPod default to the agent's main session, with an opt-in
isolated mode that creates a scratch session per heartbeat run. Light context
mode further reduces token usage by injecting only the heartbeat file.

ZeroClaw inverts the default: heartbeats are stateless unless
`load_session_context` is explicitly enabled.

Clawith always injects the last 50 activity log entries and unread notifications,
with no option to run without context.

### Transcript Pruning

OpenClaw prunes the session transcript back to pre-heartbeat size when the
response is only `HEARTBEAT_OK`. This prevents heartbeat turns from polluting the
main session's conversation history.

ClawPod cannot replicate this exactly because session history is owned by the
provider CLIs (`claude -c`, `codex exec resume --last`), not by ClawPod itself.
This is the primary remaining parity gap.

## Active Hours

| System | Supported | Format | Timezone |
|--------|-----------|--------|----------|
| OpenClaw | Yes | Time window | IANA timezone |
| ClawPod | Yes | `HH:MM` start/end | IANA timezone (default UTC) |
| ZeroClaw | No | — | — |
| TinyClaw | No | — | — |
| Clawith | Yes | `HH:MM-HH:MM` string | IANA timezone (inherits from tenant) |

OpenClaw, ClawPod, and Clawith gate scheduled heartbeats to a configured time
window. Event-driven heartbeats (manual, cron, webhook) bypass active hours in
OpenClaw and ClawPod.

## Unique Features by System

### ZeroClaw

- **Two-Phase Mode**: Cost optimization by checking for work before running the
  full heartbeat.
- **Dead-Man's Switch**: Alert on a separate channel/recipient if a heartbeat
  misses its expected window. Configured via `deadman_timeout_minutes`,
  `deadman_channel`, and `deadman_to`.
- **Adaptive Intervals**: Automatically adjust heartbeat interval based on
  failure rate, bounded by `min_interval_minutes` and `max_interval_minutes`.

### Clawith

- **Multi-Turn Tool Execution**: Heartbeat runs up to 20 LLM rounds with access
  to tools (web search, file operations, plaza posting, task management). This
  makes heartbeat a full autonomous activity period, not just a status check.
- **Plaza Integration**: Heartbeat can post to the shared agent plaza (max 1
  post, max 2 comments per run), enabling inter-agent communication during
  heartbeat.
- **Notification Draining**: Reads and marks unread notifications as part of the
  heartbeat cycle.

### OpenClaw

- **Transcript Pruning**: OK-only heartbeats are pruned from session history to
  prevent context pollution.
- **Reasoning Delivery**: `includeReasoning: true` sends the LLM's reasoning
  trace as a separate message.
- **Duplicate Suppression**: 24-hour deduplication window prevents sending
  identical heartbeat outputs repeatedly.

### ClawPod

- **Chatroom Target**: Can deliver heartbeat output to the internal team
  chatroom, not just external channels.
- **Reset Flag**: Placing a `reset.flag` file in the agent's `.clawpod/`
  directory triggers a full session reset on the next heartbeat run.
- **`heartbeat.md` Frontmatter**: YAML frontmatter in the heartbeat file can
  override `target` and `to` without touching the TOML config.

## Summary

ClawPod's heartbeat design is a near-exact port of OpenClaw's model: same config
shape, same precedence rules, same delivery targeting, same HEARTBEAT_OK
semantics, same active-hours gating. The only structural gap is transcript
pruning, which requires ClawPod to own session history rather than delegating to
provider CLIs.

ZeroClaw optimizes for cost (two-phase, adaptive intervals, dead-man's switch)
at the expense of per-agent flexibility. Clawith treats heartbeat as autonomous
work time with multi-turn tool access. TinyClaw is a minimal implementation
suitable for prototyping.

When evaluating features to adopt from other systems, the candidates with the
highest value-to-effort ratio are:

1. **ZeroClaw's two-phase mode** — reduces API cost for agents that rarely have
   work during heartbeat.
2. **Clawith's multi-turn tool execution** — enables heartbeat to perform actual
   work, not just report status.
3. **ZeroClaw's dead-man's switch** — operational safety net for production
   deployments.
