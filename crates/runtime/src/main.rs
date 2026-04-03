mod heartbeat;
mod service;

use std::collections::HashSet;
use std::env;
use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ::heartbeat::HeartbeatService;
use agent::reset_agent_workspace;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use config::{
    default_config_path, ensure_runtime_dirs, load_config, ChannelAccessConfig,
    DirectMessagePolicy, DiscordConfig, GroupPolicy, PerChannelAccessConfig, RuntimeConfig,
    SlackConfig, TelegramConfig,
};
use domain::{
    ActiveHoursConfig, AgentBrowserConfig, AgentConfig, AgentHeartbeatConfig, BindingMatch,
    BindingRule, ChatType, HeartbeatDirectPolicy, HeartbeatTarget, ProviderKind, ThinkLevel,
};
use observer::{
    bump_component_restart, log_startup_banner, mark_component_disabled, mark_component_error,
    mark_component_ok, FileEventSink,
};
use queue::{enqueue_message, EnqueueMessage, QueueProcessor};
use runner::CliRunner;
use serde_json::Value;
use store::StateStore;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout, Duration};
use tracing::info;

#[derive(Debug, Parser)]
#[command(name = "clawpod")]
#[command(about = "ClawPod daemon and queue controls")]
struct Cli {
    /// Config file path. Defaults to ~/.clawpod/clawpod.toml
    #[arg(long)]
    config: Option<PathBuf>,

    /// Tracing filter (info,debug,...)
    #[arg(long, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run queue daemon
    Daemon,

    /// Print runtime status summary
    Status,

    /// Print runtime health from the running office server
    Health,

    /// Tail runtime logs
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,

        #[arg(long)]
        follow: bool,

        #[arg(long, value_enum, default_value = "daemon")]
        source: LogSourceArg,
    },

    /// Install or control background service
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },

    /// Enqueue a message into incoming queue
    Enqueue {
        #[arg(long)]
        channel: String,

        #[arg(long)]
        sender: String,

        #[arg(long)]
        sender_id: String,

        #[arg(long)]
        message: String,

        #[arg(long)]
        peer_id: String,

        #[arg(long)]
        account_id: Option<String>,

        #[arg(long)]
        agent: Option<String>,

        #[arg(long)]
        message_id: Option<String>,

        #[arg(long, value_enum, default_value = "direct")]
        chat_type: ChatTypeArg,
    },

    /// Health checks for local runtime
    Doctor,

    /// Run the Office API and dashboard only
    Office,

    /// Reset persisted sessions for an agent
    Reset {
        #[arg(long)]
        agent: String,
    },

    /// Manage sender pairing
    Pairing {
        #[command(subcommand)]
        command: PairingCommand,
    },

    /// Manage provider authentication
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },

    /// Manage heartbeat automation
    Heartbeat {
        #[command(subcommand)]
        command: HeartbeatCommand,
    },

    /// Manage agents
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },

    /// Manage teams
    Team {
        #[command(subcommand)]
        command: TeamCommand,
    },

    /// Manage bindings
    Binding {
        #[command(subcommand)]
        command: BindingCommand,
    },

    /// Manage channel access rules
    Access {
        #[command(subcommand)]
        command: AccessCommand,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum PairingCommand {
    /// List pending pairing requests
    List,
    /// Approve a sender by pairing code
    Approve {
        /// The pairing code shared by the sender
        code: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AuthCommand {
    /// Login to Anthropic via claude CLI
    Claude,
    /// Login to OpenAI via codex CLI
    Openai,
    /// Show authentication status for all providers
    Status,
}

#[derive(Debug, Clone, Subcommand)]
pub enum HeartbeatCommand {
    /// Show the last heartbeat run for each agent
    Last {
        /// Filter by agent id
        #[arg(long)]
        agent: Option<String>,
    },
    /// Trigger a heartbeat run for an agent
    Run {
        /// Agent id to run heartbeat for
        #[arg(long)]
        agent: String,
    },
    /// Enable heartbeat in config
    Enable,
    /// Disable heartbeat in config
    Disable,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ChatTypeArg {
    Direct,
    Group,
    Thread,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LogSourceArg {
    Daemon,
    Stderr,
    Events,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ServiceCommand {
    Install,
    Start,
    Stop,
    Restart,
    Status,
    Uninstall,
}

#[derive(Debug, Clone, Subcommand)]
enum AgentCommand {
    /// List all configured agents
    #[command(alias = "ls")]
    List,
    /// Add a new agent
    Add(AgentAddArgs),
    /// Edit an existing agent
    Edit(AgentEditArgs),
    /// Remove an agent
    #[command(alias = "rm")]
    Remove(AgentRemoveArgs),
    /// Show agent configuration
    Show {
        /// Agent ID
        id: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum TeamCommand {
    /// List teams
    #[command(alias = "ls")]
    List,
    /// Show team configuration
    Show {
        /// Team ID
        id: String,
    },
    /// Add an agent to a team
    AddAgent {
        /// Team ID
        team: String,
        /// Agent ID
        agent: String,
    },
    /// Remove an agent from a team
    RemoveAgent {
        /// Team ID
        team: String,
        /// Agent ID
        agent: String,
    },
    /// Set the leader for a team
    SetLeader {
        /// Team ID
        team: String,
        /// Agent ID
        agent: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum BindingCommand {
    /// List bindings
    #[command(alias = "ls")]
    List,
    /// Show a binding
    Show {
        /// Binding index
        index: usize,
    },
    /// Add a binding
    Add(BindingAddArgs),
    /// Update a binding
    Update(BindingUpdateArgs),
    /// Remove a binding
    #[command(alias = "rm")]
    Remove {
        /// Binding index
        index: usize,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum AccessCommand {
    /// List configured per-channel access overrides
    List {
        /// Restrict to one channel adapter
        #[arg(long, value_enum)]
        channel: Option<ChannelKindArg>,
    },
    /// Allow a specific channel ID
    AllowChannel {
        /// Channel adapter
        #[arg(long, value_enum)]
        channel: ChannelKindArg,
        /// Channel ID or "*"
        id: String,
        /// Whether mentions are required for this channel override
        #[arg(long)]
        require_mention: Option<bool>,
    },
    /// Deny a specific channel ID
    DenyChannel {
        /// Channel adapter
        #[arg(long, value_enum)]
        channel: ChannelKindArg,
        /// Channel ID or "*"
        id: String,
        /// Optional mention requirement override
        #[arg(long)]
        require_mention: Option<bool>,
    },
    /// Remove a specific channel override
    RemoveChannel {
        /// Channel adapter
        #[arg(long, value_enum)]
        channel: ChannelKindArg,
        /// Channel ID or "*"
        id: String,
    },
}

#[derive(Debug, Clone, Args)]
struct AgentAddArgs {
    /// Agent ID (alphanumeric, _, -)
    id: String,
    /// Display name (defaults to ID)
    #[arg(long)]
    name: Option<String>,
    /// Provider
    #[arg(long, value_enum, default_value = "anthropic")]
    provider: ProviderKindArg,
    /// Model ID
    #[arg(long, default_value = "claude-sonnet-4-6")]
    model: String,
    /// Think level
    #[arg(long, value_enum)]
    think_level: Option<ThinkLevelArg>,
    /// Custom provider ID (required when provider is custom)
    #[arg(long)]
    provider_id: Option<String>,
    /// Inline system prompt text
    #[arg(long)]
    system_prompt: Option<String>,
    /// Prompt file path (relative to agent workspace or absolute)
    #[arg(long)]
    prompt_file: Option<String>,
    /// Browser profile name
    #[arg(long)]
    browser_profile: Option<String>,
    #[command(flatten)]
    heartbeat: HeartbeatArgs,
}

#[derive(Debug, Clone, Args)]
struct AgentEditArgs {
    /// Agent ID
    id: String,
    /// Updated display name
    #[arg(long)]
    name: Option<String>,
    /// Updated provider
    #[arg(long, value_enum)]
    provider: Option<ProviderKindArg>,
    /// Updated model
    #[arg(long)]
    model: Option<String>,
    /// Updated think level
    #[arg(long, value_enum)]
    think_level: Option<ThinkLevelArg>,
    /// Clear think level override
    #[arg(long, action = ArgAction::SetTrue)]
    clear_think_level: bool,
    /// Updated custom provider ID
    #[arg(long)]
    provider_id: Option<String>,
    /// Clear custom provider ID
    #[arg(long, action = ArgAction::SetTrue)]
    clear_provider_id: bool,
    /// Updated inline system prompt
    #[arg(long)]
    system_prompt: Option<String>,
    /// Clear inline system prompt
    #[arg(long, action = ArgAction::SetTrue)]
    clear_system_prompt: bool,
    /// Updated prompt file path
    #[arg(long)]
    prompt_file: Option<String>,
    /// Clear prompt file path
    #[arg(long, action = ArgAction::SetTrue)]
    clear_prompt_file: bool,
    /// Updated browser profile
    #[arg(long)]
    browser_profile: Option<String>,
    /// Clear browser profile override
    #[arg(long, action = ArgAction::SetTrue)]
    clear_browser_profile: bool,
    /// Remove all heartbeat overrides for this agent
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat: bool,
    #[command(flatten)]
    heartbeat: HeartbeatArgs,
    #[command(flatten)]
    heartbeat_clear: HeartbeatClearArgs,
}

#[derive(Debug, Clone, Args)]
struct AgentRemoveArgs {
    /// Agent ID to remove
    id: String,
    /// Move the workspace into an archive directory instead of leaving it in place
    #[arg(long, action = ArgAction::SetTrue)]
    archive_workspace: bool,
}

#[derive(Debug, Clone, Args, Default)]
struct HeartbeatArgs {
    #[arg(long)]
    heartbeat_every: Option<String>,
    #[arg(long)]
    heartbeat_model: Option<String>,
    #[arg(long)]
    heartbeat_prompt: Option<String>,
    #[arg(long, value_enum)]
    heartbeat_target: Option<HeartbeatTargetArg>,
    #[arg(long)]
    heartbeat_to: Option<String>,
    #[arg(long)]
    heartbeat_account_id: Option<String>,
    #[arg(long)]
    heartbeat_ack_max_chars: Option<usize>,
    #[arg(long, value_enum)]
    heartbeat_direct_policy: Option<HeartbeatDirectPolicyArg>,
    #[arg(long)]
    heartbeat_include_reasoning: Option<bool>,
    #[arg(long)]
    heartbeat_light_context: Option<bool>,
    #[arg(long)]
    heartbeat_isolated_session: Option<bool>,
    #[arg(long)]
    heartbeat_active_start: Option<String>,
    #[arg(long)]
    heartbeat_active_end: Option<String>,
    #[arg(long)]
    heartbeat_active_timezone: Option<String>,
}

#[derive(Debug, Clone, Args, Default)]
struct HeartbeatClearArgs {
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_every: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_model: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_prompt: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_target: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_to: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_account_id: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_ack_max_chars: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_direct_policy: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_include_reasoning: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_light_context: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_isolated_session: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_heartbeat_active_hours: bool,
}

#[derive(Debug, Clone, Args)]
struct BindingAddArgs {
    /// Target agent ID
    #[arg(long)]
    agent: String,
    #[arg(long, value_enum)]
    channel: Option<ChannelKindArg>,
    #[arg(long)]
    account_id: Option<String>,
    #[arg(long)]
    peer_id: Option<String>,
    #[arg(long)]
    group_id: Option<String>,
    #[arg(long)]
    thread_id: Option<String>,
}

#[derive(Debug, Clone, Args)]
struct BindingUpdateArgs {
    /// Binding index
    index: usize,
    /// Updated target agent ID
    #[arg(long)]
    agent: Option<String>,
    #[arg(long, value_enum)]
    channel: Option<ChannelKindArg>,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_channel: bool,
    #[arg(long)]
    account_id: Option<String>,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_account_id: bool,
    #[arg(long)]
    peer_id: Option<String>,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_peer_id: bool,
    #[arg(long)]
    group_id: Option<String>,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_group_id: bool,
    #[arg(long)]
    thread_id: Option<String>,
    #[arg(long, action = ArgAction::SetTrue)]
    clear_thread_id: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProviderKindArg {
    Anthropic,
    Openai,
    Custom,
    Mock,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum HeartbeatTargetArg {
    None,
    Last,
    Telegram,
    Discord,
    Slack,
    Chatroom,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum HeartbeatDirectPolicyArg {
    Allow,
    Block,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ChannelKindArg {
    Discord,
    Slack,
    Telegram,
}

impl From<ProviderKindArg> for ProviderKind {
    fn from(value: ProviderKindArg) -> Self {
        match value {
            ProviderKindArg::Anthropic => ProviderKind::Anthropic,
            ProviderKindArg::Openai => ProviderKind::Openai,
            ProviderKindArg::Custom => ProviderKind::Custom,
            ProviderKindArg::Mock => ProviderKind::Mock,
        }
    }
}

impl From<HeartbeatTargetArg> for HeartbeatTarget {
    fn from(value: HeartbeatTargetArg) -> Self {
        match value {
            HeartbeatTargetArg::None => HeartbeatTarget::None,
            HeartbeatTargetArg::Last => HeartbeatTarget::Last,
            HeartbeatTargetArg::Telegram => HeartbeatTarget::Telegram,
            HeartbeatTargetArg::Discord => HeartbeatTarget::Discord,
            HeartbeatTargetArg::Slack => HeartbeatTarget::Slack,
            HeartbeatTargetArg::Chatroom => HeartbeatTarget::Chatroom,
        }
    }
}

impl From<HeartbeatDirectPolicyArg> for HeartbeatDirectPolicy {
    fn from(value: HeartbeatDirectPolicyArg) -> Self {
        match value {
            HeartbeatDirectPolicyArg::Allow => HeartbeatDirectPolicy::Allow,
            HeartbeatDirectPolicyArg::Block => HeartbeatDirectPolicy::Block,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ThinkLevelArg {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Adaptive,
}

impl From<ThinkLevelArg> for ThinkLevel {
    fn from(value: ThinkLevelArg) -> Self {
        match value {
            ThinkLevelArg::Off => ThinkLevel::Off,
            ThinkLevelArg::Minimal => ThinkLevel::Minimal,
            ThinkLevelArg::Low => ThinkLevel::Low,
            ThinkLevelArg::Medium => ThinkLevel::Medium,
            ThinkLevelArg::High => ThinkLevel::High,
            ThinkLevelArg::Xhigh => ThinkLevel::Xhigh,
            ThinkLevelArg::Adaptive => ThinkLevel::Adaptive,
        }
    }
}

impl From<ChatTypeArg> for ChatType {
    fn from(value: ChatTypeArg) -> Self {
        match value {
            ChatTypeArg::Direct => ChatType::Direct,
            ChatTypeArg::Group => ChatType::Group,
            ChatTypeArg::Thread => ChatType::Thread,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&cli.log_level)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config_path = cli.config.clone().unwrap_or_else(default_config_path);
    let config = load_config(Some(config_path.clone()))?;
    ensure_runtime_dirs(&config)?;

    match cli.command {
        Commands::Daemon => run_daemon(config, config_path).await?,
        Commands::Status => status(&config, &config_path).await?,
        Commands::Health => health(&config).await?,
        Commands::Logs {
            lines,
            follow,
            source,
        } => logs(&config, source, lines, follow).await?,
        Commands::Service { command } => service::handle_command(&command, &config, &config_path)?,
        Commands::Enqueue {
            channel,
            sender,
            sender_id,
            message,
            peer_id,
            account_id,
            agent,
            message_id,
            chat_type,
        } => {
            let message_id =
                message_id.unwrap_or_else(|| format!("{}", uuid::Uuid::new_v4().simple()));
            let msg = EnqueueMessage {
                channel,
                sender,
                sender_id,
                message,
                message_id,
                timestamp_ms: Utc::now().timestamp_millis(),
                chat_type: chat_type.into(),
                peer_id,
                account_id,
                pre_routed_agent: agent,
                from_agent: None,
                files: vec![],
                chain_depth: 0,
            };
            let path = enqueue_message(&config, msg).await?;
            info!(path = %path.display(), "queued message");
        }
        Commands::Doctor => doctor(&config, &config_path).await?,
        Commands::Office => {
            log_startup_banner(&config.home_dir());
            let sink = FileEventSink::new(config.event_log_path())?;
            let store = StateStore::new(config.state_path())?;
            let run_events = Arc::new(store::RunEventBuffer::new(50, 500));
            server::run(config, config_path, store, sink, None, None, run_events).await?;
        }
        Commands::Reset { agent } => reset(&config, &agent)?,
        Commands::Pairing { command } => pairing_cmd(&config, &command)?,
        Commands::Auth { command } => auth_cmd(&command).await?,
        Commands::Heartbeat { command } => heartbeat_cmd(&config, &config_path, &command).await?,
        Commands::Agent { command } => agent_cmd(&config, &config_path, &command)?,
        Commands::Team { command } => team_cmd(&config, &config_path, &command)?,
        Commands::Binding { command } => binding_cmd(&config, &config_path, &command)?,
        Commands::Access { command } => access_cmd(&config, &config_path, &command)?,
    }

    Ok(())
}

async fn run_daemon(config: RuntimeConfig, config_path: PathBuf) -> Result<()> {
    log_startup_banner(&config.home_dir());
    mark_component_ok("daemon");

    let sink = FileEventSink::new(config.event_log_path())?;
    let store = StateStore::new(config.state_path())?;
    let runner = Arc::new(CliRunner::new(config.runner.timeout_sec));
    let config_arc = Arc::new(config.clone());
    let run_events = Arc::new(store::RunEventBuffer::new(50, 500));
    let processor = QueueProcessor::new(
        config.clone(),
        runner.clone(),
        store.clone(),
        sink.clone(),
        run_events.clone(),
    );
    let mut tasks = JoinSet::new();

    spawn_component(
        &mut tasks,
        "queue",
        async move { processor.run_forever().await },
    );

    // Create HeartbeatService (shared between heartbeat loop and server)
    let heartbeat_control = ::heartbeat::HeartbeatLoopControl::new(&config);
    let heartbeat_service = Arc::new(HeartbeatService::new(
        config_arc.clone(),
        runner.clone(),
        store.clone(),
        sink.clone(),
    ));

    let hb_service = heartbeat_service.clone();
    let hb_control = heartbeat_control.clone();
    let hb_store = store.clone();
    let hb_sink = sink.clone();
    spawn_component(&mut tasks, "heartbeat", async move {
        heartbeat::run_loop(hb_service, hb_control, hb_store, hb_sink).await
    });

    if config.server.enabled {
        let server_config = config.clone();
        let server_path = config_path.clone();
        let server_store = StateStore::new(server_config.state_path())?;
        let server_sink = FileEventSink::new(server_config.event_log_path())?;
        let server_hb = heartbeat_service.clone();
        let server_hb_control = heartbeat_control.clone();
        let server_run_events = run_events.clone();
        spawn_component(&mut tasks, "office", async move {
            server::run(
                server_config,
                server_path,
                server_store,
                server_sink,
                Some(server_hb),
                Some(server_hb_control),
                server_run_events,
            )
            .await
        });
    } else {
        mark_component_disabled("office", "server disabled");
    }

    let telegram_config = config.clone();
    spawn_component(&mut tasks, "telegram", async move {
        telegram::run(telegram_config).await
    });

    let discord_config = config.clone();
    spawn_component(&mut tasks, "discord", async move {
        discord::run(discord_config).await
    });

    let slack_config = config.clone();
    spawn_component(&mut tasks, "slack", async move {
        slack::run(slack_config).await
    });

    loop {
        tokio::select! {
            maybe_result = tasks.join_next() => {
                match maybe_result {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(err))) => {
                        mark_component_error("daemon", err.to_string());
                        return Err(err);
                    }
                    Some(Err(err)) => {
                        let err = anyhow!(err);
                        mark_component_error("daemon", err.to_string());
                        return Err(err);
                    }
                    None => break,
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown signal received");
                break;
            }
        }
    }

    Ok(())
}

fn spawn_component<F>(tasks: &mut JoinSet<Result<()>>, component: &'static str, future: F)
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    bump_component_restart(component);
    tasks.spawn(async move {
        mark_component_ok(component);
        let result = future.await;
        if let Err(err) = &result {
            mark_component_error(component, err.to_string());
        }
        result
    });
}

fn agent_cmd(config: &RuntimeConfig, config_path: &Path, command: &AgentCommand) -> Result<()> {
    print_config_context(config_path, config);
    match command {
        AgentCommand::List => agent_list(config),
        AgentCommand::Add(args) => agent_add(config, config_path, args),
        AgentCommand::Edit(args) => agent_edit(config, config_path, args),
        AgentCommand::Remove(args) => agent_remove(config, config_path, args),
        AgentCommand::Show { id } => agent_show(config, id),
    }
}

fn team_cmd(config: &RuntimeConfig, config_path: &Path, command: &TeamCommand) -> Result<()> {
    print_config_context(config_path, config);
    match command {
        TeamCommand::List => team_list(config),
        TeamCommand::Show { id } => team_show(config, id),
        TeamCommand::AddAgent { team, agent } => team_add_agent(config, config_path, team, agent),
        TeamCommand::RemoveAgent { team, agent } => {
            team_remove_agent(config, config_path, team, agent)
        }
        TeamCommand::SetLeader { team, agent } => team_set_leader(config, config_path, team, agent),
    }
}

fn binding_cmd(config: &RuntimeConfig, config_path: &Path, command: &BindingCommand) -> Result<()> {
    print_config_context(config_path, config);
    match command {
        BindingCommand::List => binding_list(config),
        BindingCommand::Show { index } => binding_show(config, *index),
        BindingCommand::Add(args) => binding_add(config, config_path, args),
        BindingCommand::Update(args) => binding_update(config, config_path, args),
        BindingCommand::Remove { index } => binding_remove(config, config_path, *index),
    }
}

fn access_cmd(config: &RuntimeConfig, config_path: &Path, command: &AccessCommand) -> Result<()> {
    print_config_context(config_path, config);
    match command {
        AccessCommand::List { channel } => access_list(config, *channel),
        AccessCommand::AllowChannel {
            channel,
            id,
            require_mention,
        } => access_set_channel(config, config_path, *channel, id, true, *require_mention),
        AccessCommand::DenyChannel {
            channel,
            id,
            require_mention,
        } => access_set_channel(config, config_path, *channel, id, false, *require_mention),
        AccessCommand::RemoveChannel { channel, id } => {
            access_remove_channel(config, config_path, *channel, id)
        }
    }
}

fn print_config_context(config_path: &Path, config: &RuntimeConfig) {
    println!("using config: {}", config_path.display());
    for warning in config_path_warnings(config, config_path) {
        println!("warning: {warning}");
    }
    println!("runtime home: {}", config.home_dir().display());
}

fn update_runtime_config<F>(
    config: &RuntimeConfig,
    config_path: &Path,
    mutate: F,
) -> Result<RuntimeConfig>
where
    F: FnOnce(&mut RuntimeConfig) -> Result<()>,
{
    let mut updated = config.clone();
    mutate(&mut updated)?;
    updated.validate()?;
    ensure_runtime_dirs(&updated)?;
    config::write_config(config_path, &updated)?;
    Ok(updated)
}

fn agent_list(config: &RuntimeConfig) -> Result<()> {
    if config.agents.is_empty() {
        println!("no agents configured");
        println!("add one with: clawpod agent add <id>");
        return Ok(());
    }
    let mut entries: Vec<_> = config.agents.iter().collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    println!("agents:");
    for (id, agent) in entries {
        let provider = provider_label(&agent.provider);
        let think = agent
            .think_level
            .map(|t| think_level_label(&t))
            .unwrap_or("-");
        let browser_profile = agent
            .browser
            .as_ref()
            .and_then(|browser| browser.profile.as_deref())
            .or(config.browser.default_profile.as_deref())
            .unwrap_or("-");
        let heartbeat_every = agent
            .heartbeat
            .as_ref()
            .and_then(|heartbeat| heartbeat.every.as_deref())
            .unwrap_or("-");
        println!(
            "  @{id}  {name}  {provider}/{model}  think={think}  browser={browser_profile}  heartbeat={heartbeat_every}",
            name = agent.name,
            model = agent.model,
        );
    }
    Ok(())
}

fn agent_add(config: &RuntimeConfig, config_path: &Path, args: &AgentAddArgs) -> Result<()> {
    let id = normalize_identifier(&args.id, "agent id")?;
    if config.agents.contains_key(&id) {
        bail!("agent already exists: {id}");
    }

    let agent_config = build_agent_config_for_add(args, &id)?;
    let mut assigned_team_id = None;
    let updated = update_runtime_config(config, config_path, |next| {
        next.agents.insert(id.clone(), agent_config.clone());
        assigned_team_id = next.add_agent_to_default_team(&id)?;
        Ok(())
    })?;

    let agent_root = updated.resolve_agent_workdir(&id);
    if let Err(e) = agent::ensure_agent_workspace(
        &id,
        &agent_config,
        &updated.agents,
        &updated.teams,
        &agent_root,
    ) {
        eprintln!("warning: failed to bootstrap workspace: {e:#}");
    }

    if let Some(team_id) = assigned_team_id.as_deref() {
        if let Some(team) = updated.teams.get(team_id) {
            let agent_ids: Vec<&str> = team.agents.iter().map(String::as_str).collect();
            reset_agents(&updated, &agent_ids)?;
        }
    }

    match assigned_team_id {
        Some(team_id) => println!(
            "agent '{id}' created ({}/{}) and added to team '{team_id}'",
            provider_label(&agent_config.provider),
            agent_config.model
        ),
        None => println!(
            "agent '{id}' created ({}/{})",
            provider_label(&agent_config.provider),
            agent_config.model
        ),
    }
    Ok(())
}

fn agent_edit(config: &RuntimeConfig, config_path: &Path, args: &AgentEditArgs) -> Result<()> {
    let id = normalize_identifier(&args.id, "agent id")?;
    let current = config
        .agents
        .get(&id)
        .cloned()
        .ok_or_else(|| anyhow!("agent not found: {id}"))?;
    let mut next_agent = current.clone();

    if let Some(name) = normalize_optional_trimmed(args.name.as_deref()) {
        next_agent.name = name;
    }
    if let Some(provider) = args.provider {
        next_agent.provider = provider.into();
    }
    if let Some(model) = normalize_optional_trimmed(args.model.as_deref()) {
        next_agent.model = model;
    }
    if args.clear_think_level {
        if args.think_level.is_some() {
            bail!("cannot combine --think-level with --clear-think-level");
        }
        next_agent.think_level = None;
    } else if let Some(level) = args.think_level {
        next_agent.think_level = Some(level.into());
    }
    if args.clear_provider_id {
        if args.provider_id.is_some() {
            bail!("cannot combine --provider-id with --clear-provider-id");
        }
        next_agent.provider_id = None;
    } else if let Some(provider_id) = normalize_optional_trimmed(args.provider_id.as_deref()) {
        next_agent.provider_id = Some(provider_id);
    }
    if args.clear_system_prompt {
        if args.system_prompt.is_some() {
            bail!("cannot combine --system-prompt with --clear-system-prompt");
        }
        next_agent.system_prompt = None;
    } else if let Some(system_prompt) = normalize_optional_text(args.system_prompt.as_deref()) {
        next_agent.system_prompt = Some(system_prompt);
    }
    if args.clear_prompt_file {
        if args.prompt_file.is_some() {
            bail!("cannot combine --prompt-file with --clear-prompt-file");
        }
        next_agent.prompt_file = None;
    } else if let Some(prompt_file) = normalize_optional_trimmed(args.prompt_file.as_deref()) {
        next_agent.prompt_file = Some(prompt_file);
    }
    if args.clear_browser_profile {
        if args.browser_profile.is_some() {
            bail!("cannot combine --browser-profile with --clear-browser-profile");
        }
        next_agent.browser = None;
    } else if let Some(browser_profile) =
        normalize_optional_trimmed(args.browser_profile.as_deref())
    {
        next_agent.browser = Some(AgentBrowserConfig {
            profile: Some(browser_profile),
        });
    }

    if args.clear_heartbeat {
        if heartbeat_has_updates(&args.heartbeat, &args.heartbeat_clear) {
            bail!("cannot combine --clear-heartbeat with heartbeat update flags");
        }
        next_agent.heartbeat = None;
    } else {
        apply_heartbeat_updates(
            &mut next_agent.heartbeat,
            &args.heartbeat,
            &args.heartbeat_clear,
        )?;
    }

    let updated = update_runtime_config(config, config_path, |next| {
        let slot = next
            .agents
            .get_mut(&id)
            .ok_or_else(|| anyhow!("agent not found: {id}"))?;
        *slot = next_agent.clone();
        Ok(())
    })?;

    let agent_root = updated.resolve_agent_workdir(&id);
    if let Err(e) = agent::ensure_agent_workspace(
        &id,
        &next_agent,
        &updated.agents,
        &updated.teams,
        &agent_root,
    ) {
        eprintln!("warning: failed to refresh workspace bootstrap: {e:#}");
    }
    reset_agent_runtime_state(&updated, &id)?;

    println!("agent '{id}' updated");
    println!("sessions reset so the new config takes effect on the next run");
    Ok(())
}

fn agent_remove(config: &RuntimeConfig, config_path: &Path, args: &AgentRemoveArgs) -> Result<()> {
    let id = normalize_identifier(&args.id, "agent id")?;
    if !config.agents.contains_key(&id) {
        bail!("agent not found: {id}");
    }
    if config.agents.len() <= 1 {
        bail!("cannot remove the last agent");
    }
    if let Some((team_id, _)) = config
        .teams
        .iter()
        .find(|(_, team)| team.leader_agent == id || team.agents.iter().any(|a| a == &id))
    {
        bail!("agent {id} is still referenced by team {team_id}");
    }

    let updated = update_runtime_config(config, config_path, |next| {
        next.agents.remove(&id);
        Ok(())
    })?;
    reset_agent_runtime_state(&updated, &id)?;

    if args.archive_workspace {
        if let Some(path) = archive_agent_workspace(config, &id)? {
            println!("workspace archived: {}", path.display());
        } else {
            println!("workspace archived: none (directory missing)");
        }
    }

    println!("agent '{id}' removed");
    Ok(())
}

fn agent_show(config: &RuntimeConfig, id: &str) -> Result<()> {
    let agent = config.agents.get(id).ok_or_else(|| {
        let available: Vec<&str> = config.agents.keys().map(|s| s.as_str()).collect();
        anyhow!(
            "agent not found: {id} (available: {})",
            available.join(", ")
        )
    })?;
    println!("{}", serde_json::to_string_pretty(agent)?);
    Ok(())
}

fn team_list(config: &RuntimeConfig) -> Result<()> {
    if config.teams.is_empty() {
        println!("no teams configured");
        return Ok(());
    }
    let mut teams: Vec<_> = config.teams.iter().collect();
    teams.sort_by(|(a, _), (b, _)| a.cmp(b));
    println!("teams:");
    for (id, team) in teams {
        println!(
            "  @{id}  name={}  leader={}  members={}",
            team.name,
            team.leader_agent,
            team.agents.join(","),
        );
    }
    Ok(())
}

fn team_show(config: &RuntimeConfig, id: &str) -> Result<()> {
    let team = config.teams.get(id).ok_or_else(|| {
        let available: Vec<&str> = config.teams.keys().map(|s| s.as_str()).collect();
        anyhow!("team not found: {id} (available: {})", available.join(", "))
    })?;
    println!("{}", serde_json::to_string_pretty(team)?);
    Ok(())
}

fn team_add_agent(
    config: &RuntimeConfig,
    config_path: &Path,
    team: &str,
    agent: &str,
) -> Result<()> {
    let team_id = normalize_identifier(team, "team id")?;
    let agent_id = normalize_identifier(agent, "agent id")?;
    let team_before = config
        .teams
        .get(&team_id)
        .cloned()
        .ok_or_else(|| anyhow!("team not found: {team_id}"))?;
    let updated = update_runtime_config(config, config_path, |next| {
        if !next.agents.contains_key(&agent_id) {
            bail!("agent not found: {agent_id}");
        }
        let team_cfg = next
            .teams
            .get_mut(&team_id)
            .ok_or_else(|| anyhow!("team not found: {team_id}"))?;
        if team_cfg.agents.iter().any(|member| member == &agent_id) {
            bail!("agent {agent_id} is already in team {team_id}");
        }
        team_cfg.agents.push(agent_id.clone());
        Ok(())
    })?;
    let mut affected: HashSet<String> = team_before.agents.into_iter().collect();
    affected.insert(agent_id.clone());
    for affected_agent in affected {
        reset_agent_runtime_state(&updated, &affected_agent)?;
    }
    println!("team '{team_id}' updated: added {agent_id}");
    Ok(())
}

fn team_remove_agent(
    config: &RuntimeConfig,
    config_path: &Path,
    team: &str,
    agent: &str,
) -> Result<()> {
    let team_id = normalize_identifier(team, "team id")?;
    let agent_id = normalize_identifier(agent, "agent id")?;
    let team_before = config
        .teams
        .get(&team_id)
        .cloned()
        .ok_or_else(|| anyhow!("team not found: {team_id}"))?;
    let updated = update_runtime_config(config, config_path, |next| {
        let team_cfg = next
            .teams
            .get_mut(&team_id)
            .ok_or_else(|| anyhow!("team not found: {team_id}"))?;
        if team_cfg.leader_agent == agent_id {
            bail!("cannot remove the team leader; set a new leader first");
        }
        let before = team_cfg.agents.len();
        team_cfg.agents.retain(|member| member != &agent_id);
        if team_cfg.agents.len() == before {
            bail!("agent {agent_id} is not a member of team {team_id}");
        }
        if team_cfg.agents.is_empty() {
            bail!("team {team_id} must include at least one agent");
        }
        Ok(())
    })?;
    let affected: HashSet<String> = team_before.agents.into_iter().collect();
    for affected_agent in affected {
        reset_agent_runtime_state(&updated, &affected_agent)?;
    }
    println!("team '{team_id}' updated: removed {agent_id}");
    Ok(())
}

fn team_set_leader(
    config: &RuntimeConfig,
    config_path: &Path,
    team: &str,
    agent: &str,
) -> Result<()> {
    let team_id = normalize_identifier(team, "team id")?;
    let agent_id = normalize_identifier(agent, "agent id")?;
    let team_before = config
        .teams
        .get(&team_id)
        .cloned()
        .ok_or_else(|| anyhow!("team not found: {team_id}"))?;
    let updated = update_runtime_config(config, config_path, |next| {
        let team_cfg = next
            .teams
            .get_mut(&team_id)
            .ok_or_else(|| anyhow!("team not found: {team_id}"))?;
        if !team_cfg.agents.iter().any(|member| member == &agent_id) {
            bail!("agent {agent_id} is not a member of team {team_id}");
        }
        team_cfg.leader_agent = agent_id.clone();
        Ok(())
    })?;
    for affected_agent in team_before.agents {
        reset_agent_runtime_state(&updated, &affected_agent)?;
    }
    println!("team '{team_id}' leader set to {agent_id}");
    Ok(())
}

fn binding_list(config: &RuntimeConfig) -> Result<()> {
    if config.bindings.is_empty() {
        println!("no bindings configured");
        return Ok(());
    }
    println!("bindings:");
    for (index, binding) in config.bindings.iter().enumerate() {
        println!(
            "  #{index} agent={} channel={} peer={} group={} thread={} account={}",
            binding.agent_id,
            binding.matcher.channel.as_deref().unwrap_or("-"),
            binding.matcher.peer_id.as_deref().unwrap_or("-"),
            binding.matcher.group_id.as_deref().unwrap_or("-"),
            binding.matcher.thread_id.as_deref().unwrap_or("-"),
            binding.matcher.account_id.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

fn binding_show(config: &RuntimeConfig, index: usize) -> Result<()> {
    let binding = config
        .bindings
        .get(index)
        .ok_or_else(|| anyhow!("binding index {index} out of range"))?;
    println!("{}", serde_json::to_string_pretty(binding)?);
    Ok(())
}

fn binding_add(config: &RuntimeConfig, config_path: &Path, args: &BindingAddArgs) -> Result<()> {
    let agent_id = normalize_identifier(&args.agent, "agent id")?;
    let binding = BindingRule {
        agent_id: agent_id.clone(),
        matcher: build_binding_match_from_add(args)?,
    };
    let updated = update_runtime_config(config, config_path, |next| {
        if !next.agents.contains_key(&agent_id) {
            bail!("agent not found: {agent_id}");
        }
        next.bindings.push(binding.clone());
        Ok(())
    })?;
    println!(
        "binding #{} created for {}",
        updated.bindings.len() - 1,
        agent_id
    );
    Ok(())
}

fn binding_update(
    config: &RuntimeConfig,
    config_path: &Path,
    args: &BindingUpdateArgs,
) -> Result<()> {
    let updated = update_runtime_config(config, config_path, |next| {
        if let Some(agent_id) = args.agent.as_deref() {
            let agent_id = normalize_identifier(agent_id, "agent id")?;
            if !next.agents.contains_key(&agent_id) {
                bail!("agent not found: {agent_id}");
            }
        }
        let binding = next
            .bindings
            .get_mut(args.index)
            .ok_or_else(|| anyhow!("binding index {} out of range", args.index))?;
        if let Some(agent_id) = args.agent.as_deref() {
            let agent_id = normalize_identifier(agent_id, "agent id")?;
            binding.agent_id = agent_id;
        }
        apply_binding_update(&mut binding.matcher, args)?;
        if binding_match_is_empty(&binding.matcher) {
            bail!("binding match must include at least one field");
        }
        Ok(())
    })?;
    println!(
        "binding #{} updated for {}",
        args.index, updated.bindings[args.index].agent_id
    );
    Ok(())
}

fn binding_remove(config: &RuntimeConfig, config_path: &Path, index: usize) -> Result<()> {
    let removed = config
        .bindings
        .get(index)
        .cloned()
        .ok_or_else(|| anyhow!("binding index {index} out of range"))?;
    update_runtime_config(config, config_path, |next| {
        next.bindings.remove(index);
        Ok(())
    })?;
    println!("binding #{index} removed (agent={})", removed.agent_id);
    Ok(())
}

fn access_list(config: &RuntimeConfig, channel: Option<ChannelKindArg>) -> Result<()> {
    let channels = channel.map(|value| vec![value]).unwrap_or_else(|| {
        vec![
            ChannelKindArg::Discord,
            ChannelKindArg::Slack,
            ChannelKindArg::Telegram,
        ]
    });
    for channel in channels {
        println!("{}:", channel_label(channel));
        let Some(access) = channel_access_ref(config, channel) else {
            println!("  no explicit access config");
            continue;
        };
        println!(
            "  dm_policy={} group_policy={}",
            direct_message_policy_label(access.dm_policy),
            group_policy_label(access.group_policy)
        );
        if access.channels.is_empty() {
            println!("  overrides: none");
            continue;
        }
        let mut entries: Vec<_> = access.channels.iter().collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (id, rule) in entries {
            println!(
                "  {} allow={} require_mention={}",
                id,
                rule.allow,
                rule.require_mention
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
        }
    }
    Ok(())
}

fn access_set_channel(
    config: &RuntimeConfig,
    config_path: &Path,
    channel: ChannelKindArg,
    id: &str,
    allow: bool,
    require_mention: Option<bool>,
) -> Result<()> {
    let channel_id = normalize_channel_identifier(id)?;
    update_runtime_config(config, config_path, |next| {
        let access = channel_access_mut(next, channel);
        access.channels.insert(
            channel_id.clone(),
            PerChannelAccessConfig {
                allow,
                require_mention,
            },
        );
        Ok(())
    })?;
    println!(
        "{} access updated: {} allow={}{}",
        channel_label(channel),
        channel_id,
        allow,
        require_mention
            .map(|value| format!(" require_mention={value}"))
            .unwrap_or_default()
    );
    Ok(())
}

fn access_remove_channel(
    config: &RuntimeConfig,
    config_path: &Path,
    channel: ChannelKindArg,
    id: &str,
) -> Result<()> {
    let channel_id = normalize_channel_identifier(id)?;
    update_runtime_config(config, config_path, |next| {
        let access = channel_access_mut(next, channel);
        if access.channels.remove(&channel_id).is_none() {
            bail!(
                "{} access override not found: {}",
                channel_label(channel),
                channel_id
            );
        }
        Ok(())
    })?;
    println!(
        "{} access override removed: {}",
        channel_label(channel),
        channel_id
    );
    Ok(())
}

fn normalize_identifier(value: &str, label: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label} must not be empty");
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("{label} may only contain alphanumeric characters, '_', and '-'");
    }
    Ok(trimmed.to_string())
}

fn normalize_optional_trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .filter(|raw| !raw.trim().is_empty())
        .map(str::to_string)
}

fn normalize_channel_identifier(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("channel id must not be empty");
    }
    Ok(trimmed.to_string())
}

fn build_agent_config_for_add(args: &AgentAddArgs, id: &str) -> Result<AgentConfig> {
    let name = normalize_optional_trimmed(args.name.as_deref()).unwrap_or_else(|| id.to_string());
    let model = normalize_optional_trimmed(Some(args.model.as_str()))
        .ok_or_else(|| anyhow!("agent model must not be empty"))?;
    Ok(AgentConfig {
        name,
        provider: args.provider.into(),
        model,
        think_level: args.think_level.map(ThinkLevel::from),
        provider_id: normalize_optional_trimmed(args.provider_id.as_deref()),
        system_prompt: normalize_optional_text(args.system_prompt.as_deref()),
        prompt_file: normalize_optional_trimmed(args.prompt_file.as_deref()),
        heartbeat: heartbeat_from_args(&args.heartbeat)?,
        browser: normalize_optional_trimmed(args.browser_profile.as_deref()).map(|profile| {
            AgentBrowserConfig {
                profile: Some(profile),
            }
        }),
    })
}

fn heartbeat_from_args(args: &HeartbeatArgs) -> Result<Option<AgentHeartbeatConfig>> {
    build_heartbeat_struct(args)
}

fn build_heartbeat_struct(args: &HeartbeatArgs) -> Result<Option<AgentHeartbeatConfig>> {
    if !heartbeat_args_present(args) {
        return Ok(None);
    }
    let active_hours = build_active_hours(
        args.heartbeat_active_start.as_deref(),
        args.heartbeat_active_end.as_deref(),
        args.heartbeat_active_timezone.as_deref(),
        None,
    )?;
    let every = normalize_optional_trimmed(args.heartbeat_every.as_deref());
    if let Some(value) = &every {
        config::parse_duration_str(value)?;
    }
    let heartbeat = AgentHeartbeatConfig {
        every,
        model: normalize_optional_trimmed(args.heartbeat_model.as_deref()),
        prompt: normalize_optional_text(args.heartbeat_prompt.as_deref()),
        target: args.heartbeat_target.map(HeartbeatTarget::from),
        to: normalize_optional_trimmed(args.heartbeat_to.as_deref()),
        account_id: normalize_optional_trimmed(args.heartbeat_account_id.as_deref()),
        ack_max_chars: args.heartbeat_ack_max_chars,
        direct_policy: args
            .heartbeat_direct_policy
            .map(HeartbeatDirectPolicy::from),
        include_reasoning: args.heartbeat_include_reasoning,
        light_context: args.heartbeat_light_context,
        isolated_session: args.heartbeat_isolated_session,
        active_hours,
    };
    if heartbeat_is_empty(&heartbeat) {
        Ok(None)
    } else {
        Ok(Some(heartbeat))
    }
}

fn heartbeat_args_present(args: &HeartbeatArgs) -> bool {
    args.heartbeat_every.is_some()
        || args.heartbeat_model.is_some()
        || args.heartbeat_prompt.is_some()
        || args.heartbeat_target.is_some()
        || args.heartbeat_to.is_some()
        || args.heartbeat_account_id.is_some()
        || args.heartbeat_ack_max_chars.is_some()
        || args.heartbeat_direct_policy.is_some()
        || args.heartbeat_include_reasoning.is_some()
        || args.heartbeat_light_context.is_some()
        || args.heartbeat_isolated_session.is_some()
        || args.heartbeat_active_start.is_some()
        || args.heartbeat_active_end.is_some()
        || args.heartbeat_active_timezone.is_some()
}

fn heartbeat_has_updates(args: &HeartbeatArgs, clears: &HeartbeatClearArgs) -> bool {
    heartbeat_args_present(args)
        || clears.clear_heartbeat_every
        || clears.clear_heartbeat_model
        || clears.clear_heartbeat_prompt
        || clears.clear_heartbeat_target
        || clears.clear_heartbeat_to
        || clears.clear_heartbeat_account_id
        || clears.clear_heartbeat_ack_max_chars
        || clears.clear_heartbeat_direct_policy
        || clears.clear_heartbeat_include_reasoning
        || clears.clear_heartbeat_light_context
        || clears.clear_heartbeat_isolated_session
        || clears.clear_heartbeat_active_hours
}

fn apply_heartbeat_updates(
    heartbeat: &mut Option<AgentHeartbeatConfig>,
    args: &HeartbeatArgs,
    clears: &HeartbeatClearArgs,
) -> Result<()> {
    if !heartbeat_has_updates(args, clears) {
        return Ok(());
    }

    validate_clear_conflict(
        clears.clear_heartbeat_every,
        args.heartbeat_every.is_some(),
        "--heartbeat-every",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_model,
        args.heartbeat_model.is_some(),
        "--heartbeat-model",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_prompt,
        args.heartbeat_prompt.is_some(),
        "--heartbeat-prompt",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_target,
        args.heartbeat_target.is_some(),
        "--heartbeat-target",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_to,
        args.heartbeat_to.is_some(),
        "--heartbeat-to",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_account_id,
        args.heartbeat_account_id.is_some(),
        "--heartbeat-account-id",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_ack_max_chars,
        args.heartbeat_ack_max_chars.is_some(),
        "--heartbeat-ack-max-chars",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_direct_policy,
        args.heartbeat_direct_policy.is_some(),
        "--heartbeat-direct-policy",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_include_reasoning,
        args.heartbeat_include_reasoning.is_some(),
        "--heartbeat-include-reasoning",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_light_context,
        args.heartbeat_light_context.is_some(),
        "--heartbeat-light-context",
    )?;
    validate_clear_conflict(
        clears.clear_heartbeat_isolated_session,
        args.heartbeat_isolated_session.is_some(),
        "--heartbeat-isolated-session",
    )?;

    let mut next = heartbeat.clone().unwrap_or_default();
    if clears.clear_heartbeat_every {
        next.every = None;
    }
    if clears.clear_heartbeat_model {
        next.model = None;
    }
    if clears.clear_heartbeat_prompt {
        next.prompt = None;
    }
    if clears.clear_heartbeat_target {
        next.target = None;
    }
    if clears.clear_heartbeat_to {
        next.to = None;
    }
    if clears.clear_heartbeat_account_id {
        next.account_id = None;
    }
    if clears.clear_heartbeat_ack_max_chars {
        next.ack_max_chars = None;
    }
    if clears.clear_heartbeat_direct_policy {
        next.direct_policy = None;
    }
    if clears.clear_heartbeat_include_reasoning {
        next.include_reasoning = None;
    }
    if clears.clear_heartbeat_light_context {
        next.light_context = None;
    }
    if clears.clear_heartbeat_isolated_session {
        next.isolated_session = None;
    }
    if clears.clear_heartbeat_active_hours {
        next.active_hours = None;
    }

    if let Some(every) = normalize_optional_trimmed(args.heartbeat_every.as_deref()) {
        config::parse_duration_str(&every)?;
        next.every = Some(every);
    }
    if let Some(model) = normalize_optional_trimmed(args.heartbeat_model.as_deref()) {
        next.model = Some(model);
    }
    if let Some(prompt) = normalize_optional_text(args.heartbeat_prompt.as_deref()) {
        next.prompt = Some(prompt);
    }
    if let Some(target) = args.heartbeat_target {
        next.target = Some(target.into());
    }
    if let Some(to) = normalize_optional_trimmed(args.heartbeat_to.as_deref()) {
        next.to = Some(to);
    }
    if let Some(account_id) = normalize_optional_trimmed(args.heartbeat_account_id.as_deref()) {
        next.account_id = Some(account_id);
    }
    if let Some(ack_max_chars) = args.heartbeat_ack_max_chars {
        next.ack_max_chars = Some(ack_max_chars);
    }
    if let Some(direct_policy) = args.heartbeat_direct_policy {
        next.direct_policy = Some(direct_policy.into());
    }
    if let Some(include_reasoning) = args.heartbeat_include_reasoning {
        next.include_reasoning = Some(include_reasoning);
    }
    if let Some(light_context) = args.heartbeat_light_context {
        next.light_context = Some(light_context);
    }
    if let Some(isolated_session) = args.heartbeat_isolated_session {
        next.isolated_session = Some(isolated_session);
    }
    if args.heartbeat_active_start.is_some()
        || args.heartbeat_active_end.is_some()
        || args.heartbeat_active_timezone.is_some()
    {
        next.active_hours = build_active_hours(
            args.heartbeat_active_start.as_deref(),
            args.heartbeat_active_end.as_deref(),
            args.heartbeat_active_timezone.as_deref(),
            next.active_hours.clone(),
        )?;
    }

    if heartbeat_is_empty(&next) {
        *heartbeat = None;
    } else {
        *heartbeat = Some(next);
    }
    Ok(())
}

fn validate_clear_conflict(clear_flag: bool, has_value: bool, option_name: &str) -> Result<()> {
    if clear_flag && has_value {
        bail!("cannot combine {option_name} with its clear flag");
    }
    Ok(())
}

fn build_active_hours(
    start: Option<&str>,
    end: Option<&str>,
    timezone: Option<&str>,
    existing: Option<ActiveHoursConfig>,
) -> Result<Option<ActiveHoursConfig>> {
    let start = normalize_optional_trimmed(start);
    let end = normalize_optional_trimmed(end);
    let timezone = normalize_optional_trimmed(timezone);

    if start.is_none() && end.is_none() && timezone.is_none() {
        return Ok(existing);
    }

    let mut active = existing.unwrap_or_default();
    if let Some(start) = start {
        active.start = start;
    }
    if let Some(end) = end {
        active.end = end;
    }
    if let Some(timezone) = timezone {
        active.timezone = Some(timezone);
    }

    if active.start.trim().is_empty() || active.end.trim().is_empty() {
        bail!("active hours require both start and end, for example 09:00 and 17:00");
    }
    Ok(Some(active))
}

fn heartbeat_is_empty(heartbeat: &AgentHeartbeatConfig) -> bool {
    heartbeat.every.is_none()
        && heartbeat.model.is_none()
        && heartbeat.prompt.is_none()
        && heartbeat.target.is_none()
        && heartbeat.to.is_none()
        && heartbeat.account_id.is_none()
        && heartbeat.ack_max_chars.is_none()
        && heartbeat.direct_policy.is_none()
        && heartbeat.include_reasoning.is_none()
        && heartbeat.light_context.is_none()
        && heartbeat.isolated_session.is_none()
        && heartbeat.active_hours.is_none()
}

fn clear_agent_sessions(config: &RuntimeConfig, agent_id: &str) -> Result<()> {
    let store = StateStore::new(config.state_path())?;
    store.clear_agent_sessions(agent_id)?;
    Ok(())
}

fn reset_agent_runtime_state(config: &RuntimeConfig, agent_id: &str) -> Result<()> {
    let workdir = config.resolve_agent_workdir(agent_id);
    reset_agent_workspace(&workdir)?;
    clear_agent_sessions(config, agent_id)
}

fn archive_agent_workspace(config: &RuntimeConfig, agent_id: &str) -> Result<Option<PathBuf>> {
    let workdir = config.resolve_agent_workdir(agent_id);
    if !workdir.exists() {
        return Ok(None);
    }
    let archive_root = config.home_dir().join("archive").join("workspaces");
    std::fs::create_dir_all(&archive_root)
        .with_context(|| format!("failed to create archive dir: {}", archive_root.display()))?;
    let archived = archive_root.join(format!("{agent_id}-{}", Utc::now().format("%Y%m%d-%H%M%S")));
    std::fs::rename(&workdir, &archived).with_context(|| {
        format!(
            "failed to archive workspace {} -> {}",
            workdir.display(),
            archived.display()
        )
    })?;
    Ok(Some(archived))
}

fn build_binding_match_from_add(args: &BindingAddArgs) -> Result<BindingMatch> {
    let matcher = BindingMatch {
        channel: args.channel.map(channel_config_key),
        account_id: normalize_optional_trimmed(args.account_id.as_deref()),
        peer_id: normalize_optional_trimmed(args.peer_id.as_deref()),
        group_id: normalize_optional_trimmed(args.group_id.as_deref()),
        thread_id: normalize_optional_trimmed(args.thread_id.as_deref()),
    };
    if binding_match_is_empty(&matcher) {
        bail!("binding match must include at least one field");
    }
    Ok(matcher)
}

fn apply_binding_update(matcher: &mut BindingMatch, args: &BindingUpdateArgs) -> Result<()> {
    validate_clear_conflict(args.clear_channel, args.channel.is_some(), "--channel")?;
    validate_clear_conflict(
        args.clear_account_id,
        args.account_id.is_some(),
        "--account-id",
    )?;
    validate_clear_conflict(args.clear_peer_id, args.peer_id.is_some(), "--peer-id")?;
    validate_clear_conflict(args.clear_group_id, args.group_id.is_some(), "--group-id")?;
    validate_clear_conflict(
        args.clear_thread_id,
        args.thread_id.is_some(),
        "--thread-id",
    )?;

    if args.clear_channel {
        matcher.channel = None;
    } else if let Some(channel) = args.channel {
        matcher.channel = Some(channel_config_key(channel));
    }
    if args.clear_account_id {
        matcher.account_id = None;
    } else if let Some(account_id) = normalize_optional_trimmed(args.account_id.as_deref()) {
        matcher.account_id = Some(account_id);
    }
    if args.clear_peer_id {
        matcher.peer_id = None;
    } else if let Some(peer_id) = normalize_optional_trimmed(args.peer_id.as_deref()) {
        matcher.peer_id = Some(peer_id);
    }
    if args.clear_group_id {
        matcher.group_id = None;
    } else if let Some(group_id) = normalize_optional_trimmed(args.group_id.as_deref()) {
        matcher.group_id = Some(group_id);
    }
    if args.clear_thread_id {
        matcher.thread_id = None;
    } else if let Some(thread_id) = normalize_optional_trimmed(args.thread_id.as_deref()) {
        matcher.thread_id = Some(thread_id);
    }
    Ok(())
}

fn channel_access_ref(
    config: &RuntimeConfig,
    channel: ChannelKindArg,
) -> Option<&ChannelAccessConfig> {
    match channel {
        ChannelKindArg::Discord => config
            .channels
            .discord
            .as_ref()
            .and_then(|cfg| cfg.access.as_ref()),
        ChannelKindArg::Slack => config
            .channels
            .slack
            .as_ref()
            .and_then(|cfg| cfg.access.as_ref()),
        ChannelKindArg::Telegram => config
            .channels
            .telegram
            .as_ref()
            .and_then(|cfg| cfg.access.as_ref()),
    }
}

fn channel_access_mut(
    config: &mut RuntimeConfig,
    channel: ChannelKindArg,
) -> &mut ChannelAccessConfig {
    match channel {
        ChannelKindArg::Discord => {
            let channel_cfg = config
                .channels
                .discord
                .get_or_insert_with(DiscordConfig::default);
            channel_cfg
                .access
                .get_or_insert_with(ChannelAccessConfig::default)
        }
        ChannelKindArg::Slack => {
            let channel_cfg = config
                .channels
                .slack
                .get_or_insert_with(SlackConfig::default);
            channel_cfg
                .access
                .get_or_insert_with(ChannelAccessConfig::default)
        }
        ChannelKindArg::Telegram => {
            let channel_cfg = config
                .channels
                .telegram
                .get_or_insert_with(TelegramConfig::default);
            channel_cfg
                .access
                .get_or_insert_with(ChannelAccessConfig::default)
        }
    }
}

fn channel_config_key(channel: ChannelKindArg) -> String {
    match channel {
        ChannelKindArg::Discord => "discord".to_string(),
        ChannelKindArg::Slack => "slack".to_string(),
        ChannelKindArg::Telegram => "telegram".to_string(),
    }
}

fn channel_label(channel: ChannelKindArg) -> &'static str {
    match channel {
        ChannelKindArg::Discord => "discord",
        ChannelKindArg::Slack => "slack",
        ChannelKindArg::Telegram => "telegram",
    }
}

fn binding_match_is_empty(matcher: &BindingMatch) -> bool {
    matcher.channel.is_none()
        && matcher.account_id.is_none()
        && matcher.peer_id.is_none()
        && matcher.group_id.is_none()
        && matcher.thread_id.is_none()
}

fn direct_message_policy_label(policy: DirectMessagePolicy) -> &'static str {
    match policy {
        DirectMessagePolicy::Open => "open",
        DirectMessagePolicy::Allowlist => "allowlist",
        DirectMessagePolicy::Pairing => "pairing",
        DirectMessagePolicy::Disabled => "disabled",
    }
}

fn group_policy_label(policy: GroupPolicy) -> &'static str {
    match policy {
        GroupPolicy::Disabled => "disabled",
        GroupPolicy::MentionOnly => "mention_only",
        GroupPolicy::Allowlist => "allowlist",
        GroupPolicy::Open => "open",
    }
}

fn provider_label(provider: &ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Openai => "openai",
        ProviderKind::Custom => "custom",
        ProviderKind::Mock => "mock",
    }
}

fn think_level_label(level: &ThinkLevel) -> &'static str {
    match level {
        ThinkLevel::Off => "off",
        ThinkLevel::Minimal => "minimal",
        ThinkLevel::Low => "low",
        ThinkLevel::Medium => "medium",
        ThinkLevel::High => "high",
        ThinkLevel::Xhigh => "xhigh",
        ThinkLevel::Adaptive => "adaptive",
    }
}

fn reset(config: &RuntimeConfig, agent_id: &str) -> Result<()> {
    if !config.agents.contains_key(agent_id) {
        bail!("agent not found: {agent_id}");
    }
    reset_agents(config, &[agent_id])?;
    let workdir = config.resolve_agent_workdir(agent_id);
    info!(agent = %agent_id, workdir = %workdir.display(), "agent reset completed");
    Ok(())
}

fn reset_agents(config: &RuntimeConfig, agent_ids: &[&str]) -> Result<()> {
    let store = StateStore::new(config.state_path())?;
    for &agent_id in agent_ids {
        let workdir = config.resolve_agent_workdir(agent_id);
        reset_agent_workspace(&workdir)?;
        store.clear_agent_sessions(agent_id)?;
    }
    Ok(())
}

fn pairing_cmd(config: &RuntimeConfig, command: &PairingCommand) -> Result<()> {
    let store = StateStore::new(config.state_path())?;
    match command {
        PairingCommand::List => {
            let entries = store.list_sender_access(None, Some("pending"))?;
            if entries.is_empty() {
                println!("no pending pairing requests");
                return Ok(());
            }
            for entry in &entries {
                let code_status = if entry.is_locked_out {
                    "locked"
                } else if entry.has_pairing_code {
                    "has_code"
                } else {
                    "no_code"
                };
                let label = entry.sender_label.as_deref().unwrap_or("-");
                let preview = entry
                    .last_message_preview
                    .as_deref()
                    .map(|s| if s.len() > 40 { &s[..40] } else { s })
                    .unwrap_or("-");
                println!(
                    "  {} {} status={} label={} preview={}",
                    entry.channel, entry.sender_id, code_status, label, preview
                );
            }
        }
        PairingCommand::Approve { code } => {
            let entry = store
                .find_pending_by_code(code)?
                .ok_or_else(|| anyhow!("no pending sender found for code: {code}"))?;

            if let Some(expires_str) = &entry.pairing_code_expires_at {
                if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(expires_str) {
                    if pairing::is_code_expired(&expires.with_timezone(&Utc)) {
                        bail!(
                            "pairing code has expired for {}/{}",
                            entry.channel,
                            entry.sender_id
                        );
                    }
                }
            }

            store.approve_sender_access(&entry.channel, &entry.sender_id)?;
            store.clear_pairing_code(&entry.channel, &entry.sender_id)?;
            info!(
                channel = %entry.channel,
                sender_id = %entry.sender_id,
                label = entry.sender_label.as_deref().unwrap_or("-"),
                "sender approved via pairing code"
            );
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct SystemServiceHint {
    unit_path: PathBuf,
    user: Option<String>,
    working_directory: Option<PathBuf>,
    config_path: Option<PathBuf>,
}

fn config_path_warnings(config: &RuntimeConfig, config_path: &Path) -> Vec<String> {
    let mut warnings = vec![];
    let expected = config.home_dir().join("clawpod.toml");
    if config_path != expected {
        warnings.push(format!(
            "config path differs from runtime home default: expected {}",
            expected.display()
        ));
    }

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        let current_home_default = home.join(".clawpod").join("clawpod.toml");
        let current_runtime_home = home.join(".clawpod");
        if current_runtime_home != config.home_dir() {
            warnings.push(format!(
                "current HOME points at {}, but runtime home is {}",
                current_runtime_home.display(),
                config.home_dir().display()
            ));
        }
        if config_path == current_home_default && current_home_default != expected {
            warnings.push(format!(
                "running with the current user's default config path {}; confirm this is intentional",
                current_home_default.display()
            ));
        }
    }

    if let Some(service) = detect_system_service_hint() {
        if let Some(working_directory) = service.working_directory.as_ref() {
            if working_directory != &config.home_dir() {
                warnings.push(format!(
                    "system service {} uses WorkingDirectory={}, but runtime home is {}",
                    service.unit_path.display(),
                    working_directory.display(),
                    config.home_dir().display()
                ));
            }

            if service.config_path.is_none() {
                let inferred = working_directory.join("clawpod.toml");
                if inferred != config_path {
                    warnings.push(format!(
                        "system service {} will infer config {}, but current CLI config is {}",
                        service.unit_path.display(),
                        inferred.display(),
                        config_path.display()
                    ));
                }
            }
        }
        if let Some(service_config_path) = service.config_path.as_ref() {
            if service_config_path != config_path {
                warnings.push(format!(
                    "system service {} uses --config {}, but current CLI config is {}",
                    service.unit_path.display(),
                    service_config_path.display(),
                    config_path.display()
                ));
            }
        }
        if let (Some(service_user), Some(current_user)) =
            (service.user.as_deref(), env::var("USER").ok().as_deref())
        {
            if service_user != current_user {
                warnings.push(format!(
                    "system service runs as user {}, but current shell user is {}",
                    service_user, current_user
                ));
            }
        }
    }

    warnings
}

fn detect_system_service_hint() -> Option<SystemServiceHint> {
    if !cfg!(target_os = "linux") {
        return None;
    }

    let candidates = [
        PathBuf::from("/etc/systemd/system/clawpod.service"),
        PathBuf::from("/lib/systemd/system/clawpod.service"),
    ];
    for unit_path in candidates {
        let Ok(content) = std::fs::read_to_string(&unit_path) else {
            continue;
        };
        let mut hint = SystemServiceHint {
            unit_path: unit_path.clone(),
            ..Default::default()
        };
        for line in content.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("User=") {
                hint.user = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("WorkingDirectory=") {
                hint.working_directory = Some(PathBuf::from(value.trim()));
            } else if let Some(value) = line.strip_prefix("ExecStart=") {
                hint.config_path = parse_execstart_config_path(value);
            }
        }
        return Some(hint);
    }
    None
}

fn parse_execstart_config_path(exec_start: &str) -> Option<PathBuf> {
    let mut parts = exec_start.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "--config" {
            return parts.next().map(PathBuf::from);
        }
        if let Some(value) = part.strip_prefix("--config=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

async fn doctor(config: &RuntimeConfig, config_path: &Path) -> Result<()> {
    info!(home = %config.home_dir().display(), "checking runtime directories");
    info!(config = %config_path.display(), "using config");
    for warning in config_path_warnings(config, config_path) {
        info!("warning: {warning}");
    }

    let checks = [
        ("incoming", config.incoming_dir()),
        ("processing", config.processing_dir()),
        ("outgoing", config.outgoing_dir()),
        ("dead_letter", config.dead_letter_dir()),
        ("workspace", config.workspace_dir()),
    ];

    for (name, path) in checks {
        if path.exists() {
            info!(%name, path = %path.display(), "ok");
        } else {
            info!(%name, path = %path.display(), "missing");
        }
    }

    info!(
        host = %config.server_bind_host(),
        port = config.server.api_port,
        public_bind = config.server.is_public_bind(),
        "server configuration"
    );

    let browser_profiles = config.resolved_browser_profiles()?;
    if browser_profiles.is_empty() {
        info!("browser profiles: none configured");
    } else {
        for profile in browser_profiles {
            let kasm_listening = is_port_listening(profile.kasm_port).await;
            let cdp_listening = is_port_listening(profile.cdp_port).await;
            info!(
                profile = %profile.name,
                display = %profile.display,
                cdp_port = profile.cdp_port,
                cdp_listening,
                kasm_port = profile.kasm_port,
                kasm_listening,
                view_path = %profile.view_path,
                profile_dir = %profile.profile_dir.display(),
                "browser profile"
            );
        }
    }

    let claude = command_version("claude", ["--version"]).await;
    let codex = command_version("codex", ["--version"]).await;

    match claude {
        Ok(v) => info!("claude detected: {v}"),
        Err(err) => info!("claude not available: {err}"),
    }

    match codex {
        Ok(v) => info!("codex detected: {v}"),
        Err(err) => info!("codex not available: {err}"),
    }

    let claude_auth = config::check_claude_auth();
    if claude_auth.is_usable() {
        info!(
            email = claude_auth.email.as_deref().unwrap_or("-"),
            plan = claude_auth.subscription_type.as_deref().unwrap_or("-"),
            "claude auth: valid"
        );
    } else {
        info!("claude auth: not logged in, run 'claude login' if using anthropic provider");
    }

    let codex_auth = config::check_codex_auth();
    if codex_auth.auth_file_exists {
        if codex_auth.is_usable() {
            info!(
                account_id = codex_auth.account_id.as_deref().unwrap_or("-"),
                expires_at = codex_auth.expires_at.as_deref().unwrap_or("-"),
                "codex auth: valid"
            );
        } else if codex_auth.token_expired {
            info!("codex auth: token expired, run 'codex login' to refresh");
        } else {
            info!("codex auth: auth.json found but no valid access token");
        }
    } else {
        info!("codex auth: not logged in, run 'codex login' if using openai provider");
    }

    Ok(())
}

async fn auth_cmd(command: &AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Claude => auth_claude().await,
        AuthCommand::Openai => auth_openai().await,
        AuthCommand::Status => {
            let claude_auth = config::check_claude_auth();
            println!("Anthropic (claude):");
            if claude_auth.is_usable() {
                println!("  status:  authenticated");
                if let Some(email) = &claude_auth.email {
                    println!("  email:   {email}");
                }
                if let Some(org) = &claude_auth.org_name {
                    println!("  org:     {org}");
                }
                if let Some(sub) = &claude_auth.subscription_type {
                    println!("  plan:    {sub}");
                }
            } else {
                println!("  status:  not logged in (run 'clawpod auth claude')");
            }
            println!();

            let codex_auth = config::check_codex_auth();
            println!("OpenAI (codex):");
            if codex_auth.is_usable() {
                println!("  status:  authenticated");
                if let Some(account) = &codex_auth.account_id {
                    println!("  account: {account}");
                }
                if let Some(expires) = &codex_auth.expires_at {
                    println!("  expires: {expires}");
                }
            } else if codex_auth.token_expired {
                println!("  status:  expired (run 'clawpod auth openai' to refresh)");
            } else {
                println!("  status:  not logged in (run 'clawpod auth openai')");
            }
            Ok(())
        }
    }
}

async fn heartbeat_cmd(
    config: &RuntimeConfig,
    config_path: &Path,
    command: &HeartbeatCommand,
) -> Result<()> {
    match command {
        HeartbeatCommand::Last { agent } => {
            let store = StateStore::new(config.state_path())?;
            let runs = store.list_heartbeat_runs(
                if agent.is_some() {
                    1
                } else {
                    config.agents.len()
                },
                agent.as_deref(),
            )?;
            if runs.is_empty() {
                println!("no heartbeat runs recorded");
                return Ok(());
            }
            for run in &runs {
                let indicator = run.indicator_type.as_deref().unwrap_or("-");
                let preview = run.preview.as_deref().unwrap_or("-");
                let skip = run.skip_reason.as_deref().unwrap_or("");
                println!(
                    "  {} agent={} status={} reason={} indicator={} {}{}",
                    run.finished_at,
                    run.agent_id,
                    run.status,
                    run.reason,
                    indicator,
                    if skip.is_empty() {
                        String::new()
                    } else {
                        format!("skip={skip} ")
                    },
                    preview,
                );
            }
        }
        HeartbeatCommand::Run { agent } => {
            if !config.agents.contains_key(agent) {
                bail!("agent not found: {agent}");
            }
            let sink = FileEventSink::new(config.event_log_path())?;
            let store = StateStore::new(config.state_path())?;
            let runner = Arc::new(runner::CliRunner::new(config.runner.timeout_sec));
            let config_arc = Arc::new(config.clone());
            let service = HeartbeatService::new(config_arc, runner, store, sink);
            let view = service
                .run_once(agent, domain::HeartbeatRunReason::Manual)
                .await?;
            println!(
                "agent={} status={} reason={} indicator={} preview={}",
                view.agent_id,
                view.status,
                view.reason,
                view.indicator_type.as_deref().unwrap_or("-"),
                view.preview.as_deref().unwrap_or("-"),
            );
        }
        HeartbeatCommand::Enable => {
            let mut updated = config.clone();
            updated.heartbeat.enabled = true;
            config::write_config(config_path, &updated)?;
            println!("heartbeat enabled");
        }
        HeartbeatCommand::Disable => {
            let mut updated = config.clone();
            updated.heartbeat.enabled = false;
            config::write_config(config_path, &updated)?;
            println!("heartbeat disabled");
        }
    }
    Ok(())
}

async fn auth_claude() -> Result<()> {
    let claude_check = Command::new("claude").arg("--version").output().await;
    if claude_check.is_err() || !claude_check.unwrap().status.success() {
        bail!("claude CLI not found. Install it first: https://docs.anthropic.com/en/docs/claude-code");
    }

    println!("Starting Anthropic authentication via claude CLI...");
    println!();

    let status = Command::new("claude")
        .arg("login")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await
        .context("failed to run 'claude login'")?;

    if !status.success() {
        bail!(
            "claude login failed with exit code: {}",
            status.code().unwrap_or(-1)
        );
    }

    println!();

    let auth = config::check_claude_auth();
    if auth.is_usable() {
        println!("Authentication successful.");
        if let Some(email) = &auth.email {
            println!("  email: {email}");
        }
        if let Some(org) = &auth.org_name {
            println!("  org:   {org}");
        }
        if let Some(sub) = &auth.subscription_type {
            println!("  plan:  {sub}");
        }
    } else {
        println!("Warning: claude login completed but auth status could not be verified.");
    }

    Ok(())
}

async fn auth_openai() -> Result<()> {
    let codex_check = Command::new("codex").arg("--version").output().await;
    if codex_check.is_err() || !codex_check.unwrap().status.success() {
        bail!("codex CLI not found. Install it first: https://github.com/openai/codex");
    }

    println!("Starting OpenAI authentication via codex CLI...");
    println!();

    let status = Command::new("codex")
        .arg("login")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await
        .context("failed to run 'codex login'")?;

    if !status.success() {
        bail!(
            "codex login failed with exit code: {}",
            status.code().unwrap_or(-1)
        );
    }

    println!();

    let auth = config::check_codex_auth();
    if auth.is_usable() {
        println!("Authentication successful.");
        if let Some(account) = &auth.account_id {
            println!("  account: {account}");
        }
        if let Some(expires) = &auth.expires_at {
            println!("  expires: {expires}");
        }
    } else {
        println!("Warning: codex login completed but token could not be verified.");
    }

    Ok(())
}

async fn status(config: &RuntimeConfig, config_path: &Path) -> Result<()> {
    let queue_counts = queue_counts(config)?;
    let store = StateStore::new(config.state_path())?;
    let runs = store.list_recent_runs(5)?;
    let health = if config.server.enabled {
        Some(fetch_health(config).await)
    } else {
        None
    };

    println!("config: {}", config_path.display());
    println!("home: {}", config.home_dir().display());
    println!("workspace: {}", config.workspace_dir().display());
    let warnings = config_path_warnings(config, config_path);
    if warnings.is_empty() {
        println!("config_warnings: none");
    } else {
        println!("config_warnings:");
        for warning in warnings {
            println!("  - {warning}");
        }
    }
    println!(
        "server: {} ({})",
        if config.server.enabled {
            "enabled"
        } else {
            "disabled"
        },
        config.server_listen_addr()
    );
    println!("public_bind: {}", config.server.is_public_bind());
    println!(
        "channels: slack={} discord={} telegram={}",
        config.channels.slack.is_some(),
        config.channels.discord.is_some(),
        config.channels.telegram.is_some(),
    );
    println!(
        "queue: incoming={} processing={} outgoing={} dead_letter={}",
        queue_counts.0, queue_counts.1, queue_counts.2, queue_counts.3
    );
    println!("stdout_log: {}", config.daemon_log_path().display());
    println!("stderr_log: {}", config.daemon_stderr_path().display());
    println!("event_log: {}", config.event_log_path().display());
    let browser_profiles = config.resolved_browser_profiles()?;
    if browser_profiles.is_empty() {
        println!("browser_profiles: none");
    } else {
        println!("browser_profiles:");
        for profile in browser_profiles {
            let kasm_listening = is_port_listening(profile.kasm_port).await;
            let cdp_listening = is_port_listening(profile.cdp_port).await;
            println!(
                "  - {name} display={display} cdp={cdp_port}({cdp_status}) kasm={kasm_port}({kasm_status}) view={view}",
                name = profile.name,
                display = profile.display,
                cdp_port = profile.cdp_port,
                cdp_status = if cdp_listening { "listening" } else { "down" },
                kasm_port = profile.kasm_port,
                kasm_status = if kasm_listening { "listening" } else { "down" },
                view = profile.view_path,
            );
        }
    }

    match health {
        Some(Ok(snapshot)) => {
            println!("health: reachable");
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        Some(Err(err)) => println!("health: unreachable ({err})"),
        None => println!("health: server disabled"),
    }

    if runs.is_empty() {
        println!("recent_runs: none");
    } else {
        println!("recent_runs:");
        for run in runs {
            println!(
                "  - {} {} {} {}",
                run.get("updated_at").and_then(Value::as_str).unwrap_or("-"),
                run.get("agent_id").and_then(Value::as_str).unwrap_or("-"),
                run.get("status").and_then(Value::as_str).unwrap_or("-"),
                run.get("message_id").and_then(Value::as_str).unwrap_or("-"),
            );
        }
    }

    Ok(())
}

async fn health(config: &RuntimeConfig) -> Result<()> {
    let snapshot = fetch_health(config).await?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}

async fn is_port_listening(port: u16) -> bool {
    timeout(
        Duration::from_millis(300),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

async fn fetch_health(config: &RuntimeConfig) -> Result<Value> {
    if !config.server.enabled {
        bail!("server is disabled");
    }

    let client = reqwest::Client::builder()
        .build()
        .context("failed to build http client")?;
    let url = format!(
        "http://{}:{}/health",
        local_server_host(config),
        config.server.api_port
    );
    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to query {url}"))?;
    if !response.status().is_success() {
        bail!("health endpoint returned {}", response.status());
    }
    response
        .json::<Value>()
        .await
        .context("failed to parse health response")
}

async fn logs(
    config: &RuntimeConfig,
    source: LogSourceArg,
    lines: usize,
    follow: bool,
) -> Result<()> {
    let path = match source {
        LogSourceArg::Daemon => config.daemon_log_path(),
        LogSourceArg::Stderr => config.daemon_stderr_path(),
        LogSourceArg::Events => config.event_log_path(),
    };

    let mut offset = print_tail(&path, lines)?;
    if follow {
        loop {
            sleep(Duration::from_millis(1000)).await;
            offset = print_new_bytes(&path, offset)?;
        }
    }

    Ok(())
}

fn print_tail(path: &Path, lines: usize) -> Result<u64> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read log file: {}", path.display()))?;
    let total_len = content.len() as u64;
    let mut tail = content.lines().rev().take(lines).collect::<Vec<_>>();
    tail.reverse();
    for line in tail {
        println!("{line}");
    }
    Ok(total_len)
}

fn print_new_bytes(path: &Path, offset: u64) -> Result<u64> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("failed to read log file: {}", path.display()))?;
    let len = file.metadata()?.len();
    let start = if len < offset { 0 } else { offset };
    file.seek(SeekFrom::Start(start))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    if !buf.is_empty() {
        print!("{buf}");
    }
    Ok(len)
}

fn queue_counts(config: &RuntimeConfig) -> Result<(usize, usize, usize, usize)> {
    Ok((
        count_json_files(&config.incoming_dir())?,
        count_json_files(&config.processing_dir())?,
        count_json_files(&config.outgoing_dir())?,
        count_json_files(&config.dead_letter_dir())?,
    ))
}

fn count_json_files(path: &Path) -> Result<usize> {
    let mut count = 0usize;
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("failed to read dir: {}", path.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
        {
            count += 1;
        }
    }
    Ok(count)
}

fn local_server_host(config: &RuntimeConfig) -> String {
    match config.server_bind_host() {
        "0.0.0.0" => "127.0.0.1".to_string(),
        "::" | "[::]" => "::1".to_string(),
        host => host.to_string(),
    }
}

async fn command_version<const N: usize>(program: &str, args: [&str; N]) -> Result<String> {
    let output = Command::new(program).args(args).output().await?;
    if !output.status.success() {
        return Err(anyhow!(
            "{} exited with status {:?}",
            program,
            output.status.code()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stdout.is_empty() {
        Ok(stdout)
    } else {
        Ok(stderr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_execstart_config_path_supports_split_and_inline_flags() {
        assert_eq!(
            parse_execstart_config_path(
                "/usr/local/bin/clawpod daemon --config /srv/clawpod/clawpod.toml"
            ),
            Some(PathBuf::from("/srv/clawpod/clawpod.toml"))
        );
        assert_eq!(
            parse_execstart_config_path(
                "/usr/local/bin/clawpod daemon --config=/srv/clawpod/clawpod.toml"
            ),
            Some(PathBuf::from("/srv/clawpod/clawpod.toml"))
        );
        assert_eq!(
            parse_execstart_config_path("/usr/local/bin/clawpod daemon"),
            None
        );
    }

    #[test]
    fn build_active_hours_requires_both_start_and_end() {
        let err = build_active_hours(Some("09:00"), None, Some("UTC"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("active hours require both start and end"));
    }

    #[test]
    fn apply_heartbeat_updates_removes_empty_override() {
        let mut heartbeat = Some(AgentHeartbeatConfig {
            every: Some("30m".to_string()),
            ..AgentHeartbeatConfig::default()
        });
        let args = HeartbeatArgs::default();
        let clears = HeartbeatClearArgs {
            clear_heartbeat_every: true,
            ..HeartbeatClearArgs::default()
        };

        apply_heartbeat_updates(&mut heartbeat, &args, &clears).unwrap();

        assert!(heartbeat.is_none());
    }
}
