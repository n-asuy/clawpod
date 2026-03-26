use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[default]
    Anthropic,
    Openai,
    Custom,
    Mock,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderHarness {
    Anthropic,
    Openai,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkLevel {
    Off,
    Minimal,
    #[default]
    Low,
    Medium,
    High,
    Xhigh,
    Adaptive,
}

impl ThinkLevel {
    /// Map to `claude --effort` flag values.
    pub fn to_claude_effort(self) -> Option<&'static str> {
        match self {
            Self::Off | Self::Minimal | Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Xhigh => Some("max"),
            Self::Adaptive => None, // let Claude decide
        }
    }
}

impl std::fmt::Display for ThinkLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => write!(f, "off"),
            Self::Minimal => write!(f, "minimal"),
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
            Self::Xhigh => write!(f, "xhigh"),
            Self::Adaptive => write!(f, "adaptive"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDefinition {
    pub id: String,
    pub name: String,
    pub provider: ProviderKind,
    #[serde(default)]
    pub supports_thinking: bool,
    #[serde(default)]
    pub default_think_level: ThinkLevel,
    #[serde(default)]
    pub context_window: Option<u32>,
}

static MODEL_CATALOG: LazyLock<Vec<ModelDefinition>> = LazyLock::new(|| {
    vec![
        // Anthropic
        ModelDefinition {
            id: "claude-opus-4-6".into(),
            name: "Claude Opus 4.6".into(),
            provider: ProviderKind::Anthropic,
            supports_thinking: true,
            default_think_level: ThinkLevel::Adaptive,
            context_window: Some(1_000_000),
        },
        ModelDefinition {
            id: "claude-sonnet-4-6".into(),
            name: "Claude Sonnet 4.6".into(),
            provider: ProviderKind::Anthropic,
            supports_thinking: true,
            default_think_level: ThinkLevel::Adaptive,
            context_window: Some(200_000),
        },
        ModelDefinition {
            id: "claude-haiku-4-5".into(),
            name: "Claude Haiku 4.5".into(),
            provider: ProviderKind::Anthropic,
            supports_thinking: false,
            default_think_level: ThinkLevel::Off,
            context_window: Some(200_000),
        },
        // OpenAI
        ModelDefinition {
            id: "gpt-5.4".into(),
            name: "GPT-5.4".into(),
            provider: ProviderKind::Openai,
            supports_thinking: true,
            default_think_level: ThinkLevel::Low,
            context_window: Some(1_050_000),
        },
        ModelDefinition {
            id: "gpt-5.4-mini".into(),
            name: "GPT-5.4 Mini".into(),
            provider: ProviderKind::Openai,
            supports_thinking: true,
            default_think_level: ThinkLevel::Low,
            context_window: Some(1_050_000),
        },
    ]
});

pub fn model_catalog() -> &'static [ModelDefinition] {
    &MODEL_CATALOG
}

pub fn models_for_provider(provider: ProviderKind) -> Vec<&'static ModelDefinition> {
    MODEL_CATALOG
        .iter()
        .filter(|m| m.provider == provider)
        .collect()
}

pub fn is_known_model(provider: ProviderKind, model_id: &str) -> bool {
    MODEL_CATALOG
        .iter()
        .any(|m| m.provider == provider && m.id == model_id)
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    #[default]
    Collect,
    Followup,
    Steer,
    SteerBacklog,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DmScope {
    Main,
    PerPeer,
    #[default]
    PerChannelPeer,
    PerAccountChannelPeer,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChatType {
    #[default]
    Direct,
    Group,
    Thread,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActiveHoursConfig {
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentHeartbeatConfig {
    #[serde(default)]
    pub every: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub target: Option<HeartbeatTarget>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub ack_max_chars: Option<usize>,
    #[serde(default)]
    pub direct_policy: Option<HeartbeatDirectPolicy>,
    #[serde(default)]
    pub include_reasoning: Option<bool>,
    #[serde(default)]
    pub light_context: Option<bool>,
    #[serde(default)]
    pub isolated_session: Option<bool>,
    #[serde(default)]
    pub active_hours: Option<ActiveHoursConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelHeartbeatConfig {
    #[serde(default)]
    pub show_ok: Option<bool>,
    #[serde(default)]
    pub show_alerts: Option<bool>,
    #[serde(default)]
    pub use_indicator: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    #[serde(default)]
    pub provider: ProviderKind,
    pub model: String,
    #[serde(default)]
    pub think_level: Option<ThinkLevel>,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub prompt_file: Option<String>,
    #[serde(default)]
    pub heartbeat: Option<AgentHeartbeatConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub name: String,
    pub agents: Vec<String>,
    pub leader_agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BindingMatch {
    pub channel: Option<String>,
    pub account_id: Option<String>,
    pub peer_id: Option<String>,
    pub group_id: Option<String>,
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingRule {
    pub agent_id: String,
    #[serde(rename = "match")]
    pub matcher: BindingMatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundEvent {
    pub message_id: String,
    pub channel: String,
    pub sender: String,
    pub sender_id: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub chat_type: ChatType,
    pub peer_id: String,
    pub account_id: Option<String>,
    pub files: Vec<String>,
    pub pre_routed_agent: Option<String>,
    pub from_agent: Option<String>,
    #[serde(default)]
    pub chain_depth: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundEvent {
    pub channel: String,
    pub recipient_id: String,
    pub message: String,
    pub message_id: String,
    pub original_message_id: String,
    pub agent_id: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChainState {
    Running,
    Succeeded,
    Failed,
    MaxStepsReached,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub run_id: Uuid,
    pub task_id: Uuid,
    pub session_key: String,
    pub agent_id: String,
    pub provider: ProviderKind,
    pub model: String,
    pub think_level: ThinkLevel,
    pub working_directory: String,
    pub prompt: String,
    pub continue_session: bool,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub text: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u128,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamChainStepView {
    pub chain_id: String,
    pub task_id: String,
    pub step_index: usize,
    pub agent_id: String,
    pub input: String,
    pub output: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatroomMessageView {
    pub id: i64,
    pub team_id: String,
    pub from_agent: String,
    pub message: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatroomPost {
    pub team_id: String,
    pub message: String,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HeartbeatTarget {
    #[default]
    None,
    Last,
    Telegram,
    Discord,
    Slack,
    Chatroom,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HeartbeatDirectPolicy {
    #[default]
    Allow,
    Block,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatRunReason {
    Scheduled,
    Manual,
    Retry,
    ExecEvent,
    Wake,
    Cron,
    Hook,
    Other,
}

impl std::fmt::Display for HeartbeatRunReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scheduled => write!(f, "scheduled"),
            Self::Manual => write!(f, "manual"),
            Self::Retry => write!(f, "retry"),
            Self::ExecEvent => write!(f, "exec_event"),
            Self::Wake => write!(f, "wake"),
            Self::Cron => write!(f, "cron"),
            Self::Hook => write!(f, "hook"),
            Self::Other => write!(f, "other"),
        }
    }
}

impl HeartbeatRunReason {
    /// Returns true for event-driven reasons that bypass file gates.
    pub fn is_event_driven(self) -> bool {
        matches!(self, Self::ExecEvent | Self::Cron | Self::Wake | Self::Hook)
    }

    /// Returns true for action-level wake reasons (higher priority).
    pub fn is_action_wake(self) -> bool {
        matches!(self, Self::Manual | Self::ExecEvent | Self::Hook)
    }
}

/// Indicator type emitted with heartbeat events for UI display.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatIndicatorType {
    Ok,
    Sent,
    Alert,
    Error,
}

impl std::fmt::Display for HeartbeatIndicatorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => write!(f, "ok"),
            Self::Sent => write!(f, "sent"),
            Self::Alert => write!(f, "alert"),
            Self::Error => write!(f, "error"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatRunStatus {
    Ran,
    Skipped,
    Failed,
}

impl std::fmt::Display for HeartbeatRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ran => write!(f, "ran"),
            Self::Skipped => write!(f, "skipped"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatDeliveryMode {
    Delivered,
    Suppressed,
    NoTarget,
}

impl std::fmt::Display for HeartbeatDeliveryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Delivered => write!(f, "delivered"),
            Self::Suppressed => write!(f, "suppressed"),
            Self::NoTarget => write!(f, "no_target"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatRunView {
    pub id: i64,
    pub agent_id: String,
    pub reason: String,
    #[serde(default)]
    pub session_key: Option<String>,
    pub prompt: String,
    pub output: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    pub status: String,
    #[serde(default)]
    pub skip_reason: Option<String>,
    #[serde(default)]
    pub delivery_channel: Option<String>,
    #[serde(default)]
    pub delivery_recipient: Option<String>,
    #[serde(default)]
    pub delivery_mode: Option<String>,
    #[serde(default)]
    pub used_model: Option<String>,
    #[serde(default)]
    pub used_prompt: Option<String>,
    #[serde(default)]
    pub indicator_type: Option<String>,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MentionHandoff {
    pub teammate_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainStep {
    pub agent_id: String,
    pub input: String,
    pub output: String,
    pub handoffs: Vec<MentionHandoff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainResult {
    pub final_text: String,
    pub steps: Vec<ChainStep>,
    pub state: ChainState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteDecision {
    pub agent_id: String,
    pub message: String,
    pub is_team_routed: bool,
    pub team_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Run event streaming
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunEventType {
    Started,
    ToolCall,
    ToolResult,
    AgentMessage,
    Thinking,
    Completed,
    Failed,
    TextChunk,
}

impl std::fmt::Display for RunEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Started => "started",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::AgentMessage => "agent_message",
            Self::Thinking => "thinking",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TextChunk => "text_chunk",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEvent {
    pub run_id: String,
    pub seq: u32,
    pub timestamp: String,
    pub event_type: RunEventType,
    pub data: Value,
}

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, request: RunRequest) -> Result<RunResult>;

    async fn run_streamed(
        &self,
        request: RunRequest,
        tx: tokio::sync::mpsc::Sender<RunEvent>,
    ) -> Result<RunResult> {
        let _ = tx;
        self.run(request).await
    }
}

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("agent not found: {0}")]
    AgentNotFound(String),

    #[error("team not found: {0}")]
    TeamNotFound(String),

    #[error("invalid routing message")]
    InvalidRoutingMessage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_catalog_is_not_empty() {
        assert!(!model_catalog().is_empty());
    }

    #[test]
    fn models_for_provider_anthropic_returns_only_anthropic() {
        let models = models_for_provider(ProviderKind::Anthropic);
        assert!(!models.is_empty());
        for m in &models {
            assert_eq!(m.provider, ProviderKind::Anthropic);
        }
    }

    #[test]
    fn models_for_provider_openai_returns_only_openai() {
        let models = models_for_provider(ProviderKind::Openai);
        assert!(!models.is_empty());
        for m in &models {
            assert_eq!(m.provider, ProviderKind::Openai);
        }
    }

    #[test]
    fn is_known_model_positive() {
        assert!(is_known_model(ProviderKind::Anthropic, "claude-sonnet-4-6"));
        assert!(is_known_model(ProviderKind::Openai, "gpt-5.4"));
    }

    #[test]
    fn is_known_model_negative() {
        assert!(!is_known_model(ProviderKind::Anthropic, "nonexistent"));
        assert!(!is_known_model(ProviderKind::Openai, "claude-sonnet-4-6"));
    }

    #[test]
    fn all_models_have_non_empty_id_and_name() {
        for m in model_catalog() {
            assert!(!m.id.is_empty(), "model id must not be empty");
            assert!(!m.name.is_empty(), "model name must not be empty");
        }
    }

    #[test]
    fn heartbeat_target_serde_roundtrip() {
        for (variant, json_str) in [
            (HeartbeatTarget::None, "\"none\""),
            (HeartbeatTarget::Last, "\"last\""),
            (HeartbeatTarget::Telegram, "\"telegram\""),
            (HeartbeatTarget::Discord, "\"discord\""),
            (HeartbeatTarget::Slack, "\"slack\""),
            (HeartbeatTarget::Chatroom, "\"chatroom\""),
        ] {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, json_str);
            let deserialized: HeartbeatTarget = serde_json::from_str(json_str).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn heartbeat_target_default_is_none() {
        assert_eq!(HeartbeatTarget::default(), HeartbeatTarget::None);
    }

    #[test]
    fn heartbeat_direct_policy_serde_roundtrip() {
        let allow: HeartbeatDirectPolicy = serde_json::from_str("\"allow\"").unwrap();
        assert_eq!(allow, HeartbeatDirectPolicy::Allow);
        let block: HeartbeatDirectPolicy = serde_json::from_str("\"block\"").unwrap();
        assert_eq!(block, HeartbeatDirectPolicy::Block);
    }

    #[test]
    fn heartbeat_run_reason_display() {
        assert_eq!(HeartbeatRunReason::Scheduled.to_string(), "scheduled");
        assert_eq!(HeartbeatRunReason::Manual.to_string(), "manual");
        assert_eq!(HeartbeatRunReason::ExecEvent.to_string(), "exec_event");
        assert_eq!(HeartbeatRunReason::Cron.to_string(), "cron");
        assert_eq!(HeartbeatRunReason::Hook.to_string(), "hook");
        assert_eq!(HeartbeatRunReason::Wake.to_string(), "wake");
        assert_eq!(HeartbeatRunReason::Retry.to_string(), "retry");
        assert_eq!(HeartbeatRunReason::Other.to_string(), "other");
    }

    #[test]
    fn heartbeat_run_reason_event_driven() {
        assert!(!HeartbeatRunReason::Scheduled.is_event_driven());
        assert!(!HeartbeatRunReason::Manual.is_event_driven());
        assert!(HeartbeatRunReason::ExecEvent.is_event_driven());
        assert!(HeartbeatRunReason::Cron.is_event_driven());
        assert!(HeartbeatRunReason::Wake.is_event_driven());
        assert!(HeartbeatRunReason::Hook.is_event_driven());
    }

    #[test]
    fn heartbeat_indicator_type_display() {
        assert_eq!(HeartbeatIndicatorType::Ok.to_string(), "ok");
        assert_eq!(HeartbeatIndicatorType::Sent.to_string(), "sent");
        assert_eq!(HeartbeatIndicatorType::Alert.to_string(), "alert");
        assert_eq!(HeartbeatIndicatorType::Error.to_string(), "error");
    }

    #[test]
    fn heartbeat_run_status_display() {
        assert_eq!(HeartbeatRunStatus::Ran.to_string(), "ran");
        assert_eq!(HeartbeatRunStatus::Skipped.to_string(), "skipped");
        assert_eq!(HeartbeatRunStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn heartbeat_delivery_mode_display() {
        assert_eq!(HeartbeatDeliveryMode::Delivered.to_string(), "delivered");
        assert_eq!(HeartbeatDeliveryMode::Suppressed.to_string(), "suppressed");
        assert_eq!(HeartbeatDeliveryMode::NoTarget.to_string(), "no_target");
    }

    #[test]
    fn heartbeat_run_view_deserializes_with_new_fields() {
        let json = r#"{
            "id": 1,
            "agent_id": "default",
            "reason": "scheduled",
            "prompt": "check",
            "output": "HEARTBEAT_OK",
            "status": "ran",
            "started_at": "2026-01-01T00:00:00Z",
            "finished_at": "2026-01-01T00:01:00Z",
            "duration_ms": 60000
        }"#;
        let view: HeartbeatRunView = serde_json::from_str(json).unwrap();
        assert_eq!(view.reason, "scheduled");
        assert!(view.session_key.is_none());
        assert!(view.preview.is_none());
        assert!(view.skip_reason.is_none());
        assert!(view.delivery_channel.is_none());
    }

    #[test]
    fn heartbeat_run_view_backward_compat_old_format() {
        // Old format without new fields should still deserialize
        let json = r#"{
            "id": 1,
            "agent_id": "default",
            "reason": "scheduled",
            "prompt": "check",
            "output": null,
            "status": "ok",
            "started_at": "2026-01-01T00:00:00Z",
            "finished_at": "2026-01-01T00:01:00Z",
            "duration_ms": 1000
        }"#;
        let view: HeartbeatRunView = serde_json::from_str(json).unwrap();
        assert_eq!(view.id, 1);
        assert_eq!(view.status, "ok");
    }
}
