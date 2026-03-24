pub mod active_hours;
pub mod dedup;
pub mod delivery;
pub mod indicator;
pub mod normalize;
pub mod policy;
pub mod visibility;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use agent::{ensure_agent_workspace, ensure_session_workspace, PromptContext, SystemPromptBuilder};
use anyhow::{Context, Result};
use chrono::Utc;
use config::RuntimeConfig;
use domain::{
    AgentConfig, HeartbeatDeliveryMode, HeartbeatRunReason, HeartbeatRunStatus, HeartbeatRunView,
    RunRequest, Runner,
};
use observer::FileEventSink;
use serde_json::json;
use store::StateStore;
use tokio::fs;
use tokio::sync::watch;
use tracing::warn;
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
            interval_sec: resolve_global_interval(config).as_secs(),
        }
    }

    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_sec.max(10))
    }
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
        }
    }

    /// Run heartbeat for all enabled agents.
    pub async fn run_scheduled_cycle(&self) -> CycleOutcome {
        let mut outcome = CycleOutcome::default();
        let agents = self.resolve_heartbeat_agents();

        for (agent_id, _agent) in &agents {
            match self.run_once(agent_id, HeartbeatRunReason::Scheduled).await {
                Ok(view) => match view.status.as_str() {
                    "ran" => outcome.ran += 1,
                    "skipped" => outcome.skipped += 1,
                    _ => outcome.failed += 1,
                },
                Err(err) => {
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

        let policy = self.resolve_effective_policy(agent_id)?;

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

        // Load HEARTBEAT.md. If present and non-empty, use it.
        // If absent or effectively empty, fall back to the policy default prompt.
        let heartbeat_prompt = load_heartbeat_md(&agent_root).await;
        let effective_prompt = match &heartbeat_prompt {
            Some(content) if !is_effectively_empty(content) => content.as_str(),
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
            prompt: format!("{system_prompt}\n\n{effective_prompt}"),
            continue_session: !policy.isolated_session,
            metadata: HashMap::new(),
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
                let delivery_target = resolve_delivery_target(&policy, delivery_session.as_ref());

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
                        (NormalizeResult::Alert(_), Some(target)) => (
                            HeartbeatDeliveryMode::Delivered,
                            Some(target.channel.clone()),
                            Some(target.recipient.clone()),
                        ),
                        (NormalizeResult::Alert(_), None) => {
                            (HeartbeatDeliveryMode::NoTarget, None, None)
                        }
                    }
                };

                let delivered = delivery_mode == HeartbeatDeliveryMode::Delivered;

                // Enqueue outbound if delivering
                if delivered {
                    if let (Some(ch), Some(recip)) = (&delivery_channel, &delivery_recipient) {
                        let delivery_text = match &normalized {
                            NormalizeResult::Alert(text) => text.as_str(),
                            _ => output.as_str(),
                        };
                        if let Err(err) = queue::enqueue_outgoing_message(
                            &self.config,
                            ch,
                            agent_id,
                            recip,
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

/// Resolve the global heartbeat interval from config.
pub fn resolve_global_interval(config: &RuntimeConfig) -> std::time::Duration {
    if let Some(defaults) = config
        .agent_defaults
        .as_ref()
        .and_then(|d| d.heartbeat.as_ref())
    {
        if let Some(every) = &defaults.every {
            if let Ok(dur) = config::parse_duration_str(every) {
                return dur;
            }
        }
    }
    std::time::Duration::from_secs(config.heartbeat.interval_sec.max(10))
}

async fn load_heartbeat_md(agent_root: &std::path::Path) -> Option<String> {
    let path = agent_root.join("heartbeat.md");
    fs::read_to_string(&path).await.ok()
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
    if text.len() <= max {
        text.to_string()
    } else {
        format!("{}...", &text[..max.min(text.len())])
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
    fn reason_event_driven_bypasses_file_gate() {
        assert!(!HeartbeatRunReason::Scheduled.is_event_driven());
        assert!(!HeartbeatRunReason::Manual.is_event_driven());
        assert!(HeartbeatRunReason::ExecEvent.is_event_driven());
        assert!(HeartbeatRunReason::Cron.is_event_driven());
        assert!(HeartbeatRunReason::Hook.is_event_driven());
        assert!(HeartbeatRunReason::Wake.is_event_driven());
    }
}
