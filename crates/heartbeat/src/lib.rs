pub mod active_hours;
pub mod dedup;
pub mod delivery;
pub mod indicator;
pub mod normalize;
pub mod policy;
pub mod visibility;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use agent::{ensure_agent_workspace, ensure_session_workspace, PromptContext, SystemPromptBuilder};
use anyhow::{Context, Result};
use chrono::Utc;
use config::RuntimeConfig;
use domain::{
    AgentConfig, HeartbeatDeliveryMode, HeartbeatRunReason, HeartbeatRunStatus, HeartbeatRunView,
    HeartbeatTarget, RunRequest, Runner,
};
use observer::FileEventSink;
use serde_json::json;
use store::StateStore;
use tokio::fs;
use tokio::sync::watch;
use tracing::{info, warn};
use uuid::Uuid;

use active_hours::is_within_active_hours;
use dedup::is_duplicate_heartbeat;
use delivery::resolve_delivery_target;
use indicator::{derive_event_status, resolve_indicator_type};
use normalize::{normalize_heartbeat_output, NormalizeResult};
use policy::{resolve_effective_policy, EffectiveHeartbeatPolicy};

/// Outcome of a single heartbeat cycle (one pass over all agents).
#[derive(Debug, Default)]
pub struct CycleOutcome {
    pub ran: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatLoopSettings {
    pub enabled: bool,
    pub interval_sec: u64,
}

impl HeartbeatLoopSettings {
    pub fn from_config(config: &RuntimeConfig) -> Self {
        Self {
            enabled: config.heartbeat.enabled,
            interval_sec: resolve_tick_interval(config).as_secs(),
        }
    }

    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_sec.max(10))
    }
}

/// Per-agent schedule state, modeled after OpenClaw's HeartbeatAgentState.
#[derive(Debug, Clone)]
struct AgentSchedule {
    interval: std::time::Duration,
    last_run: Option<Instant>,
    next_due: Instant,
}

#[derive(Clone)]
pub struct HeartbeatLoopControl {
    tx: watch::Sender<HeartbeatLoopSettings>,
}

impl HeartbeatLoopControl {
    pub fn new(config: &RuntimeConfig) -> Self {
        let (tx, _rx) = watch::channel(HeartbeatLoopSettings::from_config(config));
        Self { tx }
    }

    pub fn subscribe(&self) -> watch::Receiver<HeartbeatLoopSettings> {
        self.tx.subscribe()
    }

    pub fn update_from_config(&self, config: &RuntimeConfig) -> bool {
        let next = HeartbeatLoopSettings::from_config(config);
        self.tx.send_if_modified(|current| {
            if *current == next {
                false
            } else {
                *current = next;
                true
            }
        })
    }
}

pub struct HeartbeatService {
    config: Arc<RuntimeConfig>,
    runner: Arc<dyn Runner>,
    store: StateStore,
    sink: FileEventSink,
    /// Per-agent schedule state (OpenClaw-style per-agent timers).
    schedules: Mutex<HashMap<String, AgentSchedule>>,
}

impl HeartbeatService {
    pub fn new(
        config: Arc<RuntimeConfig>,
        runner: Arc<dyn Runner>,
        store: StateStore,
        sink: FileEventSink,
    ) -> Self {
        Self {
            config,
            runner,
            store,
            sink,
            schedules: Mutex::new(HashMap::new()),
        }
    }

    /// Run heartbeat for agents that are due (per-agent scheduling).
    pub async fn run_scheduled_cycle(&self) -> CycleOutcome {
        let mut outcome = CycleOutcome::default();
        let now = Instant::now();
        let agents = self.resolve_heartbeat_agents();

        // Rebuild schedule map: add new agents, update intervals, remove stale ones.
        {
            let mut schedules = self.schedules.lock().expect("schedules lock poisoned");
            let agent_ids: std::collections::HashSet<_> =
                agents.iter().map(|(id, _)| id.clone()).collect();

            // Remove agents no longer participating
            schedules.retain(|id, _| agent_ids.contains(id));

            // Add or update agents
            for (agent_id, _) in &agents {
                let interval = self
                    .resolve_effective_policy(agent_id)
                    .map(|p| p.every)
                    .unwrap_or_else(|_| std::time::Duration::from_secs(1800));

                schedules
                    .entry(agent_id.clone())
                    .and_modify(|sched| {
                        // Update interval if config changed; keep next_due relative
                        if sched.interval != interval {
                            sched.interval = interval;
                        }
                    })
                    .or_insert_with(|| AgentSchedule {
                        interval,
                        last_run: None,
                        next_due: now, // due immediately on first appearance
                    });
            }
        }

        for (agent_id, _agent) in &agents {
            let is_due = {
                let schedules = self.schedules.lock().expect("schedules lock poisoned");
                schedules
                    .get(agent_id)
                    .map_or(false, |sched| now >= sched.next_due)
            };

            if !is_due {
                continue;
            }

            info!(agent = %agent_id, "heartbeat due, running");

            match self.run_once(agent_id, HeartbeatRunReason::Scheduled).await {
                Ok(view) => {
                    // Mark as ran and schedule next
                    let mut schedules = self.schedules.lock().expect("schedules lock poisoned");
                    if let Some(sched) = schedules.get_mut(agent_id) {
                        sched.last_run = Some(Instant::now());
                        sched.next_due = Instant::now() + sched.interval;
                    }

                    match view.status.as_str() {
                        "ran" => outcome.ran += 1,
                        "skipped" => outcome.skipped += 1,
                        _ => outcome.failed += 1,
                    }
                }
                Err(err) => {
                    // Still advance the schedule to avoid tight retry loops
                    let mut schedules = self.schedules.lock().expect("schedules lock poisoned");
                    if let Some(sched) = schedules.get_mut(agent_id) {
                        sched.last_run = Some(Instant::now());
                        sched.next_due = Instant::now() + sched.interval;
                    }

                    warn!(agent = %agent_id, error = %err, "heartbeat run failed");
                    outcome.failed += 1;
                }
            }
        }
        outcome
    }

    /// Run heartbeat for a single agent.
    pub async fn run_once(
        &self,
        agent_id: &str,
        reason: HeartbeatRunReason,
    ) -> Result<HeartbeatRunView> {
        let started = Instant::now();
        let started_at = Utc::now().to_rfc3339();

        let agent = self
            .config
            .agents
            .get(agent_id)
            .context("agent not found")?
            .clone();

        let mut policy = self.resolve_effective_policy(agent_id)?;

        // Active hours gating (skip for manual and event-driven runs)
        if reason == HeartbeatRunReason::Scheduled
            && !is_within_active_hours(policy.active_hours.as_ref())
        {
            return self.record_skip(agent_id, &reason, &started_at, started, "quiet-hours");
        }

        // Ensure workspace
        let agent_root = self.config.resolve_agent_workdir(agent_id);
        ensure_agent_workspace(
            agent_id,
            &agent,
            &self.config.agents,
            &self.config.teams,
            &agent_root,
        )?;

        // Load heartbeat.md and parse frontmatter overrides + body.
        let heartbeat_raw = load_heartbeat_md(&agent_root).await;
        let file_settings = heartbeat_raw.as_deref().map(parse_heartbeat_frontmatter);

        // File frontmatter overrides config policy
        if let Some(ref settings) = file_settings {
            if let Some(target) = settings.target {
                policy.target = target;
            }
            if settings.to.is_some() {
                policy.to = settings.to.clone();
            }
        }

        let effective_prompt = match &file_settings {
            Some(settings) if !is_effectively_empty(&settings.body) => settings.body.as_str(),
            _ => &policy.prompt,
        };

        // If even the policy prompt is empty, skip (shouldn't happen with defaults).
        if is_effectively_empty(effective_prompt) && !reason.is_event_driven() {
            return self.record_skip(
                agent_id,
                &reason,
                &started_at,
                started,
                "empty-heartbeat-file",
            );
        }

        // Resolve session key
        let main_session_key = format!("agent:{agent_id}:{}", self.config.session.main_key);
        let session_key = if policy.isolated_session {
            format!("{main_session_key}:heartbeat")
        } else {
            main_session_key.clone()
        };
        let session_dir = ensure_session_workspace(&agent_root, &session_key)?;

        // Build system prompt
        let model = policy.model.as_deref().unwrap_or(&agent.model);
        let provider = agent.provider;
        let think_level = agent.think_level.unwrap_or_default();

        let system_prompt = self.build_system_prompt(&session_dir, agent_id, &policy)?;

        // Pass system prompt via metadata so the runner sends it as
        // `--system-prompt` (system role) instead of concatenating it
        // into `-p` (user role) where it would accumulate in history.
        let mut metadata = HashMap::new();
        if !system_prompt.trim().is_empty() {
            metadata.insert("system_preamble".to_string(), system_prompt);
        }

        // Build run request
        let run_request = RunRequest {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            session_key: session_key.clone(),
            agent_id: agent_id.to_string(),
            provider,
            model: model.to_string(),
            think_level,
            working_directory: session_dir.to_string_lossy().to_string(),
            prompt: effective_prompt.to_string(),
            continue_session: !policy.isolated_session,
            metadata,
        };

        // Store the session
        self.store.touch_session(&session_key, agent_id)?;

        // Invoke runner
        let run_result = self.runner.run(run_request).await;
        let duration_ms = started.elapsed().as_millis() as i64;
        let finished_at = Utc::now().to_rfc3339();

        match run_result {
            Ok(result) => {
                let output = &result.text;
                let normalized = normalize_heartbeat_output(output, policy.ack_max_chars);

                // Resolve delivery from main session (isolated sessions lack delivery context)
                let delivery_session = if policy.isolated_session {
                    self.store.get_session(&main_session_key)?
                } else {
                    self.store.get_session(&session_key)?
                };
                let delivery_target = resolve_delivery_target(
                    &policy,
                    delivery_session.as_ref(),
                    agent_id,
                    &self.config.teams,
                );

                // Duplicate suppression (only for non-event-driven reasons)
                let is_dup = !reason.is_event_driven()
                    && matches!(normalized, NormalizeResult::Alert(_))
                    && is_duplicate_heartbeat(delivery_session.as_ref(), output, &finished_at);

                let (delivery_mode, delivery_channel, delivery_recipient) = if is_dup {
                    (HeartbeatDeliveryMode::Suppressed, None, None)
                } else {
                    match (&normalized, &delivery_target) {
                        (NormalizeResult::AckOnly | NormalizeResult::OkWithText(_), _) => {
                            (HeartbeatDeliveryMode::Suppressed, None, None)
                        }
                        (NormalizeResult::Alert(_), Some(delivery::DeliveryKind::Outbound(t))) => (
                            HeartbeatDeliveryMode::Delivered,
                            Some(t.channel.clone()),
                            Some(t.recipient.clone()),
                        ),
                        (
                            NormalizeResult::Alert(_),
                            Some(delivery::DeliveryKind::Chatroom { team_id }),
                        ) => (
                            HeartbeatDeliveryMode::Delivered,
                            Some("chatroom".to_string()),
                            Some(team_id.clone()),
                        ),
                        (NormalizeResult::Alert(_), None) => {
                            (HeartbeatDeliveryMode::NoTarget, None, None)
                        }
                    }
                };

                let delivered = delivery_mode == HeartbeatDeliveryMode::Delivered;

                // Deliver heartbeat output
                if delivered {
                    let delivery_text = match &normalized {
                        NormalizeResult::Alert(text) => text.as_str(),
                        _ => output.as_str(),
                    };

                    match &delivery_target {
                        Some(delivery::DeliveryKind::Outbound(target)) => {
                            if let Err(err) = queue::enqueue_outgoing_message(
                                &self.config,
                                &target.channel,
                                agent_id,
                                &target.recipient,
                                delivery_text,
                                effective_prompt,
                                &format!("heartbeat-{}", Uuid::new_v4().simple()),
                                agent_id,
                                vec![],
                                HashMap::new(),
                            )
                            .await
                            {
                                warn!(agent = %agent_id, error = %err, "failed to enqueue heartbeat delivery");
                            }
                        }
                        Some(delivery::DeliveryKind::Chatroom { team_id }) => {
                            if let Err(err) =
                                self.store
                                    .record_chatroom_message(team_id, agent_id, delivery_text)
                            {
                                warn!(agent = %agent_id, error = %err, "failed to record heartbeat chatroom message");
                            }
                            if let Some(team) = self.config.teams.get(team_id) {
                                for teammate_id in &team.agents {
                                    if teammate_id == agent_id
                                        || !self.config.agents.contains_key(teammate_id)
                                    {
                                        continue;
                                    }
                                    if let Err(err) = queue::enqueue_chatroom_message(
                                        &self.config,
                                        team_id,
                                        teammate_id,
                                        agent_id,
                                        delivery_text,
                                    )
                                    .await
                                    {
                                        warn!(
                                            agent = %agent_id,
                                            teammate = %teammate_id,
                                            error = %err,
                                            "failed to enqueue heartbeat chatroom message"
                                        );
                                    }
                                }
                            }
                        }
                        None => {}
                    }
                }

                // Update session heartbeat tracking (always on main session for dedup)
                if let Err(err) =
                    self.store
                        .update_session_heartbeat(&main_session_key, output, &finished_at)
                {
                    warn!(agent = %agent_id, error = %err, "failed to update session heartbeat");
                }

                let preview = match &normalized {
                    NormalizeResult::AckOnly => Some("HEARTBEAT_OK".to_string()),
                    NormalizeResult::OkWithText(t) => Some(truncate_preview(t, 100)),
                    NormalizeResult::Alert(t) => Some(truncate_preview(t, 100)),
                };

                // Indicator
                let has_delivery_issue = delivery_mode == HeartbeatDeliveryMode::NoTarget
                    && matches!(normalized, NormalizeResult::Alert(_));
                let event_status =
                    derive_event_status(&normalized, delivered, has_delivery_issue, false);
                let indicator = resolve_indicator_type(event_status);
                let indicator_str = indicator.map(|i| i.to_string());

                let skip_reason = if is_dup { Some("duplicate") } else { None };

                // Emit event
                let _ = self.sink.emit(
                    "heartbeat_run_succeeded",
                    json!({
                        "agent_id": agent_id,
                        "reason": reason.to_string(),
                        "delivery_mode": delivery_mode.to_string(),
                        "preview": preview,
                        "indicator_type": indicator_str,
                        "duplicate": is_dup,
                    }),
                );

                self.store.record_heartbeat_run(
                    agent_id,
                    &reason.to_string(),
                    Some(&session_key),
                    effective_prompt,
                    Some(output),
                    preview.as_deref(),
                    &HeartbeatRunStatus::Ran.to_string(),
                    skip_reason,
                    delivery_channel.as_deref(),
                    delivery_recipient.as_deref(),
                    Some(&delivery_mode.to_string()),
                    Some(model),
                    None,
                    indicator_str.as_deref(),
                    &started_at,
                    &finished_at,
                    duration_ms,
                )
            }
            Err(err) => {
                let indicator = resolve_indicator_type(indicator::HeartbeatEventStatus::Failed);
                let indicator_str = indicator.map(|i| i.to_string());

                let _ = self.sink.emit(
                    "heartbeat_run_failed",
                    json!({
                        "agent_id": agent_id,
                        "reason": reason.to_string(),
                        "error": err.to_string(),
                        "indicator_type": indicator_str,
                    }),
                );

                self.store.record_heartbeat_run(
                    agent_id,
                    &reason.to_string(),
                    Some(&session_key),
                    effective_prompt,
                    None,
                    None,
                    &HeartbeatRunStatus::Failed.to_string(),
                    Some(&err.to_string()),
                    None,
                    None,
                    None,
                    Some(model),
                    None,
                    indicator_str.as_deref(),
                    &started_at,
                    &finished_at,
                    duration_ms,
                )
            }
        }
    }

    /// Resolve the effective heartbeat policy for an agent.
    pub fn resolve_effective_policy(&self, agent_id: &str) -> Result<EffectiveHeartbeatPolicy> {
        let agent_defaults_hb = self
            .config
            .agent_defaults
            .as_ref()
            .and_then(|d| d.heartbeat.as_ref());
        let agent_hb = self
            .config
            .agents
            .get(agent_id)
            .and_then(|a| a.heartbeat.as_ref());
        resolve_effective_policy(agent_defaults_hb, agent_hb)
    }

    /// Determine which agents should participate in heartbeat.
    fn resolve_heartbeat_agents(&self) -> Vec<(String, AgentConfig)> {
        let any_explicit = self.config.agents.values().any(|a| a.heartbeat.is_some());

        if any_explicit {
            self.config
                .agents
                .iter()
                .filter(|(_, a)| a.heartbeat.is_some())
                .map(|(id, a)| (id.clone(), a.clone()))
                .collect()
        } else {
            self.config
                .agents
                .iter()
                .map(|(id, a)| (id.clone(), a.clone()))
                .collect()
        }
    }

    fn build_system_prompt(
        &self,
        session_dir: &std::path::Path,
        agent_id: &str,
        policy: &EffectiveHeartbeatPolicy,
    ) -> Result<String> {
        let ctx = PromptContext {
            workspace_dir: session_dir,
            agent_id,
            agents: &self.config.agents,
            teams: &self.config.teams,
            user_system_prompt: self
                .config
                .agents
                .get(agent_id)
                .and_then(|a| a.system_prompt.as_deref()),
            is_heartbeat: true,
            heartbeat_ack_max_chars: Some(policy.ack_max_chars),
            light_context: policy.light_context,
        };

        let builder = if policy.light_context {
            SystemPromptBuilder::with_heartbeat_defaults()
        } else {
            SystemPromptBuilder::with_defaults()
        };

        builder.build(&ctx)
    }

    fn record_skip(
        &self,
        agent_id: &str,
        reason: &HeartbeatRunReason,
        started_at: &str,
        started: Instant,
        skip_reason: &str,
    ) -> Result<HeartbeatRunView> {
        let duration_ms = started.elapsed().as_millis() as i64;
        let finished_at = Utc::now().to_rfc3339();

        let _ = self.sink.emit(
            "heartbeat_run_skipped",
            json!({
                "agent_id": agent_id,
                "reason": reason.to_string(),
                "skip_reason": skip_reason,
            }),
        );

        self.store.record_heartbeat_run(
            agent_id,
            &reason.to_string(),
            None,
            "",
            None,
            None,
            &HeartbeatRunStatus::Skipped.to_string(),
            Some(skip_reason),
            None,
            None,
            None,
            None,
            None,
            None, // skipped -> no indicator
            started_at,
            &finished_at,
            duration_ms,
        )
    }
}

/// Resolve the tick interval for the heartbeat loop.
///
/// Uses the minimum per-agent `every` so that the loop wakes often enough
/// for every agent's schedule.  Falls back to agent_defaults, then the
/// legacy global `interval_sec`.
pub fn resolve_tick_interval(config: &RuntimeConfig) -> std::time::Duration {
    let mut min_secs: Option<u64> = None;

    // Check per-agent intervals
    for agent in config.agents.values() {
        if let Some(hb) = &agent.heartbeat {
            if let Some(every) = &hb.every {
                if let Ok(dur) = config::parse_duration_str(every) {
                    let secs = dur.as_secs();
                    min_secs = Some(min_secs.map_or(secs, |m: u64| m.min(secs)));
                }
            }
        }
    }

    // Fallback to agent_defaults.heartbeat.every
    if min_secs.is_none() {
        if let Some(defaults) = config
            .agent_defaults
            .as_ref()
            .and_then(|d| d.heartbeat.as_ref())
        {
            if let Some(every) = &defaults.every {
                if let Ok(dur) = config::parse_duration_str(every) {
                    min_secs = Some(dur.as_secs());
                }
            }
        }
    }

    // Fallback to legacy global interval_sec
    let secs = min_secs.unwrap_or(config.heartbeat.interval_sec);
    std::time::Duration::from_secs(secs.max(10))
}

/// Legacy alias for backward compatibility with tests.
pub fn resolve_global_interval(config: &RuntimeConfig) -> std::time::Duration {
    resolve_tick_interval(config)
}

async fn load_heartbeat_md(agent_root: &std::path::Path) -> Option<String> {
    let path = agent_root.join("heartbeat.md");
    fs::read_to_string(&path).await.ok()
}

/// Settings parsed from heartbeat.md YAML frontmatter.
#[derive(Debug, Default)]
struct HeartbeatFileSettings {
    target: Option<HeartbeatTarget>,
    to: Option<String>,
    body: String,
}

/// Parse optional YAML frontmatter from heartbeat.md content.
///
/// ```markdown
/// ---
/// target: chatroom
/// ---
///
/// Prompt body here.
/// ```
fn parse_heartbeat_frontmatter(content: &str) -> HeartbeatFileSettings {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return HeartbeatFileSettings {
            body: content.to_string(),
            ..Default::default()
        };
    }

    let after_open = &trimmed[3..];
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);

    // Find closing --- (could be at start if frontmatter is empty)
    let (frontmatter, rest) = if after_open.starts_with("---") {
        ("", &after_open[3..])
    } else if let Some(pos) = after_open.find("\n---") {
        (&after_open[..pos], &after_open[pos + 4..])
    } else {
        return HeartbeatFileSettings {
            body: content.to_string(),
            ..Default::default()
        };
    };

    let body = rest.strip_prefix('\n').unwrap_or(rest);

    let mut target = None;
    let mut to = None;

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "target" => target = parse_heartbeat_target(value),
                "to" => {
                    if !value.is_empty() {
                        to = Some(value.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    HeartbeatFileSettings {
        target,
        to,
        body: body.to_string(),
    }
}

fn parse_heartbeat_target(s: &str) -> Option<HeartbeatTarget> {
    match s {
        "none" => Some(HeartbeatTarget::None),
        "last" => Some(HeartbeatTarget::Last),
        "telegram" => Some(HeartbeatTarget::Telegram),
        "discord" => Some(HeartbeatTarget::Discord),
        "slack" => Some(HeartbeatTarget::Slack),
        "chatroom" => Some(HeartbeatTarget::Chatroom),
        _ => None,
    }
}

fn is_effectively_empty(content: &str) -> bool {
    content.lines().all(|line| {
        let trimmed = line.trim();
        trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("<!--")
            || trimmed.starts_with("-->")
            || trimmed == "- [ ]"
            || trimmed == "* "
    })
}

fn truncate_preview(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_global_interval_from_agent_defaults() {
        let mut config = RuntimeConfig::default();
        config.agent_defaults = Some(config::AgentDefaultsConfig {
            heartbeat: Some(domain::AgentHeartbeatConfig {
                every: Some("15m".into()),
                ..Default::default()
            }),
        });
        let dur = resolve_global_interval(&config);
        assert_eq!(dur, std::time::Duration::from_secs(900));
    }

    #[test]
    fn resolve_global_interval_legacy_fallback() {
        let mut config = RuntimeConfig::default();
        config.heartbeat.interval_sec = 600;
        let dur = resolve_global_interval(&config);
        assert_eq!(dur, std::time::Duration::from_secs(600));
    }

    #[test]
    fn resolve_global_interval_minimum_10s() {
        let mut config = RuntimeConfig::default();
        config.heartbeat.interval_sec = 1;
        let dur = resolve_global_interval(&config);
        assert_eq!(dur, std::time::Duration::from_secs(10));
    }

    #[test]
    fn heartbeat_loop_settings_use_effective_interval() {
        let mut config = RuntimeConfig::default();
        config.agent_defaults = Some(config::AgentDefaultsConfig {
            heartbeat: Some(domain::AgentHeartbeatConfig {
                every: Some("15m".into()),
                ..Default::default()
            }),
        });
        let settings = HeartbeatLoopSettings::from_config(&config);
        assert!(!settings.enabled);
        assert_eq!(settings.interval_sec, 900);
        assert_eq!(settings.interval(), std::time::Duration::from_secs(900));
    }

    #[test]
    fn heartbeat_loop_control_only_notifies_on_change() {
        let config = RuntimeConfig::default();
        let control = HeartbeatLoopControl::new(&config);

        assert!(!control.update_from_config(&config));

        let mut next = config.clone();
        next.heartbeat.enabled = true;
        assert!(control.update_from_config(&next));
        assert!(control.subscribe().borrow().enabled);
    }

    #[test]
    fn is_effectively_empty_detects_template() {
        assert!(is_effectively_empty(
            "# Heartbeat\n\n<!-- instructions -->\n"
        ));
        assert!(is_effectively_empty(""));
        assert!(is_effectively_empty("  \n  \n"));
    }

    #[test]
    fn is_effectively_empty_detects_content() {
        assert!(!is_effectively_empty("Check the deploy status"));
        assert!(!is_effectively_empty("# Heartbeat\nCheck status"));
    }

    #[test]
    fn truncate_preview_short() {
        assert_eq!(truncate_preview("hello", 100), "hello");
    }

    #[test]
    fn truncate_preview_long() {
        let long = "x".repeat(200);
        let result = truncate_preview(&long, 100);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 104);
    }

    #[test]
    fn truncate_preview_preserves_utf8_boundaries() {
        let text = "こんにちは。東京は現在12°Cで、くもりです。今日は最高18°C、最低11°Cの予報です。";
        let result = truncate_preview(text, 40);
        assert!(result.ends_with("..."));
        assert!(result.is_char_boundary(result.len()));
        assert!(result.starts_with("こんにちは。東京"));
    }

    #[test]
    fn reason_event_driven_bypasses_file_gate() {
        assert!(!HeartbeatRunReason::Scheduled.is_event_driven());
        assert!(!HeartbeatRunReason::Manual.is_event_driven());
        assert!(HeartbeatRunReason::ExecEvent.is_event_driven());
        assert!(HeartbeatRunReason::Cron.is_event_driven());
        assert!(HeartbeatRunReason::Hook.is_event_driven());
        assert!(HeartbeatRunReason::Wake.is_event_driven());
    }

    #[test]
    fn resolve_tick_interval_uses_min_agent_every() {
        let mut config = RuntimeConfig::default();
        config.heartbeat.interval_sec = 3600;
        config.agents.insert(
            "fast".into(),
            domain::AgentConfig {
                name: "fast".into(),
                model: "m".into(),
                heartbeat: Some(domain::AgentHeartbeatConfig {
                    every: Some("5m".into()),
                    ..Default::default()
                }),
                ..default_agent()
            },
        );
        config.agents.insert(
            "slow".into(),
            domain::AgentConfig {
                name: "slow".into(),
                model: "m".into(),
                heartbeat: Some(domain::AgentHeartbeatConfig {
                    every: Some("1h".into()),
                    ..Default::default()
                }),
                ..default_agent()
            },
        );
        let dur = resolve_tick_interval(&config);
        // Should use the minimum: 5 minutes
        assert_eq!(dur, std::time::Duration::from_secs(300));
    }

    #[test]
    fn resolve_tick_interval_falls_back_to_legacy() {
        let mut config = RuntimeConfig::default();
        config.heartbeat.interval_sec = 600;
        // No per-agent heartbeat configs
        let dur = resolve_tick_interval(&config);
        assert_eq!(dur, std::time::Duration::from_secs(600));
    }

    fn default_agent() -> domain::AgentConfig {
        domain::AgentConfig {
            name: String::new(),
            provider: domain::ProviderKind::Mock,
            model: String::new(),
            think_level: None,
            provider_id: None,
            system_prompt: None,
            prompt_file: None,
            heartbeat: None,
        }
    }

    // ── frontmatter parsing ──────────────────────────────────────

    #[test]
    fn frontmatter_target_chatroom() {
        let content = "---\ntarget: chatroom\n---\nCheck status";
        let settings = parse_heartbeat_frontmatter(content);
        assert_eq!(settings.target, Some(HeartbeatTarget::Chatroom));
        assert_eq!(settings.body, "Check status");
    }

    #[test]
    fn frontmatter_target_discord() {
        let content = "---\ntarget: discord\n---\nPing users";
        let settings = parse_heartbeat_frontmatter(content);
        assert_eq!(settings.target, Some(HeartbeatTarget::Discord));
        assert_eq!(settings.body, "Ping users");
    }

    #[test]
    fn frontmatter_no_frontmatter_returns_body() {
        let content = "Just a prompt with no frontmatter";
        let settings = parse_heartbeat_frontmatter(content);
        assert!(settings.target.is_none());
        assert_eq!(settings.body, content);
    }

    #[test]
    fn frontmatter_empty_frontmatter() {
        let content = "---\n---\nBody here";
        let settings = parse_heartbeat_frontmatter(content);
        assert!(settings.target.is_none());
        assert_eq!(settings.body, "Body here");
    }

    #[test]
    fn frontmatter_target_and_to() {
        let content = "---\ntarget: discord\nto: \"1486346863127822486\"\n---\nCheck status";
        let settings = parse_heartbeat_frontmatter(content);
        assert_eq!(settings.target, Some(HeartbeatTarget::Discord));
        assert_eq!(settings.to.as_deref(), Some("1486346863127822486"));
        assert_eq!(settings.body, "Check status");
    }

    #[test]
    fn frontmatter_to_without_quotes() {
        let content = "---\ntarget: slack\nto: C0123456789\n---\nBody";
        let settings = parse_heartbeat_frontmatter(content);
        assert_eq!(settings.to.as_deref(), Some("C0123456789"));
    }

    #[test]
    fn frontmatter_unknown_target_ignored() {
        let content = "---\ntarget: carrier-pigeon\n---\nBody";
        let settings = parse_heartbeat_frontmatter(content);
        assert!(settings.target.is_none());
    }

    #[test]
    fn frontmatter_preserves_body_with_comments() {
        let content = "---\ntarget: chatroom\n---\n# Heartbeat\n\n<!-- info -->\n\nDo the thing";
        let settings = parse_heartbeat_frontmatter(content);
        assert_eq!(settings.target, Some(HeartbeatTarget::Chatroom));
        assert!(settings.body.contains("Do the thing"));
    }
}
