mod audio;
mod config;
mod core;
mod db;
mod discord;
mod telegram;
mod types;

use anyhow::{Context, Result};
use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use config::Config;
use core::{CoreResponse, CoreState, handle_text_message};
use db::{load_artworks, register_sqlite_vec};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::warn;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = load_config("config.toml")?;
    register_sqlite_vec();

    if std::env::args().any(|arg| arg == "--migrate-embeddings") {
        migrate_embeddings_schema(&config.database.artworks_db_path)?;
        return Ok(());
    }

    if config.telegram.enabled {
        fs::create_dir_all(&config.telegram.download_dir)
            .with_context(|| format!("creating download dir {}", config.telegram.download_dir))?;
    }

    let artworks = load_artworks(&config.database.artworks_db_path)?;
    if artworks.is_empty() {
        warn!(
            "No artworks found in database. Artwork matching will fail until database is populated."
        );
    }

    let core = Arc::new(CoreState::new(config.clone(), artworks));

    let mut tasks = Vec::new();

    // Debug API always runs
    let debug_core = Arc::clone(&core);
    tasks.push(tokio::spawn(async move { run_debug_api(debug_core).await }));

    if config.telegram.enabled {
        let core_clone = Arc::clone(&core);
        tasks.push(tokio::spawn(async move {
            telegram::run_telegram(core_clone).await
        }));
    }
    if config.discord.enabled {
        let core_clone = Arc::clone(&core);
        tasks.push(tokio::spawn(async move {
            discord::run_discord(core_clone).await
        }));
    }

    for task in tasks {
        task.await??;
    }

    Ok(())
}

async fn run_debug_api(core: Arc<CoreState>) -> Result<()> {
    let app = Router::new()
        .route("/debug/query", post(debug_query))
        .with_state(core);

    let addr: SocketAddr = "127.0.0.1:8787".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct DebugQueryRequest {
    text: String,
    conversation_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct DebugQueryResponse {
    replies: Vec<String>,
}

async fn debug_query(
    State(core): State<Arc<CoreState>>,
    Json(payload): Json<DebugQueryRequest>,
) -> impl IntoResponse {
    let incoming = types::IncomingMessage {
        platform: "debug",
        conversation_id: payload
            .conversation_id
            .unwrap_or_else(|| "debug:local".to_string()),
        reply_target: "debug".to_string(),
        text: Some(payload.text),
        voice_file: None,
        image_file: None,
        kind: types::IncomingKind::Text,
    };

    match handle_text_message(&core, incoming).await {
        Ok(CoreResponse::Messages(messages)) => Json(DebugQueryResponse {
            replies: messages.into_iter().map(|m| m.text).collect(),
        })
        .into_response(),
        Ok(CoreResponse::TimerStart { messages, .. }) => Json(DebugQueryResponse {
            replies: messages.into_iter().map(|m| m.text).collect(),
        })
        .into_response(),
        Err(err) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("{err:#}"),
        )
            .into_response(),
    }
}

fn migrate_embeddings_schema(path: &str) -> Result<()> {
    let conn =
        Connection::open(path).with_context(|| format!("opening SQLite database at {path}"))?;
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS artwork_embeddings USING vec0(
          artwork_id integer primary key,
          embedding float[768]
        );
        "#,
    )
    .context("creating artwork_embeddings vec table")?;
    println!("migrated artwork_embeddings in {path}");
    Ok(())
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

fn load_config(path: &str) -> Result<Config> {
    let raw = fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    toml::from_str(&raw).context("parsing config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SessionStage, UserSession};
    use crate::config::{
        AsrConfig, AudioGuideConfig, DatabaseConfig, DiscordConfig, EmbeddingConfig, OcrConfig,
        ProviderConfig, SessionConfig, TelegramConfig, TtsConfig,
    };
    use crate::core::{
        artwork_audio_stop_score, extract_audio_stop_number, find_artwork_for_audio_stop,
        is_affirmative, is_negative, normalize_text,
    };
    use crate::types::Artwork;

    fn test_config_with_audio(enabled: bool) -> Config {
        Config {
            telegram: TelegramConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                poll_timeout_seconds: 30,
                download_dir: "downloads".to_string(),
            },
            discord: DiscordConfig {
                enabled: false,
                bot_token: "discord-token".to_string(),
            },
            database: DatabaseConfig {
                artworks_db_path: "artworks.db".to_string(),
            },
            session: SessionConfig {
                countdown_minutes: 10,
                max_history_messages: 24,
                auto_accept_confidence: 0.86,
                confirm_confidence: 0.62,
                min_top_gap: 0.12,
            },
            audio_guide: AudioGuideConfig {
                enabled,
                museum: "met".to_string(),
            },
            llm: ProviderConfig {
                base_url: "http://localhost/chat".to_string(),
                api_key: "x".to_string(),
                model: "test".to_string(),
            },
            embedding: EmbeddingConfig {
                base_url: "http://localhost/embed".to_string(),
                api_key: "x".to_string(),
                model: "test".to_string(),
                dimensions: 768,
                min_match_score: 0.80,
            },
            asr: AsrConfig {
                enabled: false,
                base_url: "http://localhost/asr".to_string(),
                api_key: "x".to_string(),
                model: "test".to_string(),
            },
            ocr: OcrConfig {
                enabled: false,
                base_url: "http://localhost/ocr".to_string(),
                api_key: "x".to_string(),
                model: "test".to_string(),
            },
            tts: TtsConfig {
                enabled: false,
                base_url: "http://localhost/tts".to_string(),
                api_key: "x".to_string(),
                model: "test".to_string(),
                voice: "alloy".to_string(),
                format: "mp3".to_string(),
            },
        }
    }

    fn artwork(id: i64, official_name: &str) -> Artwork {
        Artwork {
            id,
            audio_guide_id: None,
            official_name: official_name.to_string(),
            description: "Test description".to_string(),
            description_level: 3,
            is_highlight: false,
            gallery_number: String::new(),
        }
    }

    #[allow(dead_code)]
    fn test_session() -> UserSession {
        UserSession {
            stage: SessionStage::WaitingForArtwork,
            history: Vec::new(),
            started_at: std::time::Instant::now(),
            countdown_minutes: 10,
            timer_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rejected_artwork_ids: Vec::new(),
        }
    }

    #[test]
    fn normalize_text_strips_punctuation_and_case() {
        assert_eq!(normalize_text("Moo Lisa!!"), "moo lisa");
        assert_eq!(normalize_text("  Audio Stop: 102 "), "audio stop 102");
        assert_eq!(
            normalize_text("Shiva-as-Lord_of_Dance"),
            "shiva as lord of dance"
        );
    }

    #[test]
    fn affirmative_and_negative_detection_work() {
        assert!(is_affirmative("yes"));
        assert!(is_affirmative("That is right"));
        assert!(is_negative("no"));
        assert!(is_negative("not that one"));
        assert!(!is_affirmative("maybe"));
        assert!(!is_negative("correct"));
    }

    #[tokio::test]
    async fn audio_stop_number_extraction_handles_disabled_config_without_network() {
        let disabled = test_config_with_audio(false);
        assert_eq!(
            extract_audio_stop_number(&disabled, "102").await.unwrap(),
            None
        );
        assert_eq!(
            extract_audio_stop_number(&disabled, "stop 205")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            extract_audio_stop_number(&disabled, "audio 17")
                .await
                .unwrap(),
            None
        );
    }

    #[test]
    fn audio_stop_matching_prefers_best_local_artwork() {
        let artworks = vec![
            artwork(1, "Shiva as Lord of Dance (Nataraja)"),
            artwork(2, "Moon jar"),
        ];

        let matched = find_artwork_for_audio_stop(&artworks, "Nataraja").expect("expected a match");
        assert_eq!(matched.id, 1);
        assert_eq!(matched.official_name, "Shiva as Lord of Dance (Nataraja)");
        assert!(artwork_audio_stop_score("nataraja", &matched) >= 0.72);
    }

    #[test]
    fn audio_stop_matching_returns_none_when_unrelated() {
        let artworks = vec![artwork(2, "Moon jar")];

        assert!(
            find_artwork_for_audio_stop(&artworks, "Models from the Tomb of Meketre, Part 1")
                .is_none()
        );
    }
}
