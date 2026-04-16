use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::multipart;
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use tracing::{error, info};
use uuid::Uuid;

use crate::audio::maybe_convert_ogg_opus_to_wav;
use crate::config::Config;
use crate::core::{CoreResponse, CoreState, ReplyMessage, handle_text_message};
use crate::types::{IncomingKind, IncomingMessage};

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: T,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize, Clone)]
struct TelegramMessage {
    chat: TelegramChat,
    text: Option<String>,
    voice: Option<TelegramVoice>,
    photo: Option<Vec<TelegramPhotoSize>>,
}

#[derive(Debug, Deserialize, Clone)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Deserialize, Clone)]
struct TelegramVoice {
    file_id: String,
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct TelegramPhotoSize {
    file_id: String,
    width: i64,
    height: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramFileResponse {
    file_path: String,
}

pub async fn run_telegram(core: std::sync::Arc<CoreState>) -> Result<()> {
    let mut offset: i64 = 0;
    let api_base = format!(
        "https://api.telegram.org/bot{}",
        core.config.telegram.bot_token
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            core.config.telegram.poll_timeout_seconds + 15,
        ))
        .build()
        .context("building telegram HTTP client")?;

    loop {
        let updates = match get_updates(&client, &core.config, &api_base, offset).await {
            Ok(updates) => updates,
            Err(err) => {
                error!("telegram getUpdates failed (will retry in 5s): {err:#}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        if !updates.is_empty() {
            info!(
                count = updates.len(),
                offset = offset,
                "telegram updates received"
            );
        }
        for update in updates {
            offset = update.update_id + 1;
            if let Some(message) = update.message {
                if let Err(err) =
                    send_typing_action(&client, &api_base, &message.chat.id.to_string()).await
                {
                    error!("telegram typing action failed: {err:#}");
                }
                match extract_incoming_message(&client, &core, &api_base, &message).await {
                    Ok(incoming) => match handle_text_message(&core, incoming).await {
                        Ok(CoreResponse::Messages(messages)) => {
                            for ReplyMessage { text, with_tts } in messages {
                                if let Err(err) = send_text_and_voice(
                                    &client,
                                    &core.config,
                                    &api_base,
                                    &message.chat.id.to_string(),
                                    &text,
                                    with_tts,
                                )
                                .await
                                {
                                    error!("telegram send failed: {err:#}");
                                }
                            }
                        }
                        Ok(CoreResponse::TimerStart { messages, .. }) => {
                            // Telegram doesn't support live message editing timer — just send messages
                            for ReplyMessage { text, with_tts } in messages {
                                if let Err(err) = send_text_and_voice(
                                    &client,
                                    &core.config,
                                    &api_base,
                                    &message.chat.id.to_string(),
                                    &text,
                                    with_tts,
                                )
                                .await
                                {
                                    error!("telegram send failed: {err:#}");
                                }
                            }
                        }
                        Err(err) => error!("telegram message handling failed: {err:#}"),
                    },
                    Err(err) => error!("telegram extraction failed: {err:#}"),
                }
            }
        }
    }
}

async fn extract_incoming_message(
    client: &reqwest::Client,
    core: &std::sync::Arc<CoreState>,
    api_base: &str,
    message: &TelegramMessage,
) -> Result<IncomingMessage> {
    let conversation_id = format!("telegram:{}", message.chat.id);
    let reply_target = message.chat.id.to_string();

    if let Some(text) = &message.text {
        info!(chat_id = message.chat.id, extracted_text = %text, "telegram text input extracted");
        return Ok(IncomingMessage {
            platform: "telegram",
            conversation_id,
            reply_target,
            text: Some(text.clone()),
            voice_file: None,
            image_file: None,
            kind: IncomingKind::Text,
        });
    }

    if let Some(voice) = &message.voice {
        let path = download_telegram_file(
            client,
            &core.config,
            api_base,
            &voice.file_id,
            voice.mime_type.as_deref().unwrap_or("audio/ogg"),
        )
        .await?;
        let wav_path = maybe_convert_ogg_opus_to_wav(&path)?;
        let transcript = transcribe_audio(client, &core.config, &wav_path).await?;
        info!(chat_id = message.chat.id, transcript = %transcript, file = %path.display(), wav_file = %wav_path.display(), "telegram voice transcript extracted");
        return Ok(IncomingMessage {
            platform: "telegram",
            conversation_id,
            reply_target,
            text: Some(transcript),
            voice_file: Some(wav_path),
            image_file: None,
            kind: IncomingKind::Voice,
        });
    }

    if let Some(photos) = &message.photo
        && let Some(best) = photos.iter().max_by_key(|p| p.width * p.height)
    {
        let path =
            download_telegram_file(client, &core.config, api_base, &best.file_id, "image.jpg")
                .await?;
        let extracted = describe_or_ocr_image(client, &core.config, &path).await?;
        info!(chat_id = message.chat.id, extracted_text = %extracted, file = %path.display(), "telegram image OCR extracted");
        return Ok(IncomingMessage {
            platform: "telegram",
            conversation_id,
            reply_target,
            text: Some(extracted),
            voice_file: None,
            image_file: Some(path),
            kind: IncomingKind::Image,
        });
    }

    Ok(IncomingMessage {
        platform: "telegram",
        conversation_id,
        reply_target,
        text: Some(String::new()),
        voice_file: None,
        image_file: None,
        kind: IncomingKind::Text,
    })
}

async fn send_typing_action(client: &reqwest::Client, api_base: &str, chat_id: &str) -> Result<()> {
    client
        .post(format!("{api_base}/sendChatAction"))
        .json(&json!({
            "chat_id": chat_id,
            "action": "typing"
        }))
        .send()
        .await
        .context("calling Telegram sendChatAction")?
        .error_for_status()
        .context("Telegram sendChatAction returned error status")?;
    Ok(())
}

async fn get_updates(
    client: &reqwest::Client,
    config: &Config,
    api_base: &str,
    offset: i64,
) -> Result<Vec<Update>> {
    let response = client
        .post(format!("{api_base}/getUpdates"))
        .json(&json!({
            "offset": offset,
            "timeout": config.telegram.poll_timeout_seconds,
            "allowed_updates": ["message"]
        }))
        .send()
        .await
        .context("calling Telegram getUpdates")?
        .error_for_status()
        .context("Telegram getUpdates returned error status")?;

    let payload: TelegramResponse<Vec<Update>> = response
        .json()
        .await
        .context("parsing Telegram getUpdates response")?;
    if !payload.ok {
        return Err(anyhow!("Telegram getUpdates ok=false"));
    }
    Ok(payload.result)
}

async fn download_telegram_file(
    client: &reqwest::Client,
    config: &Config,
    api_base: &str,
    file_id: &str,
    fallback_name: &str,
) -> Result<PathBuf> {
    let response = client
        .post(format!("{api_base}/getFile"))
        .json(&json!({ "file_id": file_id }))
        .send()
        .await
        .context("calling Telegram getFile")?
        .error_for_status()
        .context("Telegram getFile returned error status")?;

    let payload: TelegramResponse<TelegramFileResponse> = response
        .json()
        .await
        .context("parsing Telegram getFile response")?;
    if !payload.ok {
        return Err(anyhow!("Telegram getFile ok=false"));
    }

    let remote_path = payload.result.file_path;
    let file_url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        config.telegram.bot_token, remote_path
    );

    let extension = Path::new(&remote_path)
        .extension()
        .and_then(|s| s.to_str())
        .or_else(|| {
            Path::new(fallback_name)
                .extension()
                .and_then(|s| s.to_str())
        })
        .unwrap_or("bin");

    let local_path = Path::new(&config.telegram.download_dir).join(format!(
        "telegram-{}.{}",
        Uuid::new_v4(),
        extension
    ));

    let bytes = client
        .get(file_url)
        .send()
        .await
        .context("downloading Telegram file")?
        .error_for_status()
        .context("Telegram file download returned error status")?
        .bytes()
        .await
        .context("reading Telegram file bytes")?;

    tokio::fs::write(&local_path, bytes)
        .await
        .with_context(|| format!("writing downloaded file to {}", local_path.display()))?;

    Ok(local_path)
}

async fn transcribe_audio(
    client: &reqwest::Client,
    config: &Config,
    path: &Path,
) -> Result<String> {
    if !config.asr.enabled {
        return Err(anyhow!("ASR is disabled"));
    }

    let form = multipart::Form::new()
        .text("model", config.asr.model.clone())
        .file("file", path)
        .await
        .context("adding audio file to ASR multipart form")?;

    let response = client
        .post(&config.asr.base_url)
        .bearer_auth(&config.asr.api_key)
        .multipart(form)
        .send()
        .await
        .context("calling ASR provider")?
        .error_for_status()
        .context("ASR provider returned error status")?;

    let value: serde_json::Value = response.json().await.context("parsing ASR response")?;
    value
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("ASR response missing text field"))
}

async fn describe_or_ocr_image(
    client: &reqwest::Client,
    config: &Config,
    path: &Path,
) -> Result<String> {
    if !config.ocr.enabled {
        return Err(anyhow!("OCR is disabled"));
    }

    let bytes = tokio::fs::read(path)
        .await
        .context("reading image for OCR")?;
    let mime = guess_mime(path);
    let data_url = format!("data:{};base64,{}", mime, BASE64.encode(bytes));

    let response = client
        .post(&config.ocr.base_url)
        .bearer_auth(&config.ocr.api_key)
        .json(&json!({
            "model": config.ocr.model,
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
        .context("calling OCR provider")?
        .error_for_status()
        .context("OCR provider returned error status")?;

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

async fn send_text_and_voice(
    client: &reqwest::Client,
    config: &Config,
    api_base: &str,
    chat_id: &str,
    text: &str,
    with_tts: bool,
) -> Result<()> {
    info!(chat_id = chat_id, text = %text, "sending telegram text reply");
    client
        .post(format!("{api_base}/sendMessage"))
        .json(&json!({ "chat_id": chat_id, "text": text }))
        .send()
        .await
        .context("calling Telegram sendMessage")?
        .error_for_status()
        .context("Telegram sendMessage returned error status")?;

    if config.tts.enabled && with_tts {
        info!(chat_id = chat_id, text = %text, "sending telegram TTS voice reply");
        let tts_text = crate::audio::strip_urls_for_tts(text);
        let raw_audio = synthesize_speech(client, config, &tts_text).await?;
        let (audio, mime) = crate::audio::prepare_tts_audio_for_send(raw_audio, &config.tts.format)?;
        let ext = if mime == "audio/ogg" { "ogg" } else { &config.tts.format };
        let part = multipart::Part::bytes(audio)
            .file_name(format!("reply.{ext}"))
            .mime_str(mime)?;
        let form = multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("voice", part);
        client
            .post(format!("{api_base}/sendVoice"))
            .multipart(form)
            .send()
            .await
            .context("calling Telegram sendVoice")?
            .error_for_status()
            .context("Telegram sendVoice returned error status")?;
    }

    Ok(())
}

async fn synthesize_speech(
    client: &reqwest::Client,
    config: &Config,
    text: &str,
) -> Result<Vec<u8>> {
    let response = client
        .post(&config.tts.base_url)
        .bearer_auth(&config.tts.api_key)
        .json(&json!({
            "model": config.tts.model,
            "voice": config.tts.voice,
            "input": text,
            "response_format": config.tts.format
        }))
        .send()
        .await
        .context("calling TTS provider")?
        .error_for_status()
        .context("TTS provider returned error status")?;
    Ok(response.bytes().await?.to_vec())
}
