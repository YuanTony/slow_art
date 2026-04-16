use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::multipart;
use serenity::all::{
    ChannelId, Client, Command, CommandInteraction, Context as DiscordContext, CreateAttachment,
    CreateCommand, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
    EditMessage, EventHandler, GatewayIntents, Interaction, Message, MessageId, Ready,
};
use serenity::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tracing::{error, info};
use uuid::Uuid;

use crate::audio::maybe_convert_ogg_opus_to_wav;
use crate::core::{CoreResponse, CoreState, ReplyMessage, handle_text_message, start_session};
use crate::types::{IncomingKind, IncomingMessage};

struct Handler {
    core: Arc<CoreState>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: DiscordContext, ready: Ready) {
        info!(user = %ready.user.name, "discord gateway ready");
        if let Err(err) = register_commands(&ctx).await {
            error!("discord command registration failed: {err:#}");
        }
    }

    async fn interaction_create(&self, ctx: DiscordContext, interaction: Interaction) {
        if let Interaction::Command(command) = interaction
            && let Err(err) = handle_command_interaction(&self.core, &ctx, &command).await
        {
            error!("discord slash command handling failed: {err:#}");
        }
    }

    async fn message(&self, ctx: DiscordContext, msg: Message) {
        if msg.author.bot {
            return;
        }

        info!(
            channel_id = %msg.channel_id,
            author = %msg.author.name,
            has_attachments = !msg.attachments.is_empty(),
            content = %msg.content,
            "incoming discord message"
        );

        if let Err(err) = msg.channel_id.broadcast_typing(&ctx.http).await {
            error!("discord typing indicator failed: {err:#}");
        }

        let conversation_id = format!("discord:{}:{}", msg.channel_id, msg.author.id);
        match extract_incoming_message(&self.core, &msg).await {
            Ok(incoming) => match handle_text_message(&self.core, incoming).await {
                Ok(CoreResponse::Messages(messages)) => {
                    for ReplyMessage { text, with_tts } in messages {
                        if let Err(err) =
                            send_discord_text_and_voice(&self.core, &ctx, &msg, &text, with_tts)
                                .await
                        {
                            error!("discord send failed: {err:#}");
                        }
                    }
                }
                Ok(CoreResponse::TimerStart { messages, countdown_minutes }) => {
                    let mut last_sent_msg = None;
                    let mut last_text = String::new();
                    for ReplyMessage { text, with_tts } in &messages {
                        last_text = text.clone();
                        match send_discord_text_and_voice_returning_msg(
                            &self.core, &ctx, &msg, text, *with_tts,
                        ).await {
                            Ok(sent) => last_sent_msg = Some(sent),
                            Err(err) => error!("discord send failed: {err:#}"),
                        }
                    }
                    // Spawn live timer task on the last sent message
                    if let Some(sent_msg) = last_sent_msg {
                        let (cancel, session_start) = {
                            let sessions = self.core.sessions.lock().await;
                            sessions.get(&conversation_id)
                                .map(|s| (s.timer_cancel.clone(), s.started_at))
                                .unwrap_or_else(|| (Arc::new(std::sync::atomic::AtomicBool::new(true)), Instant::now()))
                        };
                        spawn_timer_task(
                            ctx.http.clone(),
                            sent_msg.channel_id,
                            sent_msg.id,
                            last_text,
                            countdown_minutes,
                            session_start,
                            cancel,
                        );
                    }
                }
                Err(err) => error!("discord message handling failed: {err:#}"),
            },
            Err(err) => error!("discord attachment/message extraction failed: {err:#}"),
        }
    }
}

pub async fn run_discord(core: Arc<CoreState>) -> Result<()> {
    info!(
        enabled = core.config.discord.enabled,
        "discord adapter starting"
    );

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&core.config.discord.bot_token, intents)
        .event_handler(Handler { core })
        .await
        .context("creating discord client")?;

    client.start().await.context("running discord client")?;
    Ok(())
}

async fn register_commands(ctx: &DiscordContext) -> Result<()> {
    let commands = vec![
        CreateCommand::new("start").description("Start a new 10 Minute Art session"),
        CreateCommand::new("new").description("Start a fresh 10 Minute Art session"),
    ];
    Command::set_global_commands(&ctx.http, commands)
        .await
        .context("registering discord global commands")?;
    Ok(())
}

async fn handle_command_interaction(
    core: &Arc<CoreState>,
    ctx: &DiscordContext,
    command: &CommandInteraction,
) -> Result<()> {
    let name = command.data.name.as_str();
    if !matches!(name, "start" | "new") {
        return Ok(());
    }

    let conversation_id = format!("discord:{}:{}", command.channel_id, command.user.id);
    let CoreResponse::Messages(messages) = start_session(core, &conversation_id).await else {
        return Ok(());
    };
    let text = messages
        .first()
        .map(|m| m.text.clone())
        .unwrap_or_else(|| "New session started.".to_string());

    command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new().content(text),
            ),
        )
        .await
        .context("sending discord slash command response")?;

    Ok(())
}

async fn extract_incoming_message(core: &Arc<CoreState>, msg: &Message) -> Result<IncomingMessage> {
    let conversation_id = format!("discord:{}:{}", msg.channel_id, msg.author.id);
    let reply_target = msg.channel_id.to_string();

    if !msg.content.trim().is_empty() {
        return Ok(IncomingMessage {
            platform: "discord",
            conversation_id,
            reply_target,
            text: Some(msg.content.clone()),
            voice_file: None,
            image_file: None,
            kind: IncomingKind::Text,
        });
    }

    if let Some(attachment) = msg
        .attachments
        .iter()
        .find(|a| is_audio_attachment(a.filename.as_str(), a.content_type.as_deref()))
    {
        let path = download_discord_attachment(core, &attachment.url, attachment.filename.as_str())
            .await?;
        let wav_path = maybe_convert_ogg_opus_to_wav(&path)?;
        let transcript = transcribe_audio(core, &wav_path).await?;
        info!(channel = %msg.channel_id, transcript = %transcript, file = %path.display(), wav_file = %wav_path.display(), "discord voice transcript extracted");
        return Ok(IncomingMessage {
            platform: "discord",
            conversation_id,
            reply_target,
            text: Some(transcript),
            voice_file: Some(wav_path),
            image_file: None,
            kind: IncomingKind::Voice,
        });
    }

    if let Some(attachment) = msg
        .attachments
        .iter()
        .find(|a| is_image_attachment(a.filename.as_str(), a.content_type.as_deref()))
    {
        let path = download_discord_attachment(core, &attachment.url, attachment.filename.as_str())
            .await?;
        let extracted = describe_or_ocr_image(core, &path).await?;
        info!(channel = %msg.channel_id, extracted_text = %extracted, file = %path.display(), "discord image OCR extracted");
        return Ok(IncomingMessage {
            platform: "discord",
            conversation_id,
            reply_target,
            text: Some(extracted),
            voice_file: None,
            image_file: Some(path),
            kind: IncomingKind::Image,
        });
    }

    Ok(IncomingMessage {
        platform: "discord",
        conversation_id,
        reply_target,
        text: Some(String::new()),
        voice_file: None,
        image_file: None,
        kind: IncomingKind::Text,
    })
}

async fn download_discord_attachment(
    core: &Arc<CoreState>,
    url: &str,
    filename: &str,
) -> Result<PathBuf> {
    let bytes = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .context("downloading discord attachment")?
        .error_for_status()
        .context("discord attachment returned error status")?
        .bytes()
        .await
        .context("reading discord attachment bytes")?;

    let extension = Path::new(filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin");
    let path = Path::new(&core.config.telegram.download_dir).join(format!(
        "discord-{}.{}",
        Uuid::new_v4(),
        extension
    ));
    tokio::fs::write(&path, bytes)
        .await
        .context("writing discord attachment")?;
    Ok(path)
}

fn is_audio_attachment(filename: &str, content_type: Option<&str>) -> bool {
    let name = filename.to_ascii_lowercase();
    let content_type = content_type.unwrap_or_default().to_ascii_lowercase();
    content_type.starts_with("audio/")
        || ["ogg", "mp3", "wav", "m4a", "aac", "flac", "webm"]
            .iter()
            .any(|ext| name.ends_with(ext))
}

fn is_image_attachment(filename: &str, content_type: Option<&str>) -> bool {
    let name = filename.to_ascii_lowercase();
    let content_type = content_type.unwrap_or_default().to_ascii_lowercase();
    content_type.starts_with("image/")
        || ["png", "jpg", "jpeg", "webp", "gif"]
            .iter()
            .any(|ext| name.ends_with(ext))
}

async fn transcribe_audio(core: &Arc<CoreState>, path: &Path) -> Result<String> {
    if !core.config.asr.enabled {
        return Err(anyhow!("ASR is disabled"));
    }

    let form = multipart::Form::new()
        .text("model", core.config.asr.model.clone())
        .file("file", path)
        .await
        .context("adding audio file to ASR multipart form")?;

    let response = reqwest::Client::new()
        .post(&core.config.asr.base_url)
        .bearer_auth(&core.config.asr.api_key)
        .multipart(form)
        .send()
        .await
        .context("calling ASR provider for discord")?
        .error_for_status()
        .context("discord ASR provider returned error status")?;

    let value: serde_json::Value = response.json().await.context("parsing ASR response")?;
    value
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("ASR response missing text field"))
}

async fn describe_or_ocr_image(core: &Arc<CoreState>, path: &Path) -> Result<String> {
    if !core.config.ocr.enabled {
        return Err(anyhow!("OCR is disabled"));
    }

    let bytes = tokio::fs::read(path)
        .await
        .context("reading image for OCR")?;
    let mime = guess_mime(path);
    let data_url = format!("data:{};base64,{}", mime, BASE64.encode(bytes));

    let response = reqwest::Client::new()
        .post(&core.config.ocr.base_url)
        .bearer_auth(&core.config.ocr.api_key)
        .json(&serde_json::json!({
            "model": core.config.ocr.model,
            "messages": [
                {
                    "role": "system",
                    "content": "Extract any readable text from the image, and if it appears to be an artwork photo or museum label, briefly describe the visible content in plain text. Return plain text only."
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Please OCR and describe this image."},
                        {"type": "image_url", "image_url": {"url": data_url}}
                    ]
                }
            ]
        }))
        .send()
        .await
        .context("calling OCR provider for discord")?
        .error_for_status()
        .context("discord OCR provider returned error status")?;

    let value: serde_json::Value = response.json().await.context("parsing OCR response")?;
    extract_chat_content(&value).ok_or_else(|| anyhow!("OCR response missing assistant content"))
}

fn extract_chat_content(value: &serde_json::Value) -> Option<String> {
    value
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")
        .and_then(|content| {
            if let Some(text) = content.as_str() {
                Some(text.to_string())
            } else if let Some(parts) = content.as_array() {
                let mut joined = String::new();
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        if !joined.is_empty() {
                            joined.push('\n');
                        }
                        joined.push_str(text);
                    }
                }
                if joined.is_empty() {
                    None
                } else {
                    Some(joined)
                }
            } else {
                None
            }
        })
}

fn guess_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/jpeg",
    }
}

async fn send_discord_text_and_voice_returning_msg(
    core: &Arc<CoreState>,
    ctx: &DiscordContext,
    msg: &Message,
    text: &str,
    with_tts: bool,
) -> Result<Message> {
    info!(channel_id = %msg.channel_id, text = %text, "sending discord text reply");
    let chunks = split_message(text, 2000);
    let mut last_sent = None;
    for chunk in chunks {
        last_sent = Some(
            msg.channel_id
                .send_message(&ctx.http, CreateMessage::new().content(chunk))
                .await
                .context("sending discord text message")?,
        );
    }

    if core.config.tts.enabled && with_tts {
        info!(channel_id = %msg.channel_id, text = %text, "sending discord TTS voice reply");
        let tts_text = crate::audio::strip_urls_for_tts(text);
        let raw_audio = synthesize_speech(core, &tts_text).await?;
        let (audio, mime) = crate::audio::prepare_tts_audio_for_send(raw_audio, &core.config.tts.format)?;
        let ext = if mime == "audio/ogg" { "ogg" } else { &core.config.tts.format };
        let filename = format!("reply.{ext}");
        let attachment = CreateAttachment::bytes(audio, filename);
        msg.channel_id
            .send_files(
                &ctx.http,
                vec![attachment],
                CreateMessage::new().content("Voice reply"),
            )
            .await
            .context("sending discord voice attachment")?;
    }

    last_sent.ok_or_else(|| anyhow!("no message was sent"))
}

fn spawn_timer_task(
    http: Arc<serenity::http::Http>,
    channel_id: ChannelId,
    msg_id: MessageId,
    base_text: String,
    countdown_minutes: u64,
    session_start: Instant,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) {
    tokio::spawn(async move {
        let start = session_start;
        let cutoff = Duration::from_secs(countdown_minutes * 60);
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let elapsed = start.elapsed();
            // Strip any existing timer suffix from the base text before appending new one
            let clean_text = strip_timer_suffix(&base_text);
            if elapsed >= cutoff {
                let final_text = format!(
                    "{}\n\n_[{} — done!]_",
                    clean_text,
                    crate::core::format_duration(cutoff)
                );
                let _ = channel_id
                    .edit_message(&http, msg_id, EditMessage::new().content(&final_text))
                    .await;
                break;
            }
            let updated = format!(
                "{}\n\n_[{} / {}]_",
                clean_text,
                crate::core::format_duration(elapsed),
                crate::core::format_duration(cutoff)
            );
            let _ = channel_id
                .edit_message(&http, msg_id, EditMessage::new().content(&updated))
                .await;
        }
    });
}

fn strip_timer_suffix(text: &str) -> &str {
    // Remove trailing timer like "\n\n_[0:05 / 5:00]_" or "\n\n_[5:00 — done!]_"
    if let Some(pos) = text.rfind("\n\n_[") {
        if text[pos..].ends_with("]_") {
            return &text[..pos];
        }
    }
    text
}

async fn send_discord_text_and_voice(
    core: &Arc<CoreState>,
    ctx: &DiscordContext,
    msg: &Message,
    text: &str,
    with_tts: bool,
) -> Result<()> {
    info!(channel_id = %msg.channel_id, text = %text, "sending discord text reply");
    for chunk in split_message(text, 2000) {
        msg.channel_id
            .send_message(&ctx.http, CreateMessage::new().content(chunk))
            .await
            .context("sending discord text message")?;
    }

    if core.config.tts.enabled && with_tts {
        info!(channel_id = %msg.channel_id, text = %text, "sending discord TTS voice reply");
        let tts_text = crate::audio::strip_urls_for_tts(text);
        let raw_audio = synthesize_speech(core, &tts_text).await?;
        let (audio, mime) = crate::audio::prepare_tts_audio_for_send(raw_audio, &core.config.tts.format)?;
        let ext = if mime == "audio/ogg" { "ogg" } else { &core.config.tts.format };
        let filename = format!("reply.{ext}");
        let attachment = CreateAttachment::bytes(audio, filename);
        msg.channel_id
            .send_files(
                &ctx.http,
                vec![attachment],
                CreateMessage::new().content("Voice reply"),
            )
            .await
            .context("sending discord voice attachment")?;
    }

    Ok(())
}

async fn synthesize_speech(core: &Arc<CoreState>, text: &str) -> Result<Vec<u8>> {
    let response = reqwest::Client::new()
        .post(&core.config.tts.base_url)
        .bearer_auth(&core.config.tts.api_key)
        .json(&serde_json::json!({
            "model": core.config.tts.model,
            "voice": core.config.tts.voice,
            "input": text,
            "response_format": core.config.tts.format
        }))
        .send()
        .await
        .context("calling TTS provider for discord")?
        .error_for_status()
        .context("discord TTS provider returned error status")?;

    Ok(response.bytes().await?.to_vec())
}

fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining);
            break;
        }
        // Find the last newline within the limit to split at a paragraph boundary
        let split_at = remaining[..max_len]
            .rfind("\n\n")
            .or_else(|| remaining[..max_len].rfind('\n'))
            .unwrap_or(max_len);
        let (chunk, rest) = remaining.split_at(split_at);
        chunks.push(chunk);
        remaining = rest.trim_start_matches('\n');
    }
    chunks
}
