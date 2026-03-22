use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use domain::{ProviderKind, RunRequest, RunResult, Runner};
use serde_json::Value;
use tokio::fs;
use tokio::process::Command;
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
        let started = Instant::now();

        if matches!(request.provider, ProviderKind::Mock) {
            return run_mock(request).await;
        }

        let (program, args, envs) = match request.provider {
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

        let mut command = Command::new(&program);
        command.args(args).current_dir(&request.working_directory);
        for (key, value) in envs {
            command.env(key, value);
        }

        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn runner command: {program}"))?;

        let child_stdout = child.stdout.take();
        let child_stderr = child.stderr.take();
        let read_output = async {
            let stdout = match child_stdout {
                Some(mut s) => {
                    let mut buf = Vec::new();
                    tokio::io::AsyncReadExt::read_to_end(&mut s, &mut buf).await?;
                    buf
                }
                None => Vec::new(),
            };
            let stderr = match child_stderr {
                Some(mut s) => {
                    let mut buf = Vec::new();
                    tokio::io::AsyncReadExt::read_to_end(&mut s, &mut buf).await?;
                    buf
                }
                None => Vec::new(),
            };
            let status = child.wait().await?;
            Ok::<_, std::io::Error>(std::process::Output {
                status,
                stdout,
                stderr,
            })
        };

        let output = timeout(self.timeout, read_output)
            .await
            .map_err(|_| anyhow!("runner timed out after {}s", self.timeout.as_secs()))?
            .with_context(|| format!("failed to execute runner command: {program}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        let mut text = stdout.clone();
        if matches!(request.provider, ProviderKind::Openai) {
            text = extract_codex_text(&stdout).unwrap_or_else(|| {
                warn!("codex json parse fallback to raw stdout");
                stdout.clone()
            });
        }

        Ok(RunResult {
            text: text.trim().to_string(),
            stdout,
            stderr,
            exit_code,
            duration_ms: started.elapsed().as_millis(),
            metadata: request.metadata.clone(),
        })
    }
}

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
