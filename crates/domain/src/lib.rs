use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
        ModelDefinition {
            id: "gpt-5.4-nano".into(),
            name: "GPT-5.4 Nano".into(),
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

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, request: RunRequest) -> Result<RunResult>;
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
        assert!(is_known_model(ProviderKind::Anthropic, "claude-sonnet-4-5"));
        assert!(is_known_model(ProviderKind::Openai, "o3"));
    }

    #[test]
    fn is_known_model_negative() {
        assert!(!is_known_model(ProviderKind::Anthropic, "nonexistent"));
        assert!(!is_known_model(ProviderKind::Openai, "claude-sonnet-4-5"));
    }

    #[test]
    fn all_models_have_non_empty_id_and_name() {
        for m in model_catalog() {
            assert!(!m.id.is_empty(), "model id must not be empty");
            assert!(!m.name.is_empty(), "model name must not be empty");
        }
    }
}
