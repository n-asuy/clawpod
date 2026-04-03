# Per-Profile Browser + KasmVNC Design

Status: implemented in config/runtime/office; deployment rollout still required

## Implementation Status

Implemented in the repo:

- top-level `browser.profiles`
- `agents.<id>.browser.profile`
- validation for unique `cdp_port`, `display`, `kasm_port`, and `view_path`
- per-run browser metadata injection
- runner env injection for `DISPLAY` and `AGENT_BROWSER_*`
- built-in browser ensure/launch in the runner using the resolved profile metadata
- Office visibility for browser profiles and viewer entry links
- stable viewer entry paths proxied through `/view/<profile>/`
- Linux user-service generation for one KasmVNC unit per browser profile

Still operational, not automatic:

- installing KasmVNC itself on the host
- optional stronger isolation via `os_user` / `home_dir`

## Goal

Make browser state and visual browser sessions first-class in `clawpod` so that:

- each agent can use a stable browser profile
- operator-visible browser sessions do not fight each other for focus
- KasmVNC viewing can be exposed as stable per-profile URLs
- the design can later grow into stronger isolation without redoing the config model

This document describes the target architecture, not a minimal patch.

## Non-Goals

This design does not try to solve everything in phase 1.

Specifically out of scope for the initial pass:

- full Unix-user isolation for every profile
- containerized browser sandboxes
- a single KasmVNC display shared safely by multiple active agents
- path-only separation as the primary isolation boundary
- direct host-browser attach flows like OpenClaw's `existing-session`

## Current State

Today `clawpod` has the following properties:

- agent workdirs are separated under `workspace/<agent_id>`
- agents execute in one daemon process
- browser automation is not modeled in config
- KasmVNC setup assumes one Xvnc display
- the daemon service is configured globally with `DISPLAY=:1`

Relevant references:

- `AgentConfig` has no browser fields
- `RunRequest.metadata` already exists and can carry per-run execution hints
- `CliRunner` already injects environment variables into spawned CLIs
- KasmVNC deployment instructions currently hardcode one `:1` display

Operationally this means:

- browser cookies and login state are not first-class
- visual browser sessions are all painted onto the same desktop
- manual operator intervention can interfere with another agent's visual session
- path-based viewer URLs cannot provide real separation because the backend is still one display

## What Is Worth Borrowing From Other Claw Systems

### OpenClaw

OpenClaw is the most useful reference for this problem.

Its core ideas are:

- browser configuration is first-class under `browser.profiles`
- agents and browser profiles are separate concepts
- there is a stable `defaultProfile`
- profile-specific fields include `cdpPort`, `cdpUrl`, `userDataDir`, and `driver`
- auth state is also treated as per-agent state and must not be shared accidentally

The important lesson is architectural, not implementation-specific:

- the browser profile should be the primary object
- the agent should select a profile
- visual and credential state should not be hidden in ad hoc environment setup

### TinyClaw / Clawith

These are not strong references for this specific problem.

They contain useful multi-agent and per-agent configuration ideas, but they do
not provide a comparable browser-profile plus visual-session model.

## Key Design Decision

The primary unit should be a named browser profile, not the agent itself.

This is the recommended shape:

- top-level `browser.profiles.<name>` holds browser runtime state
- `agents.<id>.browser.profile` selects which profile the agent should use
- KasmVNC visual sessions attach to browser profiles, not directly to agents

This gives us:

- a stable place to define CDP port, profile dir, and display
- the ability to point multiple agents at the same browser profile if that is ever wanted
- a design close to OpenClaw's profile model

## Proposed Config Shape

```toml
[browser]
default_profile = "default"

[browser.profiles.default]
cdp_port = 9410
profile_dir = "~/.clawpod/browser/default"
display = ":11"
kasm_port = 8441
view_path = "/view/default"

[browser.profiles.reviewer]
cdp_port = 9411
profile_dir = "~/.clawpod/browser/reviewer"
display = ":12"
kasm_port = 8442
view_path = "/view/reviewer"

[agents.default]
name = "Default"
provider = "anthropic"
model = "claude-sonnet-4-6"

[agents.default.browser]
profile = "default"

[agents.reviewer]
name = "Reviewer"
provider = "openai"
model = "gpt-5"

[agents.reviewer.browser]
profile = "reviewer"
```

### Future-Compatible Extensions

Phase 1 should not require these, but the model should leave room for them:

```toml
[browser.profiles.default]
os_user = "agent_default"
home_dir = "/home/agent_default"
driver = "managed"
```

These fields support future Unix-user separation without changing the top-level shape.

## Separation Model

We need to separate three different things.

### 1. Browser state

This is handled by:

- `profile_dir`
- `cdp_port`

This isolates:

- cookies
- local storage
- browser extension state
- login sessions inside Chrome

### 2. Visual interaction

This is handled by:

- `display`
- `kasm_port`

This isolates:

- windows
- focus
- manual operator interaction
- what is visible inside each viewer

### 3. CLI and host credentials

This is not solved in phase 1.

It would later be handled by:

- `os_user`
- `home_dir`
- `HOME` and `XDG_*` overrides

This isolates:

- CLI auth state
- SSH keys
- `git` identity
- config files in `~/.config` and `~/.cache`

## Why One KasmVNC Instance Is Not Enough

One KasmVNC instance can be enough only if the goal is "keep browser cookies separate".

It is not enough if the goal is "keep agent操作 from interfering with each other".

With one shared display:

- windows overlap
- focus is shared
- operator intervention can hit the wrong browser window
- a path-based viewer split is cosmetic because the same desktop is still underneath

If visual sessions must not compete, each active profile needs its own display.

That means:

- profile `default` -> `DISPLAY=:11` -> KasmVNC backend A
- profile `reviewer` -> `DISPLAY=:12` -> KasmVNC backend B

The external path is only a routing convenience. The real boundary is the backend display.

## Path-Based Viewing

Path-based viewing is still useful, but only as a presentation layer.

Recommended model:

- internal backend per profile:
  - `127.0.0.1:8441` -> `DISPLAY=:11`
  - `127.0.0.1:8442` -> `DISPLAY=:12`
- external view paths:
  - `/view/default/`
  - `/view/reviewer/`

Important rule:

- path routing is not isolation
- path routing only selects which already-isolated backend to proxy to

If path proxying proves fragile for a future KasmVNC release, the fallback should be:

- subdomain per profile

The config model above still works in either case.

## Runtime Resolution Rules

For each run:

1. Resolve the agent.
2. Resolve `agent.browser.profile`, otherwise fall back to `browser.default_profile`.
3. Resolve the browser profile object.
4. Expand `profile_dir`.
5. Inject the resolved profile settings into the spawned CLI process.

If a profile is missing:

- fail the run early with a clear configuration error

If no agent browser profile is configured and no default exists:

- run without browser-profile env injection

## Execution Model Changes

The existing `RunRequest.metadata` and `CliRunner` are enough for phase 1.

No new runner abstraction is required.

### Queue Layer

When building run metadata for an agent, add:

- `browser_profile`
- `browser_cdp_port`
- `browser_profile_dir`
- `browser_display`
- `browser_kasm_port`
- `browser_view_path`

This should happen alongside existing per-run metadata such as provider and system prompt information.

### Runner Layer

When spawning the provider CLI, inject env vars such as:

- `DISPLAY`
- `AGENT_BROWSER_PROFILE`
- `AGENT_BROWSER_CDP_PORT`
- `AGENT_BROWSER_PROFILE_DIR`
- `AGENT_BROWSER_KASM_PORT`
- `AGENT_BROWSER_VIEW_PATH`

Future phase:

- `HOME`
- `XDG_CONFIG_HOME`
- `XDG_CACHE_HOME`
- `XDG_STATE_HOME`

This keeps browser runtime selection out of prompt text and inside execution context where it belongs.

## KasmVNC Lifecycle

Phase 1 should not make `clawpod` directly manage Xvnc process lifecycle.

Instead:

- define browser profiles in config
- run one systemd service per profile-backed visual session
- let `clawpod` consume those settings and target the matching display

Recommended service pattern:

- `kasmvnc@default.service`
- `kasmvnc@reviewer.service`

Each unit uses its own:

- display number
- websocket/listen port
- log files

This is the simplest path because it avoids teaching the daemon to supervise window-system processes immediately.

### Important Compatibility Change

The current global `DISPLAY=:1` in `clawpod.service` should be removed once per-profile injection exists.

Keeping a global display after introducing per-profile displays would silently route runs to the wrong desktop.

## Validation Rules

At config load or doctor time, enforce:

- `browser.default_profile` must exist if set
- every `agents.<id>.browser.profile` must exist
- `cdp_port` values must be unique across profiles
- `display` values must be unique across profiles
- `kasm_port` values must be unique across profiles
- `view_path` values must be unique across profiles

Optional doctor checks:

- `kasmvnc@<profile>` service is active
- the configured `kasm_port` is listening
- the configured `cdp_port` is either reachable or reserved for the matching profile

## Office / Viewer UX

The Office UI should expose browser viewing as profile-aware links.

Recommended UX:

- agent detail shows:
  - browser profile name
  - KasmVNC status
  - "Open viewer" link
- links target `view_path`
- if a profile has no configured visual session, show "no viewer configured"

This should not require the operator to know raw local ports.

## Rollout Plan

### Phase 1: First-Class Browser Profiles

- add `browser` config structs
- add `agents.<id>.browser.profile`
- resolve profile at run time
- inject env into spawned CLI
- document how skills/scripts should use the env values

This alone gives stable per-agent browser state.

### Phase 2: Multiple KasmVNC Backends

- define one KasmVNC systemd unit per browser profile
- stop using global `DISPLAY=:1`
- route each run to the profile's `display`

This removes operator and focus collisions.

### Phase 3: Viewer Routing

- expose `/view/<profile>/` in Office
- proxy HTTP assets and WebSocket traffic to the configured `kasm_port`
- keep `kasm_port` values internal-only and do not publish them through Tailscale Serve

This gives the operator a clean stable surface without external per-profile ports.

### Phase 4: Stronger Host Isolation

- add optional `os_user` and `home_dir`
- inject `HOME` and `XDG_*`
- optionally run provider CLI as the profile's user

This addresses CLI auth collisions and broader host-state separation.

## Tradeoffs

### Why not keep browser config under the agent directly?

Because it hard-codes a one-to-one relationship too early.

The profile-first model is better because:

- it matches OpenClaw's useful abstraction
- it keeps visual and browser runtime state reusable
- it is easier to reason about in viewer routing

### Why not use URL paths as the main separator?

Because paths do not isolate browser sessions or displays.

They only select a backend.

If the backend is shared, the path split is fake.

### Why not do Unix-user separation immediately?

Because it is operationally heavier and not required to stop browser-window conflicts.

The fastest high-value move is:

- browser profile separation
- display separation

Unix-user separation remains the right next step once CLI credential conflicts matter.

## Recommended Initial Implementation

The recommended first implementation for `clawpod` is:

1. Add `browser.profiles` and `agents.<id>.browser.profile`.
2. Inject per-profile env in `CliRunner`.
3. Remove the assumption that the daemon has one global display.
4. Define one KasmVNC backend per browser profile.
5. Add stable per-profile viewer URLs.

This is the smallest design that:

- follows the strongest useful OpenClaw idea
- fixes browser-state ambiguity
- fixes visual-session contention
- leaves room for future credential and host isolation
