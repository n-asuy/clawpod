use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use config::{evaluate_ingress_policy, DiscordConfig, IngressDecision, RuntimeConfig};
use domain::ChatType;
use observer::{mark_component_disabled, mark_component_error, mark_component_ok};
use queue::{
    ack_outgoing_message, enqueue_message, enqueue_outgoing_message, list_outgoing_messages,
    EnqueueMessage,
};
use serenity::all::{
    ChannelId, ChannelType, CreateAllowedMentions, CreateAttachment, CreateMessage, GatewayIntents,
    Http, Message, MessageId, Ready,
};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use store::{SenderAccessRegistration, StateStore, VerifyResult};
use tokio::fs;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

pub async fn run(config: RuntimeConfig) -> Result<()> {
    let Some(channel) = config.channels.discord.clone() else {
        mark_component_disabled("discord", "channel not configured");
        info!("discord channel disabled");
        return Ok(());
    };
    let Some(token) = config.discord_bot_token()? else {
        mark_component_disabled("discord", "bot token missing");
        info!("discord bot token missing");
        return Ok(());
    };

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;
    let store =
        StateStore::new(config.state_path()).context("failed to initialize discord state store")?;
    let mut client = Client::builder(token, intents)
        .event_handler(Handler {
            config: config.clone(),
            channel,
            store,
        })
        .await
        .context("failed to build discord client")?;
    let http = client.http.clone();
    let outgoing_config = config.clone();
    tokio::spawn(async move {
        mark_component_ok("discord_outgoing");
        if let Err(err) = outgoing_loop(outgoing_config, http).await {
            mark_component_error("discord_outgoing", err.to_string());
            error!("discord outgoing loop failed: {err:#}");
        }
    });

    let result = client.start().await.context("discord client stopped");
    if let Err(err) = &result {
        mark_component_error("discord", err.to_string());
    }
    result?;
    Ok(())
}

struct Handler {
    config: RuntimeConfig,
    channel: DiscordConfig,
    store: StateStore,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        mark_component_ok("discord");
        info!(user = %ready.user.name, "discord connected");
    }

    async fn message(&self, ctx: Context, message: Message) {
        if let Err(err) =
            handle_message(&ctx, &self.config, &self.channel, &self.store, message).await
        {
            error!("discord inbound handling failed: {err:#}");
        }
    }
}

async fn handle_message(
    ctx: &Context,
    config: &RuntimeConfig,
    channel: &DiscordConfig,
    store: &StateStore,
    message: Message,
) -> Result<()> {
    if message.author.bot {
        return Ok(());
    }

    if let Some(required_guild_id) = channel.guild_id.as_deref() {
        if message.guild_id.map(|id| id.to_string()).as_deref() != Some(required_guild_id) {
            return Ok(());
        }
    }

    let chat_type = if message.guild_id.is_none() {
        ChatType::Direct
    } else if is_thread_channel(ctx, message.channel_id).await? {
        ChatType::Thread
    } else {
        ChatType::Group
    };

    let mentions_bot = message.guild_id.is_some() && message.mentions_me(ctx).await?;
    let access = channel.effective_access();
    let sender_id = message.author.id.to_string();
    let access_state = store.is_sender_approved("discord", &sender_id)?;
    match evaluate_ingress_policy(&access, chat_type, &sender_id, mentions_bot, access_state) {
        IngressDecision::Allow => {}
        IngressDecision::Drop { .. } => return Ok(()),
        IngressDecision::RequirePairing => {
            let recipient = message.channel_id.to_string();
            let notice_id = format!("discord_pairing_{}", message.id);

            if pairing::looks_like_pairing_code(&message.content, config.pairing.code_length) {
                let result = store.verify_pairing_code(
                    "discord", &sender_id, &message.content,
                    config.pairing.max_failed_attempts, config.pairing.lockout_secs,
                )?;
                match result {
                    VerifyResult::Approved => {
                        enqueue_pairing_response(config, "discord", &recipient, &notice_id,
                            "Pairing approved. You can now send messages.").await?;
                    }
                    VerifyResult::Expired => {
                        let pc = pairing::generate_code(config.pairing.code_length, config.pairing.code_ttl_secs);
                        store.store_pairing_code("discord", &sender_id, &pc.code, &pc.expires_at.to_rfc3339())?;
                        enqueue_pairing_response(config, "discord", &recipient, &notice_id,
                            &format!("Code expired. Your new pairing code: {}", pc.code)).await?;
                    }
                    VerifyResult::LockedOut => {
                        enqueue_pairing_response(config, "discord", &recipient, &notice_id,
                            "Too many failed attempts. Please try again later.").await?;
                    }
                    VerifyResult::InvalidCode => {
                        enqueue_pairing_response(config, "discord", &recipient, &notice_id,
                            "Invalid pairing code. Please check and try again.").await?;
                    }
                }
                return Ok(());
            }

            let registration = store.register_sender_access_request(
                "discord", &sender_id, Some(&message.author.name), &recipient,
                message.guild_id.map(|id| id.to_string()).as_deref(),
                Some(&message.content), Some(&format!("discord_{}", message.id)),
            )?;
            if registration == SenderAccessRegistration::PendingCreated {
                let pc = pairing::generate_code(config.pairing.code_length, config.pairing.code_ttl_secs);
                store.store_pairing_code("discord", &sender_id, &pc.code, &pc.expires_at.to_rfc3339())?;
                enqueue_pairing_response(config, "discord", &recipient, &notice_id,
                    &format!("This conversation requires pairing. Your code: {}\n\nReply with this code to pair.", pc.code)).await?;
            }
            return Ok(());
        }
    }

    let bot_user_id = ctx.cache.current_user().id;
    let text = normalize_discord_text(&message.content, bot_user_id.get());
    let files = download_attachments(config, &message).await?;
    if text.trim().is_empty() && files.is_empty() {
        return Ok(());
    }
    let text = if text.trim().is_empty() {
        "[attachment]".to_string()
    } else {
        text
    };

    enqueue_message(
        config,
        EnqueueMessage {
            channel: "discord".to_string(),
            sender: message.author.name.clone(),
            sender_id,
            message: text,
            message_id: format!("discord_{}", message.id),
            timestamp_ms: message.timestamp.unix_timestamp() * 1000,
            chat_type,
            peer_id: message.channel_id.to_string(),
            account_id: message.guild_id.map(|id| id.to_string()),
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

async fn outgoing_loop(config: RuntimeConfig, http: Arc<Http>) -> Result<()> {
    loop {
        let messages = list_outgoing_messages(&config, "discord").await?;
        for message in messages {
            match send_outgoing(&http, &message).await {
                Ok(()) => ack_outgoing_message(&message.path).await?,
                Err(err) => {
                    warn!(path = %message.path.display(), "discord send failed: {err:#}");
                }
            }
        }

        sleep(Duration::from_millis(
            config.daemon.poll_interval_ms.max(300),
        ))
        .await;
    }
}

async fn send_outgoing(http: &Http, message: &queue::QueuedOutgoingMessage) -> Result<()> {
    let channel_id = ChannelId::new(
        message
            .recipient_id
            .parse::<u64>()
            .context("invalid discord channel id")?,
    );

    let mut builder = if message.message.trim().is_empty() {
        CreateMessage::new()
    } else {
        CreateMessage::new().content(message.message.clone())
    };
    if let Some(reply_to) = parse_discord_message_id(&message.message_id) {
        builder = builder
            .reference_message((channel_id, MessageId::new(reply_to)))
            .allowed_mentions(CreateAllowedMentions::new().replied_user(false));
    }

    let mut attachments = vec![];
    for file in &message.files {
        let path = PathBuf::from(file);
        if !path.exists() {
            warn!(path = %path.display(), "discord attachment missing");
            continue;
        }
        attachments.push(
            CreateAttachment::path(&path)
                .await
                .with_context(|| format!("failed to read attachment: {}", path.display()))?,
        );
    }

    if !attachments.is_empty() {
        builder = builder.add_files(attachments);
    }

    channel_id
        .send_message(http, builder)
        .await
        .context("failed to send discord message")?;
    Ok(())
}

async fn download_attachments(config: &RuntimeConfig, message: &Message) -> Result<Vec<String>> {
    let mut files = vec![];
    if message.attachments.is_empty() {
        return Ok(files);
    }

    let target_dir = config
        .files_dir()
        .join("discord")
        .join(format!("msg_{}", message.id));
    fs::create_dir_all(&target_dir).await.with_context(|| {
        format!(
            "failed to create discord files dir: {}",
            target_dir.display()
        )
    })?;

    for attachment in &message.attachments {
        let bytes = attachment.download().await.with_context(|| {
            format!("failed to download discord attachment: {}", attachment.url)
        })?;
        let path = target_dir.join(&attachment.filename);
        fs::write(&path, bytes)
            .await
            .with_context(|| format!("failed to write attachment: {}", path.display()))?;
        files.push(path.display().to_string());
    }

    Ok(files)
}

async fn is_thread_channel(ctx: &Context, channel_id: ChannelId) -> Result<bool> {
    let channel = channel_id
        .to_channel(ctx)
        .await
        .context("failed to fetch discord channel")?;
    let Some(channel) = channel.guild() else {
        return Ok(false);
    };
    Ok(matches!(
        channel.kind,
        ChannelType::PublicThread | ChannelType::PrivateThread | ChannelType::NewsThread
    ))
}

fn normalize_discord_text(text: &str, bot_user_id: u64) -> String {
    let mention = format!("<@{bot_user_id}>");
    let nickname_mention = format!("<@!{bot_user_id}>");
    text.replace(&mention, "")
        .replace(&nickname_mention, "")
        .trim()
        .to_string()
}

fn parse_discord_message_id(raw: &str) -> Option<u64> {
    raw.strip_prefix("discord_")?.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::{normalize_discord_text, parse_discord_message_id};

    #[test]
    fn strips_plain_bot_mention() {
        assert_eq!(
            normalize_discord_text("<@12345> summarize this", 12345),
            "summarize this"
        );
    }

    #[test]
    fn strips_nickname_bot_mention() {
        assert_eq!(
            normalize_discord_text("<@!12345> summarize this", 12345),
            "summarize this"
        );
    }

    #[test]
    fn parses_discord_message_id() {
        assert_eq!(
            parse_discord_message_id("discord_987654321"),
            Some(987654321)
        );
        assert_eq!(parse_discord_message_id("telegram_987654321"), None);
    }
}
