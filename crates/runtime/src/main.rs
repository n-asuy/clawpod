mod heartbeat;
mod service;

use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ::heartbeat::HeartbeatService;
use agent::reset_agent_workspace;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use config::{default_config_path, ensure_runtime_dirs, load_config, RuntimeConfig};
use domain::{AgentConfig, ChatType, ProviderKind, ThinkLevel};
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
    Add {
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
        /// System prompt
        #[arg(long)]
        system_prompt: Option<String>,
    },
    /// Remove an agent
    #[command(alias = "rm")]
    Remove {
        /// Agent ID to remove
        id: String,
    },
    /// Show agent configuration
    Show {
        /// Agent ID
        id: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProviderKindArg {
    Anthropic,
    Openai,
    Custom,
    Mock,
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
        Commands::Doctor => doctor(&config).await?,
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
    match command {
        AgentCommand::List => agent_list(config),
        AgentCommand::Add {
            id,
            name,
            provider,
            model,
            think_level,
            provider_id,
            system_prompt,
        } => agent_add(
            config,
            config_path,
            id,
            name.as_deref(),
            *provider,
            model,
            *think_level,
            provider_id.as_deref(),
            system_prompt.as_deref(),
        ),
        AgentCommand::Remove { id } => agent_remove(config, config_path, id),
        AgentCommand::Show { id } => agent_show(config, id),
    }
}

fn agent_list(config: &RuntimeConfig) -> Result<()> {
    if config.agents.is_empty() {
        println!("no agents configured");
        println!("add one with: clawpod agent add <id>");
        return Ok(());
    }
    println!("agents:");
    for (id, agent) in &config.agents {
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
        println!(
            "  @{id}  {name}  {provider}/{model}  think={think}  browser={browser_profile}",
            name = agent.name,
            model = agent.model,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn agent_add(
    config: &RuntimeConfig,
    config_path: &Path,
    id: &str,
    name: Option<&str>,
    provider: ProviderKindArg,
    model: &str,
    think_level: Option<ThinkLevelArg>,
    provider_id: Option<&str>,
    system_prompt: Option<&str>,
) -> Result<()> {
    let id = normalize_agent_id(id)?;
    if config.agents.contains_key(&id) {
        bail!("agent already exists: {id}");
    }

    let agent_config = AgentConfig {
        name: name.unwrap_or(&id).to_string(),
        provider: provider.into(),
        model: model.to_string(),
        think_level: think_level.map(ThinkLevel::from),
        provider_id: provider_id.map(String::from),
        system_prompt: system_prompt.map(String::from),
        prompt_file: None,
        heartbeat: None,
        browser: None,
    };

    if agent_config.name.trim().is_empty() {
        bail!("agent name must not be empty");
    }
    if agent_config.model.trim().is_empty() {
        bail!("agent model must not be empty");
    }

    let mut updated = config.clone();
    updated.agents.insert(id.clone(), agent_config.clone());
    let assigned_team_id = updated.add_agent_to_default_team(&id)?;
    config::write_config(config_path, &updated)?;

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

fn agent_remove(config: &RuntimeConfig, config_path: &Path, id: &str) -> Result<()> {
    let id = normalize_agent_id(id)?;
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

    let mut updated = config.clone();
    updated.agents.remove(&id);
    config::write_config(config_path, &updated)?;

    let store = StateStore::new(config.state_path())?;
    store.clear_agent_sessions(&id)?;

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

fn normalize_agent_id(id: &str) -> Result<String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        bail!("agent id must not be empty");
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("agent id may only contain alphanumeric characters, '_', and '-'");
    }
    Ok(trimmed.to_string())
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

async fn doctor(config: &RuntimeConfig) -> Result<()> {
    info!(home = %config.home_dir().display(), "checking runtime directories");

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
