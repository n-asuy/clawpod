use std::collections::HashMap;
use std::path::Path;

use agent::{ensure_agent_workspace, resolve_heartbeat_file};
use anyhow::Result;
use chrono::{Local, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use config::{ResolvedHeartbeatConfig, RuntimeConfig};
use observer::{mark_component_error, mark_component_ok, FileEventSink};
use queue::{enqueue_message, EnqueueMessage};
use serde_json::json;
use store::StateStore;
use tokio::fs;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};
use uuid::Uuid;

const HEARTBEAT_CHANNEL: &str = "heartbeat";
const SCHEDULER_TICK_SEC: u64 = 10;

pub async fn run_loop(config: RuntimeConfig, store: StateStore, sink: FileEventSink) -> Result<()> {
    let mut schedule = seed_schedule(&config);
    info!(agents = schedule.len(), "heartbeat loop started");

    emit_runtime_event(
        &store,
        &sink,
        "heartbeat_started",
        json!({
            "tick_sec": SCHEDULER_TICK_SEC,
            "agents": schedule.keys().cloned().collect::<Vec<_>>(),
        }),
    );

    loop {
        sleep(Duration::from_secs(SCHEDULER_TICK_SEC)).await;

        let outcome = run_cycle(&config, &store, &sink, &mut schedule).await;
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
                "tick_sec": SCHEDULER_TICK_SEC,
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

fn seed_schedule(config: &RuntimeConfig) -> HashMap<String, i64> {
    let now = Utc::now().timestamp_millis();
    let mut schedule = HashMap::new();
    for agent_id in config.heartbeat_agent_ids() {
        if let Some(heartbeat) = config.resolve_heartbeat_config(&agent_id) {
            if heartbeat.enabled && heartbeat.interval_sec > 0 {
                schedule.insert(agent_id, now + (heartbeat.interval_sec as i64 * 1000));
            }
        }
    }
    schedule
}

async fn run_cycle(
    config: &RuntimeConfig,
    store: &StateStore,
    sink: &FileEventSink,
    schedule: &mut HashMap<String, i64>,
) -> CycleOutcome {
    let mut outcome = CycleOutcome::default();
    let now = Utc::now().timestamp_millis();

    for agent_id in config.heartbeat_agent_ids() {
        let Some(heartbeat) = config.resolve_heartbeat_config(&agent_id) else {
            outcome.skipped += 1;
            continue;
        };
        if !heartbeat.enabled || heartbeat.interval_sec == 0 {
            outcome.skipped += 1;
            continue;
        }

        let next_due = schedule
            .entry(agent_id.clone())
            .or_insert(now + (heartbeat.interval_sec as i64 * 1000));
        if now < *next_due {
            continue;
        }
        *next_due = now + (heartbeat.interval_sec as i64 * 1000);

        if !is_within_active_hours(heartbeat.active_hours.as_ref(), now) {
            outcome.skipped += 1;
            continue;
        }

        let Some(agent) = config.agents.get(&agent_id) else {
            outcome.errors += 1;
            continue;
        };
        let agent_root = config.resolve_agent_workdir(&agent_id);
        if let Err(err) =
            ensure_agent_workspace(&agent_id, agent, &config.agents, &config.teams, &agent_root)
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

        let prompt = match load_heartbeat_prompt(&agent_root, &heartbeat).await {
            Ok(Some(prompt)) => prompt,
            Ok(None) => {
                outcome.skipped += 1;
                continue;
            }
            Err(err) => {
                outcome.errors += 1;
                warn!(agent_id, "failed to resolve heartbeat prompt: {err:#}");
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
                sender: heartbeat.sender.clone(),
                sender_id: HEARTBEAT_CHANNEL.to_string(),
                message: prompt,
                message_id: message_id.clone(),
                timestamp_ms: now,
                chat_type: domain::ChatType::Direct,
                peer_id: agent_id.clone(),
                account_id: None,
                pre_routed_agent: Some(agent_id.clone()),
                from_agent: None,
                files: vec![],
                chain_depth: 0,
                run_kind: domain::RunKind::Heartbeat,
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

async fn load_heartbeat_prompt(
    agent_root: &Path,
    heartbeat: &ResolvedHeartbeatConfig,
) -> Result<Option<String>> {
    let path = resolve_heartbeat_file(agent_root);
    if path.exists() {
        let raw = fs::read_to_string(&path).await?;
        if is_heartbeat_content_effectively_empty(&raw) {
            return Ok(None);
        }
    }

    Ok(Some(append_heartbeat_workspace_path_hint(
        &heartbeat.prompt,
        &path,
    )))
}

fn append_heartbeat_workspace_path_hint(prompt: &str, path: &Path) -> String {
    if !prompt.to_ascii_lowercase().contains("heartbeat.md") {
        return prompt.to_string();
    }
    let path = path.display().to_string().replace('\\', "/");
    let hint = format!(
        "When reading HEARTBEAT.md, use workspace file {} (exact case). Do not read docs/heartbeat.md.",
        path
    );
    if prompt.contains(&hint) {
        prompt.to_string()
    } else {
        format!("{prompt}\n{hint}")
    }
}

fn is_heartbeat_content_effectively_empty(raw: &str) -> bool {
    let without_comments = strip_html_comments(raw);
    for line in without_comments.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if RegexLikeHeader::matches(line) {
            continue;
        }
        if ListPlaceholder::matches(line) {
            continue;
        }
        return false;
    }
    true
}

struct RegexLikeHeader;
impl RegexLikeHeader {
    fn matches(line: &str) -> bool {
        let mut chars = line.chars();
        let mut hash_count = 0;
        while matches!(chars.next(), Some('#')) {
            hash_count += 1;
        }
        hash_count > 0
            && line
                .chars()
                .nth(hash_count)
                .map_or(true, char::is_whitespace)
    }
}

struct ListPlaceholder;
impl ListPlaceholder {
    fn matches(line: &str) -> bool {
        matches!(line, "-" | "*" | "+") || line.starts_with("- [ ]") || line.starts_with("* [ ]")
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

fn parse_active_hours_minutes(raw: &str, allow_24: bool) -> Option<u32> {
    let trimmed = raw.trim();
    if trimmed == "24:00" {
        return allow_24.then_some(24 * 60);
    }
    let (hour, minute) = trimmed.split_once(':')?;
    let hour = hour.parse::<u32>().ok()?;
    let minute = minute.parse::<u32>().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(hour * 60 + minute)
}

fn resolve_minutes_in_timezone(now_ms: i64, timezone: Option<&str>) -> Option<u32> {
    let instant = Utc.timestamp_millis_opt(now_ms).single()?;
    let timezone = timezone.map(str::trim).filter(|value| !value.is_empty());
    if let Some(timezone) = timezone {
        if timezone.eq_ignore_ascii_case("local") {
            let local = instant.with_timezone(&Local);
            return Some(local.hour() * 60 + local.minute());
        }
        let tz = timezone.parse::<Tz>().ok()?;
        let zoned = instant.with_timezone(&tz);
        return Some(zoned.hour() * 60 + zoned.minute());
    }

    let local = instant.with_timezone(&Local);
    Some(local.hour() * 60 + local.minute())
}

fn is_within_active_hours(
    active_hours: Option<&domain::HeartbeatActiveHours>,
    now_ms: i64,
) -> bool {
    let Some(active_hours) = active_hours else {
        return true;
    };
    let Some(start) = parse_active_hours_minutes(&active_hours.start, false) else {
        return true;
    };
    let Some(end) = parse_active_hours_minutes(&active_hours.end, true) else {
        return true;
    };
    if start == end {
        return false;
    }
    let Some(current) = resolve_minutes_in_timezone(now_ms, active_hours.timezone.as_deref())
    else {
        return true;
    };
    if end > start {
        current >= start && current < end
    } else {
        current >= start || current < end
    }
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
    use super::{is_heartbeat_content_effectively_empty, parse_active_hours_minutes};

    #[test]
    fn empty_or_comment_only_prompt_is_disabled() {
        let raw = r#"# Heartbeat

<!-- Leave empty to disable -->
"#;

        assert!(is_heartbeat_content_effectively_empty(raw));
    }

    #[test]
    fn actionable_prompt_stays_enabled() {
        let raw = r#"# Heartbeat

Review active work.
- Post a short status update if needed.
"#;

        assert!(!is_heartbeat_content_effectively_empty(raw));
    }

    #[test]
    fn parses_active_hours_minutes() {
        assert_eq!(parse_active_hours_minutes("09:30", false), Some(570));
        assert_eq!(parse_active_hours_minutes("24:00", true), Some(1440));
        assert_eq!(parse_active_hours_minutes("24:00", false), None);
    }
}
