use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Artwork {
    pub id: i64,
    pub audio_guide_id: Option<i64>,
    pub official_name: String,
    pub description: String,
    pub description_level: i64,
    pub is_highlight: bool,
    pub gallery_number: String,
}

#[derive(Debug, Clone)]
pub enum SessionStage {
    WaitingForArtwork,
    AwaitingConfirmation { artwork: Artwork },
    AwaitingTimeSelection { artwork: Artwork },
    DiscussingArtwork { artwork: Artwork },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct UserSession {
    pub stage: SessionStage,
    pub history: Vec<ChatMessage>,
    pub started_at: Instant,
    pub countdown_minutes: u64,
    pub timer_cancel: Arc<AtomicBool>,
    pub rejected_artwork_ids: Vec<i64>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AudioGuideStop {
    pub stop_number: i64,
    pub title: String,
    pub transcript: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ArtworkDecision {
    AutoAccept {
        artwork: Artwork,
        confidence: f64,
        rationale: String,
    },
    NeedsConfirmation {
        artwork: Artwork,
        confidence: f64,
        rationale: String,
    },
    NoMatch,
}

#[derive(Debug, Clone)]
pub enum DiscussionIntent {
    Reply,
    SearchArtwork { query: String },
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum IncomingKind {
    Text,
    Voice,
    Image,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub platform: &'static str,
    pub conversation_id: String,
    pub reply_target: String,
    pub text: Option<String>,
    pub voice_file: Option<std::path::PathBuf>,
    pub image_file: Option<std::path::PathBuf>,
    pub kind: IncomingKind,
}
