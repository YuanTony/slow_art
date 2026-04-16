use serde::Deserialize;

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub telegram: TelegramConfig,
    pub discord: DiscordConfig,
    pub database: DatabaseConfig,
    pub session: SessionConfig,
    pub audio_guide: AudioGuideConfig,
    pub llm: ProviderConfig,
    pub embedding: EmbeddingConfig,
    pub asr: AsrConfig,
    pub ocr: OcrConfig,
    pub tts: TtsConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub poll_timeout_seconds: u64,
    pub download_dir: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct DiscordConfig {
    pub enabled: bool,
    pub bot_token: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub artworks_db_path: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct SessionConfig {
    pub countdown_minutes: u64,
    pub max_history_messages: usize,
    pub auto_accept_confidence: f64,
    pub confirm_confidence: f64,
    pub min_top_gap: f64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct EmbeddingConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub dimensions: usize,
    pub min_match_score: f64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct AudioGuideConfig {
    pub enabled: bool,
    pub museum: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct AsrConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct OcrConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TtsConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub voice: String,
    pub format: String,
}
