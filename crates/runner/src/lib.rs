use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use domain::{ProviderKind, RunEvent, RunEventType, RunRequest, RunResult, Runner};
use serde_json::Value;
use tokio::fs;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};
use tracing::warn;

#[derive(Debug, Clone)]
pub struct CliRunner {
    timeout: Duration,
}

impl CliRunner {
    pub fn new(timeout_sec: u64) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_sec),
        }
    }
}

#[async_trait::async_trait]
impl Runner for CliRunner {
    async fn run(&self, request: RunRequest) -> Result<RunResult> {
        let (tx, _rx) = mpsc::channel(1);
        self.run_streamed(request, tx).await
    }

    async fn run_streamed(
        &self,
        request: RunRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<RunResult> {
        let started = Instant::now();
        let run_id = request.run_id.to_string();

        if matches!(request.provider, ProviderKind::Mock) {
            return run_mock(request).await;
        }

        let provider = request.provider;
        let (program, args, mut envs) = match request.provider {
            ProviderKind::Anthropic => {
                let (program, args) = build_claude_command(&request);
                (program, args, vec![])
            }
            ProviderKind::Openai => {
                let (program, args) = build_codex_command(&request);
                let mut envs = vec![];
                if let Some(api_key) = request.metadata.get("openai_api_key") {
                    envs.push(("OPENAI_API_KEY".to_string(), api_key.clone()));
                }
                (program, args, envs)
            }
            ProviderKind::Custom => build_custom_command(&request)?,
            ProviderKind::Mock => unreachable!("mock handled above"),
        };
        envs.extend(build_execution_envs(&request));

        // Use std::process (blocking) via spawn_blocking to avoid Tokio's
        // async Child::wait() and edge-triggered pipe EOF issues. Piped
        // stdout/stderr are read synchronously in the blocking thread,
        // completely bypassing Tokio's epoll/SIGCHLD machinery.
        let working_directory = request.working_directory.clone();
        let program_name = program.clone();

        let is_codex =
            matches!(provider, ProviderKind::Openai) || is_openai_harness(&request.metadata);
        let is_claude =
            matches!(provider, ProviderKind::Anthropic) || is_anthropic_harness(&request.metadata);
        let request_metadata = request.metadata.clone();

        let run_id_clone = run_id.clone();

        let run_child = async move {
            tokio::task::spawn_blocking(move || {
                use std::io::Read;

                let mut child = std::process::Command::new(&program)
                    .args(&args)
                    .current_dir(&working_directory)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                    .spawn()
                    .with_context(|| format!("failed to spawn runner: {program}"))?;

                let mut stdout_buf = Vec::new();
                let mut stderr_buf = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut stdout_buf);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_end(&mut stderr_buf);
                }
                let status = child.wait().context("failed to wait for runner")?;

                let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
                let stderr = String::from_utf8_lossy(&stderr_buf).to_string();

                // Emit streaming events from collected output
                let mut seq: u32 = 0;
                for line in stdout.lines() {
                    seq += 1;
                    let event = if is_codex {
                        parse_codex_jsonl_event(line, &run_id_clone, seq)
                    } else if is_claude {
                        parse_claude_stream_event(line, &run_id_clone, seq)
                    } else {
                        None
                    };
                    if let Some(ev) = event {
                        // tx.send is sync mpsc::Sender::blocking_send is not
                        // available, but try_send works for buffered events
                        let _ = tx.try_send(ev);
                    }
                }

                Ok::<_, anyhow::Error>((stdout, stderr, status))
            })
            .await
            .context("spawn_blocking join failed")?
        };

        let (stdout, stderr, status) = timeout(self.timeout, run_child)
            .await
            .map_err(|_| anyhow!("runner timed out after {}s", self.timeout.as_secs()))?
            .with_context(|| format!("failed to execute runner command: {program_name}"))?;

        let exit_code = status.code().unwrap_or(-1);

        let text = if is_codex {
            extract_codex_text(&stdout).unwrap_or_else(|| {
                warn!("codex json parse fallback to raw stdout");
                stdout.clone()
            })
        } else if is_claude {
            extract_claude_result_text(&stdout).unwrap_or_else(|| {
                warn!("claude stream-json parse fallback to raw stdout");
                stdout.clone()
            })
        } else {
            stdout.clone()
        };

        Ok(RunResult {
            text: text.trim().to_string(),
            stdout,
            stderr,
            exit_code,
            duration_ms: started.elapsed().as_millis(),
            metadata: request_metadata,
        })
    }
}

fn is_openai_harness(metadata: &std::collections::HashMap<String, String>) -> bool {
    metadata
        .get("custom_harness")
        .is_some_and(|h| h == "openai")
}

fn build_execution_envs(request: &RunRequest) -> Vec<(String, String)> {
    let mut envs = vec![];
    if let Some(display) = request.metadata.get("browser_display") {
        envs.push(("DISPLAY".to_string(), display.clone()));
        envs.push(("AGENT_BROWSER_DISPLAY".to_string(), display.clone()));
    }
    if let Some(profile) = request.metadata.get("browser_profile") {
        envs.push(("AGENT_BROWSER_PROFILE".to_string(), profile.clone()));
    }
    if let Some(cdp_port) = request.metadata.get("browser_cdp_port") {
        envs.push(("AGENT_BROWSER_CDP_PORT".to_string(), cdp_port.clone()));
    }
    if let Some(profile_dir) = request.metadata.get("browser_profile_dir") {
        envs.push(("AGENT_BROWSER_PROFILE_DIR".to_string(), profile_dir.clone()));
    }
    if let Some(kasm_port) = request.metadata.get("browser_kasm_port") {
        envs.push(("AGENT_BROWSER_KASM_PORT".to_string(), kasm_port.clone()));
    }
    if let Some(view_path) = request.metadata.get("browser_view_path") {
        envs.push(("AGENT_BROWSER_VIEW_PATH".to_string(), view_path.clone()));
    }
    envs
}

// ---------------------------------------------------------------------------
// Codex JSONL event parsing
// ---------------------------------------------------------------------------

fn parse_codex_jsonl_event(line: &str, run_id: &str, seq: u32) -> Option<RunEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    let event_type_str = value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let now = Utc::now().to_rfc3339();

    match event_type_str {
        // Tool call initiated
        "item.created" => {
            let item = value.get("item")?;
            let item_type = item
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            match item_type {
                "function_call" => {
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    Some(RunEvent {
                        run_id: run_id.to_string(),
                        seq,
                        timestamp: now,
                        event_type: RunEventType::ToolCall,
                        data: serde_json::json!({
                            "name": name,
                            "call_id": call_id,
                            "phase": "start",
                        }),
                    })
                }
                "agent_message" => Some(RunEvent {
                    run_id: run_id.to_string(),
                    seq,
                    timestamp: now,
                    event_type: RunEventType::AgentMessage,
                    data: serde_json::json!({ "phase": "start" }),
                }),
                _ => Some(RunEvent {
                    run_id: run_id.to_string(),
                    seq,
                    timestamp: now,
                    event_type: RunEventType::TextChunk,
                    data: value,
                }),
            }
        }
        // Tool call or agent message completed
        "item.completed" => {
            let item = value.get("item")?;
            let item_type = item
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            match item_type {
                "function_call" => {
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let arguments = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    Some(RunEvent {
                        run_id: run_id.to_string(),
                        seq,
                        timestamp: now,
                        event_type: RunEventType::ToolCall,
                        data: serde_json::json!({
                            "name": name,
                            "call_id": call_id,
                            "arguments": arguments,
                            "phase": "completed",
                        }),
                    })
                }
                "function_call_output" => {
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let output = item
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    Some(RunEvent {
                        run_id: run_id.to_string(),
                        seq,
                        timestamp: now,
                        event_type: RunEventType::ToolResult,
                        data: serde_json::json!({
                            "call_id": call_id,
                            "output": truncate_str(output, 4000),
                        }),
                    })
                }
                "agent_message" => {
                    let text = item
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    Some(RunEvent {
                        run_id: run_id.to_string(),
                        seq,
                        timestamp: now,
                        event_type: RunEventType::AgentMessage,
                        data: serde_json::json!({
                            "text": text,
                            "phase": "completed",
                        }),
                    })
                }
                "reasoning" => Some(RunEvent {
                    run_id: run_id.to_string(),
                    seq,
                    timestamp: now,
                    event_type: RunEventType::Thinking,
                    data: serde_json::json!({
                        "text": item.get("text").and_then(|v| v.as_str()).unwrap_or_default(),
                    }),
                }),
                _ => Some(RunEvent {
                    run_id: run_id.to_string(),
                    seq,
                    timestamp: now,
                    event_type: RunEventType::TextChunk,
                    data: value,
                }),
            }
        }
        // Reasoning item
        "item.reasoning" | "reasoning" => Some(RunEvent {
            run_id: run_id.to_string(),
            seq,
            timestamp: now,
            event_type: RunEventType::Thinking,
            data: value,
        }),
        // Usage / completion events
        "response.completed" | "response.done" => {
            let usage = value.get("response").and_then(|r| r.get("usage"));
            Some(RunEvent {
                run_id: run_id.to_string(),
                seq,
                timestamp: now,
                event_type: RunEventType::Completed,
                data: serde_json::json!({
                    "usage": usage,
                }),
            })
        }
        // Pass through any other recognized events
        _ => Some(RunEvent {
            run_id: run_id.to_string(),
            seq,
            timestamp: now,
            event_type: RunEventType::TextChunk,
            data: value,
        }),
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}... ({} chars truncated)", &s[..max], s.len() - max)
    }
}

fn is_anthropic_harness(metadata: &std::collections::HashMap<String, String>) -> bool {
    metadata
        .get("custom_harness")
        .is_some_and(|h| h == "anthropic")
}

// ---------------------------------------------------------------------------
// Claude CLI stream-json event parsing
// ---------------------------------------------------------------------------

fn parse_claude_stream_event(line: &str, run_id: &str, seq: u32) -> Option<RunEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    let event_type_str = value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let now = Utc::now().to_rfc3339();

    match event_type_str {
        "assistant" => {
            let message = value.get("message")?;
            let content = message.get("content").and_then(|c| c.as_array())?;
            let block = content.last()?;
            let block_type = block
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            match block_type {
                "thinking" => {
                    let text = block
                        .get("thinking")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    Some(RunEvent {
                        run_id: run_id.to_string(),
                        seq,
                        timestamp: now,
                        event_type: RunEventType::Thinking,
                        data: serde_json::json!({ "text": truncate_str(text, 2000) }),
                    })
                }
                "tool_use" => {
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let tool_id = block.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                    let input = block.get("input");
                    Some(RunEvent {
                        run_id: run_id.to_string(),
                        seq,
                        timestamp: now,
                        event_type: RunEventType::ToolCall,
                        data: serde_json::json!({
                            "name": name,
                            "call_id": tool_id,
                            "arguments": input,
                            "phase": "completed",
                        }),
                    })
                }
                "text" => {
                    let text = block
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    Some(RunEvent {
                        run_id: run_id.to_string(),
                        seq,
                        timestamp: now,
                        event_type: RunEventType::AgentMessage,
                        data: serde_json::json!({
                            "text": text,
                            "phase": "completed",
                        }),
                    })
                }
                _ => None,
            }
        }
        "user" => {
            let message = value.get("message")?;
            let content = message.get("content").and_then(|c| c.as_array())?;
            let block = content.last()?;
            let block_type = block
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            if block_type == "tool_result" {
                let tool_id = block
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let output = block
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let is_error = block
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Some(RunEvent {
                    run_id: run_id.to_string(),
                    seq,
                    timestamp: now,
                    event_type: RunEventType::ToolResult,
                    data: serde_json::json!({
                        "call_id": tool_id,
                        "output": truncate_str(output, 4000),
                        "is_error": is_error,
                    }),
                })
            } else {
                None
            }
        }
        "result" => {
            let usage = value.get("usage");
            let cost = value.get("total_cost_usd");
            let duration = value.get("duration_ms");
            let num_turns = value.get("num_turns");
            Some(RunEvent {
                run_id: run_id.to_string(),
                seq,
                timestamp: now,
                event_type: RunEventType::Completed,
                data: serde_json::json!({
                    "usage": usage,
                    "total_cost_usd": cost,
                    "duration_ms": duration,
                    "num_turns": num_turns,
                }),
            })
        }
        _ => None,
    }
}

/// Extract the final result text from Claude CLI stream-json output.
fn extract_claude_result_text(jsonl: &str) -> Option<String> {
    for line in jsonl.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let event_type = value
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if event_type == "result" {
            return value
                .get("result")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Mock runner
// ---------------------------------------------------------------------------

async fn run_mock(request: RunRequest) -> Result<RunResult> {
    let started = Instant::now();
    let text = if let Some(target) = parse_handoff(&request.prompt) {
        format!("[@{target}: mock handoff from @{}]", request.agent_id)
    } else if request.prompt.contains("[mock-send-file]") {
        let path = std::path::Path::new(&request.working_directory).join("mock-output.txt");
        fs::write(&path, format!("mock file from {}\n", request.agent_id))
            .await
            .with_context(|| format!("failed to write mock output file: {}", path.display()))?;
        format!(
            "mock response from @{}\n\n[send_file: {}]",
            request.agent_id,
            path.canonicalize().unwrap_or(path).display()
        )
    } else {
        format!(
            "mock response from @{} (continue_session={})\n\n{}",
            request.agent_id, request.continue_session, request.prompt
        )
    };

    Ok(RunResult {
        stdout: text.clone(),
        text,
        stderr: String::new(),
        exit_code: 0,
        duration_ms: started.elapsed().as_millis(),
        metadata: request.metadata.clone(),
    })
}

// ---------------------------------------------------------------------------
// Command builders
// ---------------------------------------------------------------------------

fn build_claude_command(request: &RunRequest) -> (String, Vec<String>) {
    let mut args = vec!["--dangerously-skip-permissions".to_string()];
    if !request.model.trim().is_empty() {
        args.push("--model".to_string());
        args.push(request.model.clone());
    }
    if let Some(effort) = request.think_level.to_claude_effort() {
        args.push("--effort".to_string());
        args.push(effort.to_string());
    }
    if let Some(system_prompt) = request.metadata.get("system_preamble") {
        if !system_prompt.trim().is_empty() {
            args.push("--system-prompt".to_string());
            args.push(system_prompt.clone());
        }
    }
    if request.continue_session {
        args.push("-c".to_string());
    }
    args.push("--output-format".to_string());
    args.push("stream-json".to_string());
    args.push("--verbose".to_string());
    args.push("-p".to_string());
    args.push(request.prompt.clone());

    ("claude".to_string(), args)
}

fn build_codex_command(request: &RunRequest) -> (String, Vec<String>) {
    use domain::ThinkLevel;

    let mut args = vec!["exec".to_string()];
    if request.continue_session {
        args.push("resume".to_string());
        args.push("--last".to_string());
    }
    if !request.model.trim().is_empty() {
        args.push("--model".to_string());
        args.push(request.model.clone());
    }
    let codex_effort = match request.think_level {
        ThinkLevel::Off | ThinkLevel::Minimal | ThinkLevel::Low => "low",
        ThinkLevel::Medium | ThinkLevel::Adaptive => "medium",
        ThinkLevel::High => "high",
        ThinkLevel::Xhigh => "high",
    };
    args.push("-c".to_string());
    args.push(format!("reasoning_effort=\"{codex_effort}\""));
    args.push("--skip-git-repo-check".to_string());
    args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
    args.push("--json".to_string());

    // Codex CLI has no --system-prompt flag, so prepend the preamble to the user prompt.
    let prompt = match request.metadata.get("system_preamble") {
        Some(preamble) if !preamble.trim().is_empty() => {
            format!(
                "<system-instructions>\n{}\n</system-instructions>\n\n{}",
                preamble.trim(),
                request.prompt
            )
        }
        _ => request.prompt.clone(),
    };
    args.push(prompt);

    ("codex".to_string(), args)
}

fn build_custom_command(
    request: &RunRequest,
) -> Result<(String, Vec<String>, Vec<(String, String)>)> {
    let harness = request
        .metadata
        .get("custom_harness")
        .map(String::as_str)
        .unwrap_or("openai");
    let base_url = request
        .metadata
        .get("custom_base_url")
        .cloned()
        .ok_or_else(|| anyhow!("custom provider missing base_url"))?;
    let api_key = request
        .metadata
        .get("custom_api_key")
        .cloned()
        .ok_or_else(|| anyhow!("custom provider missing api_key"))?;

    match harness {
        "anthropic" => Ok((
            "claude".to_string(),
            build_claude_command(request).1,
            vec![
                ("ANTHROPIC_BASE_URL".to_string(), base_url),
                ("ANTHROPIC_API_KEY".to_string(), api_key),
            ],
        )),
        "openai" => Ok((
            "codex".to_string(),
            build_codex_command(request).1,
            vec![
                ("OPENAI_BASE_URL".to_string(), base_url),
                ("OPENAI_API_KEY".to_string(), api_key),
            ],
        )),
        other => Err(anyhow!("unsupported custom provider harness: {other}")),
    }
}

fn extract_codex_text(jsonl: &str) -> Option<String> {
    let mut latest = None;

    for line in jsonl.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        let event_type = value
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if event_type != "item.completed" {
            continue;
        }

        let item = value.get("item")?;
        let item_type = item
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if item_type != "agent_message" {
            continue;
        }

        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
            latest = Some(text.to_string());
        }
    }

    latest
}

fn parse_handoff(prompt: &str) -> Option<String> {
    let marker = "[mock-handoff:";
    let start = prompt.find(marker)?;
    let rest = &prompt[start + marker.len()..];
    let end = rest.find(']')?;
    let target = rest[..end].trim();
    if target.is_empty() {
        None
    } else {
        Some(target.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_codex_agent_message_completed() {
        let line =
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"Hello world"}}"#;
        let event = parse_codex_jsonl_event(line, "run-1", 1).unwrap();
        assert_eq!(event.event_type, RunEventType::AgentMessage);
        assert_eq!(event.data["text"], "Hello world");
        assert_eq!(event.data["phase"], "completed");
    }

    #[test]
    fn parse_codex_function_call_created() {
        let line = r#"{"type":"item.created","item":{"type":"function_call","name":"shell","call_id":"c1"}}"#;
        let event = parse_codex_jsonl_event(line, "run-1", 1).unwrap();
        assert_eq!(event.event_type, RunEventType::ToolCall);
        assert_eq!(event.data["name"], "shell");
        assert_eq!(event.data["phase"], "start");
    }

    #[test]
    fn parse_codex_function_call_completed() {
        let line = r#"{"type":"item.completed","item":{"type":"function_call","name":"shell","call_id":"c1","arguments":"{\"cmd\":\"ls\"}"}}"#;
        let event = parse_codex_jsonl_event(line, "run-1", 2).unwrap();
        assert_eq!(event.event_type, RunEventType::ToolCall);
        assert_eq!(event.data["phase"], "completed");
        assert_eq!(event.data["arguments"], "{\"cmd\":\"ls\"}");
    }

    #[test]
    fn parse_codex_function_call_output() {
        let line = r#"{"type":"item.completed","item":{"type":"function_call_output","call_id":"c1","output":"file1.txt\nfile2.txt"}}"#;
        let event = parse_codex_jsonl_event(line, "run-1", 3).unwrap();
        assert_eq!(event.event_type, RunEventType::ToolResult);
        assert_eq!(event.data["call_id"], "c1");
    }

    #[test]
    fn parse_codex_response_completed() {
        let line = r#"{"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let event = parse_codex_jsonl_event(line, "run-1", 4).unwrap();
        assert_eq!(event.event_type, RunEventType::Completed);
        assert_eq!(event.data["usage"]["input_tokens"], 100);
    }

    #[test]
    fn parse_codex_invalid_json_returns_none() {
        let event = parse_codex_jsonl_event("not json", "run-1", 1);
        assert!(event.is_none());
    }

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_long() {
        let result = truncate_str("hello world", 5);
        assert!(result.starts_with("hello"));
        assert!(result.contains("truncated"));
    }

    #[test]
    fn extract_codex_text_works() {
        let jsonl = r#"{"type":"item.completed","item":{"type":"function_call","name":"shell"}}
{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#;
        assert_eq!(extract_codex_text(jsonl), Some("done".to_string()));
    }

    // Claude stream-json tests

    #[test]
    fn parse_claude_assistant_text() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello world"}]}}"#;
        let event = parse_claude_stream_event(line, "run-1", 1).unwrap();
        assert_eq!(event.event_type, RunEventType::AgentMessage);
        assert_eq!(event.data["text"], "hello world");
    }

    #[test]
    fn parse_claude_thinking() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think about this...","signature":"sig"}]}}"#;
        let event = parse_claude_stream_event(line, "run-1", 1).unwrap();
        assert_eq!(event.event_type, RunEventType::Thinking);
        assert!(event.data["text"]
            .as_str()
            .unwrap()
            .contains("Let me think"));
    }

    #[test]
    fn parse_claude_tool_use() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_123","name":"Read","input":{"file_path":"/tmp/test"}}]}}"#;
        let event = parse_claude_stream_event(line, "run-1", 2).unwrap();
        assert_eq!(event.event_type, RunEventType::ToolCall);
        assert_eq!(event.data["name"], "Read");
        assert_eq!(event.data["call_id"], "toolu_123");
    }

    #[test]
    fn parse_claude_tool_result() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"file contents here","is_error":false,"tool_use_id":"toolu_123"}]}}"#;
        let event = parse_claude_stream_event(line, "run-1", 3).unwrap();
        assert_eq!(event.event_type, RunEventType::ToolResult);
        assert_eq!(event.data["call_id"], "toolu_123");
        assert!(event.data["output"]
            .as_str()
            .unwrap()
            .contains("file contents"));
    }

    #[test]
    fn parse_claude_result() {
        let line = r#"{"type":"result","subtype":"success","result":"final answer","total_cost_usd":0.05,"duration_ms":3000,"num_turns":2,"usage":{"input_tokens":100,"output_tokens":50}}"#;
        let event = parse_claude_stream_event(line, "run-1", 4).unwrap();
        assert_eq!(event.event_type, RunEventType::Completed);
        assert_eq!(event.data["total_cost_usd"], 0.05);
        assert_eq!(event.data["num_turns"], 2);
    }

    #[test]
    fn parse_claude_system_init_skipped() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc"}"#;
        let event = parse_claude_stream_event(line, "run-1", 1);
        assert!(event.is_none());
    }

    #[test]
    fn extract_claude_result_text_works() {
        let jsonl = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}
{"type":"result","subtype":"success","result":"final text","duration_ms":1000}"#;
        assert_eq!(
            extract_claude_result_text(jsonl),
            Some("final text".to_string())
        );
    }

    #[test]
    fn build_execution_envs_includes_browser_context() {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("browser_profile".to_string(), "reviewer".to_string());
        metadata.insert("browser_cdp_port".to_string(), "9411".to_string());
        metadata.insert(
            "browser_profile_dir".to_string(),
            "/tmp/reviewer-profile".to_string(),
        );
        metadata.insert("browser_display".to_string(), ":12".to_string());
        metadata.insert("browser_kasm_port".to_string(), "8442".to_string());
        metadata.insert(
            "browser_view_path".to_string(),
            "/view/reviewer".to_string(),
        );

        let request = RunRequest {
            run_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            task_id: "00000000-0000-0000-0000-000000000002".parse().unwrap(),
            session_key: "session".to_string(),
            agent_id: "reviewer".to_string(),
            provider: ProviderKind::Mock,
            model: "mock".to_string(),
            think_level: domain::ThinkLevel::Low,
            working_directory: ".".to_string(),
            prompt: "hello".to_string(),
            continue_session: false,
            metadata,
        };

        let envs = build_execution_envs(&request);
        let env_map: std::collections::HashMap<_, _> = envs.into_iter().collect();
        assert_eq!(env_map.get("DISPLAY").map(String::as_str), Some(":12"));
        assert_eq!(
            env_map.get("AGENT_BROWSER_DISPLAY").map(String::as_str),
            Some(":12")
        );
        assert_eq!(
            env_map.get("AGENT_BROWSER_PROFILE").map(String::as_str),
            Some("reviewer")
        );
        assert_eq!(
            env_map.get("AGENT_BROWSER_CDP_PORT").map(String::as_str),
            Some("9411")
        );
        assert_eq!(
            env_map.get("AGENT_BROWSER_PROFILE_DIR").map(String::as_str),
            Some("/tmp/reviewer-profile")
        );
        assert_eq!(
            env_map.get("AGENT_BROWSER_KASM_PORT").map(String::as_str),
            Some("8442")
        );
        assert_eq!(
            env_map.get("AGENT_BROWSER_VIEW_PATH").map(String::as_str),
            Some("/view/reviewer")
        );
    }
}
