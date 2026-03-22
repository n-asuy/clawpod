use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use config::{evaluate_ingress_policy, IngressDecision, RuntimeConfig};
use domain::ChatType;
use futures_util::{SinkExt, StreamExt};
use observer::{mark_component_disabled, mark_component_error, mark_component_ok, FileEventSink};
use queue::{
    ack_outgoing_message, enqueue_message, enqueue_outgoing_message, list_outgoing_messages,
    EnqueueMessage,
};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use store::{SenderAccessRegistration, StateStore, VerifyResult};
use tokio::fs;
use tokio::time::{sleep, Duration, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{error, info, warn};

const SLACK_POLL_INTERVAL_SECS: u64 = 3;
const SLACK_DISCOVERY_REFRESH_SECS: u64 = 60;
const SLACK_SOCKET_INITIAL_BACKOFF_MS: u64 = 1_000;
const SLACK_SOCKET_MAX_BACKOFF_MS: u64 = 30_000;
const SLACK_SOCKET_MAX_JITTER_MS: u64 = 250;

#[derive(Clone)]
struct SlackDiagnostics {
    sink: Option<FileEventSink>,
    store: Option<StateStore>,
}

impl SlackDiagnostics {
    fn new(config: &RuntimeConfig) -> Self {
        let sink = match FileEventSink::new(config.event_log_path()) {
            Ok(sink) => Some(sink),
            Err(err) => {
                warn!("failed to initialize slack event sink: {err:#}");
                None
            }
        };
        let store = match StateStore::new(config.state_path()) {
            Ok(store) => Some(store),
            Err(err) => {
                warn!("failed to initialize slack state store: {err:#}");
                None
            }
        };
        Self { sink, store }
    }

    fn emit(&self, event_type: &str, payload: Value) {
        if let Some(store) = &self.store {
            if let Err(err) = store.record_event(event_type, &payload) {
                warn!("failed to record slack store event {event_type}: {err:#}");
            }
        }
        if let Some(sink) = &self.sink {
            if let Err(err) = sink.emit(event_type, payload) {
                warn!("failed to emit slack log event {event_type}: {err:#}");
            }
        }
    }
}

pub async fn run(config: RuntimeConfig) -> Result<()> {
    let Some(_channel) = config.channels.slack.clone() else {
        mark_component_disabled("slack", "channel not configured");
        info!("slack channel disabled");
        return Ok(());
    };
    let Some(bot_token) = config.slack_bot_token()? else {
        mark_component_disabled("slack", "bot token missing");
        info!("slack bot token missing");
        return Ok(());
    };

    let diagnostics = SlackDiagnostics::new(&config);
    let client = Client::builder()
        .build()
        .context("failed to build slack http client")?;
    let bot_user_id = auth_test(&client, &bot_token).await?;
    mark_component_ok("slack");

    let outgoing_client = client.clone();
    let outgoing_token = bot_token.clone();
    let outgoing_config = config.clone();
    let outgoing_diagnostics = diagnostics.clone();
    tokio::spawn(async move {
        mark_component_ok("slack_outgoing");
        if let Err(err) = outgoing_loop(
            outgoing_config,
            outgoing_client,
            outgoing_token,
            outgoing_diagnostics,
        )
        .await
        {
            mark_component_error("slack_outgoing", err.to_string());
            error!("slack outgoing loop failed: {err:#}");
        }
    });

    match config.slack_app_token()? {
        Some(app_token) => {
            diagnostics.emit(
                "slack_runtime_started",
                json!({ "mode": "socket", "bot_user_id": bot_user_id }),
            );
            listen_socket_mode(
                &config,
                &client,
                &bot_token,
                &app_token,
                &bot_user_id,
                &diagnostics,
            )
            .await
        }
        None => {
            diagnostics.emit(
                "slack_runtime_started",
                json!({ "mode": "polling", "bot_user_id": bot_user_id }),
            );
            info!("slack app token missing; falling back to polling mode");
            polling_loop(&config, &client, &bot_token, &bot_user_id, &diagnostics).await
        }
    }
}

async fn listen_socket_mode(
    config: &RuntimeConfig,
    client: &Client,
    bot_token: &str,
    app_token: &str,
    bot_user_id: &str,
    diagnostics: &SlackDiagnostics,
) -> Result<()> {
    let mut open_url_attempt = 0_u32;
    let mut socket_reconnect_attempt = 0_u32;

    loop {
        let websocket_url = match open_socket_mode(client, app_token).await {
            Ok(url) => {
                open_url_attempt = 0;
                url
            }
            Err(err) => {
                let wait = compute_socket_mode_retry_delay(open_url_attempt);
                mark_component_error("slack", err.to_string());
                diagnostics.emit(
                    "slack_socket_open_failed",
                    json!({
                        "error": err.to_string(),
                        "attempt": open_url_attempt.saturating_add(1),
                        "retry_in_ms": wait.as_millis(),
                    }),
                );
                warn!(
                    "failed to open slack socket mode connection: {err:#}; retrying in {} ms",
                    wait.as_millis()
                );
                open_url_attempt = open_url_attempt.saturating_add(1);
                sleep(wait).await;
                continue;
            }
        };

        let (stream, _) = match connect_async(&websocket_url).await {
            Ok(stream) => {
                socket_reconnect_attempt = 0;
                mark_component_ok("slack");
                diagnostics.emit("slack_socket_connected", json!({ "url": websocket_url }));
                info!("slack socket mode connected");
                stream
            }
            Err(err) => {
                let wait = compute_socket_mode_retry_delay(socket_reconnect_attempt);
                mark_component_error("slack", err.to_string());
                diagnostics.emit(
                    "slack_socket_connect_failed",
                    json!({
                        "error": err.to_string(),
                        "attempt": socket_reconnect_attempt.saturating_add(1),
                        "retry_in_ms": wait.as_millis(),
                    }),
                );
                warn!(
                    "slack websocket connect failed: {err:#}; retrying in {} ms",
                    wait.as_millis()
                );
                socket_reconnect_attempt = socket_reconnect_attempt.saturating_add(1);
                sleep(wait).await;
                continue;
            }
        };

        let (mut write, mut read) = stream.split();
        while let Some(message) = read.next().await {
            match message {
                Ok(WsMessage::Text(text)) => {
                    let payload: Value = match serde_json::from_str(&text) {
                        Ok(payload) => payload,
                        Err(err) => {
                            diagnostics.emit(
                                "slack_socket_payload_invalid",
                                json!({ "error": err.to_string() }),
                            );
                            warn!("failed to parse slack socket payload: {err:#}");
                            continue;
                        }
                    };

                    if let Some(envelope_id) = payload.get("envelope_id").and_then(Value::as_str) {
                        if let Err(err) = write
                            .send(WsMessage::Text(
                                json!({ "envelope_id": envelope_id }).to_string().into(),
                            ))
                            .await
                        {
                            let message = format!("failed to ack slack socket payload: {err}");
                            mark_component_error("slack", message.clone());
                            diagnostics.emit(
                                "slack_socket_ack_failed",
                                json!({ "error": err.to_string(), "envelope_id": envelope_id }),
                            );
                            warn!("{message}");
                            break;
                        }
                    }

                    let payload_type = payload
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if payload_type == "disconnect" {
                        mark_component_error("slack", "socket disconnect event");
                        diagnostics.emit("slack_socket_disconnect", json!({}));
                        warn!("slack socket mode received disconnect event");
                        break;
                    }
                    if payload_type != "events_api" {
                        continue;
                    }

                    if let Err(err) = handle_event_api(
                        config,
                        client,
                        bot_token,
                        bot_user_id,
                        &payload,
                        "socket",
                        diagnostics,
                    )
                    .await
                    {
                        diagnostics.emit(
                            "slack_event_failed",
                            json!({ "source": "socket", "error": err.to_string() }),
                        );
                        warn!("slack event handling failed: {err:#}");
                    }
                }
                Ok(WsMessage::Ping(payload)) => {
                    if let Err(err) = write.send(WsMessage::Pong(payload)).await {
                        let message = format!("failed to respond to slack ping: {err}");
                        mark_component_error("slack", message.clone());
                        diagnostics.emit(
                            "slack_socket_pong_failed",
                            json!({ "error": err.to_string() }),
                        );
                        warn!("{message}");
                        break;
                    }
                }
                Ok(WsMessage::Close(frame)) => {
                    mark_component_error("slack", "socket closed");
                    diagnostics.emit(
                        "slack_socket_closed",
                        json!({ "frame": frame.as_ref().map(|frame| frame.to_string()) }),
                    );
                    warn!("slack websocket closed by server");
                    break;
                }
                Ok(_) => {}
                Err(err) => {
                    let message = format!("failed to read slack socket message: {err}");
                    mark_component_error("slack", message.clone());
                    diagnostics.emit(
                        "slack_socket_read_failed",
                        json!({ "error": err.to_string() }),
                    );
                    warn!("{message}");
                    break;
                }
            }
        }

        let wait = compute_socket_mode_retry_delay(socket_reconnect_attempt);
        diagnostics.emit(
            "slack_socket_reconnecting",
            json!({
                "attempt": socket_reconnect_attempt.saturating_add(1),
                "retry_in_ms": wait.as_millis(),
            }),
        );
        socket_reconnect_attempt = socket_reconnect_attempt.saturating_add(1);
        sleep(wait).await;
    }
}

async fn polling_loop(
    config: &RuntimeConfig,
    client: &Client,
    bot_token: &str,
    bot_user_id: &str,
    diagnostics: &SlackDiagnostics,
) -> Result<()> {
    let mut discovered_channels: Vec<String> = Vec::new();
    let mut last_discovery = Instant::now() - Duration::from_secs(SLACK_DISCOVERY_REFRESH_SECS);
    let mut last_ts_by_channel: HashMap<String, String> = HashMap::new();

    loop {
        if discovered_channels.is_empty()
            || last_discovery.elapsed() >= Duration::from_secs(SLACK_DISCOVERY_REFRESH_SECS)
        {
            match list_accessible_channels(client, bot_token).await {
                Ok(channels) => {
                    if channels != discovered_channels {
                        diagnostics.emit(
                            "slack_poll_channels_refreshed",
                            json!({ "count": channels.len(), "channels": channels }),
                        );
                    }
                    discovered_channels = channels;
                    mark_component_ok("slack");
                }
                Err(err) => {
                    mark_component_error("slack", err.to_string());
                    diagnostics.emit(
                        "slack_poll_channel_discovery_failed",
                        json!({ "error": err.to_string() }),
                    );
                    warn!("slack channel discovery failed: {err:#}");
                }
            }
            last_discovery = Instant::now();
        }

        for channel_id in &discovered_channels {
            let cursor = last_ts_by_channel
                .get(channel_id)
                .cloned()
                .unwrap_or_else(slack_now_ts);

            let history = match fetch_history(client, bot_token, channel_id, &cursor).await {
                Ok(history) => history,
                Err(err) => {
                    diagnostics.emit(
                        "slack_poll_history_failed",
                        json!({ "channel": channel_id, "error": err.to_string() }),
                    );
                    warn!("slack history fetch failed for {}: {err:#}", channel_id);
                    continue;
                }
            };

            let mut latest_seen = cursor.clone();
            if let Some(messages) = history.get("messages").and_then(Value::as_array) {
                for message in messages.iter().rev() {
                    let ts = message
                        .get("ts")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if ts.is_empty() || slack_ts_value(ts) <= slack_ts_value(&cursor) {
                        continue;
                    }
                    if slack_ts_value(ts) > slack_ts_value(&latest_seen) {
                        latest_seen = ts.to_string();
                    }

                    let polled_message = with_channel_id(message, channel_id);
                    if let Err(err) = handle_message_event(
                        config,
                        client,
                        bot_token,
                        bot_user_id,
                        &polled_message,
                        "poll",
                        diagnostics,
                    )
                    .await
                    {
                        diagnostics.emit(
                            "slack_event_failed",
                            json!({
                                "source": "poll",
                                "channel": channel_id,
                                "ts": ts,
                                "error": err.to_string(),
                            }),
                        );
                        warn!("slack polled message handling failed: {err:#}");
                    }
                }
            }

            last_ts_by_channel.insert(channel_id.clone(), latest_seen);
        }

        sleep(Duration::from_secs(SLACK_POLL_INTERVAL_SECS)).await;
    }
}

async fn handle_event_api(
    config: &RuntimeConfig,
    client: &Client,
    bot_token: &str,
    bot_user_id: &str,
    payload: &Value,
    source: &str,
    diagnostics: &SlackDiagnostics,
) -> Result<()> {
    let event = payload
        .get("payload")
        .and_then(|payload| payload.get("event"))
        .ok_or_else(|| anyhow!("missing slack event payload"))?;
    handle_message_event(
        config,
        client,
        bot_token,
        bot_user_id,
        event,
        source,
        diagnostics,
    )
    .await
}

async fn handle_message_event(
    config: &RuntimeConfig,
    client: &Client,
    bot_token: &str,
    bot_user_id: &str,
    event: &Value,
    source: &str,
    diagnostics: &SlackDiagnostics,
) -> Result<()> {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    let subtype = event.get("subtype").and_then(Value::as_str);
    let channel_id = event
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let ts = event
        .get("ts")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let user_id = event
        .get("user")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    diagnostics.emit(
        "slack_event_received",
        json!({
            "source": source,
            "event_type": event_type,
            "subtype": subtype,
            "channel": channel_id,
            "user": user_id,
            "ts": ts,
        }),
    );

    if event_type != "message" && event_type != "app_mention" {
        diagnostics.emit(
            "slack_event_dropped",
            json!({
                "source": source,
                "reason": "unsupported_event_type",
                "event_type": event_type,
                "channel": channel_id,
                "ts": ts,
            }),
        );
        return Ok(());
    }
    if !is_supported_message_subtype(subtype) {
        diagnostics.emit(
            "slack_event_dropped",
            json!({
                "source": source,
                "reason": "unsupported_subtype",
                "event_type": event_type,
                "subtype": subtype,
                "channel": channel_id,
                "ts": ts,
            }),
        );
        return Ok(());
    }
    if event.get("bot_id").is_some() {
        diagnostics.emit(
            "slack_event_dropped",
            json!({
                "source": source,
                "reason": "bot_message",
                "event_type": event_type,
                "channel": channel_id,
                "ts": ts,
            }),
        );
        return Ok(());
    }

    let user_id = event
        .get("user")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("slack event missing user"))?;
    if user_id == bot_user_id {
        diagnostics.emit(
            "slack_event_dropped",
            json!({
                "source": source,
                "reason": "self_message",
                "event_type": event_type,
                "channel": channel_id,
                "user": user_id,
                "ts": ts,
            }),
        );
        return Ok(());
    }

    let channel_id = event
        .get("channel")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("slack event missing channel"))?;
    let ts = event
        .get("ts")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("slack event missing ts"))?;
    let thread_ts = event.get("thread_ts").and_then(Value::as_str);
    let peer_id = match thread_ts {
        Some(thread_ts) if thread_ts != ts => format!("{channel_id}|{thread_ts}"),
        _ => channel_id.to_string(),
    };
    let chat_type = if thread_ts.is_some() && thread_ts != Some(ts) {
        ChatType::Thread
    } else if channel_id.starts_with('D') {
        ChatType::Direct
    } else {
        ChatType::Group
    };
    let raw_text = event
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mentions_bot =
        event_type == "app_mention" || raw_text.contains(&format!("<@{bot_user_id}>"));
    let access = config
        .channels
        .slack
        .as_ref()
        .map(|channel| channel.effective_access())
        .unwrap_or_default();
    let store = diagnostics
        .store
        .as_ref()
        .ok_or_else(|| anyhow!("slack sender access requires state store"))?;
    let access_state = store.is_sender_approved("slack", user_id)?;
    match evaluate_ingress_policy(&access, chat_type, user_id, mentions_bot, access_state) {
        IngressDecision::Allow => {}
        IngressDecision::Drop { reason } => {
            diagnostics.emit(
                "slack_event_dropped",
                json!({
                    "source": source,
                    "reason": reason,
                    "event_type": event_type,
                    "channel": channel_id,
                    "user": user_id,
                    "ts": ts,
                }),
            );
            return Ok(());
        }
        IngressDecision::RequirePairing => {
            let notice_id = format!("slack_pairing_{}_{}", channel_id, ts.replace('.', "_"));

            if pairing::looks_like_pairing_code(raw_text, config.pairing.code_length) {
                let result = store.verify_pairing_code(
                    "slack", user_id, raw_text,
                    config.pairing.max_failed_attempts, config.pairing.lockout_secs,
                )?;
                match result {
                    VerifyResult::Approved => {
                        enqueue_pairing_response(config, "slack", &peer_id, &notice_id,
                            "Pairing approved. You can now send messages.").await?;
                    }
                    VerifyResult::Expired => {
                        let pc = pairing::generate_code(config.pairing.code_length, config.pairing.code_ttl_secs);
                        store.store_pairing_code("slack", user_id, &pc.code, &pc.expires_at.to_rfc3339())?;
                        enqueue_pairing_response(config, "slack", &peer_id, &notice_id,
                            &format!("Code expired. Your new pairing code: {}", pc.code)).await?;
                    }
                    VerifyResult::LockedOut => {
                        enqueue_pairing_response(config, "slack", &peer_id, &notice_id,
                            "Too many failed attempts. Please try again later.").await?;
                    }
                    VerifyResult::InvalidCode => {
                        enqueue_pairing_response(config, "slack", &peer_id, &notice_id,
                            "Invalid pairing code. Please check and try again.").await?;
                    }
                }
                return Ok(());
            }

            let registration = store.register_sender_access_request(
                "slack", user_id, Some(user_id), &peer_id, None,
                Some(raw_text), Some(&format!("slack_{}_{}", channel_id, ts.replace('.', "_"))),
            )?;
            diagnostics.emit(
                "slack_event_dropped",
                json!({
                    "source": source,
                    "reason": "pairing_required",
                    "event_type": event_type,
                    "channel": channel_id,
                    "user": user_id,
                    "ts": ts,
                }),
            );
            if registration == SenderAccessRegistration::PendingCreated {
                let pc = pairing::generate_code(config.pairing.code_length, config.pairing.code_ttl_secs);
                store.store_pairing_code("slack", user_id, &pc.code, &pc.expires_at.to_rfc3339())?;
                enqueue_pairing_response(config, "slack", &peer_id, &notice_id,
                    &format!("This conversation requires pairing. Your code: {}\n\nReply with this code to pair.", pc.code)).await?;
            }
            return Ok(());
        }
    }

    let text = normalize_slack_text(raw_text, bot_user_id);
    let files = download_event_files(config, client, bot_token, event, ts).await?;
    let message_id = format!("slack_{}_{}", channel_id, ts.replace('.', "_"));

    enqueue_message(
        config,
        EnqueueMessage {
            channel: "slack".to_string(),
            sender: user_id.to_string(),
            sender_id: user_id.to_string(),
            message: if text.trim().is_empty() && !files.is_empty() {
                "[attachment]".to_string()
            } else {
                text
            },
            message_id: message_id.clone(),
            timestamp_ms: slack_ts_to_millis(ts),
            chat_type,
            peer_id,
            account_id: None,
            pre_routed_agent: None,
            files,
        },
    )
    .await?;

    diagnostics.emit(
        "slack_event_enqueued",
        json!({
            "source": source,
            "event_type": event_type,
            "subtype": subtype,
            "channel": channel_id,
            "user": user_id,
            "ts": ts,
            "message_id": message_id,
        }),
    );
    mark_component_ok("slack");

    Ok(())
}

async fn enqueue_pairing_response(
    config: &RuntimeConfig,
    channel: &str,
    recipient_id: &str,
    message_id: &str,
    body: &str,
) -> Result<()> {
    enqueue_outgoing_message(
        config,
        channel,
        "system",
        recipient_id,
        body,
        "",
        message_id,
        "system",
        vec![],
        HashMap::new(),
    )
    .await?;
    Ok(())
}

async fn outgoing_loop(
    config: RuntimeConfig,
    client: Client,
    bot_token: String,
    diagnostics: SlackDiagnostics,
) -> Result<()> {
    loop {
        let messages = list_outgoing_messages(&config, "slack").await?;
        for message in messages {
            match send_outgoing(&client, &bot_token, &message).await {
                Ok(()) => {
                    diagnostics.emit(
                        "slack_outgoing_succeeded",
                        json!({
                            "recipient_id": message.recipient_id,
                            "message_id": message.message_id,
                            "path": message.path.display().to_string(),
                        }),
                    );
                    mark_component_ok("slack_outgoing");
                    ack_outgoing_message(&message.path).await?;
                }
                Err(err) => {
                    diagnostics.emit(
                        "slack_outgoing_failed",
                        json!({
                            "recipient_id": message.recipient_id,
                            "message_id": message.message_id,
                            "path": message.path.display().to_string(),
                            "error": err.to_string(),
                        }),
                    );
                    mark_component_error("slack_outgoing", err.to_string());
                    warn!(path = %message.path.display(), "slack send failed: {err:#}");
                }
            }
        }

        sleep(Duration::from_millis(
            config.daemon.poll_interval_ms.max(500),
        ))
        .await;
    }
}

async fn send_outgoing(
    client: &Client,
    bot_token: &str,
    message: &queue::QueuedOutgoingMessage,
) -> Result<()> {
    let (channel, thread_ts) = parse_slack_recipient(&message.recipient_id);
    post_slack_json(
        client,
        bot_token,
        "https://slack.com/api/chat.postMessage",
        json!({
            "channel": channel,
            "text": if message.message.trim().is_empty() { "(attachment)" } else { &message.message },
            "thread_ts": thread_ts,
        }),
    )
    .await?;

    for file in &message.files {
        upload_file_external(client, bot_token, &channel, thread_ts.as_deref(), file).await?;
    }

    Ok(())
}

async fn download_event_files(
    config: &RuntimeConfig,
    client: &Client,
    bot_token: &str,
    event: &Value,
    ts: &str,
) -> Result<Vec<String>> {
    let mut files = vec![];
    let Some(raw_files) = event.get("files").and_then(Value::as_array) else {
        return Ok(files);
    };

    let target_dir = config
        .files_dir()
        .join("slack")
        .join(format!("msg_{}", ts.replace('.', "_")));
    fs::create_dir_all(&target_dir)
        .await
        .with_context(|| format!("failed to create slack files dir: {}", target_dir.display()))?;

    for raw_file in raw_files {
        let file: SlackEventFile = serde_json::from_value(raw_file.clone())
            .context("failed to parse slack file metadata")?;
        let Some(download_url) = file.url_private_download.or(file.url_private) else {
            continue;
        };
        let filename = file.name.unwrap_or_else(|| format!("slack_{}", file.id));
        let bytes = client
            .get(download_url)
            .bearer_auth(bot_token)
            .send()
            .await
            .context("failed to download slack file")?
            .error_for_status()
            .context("slack file download returned error")?
            .bytes()
            .await
            .context("failed to read slack file bytes")?;
        let path = target_dir.join(filename);
        fs::write(&path, bytes)
            .await
            .with_context(|| format!("failed to write slack file: {}", path.display()))?;
        files.push(path.display().to_string());
    }

    Ok(files)
}

async fn open_socket_mode(client: &Client, app_token: &str) -> Result<String> {
    let response = client
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(app_token)
        .send()
        .await
        .context("failed to call apps.connections.open")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read apps.connections.open response body")?;
    if !status.is_success() {
        return Err(anyhow!(
            "slack apps.connections.open failed ({status}): {body}"
        ));
    }
    let response: SlackEnvelope<SlackSocketUrl> =
        serde_json::from_str(&body).context("failed to parse apps.connections.open response")?;
    response.into_result().map(|payload| payload.url)
}

async fn auth_test(client: &Client, bot_token: &str) -> Result<String> {
    let response = client
        .post("https://slack.com/api/auth.test")
        .bearer_auth(bot_token)
        .send()
        .await
        .context("failed to call auth.test")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read auth.test response body")?;
    if !status.is_success() {
        return Err(anyhow!("slack auth.test failed ({status}): {body}"));
    }
    let response: SlackEnvelope<SlackAuthTest> =
        serde_json::from_str(&body).context("failed to parse auth.test response")?;
    response.into_result().map(|payload| payload.user_id)
}

async fn list_accessible_channels(client: &Client, bot_token: &str) -> Result<Vec<String>> {
    let mut channels = vec![];
    let mut cursor: Option<String> = None;

    loop {
        let mut request = client
            .get("https://slack.com/api/conversations.list")
            .bearer_auth(bot_token)
            .query(&[
                ("exclude_archived", "true"),
                ("limit", "200"),
                ("types", "public_channel,private_channel,mpim,im"),
            ]);
        if let Some(next) = cursor.as_ref() {
            request = request.query(&[("cursor", next.as_str())]);
        }

        let response = request
            .send()
            .await
            .context("failed to call conversations.list")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read conversations.list response body")?;
        if !status.is_success() {
            return Err(anyhow!(
                "slack conversations.list failed ({status}): {body}"
            ));
        }

        let data: Value =
            serde_json::from_str(&body).context("failed to parse conversations.list response")?;
        if data.get("ok") == Some(&Value::Bool(false)) {
            let err = data
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Err(anyhow!("slack conversations.list failed: {err}"));
        }

        channels.extend(extract_channel_ids(&data));
        cursor = data
            .get("response_metadata")
            .and_then(|meta| meta.get("next_cursor"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|cursor| !cursor.is_empty())
            .map(ToOwned::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    channels.sort();
    channels.dedup();
    Ok(channels)
}

async fn fetch_history(
    client: &Client,
    bot_token: &str,
    channel_id: &str,
    oldest: &str,
) -> Result<Value> {
    let response = client
        .get("https://slack.com/api/conversations.history")
        .bearer_auth(bot_token)
        .query(&[
            ("channel", channel_id),
            ("limit", "10"),
            ("oldest", oldest),
            ("inclusive", "false"),
        ])
        .send()
        .await
        .context("failed to call conversations.history")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read conversations.history response body")?;
    if !status.is_success() {
        return Err(anyhow!(
            "slack conversations.history failed for {channel_id} ({status}): {body}"
        ));
    }

    let data: Value =
        serde_json::from_str(&body).context("failed to parse conversations.history response")?;
    if data.get("ok") == Some(&Value::Bool(false)) {
        let err = data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(anyhow!(
            "slack conversations.history failed for {channel_id}: {err}"
        ));
    }

    Ok(data)
}

async fn upload_file_external(
    client: &Client,
    bot_token: &str,
    channel: &str,
    thread_ts: Option<&str>,
    file_path: &str,
) -> Result<()> {
    let path = PathBuf::from(file_path);
    if !path.exists() {
        return Err(anyhow!("slack attachment missing: {}", path.display()));
    }

    let bytes = fs::read(&path)
        .await
        .with_context(|| format!("failed to read attachment: {}", path.display()))?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid attachment filename: {}", path.display()))?
        .to_string();

    let upload: SlackEnvelope<SlackUploadUrl> = client
        .post("https://slack.com/api/files.getUploadURLExternal")
        .bearer_auth(bot_token)
        .form(&[
            ("filename", filename.clone()),
            ("length", bytes.len().to_string()),
        ])
        .send()
        .await
        .context("failed to call files.getUploadURLExternal")?
        .json()
        .await
        .context("failed to parse files.getUploadURLExternal response")?;
    let upload = upload.into_result()?;

    client
        .post(upload.upload_url)
        .body(bytes)
        .send()
        .await
        .context("failed to upload file bytes to slack")?
        .error_for_status()
        .context("slack upload URL returned error")?;

    let files_json = serde_json::to_string(&vec![json!({
        "id": upload.file_id,
        "title": filename,
    })])?;

    let mut form_fields = vec![
        ("files".to_string(), files_json),
        ("channel_id".to_string(), channel.to_string()),
    ];
    if let Some(thread_ts) = thread_ts {
        form_fields.push(("thread_ts".to_string(), thread_ts.to_string()));
    }

    let response: SlackEnvelope<Value> = client
        .post("https://slack.com/api/files.completeUploadExternal")
        .bearer_auth(bot_token)
        .form(&form_fields)
        .send()
        .await
        .context("failed to call files.completeUploadExternal")?
        .json()
        .await
        .context("failed to parse files.completeUploadExternal response")?;
    response.into_result().map(|_| ())
}

async fn post_slack_json(client: &Client, bot_token: &str, url: &str, body: Value) -> Result<()> {
    let response: SlackEnvelope<Value> = client
        .post(url)
        .bearer_auth(bot_token)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("failed to call slack endpoint: {url}"))?
        .json()
        .await
        .with_context(|| format!("failed to parse slack response: {url}"))?;
    response.into_result().map(|_| ())
}

fn normalize_slack_text(text: &str, bot_user_id: &str) -> String {
    text.trim()
        .trim_start_matches(&format!("<@{bot_user_id}>"))
        .trim()
        .to_string()
}

fn parse_slack_recipient(raw: &str) -> (String, Option<String>) {
    if let Some((channel, thread_ts)) = raw.split_once('|') {
        (channel.to_string(), Some(thread_ts.to_string()))
    } else {
        (raw.to_string(), None)
    }
}

fn extract_channel_ids(payload: &Value) -> Vec<String> {
    payload
        .get("channels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|channel| {
            channel
                .get("is_im")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || channel
                    .get("is_mpim")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                || channel
                    .get("is_member")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
        .filter_map(|channel| channel.get("id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

fn with_channel_id(message: &Value, channel_id: &str) -> Value {
    let mut message = message.clone();
    if let Some(object) = message.as_object_mut() {
        object
            .entry("channel".to_string())
            .or_insert_with(|| Value::String(channel_id.to_string()));
    }
    message
}

fn is_supported_message_subtype(subtype: Option<&str>) -> bool {
    matches!(subtype, None | Some("file_share" | "thread_broadcast"))
}

fn slack_ts_to_millis(ts: &str) -> i64 {
    let value = ts.parse::<f64>().unwrap_or_default();
    (value * 1000.0) as i64
}

fn slack_ts_value(ts: &str) -> f64 {
    ts.parse::<f64>().unwrap_or_default()
}

fn slack_now_ts() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:06}", now.as_secs(), now.subsec_micros())
}

fn compute_socket_mode_retry_delay(attempt: u32) -> Duration {
    let exp = 1_u64 << attempt.min(5);
    let base =
        (SLACK_SOCKET_INITIAL_BACKOFF_MS.saturating_mul(exp)).min(SLACK_SOCKET_MAX_BACKOFF_MS);
    let jitter = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis() as u64)
        % SLACK_SOCKET_MAX_JITTER_MS;
    Duration::from_millis(base.saturating_add(jitter))
}

#[derive(Debug, Deserialize)]
struct SlackEnvelope<T> {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(flatten)]
    payload: T,
}

impl<T> SlackEnvelope<T> {
    fn into_result(self) -> Result<T> {
        if self.ok {
            Ok(self.payload)
        } else {
            Err(anyhow!(
                "slack api error: {}",
                self.error.unwrap_or_else(|| "unknown error".to_string())
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
struct SlackSocketUrl {
    url: String,
}

#[derive(Debug, Deserialize)]
struct SlackAuthTest {
    user_id: String,
}

#[derive(Debug, Deserialize)]
struct SlackUploadUrl {
    upload_url: String,
    file_id: String,
}

#[derive(Debug, Deserialize)]
struct SlackEventFile {
    id: String,
    name: Option<String>,
    url_private: Option<String>,
    url_private_download: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_channel_ids_keeps_only_joined_or_direct_channels() {
        let payload = json!({
            "channels": [
                { "id": "C_JOINED", "is_member": true },
                { "id": "C_SKIPPED", "is_member": false },
                { "id": "D_DIRECT", "is_im": true },
                { "id": "G_GROUP_DM", "is_mpim": true }
            ]
        });

        assert_eq!(
            extract_channel_ids(&payload),
            vec![
                "C_JOINED".to_string(),
                "D_DIRECT".to_string(),
                "G_GROUP_DM".to_string()
            ]
        );
    }

    #[test]
    fn with_channel_id_sets_missing_channel_for_polled_messages() {
        let message = json!({
            "type": "message",
            "user": "U123",
            "text": "hi",
            "ts": "123.456"
        });

        let enriched = with_channel_id(&message, "D123");
        assert_eq!(
            enriched.get("channel").and_then(Value::as_str),
            Some("D123")
        );
    }
}
