use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use dirs_next::home_dir;
use domain::{
    AgentConfig, AgentHeartbeatConfig, BindingRule, ChannelHeartbeatConfig, ChatType, DmScope,
    ProviderHarness, ProviderKind, QueueMode, TeamConfig,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub enabled: bool,
    pub api_port: u16,
    #[serde(default = "default_server_host")]
    pub host: String,
    #[serde(default)]
    pub allow_public_bind: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_port: 3777,
            host: default_server_host(),
            allow_public_bind: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingConfig {
    #[serde(default = "default_code_length")]
    pub code_length: usize,
    #[serde(default = "default_code_ttl_secs")]
    pub code_ttl_secs: u64,
    #[serde(default = "default_max_failed_attempts")]
    pub max_failed_attempts: u32,
    #[serde(default = "default_lockout_secs")]
    pub lockout_secs: u64,
}

impl Default for PairingConfig {
    fn default() -> Self {
        Self {
            code_length: default_code_length(),
            code_ttl_secs: default_code_ttl_secs(),
            max_failed_attempts: default_max_failed_attempts(),
            lockout_secs: default_lockout_secs(),
        }
    }
}

fn default_code_length() -> usize {
    8
}

fn default_code_ttl_secs() -> u64 {
    3600
}

fn default_max_failed_attempts() -> u32 {
    5
}

fn default_lockout_secs() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    pub home_dir: String,
    pub workspace_dir: String,
    pub poll_interval_ms: u64,
    pub max_concurrent_runs: usize,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            home_dir: "~/.clawpod".to_string(),
            workspace_dir: "~/.clawpod/workspace".to_string(),
            poll_interval_ms: 1000,
            max_concurrent_runs: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueConfig {
    #[serde(default)]
    pub mode: QueueMode,
    pub max_retries: u32,
    pub backoff_base_ms: u64,
    pub dead_letter_enabled: bool,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            mode: QueueMode::Collect,
            max_retries: 3,
            backoff_base_ms: 500,
            dead_letter_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default)]
    pub dm_scope: DmScope,
    pub main_key: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            dm_scope: DmScope::PerChannelPeer,
            main_key: "main".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainConfig {
    pub max_chain_steps: usize,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self { max_chain_steps: 8 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConfig {
    #[serde(default)]
    pub default_provider: ProviderKind,
    pub timeout_sec: u64,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            default_provider: ProviderKind::Anthropic,
            timeout_sec: 120,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_heartbeat_interval_sec")]
    pub interval_sec: u64,
    #[serde(default = "default_heartbeat_sender")]
    pub sender: String,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_sec: default_heartbeat_interval_sec(),
            sender: default_heartbeat_sender(),
        }
    }
}

fn default_heartbeat_interval_sec() -> u64 {
    3600
}

fn default_heartbeat_sender() -> String {
    "Heartbeat".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentDefaultsConfig {
    #[serde(default)]
    pub heartbeat: Option<AgentHeartbeatConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelDefaultsConfig {
    #[serde(default)]
    pub heartbeat: Option<ChannelHeartbeatConfig>,
}

/// Parse a human-readable duration string into `std::time::Duration`.
///
/// Supported suffixes: `s` (seconds), `m` (minutes), `h` (hours).
/// Plain numbers without suffix are treated as seconds.
pub fn parse_duration_str(s: &str) -> Result<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration string");
    }
    let (num_part, multiplier) = if let Some(n) = s.strip_suffix('h') {
        (n, 3600u64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60u64)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1u64)
    } else {
        (s, 1u64)
    };
    let value: u64 = num_part
        .trim()
        .parse()
        .with_context(|| format!("invalid duration: {s}"))?;
    if value == 0 {
        bail!("duration must be greater than zero: {s}");
    }
    Ok(std::time::Duration::from_secs(value * multiplier))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomProviderConfig {
    pub name: String,
    pub harness: ProviderHarness,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TelegramConfig {
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default)]
    pub bot_token_env: Option<String>,
    #[serde(default)]
    pub access: Option<ChannelAccessConfig>,
    #[serde(default)]
    pub heartbeat: Option<ChannelHeartbeatConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscordConfig {
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default)]
    pub bot_token_env: Option<String>,
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub mention_only: bool,
    #[serde(default)]
    pub access: Option<ChannelAccessConfig>,
    #[serde(default)]
    pub heartbeat: Option<ChannelHeartbeatConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SlackConfig {
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default)]
    pub bot_token_env: Option<String>,
    #[serde(default)]
    pub app_token: Option<String>,
    #[serde(default)]
    pub app_token_env: Option<String>,
    #[serde(default)]
    pub access: Option<ChannelAccessConfig>,
    #[serde(default)]
    pub heartbeat: Option<ChannelHeartbeatConfig>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DirectMessagePolicy {
    Open,
    Allowlist,
    Pairing,
    Disabled,
}

impl Default for DirectMessagePolicy {
    fn default() -> Self {
        Self::Open
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupPolicy {
    Disabled,
    MentionOnly,
    Allowlist,
    Open,
}

impl Default for GroupPolicy {
    fn default() -> Self {
        Self::Disabled
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelAccessConfig {
    #[serde(default)]
    pub dm_policy: DirectMessagePolicy,
    #[serde(default)]
    pub group_policy: GroupPolicy,
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default)]
    pub group_allow_from: Vec<String>,
    #[serde(default)]
    pub channels: HashMap<String, PerChannelAccessConfig>,
}

fn default_per_channel_allow() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerChannelAccessConfig {
    #[serde(default = "default_per_channel_allow")]
    pub allow: bool,
    #[serde(default)]
    pub require_mention: Option<bool>,
}

impl Default for PerChannelAccessConfig {
    fn default() -> Self {
        Self {
            allow: true,
            require_mention: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressDecision {
    Allow,
    Drop { reason: &'static str },
    RequirePairing,
}

impl TelegramConfig {
    pub fn effective_access(&self) -> ChannelAccessConfig {
        self.access.clone().unwrap_or(ChannelAccessConfig {
            dm_policy: DirectMessagePolicy::Open,
            group_policy: GroupPolicy::MentionOnly,
            allow_from: vec![],
            group_allow_from: vec![],
            channels: HashMap::new(),
        })
    }
}

impl DiscordConfig {
    pub fn effective_access(&self) -> ChannelAccessConfig {
        self.access.clone().unwrap_or(ChannelAccessConfig {
            dm_policy: DirectMessagePolicy::Open,
            group_policy: if self.mention_only {
                GroupPolicy::MentionOnly
            } else {
                GroupPolicy::Open
            },
            allow_from: vec![],
            group_allow_from: vec![],
            channels: HashMap::new(),
        })
    }
}

impl SlackConfig {
    pub fn effective_access(&self) -> ChannelAccessConfig {
        self.access.clone().unwrap_or(ChannelAccessConfig {
            dm_policy: DirectMessagePolicy::Open,
            group_policy: GroupPolicy::MentionOnly,
            allow_from: vec![],
            group_allow_from: vec![],
            channels: HashMap::new(),
        })
    }
}

pub fn evaluate_ingress_policy(
    access: &ChannelAccessConfig,
    chat_type: ChatType,
    sender_id: &str,
    mentions_bot: bool,
    is_pairing_approved: bool,
    channel_id: Option<&str>,
) -> IngressDecision {
    match chat_type {
        ChatType::Direct => match access.dm_policy {
            DirectMessagePolicy::Open => IngressDecision::Allow,
            DirectMessagePolicy::Disabled => IngressDecision::Drop {
                reason: "dm_disabled",
            },
            DirectMessagePolicy::Allowlist => {
                if access.allow_from.iter().any(|value| value == sender_id) {
                    IngressDecision::Allow
                } else {
                    IngressDecision::Drop {
                        reason: "sender_not_allowlisted",
                    }
                }
            }
            DirectMessagePolicy::Pairing => {
                if is_pairing_approved {
                    IngressDecision::Allow
                } else {
                    IngressDecision::RequirePairing
                }
            }
        },
        ChatType::Group | ChatType::Thread => {
            // Per-channel override: specific ID → wildcard "*" → group_policy fallback
            if !access.channels.is_empty() {
                if let Some(cid) = channel_id {
                    let per_channel = access
                        .channels
                        .get(cid)
                        .or_else(|| access.channels.get("*"));

                    if let Some(pc) = per_channel {
                        if !pc.allow {
                            return IngressDecision::Drop {
                                reason: "channel_not_allowed",
                            };
                        }
                        if pc.require_mention == Some(true) && !mentions_bot {
                            return IngressDecision::Drop {
                                reason: "mention_required",
                            };
                        }
                        return IngressDecision::Allow;
                    }
                }
            }

            match access.group_policy {
                GroupPolicy::Disabled => IngressDecision::Drop {
                    reason: "group_disabled",
                },
                GroupPolicy::Open => IngressDecision::Allow,
                GroupPolicy::MentionOnly => {
                    if mentions_bot {
                        IngressDecision::Allow
                    } else {
                        IngressDecision::Drop {
                            reason: "mention_required",
                        }
                    }
                }
                GroupPolicy::Allowlist => {
                    if access
                        .group_allow_from
                        .iter()
                        .any(|value| value == sender_id)
                    {
                        IngressDecision::Allow
                    } else {
                        IngressDecision::Drop {
                            reason: "sender_not_group_allowlisted",
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub defaults: Option<ChannelDefaultsConfig>,
    pub telegram: Option<TelegramConfig>,
    pub discord: Option<DiscordConfig>,
    pub slack: Option<SlackConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub queue: QueueConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub chain: ChainConfig,
    #[serde(default)]
    pub runner: RunnerConfig,
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
    #[serde(default)]
    pub agent_defaults: Option<AgentDefaultsConfig>,
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,
    #[serde(default)]
    pub custom_providers: HashMap<String, CustomProviderConfig>,
    #[serde(default)]
    pub teams: HashMap<String, TeamConfig>,
    #[serde(default)]
    pub bindings: Vec<BindingRule>,
    #[serde(default)]
    pub channels: ChannelsConfig,
    #[serde(default)]
    pub pairing: PairingConfig,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        let mut agents = HashMap::new();
        agents.insert(
            "default".to_string(),
            AgentConfig {
                name: "Default".to_string(),
                provider: ProviderKind::Anthropic,
                model: "claude-sonnet-4-6".to_string(),
                think_level: None,
                provider_id: None,
                system_prompt: None,
                prompt_file: None,
                heartbeat: None,
            },
        );

        Self {
            daemon: DaemonConfig::default(),
            server: ServerConfig::default(),
            queue: QueueConfig::default(),
            session: SessionConfig::default(),
            chain: ChainConfig::default(),
            runner: RunnerConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            agent_defaults: None,
            agents,
            custom_providers: HashMap::new(),
            teams: HashMap::new(),
            bindings: vec![],
            channels: ChannelsConfig::default(),
            pairing: PairingConfig::default(),
        }
    }
}

impl RuntimeConfig {
    pub fn validate(&self) -> Result<()> {
        if self.server.host.trim().is_empty() {
            bail!("server.host must not be empty");
        }
        if self.server.is_public_bind() && !self.server.allow_public_bind {
            bail!(
                "server.host={} requires server.allow_public_bind = true",
                self.server.host
            );
        }
        if self.daemon.max_concurrent_runs == 0 {
            bail!("daemon.max_concurrent_runs must be at least 1");
        }
        if self.heartbeat.interval_sec == 0 {
            bail!("heartbeat.interval_sec must be at least 1");
        }

        for (provider_id, _provider) in &self.custom_providers {
            if self.custom_provider_api_key(provider_id)?.is_none() {
                bail!("custom_providers.{provider_id} requires api_key or api_key_env");
            }
        }

        Ok(())
    }

    pub fn home_dir(&self) -> PathBuf {
        expand_tilde(&self.daemon.home_dir)
    }

    pub fn workspace_dir(&self) -> PathBuf {
        expand_tilde(&self.daemon.workspace_dir)
    }

    pub fn queue_dir(&self) -> PathBuf {
        self.home_dir().join("queue")
    }

    pub fn incoming_dir(&self) -> PathBuf {
        self.queue_dir().join("incoming")
    }

    pub fn processing_dir(&self) -> PathBuf {
        self.queue_dir().join("processing")
    }

    pub fn outgoing_dir(&self) -> PathBuf {
        self.queue_dir().join("outgoing")
    }

    pub fn files_dir(&self) -> PathBuf {
        self.home_dir().join("files")
    }

    pub fn dead_letter_dir(&self) -> PathBuf {
        self.queue_dir().join("dead_letter")
    }

    pub fn state_path(&self) -> PathBuf {
        self.home_dir().join("state").join("clawpod-state.json")
    }

    pub fn event_log_path(&self) -> PathBuf {
        self.home_dir().join("logs").join("events.jsonl")
    }

    pub fn daemon_log_path(&self) -> PathBuf {
        self.home_dir().join("logs").join("daemon.log")
    }

    pub fn daemon_stderr_path(&self) -> PathBuf {
        self.home_dir().join("logs").join("daemon.stderr.log")
    }

    pub fn resolve_agent_workdir(&self, agent_id: &str) -> PathBuf {
        self.workspace_dir().join(agent_id)
    }

    pub fn server_bind_host(&self) -> &str {
        self.server.host.trim()
    }

    pub fn server_listen_addr(&self) -> String {
        format!("{}:{}", self.server_bind_host(), self.server.api_port)
    }

    pub fn office_url(&self) -> String {
        format!(
            "http://{}:{}/office",
            self.server_bind_host(),
            self.server.api_port
        )
    }

    pub fn telegram_bot_token(&self) -> Result<Option<String>> {
        resolve_channel_secret(
            self.channels
                .telegram
                .as_ref()
                .and_then(|channel| channel.bot_token.as_ref()),
            self.channels
                .telegram
                .as_ref()
                .and_then(|channel| channel.bot_token_env.as_ref()),
            "channels.telegram.bot_token",
        )
    }

    pub fn discord_bot_token(&self) -> Result<Option<String>> {
        resolve_channel_secret(
            self.channels
                .discord
                .as_ref()
                .and_then(|channel| channel.bot_token.as_ref()),
            self.channels
                .discord
                .as_ref()
                .and_then(|channel| channel.bot_token_env.as_ref()),
            "channels.discord.bot_token",
        )
    }

    pub fn slack_bot_token(&self) -> Result<Option<String>> {
        resolve_channel_secret(
            self.channels
                .slack
                .as_ref()
                .and_then(|channel| channel.bot_token.as_ref()),
            self.channels
                .slack
                .as_ref()
                .and_then(|channel| channel.bot_token_env.as_ref()),
            "channels.slack.bot_token",
        )
    }

    pub fn slack_app_token(&self) -> Result<Option<String>> {
        resolve_channel_secret(
            self.channels
                .slack
                .as_ref()
                .and_then(|channel| channel.app_token.as_ref()),
            self.channels
                .slack
                .as_ref()
                .and_then(|channel| channel.app_token_env.as_ref()),
            "channels.slack.app_token",
        )
    }

    pub fn custom_provider_api_key(&self, provider_id: &str) -> Result<Option<String>> {
        let provider = self
            .custom_providers
            .get(provider_id)
            .with_context(|| format!("custom provider not found: {provider_id}"))?;
        resolve_secret(
            provider.api_key.as_ref(),
            provider.api_key_env.as_ref(),
            &format!("custom_providers.{provider_id}.api_key"),
        )
    }

    pub fn masked_for_display(&self) -> Self {
        let mut masked = self.clone();

        if let Some(channel) = masked.channels.telegram.as_mut() {
            mask_secret(&mut channel.bot_token);
        }
        if let Some(channel) = masked.channels.discord.as_mut() {
            mask_secret(&mut channel.bot_token);
        }
        if let Some(channel) = masked.channels.slack.as_mut() {
            mask_secret(&mut channel.bot_token);
            mask_secret(&mut channel.app_token);
        }
        for provider in masked.custom_providers.values_mut() {
            mask_secret(&mut provider.api_key);
        }

        masked
    }

    pub fn restore_masked_secrets(&mut self, previous: &Self) {
        if let Some(channel) = self.channels.telegram.as_mut() {
            restore_secret(
                &mut channel.bot_token,
                previous
                    .channels
                    .telegram
                    .as_ref()
                    .and_then(|prev| prev.bot_token.as_ref()),
            );
        }
        if let Some(channel) = self.channels.discord.as_mut() {
            restore_secret(
                &mut channel.bot_token,
                previous
                    .channels
                    .discord
                    .as_ref()
                    .and_then(|prev| prev.bot_token.as_ref()),
            );
        }
        if let Some(channel) = self.channels.slack.as_mut() {
            restore_secret(
                &mut channel.bot_token,
                previous
                    .channels
                    .slack
                    .as_ref()
                    .and_then(|prev| prev.bot_token.as_ref()),
            );
            restore_secret(
                &mut channel.app_token,
                previous
                    .channels
                    .slack
                    .as_ref()
                    .and_then(|prev| prev.app_token.as_ref()),
            );
        }
        for (provider_id, provider) in &mut self.custom_providers {
            restore_secret(
                &mut provider.api_key,
                previous
                    .custom_providers
                    .get(provider_id)
                    .and_then(|prev| prev.api_key.as_ref()),
            );
        }
    }
}

pub fn default_config_path() -> PathBuf {
    expand_tilde("~/.clawpod/clawpod.toml")
}

pub fn load_config(path: Option<PathBuf>) -> Result<RuntimeConfig> {
    let path = path.unwrap_or_else(default_config_path);
    if !path.exists() {
        let config = RuntimeConfig::default();
        config.validate()?;
        write_default_config(&path, &config)?;
        return Ok(config);
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config: {}", path.display()))?;
    let mut parsed: RuntimeConfig =
        toml::from_str(&raw).with_context(|| format!("invalid toml: {}", path.display()))?;

    if parsed.agents.is_empty() {
        parsed.agents = RuntimeConfig::default().agents;
    }

    parsed.validate()?;
    Ok(parsed)
}

pub fn write_default_config(path: &Path, config: &RuntimeConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config dir: {}", parent.display()))?;
    }

    let toml = toml::to_string_pretty(config).context("failed to serialize default config")?;
    fs::write(path, toml).with_context(|| format!("failed to write config: {}", path.display()))?;
    Ok(())
}

pub fn write_config(path: &Path, config: &RuntimeConfig) -> Result<()> {
    config.validate()?;
    write_default_config(path, config)
}

pub fn ensure_runtime_dirs(config: &RuntimeConfig) -> Result<()> {
    let dirs = [
        config.home_dir(),
        config.workspace_dir(),
        config.incoming_dir(),
        config.processing_dir(),
        config.outgoing_dir(),
        config.dead_letter_dir(),
        config.files_dir(),
        config.home_dir().join("runs"),
        config.home_dir().join("logs"),
        config.home_dir().join("events"),
        config.home_dir().join("state"),
    ];

    for dir in dirs {
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create runtime dir: {}", dir.display()))?;
    }

    for agent_id in config.agents.keys() {
        let workdir = config.resolve_agent_workdir(agent_id);
        fs::create_dir_all(&workdir)
            .with_context(|| format!("failed to create agent workdir: {}", workdir.display()))?;
    }

    Ok(())
}

fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(stripped);
        }
    }

    if raw == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }

    PathBuf::from(raw)
}

fn default_server_host() -> String {
    "127.0.0.1".to_string()
}

impl ServerConfig {
    pub fn is_public_bind(&self) -> bool {
        !matches!(self.host.trim(), "127.0.0.1" | "localhost" | "::1")
    }
}

pub const MASKED_SECRET: &str = "***MASKED***";

fn resolve_channel_secret(
    inline: Option<&String>,
    env_name: Option<&String>,
    field_name: &str,
) -> Result<Option<String>> {
    resolve_secret(inline, env_name, field_name)
}

fn resolve_secret(
    inline: Option<&String>,
    env_name: Option<&String>,
    field_name: &str,
) -> Result<Option<String>> {
    if let Some(value) = normalize_secret(inline.cloned()) {
        return Ok(Some(value));
    }

    let Some(env_name) = normalize_plain(env_name.cloned()) else {
        return Ok(None);
    };
    let value = env::var(&env_name)
        .with_context(|| format!("missing env var {env_name} for {field_name}"))?;
    if value.trim().is_empty() {
        bail!("empty env var {env_name} for {field_name}");
    }
    Ok(Some(value))
}

fn normalize_secret(value: Option<String>) -> Option<String> {
    match value {
        Some(value) if !value.trim().is_empty() && value != MASKED_SECRET => Some(value),
        _ => None,
    }
}

fn normalize_plain(value: Option<String>) -> Option<String> {
    match value {
        Some(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

fn mask_secret(value: &mut Option<String>) {
    if value.as_ref().is_some_and(|raw| !raw.trim().is_empty()) {
        *value = Some(MASKED_SECRET.to_string());
    }
}

fn restore_secret(slot: &mut Option<String>, previous: Option<&String>) {
    if slot.as_deref() == Some(MASKED_SECRET) {
        *slot = previous.cloned();
    }
}

// --- Codex CLI credential check ---

const CODEX_AUTH_REFRESH_SKEW_SECS: i64 = 90;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexAuthStatus {
    pub auth_file_exists: bool,
    pub has_access_token: bool,
    pub token_expired: bool,
    pub account_id: Option<String>,
    pub expires_at: Option<String>,
}

impl CodexAuthStatus {
    pub fn is_usable(&self) -> bool {
        self.auth_file_exists && self.has_access_token && !self.token_expired
    }
}

fn codex_auth_path() -> Option<PathBuf> {
    if let Ok(codex_home) = env::var("CODEX_HOME") {
        let p = PathBuf::from(codex_home);
        if !p.as_os_str().is_empty() {
            return Some(p.join("auth.json"));
        }
    }
    home_dir().map(|h| h.join(".codex").join("auth.json"))
}

fn decode_jwt_exp(token: &str) -> Option<i64> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    payload.get("exp")?.as_i64()
}

pub fn check_codex_auth() -> CodexAuthStatus {
    let Some(path) = codex_auth_path() else {
        return CodexAuthStatus {
            auth_file_exists: false,
            has_access_token: false,
            token_expired: false,
            account_id: None,
            expires_at: None,
        };
    };

    if !path.exists() {
        return CodexAuthStatus {
            auth_file_exists: false,
            has_access_token: false,
            token_expired: false,
            account_id: None,
            expires_at: None,
        };
    }

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            return CodexAuthStatus {
                auth_file_exists: true,
                has_access_token: false,
                token_expired: false,
                account_id: None,
                expires_at: None,
            };
        }
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => {
            return CodexAuthStatus {
                auth_file_exists: true,
                has_access_token: false,
                token_expired: false,
                account_id: None,
                expires_at: None,
            };
        }
    };

    let tokens = json.get("tokens");
    let access_token = tokens
        .and_then(|t| t.get("access_token"))
        .and_then(|v| v.as_str());
    let account_id = tokens
        .and_then(|t| t.get("account_id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let Some(access_token) = access_token else {
        return CodexAuthStatus {
            auth_file_exists: true,
            has_access_token: false,
            token_expired: false,
            account_id,
            expires_at: None,
        };
    };

    let (token_expired, expires_at) = if let Some(exp) = decode_jwt_exp(access_token) {
        let now = chrono::Utc::now().timestamp();
        let expired = now >= exp - CODEX_AUTH_REFRESH_SKEW_SECS;
        let expires_str = chrono::DateTime::<chrono::Utc>::from_timestamp(exp, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| exp.to_string());
        (expired, Some(expires_str))
    } else {
        (false, None)
    };

    CodexAuthStatus {
        auth_file_exists: true,
        has_access_token: true,
        token_expired,
        account_id,
        expires_at,
    }
}

// --- Claude Code credential check ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeAuthStatus {
    pub logged_in: bool,
    pub email: Option<String>,
    pub org_name: Option<String>,
    pub subscription_type: Option<String>,
}

impl ClaudeAuthStatus {
    pub fn is_usable(&self) -> bool {
        self.logged_in
    }

    fn not_available() -> Self {
        Self {
            logged_in: false,
            email: None,
            org_name: None,
            subscription_type: None,
        }
    }
}

/// Check Claude Code login status by running `claude auth status`.
///
/// Returns auth details parsed from the CLI JSON output.
/// Falls back to not-available on timeout (5s) or parse errors.
pub fn check_claude_auth() -> ClaudeAuthStatus {
    let mut cmd = std::process::Command::new("claude");
    cmd.args(["auth", "status"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return ClaudeAuthStatus::not_available(),
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return ClaudeAuthStatus::not_available();
                }
                let Some(stdout) = child.stdout.take() else {
                    return ClaudeAuthStatus::not_available();
                };
                let json: serde_json::Value =
                    match serde_json::from_reader(std::io::BufReader::new(stdout)) {
                        Ok(v) => v,
                        Err(_) => return ClaudeAuthStatus::not_available(),
                    };
                let logged_in = json
                    .get("loggedIn")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                return ClaudeAuthStatus {
                    logged_in,
                    email: json
                        .get("email")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    org_name: json
                        .get("orgName")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    subscription_type: json
                        .get("subscriptionType")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return ClaudeAuthStatus::not_available();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return ClaudeAuthStatus::not_available(),
        }
    }
}

pub fn read_codex_access_token() -> Option<String> {
    let path = codex_auth_path()?;
    let content = fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let access_token = json.get("tokens")?.get("access_token")?.as_str()?;

    if let Some(exp) = decode_jwt_exp(access_token) {
        let now = chrono::Utc::now().timestamp();
        if now >= exp - CODEX_AUTH_REFRESH_SKEW_SECS {
            return None;
        }
    }

    Some(access_token.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        decode_jwt_exp, evaluate_ingress_policy, parse_duration_str, ChannelAccessConfig,
        ClaudeAuthStatus, DirectMessagePolicy, GroupPolicy, IngressDecision,
        PerChannelAccessConfig, RuntimeConfig,
    };
    use domain::ChatType;

    #[test]
    fn allowlist_dm_requires_explicit_sender() {
        let access = ChannelAccessConfig {
            dm_policy: DirectMessagePolicy::Allowlist,
            allow_from: vec!["U123".to_string()],
            ..ChannelAccessConfig::default()
        };

        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Direct, "U123", false, false, None),
            IngressDecision::Allow
        );
        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Direct, "U999", false, false, None),
            IngressDecision::Drop {
                reason: "sender_not_allowlisted",
            }
        );
    }

    #[test]
    fn pairing_dm_requires_approval() {
        let access = ChannelAccessConfig {
            dm_policy: DirectMessagePolicy::Pairing,
            ..ChannelAccessConfig::default()
        };

        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Direct, "U123", false, false, None),
            IngressDecision::RequirePairing
        );
        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Direct, "U123", false, true, None),
            IngressDecision::Allow
        );
    }

    #[test]
    fn mention_only_group_requires_mention() {
        let access = ChannelAccessConfig {
            group_policy: GroupPolicy::MentionOnly,
            ..ChannelAccessConfig::default()
        };

        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U123", false, false, None),
            IngressDecision::Drop {
                reason: "mention_required",
            }
        );
        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U123", true, false, None),
            IngressDecision::Allow
        );
    }

    #[test]
    fn per_channel_specific_allows_wildcard_denies() {
        let mut channels = HashMap::new();
        channels.insert(
            "C123".to_string(),
            PerChannelAccessConfig {
                allow: true,
                require_mention: None,
            },
        );
        channels.insert(
            "*".to_string(),
            PerChannelAccessConfig {
                allow: false,
                require_mention: None,
            },
        );
        let access = ChannelAccessConfig {
            group_policy: GroupPolicy::Disabled,
            channels,
            ..ChannelAccessConfig::default()
        };

        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U1", false, false, Some("C123")),
            IngressDecision::Allow
        );
        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U1", false, false, Some("C999")),
            IngressDecision::Drop {
                reason: "channel_not_allowed"
            }
        );
    }

    #[test]
    fn per_channel_require_mention() {
        let mut channels = HashMap::new();
        channels.insert(
            "C123".to_string(),
            PerChannelAccessConfig {
                allow: true,
                require_mention: Some(true),
            },
        );
        let access = ChannelAccessConfig {
            group_policy: GroupPolicy::Open,
            channels,
            ..ChannelAccessConfig::default()
        };

        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U1", false, false, Some("C123")),
            IngressDecision::Drop {
                reason: "mention_required"
            }
        );
        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U1", true, false, Some("C123")),
            IngressDecision::Allow
        );
    }

    #[test]
    fn empty_channels_falls_through_to_group_policy() {
        let access = ChannelAccessConfig {
            group_policy: GroupPolicy::MentionOnly,
            ..ChannelAccessConfig::default()
        };

        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U1", false, false, Some("C123")),
            IngressDecision::Drop {
                reason: "mention_required"
            }
        );
        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U1", true, false, Some("C123")),
            IngressDecision::Allow
        );
    }

    #[test]
    fn unmatched_channel_falls_through_to_group_policy() {
        let mut channels = HashMap::new();
        channels.insert(
            "C123".to_string(),
            PerChannelAccessConfig {
                allow: true,
                require_mention: None,
            },
        );
        let access = ChannelAccessConfig {
            group_policy: GroupPolicy::Disabled,
            channels,
            ..ChannelAccessConfig::default()
        };

        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U1", false, false, Some("C123")),
            IngressDecision::Allow
        );
        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Group, "U1", false, false, Some("C999")),
            IngressDecision::Drop {
                reason: "group_disabled"
            }
        );
    }

    #[test]
    fn dm_ignores_per_channel_config() {
        let mut channels = HashMap::new();
        channels.insert(
            "*".to_string(),
            PerChannelAccessConfig {
                allow: false,
                require_mention: None,
            },
        );
        let access = ChannelAccessConfig {
            dm_policy: DirectMessagePolicy::Open,
            channels,
            ..ChannelAccessConfig::default()
        };

        assert_eq!(
            evaluate_ingress_policy(&access, ChatType::Direct, "U1", false, false, Some("C123")),
            IngressDecision::Allow
        );
    }

    #[test]
    fn per_channel_access_parses_from_toml() {
        let toml_str = r#"
bindings = []

[daemon]
home_dir = "/tmp/test"
workspace_dir = "/tmp/test/workspace"
poll_interval_ms = 1000
max_concurrent_runs = 2

[channels.discord]
bot_token = "test"

[channels.discord.access]
group_policy = "open"

[channels.discord.access.channels."1486343914892820500"]
allow = true

[channels.discord.access.channels."*"]
allow = false
"#;
        let config: RuntimeConfig = toml::from_str(toml_str).unwrap();
        let discord = config.channels.discord.unwrap();
        let access = discord.effective_access();
        assert_eq!(access.channels.len(), 2);
        assert!(access.channels.get("1486343914892820500").unwrap().allow);
        assert!(!access.channels.get("*").unwrap().allow);
    }

    #[test]
    fn decode_jwt_exp_valid() {
        // JWT with payload: {"exp": 9999999999}
        // header: {"alg":"none"}
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(r#"{"exp":9999999999}"#);
        let token = format!("{header}.{payload}.sig");

        assert_eq!(decode_jwt_exp(&token), Some(9_999_999_999));
    }

    #[test]
    fn decode_jwt_exp_malformed() {
        assert_eq!(decode_jwt_exp("not-a-jwt"), None);
        assert_eq!(decode_jwt_exp("a.!!!invalid-base64.c"), None);
        assert_eq!(decode_jwt_exp(""), None);
    }

    #[test]
    fn decode_jwt_exp_missing_exp_field() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(r#"{"sub":"user123"}"#);
        let token = format!("{header}.{payload}.sig");

        assert_eq!(decode_jwt_exp(&token), None);
    }

    #[test]
    fn codex_auth_missing_file() {
        // Point to a nonexistent path
        std::env::set_var("CODEX_HOME", "/tmp/clawpod-test-nonexistent-dir");
        let status = super::check_codex_auth();
        assert!(!status.auth_file_exists);
        assert!(!status.has_access_token);
        assert!(!status.is_usable());
        std::env::remove_var("CODEX_HOME");
    }

    #[test]
    fn codex_auth_valid_token() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let tmp = std::env::temp_dir().join("clawpod-test-codex-valid");
        let _ = std::fs::create_dir_all(&tmp);

        let exp = chrono::Utc::now().timestamp() + 3600;
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp},"sub":"test"}}"#));
        let token = format!("{header}.{payload}.sig");

        let auth_json = serde_json::json!({
            "tokens": {
                "access_token": token,
                "refresh_token": "refresh",
                "account_id": "test-account"
            }
        });
        std::fs::write(tmp.join("auth.json"), auth_json.to_string()).unwrap();

        std::env::set_var("CODEX_HOME", tmp.to_str().unwrap());
        let status = super::check_codex_auth();
        assert!(status.auth_file_exists);
        assert!(status.has_access_token);
        assert!(!status.token_expired);
        assert_eq!(status.account_id.as_deref(), Some("test-account"));
        assert!(status.is_usable());

        let access = super::read_codex_access_token();
        assert!(access.is_some());

        std::env::remove_var("CODEX_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration_str("60s").unwrap().as_secs(), 60);
        assert_eq!(parse_duration_str("120").unwrap().as_secs(), 120);
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration_str("30m").unwrap().as_secs(), 1800);
        assert_eq!(parse_duration_str("1m").unwrap().as_secs(), 60);
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration_str("1h").unwrap().as_secs(), 3600);
        assert_eq!(parse_duration_str("2h").unwrap().as_secs(), 7200);
    }

    #[test]
    fn parse_duration_rejects_zero() {
        assert!(parse_duration_str("0m").is_err());
        assert!(parse_duration_str("0").is_err());
    }

    #[test]
    fn parse_duration_rejects_empty() {
        assert!(parse_duration_str("").is_err());
        assert!(parse_duration_str("  ").is_err());
    }

    #[test]
    fn parse_duration_rejects_invalid() {
        assert!(parse_duration_str("abc").is_err());
        assert!(parse_duration_str("10x").is_err());
    }

    #[test]
    fn config_with_agent_heartbeat_parses() {
        let toml = r#"
[agents.default]
name = "Default"
model = "claude-sonnet-4-6"

[agents.default.heartbeat]
every = "30m"
target = "last"
ack_max_chars = 200
"#;
        let config: RuntimeConfig = toml::from_str(toml).unwrap();
        let agent = config.agents.get("default").unwrap();
        let hb = agent.heartbeat.as_ref().unwrap();
        assert_eq!(hb.every.as_deref(), Some("30m"));
        assert_eq!(hb.target, Some(domain::HeartbeatTarget::Last));
        assert_eq!(hb.ack_max_chars, Some(200));
    }

    #[test]
    fn config_with_agent_defaults_heartbeat_parses() {
        let toml = r#"
[agent_defaults.heartbeat]
every = "1h"
target = "none"
prompt = "Check status"
ack_max_chars = 300
direct_policy = "block"
light_context = true
isolated_session = false
"#;
        let config: RuntimeConfig = toml::from_str(toml).unwrap();
        let defaults = config.agent_defaults.unwrap();
        let hb = defaults.heartbeat.unwrap();
        assert_eq!(hb.every.as_deref(), Some("1h"));
        assert_eq!(hb.target, Some(domain::HeartbeatTarget::None));
        assert_eq!(hb.prompt.as_deref(), Some("Check status"));
        assert_eq!(hb.direct_policy, Some(domain::HeartbeatDirectPolicy::Block));
        assert_eq!(hb.light_context, Some(true));
    }

    #[test]
    fn config_with_channel_heartbeat_visibility_parses() {
        let toml = r#"
[channels.defaults.heartbeat]
show_ok = false
show_alerts = true
use_indicator = true

[channels.telegram]
bot_token = "test"

[channels.telegram.heartbeat]
show_ok = true
"#;
        let config: RuntimeConfig = toml::from_str(toml).unwrap();
        let defaults_hb = config
            .channels
            .defaults
            .as_ref()
            .unwrap()
            .heartbeat
            .as_ref()
            .unwrap();
        assert_eq!(defaults_hb.show_ok, Some(false));
        assert_eq!(defaults_hb.show_alerts, Some(true));

        let telegram_hb = config
            .channels
            .telegram
            .as_ref()
            .unwrap()
            .heartbeat
            .as_ref()
            .unwrap();
        assert_eq!(telegram_hb.show_ok, Some(true));
    }

    #[test]
    fn config_backward_compat_old_heartbeat_only() {
        let toml = r#"
[heartbeat]
enabled = true
interval_sec = 600
sender = "hb-bot"
"#;
        let config: RuntimeConfig = toml::from_str(toml).unwrap();
        assert!(config.heartbeat.enabled);
        assert_eq!(config.heartbeat.interval_sec, 600);
        assert_eq!(config.heartbeat.sender, "hb-bot");
        assert!(config.agent_defaults.is_none());
    }

    #[test]
    fn codex_auth_expired_token() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let tmp = std::env::temp_dir().join("clawpod-test-codex-expired");
        let _ = std::fs::create_dir_all(&tmp);

        let exp = chrono::Utc::now().timestamp() - 100;
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        let token = format!("{header}.{payload}.sig");

        let auth_json = serde_json::json!({
            "tokens": { "access_token": token }
        });
        std::fs::write(tmp.join("auth.json"), auth_json.to_string()).unwrap();

        std::env::set_var("CODEX_HOME", tmp.to_str().unwrap());
        let status = super::check_codex_auth();
        assert!(status.auth_file_exists);
        assert!(status.has_access_token);
        assert!(status.token_expired);
        assert!(!status.is_usable());

        let access = super::read_codex_access_token();
        assert!(access.is_none());

        std::env::remove_var("CODEX_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn claude_auth_not_available_is_not_usable() {
        let status = ClaudeAuthStatus::not_available();
        assert!(!status.logged_in);
        assert!(!status.is_usable());
        assert!(status.email.is_none());
        assert!(status.org_name.is_none());
        assert!(status.subscription_type.is_none());
    }

    #[test]
    fn claude_auth_logged_in_is_usable() {
        let status = ClaudeAuthStatus {
            logged_in: true,
            email: Some("user@example.com".into()),
            org_name: Some("Test Org".into()),
            subscription_type: Some("max".into()),
        };
        assert!(status.is_usable());
    }
}
