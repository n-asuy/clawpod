# Run Execution Viewer

Status: draft

## Goal

Add a real-time execution viewer to the Office UI so operators can see what each
agent run is doing as it happens: tool calls, intermediate output, errors, and
final response.

## Current State

Today the runner operates in batch mode:

1. Spawn CLI process (`claude` or `codex`) with piped stdout/stderr.
2. Wait for full process completion.
3. Read all stdout/stderr into memory.
4. For Codex: `extract_codex_text()` parses JSONL, extracts only the last
   `item.completed` event with `type: "agent_message"`. All other events
   (tool calls, thinking, intermediate messages) are discarded.
5. For Claude: raw stdout is used as-is. No structured parsing.
6. Store `RunRecord` with final `output` text, `status`, `duration_ms`.

The Office UI shows a runs table (ID, agent, status, timestamps) with no output
content, no drill-down, and no streaming.

## Design

### Approach: Incremental JSONL Streaming

Instead of waiting for process completion, read stdout line-by-line and emit
each parsed event to the UI in real time.

This requires two changes:

1. **Runner**: Stream stdout lines instead of buffering to completion.
2. **Server**: Forward parsed events to the UI via SSE.

The runner still spawns the same CLI processes. No change to the provider
interface or session model.

### Provider Coverage

**Codex (OpenAI)**: Already emits structured JSONL to stdout. Each line is a
self-contained JSON event. The event types observed include:

- `item.completed` with `item.type: "agent_message"` — final text
- `item.completed` with other item types — tool calls, tool results
- Other event types — execution lifecycle

This is the primary target. Parse each JSONL line and forward it.

**Claude (Anthropic)**: Emits unstructured text to stdout. Two options:

- (a) Add `--output-format json` or `--json` flag if the CLI supports it.
- (b) Treat the entire stdout as a single text stream and emit periodic text
  chunks.

Option (b) is the pragmatic starting point. Structured Claude CLI streaming can
be added later if the CLI gains a JSON output mode.

## Data Model

### RunEvent

A new type representing a single event within a run:

```rust
pub struct RunEvent {
    pub run_id: String,
    pub seq: u32,
    pub timestamp: String,         // RFC3339
    pub event_type: RunEventType,
    pub data: serde_json::Value,   // raw event payload
}

pub enum RunEventType {
    /// Run started executing
    Started,
    /// Tool invocation (name, arguments)
    ToolCall,
    /// Tool execution result (name, output)
    ToolResult,
    /// Agent text output (intermediate or final)
    AgentMessage,
    /// Thinking / reasoning content
    Thinking,
    /// Run completed
    Completed,
    /// Run failed
    Failed,
    /// Raw text chunk (for unstructured providers)
    TextChunk,
}
```

### RunRecord Extension

Add optional fields to `RunRecord`:

```rust
pub struct RunRecord {
    // ... existing fields ...

    /// Model actually used for this run
    pub model: Option<String>,
    /// Provider kind (anthropic, openai, custom)
    pub provider: Option<String>,
    /// Number of events emitted during this run
    pub event_count: Option<u32>,
    /// Raw stderr captured after completion
    pub stderr: Option<String>,
}
```

### In-Memory Event Buffer

Run events are held in memory during execution and for a retention window
after completion. They are not persisted to `clawpod-state.json`.

```rust
pub struct RunEventBuffer {
    /// Events keyed by run_id, ordered by seq
    runs: HashMap<String, Vec<RunEvent>>,
    /// Maximum completed runs to retain (default: 50)
    max_completed_runs: usize,
    /// Maximum events per run (default: 500)
    max_events_per_run: usize,
}
```

Rationale: Run events are high-volume and ephemeral. Persisting them to the
JSON state file would degrade write performance and bloat the file. The buffer
provides real-time and recent-history access. For longer retention, a future
phase can write events to a separate append-only log file.

## Runner Changes

### Streaming Runner Trait

Extend the `Runner` trait with an optional streaming method:

```rust
#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, request: RunRequest) -> Result<RunResult>;

    async fn run_streamed(
        &self,
        request: RunRequest,
        tx: tokio::sync::mpsc::Sender<RunEvent>,
    ) -> Result<RunResult> {
        // Default: fall back to non-streaming run
        self.run(request).await
    }
}
```

### CliRunner Streaming Implementation

Replace the current batch read with line-by-line async reading:

```rust
// Pseudocode
let stdout = child.stdout.take().unwrap();
let reader = BufReader::new(stdout);
let mut lines = reader.lines();
let mut seq = 0;

while let Some(line) = lines.next_line().await? {
    seq += 1;
    let event = match provider {
        Openai => parse_codex_jsonl(&line, run_id, seq),
        Anthropic => parse_text_chunk(&line, run_id, seq),
        _ => parse_text_chunk(&line, run_id, seq),
    };
    if let Some(event) = event {
        let _ = tx.send(event).await;
    }
}
```

### Codex JSONL Parsing

Map Codex event types to `RunEventType`:

| Codex Event | RunEventType |
|-------------|-------------|
| `item.completed` + `agent_message` | `AgentMessage` |
| `item.completed` + tool-related type | `ToolResult` |
| tool invocation events | `ToolCall` |
| Other recognized events | mapped by type |
| Unrecognized events | forwarded as raw `data` |

The parser should preserve the original JSON in `data` for the UI to render
details. `RunEventType` is a classification for filtering and display, not a
lossy transformation.

### Backward Compatibility

`extract_codex_text()` remains unchanged. The final `RunResult.text` is still
derived the same way. Streaming events are an additional output channel, not a
replacement.

## Server Changes

### SSE Event Extension

The existing `/api/events/stream` SSE endpoint gains new event types:

```
event: run_event
data: {"run_id":"...","seq":1,"timestamp":"...","event_type":"tool_call","data":{...}}

event: run_started
data: {"run_id":"...","agent_id":"...","session_key":"...","prompt_preview":"..."}

event: run_completed
data: {"run_id":"...","status":"succeeded","duration_ms":4200,"output_preview":"..."}
```

Clients filter by `run_id` to follow a specific run.

### REST Endpoints

```
GET /api/runs/:run_id
```

Returns the full `RunRecord` including model, provider, stderr, and event_count.

```
GET /api/runs/:run_id/events
```

Returns all buffered `RunEvent` entries for a run. Returns `404` if the run has
been evicted from the buffer.

```
GET /api/runs
```

Existing endpoint. Add optional query parameter `?status=running` to filter
active runs.

## Office UI Changes

### Run Detail View

Clicking a run row in the runs table opens a detail view with two sections:

**Header:**
- Run ID, agent, status badge, duration
- Model, provider
- Session key (linked to session view)
- Prompt (collapsible, first 200 chars visible)

**Event Timeline:**
- Chronological list of `RunEvent` entries
- Each entry shows: timestamp, event type icon, content

Event type rendering:

| Type | Display |
|------|---------|
| `ToolCall` | Tool name as header, arguments as collapsible JSON |
| `ToolResult` | Tool name as header, output as collapsible text block |
| `AgentMessage` | Text content, rendered as markdown |
| `Thinking` | Muted/italic text block, collapsed by default |
| `TextChunk` | Appended to a streaming text area |
| `Started` | Timestamp marker |
| `Completed` | Status badge + duration |
| `Failed` | Error message in red |

### Live Streaming

When viewing a running execution:

1. Load existing events from `GET /api/runs/:run_id/events`.
2. Subscribe to SSE stream, filter for matching `run_id`.
3. Append new events to the timeline as they arrive.
4. Auto-scroll to bottom (toggleable).
5. Show a pulsing indicator while the run is active.

When the `run_completed` or `run_failed` SSE event arrives, stop streaming and
show final status.

### Runs Table Enhancement

Add columns:

| Column | Source |
|--------|--------|
| Agent | `agent_id` (existing) |
| Status | badge with color (existing) |
| Duration | `duration_ms` formatted as `Xs` or `Xm Ys` |
| Output preview | First 80 chars of `output` |
| Events | `event_count` |

Add a filter for active/completed runs. Highlight currently running rows with a
subtle animation.

## Queue Processor Integration

The queue processor creates the `mpsc::channel`, passes `tx` to
`runner.run_streamed()`, and forwards received events to:

1. `RunEventBuffer` (in-memory storage)
2. SSE broadcast (via the existing event sink)

```rust
let (tx, mut rx) = mpsc::channel::<RunEvent>(256);

// Spawn event forwarding task
let buffer = run_event_buffer.clone();
let sink = event_sink.clone();
tokio::spawn(async move {
    while let Some(event) = rx.recv().await {
        buffer.push(event.clone());
        sink.emit_run_event(event);
    }
});

let result = runner.run_streamed(request, tx).await?;
```

## Phases

### Phase 1: Codex JSONL Streaming + Detail View

- Implement `run_streamed` for `CliRunner` with Codex JSONL parsing.
- Add `RunEventBuffer` to store.
- Extend SSE with `run_event`, `run_started`, `run_completed`.
- Add `GET /api/runs/:run_id` and `GET /api/runs/:run_id/events`.
- Build run detail view in Office UI with event timeline.
- Claude provider falls back to `TextChunk` per line.

This phase delivers the core value: operators can see what Codex is doing in
real time.

### Phase 2: Richer Event Parsing

- Parse additional Codex event types (thinking, function calls with arguments).
- Add Claude `--json` support if/when the CLI supports it.
- Add event filtering in the UI (by type, by text search).
- Add cost/token display if available in Codex events.

### Phase 3: Retention and Export

- Write completed run events to an append-only JSONL file for post-mortem
  analysis.
- Add `GET /api/runs/:run_id/events/export` for JSONL download.
- Add configurable retention policy.

## Not In Scope

- Modifying the provider CLI behavior or flags (we consume what the CLI emits).
- Replacing the CLI-based runner with a direct API integration.
- Real-time editing or intervention in a running execution.
- Persisting events to the main `clawpod-state.json` file.
