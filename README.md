# Slow Art

A Rust-powered participatory museum companion that helps visitors engage more deeply and directly with art. Instead of glancing at a piece for 3 seconds and moving on, Slow Art brings you into the act of looking — through conversation, voice, and a mindful timer.

It supports both Discord and Telegram, with text, voice, and image inputs, and replies in both text and synthesized speech.

## What it does

1. **Onboarding** — `/start` or `/new` begins a fresh session with a friendly introduction explaining how to interact
2. **Artwork identification** — the visitor describes the artwork in front of them (by title, description, audio guide number, or any natural language). The bot identifies it using semantic embedding search (768-d vectors via `gemini-embedding-001`) with FTS5 fallback and LLM candidate resolution
3. **Time selection** — the visitor chooses how long to spend (e.g. 1, 3, 5, or 10 minutes). If they skip the prompt and start talking, it defaults to 5 minutes
4. **Guided discussion** — the bot uses deeply researched artwork context and conversation history to respond naturally, guiding the visitor's attention toward details, composition, symbolism, and technique. Every reply ends with a specific observation prompt that invites closer looking
5. **Live timer** — a countdown timer is embedded in the bot's last message and updates every 5 seconds via message editing, keeping continuous track of elapsed time across the session
6. **Voice in, voice out** — accepts voice messages (transcribed via ASR), replies with TTS audio alongside text so the visitor never has to look away from the artwork
7. **Artwork re-identification** — if the visitor indicates the current artwork is wrong, a lightweight classifier triggers a fresh search mid-conversation
8. **Concurrent sessions** — supports multiple simultaneous conversations across chat channels

## Description levels

Artworks in the database have three tiers of description richness:

- **L1** — metadata only (title, culture, date, medium, dimensions, gallery, tags). Fast to generate from the Met API and local parquet data
- **L2** — shallow research via Wikipedia and web sources, producing 2,000-4,000 characters of contextual information
- **L3** — deep agent-driven research (10-minute timeout) producing comprehensive reports with curatorial descriptions, provenance, scholarly interpretations, and verified image URLs

The current database contains 1,859 on-display Asian Art pieces from the Met (1,728 L1, 8 L2, 123 L3).

## Quick start

```bash
# 1. Copy and edit the config
cp config.example.toml config.toml
# Edit config.toml — set your bot tokens, LLM/ASR/TTS API keys, etc.

# 2. The pre-built artworks.db is included in this repo.
#    To add more artworks, see "Build artworks.db" below.

# 3. Build and run (embedding search mode, default)
cargo build
./target/debug/ten-minute-art
```

## Configure

Edit `config.toml` and set:

- Discord bot token (and/or Telegram bot token)
- LLM provider URL / key / model
- Embedding provider URL / key / model
- Confidence thresholds for artwork matching (`auto_accept_confidence`, `confirm_confidence`, `min_top_gap`)
- ASR provider URL / key / model (for voice input)
- OCR provider URL / key / model (for image input)
- TTS provider URL / key / model and voice/format (for voice output)
- Audio guide mode (`[audio_guide] enabled = true/false`)
- Session defaults (`countdown_minutes`, `max_history_messages`)

All API endpoints use OpenAI-compatible formats and can work with any compatible provider.

### TTS response format notes

Supported behaviors in the send path:
- `opus` / `ogg` — passed through directly and sent as `audio/ogg`
- `mp3` — passed through directly and sent as `audio/mpeg`
- `wav` — normalized in-memory if needed, then converted to OGG/Opus for delivery

For Telegram, `opus` is the best choice as it matches the native voice-note format. For Discord, any format works.

## Build `artworks.db`

A pre-built `artworks.db` with 1,859 Met Asian Art artworks is included. To add more artworks or rebuild from scratch:

### Prerequisites

Download parquet files from the [Met Asian Art HuggingFace dataset](https://huggingface.co/datasets/metmuseum/met-asian-art-open-access-hackathon):

```bash
mkdir -p data/embeddings

# Download embeddings parquet (768-d vectors, ~99MB)
curl -L -H "Authorization: Bearer $HF_TOKEN" \
  -o data/embeddings/train-00000.parquet \
  "https://huggingface.co/datasets/metmuseum/met-asian-art-open-access-hackathon/resolve/main/embeddings/agentic-vision-gemini/train-00000.parquet?download=true"

# Download metadata parquet (~1MB)
curl -L -H "Authorization: Bearer $HF_TOKEN" \
  -o data/metadata.parquet \
  "https://huggingface.co/datasets/metmuseum/met-asian-art-open-access-hackathon/resolve/main/metadata/train-00000.parquet?download=true"
```

Install Python dependencies:

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install pyarrow sqlite-vec
```

You also need `claude` (Claude Code CLI) or `codex` (OpenAI Codex CLI) on PATH for L2/L3 research.

### `scripts/add_met_artwork_by_object_id.py`

Adds an artwork from a Met Collection API object ID.

```bash
python3 scripts/add_met_artwork_by_object_id.py --object-id 39328 --level 3
```

Flags:
- `--object-id` (required) — Met Collection API object ID
- `--level` — description level: `1` (metadata only), `2` (shallow research), `3` (deep research, default)
- `--research-agent` — agent CLI: `claude` (default) or `codex`
- `--no-research` — skip research (sets level to 1)
- `--embeddings-dir` — directory containing embedding parquet files (default: `data/embeddings/`)
- `--no-embedding` — skip embedding lookup
- `--db` — path to SQLite database
- `--force` — replace existing row

### `scripts/add_met_audio_guide_artwork.py`

Adds an artwork from a Met audio guide stop ID.

```bash
python3 scripts/add_met_audio_guide_artwork.py --audio-id 102 --level 2
```

Flags:
- `--audio-id` (required) — Met audio guide stop number
- `--level` — description level: `1`, `2`, or `3` (default)
- `--research-agent` — agent CLI: `claude` (default) or `codex`
- `--no-research` — skip research (sets level to 1)
- `--embeddings-dir` — embedding parquet directory (default: `data/embeddings/`)
- `--no-embedding` — skip embedding lookup
- `--db` — path to SQLite database (default: `artworks.db`)
- `--force` — replace existing row

### Deep research via agent CLI

Both scripts delegate research to an external agent CLI. The shared logic lives in `scripts/research_artwork.py`.

- **L2 (shallow)** — light research with Wikipedia, 2,000-4,000 character target, 180s timeout
- **L3 (deep)** — three-phase research: extract Met page content, deep internet research, write structured report. 10-minute timeout

### Pre-computed embeddings

Scripts look up pre-computed 768-d embeddings from `data/embeddings/` (shared logic in `scripts/hf_embeddings.py`).

- **Model:** `gemini-embedding-001`, 768 dimensions
- **Coverage:** ~31,500 Asian Art department objects
- Embeddings are written directly into the `artwork_embeddings` sqlite-vec virtual table

## Rust feature flags

### `search_image` (default)

Semantic embedding search with FTS5 fallback.

```bash
cargo build
./target/debug/ten-minute-art
```

Search path: query embedding → sqlite-vec similarity → threshold check → FTS5 fallback → LLM candidate resolver

### `search_audio_id`

Audio-guide-ID-first behavior.

```bash
cargo build --no-default-features --features search_audio_id
./target/debug/ten-minute-art
```

Search path: LLM number detector → audio_guide_id lookup → FTS5 → LLM candidate resolver

## Architecture

```
User (Discord/Telegram)
  │
  ├─ Text message ──────────────┐
  ├─ Voice message → ASR ───────┤
  └─ Image → OCR/Vision ────────┤
                                 ▼
                          core::handle_text_message()
                                 │
                    ┌────────────┼────────────────┐
                    ▼            ▼                 ▼
            WaitingForArtwork  AwaitingConfirm  DiscussingArtwork
                    │            │                 │
                    ▼            ▼                 ▼
            Embedding search   Yes/No          LLM reply + TTS
            + FTS5 fallback    handling         + live timer
                    │
                    ▼
            AwaitingTimeSelection
                    │
                    ▼
            User picks minutes → timer starts
```

Key design choices:
- **Session state machine** in `src/types.rs` with four stages: `WaitingForArtwork` → `AwaitingConfirmation` → `AwaitingTimeSelection` → `DiscussingArtwork`
- **Platform-agnostic core** in `src/core.rs` returns `CoreResponse::Messages` or `CoreResponse::TimerStart`, and platform adapters (Discord, Telegram) handle delivery
- **Live timer** via `tokio::spawn` background tasks that edit the last Discord message every 5 seconds; timer persists across messages using the session's `started_at` timestamp
- **Timer cancellation** via `Arc<AtomicBool>` per session — old timers stop when new replies are sent
- All LLM interactions are stateless `/chat/completions` requests; Rust owns all state transitions

## Test

```bash
cargo test
scripts/e2e_test.sh
```

## Database schema

```sql
CREATE TABLE IF NOT EXISTS artworks (
  id INTEGER PRIMARY KEY,
  audio_guide_id INTEGER,
  official_name TEXT NOT NULL,
  search_text TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL,
  description_level INTEGER NOT NULL DEFAULT 1
);

-- FTS5 index (external content, synced via triggers)
CREATE VIRTUAL TABLE IF NOT EXISTS artworks_fts USING fts5(
  official_name, search_text,
  content='artworks', content_rowid='id'
);

-- Embedding storage (sqlite-vec)
CREATE VIRTUAL TABLE IF NOT EXISTS artwork_embeddings USING vec0(
  artwork_id integer primary key,
  embedding float[768]
);
```

## Notes

- Session state is in-memory; restarting the process resets active sessions
- The pre-built `artworks.db` covers on-display Met Asian Art pieces only
- The discussion LLM is instructed to keep responses under 1,800 characters to respect Discord's 2,000-char message limit
- Long messages are automatically split at paragraph boundaries for Discord delivery
