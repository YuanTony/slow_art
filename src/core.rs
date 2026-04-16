use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::info;

use crate::config::Config;
use crate::db::{search_artworks_by_embedding, search_artworks_fts};
use crate::types::{
    Artwork, ArtworkDecision, ChatMessage, DiscussionIntent, IncomingMessage, SessionStage,
    UserSession,
};

pub struct CoreState {
    pub config: Config,
    pub artworks: Arc<Vec<Artwork>>,
    pub sessions: Arc<Mutex<HashMap<String, UserSession>>>,
}

impl CoreState {
    pub fn new(config: Config, artworks: Vec<Artwork>) -> Self {
        Self {
            config,
            artworks: Arc::new(artworks),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

pub enum CoreResponse {
    Messages(Vec<ReplyMessage>),
    TimerStart {
        messages: Vec<ReplyMessage>,
        countdown_minutes: u64,
    },
}

pub struct ReplyMessage {
    pub text: String,
    pub with_tts: bool,
}

pub async fn start_session(state: &CoreState, conversation_id: &str) -> CoreResponse {
    #[cfg(all(feature = "search_image", not(feature = "search_audio_id")))]
    let hint = " Describe what you see, or read the title from the label.";
    #[cfg(feature = "search_audio_id")]
    let hint = " You can say the audio guide number, or read the title from the label.";
    #[cfg(not(any(feature = "search_image", feature = "search_audio_id")))]
    let hint = "";

    let opening = format!(
        "Hello! I'm your museum companion. Give me a brief description of the artwork in front of you — the title, what it looks like, or anything from the label — and I'll try to identify it. Then we can have an in-depth discussion about it.{hint}\n\nRemember: just send a message any time you want me to respond."
    );

    let mut sessions = state.sessions.lock().await;
    sessions.insert(
        conversation_id.to_string(),
        UserSession {
            stage: SessionStage::WaitingForArtwork,
            history: vec![ChatMessage {
                role: "assistant".to_string(),
                content: opening.clone(),
            }],
            started_at: Instant::now(),
            countdown_minutes: state.config.session.countdown_minutes,
            timer_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );
    CoreResponse::Messages(vec![ReplyMessage {
        text: opening,
        with_tts: false,
    }])
}

pub async fn handle_text_message(state: &CoreState, msg: IncomingMessage) -> Result<CoreResponse> {
    let text = msg.text.unwrap_or_default();
    let trimmed = text.trim();
    if trimmed == "/start" || trimmed == "/new" {
        return Ok(start_session(state, &msg.conversation_id).await);
    }

    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .entry(msg.conversation_id.clone())
        .or_insert(UserSession {
            stage: SessionStage::WaitingForArtwork,
            history: Vec::new(),
            started_at: Instant::now(),
            countdown_minutes: state.config.session.countdown_minutes,
            timer_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
    session.history.push(ChatMessage {
        role: "user".to_string(),
        content: text.clone(),
    });
    truncate_history(session, state.config.session.max_history_messages);
    let stage = session.stage.clone();
    drop(sessions);

    let reply = match stage {
        SessionStage::WaitingForArtwork => {
            if let Some(stop_number) =
                llm_detect_audio_number_or_plain_number(&state.config, &text).await?
            {
                return handle_audio_stop_lookup_by_db(state, &msg.conversation_id, stop_number)
                    .await;
            }

            match identify_artwork_from_text_query(&state.config, &text).await? {
                ArtworkDecision::AutoAccept { artwork, .. } => {
                    let reply = time_selection_prompt(&artwork);
                    let mut sessions = state.sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                        session.stage = SessionStage::AwaitingTimeSelection { artwork };
                        session.history.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: reply.clone(),
                        });
                        truncate_history(session, state.config.session.max_history_messages);
                    }
                    reply
                }
                ArtworkDecision::NeedsConfirmation { artwork, .. } => {
                    let reply = format!(
                        "I think you probably mean \"{}\". Is that the one? Reply yes or no.",
                        artwork.official_name
                    );
                    let mut sessions = state.sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                        session.stage = SessionStage::AwaitingConfirmation { artwork };
                        session.history.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: reply.clone(),
                        });
                        truncate_history(session, state.config.session.max_history_messages);
                    }
                    reply
                }
                ArtworkDecision::NoMatch => {
                    "I couldn’t identify the artwork yet. Try the title or a short description from the label beside it.".to_string()
                }
            }
        }
        SessionStage::AwaitingConfirmation { artwork } => {
            if is_affirmative(&text) {
                let reply = time_selection_prompt(&artwork);
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                    session.stage = SessionStage::AwaitingTimeSelection { artwork };
                    session.history.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: reply.clone(),
                    });
                }
                reply
            } else if is_negative(&text) {
                let reply = "Okay — tell me the title or anything from the museum label, and I’ll try again.".to_string();
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                    session.stage = SessionStage::WaitingForArtwork;
                    session.history.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: reply.clone(),
                    });
                }
                reply
            } else {
                format!(
                    "Just answer yes or no — do you mean \"{}\"?",
                    artwork.official_name
                )
            }
        }
        SessionStage::AwaitingTimeSelection { artwork } => {
            // Parse minutes from user input
            let parsed = text.trim().parse::<u64>();
            if let Ok(minutes) = parsed {
                let minutes = minutes.clamp(1, 60);
                let reply = format!(
                    "Great — {} minute{} with \"{}\". Let's begin! What's the first thing that catches your eye?",
                    minutes,
                    if minutes == 1 { "" } else { "s" },
                    artwork.official_name
                );
                {
                    let mut sessions = state.sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                        session.stage = SessionStage::DiscussingArtwork { artwork };
                        session.countdown_minutes = minutes;
                        session.started_at = Instant::now();
                        session.timer_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        session.timer_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                        session.history.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: reply.clone(),
                        });
                        truncate_history(session, state.config.session.max_history_messages);
                    }
                } // drop lock before re-acquiring
                let sessions = state.sessions.lock().await;
                let text_with_time = with_session_time_suffix(
                    sessions.get(&msg.conversation_id),
                    &reply,
                    minutes,
                );
                return Ok(CoreResponse::TimerStart {
                    messages: vec![ReplyMessage {
                        text: text_with_time,
                        with_tts: true,
                    }],
                    countdown_minutes: minutes,
                });
            } else {
                // User skipped time selection — default to 5 minutes and treat input as first discussion message
                let default_minutes = 5u64;
                {
                    let mut sessions = state.sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                        session.stage = SessionStage::DiscussingArtwork {
                            artwork: artwork.clone(),
                        };
                        session.countdown_minutes = default_minutes;
                        session.started_at = Instant::now();
                        session.timer_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        session.timer_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    }
                }
                let reply = generate_artwork_reply(state, &artwork, &msg.conversation_id).await?;
                {
                    let mut sessions = state.sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                        session.history.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: reply.clone(),
                        });
                        truncate_history(session, state.config.session.max_history_messages);
                    }
                }
                let sessions = state.sessions.lock().await;
                let text_with_time = with_session_time_suffix(
                    sessions.get(&msg.conversation_id),
                    &reply,
                    default_minutes,
                );
                return Ok(CoreResponse::TimerStart {
                    messages: vec![ReplyMessage {
                        text: text_with_time,
                        with_tts: true,
                    }],
                    countdown_minutes: default_minutes,
                });
            }
        }
        SessionStage::DiscussingArtwork { artwork } => {
            match classify_discussion_intent(state, &artwork, &msg.conversation_id, &text).await? {
                DiscussionIntent::Reply => {
                    let reply = generate_artwork_reply(state, &artwork, &msg.conversation_id).await?;
                    let mut sessions = state.sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                        session.history.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: reply.clone(),
                        });
                        truncate_history(session, state.config.session.max_history_messages);
                    }
                    reply
                }
                DiscussionIntent::SearchArtwork { query } => {
                    if let Some(stop_number) =
                        llm_detect_audio_number_or_plain_number(&state.config, &query).await?
                    {
                        return handle_audio_stop_lookup_by_db(state, &msg.conversation_id, stop_number)
                            .await;
                    }

                    match identify_artwork_from_text_query(&state.config, &query).await? {
                        ArtworkDecision::AutoAccept { artwork, .. } => {
                            let reply = time_selection_prompt(&artwork);
                            let mut sessions = state.sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                                session.stage = SessionStage::AwaitingTimeSelection { artwork };
                                session.history.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: reply.clone(),
                                });
                                truncate_history(session, state.config.session.max_history_messages);
                            }
                            reply
                        }
                        ArtworkDecision::NeedsConfirmation { artwork, .. } => {
                            let reply = format!(
                                "I think you may actually mean \"{}\". Is that the one? Reply yes or no.",
                                artwork.official_name
                            );
                            let mut sessions = state.sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                                session.stage = SessionStage::AwaitingConfirmation { artwork };
                                session.history.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: reply.clone(),
                                });
                                truncate_history(session, state.config.session.max_history_messages);
                            }
                            reply
                        }
                        ArtworkDecision::NoMatch => {
                            let reply = "Okay — I may have the wrong artwork. Tell me the title or give me a short visual description and I’ll try to identify it again.".to_string();
                            let mut sessions = state.sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&msg.conversation_id) {
                                session.history.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: reply.clone(),
                                });
                                truncate_history(session, state.config.session.max_history_messages);
                            }
                            reply
                        }
                    }
                }
            }
        }
    };

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&msg.conversation_id);
    let countdown = session.map(|s| s.countdown_minutes).unwrap_or(state.config.session.countdown_minutes);
    let is_discussing = session.map(|s| matches!(s.stage, SessionStage::DiscussingArtwork { .. })).unwrap_or(false);
    let text_with_time = with_session_time_suffix(session, &reply, countdown);
    drop(sessions);

    if is_discussing {
        // Cancel old timer and create new cancel flag
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&msg.conversation_id) {
            session.timer_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            session.timer_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        }
        Ok(CoreResponse::TimerStart {
            messages: vec![ReplyMessage {
                text: text_with_time,
                with_tts: true,
            }],
            countdown_minutes: countdown,
        })
    } else {
        Ok(CoreResponse::Messages(vec![ReplyMessage {
            text: text_with_time,
            with_tts: true,
        }]))
    }
}

fn extract_first_image_url(description: &str) -> Option<String> {
    description
        .split_whitespace()
        .find_map(|token| {
            let cleaned = token
                .trim_matches(|c: char| matches!(c, ')' | '(' | ']' | '[' | '}' | '{' | ',' | '.' | ';' | '"' | '\''));
            let lower = cleaned.to_ascii_lowercase();
            if (lower.starts_with("http://") || lower.starts_with("https://"))
                && (lower.ends_with(".jpg")
                    || lower.ends_with(".jpeg")
                    || lower.ends_with(".png")
                    || lower.ends_with(".webp")
                    || lower.contains("images.metmuseum.org")
                    || lower.contains("iiif"))
            {
                Some(cleaned.to_string())
            } else {
                None
            }
        })
}

fn time_selection_prompt(artwork: &Artwork) -> String {
    let mut reply = format!(
        "Got it — looks like you're looking at \"{}\".\n\nHow long would you like to discuss it? Reply with a number of minutes (e.g. 1, 3, 5, or 10).",
        artwork.official_name
    );
    if let Some(url) = extract_first_image_url(&artwork.description) {
        reply.push_str("\n\nImage: ");
        reply.push_str(&url);
    }
    reply
}

async fn handle_audio_stop_lookup_by_db(
    state: &CoreState,
    conversation_id: &str,
    stop_number: i64,
) -> Result<CoreResponse> {
    let matched = state
        .artworks
        .iter()
        .find(|art| art.audio_guide_id == Some(stop_number))
        .cloned();

    let Some(artwork) = matched else {
        let sessions = state.sessions.lock().await;
        return Ok(CoreResponse::Messages(vec![ReplyMessage {
            text: with_session_time_suffix(
                sessions.get(conversation_id),
                &format!(
                    "I couldn’t find audio guide number {} in my database. Please try another number or send the artwork name.",
                    stop_number
                ),
                state.config.session.countdown_minutes,
            ),
            with_tts: true,
        }]));
    };

    let reply = format!(
        "Got it — audio stop {} is \"{}\". \n\nHow long would you like to discuss it? Reply with a number of minutes (e.g. 1, 3, 5, or 10).",
        stop_number, artwork.official_name
    );

    let mut sessions = state.sessions.lock().await;
    if let Some(session) = sessions.get_mut(conversation_id) {
        session.stage = SessionStage::AwaitingTimeSelection {
            artwork: artwork.clone(),
        };
        session.history.push(ChatMessage {
            role: "assistant".to_string(),
            content: reply.clone(),
        });
        truncate_history(session, state.config.session.max_history_messages);
    }

    Ok(CoreResponse::Messages(vec![ReplyMessage {
        text: reply,
        with_tts: true,
    }]))
}

async fn llm_detect_audio_number_or_plain_number(
    config: &Config,
    input: &str,
) -> Result<Option<i64>> {
    info!(input = %input, "starting audio id detector");
    let response = reqwest::Client::new()
        .post(&config.llm.base_url)
        .bearer_auth(&config.llm.api_key)
        .json(&json!({
            "model": config.llm.model,
            "messages": [
                {
                    "role": "system",
                    "content": "You classify whether a museum visitor message refers to an audio guide number. Return strict JSON only in the format {\"is_audio_guide_number\": true|false, \"number\": integer|null, \"confidence\": 0..1, \"reason\": \"short string\"}. If the message clearly refers to a stop/audio/guide number, set true and extract the integer. Otherwise set false."
                },
                {
                    "role": "user",
                    "content": input
                }
            ],
            "temperature": 0
        }))
        .send()
        .await
        .context("calling LLM number detector")?
        .error_for_status()
        .context("LLM number detector returned error status")?;

    let value: Value = response
        .json()
        .await
        .context("parsing LLM number detector response")?;
    let content = extract_chat_content(&value)
        .ok_or_else(|| anyhow!("LLM number detector missing assistant content"))?;
    let parsed: Value = serde_json::from_str(&content)
        .or_else(|_| {
            extract_json_object(&content).and_then(|s| serde_json::from_str(&s).map_err(Into::into))
        })
        .context("parsing detector JSON")?;

    let is_audio = parsed
        .get("is_audio_guide_number")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let confidence = parsed
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let number = parsed.get("number").and_then(|v| v.as_i64());

    if is_audio && confidence >= 0.6 {
        info!(input = %input, number = ?number, confidence = confidence, "audio id detector matched");
        Ok(number)
    } else {
        info!(input = %input, confidence = confidence, "audio id detector no-match");
        Ok(None)
    }
}

async fn identify_artwork_from_text_query(
    config: &Config,
    user_text: &str,
) -> Result<ArtworkDecision> {
    #[cfg(feature = "search_image")]
    {
        if let Some(decision) = identify_artwork_from_embedding(config, user_text).await? {
            return Ok(decision);
        }
    }

    identify_artwork_from_fts(config, user_text).await
}

async fn identify_artwork_from_fts(config: &Config, user_text: &str) -> Result<ArtworkDecision> {
    let fts_query = build_fts_query(user_text);
    info!(user_text = %user_text, fts_query = %fts_query, "built fts query");
    let candidate_pool =
        search_artworks_fts(&config.database.artworks_db_path, &fts_query, 10).unwrap_or_default();
    info!(count = candidate_pool.len(), candidates = ?candidate_pool.iter().map(|a| format!("{}: {}", a.id, a.official_name)).collect::<Vec<_>>(), "fts candidates returned");

    if candidate_pool.is_empty() {
        return Ok(ArtworkDecision::NoMatch);
    }
    if candidate_pool.len() == 1 {
        let artwork = candidate_pool[0].clone();
        return Ok(ArtworkDecision::AutoAccept {
            artwork,
            confidence: 1.0,
            rationale: "single FTS candidate".to_string(),
        });
    }

    resolve_candidates_with_llm(config, user_text, &candidate_pool).await
}

#[cfg(feature = "search_image")]
async fn identify_artwork_from_embedding(
    config: &Config,
    user_text: &str,
) -> Result<Option<ArtworkDecision>> {
    if user_text.trim().is_empty() {
        return Ok(None);
    }
    let embedding = embed_text_query(config, user_text).await?;
    let matches = search_artworks_by_embedding(&config.database.artworks_db_path, &embedding, 10)?;
    info!(count = matches.len(), candidates = ?matches.iter().map(|(a, d)| format!("{}: {} @ {}", a.id, a.official_name, d)).collect::<Vec<_>>(), "embedding candidates returned");

    let Some((artwork, distance)) = matches.first() else {
        return Ok(None);
    };

    let score = 1.0 / (1.0 + distance);
    if score >= config.embedding.min_match_score {
        return Ok(Some(ArtworkDecision::AutoAccept {
            artwork: artwork.clone(),
            confidence: score,
            rationale: format!("top embedding match with score {:.3}", score),
        }));
    }

    Ok(None)
}

#[cfg(feature = "search_image")]
async fn embed_text_query(config: &Config, input: &str) -> Result<Vec<f32>> {
    let response = reqwest::Client::new()
        .post(&config.embedding.base_url)
        .bearer_auth(&config.embedding.api_key)
        .json(&json!({
            "model": config.embedding.model,
            "input": input,
            "dimensions": config.embedding.dimensions,
        }))
        .send()
        .await
        .context("calling embedding API")?
        .error_for_status()
        .context("embedding API returned error status")?;

    let value: Value = response
        .json()
        .await
        .context("parsing embedding response")?;
    let embedding = value
        .get("data")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("embedding"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("embedding response missing vector"))?;

    let vec = embedding
        .iter()
        .map(|v| v.as_f64().unwrap_or_default() as f32)
        .collect::<Vec<_>>();

    if vec.len() != config.embedding.dimensions {
        return Err(anyhow!(
            "embedding dimensions mismatch: expected {}, got {}",
            config.embedding.dimensions,
            vec.len()
        ));
    }

    Ok(vec)
}

fn build_fts_query(input: &str) -> String {
    let stopwords = [
        "with", "the", "and", "big", "small", "very", "that", "this", "painting",
    ];
    let mut terms = normalize_text(input)
        .split_whitespace()
        .filter(|term| term.len() >= 3 && !stopwords.contains(term))
        .map(singularize_term)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return normalize_text(input);
    }
    terms.sort();
    terms.dedup();
    if terms.len() == 1 {
        terms[0].clone()
    } else {
        terms.join(" OR ")
    }
}

fn singularize_term(term: &str) -> String {
    if let Some(stripped) = term.strip_suffix("ies") {
        format!("{}y", stripped)
    } else if term.ends_with('s') && term.len() > 3 {
        term.trim_end_matches('s').to_string()
    } else {
        term.to_string()
    }
}

async fn resolve_candidates_with_llm(
    config: &Config,
    user_text: &str,
    candidates: &[Artwork],
) -> Result<ArtworkDecision> {
    let shortlist = candidates
        .iter()
        .map(|art| {
            let desc = art.description.chars().take(400).collect::<String>();
            format!(
                "ID: {} | Title: {} | Description: {}",
                art.id, art.official_name, desc
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let response = reqwest::Client::new()
        .post(&config.llm.base_url)
        .bearer_auth(&config.llm.api_key)
        .json(&json!({
            "model": config.llm.model,
            "messages": [
                {
                    "role": "system",
                    "content": format!(
                        "You resolve a museum visitor's description to the best matching artwork from a candidate shortlist. Choose the candidate that best semantically fits the user’s description, even if the wording is indirect. Ignore incidental keyword overlap in unrelated candidates. If one candidate is clearly the famous or direct match, select it. Return strict JSON only in the format {{\"matched_id\": integer|null, \"confidence\": 0..1, \"reason\": \"short string\"}}. User text: {}\n\nCandidates:\n{}",
                        user_text, shortlist
                    )
                },
                {
                    "role": "user",
                    "content": user_text
                }
            ],
            "temperature": 0
        }))
        .send()
        .await
        .context("calling LLM candidate resolver")?
        .error_for_status()
        .context("LLM candidate resolver returned error status")?;

    let value: Value = response
        .json()
        .await
        .context("parsing LLM candidate resolver response")?;
    let content = extract_chat_content(&value)
        .ok_or_else(|| anyhow!("LLM candidate resolver missing assistant content"))?;
    let parsed: Value = serde_json::from_str(&content)
        .or_else(|_| {
            extract_json_object(&content).and_then(|s| serde_json::from_str(&s).map_err(Into::into))
        })
        .context("parsing candidate resolver JSON")?;

    let matched_id = parsed.get("matched_id").and_then(|v| v.as_i64());
    let confidence = parsed
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let rationale = parsed
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("no reason provided")
        .to_string();

    let matched = matched_id.and_then(|id| candidates.iter().find(|art| art.id == id).cloned());
    info!(user_text = %user_text, matched_id = ?matched_id, confidence = confidence, rationale = %rationale, "llm candidate resolver result");
    let Some(artwork) = matched else {
        return Ok(ArtworkDecision::NoMatch);
    };

    if confidence >= config.session.auto_accept_confidence {
        return Ok(ArtworkDecision::AutoAccept {
            artwork,
            confidence,
            rationale,
        });
    }
    if confidence >= config.session.confirm_confidence {
        return Ok(ArtworkDecision::NeedsConfirmation {
            artwork,
            confidence,
            rationale,
        });
    }
    Ok(ArtworkDecision::NoMatch)
}

async fn classify_discussion_intent(
    state: &CoreState,
    artwork: &Artwork,
    conversation_id: &str,
    user_text: &str,
) -> Result<DiscussionIntent> {
    let sessions = state.sessions.lock().await;
    let history = sessions
        .get(conversation_id)
        .map(|s| s.history.clone())
        .unwrap_or_default();
    drop(sessions);

    let response = reqwest::Client::new()
        .post(&state.config.llm.base_url)
        .bearer_auth(&state.config.llm.api_key)
        .json(&json!({
            "model": state.config.llm.model,
            "messages": [
                {
                    "role": "system",
                    "content": format!(
                        "You are a tiny classifier for a museum bot. The current selected artwork is titled: {}. Decide whether the visitor is still discussing this artwork, or whether they are indicating that this is the wrong artwork and providing a new description that should trigger a fresh artwork search. Return strict JSON only in one of these forms: {{\"action\":\"reply\"}} or {{\"action\":\"search_artwork\",\"query\":\"short search query\"}}. Use search_artwork only when there is meaningful evidence the visitor is correcting the artwork or describing a different one. Keep query short and based mainly on the latest user message. Conversation history:\n{}",
                        artwork.official_name,
                        history.iter().map(|m| format!("{}: {}", m.role, m.content)).collect::<Vec<_>>().join("\n")
                    )
                },
                {
                    "role": "user",
                    "content": user_text
                }
            ],
            "temperature": 0
        }))
        .send()
        .await
        .context("calling discussion intent classifier")?
        .error_for_status()
        .context("discussion intent classifier returned error status")?;

    let value: Value = response
        .json()
        .await
        .context("parsing discussion intent classifier response")?;
    let content = extract_chat_content(&value)
        .ok_or_else(|| anyhow!("discussion intent classifier missing assistant content"))?;
    let parsed: Value = serde_json::from_str(&content)
        .or_else(|_| {
            extract_json_object(&content).and_then(|s| serde_json::from_str(&s).map_err(Into::into))
        })
        .context("parsing discussion intent classifier JSON")?;

    let action = parsed
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("reply");

    if action == "search_artwork" {
        let query = parsed
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or(user_text)
            .trim()
            .to_string();
        if !query.is_empty() {
            info!(current_artwork = %artwork.official_name, user_text = %user_text, query = %query, "discussion intent requested artwork re-search");
            return Ok(DiscussionIntent::SearchArtwork { query });
        }
    }

    Ok(DiscussionIntent::Reply)
}

async fn generate_artwork_reply(
    state: &CoreState,
    artwork: &Artwork,
    conversation_id: &str,
) -> Result<String> {
    let sessions = state.sessions.lock().await;
    let history = sessions
        .get(conversation_id)
        .map(|s| s.history.clone())
        .unwrap_or_default();
    drop(sessions);

    let response = reqwest::Client::new()
        .post(&state.config.llm.base_url)
        .bearer_auth(&state.config.llm.api_key)
        .json(&json!({
            "model": state.config.llm.model,
            "messages": [
                {
                    "role": "system",
                    "content": format!(
                        "You are 10 Minute Art, a calm museum companion. The visitor is standing in front of an artwork for a sustained viewing session. Use the artwork context and conversation history to respond in 2-3 concise paragraphs. Tell the user something interesting they have not already discussed or described, and guide their attention toward fresh details, relationships, emotions, texture, composition, symbolism, or technique. Avoid repeating their own words. Always end your response with a specific question or observation prompt that invites the visitor to look more closely or share what they notice — for example, 'Can you spot the small figure in the lower left corner?' or 'What emotion does the central figure's expression convey to you?' Keep your total response under 1800 characters. Artwork title: {}. Full artwork context: {}",
                        artwork.official_name, artwork.description
                    )
                },
                {
                    "role": "user",
                    "content": history.iter().map(|m| format!("{}: {}", m.role, m.content)).collect::<Vec<_>>().join("\n")
                }
            ],
            "temperature": 0.8
        }))
        .send()
        .await
        .context("calling LLM artwork reply generator")?
        .error_for_status()
        .context("LLM artwork reply generator returned error status")?;

    let value: Value = response
        .json()
        .await
        .context("parsing artwork reply response")?;
    extract_chat_content(&value)
        .ok_or_else(|| anyhow!("artwork reply response missing assistant content"))
}

#[allow(dead_code)]
pub async fn extract_audio_stop_number(config: &Config, input: &str) -> Result<Option<i64>> {
    if !config.audio_guide.enabled {
        return Ok(None);
    }
    llm_detect_audio_number_or_plain_number(config, input).await
}

#[allow(dead_code)]
pub fn find_artwork_for_audio_stop(artworks: &[Artwork], stop_title: &str) -> Option<Artwork> {
    let title_norm = normalize_text(stop_title);
    artworks
        .iter()
        .find(|art| {
            normalize_text(&art.official_name).contains(&title_norm)
                || title_norm.contains(&normalize_text(&art.official_name))
        })
        .cloned()
}

#[allow(dead_code)]
pub fn artwork_audio_stop_score(title_norm: &str, art: &Artwork) -> f64 {
    let title_norm = normalize_text(title_norm);
    let official_norm = normalize_text(&art.official_name);
    if title_norm == official_norm {
        1.0
    } else if title_norm.contains(&official_norm) || official_norm.contains(&title_norm) {
        0.85
    } else {
        0.0
    }
}

pub fn normalize_text(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_affirmative(input: &str) -> bool {
    matches!(
        normalize_text(input).as_str(),
        "yes" | "y" | "yeah" | "yep" | "correct" | "right" | "thats right" | "that is right"
    )
}

pub fn is_negative(input: &str) -> bool {
    matches!(
        normalize_text(input).as_str(),
        "no" | "n" | "nope" | "wrong" | "not that one"
    )
}

fn truncate_history(session: &mut UserSession, max_history_messages: usize) {
    if session.history.len() > max_history_messages {
        let remove_count = session.history.len() - max_history_messages;
        session.history.drain(0..remove_count);
    }
}

fn with_session_time_suffix(
    session: Option<&UserSession>,
    text: &str,
    countdown_minutes: u64,
) -> String {
    let Some(session) = session else {
        return text.to_string();
    };
    if !matches!(session.stage, SessionStage::DiscussingArtwork { .. }) {
        return text.to_string();
    }
    let elapsed = session.started_at.elapsed();
    let cutoff = Duration::from_secs(countdown_minutes * 60);
    if elapsed >= cutoff {
        return format!("{}\n\n_[{} — done!]_", text, format_duration(cutoff));
    }
    format!(
        "{}\n\n_[{} / {}]_",
        text,
        format_duration(elapsed),
        format_duration(cutoff)
    )
}

pub fn format_duration(d: Duration) -> String {
    let total_seconds = d.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{}:{:02}", minutes, seconds)
}

fn extract_chat_content(value: &Value) -> Option<String> {
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

fn extract_json_object(input: &str) -> Result<String> {
    let start = input
        .find('{')
        .ok_or_else(|| anyhow!("no JSON object start found"))?;
    let end = input
        .rfind('}')
        .ok_or_else(|| anyhow!("no JSON object end found"))?;
    Ok(input[start..=end].to_string())
}

fn _ensure_supported_channels(config: &Config) -> Result<()> {
    if !config.telegram.enabled && !config.discord.enabled {
        return Err(anyhow!(
            "At least one channel must be enabled: telegram or discord"
        ));
    }
    Ok(())
}
