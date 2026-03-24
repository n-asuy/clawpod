use std::collections::HashSet;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{Html, Json};
use axum::routing::{get, post};
use axum::{serve, Router};
use chrono::Utc;
use config::{ensure_runtime_dirs, write_config, RuntimeConfig};
use domain::{AgentConfig, TeamConfig};
use heartbeat::{HeartbeatLoopControl, HeartbeatService};
use observer::{mark_component_error, mark_component_ok, snapshot_json, FileEventSink};
use queue::{enqueue_chatroom_message, enqueue_outgoing_message, list_outgoing_messages};
use serde::Deserialize;
use serde_json::{json, Value};
use store::StateStore;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower_http::cors::CorsLayer;
use tracing::info;
use uuid::Uuid;

type ApiResult<T> = std::result::Result<T, (StatusCode, String)>;

use axum::http::StatusCode;

#[derive(Clone)]
struct AppState {
    config: Arc<RwLock<RuntimeConfig>>,
    config_path: PathBuf,
    store: StateStore,
    sink: FileEventSink,
    heartbeat_service: Option<Arc<HeartbeatService>>,
    heartbeat_control: Option<HeartbeatLoopControl>,
}

#[derive(Debug, Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
    #[serde(default)]
    session_key: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PendingResponsesQuery {
    channel: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatroomQuery {
    limit: Option<usize>,
    #[serde(default)]
    since: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SenderAccessQuery {
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateResponseRequest {
    channel: String,
    recipient_id: String,
    message: String,
    #[serde(default)]
    original_message: Option<String>,
    #[serde(default)]
    sender: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct CreateAgentRequest {
    id: String,
    agent: AgentConfig,
}

#[derive(Debug, Deserialize)]
struct UpdateAgentRequest {
    agent: AgentConfig,
}

#[derive(Debug, Deserialize)]
struct CreateTeamRequest {
    id: String,
    team: TeamConfig,
}

#[derive(Debug, Deserialize)]
struct UpdateTeamRequest {
    team: TeamConfig,
}

#[derive(Debug, Deserialize)]
struct PairingApproveRequest {
    code: String,
}

#[derive(Debug, Deserialize)]
struct SaveFileRequest {
    content: String,
}

#[derive(Debug, Deserialize)]
struct PostChatroomRequest {
    message: String,
}

/// Workspace files that can be edited via the API.
/// Each entry is (subdirectory relative to agent root, display name).
const EDITABLE_FILES: &[(&str, &str)] = &[
    (".clawpod", "SOUL.md"),
    ("", "AGENTS.md"),
    ("", "heartbeat.md"),
    ("", "focus.md"),
    ("memory", "reflections.md"),
    ("memory", "curiosity_journal.md"),
];

pub async fn run(
    config: RuntimeConfig,
    config_path: PathBuf,
    store: StateStore,
    sink: FileEventSink,
    heartbeat_service: Option<Arc<HeartbeatService>>,
    heartbeat_control: Option<HeartbeatLoopControl>,
) -> Result<()> {
    let bind_addr = config.server_listen_addr();
    let office_url = config.office_url();
    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        config_path,
        store,
        sink,
        heartbeat_service,
        heartbeat_control,
    };

    let app = Router::new()
        .route("/", get(office))
        .route("/office", get(office))
        .route("/office/*rest", get(office))
        .route("/health", get(health))
        .route("/api/health", get(api_health))
        .route("/api/settings", get(get_settings).put(update_settings))
        .route("/api/runtime/restart", post(restart_runtime))
        .route("/api/agents", get(list_agents).post(create_agent))
        .route(
            "/api/agents/:agent_id",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route(
            "/api/agents/:agent_id/sessions/clear",
            post(clear_agent_sessions_api),
        )
        .route("/api/agents/:agent_id/files", get(list_agent_files))
        .route(
            "/api/agents/:agent_id/files/:filename",
            get(get_agent_file).put(put_agent_file),
        )
        .route("/api/teams", get(list_teams).post(create_team))
        .route(
            "/api/teams/:team_id",
            get(get_team).put(update_team).delete(delete_team),
        )
        .route("/api/access/senders", get(list_sender_access_api))
        .route(
            "/api/access/senders/:channel/:sender_id/approve",
            post(approve_sender_access_api),
        )
        .route(
            "/api/access/senders/:channel/:sender_id/reject",
            post(reject_sender_access_api),
        )
        .route("/api/pairing/approve", post(approve_pairing_api))
        .route("/api/queue/status", get(get_queue_status))
        .route(
            "/api/responses",
            get(list_pending_responses).post(create_response),
        )
        .route("/api/responses/pending", get(list_pending_responses))
        .route("/api/runs", get(list_runs))
        .route("/api/sessions", get(list_sessions_api))
        .route("/api/logs/events", get(list_events))
        .route(
            "/api/chatroom/:team_id",
            get(get_chatroom).post(post_chatroom),
        )
        .route("/api/heartbeat/runs", get(list_heartbeat_runs))
        .route("/api/heartbeat/run", post(trigger_heartbeat_run))
        .route("/api/models", get(list_models))
        .route("/api/doctor", get(api_doctor))
        .route("/api/events/stream", get(stream_events))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind office server on {bind_addr}"))?;
    info!("ClawPod office listening on {office_url}");
    mark_component_ok("office");
    let result = serve(listener, app)
        .await
        .context("office server exited unexpectedly");
    if let Err(err) = &result {
        mark_component_error("office", err.to_string());
    }
    result
}

async fn office(State(_state): State<AppState>) -> ApiResult<Html<&'static str>> {
    Ok(Html(OFFICE_HTML))
}

async fn health() -> Json<Value> {
    Json(snapshot_json())
}

async fn api_health(State(_state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(snapshot_json()))
}

async fn get_settings(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let config = state.config.read().await.masked_for_display();
    Ok(Json(json!({
        "config_path": state.config_path.display().to_string(),
        "config": config,
        "runtime": {
            "restart_supported": runtime_restart_supported(&state),
        },
    })))
}

async fn update_settings(
    State(state): State<AppState>,
    Json(mut payload): Json<RuntimeConfig>,
) -> ApiResult<Json<Value>> {
    let previous = state.config.read().await.clone();
    payload.restore_masked_secrets(&previous);
    payload.validate().map_err(internal_error)?;
    ensure_runtime_dirs(&payload).map_err(internal_error)?;
    write_config(&state.config_path, &payload).map_err(internal_error)?;
    let heartbeat_hot_reloaded = state
        .heartbeat_control
        .as_ref()
        .is_some_and(|control| control.update_from_config(&payload));
    *state.config.write().await = payload.clone();
    emit_server_event(
        &state,
        "settings_updated",
        json!({
            "config_path": state.config_path.display().to_string(),
            "heartbeat_hot_reloaded": heartbeat_hot_reloaded,
        }),
    );
    Ok(Json(json!({
        "ok": true,
        "message": "settings saved; restart runtime for full effect",
        "config": payload.masked_for_display(),
        "runtime": {
            "restart_supported": runtime_restart_supported(&state),
        },
        "heartbeat_hot_reloaded": heartbeat_hot_reloaded,
    })))
}

async fn restart_runtime(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    if !runtime_restart_supported(&state) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime restart is not available in office-only mode".to_string(),
        ));
    }

    emit_server_event(
        &state,
        "runtime_restart_requested",
        json!({
            "pid": std::process::id(),
        }),
    );

    tokio::spawn(async {
        sleep(Duration::from_millis(200)).await;
        std::process::exit(0);
    });

    Ok(Json(json!({
        "ok": true,
        "message": "runtime restart scheduled",
    })))
}

async fn list_agents(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let config = state.config.read().await;
    Ok(Json(json!({ "agents": config.agents })))
}

async fn get_agent(
    State(state): State<AppState>,
    AxumPath(agent_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let config = state.config.read().await;
    let Some(agent) = config.agents.get(&agent_id) else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("agent not found: {agent_id}"),
        ));
    };
    Ok(Json(json!({ "id": agent_id, "agent": agent })))
}

async fn create_agent(
    State(state): State<AppState>,
    Json(payload): Json<CreateAgentRequest>,
) -> ApiResult<Json<Value>> {
    let agent_id = normalize_identifier(&payload.id, "agent id").map_err(internal_error)?;
    validate_agent_config(&payload.agent).map_err(internal_error)?;
    let next = mutate_config(&state, |config| {
        if config.agents.contains_key(&agent_id) {
            bail!("agent already exists: {agent_id}");
        }
        config
            .agents
            .insert(agent_id.clone(), payload.agent.clone());
        Ok(())
    })
    .await?;

    // Bootstrap workspace so SOUL.md / AGENTS.md are immediately available.
    let config = state.config.read().await;
    let agent_root = config.resolve_agent_workdir(&agent_id);
    if let Err(e) = agent::ensure_agent_workspace(
        &agent_id,
        &payload.agent,
        &config.agents,
        &config.teams,
        &agent_root,
    ) {
        tracing::warn!("failed to bootstrap workspace for {agent_id}: {e:#}");
    }
    drop(config);

    emit_server_event(&state, "agent_created", json!({ "agent_id": agent_id }));
    Ok(Json(json!({
        "ok": true,
        "agents": next.agents,
    })))
}

async fn update_agent(
    State(state): State<AppState>,
    AxumPath(agent_id): AxumPath<String>,
    Json(payload): Json<UpdateAgentRequest>,
) -> ApiResult<Json<Value>> {
    validate_agent_config(&payload.agent).map_err(internal_error)?;
    let agent_id = normalize_identifier(&agent_id, "agent id").map_err(internal_error)?;
    let next = mutate_config(&state, |config| {
        if !config.agents.contains_key(&agent_id) {
            bail!("agent not found: {agent_id}");
        }
        config
            .agents
            .insert(agent_id.clone(), payload.agent.clone());
        Ok(())
    })
    .await?;
    emit_server_event(&state, "agent_updated", json!({ "agent_id": agent_id }));
    Ok(Json(json!({
        "ok": true,
        "agents": next.agents,
    })))
}

async fn delete_agent(
    State(state): State<AppState>,
    AxumPath(agent_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let agent_id = normalize_identifier(&agent_id, "agent id").map_err(internal_error)?;
    let next = mutate_config(&state, |config| {
        if !config.agents.contains_key(&agent_id) {
            bail!("agent not found: {agent_id}");
        }
        if config.agents.len() <= 1 {
            bail!("cannot remove the last agent");
        }
        if let Some((team_id, _team)) = config.teams.iter().find(|(_, team)| {
            team.leader_agent == agent_id || team.agents.iter().any(|id| id == &agent_id)
        }) {
            bail!("agent {agent_id} is still referenced by team {team_id}");
        }
        config.agents.remove(&agent_id);
        Ok(())
    })
    .await?;
    state
        .store
        .clear_agent_sessions(&agent_id)
        .map_err(internal_error)?;
    emit_server_event(&state, "agent_deleted", json!({ "agent_id": agent_id }));
    Ok(Json(json!({
        "ok": true,
        "agents": next.agents,
    })))
}

async fn clear_agent_sessions_api(
    State(state): State<AppState>,
    AxumPath(agent_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let agent_id = normalize_identifier(&agent_id, "agent id").map_err(internal_error)?;
    state
        .store
        .clear_agent_sessions(&agent_id)
        .map_err(internal_error)?;
    emit_server_event(
        &state,
        "agent_sessions_cleared",
        json!({ "agent_id": agent_id }),
    );
    Ok(Json(json!({ "ok": true })))
}

async fn list_teams(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let config = state.config.read().await;
    Ok(Json(json!({ "teams": config.teams })))
}

async fn get_team(
    State(state): State<AppState>,
    AxumPath(team_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let config = state.config.read().await;
    let Some(team) = config.teams.get(&team_id) else {
        return Err((StatusCode::NOT_FOUND, format!("team not found: {team_id}")));
    };
    Ok(Json(json!({ "id": team_id, "team": team })))
}

async fn create_team(
    State(state): State<AppState>,
    Json(payload): Json<CreateTeamRequest>,
) -> ApiResult<Json<Value>> {
    let team_id = normalize_identifier(&payload.id, "team id").map_err(internal_error)?;
    let next = mutate_config(&state, |config| {
        if config.teams.contains_key(&team_id) {
            bail!("team already exists: {team_id}");
        }
        validate_team_config(config, &payload.team)?;
        config.teams.insert(team_id.clone(), payload.team.clone());
        Ok(())
    })
    .await?;
    emit_server_event(&state, "team_created", json!({ "team_id": team_id }));
    Ok(Json(json!({
        "ok": true,
        "teams": next.teams,
    })))
}

async fn update_team(
    State(state): State<AppState>,
    AxumPath(team_id): AxumPath<String>,
    Json(payload): Json<UpdateTeamRequest>,
) -> ApiResult<Json<Value>> {
    let team_id = normalize_identifier(&team_id, "team id").map_err(internal_error)?;
    let next = mutate_config(&state, |config| {
        if !config.teams.contains_key(&team_id) {
            bail!("team not found: {team_id}");
        }
        validate_team_config(config, &payload.team)?;
        config.teams.insert(team_id.clone(), payload.team.clone());
        Ok(())
    })
    .await?;
    emit_server_event(&state, "team_updated", json!({ "team_id": team_id }));
    Ok(Json(json!({
        "ok": true,
        "teams": next.teams,
    })))
}

async fn delete_team(
    State(state): State<AppState>,
    AxumPath(team_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let team_id = normalize_identifier(&team_id, "team id").map_err(internal_error)?;
    let next = mutate_config(&state, |config| {
        if config.teams.remove(&team_id).is_none() {
            bail!("team not found: {team_id}");
        }
        Ok(())
    })
    .await?;
    emit_server_event(&state, "team_deleted", json!({ "team_id": team_id }));
    Ok(Json(json!({
        "ok": true,
        "teams": next.teams,
    })))
}

async fn get_queue_status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let config = state.config.read().await.clone();
    let incoming = config.incoming_dir();
    let processing = config.processing_dir();
    let outgoing = config.outgoing_dir();
    let dead_letter = config.dead_letter_dir();
    Ok(Json(json!({
        "incoming": count_json_files(&incoming).await.map_err(internal_error)?,
        "processing": count_json_files(&processing).await.map_err(internal_error)?,
        "outgoing": count_json_files(&outgoing).await.map_err(internal_error)?,
        "dead_letter": count_json_files(&dead_letter).await.map_err(internal_error)?,
    })))
}

async fn list_sender_access_api(
    State(state): State<AppState>,
    Query(query): Query<SenderAccessQuery>,
) -> ApiResult<Json<Value>> {
    let entries = state
        .store
        .list_sender_access(query.channel.as_deref(), query.status.as_deref())
        .map_err(internal_error)?;
    Ok(Json(json!({ "entries": entries })))
}

async fn approve_sender_access_api(
    State(state): State<AppState>,
    AxumPath((channel, sender_id)): AxumPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    let Some(entry) = state
        .store
        .approve_sender_access(&channel, &sender_id)
        .map_err(internal_error)?
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("sender access not found: {channel}/{sender_id}"),
        ));
    };
    emit_server_event(
        &state,
        "sender_access_approved",
        json!({ "channel": channel, "sender_id": sender_id }),
    );
    Ok(Json(json!({ "ok": true, "entry": entry })))
}

async fn reject_sender_access_api(
    State(state): State<AppState>,
    AxumPath((channel, sender_id)): AxumPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    let Some(entry) = state
        .store
        .reject_sender_access(&channel, &sender_id)
        .map_err(internal_error)?
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("sender access not found: {channel}/{sender_id}"),
        ));
    };
    emit_server_event(
        &state,
        "sender_access_rejected",
        json!({ "channel": channel, "sender_id": sender_id }),
    );
    Ok(Json(json!({ "ok": true, "entry": entry })))
}

async fn approve_pairing_api(
    State(state): State<AppState>,
    Json(payload): Json<PairingApproveRequest>,
) -> ApiResult<Json<Value>> {
    let entry = state
        .store
        .find_pending_by_code(&payload.code)
        .map_err(internal_error)?;

    let Some(entry) = entry else {
        return Err((
            StatusCode::NOT_FOUND,
            "no pending sender found for the given pairing code".to_string(),
        ));
    };

    if let Some(expires_str) = &entry.pairing_code_expires_at {
        if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(expires_str) {
            if pairing::is_code_expired(&expires.with_timezone(&Utc)) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "pairing code has expired".to_string(),
                ));
            }
        }
    }

    let approved = state
        .store
        .approve_sender_access(&entry.channel, &entry.sender_id)
        .map_err(internal_error)?;

    let Some(approved) = approved else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to approve sender access after pairing code match".to_string(),
        ));
    };

    state
        .store
        .clear_pairing_code(&entry.channel, &entry.sender_id)
        .map_err(internal_error)?;

    emit_server_event(
        &state,
        "pairing_approved",
        json!({ "channel": entry.channel, "sender_id": entry.sender_id }),
    );

    Ok(Json(json!({ "ok": true, "entry": approved })))
}

async fn list_pending_responses(
    State(state): State<AppState>,
    Query(query): Query<PendingResponsesQuery>,
) -> ApiResult<Json<Value>> {
    let config = state.config.read().await.clone();
    let responses = if let Some(channel) = query.channel {
        list_outgoing_messages(&config, &channel)
            .await
            .map_err(internal_error)?
            .into_iter()
            .map(outgoing_to_json)
            .collect::<Vec<_>>()
    } else {
        read_all_pending_responses(&config)
            .await
            .map_err(internal_error)?
    };
    Ok(Json(json!({ "responses": responses })))
}

async fn create_response(
    State(state): State<AppState>,
    Json(payload): Json<CreateResponseRequest>,
) -> ApiResult<Json<Value>> {
    let config = state.config.read().await.clone();
    let message_id = payload
        .message_id
        .unwrap_or_else(|| format!("manual_{}", Uuid::new_v4().simple()));
    let path = enqueue_outgoing_message(
        &config,
        &payload.channel,
        payload.sender.as_deref().unwrap_or("office"),
        &payload.recipient_id,
        &payload.message,
        payload.original_message.as_deref().unwrap_or(""),
        &message_id,
        payload.agent_id.as_deref().unwrap_or("office"),
        payload.files,
        payload.metadata,
    )
    .await
    .map_err(internal_error)?;
    emit_server_event(
        &state,
        "manual_response_queued",
        json!({
            "path": path.display().to_string(),
            "channel": payload.channel,
            "recipient_id": payload.recipient_id,
        }),
    );
    Ok(Json(json!({
        "ok": true,
        "path": path.display().to_string(),
        "message_id": message_id,
    })))
}

async fn list_runs(
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    let runs = state
        .store
        .list_recent_runs_filtered(
            query.limit.unwrap_or(50),
            query.session_key.as_deref(),
            query.agent_id.as_deref(),
        )
        .map_err(internal_error)?;
    Ok(Json(json!({ "runs": runs })))
}

async fn list_sessions_api(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let sessions = state.store.list_sessions().map_err(internal_error)?;
    Ok(Json(json!({ "sessions": sessions })))
}

async fn list_events(
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    let events = state
        .store
        .list_recent_events(query.limit.unwrap_or(100))
        .map_err(internal_error)?;
    Ok(Json(json!({ "events": events })))
}

async fn get_chatroom(
    State(state): State<AppState>,
    AxumPath(team_id): AxumPath<String>,
    Query(query): Query<ChatroomQuery>,
) -> ApiResult<Json<Value>> {
    let team_id = normalize_identifier(&team_id, "team id").map_err(internal_error)?;
    let config = state.config.read().await;
    if !config.teams.contains_key(&team_id) {
        return Err((StatusCode::NOT_FOUND, format!("team not found: {team_id}")));
    }
    drop(config);

    let messages = state
        .store
        .list_chatroom_messages(&team_id, query.limit.unwrap_or(100), query.since)
        .map_err(internal_error)?;
    Ok(Json(json!({ "team_id": team_id, "messages": messages })))
}

async fn post_chatroom(
    State(state): State<AppState>,
    AxumPath(team_id): AxumPath<String>,
    Json(payload): Json<PostChatroomRequest>,
) -> ApiResult<Json<Value>> {
    let team_id = normalize_identifier(&team_id, "team id").map_err(internal_error)?;
    let message = payload.message.trim();
    if message.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "message must not be empty".to_string(),
        ));
    }

    let config = state.config.read().await.clone();
    let Some(team) = config.teams.get(&team_id).cloned() else {
        return Err((StatusCode::NOT_FOUND, format!("team not found: {team_id}")));
    };

    let entry = state
        .store
        .record_chatroom_message(&team_id, "user", message)
        .map_err(internal_error)?;

    for agent_id in &team.agents {
        enqueue_chatroom_message(&config, &team_id, agent_id, "user", message)
            .await
            .map_err(internal_error)?;
    }

    emit_server_event(
        &state,
        "chatroom_message_posted",
        json!({
            "message_id": entry.id,
            "team_id": team_id,
            "from_agent": "user",
        }),
    );

    Ok(Json(json!({
        "ok": true,
        "message": entry,
    })))
}

async fn list_heartbeat_runs(
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    let config = state.config.read().await;
    let runs = state
        .store
        .list_heartbeat_runs(query.limit.unwrap_or(100), query.agent_id.as_deref())
        .map_err(internal_error)?;
    Ok(Json(json!({
        "config": {
            "enabled": config.heartbeat.enabled,
            "interval_sec": config.heartbeat.interval_sec,
            "sender": config.heartbeat.sender,
            "restart_supported": runtime_restart_supported(&state),
        },
        "runs": runs,
    })))
}

#[derive(Debug, Deserialize)]
struct TriggerHeartbeatBody {
    agent_id: String,
}

async fn trigger_heartbeat_run(
    State(state): State<AppState>,
    Json(body): Json<TriggerHeartbeatBody>,
) -> ApiResult<Json<Value>> {
    let service = state.heartbeat_service.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat service not available".to_string(),
        )
    })?;

    let view = service
        .run_once(&body.agent_id, domain::HeartbeatRunReason::Manual)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        serde_json::to_value(view).map_err(|e| internal_error(e.into()))?,
    ))
}

async fn stream_events(
    State(state): State<AppState>,
) -> ApiResult<Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>>> {
    let stream = BroadcastStream::new(state.sink.subscribe()).map(|item| {
        let event = match item {
            Ok(record) => Event::default().data(
                json!({
                    "timestamp": record.timestamp,
                    "event_type": record.event_type,
                    "payload": record.payload,
                })
                .to_string(),
            ),
            Err(err) => Event::default().data(json!({ "error": err.to_string() }).to_string()),
        };
        Ok::<_, Infallible>(event)
    });
    Ok(Sse::new(stream))
}

// ---------------------------------------------------------------------------
// Workspace file API (SOUL.md, AGENTS.md, etc.)
// ---------------------------------------------------------------------------

async fn list_agent_files(
    State(state): State<AppState>,
    AxumPath(agent_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let config = state.config.read().await;
    if !config.agents.contains_key(&agent_id) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("agent not found: {agent_id}"),
        ));
    }
    let agent_root = config.resolve_agent_workdir(&agent_id);
    let mut files = vec![];
    for &(subdir, name) in EDITABLE_FILES {
        let path = if subdir.is_empty() {
            agent_root.join(name)
        } else {
            agent_root.join(subdir).join(name)
        };
        let (exists, size) = match tokio::fs::metadata(&path).await {
            Ok(m) => (true, m.len()),
            Err(_) => (false, 0),
        };
        files.push(json!({
            "name": name,
            "exists": exists,
            "size": size,
        }));
    }
    Ok(Json(json!({ "files": files })))
}

async fn get_agent_file(
    State(state): State<AppState>,
    AxumPath((agent_id, filename)): AxumPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    let config = state.config.read().await;
    if !config.agents.contains_key(&agent_id) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("agent not found: {agent_id}"),
        ));
    }
    let path = resolve_workspace_file(&config, &agent_id, &filename)?;
    let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    Ok(Json(json!({ "name": filename, "content": content })))
}

async fn put_agent_file(
    State(state): State<AppState>,
    AxumPath((agent_id, filename)): AxumPath<(String, String)>,
    Json(payload): Json<SaveFileRequest>,
) -> ApiResult<Json<Value>> {
    let config = state.config.read().await;
    if !config.agents.contains_key(&agent_id) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("agent not found: {agent_id}"),
        ));
    }
    let path = resolve_workspace_file(&config, &agent_id, &filename)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| internal_error(e.into()))?;
    }
    tokio::fs::write(&path, &payload.content)
        .await
        .map_err(|e| internal_error(e.into()))?;
    emit_server_event(
        &state,
        "workspace_file_saved",
        json!({ "agent_id": agent_id, "file": filename }),
    );
    Ok(Json(json!({ "ok": true, "name": filename })))
}

fn resolve_workspace_file(
    config: &RuntimeConfig,
    agent_id: &str,
    filename: &str,
) -> Result<PathBuf, (StatusCode, String)> {
    for &(subdir, name) in EDITABLE_FILES {
        if name == filename {
            let agent_root = config.resolve_agent_workdir(agent_id);
            let path = if subdir.is_empty() {
                agent_root.join(name)
            } else {
                agent_root.join(subdir).join(name)
            };
            return Ok(path);
        }
    }
    Err((
        StatusCode::BAD_REQUEST,
        format!("file not editable: {filename}"),
    ))
}

async fn count_json_files(path: &Path) -> Result<usize> {
    let mut count = 0usize;
    let mut entries = tokio::fs::read_dir(path)
        .await
        .with_context(|| format!("failed to read dir: {}", path.display()))?;
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if file_type.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
        {
            count += 1;
        }
    }
    Ok(count)
}

async fn read_all_pending_responses(config: &RuntimeConfig) -> Result<Vec<Value>> {
    let mut responses = vec![];
    let mut entries = tokio::fs::read_dir(config.outgoing_dir())
        .await
        .with_context(|| {
            format!(
                "failed to read outgoing dir: {}",
                config.outgoing_dir().display()
            )
        })?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let raw = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read outgoing payload: {}", path.display()))?;
        let mut payload: Value = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse outgoing payload: {}", path.display()))?;
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "path".to_string(),
                Value::String(path.display().to_string()),
            );
        }
        responses.push(payload);
    }
    responses.sort_by(|a, b| {
        a.get("path")
            .and_then(Value::as_str)
            .cmp(&b.get("path").and_then(Value::as_str))
    });
    Ok(responses)
}

fn outgoing_to_json(message: queue::QueuedOutgoingMessage) -> Value {
    json!({
        "path": message.path.display().to_string(),
        "channel": message.channel,
        "recipient_id": message.recipient_id,
        "message": message.message,
        "message_id": message.message_id,
        "original_message": message.original_message,
        "agent_id": message.agent_id,
        "files": message.files,
        "metadata": message.metadata,
    })
}

fn emit_server_event(state: &AppState, event_type: &str, payload: Value) {
    if let Err(err) = state.sink.emit(event_type, payload.clone()) {
        tracing::warn!("failed to emit server event {event_type}: {err:#}");
    }
    if let Err(err) = state.store.record_event(event_type, &payload) {
        tracing::warn!("failed to persist server event {event_type}: {err:#}");
    }
}

fn runtime_restart_supported(state: &AppState) -> bool {
    state.heartbeat_control.is_some()
}

async fn mutate_config<F>(state: &AppState, mutate: F) -> ApiResult<RuntimeConfig>
where
    F: FnOnce(&mut RuntimeConfig) -> Result<()>,
{
    let mut next = state.config.read().await.clone();
    mutate(&mut next).map_err(internal_error)?;
    next.validate().map_err(internal_error)?;
    ensure_runtime_dirs(&next).map_err(internal_error)?;
    write_config(&state.config_path, &next).map_err(internal_error)?;
    *state.config.write().await = next.clone();
    Ok(next)
}

fn internal_error(err: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn normalize_identifier(value: &str, label: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label} must not be empty");
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        bail!("{label} may only contain letters, numbers, '_' and '-'");
    }
    Ok(trimmed.to_string())
}

fn validate_agent_config(agent: &AgentConfig) -> Result<()> {
    if agent.name.trim().is_empty() {
        bail!("agent.name must not be empty");
    }
    if agent.model.trim().is_empty() {
        bail!("agent.model must not be empty");
    }
    Ok(())
}

fn validate_team_config(config: &RuntimeConfig, team: &TeamConfig) -> Result<()> {
    if team.name.trim().is_empty() {
        bail!("team.name must not be empty");
    }
    if team.agents.is_empty() {
        bail!("team.agents must include at least one agent");
    }
    if !config.agents.contains_key(&team.leader_agent) {
        bail!("team.leader_agent must reference an existing agent");
    }
    if !team
        .agents
        .iter()
        .any(|agent_id| agent_id == &team.leader_agent)
    {
        bail!("team.leader_agent must also be listed in team.agents");
    }
    let mut seen = HashSet::new();
    for agent_id in &team.agents {
        if !config.agents.contains_key(agent_id) {
            bail!("team.agents contains unknown agent: {agent_id}");
        }
        if !seen.insert(agent_id) {
            bail!("team.agents contains duplicate agent: {agent_id}");
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ModelsQuery {
    provider: Option<String>,
}

async fn list_models(Query(query): Query<ModelsQuery>) -> ApiResult<Json<Value>> {
    let catalog = domain::model_catalog();
    let filtered: Vec<&domain::ModelDefinition> = if let Some(ref provider_str) = query.provider {
        let provider = match provider_str.as_str() {
            "anthropic" => Some(domain::ProviderKind::Anthropic),
            "openai" => Some(domain::ProviderKind::Openai),
            _ => None,
        };
        match provider {
            Some(p) => catalog.iter().filter(|m| m.provider == p).collect(),
            None => vec![],
        }
    } else {
        catalog.iter().collect()
    };
    Ok(Json(json!({ "models": filtered })))
}

async fn api_doctor(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let config = state.config.read().await;

    let claude_version = command_version_check("claude", &["--version"]).await;
    let codex_version = command_version_check("codex", &["--version"]).await;
    let codex_auth = config::check_codex_auth();

    let has_openai_agents = config
        .agents
        .values()
        .any(|a| matches!(a.provider, domain::ProviderKind::Openai));

    let mut warnings: Vec<String> = vec![];
    if has_openai_agents {
        if !codex_auth.auth_file_exists {
            warnings.push(
                "OpenAI agents configured but codex is not logged in. Run 'codex login'.".into(),
            );
        } else if codex_auth.token_expired {
            warnings.push("Codex auth token expired. Run 'codex login' to refresh.".into());
        }
    }

    Ok(Json(json!({
        "claude": {
            "installed": claude_version.is_some(),
            "version": claude_version.unwrap_or_default(),
        },
        "codex": {
            "installed": codex_version.is_some(),
            "version": codex_version.unwrap_or_default(),
        },
        "codex_auth": codex_auth,
        "warnings": warnings,
    })))
}

async fn command_version_check(program: &str, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

const OFFICE_HTML: &str = include_str!("office.html");
