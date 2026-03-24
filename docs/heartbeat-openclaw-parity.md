# Heartbeat OpenClaw Parity Design

Status: draft

## Goal

Bring `clawpod` heartbeat behavior up to OpenClaw-level parity, not just by
adding a few config flags, but by matching the operational model:

- periodic agent turns with a default heartbeat prompt
- per-agent heartbeat policy with shared defaults
- main-session heartbeats by default
- explicit delivery targets (`none`, `last`, channel-specific)
- `HEARTBEAT_OK` ack suppression with `ack_max_chars`
- active-hours gating
- visibility controls (`show_ok`, `show_alerts`, `use_indicator`)
- manual wake / future cron-webhook integration
- duplicate suppression and proper run history

This document describes what `clawpod` should do if we want full behavioral
alignment, including the breaking changes required to get there cleanly.

## Non-Goal

This is not a minimal patch plan.

If we only want a smaller improvement, we can add `HEARTBEAT_OK` stripping and
delivery targeting on top of the current design. That would improve ergonomics,
but it would still not match OpenClaw's session model, run semantics, or
automation hooks.

## Current State

Today heartbeat is a thin scheduler:

- top-level `[heartbeat]` only exposes `enabled`, `interval_sec`, and `sender`
- every tick walks every agent
- each agent's `heartbeat.md` is loaded verbatim and enqueued as a synthetic
  inbound message on channel `heartbeat`
- empty/comment-only `heartbeat.md` disables that agent's heartbeat
- heartbeat runs are recorded, but all heartbeat outbound delivery is dropped
  unconditionally

This design is simple, but it is fundamentally different from OpenClaw in four
ways:

1. Heartbeat is modeled as a fake inbound channel instead of a first-class
   automation run.
2. There is no per-agent heartbeat policy object.
3. There is no concept of heartbeat delivery targeting or visibility policy.
4. Session ownership lives in the provider CLI (`claude -c`, `codex exec resume
   --last`), so `clawpod` cannot prune or restore heartbeat turns exactly.

## Key Design Decision

If parity is the goal, heartbeat must stop being "just another queued inbound
message".

The recommended design is:

- keep the scheduler in `crates/runtime`
- move heartbeat execution into a dedicated `HeartbeatService`
- extract the common "run one agent task" path from `QueueProcessor` into a
  shared execution service
- let inbound chat and heartbeat both use the same execution core, but keep
  heartbeat-specific prompt/session/delivery logic out of the normal inbound
  message path

Without this separation, every parity feature becomes a special case inside the
queue processor and the code keeps getting harder to reason about.

## Target Behavior

### Prompt contract

Scheduled heartbeat runs should use a built-in default prompt when no explicit
override is configured:

`Read HEARTBEAT.md if it exists (workspace context). Follow it strictly. Do not infer or repeat old tasks from prior chats. If nothing needs attention, reply HEARTBEAT_OK.`

Rules:

- if `HEARTBEAT.md` is missing, heartbeat still runs
- if `HEARTBEAT.md` exists but is effectively empty, scheduled heartbeat skips
  to save calls
- manual wake may still run even if `HEARTBEAT.md` is empty
- heartbeat runs add an explicit heartbeat section to the system prompt so the
  model knows this is an automation turn

### Session contract

Default heartbeat context must be the agent main session, not a dedicated
synthetic `heartbeat` session and not a per-channel DM session.

Rules:

- default heartbeat session: `agent:<agent_id>:<main_key>`
- optional explicit `session` override targets a known session key
- optional `isolated_session = true` runs in a fresh scratch session with no
  previous conversation
- heartbeat runs must not depend on `channel = "heartbeat"` to derive session
  identity

### Delivery contract

Heartbeat output must be deliverable, with the same decision points OpenClaw
has:

- `target = "none"`: run heartbeat but do not send externally
- `target = "last"`: send to the last resolved external contact for the chosen
  session
- `target = "<channel>"`: send to a specific channel adapter
- `to = ...`: explicit recipient override
- `account_id = ...`: explicit multi-account override
- `direct_policy = "allow" | "block"`: block DM-style heartbeat delivery while
  still running the turn

### Response contract

Heartbeat runs must treat `HEARTBEAT_OK` specially:

- strip `HEARTBEAT_OK` only when it appears at the start or end
- if remaining text length is `<= ack_max_chars`, suppress external delivery
- if `HEARTBEAT_OK` appears in the middle, treat it as normal text
- for alert text, the model should not include `HEARTBEAT_OK`
- optionally send a separate reasoning payload when `include_reasoning = true`

### Scheduling contract

Heartbeat cadence is per-agent, with shared defaults:

- shared default heartbeat policy
- per-agent override
- if any agent declares heartbeat config, only those agents participate
- active-hours gating based on configured timezone
- if the target session is busy, skip with an explicit reason instead of
  queueing an unbounded backlog

### Visibility contract

Delivery policy and visibility policy are separate:

- `show_ok`: whether OK-only acks can be delivered
- `show_alerts`: whether non-OK payloads can be delivered
- `use_indicator`: whether heartbeat status is still emitted to UI/telemetry

If all three are false, scheduled heartbeats should short-circuit before a
model call.

## Proposed Config Shape

To reach parity cleanly, heartbeat configuration should move out of the current
top-level `[heartbeat]` block and into agent/channel policy objects.

Recommended shape:

```toml
[heartbeat]
enabled = true

[agent_defaults.heartbeat]
every = "30m"
target = "none"
prompt = "Read HEARTBEAT.md if it exists (workspace context). Follow it strictly. Do not infer or repeat old tasks from prior chats. If nothing needs attention, reply HEARTBEAT_OK."
ack_max_chars = 300
direct_policy = "allow"
include_reasoning = false
light_context = false
isolated_session = false
# session = "main"
# model = "openai/gpt-5.4"
# active_hours.start = "09:00"
# active_hours.end = "22:00"
# active_hours.timezone = "America/New_York"

[agents.default]
name = "Default"
provider = "anthropic"
model = "claude-sonnet-4-6"

[agents.default.heartbeat]
target = "last"

[channels.defaults.heartbeat]
show_ok = false
show_alerts = true
use_indicator = true

[channels.telegram.heartbeat]
show_ok = true
```

### Compatibility policy

We should keep the current top-level `[heartbeat]` temporarily, but only as a
compatibility shim:

- `heartbeat.enabled` remains the global master switch
- `heartbeat.interval_sec` is deprecated and maps into
  `agent_defaults.heartbeat.every` only when the new cadence is unset
- `heartbeat.sender` is deprecated and ignored once heartbeat is no longer
  modeled as a synthetic inbound message

This lets us migrate existing configs without forcing a flag day.

## Required Data Model Changes

### 1. Agent heartbeat policy

Add a new `AgentHeartbeatConfig` and attach it to:

- `agent_defaults`
- each `AgentConfig`

Suggested fields:

- `every`
- `model`
- `prompt`
- `target`
- `to`
- `account_id`
- `session`
- `ack_max_chars`
- `direct_policy`
- `include_reasoning`
- `light_context`
- `isolated_session`
- `suppress_tool_error_warnings`
- `active_hours`

### 2. Channel heartbeat visibility

Add `ChannelHeartbeatConfig` under:

- `channels.defaults`
- each concrete channel config

Suggested fields:

- `show_ok`
- `show_alerts`
- `use_indicator`

### 3. Session delivery context

Current `SessionRecord` only stores:

- `session_key`
- `agent_id`
- `created_at`
- `updated_at`

That is insufficient for `target = "last"`.

We need to extend session state so heartbeat can resolve the last external
delivery target:

- `last_channel`
- `last_peer_id`
- `last_account_id`
- `last_chat_type`
- `last_sender_id`
- `last_message_id`
- optional `last_thread_id` for future thread-aware adapters
- `last_heartbeat_text`
- `last_heartbeat_sent_at`

We also need an agent-level fallback:

- `agent_last_external_context`

Reason: if human chat remains scoped per-channel, a main-session heartbeat still
needs somewhere to resolve `target = "last"` from.

### 4. Heartbeat run record v2

The current `HeartbeatRunRecord` is too thin. Extend it with:

- `reason` (`scheduled`, `manual`, `cron`, `webhook`, `exec_exit`)
- `session_key`
- `delivery_channel`
- `delivery_recipient`
- `delivery_account_id`
- `delivery_mode`
- `status` (`ran`, `skipped`, `failed`)
- `skip_reason`
- `preview`
- `used_model`
- `used_prompt`
- `indicator_type`

This gives Office enough information to explain heartbeat behavior instead of
just showing raw output blobs.

## Required Runtime Changes

### 1. Extract execution core from `QueueProcessor`

Create a shared service, for example `AgentRunService`, that owns:

- session locking
- global concurrency guard
- prompt augmentation
- metadata construction
- runner invocation
- run-store persistence
- outbound payload preparation

Then:

- inbound chat uses `AgentRunService`
- heartbeat uses `AgentRunService`

This avoids duplicating run logic and removes heartbeat-specific hacks from the
queue path.

### 2. Add `HeartbeatService`

Create a dedicated service responsible for:

- resolving which agents are heartbeat-enabled
- resolving effective heartbeat policy
- resolving the run session
- loading `HEARTBEAT.md`
- enforcing active-hours gating
- applying visibility short-circuit rules
- invoking `AgentRunService`
- normalizing heartbeat responses
- deciding whether to deliver, suppress, or skip
- recording structured heartbeat results
- emitting heartbeat-specific events

Public API:

- `run_scheduled_cycle(now)`
- `run_once(agent_id, reason, overrides)`
- `resolve_effective_policy(agent_id)`

### 3. Stop enqueueing heartbeat as an inbound message

Delete the current synthetic inbound behavior from `crates/runtime/src/heartbeat.rs`.

Instead of:

- enqueue synthetic `channel = "heartbeat"`
- let normal queue processing handle it

Do:

- ask `HeartbeatService` to execute the run directly

This removes three current mismatches:

- wrong session key derivation
- unconditional outbound drop for heartbeat runs
- fake sender/channel metadata leaking into routing and hooks

## Session Strategy

### Functional parity target

Heartbeat should default to the agent main session regardless of current
`session.dm_scope`.

Recommended resolution order:

1. explicit `heartbeat.session`
2. agent main session (`agent:<id>:<main_key>`)
3. isolated scratch session when `isolated_session = true`

### Exact parity blocker

OpenClaw can prune or restore heartbeat-only turns. `clawpod` currently cannot,
because conversation state is owned by provider CLIs:

- Anthropic path uses `claude -c`
- OpenAI path uses `codex exec resume --last`

That means `clawpod` does not control transcript storage strongly enough to:

- delete the just-added heartbeat turn
- restore pre-heartbeat `updated_at`
- guarantee exact "main session stays clean on OK-only heartbeat" semantics

If exact parity is required, this becomes a hard prerequisite:

- move session history ownership from provider CLIs into `clawpod`
- or replace resume-based providers with integrations that accept explicit
  conversation history under `clawpod` control

Without that change, we can reach strong functional parity, but not exact
history parity.

### Recommendation

Treat this as a two-stage delivery:

1. full external behavior parity on top of the current runner model
2. exact transcript parity after runner/session ownership moves into `clawpod`

## Prompt Builder Changes

Add a heartbeat-aware system prompt section.

Needed changes:

- add `is_heartbeat` and effective heartbeat policy into `PromptContext`
- add `HeartbeatSection` to `SystemPromptBuilder`
- when `light_context = true`, inject only `heartbeat.md` from workspace files
- preserve current `SOUL.md` and `AGENTS.md` behavior for normal runs

The system prompt should explicitly tell the model:

- this is a heartbeat run
- `HEARTBEAT_OK` is a special ack token
- alerts must omit `HEARTBEAT_OK`

## Delivery Pipeline Changes

Heartbeat delivery must stop piggybacking on `write_outgoing`'s current
"drop everything for heartbeat channel" logic.

Instead:

- heartbeat output is normalized before outbound enqueue
- delivery target is resolved explicitly
- outbound enqueue happens only when delivery policy says yes
- `target = "none"` records the run but sends nothing

Normalization pipeline:

1. strip `HEARTBEAT_OK` token at edges
2. apply `ack_max_chars`
3. detect duplicate payloads within a suppression window
4. split reasoning payload when enabled
5. respect channel visibility (`show_ok`, `show_alerts`)
6. enqueue outbound payloads

## API, Office, and CLI

### Office API

Extend `/api/heartbeat/runs` to return:

- resolved per-agent heartbeat summary
- skip reasons
- delivery target summary
- visibility summary

Add a manual trigger endpoint:

- `POST /api/heartbeat/run`

Payload:

- `agent_id`
- `mode = "now" | "next_heartbeat"` for future scheduler integration
- optional `reason`

### CLI

Add first-class heartbeat commands:

- `clawpod heartbeat last`
- `clawpod heartbeat run --agent <id>`
- `clawpod heartbeat enable`
- `clawpod heartbeat disable`

If we later add cron/webhook automation, they must call the same
`HeartbeatService`, not recreate heartbeat behavior themselves.

### Office UI

The heartbeat page should show:

- enabled agents
- effective cadence
- effective target
- last status
- last skip reason
- last delivery summary
- whether alerts/OK/indicator are enabled

This page should become an operational surface, not just a raw run log.

## Hook and Automation Alignment

To fully align with OpenClaw's automation model, heartbeat must become the shared
wake target for future features:

- webhook-triggered wake
- cron "next heartbeat" mode
- background exec completion wake

That means we should reserve a small internal API now:

- `HeartbeatWakeReason`
- `HeartbeatWakeMode`
- `request_heartbeat(agent_id, reason, mode)`

Even if cron/webhooks are implemented later, the heartbeat subsystem should be
built so those features plug into it directly.

## Migration Plan

### Phase 1: functional parity foundation

- add new heartbeat config structs
- keep top-level `[heartbeat]` as compatibility shim
- add session delivery context to store
- add `HeartbeatService`
- stop queueing synthetic `heartbeat` inbound events
- add default prompt and `HEARTBEAT_OK` normalization
- add delivery target resolution and visibility rules
- add Office API/UI support

Result:

- outward behavior matches OpenClaw closely
- history still depends on CLI resume semantics

### Phase 2: automation integration

- add manual wake endpoint/CLI
- add reusable wake API for cron/webhook/exec completion
- add richer skip reasons and indicator events
- add duplicate suppression and target-specific status summaries

### Phase 3: exact transcript parity

- replace provider-owned session history with `clawpod`-owned history
- support exact pruning/restoration for OK-only heartbeat runs
- support main-session hygiene that matches OpenClaw semantics exactly

This is the only phase that is truly architectural.

## Testing Plan

Minimum required coverage:

- config parsing and precedence
- legacy config migration behavior
- agent selection when any per-agent heartbeat config exists
- active-hours resolution and timezone fallback
- target resolution for `none`, `last`, explicit channel, explicit recipient
- DM blocking with `direct_policy = "block"`
- `HEARTBEAT_OK` stripping and `ack_max_chars`
- duplicate suppression window
- visibility short-circuit when all outputs are disabled
- main-session vs explicit-session vs isolated-session execution
- manual wake endpoint and CLI
- Office API payload shape

Add integration tests for both providers:

- Anthropic runner path
- OpenAI runner path

The important part is validating behavior, not just config parsing.

## Breaking Changes and Risks

### Breaking changes

- heartbeat no longer behaves like a synthetic inbound channel
- config ownership moves from top-level `[heartbeat]` to agent/channel policy
- session records must be migrated to include delivery context
- Office heartbeat page must be rewritten around structured status

### Risks

- exact parity is impossible without runner/session ownership changes
- `target = "last"` is underspecified until session delivery context exists
- keeping backward compatibility forever will make precedence rules harder to
  explain
- heartbeat logic spread across scheduler, queue, store, and channel adapters
  will become brittle unless we centralize it early

## Recommendation Summary

If we want full parity, the right move is not "make the existing heartbeat queue
path more clever".

The right move is:

1. introduce first-class heartbeat policy objects
2. introduce first-class heartbeat execution service
3. store last external delivery context in session state
4. make heartbeat deliverable and suppressible through explicit rules
5. treat exact transcript cleanup as a runner-ownership project, not a small
   follow-up fix

That gets `clawpod` to OpenClaw-style heartbeat behavior with a design that can
still be maintained after cron, webhooks, and other automation features land.
