use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    Http, Message, MessageId, ReactionType, Ready,
};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use store::{SenderAccessRegistration, StateStore, VerifyResult};
use tokio::fs;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

const ACK_REACTIONS: &[&str] = &["⚡", "🦀", "🙌", "💪", "👌", "👀", "👣"];
const DISCORD_MAX_CONTENT_LEN: usize = 2000;

fn pick_ack_reaction(seed: u64) -> &'static str {
    ACK_REACTIONS[seed as usize % ACK_REACTIONS.len()]
}

struct DiscordLiveState {
    typing_handles: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl DiscordLiveState {
    fn new() -> Self {
        Self {
            typing_handles: Mutex::new(HashMap::new()),
        }
    }

    fn start_typing(&self, channel_id: ChannelId, http: Arc<Http>) {
        let key = channel_id.to_string();
        let mut handles = self.typing_handles.lock().unwrap();
        if let Some(old) = handles.remove(&key) {
            old.abort();
        }
        let handle = tokio::spawn(async move {
            loop {
                let _ = channel_id.broadcast_typing(&*http).await;
                tokio::time::sleep(Duration::from_secs(8)).await;
            }
        });
        handles.insert(key, handle);
    }

    fn stop_typing(&self, channel_key: &str) {
        let mut handles = self.typing_handles.lock().unwrap();
        if let Some(handle) = handles.remove(channel_key) {
            handle.abort();
        }
    }
}

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
    let live = Arc::new(DiscordLiveState::new());
    let mut client = Client::builder(token, intents)
        .event_handler(Handler {
            config: config.clone(),
            channel,
            store,
            live: Arc::clone(&live),
        })
        .await
        .context("failed to build discord client")?;
    let http = client.http.clone();
    let outgoing_config = config.clone();
    let outgoing_live = Arc::clone(&live);
    tokio::spawn(async move {
        mark_component_ok("discord_outgoing");
        outgoing_loop(outgoing_config, http, outgoing_live).await;
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
    live: Arc<DiscordLiveState>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        mark_component_ok("discord");
        info!(user = %ready.user.name, "discord connected");
    }

    async fn message(&self, ctx: Context, message: Message) {
        if let Err(err) = handle_message(
            &ctx,
            &self.config,
            &self.channel,
            &self.store,
            &self.live,
            message,
        )
        .await
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
    live: &DiscordLiveState,
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
    let channel_id_str = message.channel_id.to_string();
    match evaluate_ingress_policy(
        &access,
        chat_type,
        &sender_id,
        mentions_bot,
        access_state,
        Some(&channel_id_str),
    ) {
        IngressDecision::Allow => {}
        IngressDecision::Drop { .. } => return Ok(()),
        IngressDecision::RequirePairing => {
            let recipient = message.channel_id.to_string();
            let notice_id = format!("discord_pairing_{}", message.id);

            if pairing::looks_like_pairing_code(&message.content, config.pairing.code_length) {
                let result = store.verify_pairing_code(
                    "discord",
                    &sender_id,
                    &message.content,
                    config.pairing.max_failed_attempts,
                    config.pairing.lockout_secs,
                )?;
                match result {
                    VerifyResult::Approved => {
                        enqueue_pairing_response(
                            config,
                            "discord",
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
                            "discord",
                            &sender_id,
                            &pc.code,
                            &pc.expires_at.to_rfc3339(),
                        )?;
                        enqueue_pairing_response(
                            config,
                            "discord",
                            &recipient,
                            &notice_id,
                            &format!("Code expired. Your new pairing code: {}", pc.code),
                        )
                        .await?;
                    }
                    VerifyResult::LockedOut => {
                        enqueue_pairing_response(
                            config,
                            "discord",
                            &recipient,
                            &notice_id,
                            "Too many failed attempts. Please try again later.",
                        )
                        .await?;
                    }
                    VerifyResult::InvalidCode => {
                        enqueue_pairing_response(
                            config,
                            "discord",
                            &recipient,
                            &notice_id,
                            "Invalid pairing code. Please check and try again.",
                        )
                        .await?;
                    }
                }
                return Ok(());
            }

            let registration = store.register_sender_access_request(
                "discord",
                &sender_id,
                Some(&message.author.name),
                &recipient,
                message.guild_id.map(|id| id.to_string()).as_deref(),
                Some(&message.content),
                Some(&format!("discord_{}", message.id)),
            )?;
            if registration == SenderAccessRegistration::PendingCreated {
                let pc = pairing::generate_code(
                    config.pairing.code_length,
                    config.pairing.code_ttl_secs,
                );
                store.store_pairing_code(
                    "discord",
                    &sender_id,
                    &pc.code,
                    &pc.expires_at.to_rfc3339(),
                )?;
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

    let channel_id = message.channel_id;
    let msg_id = message.id;

    enqueue_message(
        config,
        EnqueueMessage {
            channel: "discord".to_string(),
            sender: message.author.name.clone(),
            sender_id,
            message: text,
            message_id: format!("discord_{}", msg_id),
            timestamp_ms: message.timestamp.unix_timestamp() * 1000,
            chat_type,
            peer_id: channel_id.to_string(),
            account_id: message.guild_id.map(|id| id.to_string()),
            pre_routed_agent: None,
            from_agent: None,
            files,
            chain_depth: 0,
        },
    )
    .await?;

    // Start typing indicator (loops every 8s until stopped by outgoing send)
    live.start_typing(channel_id, ctx.http.clone());

    // Add random ACK reaction to acknowledge receipt
    let emoji_str = pick_ack_reaction(msg_id.get());
    let emoji = ReactionType::Unicode(emoji_str.to_string());
    let http = ctx.http.clone();
    tokio::spawn(async move {
        let _ = http.create_reaction(channel_id, msg_id, &emoji).await;
    });

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

async fn outgoing_loop(config: RuntimeConfig, http: Arc<Http>, live: Arc<DiscordLiveState>) {
    loop {
        let messages = match list_outgoing_messages(&config, "discord").await {
            Ok(msgs) => msgs,
            Err(err) => {
                error!("discord outgoing scan failed: {err:#}");
                sleep(Duration::from_millis(
                    config.daemon.poll_interval_ms.max(300),
                ))
                .await;
                continue;
            }
        };
        for message in messages {
            match send_outgoing(&http, &message).await {
                Ok(()) => {
                    if let Err(err) = ack_outgoing_message(&message.path).await {
                        error!(path = %message.path.display(), "discord ack failed: {err:#}");
                    }
                    live.stop_typing(&message.recipient_id);
                }
                Err(err) => {
                    let err_str = format!("{err:#}");
                    if is_permanent_send_error(&err_str) {
                        warn!(path = %message.path.display(), "discord send permanently failed, dropping: {err_str}");
                        if let Err(ack_err) = ack_outgoing_message(&message.path).await {
                            error!(path = %message.path.display(), "discord ack failed: {ack_err:#}");
                        }
                    } else {
                        warn!(path = %message.path.display(), "discord send failed: {err_str}");
                    }
                }
            }
        }

        sleep(Duration::from_millis(
            config.daemon.poll_interval_ms.max(300),
        ))
        .await;
    }
}

fn is_permanent_send_error(err: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "Message too large",
        "Unknown Channel",
        "Missing Access",
        "Missing Permissions",
    ];
    PATTERNS.iter().any(|p| err.contains(p))
}

async fn send_outgoing(http: &Http, message: &queue::QueuedOutgoingMessage) -> Result<()> {
    let channel_id = ChannelId::new(
        message
            .recipient_id
            .parse::<u64>()
            .context("invalid discord channel id")?,
    );

    let chunks = if message.message.trim().is_empty() {
        vec![String::new()]
    } else {
        split_message_chunks(&message.message, DISCORD_MAX_CONTENT_LEN)
    };

    for (i, chunk) in chunks.iter().enumerate() {
        let mut builder = if chunk.is_empty() {
            CreateMessage::new()
        } else {
            CreateMessage::new().content(chunk.clone())
        };

        if i == 0 {
            if let Some(reply_to) = parse_discord_message_id(&message.message_id) {
                builder = builder
                    .reference_message((channel_id, MessageId::new(reply_to)))
                    .allowed_mentions(CreateAllowedMentions::new().replied_user(false));
            }
        }

        if i == 0 {
            let mut attachments = vec![];
            for file in &message.files {
                let path = PathBuf::from(file);
                if !path.exists() {
                    warn!(path = %path.display(), "discord attachment missing");
                    continue;
                }
                attachments.push(
                    CreateAttachment::path(&path).await.with_context(|| {
                        format!("failed to read attachment: {}", path.display())
                    })?,
                );
            }
            if !attachments.is_empty() {
                builder = builder.add_files(attachments);
            }
        }

        channel_id
            .send_message(http, builder)
            .await
            .context("failed to send discord message")?;

        // Delay between chunks to avoid Discord rate limits (cf. ZeroClaw)
        if i < chunks.len() - 1 {
            sleep(Duration::from_millis(500)).await;
        }
    }

    Ok(())
}

/// Maximum overhead for closing + reopening a fenced code block across chunks.
const FENCE_OVERHEAD: usize = 30;

/// Split text into chunks that fit within Discord's character limit,
/// preserving fenced code blocks across chunk boundaries.
fn split_message_chunks(text: &str, max_chars: usize) -> Vec<String> {
    if text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }

    // Reserve space for code fence close/reopen markers
    let effective_limit = max_chars.saturating_sub(FENCE_OVERHEAD);
    let raw = split_at_boundaries(text, effective_limit);
    balance_code_fences(&raw)
}

/// Low-level splitter: breaks text at newline > space > hard boundaries.
fn split_at_boundaries(text: &str, max_chars: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let remaining = &text[start..];
        if remaining.chars().count() <= max_chars {
            chunks.push(remaining);
            break;
        }

        let end_byte = remaining
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());

        let search = &remaining[..end_byte];
        let half_byte = remaining
            .char_indices()
            .nth(max_chars / 2)
            .map(|(i, _)| i)
            .unwrap_or(0);

        // Prefer newline, but only if it's in the latter half (cf. ZeroClaw)
        let split_byte = search
            .rfind('\n')
            .filter(|&pos| pos >= half_byte)
            .or_else(|| search.rfind(' '))
            .map(|pos| pos + 1)
            .unwrap_or(end_byte);

        let split_byte = if split_byte == 0 {
            end_byte
        } else {
            split_byte
        };

        chunks.push(&remaining[..split_byte]);
        start += split_byte;
    }

    chunks
}

/// Post-process chunks to close and reopen fenced code blocks at boundaries.
fn balance_code_fences(chunks: &[&str]) -> Vec<String> {
    if chunks.len() <= 1 {
        return chunks.iter().map(|c| c.to_string()).collect();
    }

    let mut result = Vec::with_capacity(chunks.len());
    let mut open_fence: Option<String> = None;

    for chunk in chunks {
        let mut buf = String::new();

        if let Some(ref fence_line) = open_fence {
            buf.push_str(fence_line);
            buf.push('\n');
        }

        buf.push_str(chunk);

        // Determine fence state at the end of this chunk (including re-opened prefix)
        open_fence = find_open_fence(&buf);

        if open_fence.is_some() {
            if !buf.ends_with('\n') {
                buf.push('\n');
            }
            buf.push_str("```");
        }

        result.push(buf);
    }

    result
}

/// Returns the opening fence line (e.g. "```rust") if a code fence is unclosed
/// at the end of the text.
fn find_open_fence(text: &str) -> Option<String> {
    let mut open: Option<(char, usize, String)> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();
        let first = match trimmed.chars().next() {
            Some(c @ '`') | Some(c @ '~') => c,
            _ => continue,
        };
        let marker_len = trimmed.chars().take_while(|&c| c == first).count();
        if marker_len < 3 {
            continue;
        }

        if let Some((open_char, open_len, _)) = &open {
            // Closing fence: same marker, >= length, no content after markers
            if first == *open_char && marker_len >= *open_len {
                let after_byte = trimmed
                    .char_indices()
                    .nth(marker_len)
                    .map(|(i, _)| i)
                    .unwrap_or(trimmed.len());
                if trimmed[after_byte..].trim().is_empty() {
                    open = None;
                }
            }
        } else {
            open = Some((first, marker_len, line.to_string()));
        }
    }

    open.map(|(_, _, line)| line)
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
    use super::{
        find_open_fence, is_permanent_send_error, normalize_discord_text, parse_discord_message_id,
        split_at_boundaries, split_message_chunks,
    };

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

    // ── split_at_boundaries (low-level) ──────────────────────────

    #[test]
    fn short_message_stays_single_chunk() {
        let text = "hello world";
        let chunks = split_at_boundaries(text, 2000);
        assert_eq!(chunks, vec!["hello world"]);
    }

    #[test]
    fn splits_at_newline_in_latter_half() {
        // Newline in the latter half → split there
        let text = format!("{}\n{}", "a".repeat(15), "b".repeat(10));
        let chunks = split_at_boundaries(&text, 20);
        assert!(chunks[0].ends_with('\n'));
    }

    #[test]
    fn skips_newline_in_first_half() {
        // Newline very early → falls back to space or hard break
        let text = format!("ab\n{}", "c".repeat(25));
        let chunks = split_at_boundaries(&text, 20);
        // Should NOT split at the early newline (pos 2 < half of 20)
        assert!(chunks[0].chars().count() > 3);
    }

    #[test]
    fn splits_at_space_when_no_newline() {
        let text = "aaaa bbbb cccc dddd";
        let chunks = split_at_boundaries(text, 10);
        assert!(chunks.iter().all(|c| c.chars().count() <= 10));
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn hard_breaks_when_no_delimiter() {
        let text = "a".repeat(30);
        let chunks = split_at_boundaries(&text, 10);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 10));
    }

    #[test]
    fn handles_multibyte_characters() {
        let text = "あ".repeat(30);
        let chunks = split_at_boundaries(&text, 10);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 10));
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn preserves_all_content() {
        let original = "Hello world! This is a test. ".repeat(200);
        let chunks = split_at_boundaries(&original, 2000);
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, original);
    }

    // ── split_message_chunks (with fence balancing) ──────────────

    #[test]
    fn short_message_no_split() {
        let chunks = split_message_chunks("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn code_fence_closed_and_reopened_across_chunks() {
        let mut text = String::from("```rust\n");
        text.push_str(&"x".repeat(2000));
        text.push_str("\n```\nafter");

        let chunks = split_message_chunks(&text, 2000);
        assert!(chunks.len() >= 2);

        // First chunk should close the fence
        assert!(
            chunks[0].trim_end().ends_with("```"),
            "first chunk must close fence"
        );

        // Second chunk should reopen it
        assert!(
            chunks[1].starts_with("```rust"),
            "second chunk must reopen fence"
        );
    }

    #[test]
    fn closed_fence_not_reopened() {
        let mut text = String::from("```\ncode\n```\n");
        text.push_str(&"x".repeat(2000));

        let chunks = split_message_chunks(&text, 2000);
        assert!(chunks.len() >= 2);

        // No fence open at boundary → second chunk should NOT start with ```
        assert!(
            !chunks[1].starts_with("```"),
            "closed fence must not be reopened"
        );
    }

    #[test]
    fn tilde_fence_balanced() {
        let mut text = String::from("~~~python\n");
        text.push_str(&"y".repeat(2000));
        text.push_str("\n~~~");

        let chunks = split_message_chunks(&text, 2000);
        assert!(chunks.len() >= 2);
        assert!(chunks[0].trim_end().ends_with("```"));
        assert!(chunks[1].starts_with("~~~python"));
    }

    #[test]
    fn emoji_at_boundary_no_panic() {
        let mut msg = "a".repeat(1998);
        msg.push_str("🎉🎊");
        let chunks = split_message_chunks(&msg, 2000);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 2000);
        }
    }

    // ── find_open_fence ──────────────────────────────────────────

    #[test]
    fn detects_open_backtick_fence() {
        assert!(find_open_fence("```rust\nlet x = 1;").is_some());
    }

    #[test]
    fn detects_closed_fence() {
        assert!(find_open_fence("```\ncode\n```").is_none());
    }

    #[test]
    fn closing_fence_needs_matching_marker() {
        // Opened with ```, closed with ~~~ → still open
        assert!(find_open_fence("```\ncode\n~~~").is_some());
    }

    #[test]
    fn closing_fence_ignores_content_after_markers() {
        // ``` with content after is an opening fence, not closing
        assert!(find_open_fence("```\ncode\n```not_a_close").is_some());
    }

    // ── permanent error detection ────────────────────────────────

    #[test]
    fn permanent_error_detection() {
        assert!(is_permanent_send_error(
            "failed to send discord message: Message too large.: Message too large."
        ));
        assert!(is_permanent_send_error("Unknown Channel"));
        assert!(is_permanent_send_error("Missing Access"));
        assert!(!is_permanent_send_error("connection reset by peer"));
        assert!(!is_permanent_send_error("timeout"));
    }
}
