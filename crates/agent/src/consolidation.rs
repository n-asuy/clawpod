use std::path::Path;

use anyhow::{anyhow, Result};
use domain::ProviderKind;
use serde::Deserialize;
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use tracing::debug;

/// Minimum message length (in chars) to trigger consolidation.
/// Short messages like "ok" or "thanks" rarely contain memorable facts.
const MIN_MESSAGE_CHARS: usize = 20;

/// Maximum chars of the conversation turn passed to the consolidation prompt.
/// Keeps token usage predictable.
const MAX_TURN_CHARS: usize = 4000;

/// Timeout for the consolidation CLI process.
const CONSOLIDATION_TIMEOUT: Duration = Duration::from_secs(30);

const CONSOLIDATION_PROMPT: &str = r#"You are a memory consolidation engine. Given a conversation turn between a user and an assistant, extract:

1. "memory_update": Any NEW facts, preferences, decisions, or commitments worth remembering long-term. Return null if nothing new was learned. Be selective — only extract information that would be valuable in future conversations.

Respond ONLY with valid JSON: {"memory_update": "..." or null}
Do not include any text outside the JSON object.

Conversation turn:
"#;

#[derive(Debug, Deserialize)]
struct ConsolidationResult {
    memory_update: Option<String>,
}

/// Returns true if the message is worth consolidating.
pub fn should_consolidate(user_message: &str, channel: &str) -> bool {
    if channel == "heartbeat" || channel == "chatroom" {
        return false;
    }
    user_message.chars().count() >= MIN_MESSAGE_CHARS
}

/// Run memory consolidation as a background CLI call.
///
/// Spawns the appropriate CLI (`claude` or `codex`) with a lightweight model
/// to extract durable facts from the conversation turn, then appends them to
/// `memory/daily.md`.
pub async fn consolidate_turn(
    user_message: &str,
    assistant_response: &str,
    memory_dir: &Path,
    provider: ProviderKind,
) -> Result<()> {
    let turn_text = format!("User: {user_message}\nAssistant: {assistant_response}");
    let truncated = truncate_at_char_boundary(&turn_text, MAX_TURN_CHARS);
    let full_prompt = format!("{CONSOLIDATION_PROMPT}{truncated}");

    let output = match provider {
        ProviderKind::Openai => {
            timeout(
                CONSOLIDATION_TIMEOUT,
                Command::new("codex")
                    .args([
                        "exec",
                        "--model",
                        "gpt-5.4-mini",
                        "--dangerously-bypass-approvals-and-sandbox",
                        "--json",
                        &full_prompt,
                    ])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
        }
        // Anthropic, Custom, Mock — all fall back to claude CLI.
        _ => {
            timeout(
                CONSOLIDATION_TIMEOUT,
                Command::new("claude")
                    .args([
                        "--dangerously-skip-permissions",
                        "--model",
                        "haiku",
                        "--bare",
                        "-p",
                        &full_prompt,
                    ])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
        }
    }
    .map_err(|_| anyhow!("consolidation timed out"))?
    .map_err(|e| anyhow!("consolidation process failed: {e}"))?;

    if !output.status.success() {
        return Err(anyhow!(
            "consolidation exited with code {}",
            output.status.code().unwrap_or(-1)
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout);

    // Codex returns JSONL; extract the last agent_message text.
    let response_text = if provider == ProviderKind::Openai {
        extract_codex_text(&raw).unwrap_or_else(|| raw.to_string())
    } else {
        raw.to_string()
    };

    let result = parse_consolidation_response(&response_text);

    if let Some(ref update) = result.memory_update {
        if !update.trim().is_empty() {
            append_daily_memory(memory_dir, update)?;
        }
    }

    debug!(
        has_update = result.memory_update.is_some(),
        "memory consolidation complete"
    );
    Ok(())
}

/// Extract text from Codex JSONL output (same format as runner).
fn extract_codex_text(jsonl: &str) -> Option<String> {
    let mut latest = None;
    for line in jsonl.lines() {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("type").and_then(|v| v.as_str()) != Some("item.completed") {
            continue;
        }
        let item = value.get("item")?;
        if item.get("type").and_then(|v| v.as_str()) != Some("agent_message") {
            continue;
        }
        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
            latest = Some(text.to_string());
        }
    }
    latest
}

fn parse_consolidation_response(raw: &str) -> ConsolidationResult {
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str(cleaned).unwrap_or(ConsolidationResult {
        memory_update: None,
    })
}

/// Append a memory update to `memory/daily.md`, creating it with frontmatter
/// if it does not exist yet.
fn append_daily_memory(memory_dir: &Path, content: &str) -> Result<()> {
    let daily_path = memory_dir.join("daily.md");
    let date = chrono::Local::now().format("%Y-%m-%d %H:%M");

    let entry = format!("\n### {date}\n{content}\n");

    if daily_path.exists() {
        let mut existing = std::fs::read_to_string(&daily_path)?;
        existing.push_str(&entry);
        std::fs::write(&daily_path, existing)?;
    } else {
        let initial = format!(
            "---\nname: daily\nsummary: Auto-consolidated conversation insights\n---\n\n# Daily Consolidation\n{entry}"
        );
        std::fs::write(&daily_path, initial)?;
    }

    Ok(())
}

/// Truncate a string at the given char limit, respecting char boundaries.
fn truncate_at_char_boundary(s: &str, max_chars: usize) -> &str {
    if s.chars().count() <= max_chars {
        return s;
    }
    let end = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_consolidate_skips_short_messages() {
        assert!(!should_consolidate("ok", "slack"));
        assert!(!should_consolidate("thanks", "slack"));
    }

    #[test]
    fn should_consolidate_accepts_long_messages() {
        assert!(should_consolidate(
            "Please remember that I prefer Rust over Go for this project",
            "slack"
        ));
    }

    #[test]
    fn should_consolidate_skips_heartbeat() {
        assert!(!should_consolidate(
            "a long enough message for consolidation",
            "heartbeat"
        ));
    }

    #[test]
    fn should_consolidate_skips_chatroom() {
        assert!(!should_consolidate(
            "a long enough message for consolidation",
            "chatroom"
        ));
    }

    #[test]
    fn parse_consolidation_valid_with_update() {
        let raw = r#"{"memory_update": "User prefers Rust over Go"}"#;
        let result = parse_consolidation_response(raw);
        assert_eq!(
            result.memory_update.as_deref(),
            Some("User prefers Rust over Go")
        );
    }

    #[test]
    fn parse_consolidation_valid_null() {
        let raw = r#"{"memory_update": null}"#;
        let result = parse_consolidation_response(raw);
        assert!(result.memory_update.is_none());
    }

    #[test]
    fn parse_consolidation_with_code_fence() {
        let raw = "```json\n{\"memory_update\": \"some fact\"}\n```";
        let result = parse_consolidation_response(raw);
        assert_eq!(result.memory_update.as_deref(), Some("some fact"));
    }

    #[test]
    fn parse_consolidation_malformed_returns_none() {
        let raw = "I couldn't parse that";
        let result = parse_consolidation_response(raw);
        assert!(result.memory_update.is_none());
    }

    #[test]
    fn truncate_at_char_boundary_short_string() {
        assert_eq!(truncate_at_char_boundary("hello", 10), "hello");
    }

    #[test]
    fn truncate_at_char_boundary_long_string() {
        let s = "abcdefghij";
        assert_eq!(truncate_at_char_boundary(s, 5), "abcde");
    }

    #[test]
    fn truncate_at_char_boundary_multibyte() {
        let s = "あいうえお";
        assert_eq!(truncate_at_char_boundary(s, 3), "あいう");
    }

    #[test]
    fn append_daily_memory_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let memory_dir = dir.path();
        std::fs::create_dir_all(memory_dir).unwrap();

        append_daily_memory(memory_dir, "User prefers Rust").unwrap();

        let content = std::fs::read_to_string(memory_dir.join("daily.md")).unwrap();
        assert!(content.contains("name: daily"));
        assert!(content.contains("summary: Auto-consolidated"));
        assert!(content.contains("User prefers Rust"));
    }

    #[test]
    fn append_daily_memory_appends_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        let memory_dir = dir.path();
        std::fs::create_dir_all(memory_dir).unwrap();

        append_daily_memory(memory_dir, "First fact").unwrap();
        append_daily_memory(memory_dir, "Second fact").unwrap();

        let content = std::fs::read_to_string(memory_dir.join("daily.md")).unwrap();
        assert!(content.contains("First fact"));
        assert!(content.contains("Second fact"));
    }
}
