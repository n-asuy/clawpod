use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use config::{evaluate_ingress_policy, IngressDecision, RuntimeConfig};
use domain::ChatType;
use observer::{mark_component_disabled, mark_component_ok};
use queue::{
    ack_outgoing_message, enqueue_message, enqueue_outgoing_message, list_outgoing_messages,
    EnqueueMessage,
};
use store::{SenderAccessRegistration, StateStore, VerifyResult};
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{
    ChatId, InputFile, MediaKind, MessageId, MessageKind, ReplyParameters, User,
};
use tokio::fs;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

pub async fn run(config: RuntimeConfig) -> Result<()> {
    let Some(_channel) = config.channels.telegram.as_ref() else {
        mark_component_disabled("telegram", "channel not configured");
        info!("telegram channel disabled");
        return Ok(());
    };
    let Some(token) = config.telegram_bot_token()? else {
        mark_component_disabled("telegram", "bot token missing");
        info!("telegram channel disabled");
        return Ok(());
    };

    let bot = Bot::new(token);
    let store = StateStore::new(config.state_path())
        .context("failed to initialize telegram state store")?;
    let me = bot
        .get_me()
        .await
        .context("failed to fetch telegram bot profile")?;
    let bot_username = me.user.username;
    mark_component_ok("telegram");
    let outgoing_bot = bot.clone();
    let outgoing_config = config.clone();
    tokio::spawn(async move {
        mark_component_ok("telegram_outgoing");
        outgoing_loop(outgoing_config, outgoing_bot).await;
    });

    teloxide::repl(bot, move |bot: Bot, message: Message| {
        let config = config.clone();
        let store = store.clone();
        let bot_username = bot_username.clone();
        async move {
            if let Err(err) =
                handle_message(&bot, &config, &store, bot_username.as_deref(), message).await
            {
                error!("telegram inbound handling failed: {err:#}");
            }
            respond(())
        }
    })
    .await;

    Ok(())
}

async fn handle_message(
    bot: &Bot,
    config: &RuntimeConfig,
    store: &StateStore,
    bot_username: Option<&str>,
    message: Message,
) -> Result<()> {
    let Some(from) = message.from.as_ref() else {
        return Ok(());
    };
    if from.is_bot {
        return Ok(());
    }

    let text = message
        .text()
        .or_else(|| message.caption())
        .map(str::to_string)
        .unwrap_or_else(|| "[attachment]".to_string());
    let files = download_attachments(bot, config, &message).await?;
    if text.trim().is_empty() && files.is_empty() {
        return Ok(());
    }

    let chat_type = if message.thread_id.is_some() {
        ChatType::Thread
    } else if message.chat.is_private() {
        ChatType::Direct
    } else {
        ChatType::Group
    };

    let sender_id = from.id.0.to_string();
    let mentions_bot = bot_username
        .map(|username| text.contains(&format!("@{username}")))
        .unwrap_or(false);
    let access = config
        .channels
        .telegram
        .as_ref()
        .map(|channel| channel.effective_access())
        .unwrap_or_default();
    let access_state = store.is_sender_approved("telegram", &sender_id)?;
    match evaluate_ingress_policy(&access, chat_type, &sender_id, mentions_bot, access_state) {
        IngressDecision::Allow => {}
        IngressDecision::Drop { .. } => return Ok(()),
        IngressDecision::RequirePairing => {
            let recipient = message.chat.id.0.to_string();
            let notice_id = format!("telegram_pairing_{}", message.id.0);

            // If message looks like a pairing code, try to verify it
            if pairing::looks_like_pairing_code(&text, config.pairing.code_length) {
                let result = store.verify_pairing_code(
                    "telegram",
                    &sender_id,
                    &text,
                    config.pairing.max_failed_attempts,
                    config.pairing.lockout_secs,
                )?;
                match result {
                    VerifyResult::Approved => {
                        enqueue_pairing_response(
                            config,
                            "telegram",
                            &recipient,
                            &notice_id,
                            "Pairing approved. You can now send messages.",
                        )
                        .await?;
                    }
                    VerifyResult::Expired => {
                        let pc = pairing::generate_code(
                            config.pairing.code_length,
                            config.pairing.code_ttl_secs,
                        );
                        store.store_pairing_code(
                            "telegram",
                            &sender_id,
                            &pc.code,
                            &pc.expires_at.to_rfc3339(),
                        )?;
                        enqueue_pairing_response(
                            config,
                            "telegram",
                            &recipient,
                            &notice_id,
                            &format!("Code expired. Your new pairing code: {}", pc.code),
                        )
                        .await?;
                    }
                    VerifyResult::LockedOut => {
                        enqueue_pairing_response(
                            config,
                            "telegram",
                            &recipient,
                            &notice_id,
                            "Too many failed attempts. Please try again later.",
                        )
                        .await?;
                    }
                    VerifyResult::InvalidCode => {
                        enqueue_pairing_response(
                            config,
                            "telegram",
                            &recipient,
                            &notice_id,
                            "Invalid pairing code. Please check and try again.",
                        )
                        .await?;
                    }
                }
                return Ok(());
            }

            // Not a code: register access request and send code
            let registration = store.register_sender_access_request(
                "telegram",
                &sender_id,
                Some(&display_name(from)),
                &recipient,
                None,
                Some(&text),
                Some(&format!("telegram_{}", message.id.0)),
            )?;
            if registration == SenderAccessRegistration::PendingCreated {
                let pc = pairing::generate_code(
                    config.pairing.code_length,
                    config.pairing.code_ttl_secs,
                );
                store.store_pairing_code(
                    "telegram",
                    &sender_id,
                    &pc.code,
                    &pc.expires_at.to_rfc3339(),
                )?;
                enqueue_pairing_response(
                    config, "telegram", &recipient, &notice_id,
                    &format!(
                        "This conversation requires pairing. Your code: {}\n\nReply with this code to pair.",
                        pc.code
                    ),
                ).await?;
            }
            return Ok(());
        }
    }

    enqueue_message(
        config,
        EnqueueMessage {
            channel: "telegram".to_string(),
            sender: display_name(from),
            sender_id,
            message: text,
            message_id: format!("telegram_{}", message.id.0),
            timestamp_ms: message.date.timestamp_millis(),
            chat_type,
            peer_id: message.chat.id.0.to_string(),
            account_id: None,
            pre_routed_agent: None,
            from_agent: None,
            files,
            chain_depth: 0,
        },
    )
    .await?;

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

async fn outgoing_loop(config: RuntimeConfig, bot: Bot) {
    loop {
        let messages = match list_outgoing_messages(&config, "telegram").await {
            Ok(msgs) => msgs,
            Err(err) => {
                error!("telegram outgoing scan failed: {err:#}");
                sleep(Duration::from_millis(config.daemon.poll_interval_ms.max(300))).await;
                continue;
            }
        };
        for message in messages {
            match send_outgoing(&bot, &message).await {
                Ok(()) => {
                    if let Err(err) = ack_outgoing_message(&message.path).await {
                        error!(path = %message.path.display(), "telegram ack failed: {err:#}");
                    }
                }
                Err(err) => {
                    warn!(
                        path = %message.path.display(),
                        "telegram send failed: {err:#}"
                    );
                }
            }
        }

        sleep(Duration::from_millis(config.daemon.poll_interval_ms.max(300))).await;
    }
}

async fn send_outgoing(bot: &Bot, message: &queue::QueuedOutgoingMessage) -> Result<()> {
    let chat_id = ChatId(
        message
            .recipient_id
            .parse::<i64>()
            .context("invalid telegram chat id")?,
    );
    let reply_to = parse_telegram_message_id(&message.message_id);

    if !message.message.trim().is_empty() {
        let mut request = bot.send_message(chat_id, message.message.clone());
        if let Some(reply_to) = reply_to {
            request = request.reply_parameters(ReplyParameters::new(MessageId(reply_to)));
        }
        request.await?;
    }

    for file in &message.files {
        let path = PathBuf::from(file);
        if !path.exists() {
            warn!(path = %path.display(), "telegram attachment missing");
            continue;
        }

        bot.send_document(chat_id, InputFile::file(path)).await?;
    }

    Ok(())
}

async fn download_attachments(
    bot: &Bot,
    config: &RuntimeConfig,
    message: &Message,
) -> Result<Vec<String>> {
    let mut files = vec![];
    let target_dir = config
        .files_dir()
        .join("telegram")
        .join(format!("msg_{}", message.id.0));
    fs::create_dir_all(&target_dir).await.with_context(|| {
        format!(
            "failed to create telegram files dir: {}",
            target_dir.display()
        )
    })?;

    if let Some(document) = message.document() {
        let path = target_dir.join(
            document
                .file_name
                .clone()
                .unwrap_or_else(|| format!("document_{}", message.id.0)),
        );
        download_file(bot, document.file.id.clone(), &path).await?;
        files.push(path.display().to_string());
    }

    if let Some(photos) = message.photo() {
        if let Some(photo) = photos.last() {
            let path = target_dir.join(format!("photo_{}.jpg", photo.file.unique_id));
            download_file(bot, photo.file.id.clone(), &path).await?;
            files.push(path.display().to_string());
        }
    }

    if let MessageKind::Common(common) = &message.kind {
        match &common.media_kind {
            MediaKind::Audio(audio) => {
                let path = target_dir.join(
                    audio
                        .audio
                        .file_name
                        .clone()
                        .unwrap_or_else(|| format!("audio_{}", message.id.0)),
                );
                download_file(bot, audio.audio.file.id.clone(), &path).await?;
                files.push(path.display().to_string());
            }
            MediaKind::Voice(voice) => {
                let path = target_dir.join(format!("voice_{}.ogg", message.id.0));
                download_file(bot, voice.voice.file.id.clone(), &path).await?;
                files.push(path.display().to_string());
            }
            _ => {}
        }
    }

    Ok(files)
}

async fn download_file(bot: &Bot, file_id: String, path: &Path) -> Result<()> {
    let file = bot
        .get_file(file_id)
        .await
        .with_context(|| format!("failed to fetch telegram file metadata: {}", path.display()))?;
    let mut destination = fs::File::create(path)
        .await
        .with_context(|| format!("failed to create file: {}", path.display()))?;
    bot.download_file(&file.path, &mut destination)
        .await
        .with_context(|| format!("failed to download telegram file: {}", path.display()))?;
    Ok(())
}

fn display_name(user: &User) -> String {
    match &user.last_name {
        Some(last_name) => format!("{} {}", user.first_name, last_name),
        None => user.first_name.clone(),
    }
}

fn parse_telegram_message_id(raw: &str) -> Option<i32> {
    raw.strip_prefix("telegram_")?.parse::<i32>().ok()
}
