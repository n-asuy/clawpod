mod heartbeat;
mod service;

use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent::reset_agent_workspace;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use config::{default_config_path, ensure_runtime_dirs, load_config, RuntimeConfig};
use domain::{ChatType, RunKind};
use observer::{
    bump_component_restart, log_startup_banner, mark_component_disabled, mark_component_error,
    mark_component_ok, FileEventSink,
};
use queue::{enqueue_message, EnqueueMessage, QueueProcessor};
use runner::CliRunner;
use serde_json::Value;
use store::StateStore;
use tokio::process::Command;
use tokio::task::JoinSet;
use tokio::time::{sleep, Duration};
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
    /// Login to OpenAI via codex CLI
    Openai,
    /// Show authentication status for all providers
    Status,
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
                run_kind: RunKind::Message,
            };
            let path = enqueue_message(&config, msg).await?;
            info!(path = %path.display(), "queued message");
        }
        Commands::Doctor => doctor(&config).await?,
        Commands::Office => {
            log_startup_banner(&config.home_dir());
            let sink = FileEventSink::new(config.event_log_path())?;
            let store = StateStore::new(config.state_path())?;
            server::run(config, config_path, store, sink).await?;
        }
        Commands::Reset { agent } => reset(&config, &agent)?,
        Commands::Pairing { command } => pairing_cmd(&config, &command)?,
        Commands::Auth { command } => auth_cmd(&command).await?,
    }

    Ok(())
}

async fn run_daemon(config: RuntimeConfig, config_path: PathBuf) -> Result<()> {
    log_startup_banner(&config.home_dir());
    mark_component_ok("daemon");

    let sink = FileEventSink::new(config.event_log_path())?;
    let store = StateStore::new(config.state_path())?;
    let runner = Arc::new(CliRunner::new(config.runner.timeout_sec));
    let processor = QueueProcessor::new(config.clone(), runner, store.clone(), sink.clone());
    let mut tasks = JoinSet::new();

    spawn_component(
        &mut tasks,
        "queue",
        async move { processor.run_forever().await },
    );

    if config.heartbeat.enabled {
        let heartbeat_config = config.clone();
        let heartbeat_store = store.clone();
        let heartbeat_sink = sink.clone();
        spawn_component(&mut tasks, "heartbeat", async move {
            heartbeat::run_loop(heartbeat_config, heartbeat_store, heartbeat_sink).await
        });
    } else {
        mark_component_disabled("heartbeat", "heartbeat disabled");
    }

    if config.server.enabled {
        let server_config = config.clone();
        let server_path = config_path.clone();
        let server_store = StateStore::new(server_config.state_path())?;
        let server_sink = FileEventSink::new(server_config.event_log_path())?;
        spawn_component(&mut tasks, "office", async move {
            server::run(server_config, server_path, server_store, server_sink).await
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

fn reset(config: &RuntimeConfig, agent_id: &str) -> Result<()> {
    if !config.agents.contains_key(agent_id) {
        bail!("agent not found: {agent_id}");
    }
    let workdir = config.resolve_agent_workdir(agent_id);
    reset_agent_workspace(&workdir)?;
    let store = StateStore::new(config.state_path())?;
    store.clear_agent_sessions(agent_id)?;
    info!(agent = %agent_id, workdir = %workdir.display(), "agent reset completed");
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
        AuthCommand::Openai => auth_openai().await,
        AuthCommand::Status => {
            let codex_auth = config::check_codex_auth();
            println!("OpenAI (codex):");
            if codex_auth.is_usable() {
                println!("  status:  authenticated",);
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
