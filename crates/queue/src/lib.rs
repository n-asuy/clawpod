use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use agent::{
    clear_reset_flag, ensure_agent_workspace, ensure_lightweight_session_workspace,
    ensure_session_workspace, PromptContext, SystemPromptBuilder,
};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use config::{
    CustomProviderConfig, ResolvedHeartbeatConfig, ResolvedHeartbeatVisibility, RuntimeConfig,
};
use domain::{
    AgentConfig, ChatType, ChatroomPost, HeartbeatDirectPolicy, InboundEvent, OutboundEvent,
    ProviderHarness, ProviderKind, RunKind, RunRequest, RunStatus, Runner,
};
use observer::FileEventSink;
use plugins::{dispatch_event, transform_incoming, transform_outgoing, HookContext};
use regex::Regex;
use routing::{extract_chatroom_posts, find_team_for_agent, parse_agent_routing, resolve_binding};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use session::{build_agent_main_session_key, build_session_key};
use store::StateStore;
use tokio::fs;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};
use uuid::Uuid;

const LONG_RESPONSE_THRESHOLD: usize = 4000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnqueueMessage {
    pub channel: String,
    pub sender: String,
    pub sender_id: String,
    pub message: String,
    pub message_id: String,
    pub timestamp_ms: i64,
    pub chat_type: ChatType,
    pub peer_id: String,
    pub account_id: Option<String>,
    pub pre_routed_agent: Option<String>,
    pub from_agent: Option<String>,
    pub files: Vec<String>,
    #[serde(default)]
    pub chain_depth: u32,
    #[serde(default)]
    pub run_kind: RunKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IncomingQueueEnvelope {
    channel: String,
    sender: String,
    #[serde(default)]
    sender_id: Option<String>,
    message: String,
    timestamp: i64,
    message_id: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    from_agent: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    chat_type: Option<String>,
    #[serde(default)]
    peer_id: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    retry_count: u32,
    #[serde(default)]
    available_at_ms: Option<i64>,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    chain_depth: u32,
    #[serde(default)]
    run_kind: RunKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutgoingQueueEnvelope {
    channel: String,
    sender: String,
    message: String,
    original_message: String,
    timestamp: i64,
    message_id: String,
    agent: String,
    recipient_id: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct QueuedOutgoingMessage {
    pub path: PathBuf,
    pub channel: String,
    pub recipient_id: String,
    pub message: String,
    pub message_id: String,
    pub original_message: String,
    pub agent_id: String,
    pub files: Vec<String>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct PreparedOutbound {
    message: String,
    files: Vec<String>,
    metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct HeartbeatDeliveryTarget {
    channel: Option<String>,
    recipient_id: Option<String>,
    account_id: Option<String>,
}

#[derive(Debug, Clone)]
struct HeartbeatExecution {
    config: ResolvedHeartbeatConfig,
    base_session_key: String,
    run_session_key: String,
    previous_updated_at: Option<String>,
    delivery: HeartbeatDeliveryTarget,
    visibility: ResolvedHeartbeatVisibility,
}

#[derive(Debug, Clone)]
struct HeartbeatTextResolution {
    should_skip: bool,
    text: String,
}

#[derive(Clone)]
pub struct QueueProcessor {
    config: Arc<RuntimeConfig>,
    runner: Arc<dyn Runner>,
    store: StateStore,
    sink: FileEventSink,
    global_limit: Arc<Semaphore>,
    session_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl QueueProcessor {
    pub fn new(
        config: RuntimeConfig,
        runner: Arc<dyn Runner>,
        store: StateStore,
        sink: FileEventSink,
    ) -> Self {
        Self {
            global_limit: Arc::new(Semaphore::new(config.daemon.max_concurrent_runs.max(1))),
            config: Arc::new(config),
            runner,
            store,
            sink,
            session_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run_forever(self) -> Result<()> {
        info!("queue processor started");

        self.emit_event("processor_start", json!({ "status": "started" }))
            .await?;
        self.recover_processing_files().await?;

        let mut joinset = JoinSet::new();

        loop {
            let files = self.scan_incoming_files().await?;
            for file in files {
                let this = self.clone();
                joinset.spawn(async move {
                    if let Err(err) = this.handle_file(file).await {
                        error!("handle_file error: {err:#}");
                    }
                });
            }

            while joinset.len()
                > self
                    .config
                    .daemon
                    .max_concurrent_runs
                    .saturating_mul(4)
                    .max(8)
            {
                let _ = joinset.join_next().await;
            }

            sleep(Duration::from_millis(
                self.config.daemon.poll_interval_ms.max(100),
            ))
            .await;
        }
    }

    async fn recover_processing_files(&self) -> Result<()> {
        let mut entries = fs::read_dir(self.config.processing_dir())
            .await
            .with_context(|| {
                format!(
                    "failed to read processing dir: {}",
                    self.config.processing_dir().display()
                )
            })?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("invalid processing filename"))?;
            let incoming = self.config.incoming_dir().join(file_name);
            fs::rename(&path, &incoming).await.with_context(|| {
                format!(
                    "failed to move stale processing file back to incoming: {}",
                    path.display()
                )
            })?;
            warn!(
                "recovered stale processing file: {} -> {}",
                path.display(),
                incoming.display()
            );
        }

        Ok(())
    }

    async fn scan_incoming_files(&self) -> Result<Vec<PathBuf>> {
        let mut entries = fs::read_dir(self.config.incoming_dir())
            .await
            .with_context(|| {
                format!(
                    "failed to read incoming dir: {}",
                    self.config.incoming_dir().display()
                )
            })?;

        let mut files = vec![];
        let now = Utc::now().timestamp_millis();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Some(available_at) = parse_due_from_filename(&path) {
                if available_at > now {
                    continue;
                }
            }
            files.push(path);
        }

        files.sort_by(|a, b| {
            parse_due_from_filename(a)
                .unwrap_or_default()
                .cmp(&parse_due_from_filename(b).unwrap_or_default())
                .then_with(|| a.cmp(b))
        });
        Ok(files)
    }

    async fn handle_file(&self, incoming_path: PathBuf) -> Result<()> {
        let file_name = incoming_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("invalid incoming filename"))?
            .to_string();

        let processing_path = self.config.processing_dir().join(&file_name);

        if fs::rename(&incoming_path, &processing_path).await.is_err() {
            return Ok(());
        }

        let raw = fs::read_to_string(&processing_path)
            .await
            .with_context(|| {
                format!(
                    "failed to read processing file: {}",
                    processing_path.display()
                )
            })?;
        let envelope: IncomingQueueEnvelope = serde_json::from_str(&raw)
            .with_context(|| format!("invalid queue payload: {}", processing_path.display()))?;

        let result = self.process_claimed_file(&processing_path, &envelope).await;
        match result {
            Ok(()) => {
                let _ = fs::remove_file(&processing_path).await;
                Ok(())
            }
            Err(err) => {
                error!("processing failed for {}: {err:#}", file_name);
                self.requeue_or_dead_letter(&processing_path, envelope, &err.to_string())
                    .await?;
                Err(err)
            }
        }
    }

    async fn process_claimed_file(
        &self,
        _processing_path: &Path,
        envelope: &IncomingQueueEnvelope,
    ) -> Result<()> {
        let event = self.envelope_to_event(envelope)?;
        let incoming_context = HookContext {
            channel: event.channel.clone(),
            sender: event.sender.clone(),
            sender_id: Some(event.sender_id.clone()),
            message_id: event.message_id.clone(),
            original_message: event.text.clone(),
            agent_id: None,
        };
        let incoming_hook =
            transform_incoming(&self.config, &event.text, &incoming_context).await?;
        let default_agent = self.default_agent_id()?;

        let mut route =
            parse_agent_routing(&incoming_hook.text, &self.config.agents, &self.config.teams);
        if route.agent_id == "default" {
            route.agent_id = resolve_binding(&event, &self.config.bindings, &default_agent);
            route.is_team_routed = false;
            route.team_id = None;
        }

        if let Some(forced) = &event.pre_routed_agent {
            if self.config.agents.contains_key(forced) {
                route.agent_id = forced.clone();
                route.is_team_routed = false;
                route.team_id = None;
            }
        }

        let agent_id = if self.config.agents.contains_key(&route.agent_id) {
            route.agent_id.clone()
        } else {
            warn!(
                "route target '{}' not found; fallback to default",
                route.agent_id
            );
            default_agent.clone()
        };

        let team_id = if event.channel == "chatroom" {
            Some(event.peer_id.clone())
        } else if route.is_team_routed {
            route.team_id.clone()
        } else {
            find_team_for_agent(&agent_id, &self.config.teams)
        };

        let agent = self.agent_or_err(&agent_id)?;
        let agent_root = self.config.resolve_agent_workdir(&agent_id);
        ensure_agent_workspace(
            &agent_id,
            agent,
            &self.config.agents,
            &self.config.teams,
            &agent_root,
        )?;
        if clear_reset_flag(&agent_root)? {
            self.store.clear_agent_sessions(&agent_id)?;
        }

        let heartbeat_execution = if event.run_kind == RunKind::Heartbeat {
            let heartbeat = self
                .config
                .resolve_heartbeat_config(&agent_id)
                .ok_or_else(|| anyhow!("heartbeat config not found for agent: {agent_id}"))?;
            let base_session_key = resolve_heartbeat_session_key(
                &agent_id,
                heartbeat.session.as_deref(),
                &self.config.session.main_key,
            );
            let session_entry = self.store.get_session(&base_session_key)?;
            let delivery = resolve_heartbeat_delivery_target(session_entry.as_ref(), &heartbeat);
            let visibility = delivery
                .channel
                .as_deref()
                .map(|channel| {
                    self.config
                        .resolve_heartbeat_visibility(channel, delivery.account_id.as_deref())
                })
                .unwrap_or_default();
            let previous_updated_at = session_entry
                .as_ref()
                .map(|session| session.updated_at.clone());
            let run_session_key = if heartbeat.isolated_session {
                format!("{base_session_key}:heartbeat:{}", Uuid::new_v4().simple())
            } else {
                base_session_key.clone()
            };
            Some(HeartbeatExecution {
                config: heartbeat,
                base_session_key,
                run_session_key,
                previous_updated_at,
                delivery,
                visibility,
            })
        } else {
            None
        };

        let session_key = heartbeat_execution
            .as_ref()
            .map(|heartbeat| heartbeat.run_session_key.clone())
            .unwrap_or_else(|| {
                build_session_key(
                    &agent_id,
                    &event,
                    self.config.session.dm_scope,
                    &self.config.session.main_key,
                )
            });
        let session_dir = if let Some(heartbeat) = &heartbeat_execution {
            if heartbeat.config.light_context {
                ensure_lightweight_session_workspace(&agent_root, &session_key)?
            } else {
                ensure_session_workspace(&agent_root, &session_key)?
            }
        } else {
            ensure_session_workspace(&agent_root, &session_key)?
        };

        let session_lock = self
            .session_lock(
                heartbeat_execution
                    .as_ref()
                    .map(|heartbeat| heartbeat.base_session_key.clone())
                    .unwrap_or_else(|| session_key.clone()),
            )
            .await;
        let _session_guard = session_lock.lock().await;
        let _global_guard = self.global_limit.acquire().await?;

        let continue_session = self.store.session_exists(&session_key)?;
        self.store.touch_session(&session_key, &agent_id)?;
        if event.run_kind != RunKind::Heartbeat && event.channel != "chatroom" {
            self.store.update_session_route(
                &session_key,
                &agent_id,
                &event.channel,
                &event.peer_id,
                event.account_id.as_deref(),
                chat_type_str(event.chat_type),
            )?;
        }

        let task_id = Uuid::new_v4();

        self.emit_event(
            "run_started",
            json!({
                "task_id": task_id,
                "session_key": session_key,
                "agent_id": agent_id,
                "team_id": team_id,
                "message_id": event.message_id,
            }),
        )
        .await?;

        let prompt = augment_prompt_with_files(route.message, &event.files);
        let heartbeat_started_at = Utc::now();
        let heartbeat_started_mono = Instant::now();

        let mut metadata = self.build_run_metadata_for_agent(
            &agent_id,
            Some(&incoming_hook.metadata),
            &session_dir,
            event.run_kind,
            heartbeat_execution
                .as_ref()
                .map(|heartbeat| heartbeat.config.light_context)
                .unwrap_or(false),
        )?;
        if let Some(heartbeat) = &heartbeat_execution {
            apply_model_override(&mut metadata, heartbeat.config.model.as_deref());
        }
        let outcome = self
            .run_single_task(
                task_id,
                &event.message_id,
                &session_key,
                &session_dir,
                &agent_id,
                prompt,
                continue_session,
                metadata,
                event.run_kind,
            )
            .await;

        match outcome {
            Ok(text) => {
                if event.run_kind == RunKind::Heartbeat {
                    self.record_heartbeat_run(
                        &agent_id,
                        &event.text,
                        Some(&text),
                        "ok",
                        heartbeat_started_at,
                        heartbeat_started_mono.elapsed().as_millis() as i64,
                    )
                    .await?;
                }

                let chatroom_posts = extract_chatroom_posts(&text, &agent_id, &self.config.teams);
                if !chatroom_posts.is_empty() {
                    self.post_chatroom_messages(&agent_id, &chatroom_posts)
                        .await?;
                }

                // Extract teammate mentions and enqueue as separate messages (flat handoff)
                if event.run_kind != RunKind::Heartbeat {
                    if let Some(ref tid) = team_id {
                        let next_depth = event.chain_depth + 1;
                        let max_depth = self.config.chain.max_chain_steps as u32;

                        let handoffs = routing::extract_teammate_mentions(
                            &text,
                            &agent_id,
                            event.from_agent.as_deref(),
                            tid,
                            &self.config.teams,
                            &self.config.agents,
                        );

                        if !handoffs.is_empty() && next_depth > max_depth {
                            warn!(
                                depth = next_depth,
                                max = max_depth,
                                agent = %agent_id,
                                "handoff chain depth exceeded — dropping further handoffs"
                            );
                        }

                        for mention in &handoffs {
                            if next_depth > max_depth {
                                break;
                            }

                            info!(
                                from = %agent_id,
                                to = %mention.teammate_id,
                                depth = next_depth,
                                "handoff enqueued"
                            );
                            self.emit_event(
                                "chain_handoff",
                                json!({
                                    "team_id": tid,
                                    "from_agent": agent_id,
                                    "to_agent": mention.teammate_id,
                                    "chain_depth": next_depth,
                                }),
                            )
                            .await?;

                            let internal_msg = format!(
                                "[Message from teammate @{}]:\n{}",
                                agent_id, mention.message
                            );
                            enqueue_message(
                                &self.config,
                                EnqueueMessage {
                                    channel: event.channel.clone(),
                                    sender: event.sender.clone(),
                                    sender_id: event.sender_id.clone(),
                                    message: internal_msg,
                                    message_id: format!(
                                        "internal-{}-{}",
                                        Uuid::new_v4().simple(),
                                        mention.teammate_id
                                    ),
                                    timestamp_ms: Utc::now().timestamp_millis(),
                                    chat_type: event.chat_type,
                                    peer_id: event.peer_id.clone(),
                                    account_id: event.account_id.clone(),
                                    pre_routed_agent: Some(mention.teammate_id.clone()),
                                    from_agent: Some(agent_id.clone()),
                                    files: vec![],
                                    chain_depth: next_depth,
                                    run_kind: RunKind::Message,
                                },
                            )
                            .await?;
                        }
                    }
                }

                let display_text = convert_mentions_to_readable(&text, &agent_id);
                if let Some(heartbeat) = &heartbeat_execution {
                    self.handle_heartbeat_success(
                        &event,
                        &agent_id,
                        &session_dir,
                        heartbeat,
                        &display_text,
                        heartbeat_started_at,
                    )
                    .await?;
                } else {
                    let stripped = strip_message_heartbeat_token(
                        &display_text,
                        self.config.heartbeat.ack_max_chars,
                    );
                    if stripped.should_skip {
                        self.emit_event(
                            "message_suppressed",
                            json!({
                                "agent_id": agent_id,
                                "reason": "heartbeat_token",
                            }),
                        )
                        .await?;
                        self.emit_event(
                            "run_succeeded",
                            json!({ "task_id": task_id, "agent_id": agent_id }),
                        )
                        .await?;
                        return Ok(());
                    }
                    let prepared = self
                        .prepare_outbound_payload(
                            &stripped.text,
                            &session_dir,
                            &HookContext {
                                channel: event.channel.clone(),
                                sender: event.sender.clone(),
                                sender_id: Some(event.sender_id.clone()),
                                message_id: event.message_id.clone(),
                                original_message: event.text.clone(),
                                agent_id: Some(agent_id.clone()),
                            },
                        )
                        .await?;

                    let outbound = OutboundEvent {
                        channel: event.channel.clone(),
                        recipient_id: event.peer_id.clone(),
                        message: prepared.message,
                        message_id: event.message_id.clone(),
                        original_message_id: event.message_id.clone(),
                        agent_id: agent_id.clone(),
                        files: prepared.files,
                    };
                    self.write_outgoing(&event, outbound, prepared.metadata)
                        .await?;
                }

                self.emit_event(
                    "run_succeeded",
                    json!({ "task_id": task_id, "agent_id": agent_id }),
                )
                .await?;
                Ok(())
            }
            Err(err) => {
                if event.run_kind == RunKind::Heartbeat {
                    self.record_heartbeat_run(
                        &agent_id,
                        &event.text,
                        Some(&err.to_string()),
                        "error",
                        heartbeat_started_at,
                        heartbeat_started_mono.elapsed().as_millis() as i64,
                    )
                    .await?;
                }

                self.emit_event(
                    "run_failed",
                    json!({ "task_id": task_id, "agent_id": agent_id, "error": err.to_string() }),
                )
                .await?;
                Err(err)
            }
        }
    }

    async fn run_single_task(
        &self,
        task_id: Uuid,
        message_id: &str,
        session_key: &str,
        working_directory: &Path,
        agent_id: &str,
        prompt: String,
        continue_session: bool,
        metadata: HashMap<String, String>,
        run_kind: RunKind,
    ) -> Result<String> {
        let run_id = Uuid::new_v4();
        let agent = self.agent_or_err(agent_id)?;
        let provider = provider_from_metadata(&metadata).unwrap_or(agent.provider);
        let model = metadata
            .get("effective_model")
            .cloned()
            .unwrap_or_else(|| agent.model.clone());
        let think_level = agent.think_level.unwrap_or_default();
        let req = RunRequest {
            run_id,
            task_id,
            session_key: session_key.to_string(),
            agent_id: agent_id.to_string(),
            provider,
            model,
            think_level,
            working_directory: working_directory.display().to_string(),
            prompt,
            continue_session,
            metadata,
            run_kind,
        };

        self.store.record_run_start(
            run_id,
            task_id,
            message_id,
            session_key,
            agent_id,
            &req.prompt,
        )?;

        let out = self.runner.run(req).await;
        match out {
            Ok(run) => {
                self.store.record_run_end(
                    run_id,
                    RunStatus::Succeeded,
                    Some(&run.text),
                    None,
                    Some(run.duration_ms),
                )?;

                if run.text.is_empty() {
                    Ok(run.stdout)
                } else {
                    Ok(run.text)
                }
            }
            Err(err) => {
                self.store.record_run_end(
                    run_id,
                    RunStatus::Failed,
                    None,
                    Some(&err.to_string()),
                    None,
                )?;
                Err(err)
            }
        }
    }

    async fn write_outgoing(
        &self,
        event: &InboundEvent,
        outbound: OutboundEvent,
        metadata: HashMap<String, String>,
    ) -> Result<()> {
        if event.channel == "chatroom" {
            return Ok(());
        }

        enqueue_outgoing_message(
            &self.config,
            &event.channel,
            &event.sender,
            &outbound.recipient_id,
            &outbound.message,
            &event.text,
            &outbound.message_id,
            &outbound.agent_id,
            outbound.files,
            metadata,
        )
        .await?;
        Ok(())
    }

    async fn handle_heartbeat_success(
        &self,
        event: &InboundEvent,
        agent_id: &str,
        session_dir: &Path,
        heartbeat: &HeartbeatExecution,
        display_text: &str,
        started_at: DateTime<Utc>,
    ) -> Result<()> {
        let normalized = strip_heartbeat_token(display_text, true, heartbeat.config.ack_max_chars);
        if normalized.should_skip {
            self.restore_heartbeat_session_timestamp(heartbeat)?;
            if heartbeat.visibility.show_ok {
                self.maybe_send_heartbeat_ok(event, agent_id, heartbeat)
                    .await?;
            }
            if heartbeat.visibility.use_indicator {
                self.emit_event(
                    "heartbeat_delivery_skipped",
                    json!({
                        "agent_id": agent_id,
                        "reason": "ack",
                    }),
                )
                .await?;
            }
            return Ok(());
        }

        if self.is_duplicate_heartbeat(heartbeat, &normalized.text, started_at)? {
            self.restore_heartbeat_session_timestamp(heartbeat)?;
            if heartbeat.visibility.use_indicator {
                self.emit_event(
                    "heartbeat_delivery_skipped",
                    json!({
                        "agent_id": agent_id,
                        "reason": "duplicate",
                    }),
                )
                .await?;
            }
            return Ok(());
        }

        let Some(channel) = heartbeat.delivery.channel.as_deref() else {
            self.restore_heartbeat_session_timestamp(heartbeat)?;
            return Ok(());
        };
        let Some(recipient_id) = heartbeat.delivery.recipient_id.as_deref() else {
            self.restore_heartbeat_session_timestamp(heartbeat)?;
            return Ok(());
        };
        if !heartbeat.visibility.show_alerts {
            self.restore_heartbeat_session_timestamp(heartbeat)?;
            return Ok(());
        }

        let prepared = self
            .prepare_outbound_payload(
                &normalized.text,
                session_dir,
                &HookContext {
                    channel: channel.to_string(),
                    sender: heartbeat.config.sender.clone(),
                    sender_id: Some("heartbeat".to_string()),
                    message_id: event.message_id.clone(),
                    original_message: event.text.clone(),
                    agent_id: Some(agent_id.to_string()),
                },
            )
            .await?;

        if prepared.message.trim().is_empty() && prepared.files.is_empty() {
            self.restore_heartbeat_session_timestamp(heartbeat)?;
            if heartbeat.visibility.show_ok {
                self.maybe_send_heartbeat_ok(event, agent_id, heartbeat)
                    .await?;
            }
            return Ok(());
        }

        enqueue_outgoing_message(
            &self.config,
            channel,
            &heartbeat.config.sender,
            recipient_id,
            &prepared.message,
            &event.text,
            &event.message_id,
            agent_id,
            prepared.files,
            prepared.metadata,
        )
        .await?;

        self.store.record_heartbeat_delivery(
            &heartbeat.base_session_key,
            agent_id,
            Some(&normalized.text),
            Some(started_at.timestamp_millis()),
        )?;

        if heartbeat.visibility.use_indicator {
            self.emit_event(
                "heartbeat_delivery_sent",
                json!({
                    "agent_id": agent_id,
                    "channel": channel,
                    "recipient_id": recipient_id,
                }),
            )
            .await?;
        }

        Ok(())
    }

    async fn maybe_send_heartbeat_ok(
        &self,
        event: &InboundEvent,
        agent_id: &str,
        heartbeat: &HeartbeatExecution,
    ) -> Result<()> {
        let Some(channel) = heartbeat.delivery.channel.as_deref() else {
            return Ok(());
        };
        let Some(recipient_id) = heartbeat.delivery.recipient_id.as_deref() else {
            return Ok(());
        };
        enqueue_outgoing_message(
            &self.config,
            channel,
            &heartbeat.config.sender,
            recipient_id,
            "HEARTBEAT_OK",
            &event.text,
            &format!("{}-heartbeat-ok", event.message_id),
            agent_id,
            vec![],
            HashMap::new(),
        )
        .await?;
        Ok(())
    }

    fn restore_heartbeat_session_timestamp(&self, heartbeat: &HeartbeatExecution) -> Result<()> {
        if let Some(previous_updated_at) = &heartbeat.previous_updated_at {
            self.store
                .restore_session_updated_at(&heartbeat.run_session_key, previous_updated_at)?;
        }
        Ok(())
    }

    fn is_duplicate_heartbeat(
        &self,
        heartbeat: &HeartbeatExecution,
        text: &str,
        started_at: DateTime<Utc>,
    ) -> Result<bool> {
        if text.trim().is_empty() {
            return Ok(false);
        }
        let Some(session) = self.store.get_session(&heartbeat.base_session_key)? else {
            return Ok(false);
        };
        let Some(previous_text) = session.last_heartbeat_text.as_deref() else {
            return Ok(false);
        };
        let Some(previous_sent_at) = session.last_heartbeat_sent_at else {
            return Ok(false);
        };
        Ok(previous_text.trim() == text.trim()
            && started_at.timestamp_millis() - previous_sent_at < 24 * 60 * 60 * 1000)
    }

    async fn record_heartbeat_run(
        &self,
        agent_id: &str,
        prompt: &str,
        output: Option<&str>,
        status: &str,
        started_at: chrono::DateTime<Utc>,
        duration_ms: i64,
    ) -> Result<()> {
        let finished_at = Utc::now();
        let run = self.store.record_heartbeat_run(
            agent_id,
            prompt,
            output,
            status,
            &started_at.to_rfc3339(),
            &finished_at.to_rfc3339(),
            duration_ms,
        )?;

        self.emit_event(
            if status == "ok" {
                "heartbeat_run_succeeded"
            } else {
                "heartbeat_run_failed"
            },
            json!({
                "run_id": run.id,
                "agent_id": agent_id,
                "status": status,
                "duration_ms": duration_ms,
            }),
        )
        .await?;

        Ok(())
    }

    async fn post_chatroom_messages(&self, from_agent: &str, posts: &[ChatroomPost]) -> Result<()> {
        for post in posts {
            let Some(team) = self.config.teams.get(&post.team_id) else {
                continue;
            };

            let message =
                self.store
                    .record_chatroom_message(&post.team_id, from_agent, &post.message)?;

            self.emit_event(
                "chatroom_message_posted",
                json!({
                    "message_id": message.id,
                    "team_id": post.team_id,
                    "from_agent": from_agent,
                }),
            )
            .await?;

            for teammate_id in &team.agents {
                if teammate_id == from_agent || !self.config.agents.contains_key(teammate_id) {
                    continue;
                }

                enqueue_chatroom_message(
                    &self.config,
                    &post.team_id,
                    teammate_id,
                    from_agent,
                    &post.message,
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn requeue_or_dead_letter(
        &self,
        processing_path: &Path,
        mut envelope: IncomingQueueEnvelope,
        error: &str,
    ) -> Result<()> {
        envelope.retry_count += 1;
        envelope.last_error = Some(error.to_string());

        if envelope.retry_count > self.config.queue.max_retries {
            if self.config.queue.dead_letter_enabled {
                let dlq_path = self
                    .config
                    .dead_letter_dir()
                    .join(queued_file_name(Utc::now().timestamp_millis()));
                fs::write(&processing_path, serde_json::to_vec_pretty(&envelope)?)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to rewrite failed envelope: {}",
                            processing_path.display()
                        )
                    })?;
                fs::rename(processing_path, &dlq_path)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to move queue payload to dead letter: {}",
                            processing_path.display()
                        )
                    })?;
                self.emit_event(
                    "run_dead_letter",
                    json!({
                        "path": dlq_path.display().to_string(),
                        "message_id": envelope.message_id,
                        "error": error,
                    }),
                )
                .await?;
            } else {
                fs::remove_file(processing_path).await.with_context(|| {
                    format!(
                        "failed to remove queue payload: {}",
                        processing_path.display()
                    )
                })?;
            }
            return Ok(());
        }

        let backoff = self.config.queue.backoff_base_ms as i64
            * 2_i64.pow(envelope.retry_count.saturating_sub(1));
        envelope.available_at_ms = Some(Utc::now().timestamp_millis() + backoff);
        let incoming_path = self.config.incoming_dir().join(queued_file_name(
            envelope.available_at_ms.unwrap_or_default(),
        ));
        fs::write(processing_path, serde_json::to_vec_pretty(&envelope)?)
            .await
            .with_context(|| {
                format!(
                    "failed to rewrite retry envelope: {}",
                    processing_path.display()
                )
            })?;
        fs::rename(processing_path, &incoming_path)
            .await
            .with_context(|| {
                format!(
                    "failed to requeue payload {} -> {}",
                    processing_path.display(),
                    incoming_path.display()
                )
            })?;
        self.emit_event(
            "run_requeued",
            json!({
                "path": incoming_path.display().to_string(),
                "message_id": envelope.message_id,
                "retry_count": envelope.retry_count,
                "error": error,
            }),
        )
        .await?;
        Ok(())
    }

    fn envelope_to_event(&self, envelope: &IncomingQueueEnvelope) -> Result<InboundEvent> {
        let ts = parse_timestamp(envelope.timestamp);
        let sender_id = envelope
            .sender_id
            .clone()
            .unwrap_or_else(|| envelope.sender.clone());
        let chat_type = parse_chat_type(envelope.chat_type.as_deref());
        let peer_id = envelope
            .peer_id
            .clone()
            .unwrap_or_else(|| sender_id.clone());

        Ok(InboundEvent {
            message_id: envelope.message_id.clone(),
            channel: envelope.channel.clone(),
            sender: envelope.sender.clone(),
            sender_id,
            text: envelope.message.clone(),
            timestamp: ts,
            chat_type,
            peer_id,
            account_id: envelope.account_id.clone(),
            files: envelope.files.clone(),
            pre_routed_agent: envelope.agent.clone(),
            from_agent: envelope.from_agent.clone(),
            chain_depth: envelope.chain_depth,
            run_kind: envelope.run_kind,
        })
    }

    fn default_agent_id(&self) -> Result<String> {
        if self.config.agents.contains_key("default") {
            return Ok("default".to_string());
        }

        self.config
            .agents
            .keys()
            .next()
            .cloned()
            .ok_or_else(|| anyhow!("no agents configured"))
    }

    fn agent_or_err(&self, agent_id: &str) -> Result<&AgentConfig> {
        self.config
            .agents
            .get(agent_id)
            .ok_or_else(|| anyhow!("agent not found: {agent_id}"))
    }

    async fn session_lock(&self, session_key: String) -> Arc<Mutex<()>> {
        let mut locks = self.session_locks.lock().await;
        locks
            .entry(session_key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn prepare_outbound_payload(
        &self,
        response: &str,
        working_directory: &Path,
        context: &HookContext,
    ) -> Result<PreparedOutbound> {
        let sanitized = strip_chatroom_tags(response);
        let transformed = transform_outgoing(&self.config, &sanitized, context).await?;
        let file_re = Regex::new(r"\[send_file:\s*([^\]]+)\]").expect("valid regex");
        let mut files = vec![];
        for caps in file_re.captures_iter(&transformed.text) {
            let Some(raw_path) = caps.get(1).map(|m| m.as_str().trim()) else {
                continue;
            };
            let resolved = resolve_output_file(working_directory, raw_path);
            if resolved.exists() {
                files.push(resolved.display().to_string());
            } else {
                warn!(path = %resolved.display(), "send_file target does not exist");
            }
        }

        let mut message = file_re
            .replace_all(transformed.text.trim(), "")
            .trim()
            .to_string();
        if message.len() > LONG_RESPONSE_THRESHOLD {
            let path = self
                .config
                .files_dir()
                .join(format!("response_{}.md", Uuid::new_v4().simple()));
            fs::write(&path, &message).await.with_context(|| {
                format!(
                    "failed to write long response attachment: {}",
                    path.display()
                )
            })?;
            let preview = message
                .chars()
                .take(LONG_RESPONSE_THRESHOLD)
                .collect::<String>();
            message = format!("{preview}\n\n(Full response attached as a file.)");
            files.push(path.display().to_string());
        }

        Ok(PreparedOutbound {
            message,
            files,
            metadata: transformed.metadata,
        })
    }

    fn build_run_metadata_for_agent(
        &self,
        agent_id: &str,
        plugin_metadata: Option<&HashMap<String, String>>,
        workspace_dir: &Path,
        run_kind: RunKind,
        light_context: bool,
    ) -> Result<HashMap<String, String>> {
        let agent = self.agent_or_err(agent_id)?;
        let mut metadata = plugin_metadata.cloned().unwrap_or_default();

        if let Some(preamble) =
            self.load_system_preamble(agent_id, agent, workspace_dir, light_context)?
        {
            metadata.insert("system_preamble".to_string(), preamble);
        }
        metadata.insert("run_kind".to_string(), run_kind_str(run_kind).to_string());

        if let Some(provider_id) = &agent.provider_id {
            let provider = self
                .config
                .custom_providers
                .get(provider_id)
                .ok_or_else(|| anyhow!("custom provider not found: {provider_id}"))?;
            apply_custom_provider_metadata(&self.config, &mut metadata, provider_id, provider)?;
        } else {
            metadata
                .entry("effective_provider".to_string())
                .or_insert_with(|| provider_kind_str(agent.provider).to_string());
            if !agent.model.trim().is_empty() {
                metadata
                    .entry("effective_model".to_string())
                    .or_insert_with(|| agent.model.clone());
            }
        }

        if matches!(agent.provider, ProviderKind::Openai) {
            if let Some(token) = config::read_codex_access_token() {
                metadata
                    .entry("openai_api_key".to_string())
                    .or_insert(token);
            }
        }

        Ok(metadata)
    }

    fn load_system_preamble(
        &self,
        agent_id: &str,
        agent: &AgentConfig,
        workspace_dir: &Path,
        light_context: bool,
    ) -> Result<Option<String>> {
        // Collect user-provided prompt sections (system_prompt + prompt_file)
        let mut user_sections = vec![];
        if let Some(system_prompt) = &agent.system_prompt {
            if !system_prompt.trim().is_empty() {
                user_sections.push(system_prompt.trim().to_string());
            }
        }

        if let Some(prompt_file) = &agent.prompt_file {
            let prompt_path = if Path::new(prompt_file).is_absolute() {
                PathBuf::from(prompt_file)
            } else {
                self.config
                    .resolve_agent_workdir(agent_id)
                    .join(prompt_file)
            };
            let content = std::fs::read_to_string(&prompt_path).with_context(|| {
                format!("failed to read prompt file: {}", prompt_path.display())
            })?;
            if !content.trim().is_empty() {
                user_sections.push(content.trim().to_string());
            }
        }

        let user_prompt = if user_sections.is_empty() {
            None
        } else {
            Some(user_sections.join("\n\n"))
        };

        let ctx = PromptContext {
            workspace_dir,
            agent_id,
            agents: &self.config.agents,
            teams: &self.config.teams,
            user_system_prompt: if light_context {
                None
            } else {
                user_prompt.as_deref()
            },
        };
        let full = SystemPromptBuilder::with_defaults().build(&ctx)?;

        Ok(Some(full))
    }

    async fn emit_event(&self, event_type: &str, payload: Value) -> Result<()> {
        self.sink.emit(event_type, payload.clone())?;
        self.store.record_event(event_type, &payload)?;
        if let Err(err) = dispatch_event(&self.config, event_type, &payload).await {
            warn!("plugin event dispatch failed for {event_type}: {err:#}");
        }
        Ok(())
    }
}

pub async fn enqueue_message(config: &RuntimeConfig, msg: EnqueueMessage) -> Result<PathBuf> {
    let available_at_ms = msg.timestamp_ms;
    let payload = json!({
        "channel": msg.channel,
        "sender": msg.sender,
        "sender_id": msg.sender_id,
        "message": msg.message,
        "timestamp": msg.timestamp_ms,
        "message_id": msg.message_id,
        "agent": msg.pre_routed_agent,
        "from_agent": msg.from_agent,
        "account_id": msg.account_id,
        "chat_type": chat_type_to_str(msg.chat_type),
        "peer_id": msg.peer_id,
        "files": msg.files,
        "retry_count": 0,
        "available_at_ms": available_at_ms,
        "last_error": null,
        "chain_depth": msg.chain_depth,
        "run_kind": msg.run_kind,
    });

    let file_name = queued_file_name(available_at_ms);
    let path = config.incoming_dir().join(file_name);

    fs::write(&path, serde_json::to_vec_pretty(&payload)?)
        .await
        .with_context(|| format!("failed to write incoming payload: {}", path.display()))?;

    Ok(path)
}

pub async fn enqueue_chatroom_message(
    config: &RuntimeConfig,
    team_id: &str,
    recipient_agent: &str,
    from_agent: &str,
    message: &str,
) -> Result<PathBuf> {
    enqueue_message(
        config,
        EnqueueMessage {
            channel: "chatroom".to_string(),
            sender: from_agent.to_string(),
            sender_id: from_agent.to_string(),
            message: format!("[Chat room #{team_id} - @{from_agent}]:\n{message}"),
            message_id: format!("chatroom-{}-{}", Uuid::new_v4().simple(), recipient_agent),
            timestamp_ms: Utc::now().timestamp_millis(),
            chat_type: ChatType::Group,
            peer_id: team_id.to_string(),
            account_id: None,
            pre_routed_agent: Some(recipient_agent.to_string()),
            from_agent: Some(from_agent.to_string()),
            files: vec![],
            chain_depth: 0,
            run_kind: RunKind::Message,
        },
    )
    .await
}

pub async fn enqueue_outgoing_message(
    config: &RuntimeConfig,
    channel: &str,
    sender: &str,
    recipient_id: &str,
    message: &str,
    original_message: &str,
    message_id: &str,
    agent_id: &str,
    files: Vec<String>,
    metadata: HashMap<String, String>,
) -> Result<PathBuf> {
    let file_name = queued_file_name(Utc::now().timestamp_millis());
    let path = config.outgoing_dir().join(file_name);
    let payload = OutgoingQueueEnvelope {
        channel: channel.to_string(),
        sender: sender.to_string(),
        message: message.to_string(),
        original_message: original_message.to_string(),
        timestamp: Utc::now().timestamp_millis(),
        message_id: message_id.to_string(),
        agent: agent_id.to_string(),
        recipient_id: recipient_id.to_string(),
        files,
        metadata,
    };

    fs::write(&path, serde_json::to_vec_pretty(&payload)?)
        .await
        .with_context(|| format!("failed to write outgoing queue payload: {}", path.display()))?;
    Ok(path)
}

pub async fn list_outgoing_messages(
    config: &RuntimeConfig,
    channel: &str,
) -> Result<Vec<QueuedOutgoingMessage>> {
    let mut entries = fs::read_dir(config.outgoing_dir()).await.with_context(|| {
        format!(
            "failed to read outgoing dir: {}",
            config.outgoing_dir().display()
        )
    })?;

    let mut messages = vec![];
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = match fs::read_to_string(&path).await {
            Ok(raw) => raw,
            Err(err) => {
                warn!(path = %path.display(), "failed to read outgoing payload: {err}");
                continue;
            }
        };
        let envelope = match serde_json::from_str::<OutgoingQueueEnvelope>(&raw) {
            Ok(envelope) => envelope,
            Err(err) => {
                warn!(path = %path.display(), "failed to parse outgoing payload: {err}");
                continue;
            }
        };
        if envelope.channel != channel {
            continue;
        }
        messages.push(QueuedOutgoingMessage {
            path,
            channel: envelope.channel,
            recipient_id: envelope.recipient_id,
            message: envelope.message,
            message_id: envelope.message_id,
            original_message: envelope.original_message,
            agent_id: envelope.agent,
            files: envelope.files,
            metadata: envelope.metadata,
        });
    }

    messages.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(messages)
}

pub async fn ack_outgoing_message(path: &Path) -> Result<()> {
    fs::remove_file(path)
        .await
        .with_context(|| format!("failed to remove outgoing payload: {}", path.display()))
}

fn parse_timestamp(timestamp: i64) -> DateTime<Utc> {
    if timestamp > 9_999_999_999 {
        DateTime::<Utc>::from_timestamp_millis(timestamp).unwrap_or_else(Utc::now)
    } else {
        DateTime::<Utc>::from_timestamp(timestamp, 0).unwrap_or_else(Utc::now)
    }
}

fn parse_chat_type(raw: Option<&str>) -> ChatType {
    match raw {
        Some("group") => ChatType::Group,
        Some("thread") => ChatType::Thread,
        _ => ChatType::Direct,
    }
}

fn chat_type_to_str(chat_type: ChatType) -> &'static str {
    match chat_type {
        ChatType::Direct => "direct",
        ChatType::Group => "group",
        ChatType::Thread => "thread",
    }
}

fn chat_type_str(chat_type: ChatType) -> &'static str {
    chat_type_to_str(chat_type)
}

fn parse_stored_chat_type(raw: Option<&str>) -> Option<ChatType> {
    match raw {
        Some("direct") => Some(ChatType::Direct),
        Some("group") => Some(ChatType::Group),
        Some("thread") => Some(ChatType::Thread),
        _ => None,
    }
}

fn run_kind_str(run_kind: RunKind) -> &'static str {
    match run_kind {
        RunKind::Message => "message",
        RunKind::Heartbeat => "heartbeat",
    }
}

fn resolve_heartbeat_session_key(
    agent_id: &str,
    session_override: Option<&str>,
    main_key: &str,
) -> String {
    let trimmed = session_override.unwrap_or_default().trim();
    if trimmed.is_empty() || matches!(trimmed, "main" | "global") {
        return build_agent_main_session_key(agent_id, main_key);
    }
    if trimmed.starts_with("agent:") {
        return trimmed.to_string();
    }
    format!("agent:{agent_id}:{trimmed}")
}

fn resolve_heartbeat_delivery_target(
    session: Option<&store::SessionSummary>,
    heartbeat: &ResolvedHeartbeatConfig,
) -> HeartbeatDeliveryTarget {
    let target = heartbeat.target.trim().to_ascii_lowercase();
    if target.is_empty() || target == "none" {
        return HeartbeatDeliveryTarget {
            channel: None,
            recipient_id: None,
            account_id: heartbeat.account_id.clone(),
        };
    }

    if target == "last" {
        let Some(session) = session else {
            return HeartbeatDeliveryTarget {
                channel: None,
                recipient_id: None,
                account_id: heartbeat.account_id.clone(),
            };
        };
        let chat_type = parse_stored_chat_type(session.last_chat_type.as_deref());
        if heartbeat.direct_policy == HeartbeatDirectPolicy::Block
            && chat_type == Some(ChatType::Direct)
        {
            return HeartbeatDeliveryTarget {
                channel: None,
                recipient_id: None,
                account_id: heartbeat
                    .account_id
                    .clone()
                    .or_else(|| session.last_account_id.clone()),
            };
        }
        return HeartbeatDeliveryTarget {
            channel: session.last_channel.clone(),
            recipient_id: session.last_peer_id.clone(),
            account_id: heartbeat
                .account_id
                .clone()
                .or_else(|| session.last_account_id.clone()),
        };
    }

    let session_chat_type =
        session.and_then(|value| parse_stored_chat_type(value.last_chat_type.as_deref()));
    let recipient_id = heartbeat
        .to
        .clone()
        .map(|value| normalize_explicit_target(&value))
        .or_else(|| {
            session.and_then(|value| {
                if value.last_channel.as_deref() == Some(target.as_str()) {
                    value.last_peer_id.clone()
                } else {
                    None
                }
            })
        });
    let chat_type = infer_chat_type_from_target(heartbeat.to.as_deref()).or(session_chat_type);
    if heartbeat.direct_policy == HeartbeatDirectPolicy::Block
        && chat_type == Some(ChatType::Direct)
    {
        return HeartbeatDeliveryTarget {
            channel: None,
            recipient_id: None,
            account_id: heartbeat
                .account_id
                .clone()
                .or_else(|| session.and_then(|value| value.last_account_id.clone())),
        };
    }
    HeartbeatDeliveryTarget {
        channel: Some(target),
        recipient_id,
        account_id: heartbeat
            .account_id
            .clone()
            .or_else(|| session.and_then(|value| value.last_account_id.clone())),
    }
}

fn infer_chat_type_from_target(raw: Option<&str>) -> Option<ChatType> {
    let trimmed = raw?.trim();
    if trimmed.starts_with("direct:") || trimmed.starts_with("user:") {
        return Some(ChatType::Direct);
    }
    if trimmed.starts_with("group:") || trimmed.starts_with("channel:") {
        return Some(ChatType::Group);
    }
    if trimmed.starts_with("thread:") || trimmed.contains('|') {
        return Some(ChatType::Thread);
    }
    None
}

fn normalize_explicit_target(raw: &str) -> String {
    let trimmed = raw.trim();
    for prefix in ["direct:", "user:", "group:", "channel:", "thread:"] {
        if let Some(value) = trimmed.strip_prefix(prefix) {
            return value.trim().to_string();
        }
    }
    trimmed.to_string()
}

fn strip_message_heartbeat_token(raw: &str, ack_max_chars: usize) -> HeartbeatTextResolution {
    strip_heartbeat_token(raw, false, ack_max_chars)
}

fn strip_heartbeat_token(
    raw: &str,
    heartbeat_mode: bool,
    ack_max_chars: usize,
) -> HeartbeatTextResolution {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return HeartbeatTextResolution {
            should_skip: true,
            text: String::new(),
        };
    }

    let token = "HEARTBEAT_OK";
    if !trimmed.contains(token) {
        return HeartbeatTextResolution {
            should_skip: false,
            text: trimmed.to_string(),
        };
    }

    let mut text = trimmed.to_string();
    loop {
        let current = text.trim().to_string();
        if current.starts_with(token) {
            text = current[token.len()..]
                .trim_start_matches(|ch: char| ch.is_whitespace() || !ch.is_alphanumeric())
                .to_string();
            continue;
        }
        if let Some(prefix) = current.strip_suffix(token) {
            text = prefix
                .trim_end_matches(|ch: char| ch.is_whitespace() || !ch.is_alphanumeric())
                .to_string();
            continue;
        }
        break;
    }

    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return HeartbeatTextResolution {
            should_skip: true,
            text: String::new(),
        };
    }
    if heartbeat_mode && collapsed.len() <= ack_max_chars {
        return HeartbeatTextResolution {
            should_skip: true,
            text: String::new(),
        };
    }
    HeartbeatTextResolution {
        should_skip: false,
        text: collapsed,
    }
}

fn apply_model_override(metadata: &mut HashMap<String, String>, raw: Option<&str>) {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if let Some((provider, model)) = raw.split_once('/') {
        let provider = provider.trim().to_ascii_lowercase();
        if matches!(
            provider.as_str(),
            "anthropic" | "openai" | "custom" | "mock"
        ) {
            metadata.insert("effective_provider".to_string(), provider);
            metadata.insert("effective_model".to_string(), model.trim().to_string());
            return;
        }
    }
    metadata.insert("effective_model".to_string(), raw.to_string());
}

fn parse_due_from_filename(path: &Path) -> Option<i64> {
    let file_name = path.file_name()?.to_str()?;
    let (prefix, _) = file_name.split_once('_')?;
    prefix.parse::<i64>().ok()
}

fn queued_file_name(available_at_ms: i64) -> String {
    format!("{available_at_ms:013}_{}.json", Uuid::new_v4().simple())
}

fn augment_prompt_with_files(prompt: String, files: &[String]) -> String {
    if files.is_empty() {
        return prompt;
    }

    let mut file_block = String::from("\n\nAttached files available on disk:\n");
    for file in files {
        file_block.push_str("- ");
        file_block.push_str(file);
        file_block.push('\n');
    }
    format!("{prompt}{file_block}")
}

fn resolve_output_file(working_directory: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        candidate
    } else {
        working_directory.join(candidate)
    }
}

fn strip_chatroom_tags(response: &str) -> String {
    let tag_re = Regex::new(r"\[#(\S+?):\s*([\s\S]*?)\]").expect("valid regex");
    tag_re.replace_all(response, "").trim().to_string()
}

/// Convert `[@teammate: message]` tags to readable `@from → @to: message` format.
fn convert_mentions_to_readable(response: &str, from_agent: &str) -> String {
    let tag_re = Regex::new(r"\[@(\S+?):\s*([\s\S]*?)\]").expect("valid regex");
    tag_re
        .replace_all(response, |caps: &regex::Captures| {
            let to = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let msg = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("");
            format!("@{from_agent} → @{to}: {msg}")
        })
        .to_string()
}

fn provider_from_metadata(metadata: &HashMap<String, String>) -> Option<ProviderKind> {
    match metadata.get("effective_provider").map(String::as_str) {
        Some("anthropic") => Some(ProviderKind::Anthropic),
        Some("openai") => Some(ProviderKind::Openai),
        Some("custom") => Some(ProviderKind::Custom),
        Some("mock") => Some(ProviderKind::Mock),
        _ => None,
    }
}

fn provider_kind_str(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Openai => "openai",
        ProviderKind::Custom => "custom",
        ProviderKind::Mock => "mock",
    }
}

fn apply_custom_provider_metadata(
    config: &RuntimeConfig,
    metadata: &mut HashMap<String, String>,
    provider_id: &str,
    provider: &CustomProviderConfig,
) -> Result<()> {
    metadata.insert("effective_provider".to_string(), "custom".to_string());
    metadata.insert("custom_provider_id".to_string(), provider_id.to_string());
    metadata.insert(
        "custom_harness".to_string(),
        match provider.harness {
            ProviderHarness::Anthropic => "anthropic".to_string(),
            ProviderHarness::Openai => "openai".to_string(),
        },
    );
    metadata.insert("custom_base_url".to_string(), provider.base_url.clone());
    let api_key = config
        .custom_provider_api_key(provider_id)?
        .ok_or_else(|| anyhow!("custom provider missing api_key: {provider_id}"))?;
    metadata.insert("custom_api_key".to_string(), api_key);
    if let Some(model) = &provider.model {
        if !model.trim().is_empty() {
            metadata.insert("effective_model".to_string(), model.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip_preserves_chain_depth() {
        let payload = json!({
            "channel": "slack",
            "sender": "alice",
            "sender_id": "U123",
            "message": "[Message from teammate @default]:\nhi",
            "timestamp": 1711100000000_i64,
            "message_id": "internal-abc-tester",
            "agent": "tester",
            "chain_depth": 3,
        });
        let envelope: IncomingQueueEnvelope = serde_json::from_value(payload).expect("deserialize");
        assert_eq!(envelope.chain_depth, 3);
        assert_eq!(envelope.agent.as_deref(), Some("tester"));
    }

    #[test]
    fn envelope_defaults_chain_depth_to_zero() {
        let payload = json!({
            "channel": "slack",
            "sender": "alice",
            "message": "hello",
            "timestamp": 1711100000000_i64,
            "message_id": "msg-1",
        });
        let envelope: IncomingQueueEnvelope = serde_json::from_value(payload).expect("deserialize");
        assert_eq!(envelope.chain_depth, 0);
    }

    #[test]
    fn enqueue_payload_includes_chain_depth() {
        let msg = EnqueueMessage {
            channel: "test".to_string(),
            sender: "bob".to_string(),
            sender_id: "bob_1".to_string(),
            message: "hi".to_string(),
            message_id: "m1".to_string(),
            timestamp_ms: 1711100000000,
            chat_type: ChatType::Direct,
            peer_id: "bob_1".to_string(),
            account_id: None,
            pre_routed_agent: Some("tester".to_string()),
            from_agent: None,
            files: vec![],
            chain_depth: 5,
            run_kind: RunKind::Message,
        };

        // Simulate what enqueue_message builds
        let payload = json!({
            "channel": msg.channel,
            "sender": msg.sender,
            "sender_id": msg.sender_id,
            "message": msg.message,
            "timestamp": msg.timestamp_ms,
            "message_id": msg.message_id,
            "agent": msg.pre_routed_agent,
            "chain_depth": msg.chain_depth,
            "run_kind": msg.run_kind,
        });

        let envelope: IncomingQueueEnvelope = serde_json::from_value(payload).expect("roundtrip");
        assert_eq!(envelope.chain_depth, 5);
        assert_eq!(envelope.agent.as_deref(), Some("tester"));
    }
}
