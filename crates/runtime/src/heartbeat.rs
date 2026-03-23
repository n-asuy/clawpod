use std::path::Path;

use agent::ensure_agent_workspace;
use anyhow::Result;
use chrono::Utc;
use config::RuntimeConfig;
use observer::{mark_component_error, mark_component_ok, FileEventSink};
use queue::{enqueue_message, EnqueueMessage};
use serde_json::json;
use store::StateStore;
use tokio::fs;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};
use uuid::Uuid;

const HEARTBEAT_CHANNEL: &str = "heartbeat";
const MIN_HEARTBEAT_INTERVAL_SEC: u64 = 10;

pub async fn run_loop(config: RuntimeConfig, store: StateStore, sink: FileEventSink) -> Result<()> {
    let interval_sec = config
        .heartbeat
        .interval_sec
        .max(MIN_HEARTBEAT_INTERVAL_SEC);
    info!(interval_sec, "heartbeat loop started");

    emit_runtime_event(
        &store,
        &sink,
        "heartbeat_started",
        json!({ "interval_sec": interval_sec }),
    );

    loop {
        sleep(Duration::from_secs(interval_sec)).await;

        let outcome = run_cycle(&config, &store, &sink).await;
        if outcome.errors == 0 {
            mark_component_ok("heartbeat");
        } else {
            mark_component_error(
                "heartbeat",
                format!("{} agent heartbeat dispatch errors", outcome.errors),
            );
        }

        emit_runtime_event(
            &store,
            &sink,
            "heartbeat_tick",
            json!({
                "interval_sec": interval_sec,
                "queued_agents": outcome.queued,
                "skipped_agents": outcome.skipped,
                "errored_agents": outcome.errors,
            }),
        );
    }
}

#[derive(Default)]
struct CycleOutcome {
    queued: usize,
    skipped: usize,
    errors: usize,
}

async fn run_cycle(
    config: &RuntimeConfig,
    store: &StateStore,
    sink: &FileEventSink,
) -> CycleOutcome {
    let mut outcome = CycleOutcome::default();

    for (agent_id, agent) in &config.agents {
        let agent_root = config.resolve_agent_workdir(agent_id);
        if let Err(err) =
            ensure_agent_workspace(agent_id, agent, &config.agents, &config.teams, &agent_root)
        {
            outcome.errors += 1;
            warn!(agent_id, "heartbeat workspace bootstrap failed: {err:#}");
            emit_runtime_event(
                store,
                sink,
                "heartbeat_agent_error",
                json!({
                    "agent_id": agent_id,
                    "error": err.to_string(),
                }),
            );
            continue;
        }

        let prompt = match load_heartbeat_prompt(&agent_root).await {
            Ok(Some(prompt)) => prompt,
            Ok(None) => {
                outcome.skipped += 1;
                continue;
            }
            Err(err) => {
                outcome.errors += 1;
                warn!(agent_id, "failed to load heartbeat prompt: {err:#}");
                emit_runtime_event(
                    store,
                    sink,
                    "heartbeat_agent_error",
                    json!({
                        "agent_id": agent_id,
                        "error": err.to_string(),
                    }),
                );
                continue;
            }
        };

        let message_id = format!("heartbeat-{}", Uuid::new_v4().simple());
        let queued = enqueue_message(
            config,
            EnqueueMessage {
                channel: HEARTBEAT_CHANNEL.to_string(),
                sender: config.heartbeat.sender.clone(),
                sender_id: HEARTBEAT_CHANNEL.to_string(),
                message: prompt.clone(),
                message_id: message_id.clone(),
                timestamp_ms: Utc::now().timestamp_millis(),
                chat_type: domain::ChatType::Direct,
                peer_id: agent_id.clone(),
                account_id: None,
                pre_routed_agent: Some(agent_id.clone()),
                from_agent: None,
                files: vec![],
                chain_depth: 0,
            },
        )
        .await;

        match queued {
            Ok(path) => {
                outcome.queued += 1;
                emit_runtime_event(
                    store,
                    sink,
                    "heartbeat_queued",
                    json!({
                        "agent_id": agent_id,
                        "message_id": message_id,
                        "path": path.display().to_string(),
                    }),
                );
            }
            Err(err) => {
                outcome.errors += 1;
                warn!(agent_id, "failed to enqueue heartbeat: {err:#}");
                emit_runtime_event(
                    store,
                    sink,
                    "heartbeat_agent_error",
                    json!({
                        "agent_id": agent_id,
                        "error": err.to_string(),
                    }),
                );
            }
        }
    }

    outcome
}

async fn load_heartbeat_prompt(agent_root: &Path) -> Result<Option<String>> {
    let path = agent_root.join("heartbeat.md");
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path).await?;
    Ok(extract_active_heartbeat_prompt(&raw))
}

fn extract_active_heartbeat_prompt(raw: &str) -> Option<String> {
    let without_comments = strip_html_comments(raw);
    let lines = without_comments
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn strip_html_comments(raw: &str) -> String {
    let mut remaining = raw;
    let mut out = String::new();

    loop {
        let Some(start) = remaining.find("<!--") else {
            out.push_str(remaining);
            break;
        };

        out.push_str(&remaining[..start]);
        let comment_start = start + 4;
        let Some(end_offset) = remaining[comment_start..].find("-->") else {
            break;
        };
        remaining = &remaining[comment_start + end_offset + 3..];
    }

    out
}

fn emit_runtime_event(
    store: &StateStore,
    sink: &FileEventSink,
    event_type: &str,
    payload: serde_json::Value,
) {
    if let Err(err) = sink.emit(event_type, payload.clone()) {
        warn!("failed to emit heartbeat event {event_type}: {err:#}");
    }
    if let Err(err) = store.record_event(event_type, &payload) {
        warn!("failed to persist heartbeat event {event_type}: {err:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::extract_active_heartbeat_prompt;

    #[test]
    fn extracts_prompt_while_ignoring_comments_and_headings() {
        let raw = r#"# Heartbeat

<!-- comment -->

Review active work.
- Post a short status update if needed.
"#;

        let prompt = extract_active_heartbeat_prompt(raw).expect("prompt");
        assert_eq!(
            prompt,
            "Review active work.\n- Post a short status update if needed."
        );
    }

    #[test]
    fn empty_or_comment_only_prompt_is_disabled() {
        let raw = r#"# Heartbeat

<!-- Leave empty to disable -->
"#;

        assert!(extract_active_heartbeat_prompt(raw).is_none());
    }
}
