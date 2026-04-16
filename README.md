# 10 Minute Art

A Rust museum bot that helps a visitor stay engaged with a single artwork for at least 10 minutes. It currently supports Telegram, and the codebase also includes a Discord adapter.

## What it does

- `/start` begins a fresh artwork session and shows a short onboarding message
- `/new` also begins a fresh artwork session
- Asks the user to describe the artwork in front of them
- Uses an LLM to decide which artwork they are viewing
- Uses deeply researched artwork information as context to generate interesting follow-up prompts
- Tracks session time and appends an elapsed-time note to replies while the session is under 10 minutes
- Supports concurrent conversations across chat sessions
- Accepts text, voice, and image inputs
- Replies in text and then immediately sends a TTS voice version of the same reply
- If the user indicates the currently selected artwork is wrong, runs a lightweight classifier and can automatically re-search for a better artwork match from the new description

## Quick start

```bash
# 1. Copy and edit the config
cp config.example.toml config.toml
# Edit config.toml — set your Telegram bot token, LLM/ASR/TTS API keys, etc.

# 2. Build the artwork database (see "Build artworks.db" section below)

# 3. Build and run the bot (embedding search mode, default)
cargo build
./target/debug/ten-minute-art

# Or audio-guide-ID mode
cargo build --no-default-features --features search_audio_id
./target/debug/ten-minute-art
```

## Configure

Edit `config.toml` and set:

- Telegram bot token
- Optional Discord bot token (keep disabled unless you want to run Discord too)
- LLM provider URL / key / model
- Confidence thresholds for artwork matching (`auto_accept_confidence`, `confirm_confidence`, `min_top_gap`)
- ASR provider URL / key / model
- OCR provider URL / key / model
- TTS provider URL / key / model
- TTS output format (`[tts].format`), using the OpenAI-compatible `response_format` field on the speech API
- Audio guide mode (`[audio_guide] enabled = true/false`, one museum at a time)

The defaults are shaped like OpenAI-compatible endpoints, but the code uses plain HTTP and can work with compatible providers.

### TTS response format notes

The bot sends the configured TTS format to OpenAI-compatible speech APIs using the documented `response_format` request field.

Supported behaviors in the Rust send path:
- `opus` / `ogg` — passed through directly and sent as `audio/ogg`
- `mp3` — passed through directly and sent as `audio/mpeg`
- `wav` — normalized in-memory if the provider returns streaming-style RIFF/data sentinel sizes, then converted to OGG/Opus for chat delivery

For Telegram voice delivery, `opus` is usually the best choice because it avoids extra transcoding and matches the expected Ogg/Opus voice-note format well.


## Build `artworks.db`

Two scripts populate the database, one artwork per run. Both use an agent CLI (Claude Code or OpenAI Codex) to produce a deep research report for each artwork. Both can also look up pre-computed 768-d embeddings from the [Met Asian Art HuggingFace dataset](https://huggingface.co/datasets/metmuseum/met-asian-art-open-access-hackathon) and store them directly in `artwork_embeddings`.

### Prerequisites

Download the parquet files from the HuggingFace dataset (requires access — the dataset is gated, so you need a HuggingFace account with access granted):

```bash
mkdir -p data/embeddings

# Download the embeddings parquet (768-d vectors from gemini-embedding-001, ~99MB)
curl -L -H "Authorization: Bearer $HF_TOKEN" \
  -o data/embeddings/train-00000.parquet \
  "https://huggingface.co/datasets/metmuseum/met-asian-art-open-access-hackathon/resolve/main/embeddings/agentic-vision-gemini/train-00000.parquet?download=true"

# Download the metadata parquet (object info, ~1MB)
curl -L -H "Authorization: Bearer $HF_TOKEN" \
  -o data/metadata.parquet \
  "https://huggingface.co/datasets/metmuseum/met-asian-art-open-access-hackathon/resolve/main/metadata/train-00000.parquet?download=true"
```

The `data/` directory is tracked by git but parquet files are gitignored — you must download them locally.

Install Python dependencies:

```bash
pip install pyarrow sqlite-vec
```

You also need `claude` (Claude Code CLI) or `codex` (OpenAI Codex CLI) installed and on PATH for deep research.

### `scripts/add_met_artwork_by_object_id.py`

Adds an artwork directly from a Met Collection API object ID (no audio guide stop required).

```bash
python3 scripts/add_met_artwork_by_object_id.py --object-id 39328
```

Flags:
- `--object-id` (required) — Met Collection API object ID
- `--research-agent` — agent CLI: `claude` (default) or `codex`
- `--no-research` — skip agent research
- `--embeddings-dir` — directory containing embedding parquet files (default: `data/embeddings/`)
- `--no-embedding` — skip embedding lookup
- `--db` — path to SQLite database
- `--force` — replace existing row

### `scripts/add_met_audio_guide_artwork.py`

Adds an artwork from a Met audio guide stop ID.

```bash
python3 scripts/add_met_audio_guide_artwork.py --audio-id 102
```

Flags:
- `--audio-id` (required) — Met audio guide stop number
- `--research-agent` — agent CLI to use: `claude` (default) or `codex`
- `--no-research` — skip agent research, use Met text only
- `--embeddings-dir` — directory containing embedding parquet files (default: `data/embeddings/`)
- `--no-embedding` — skip embedding lookup
- `--db` — path to SQLite database (default: `artworks.db`)
- `--out-dir` — output directory for JSON reports (default: `output/`)
- `--sleep` — delay before fetching (default: 1.5s)
- `--force` — replace existing row for this audio guide ID

### Deep research via agent CLI

Both scripts delegate research to an external agent CLI (`claude` or `codex`). The shared logic lives in `scripts/research_artwork.py`.

The agent receives the Met collection page URL, artwork title, and structured metadata, then works in three phases:

**Phase 1 — Extract Met page content:**
The agent visits the Met collection page and extracts all textual content (curatorial description, provenance, exhibition history, references). It also finds and tests all image URLs on the page, keeping only those that return HTTP 200.

**Phase 2 — Deep research:**
The agent searches the internet — Wikipedia, museum databases, academic sources, art history sites — for additional information. It researches the artwork's origin, historical context, artist/culture, materials and techniques, scholarly interpretations, significance, provenance, and any controversies. It finds additional public image URLs (e.g. Wikimedia Commons) and verifies each one.

**Phase 3 — Write report:**
The agent writes a structured plain-text report with three sections:
1. **Met page content** — full extracted text from the Met collection page
2. **Verified image URLs** — all image URLs that passed testing, with source noted
3. **Research report** — deep research findings with section headings

The report is stored in the `description` field alongside Met Collection API metadata. There is no character limit.

The agent call is synchronous with a 10-minute timeout. The agent runs with `--dangerously-skip-permissions` (claude) or `--full-auto` (codex) to allow unattended web access.

### Pre-computed embeddings

The scripts look up pre-computed 768-d embeddings from `data/embeddings/` (downloaded in Prerequisites above). The shared logic lives in `scripts/hf_embeddings.py`.

- **Config used:** `embeddings/agentic-vision-gemini` (text embeddings of AI-generated visual descriptions)
- **Model:** `gemini-embedding-001`, 768 dimensions — matches the project's `artwork_embeddings` schema
- **Coverage:** ~31,500 Asian Art department objects

Embeddings are written directly into the `artwork_embeddings` sqlite-vec virtual table. FTS rows are maintained automatically by SQLite triggers on `artworks`. The database is fully ready after the Python scripts run — the Rust app reads it as-is.

## Rust feature flags and search paths

The project has two search-related feature modes.

### 1. `search_image` (default)
Enabled by default.

Build and run:

```bash
cargo build
./target/debug/ten-minute-art
```

Search path:
1. Generate a 768-d query embedding using `gemini-embedding-001`
2. Search `artwork_embeddings` with `sqlite-vec`
3. If the top vector match clears the configured threshold, accept it
4. Otherwise fall back to FTS5 ranked retrieval (`bm25`) + LLM candidate resolver
5. During artwork discussion, if the visitor indicates the current artwork is wrong, a small LLM classifier can trigger a fresh search using a short extracted query

Implementation notes:
- `search_image` is the default Cargo feature
- `search_image` statically links `sqlite-vec` into the Rust binary
- the binary has no external SQLite dynamic library dependency
- `search_image` requires that `artwork_embeddings` exists and contains backfilled vectors for meaningful results

### 2. `search_audio_id`
Optional feature for audio-guide-ID-first behavior.

Build and run:

```bash
cargo build --no-default-features --features search_audio_id
./target/debug/ten-minute-art
```

Search path:
1. Run the LLM audio-guide-number detector on the user's first artwork query
2. If it is an audio guide number, look up `audio_guide_id`
3. If it is not an audio guide number, build an FTS query
4. Run SQLite FTS5 ranked retrieval (`bm25`) to get a top-10 shortlist
5. Run the LLM candidate resolver over that shortlist

## Build and run

Default feature set (`search_image`):

```bash
cargo build
./target/debug/ten-minute-art
```

Audio-ID-first mode:

```bash
cargo build --no-default-features --features search_audio_id
./target/debug/ten-minute-art
```

For a release build:

```bash
cargo build --release
./target/release/ten-minute-art
```

## Test

```bash
# Unit tests
cargo test

# End-to-end test (builds, starts server, runs queries, verifies responses)
scripts/e2e_test.sh
```

## Notes

- Session state is stored in memory, so restarting the process resets active sessions.
- Artwork metadata lives in SQLite and is loaded from local file `artworks.db`.
- The main chat model is called through stateless `/chat/completions` style requests.
- Number-style audio guide detection is done by an LLM classifier.
- Non-number artwork matching uses a local DB shortlist and then an LLM resolver over returned candidates.
- During `DiscussingArtwork`, a second tiny classifier decides whether to continue discussing the current artwork or trigger a fresh artwork search.
- The discussion classifier returns strict JSON only in one of these forms:
  - `{"action":"reply"}`
  - `{"action":"search_artwork","query":"short search query"}`
- Rust remains responsible for all actual artwork lookup and session-state transitions; the LLM never directly selects artwork IDs.
- The Met audio-guide URL space is not guaranteed to be a complete contiguous sequence; some stop IDs may return 404.

## Database schema

The app expects a SQLite file `artworks.db` with a base table named `artworks`:

```sql
CREATE TABLE IF NOT EXISTS artworks (
  id INTEGER PRIMARY KEY,
  audio_guide_id INTEGER,
  official_name TEXT NOT NULL,
  search_text TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL
);
```

Field meanings:
- `id` — local artwork row ID / Met object ID
- `audio_guide_id` — optional museum audio guide stop number
- `official_name` — canonical artwork title
- `search_text` — short Met API metadata (culture, date, medium, tags, etc.) used for FTS search
- `description` — full text context used by the conversation model (Met metadata + deep research report)

The app also expects an external-content FTS5 table backed by `artworks`:

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS artworks_fts USING fts5(
  official_name,
  search_text,
  content='artworks',
  content_rowid='id'
);
```

The FTS index is kept in sync from `artworks` via SQLite triggers.

When the `search_image` feature is enabled, the app also expects embedding storage via `sqlite-vec`:

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS artwork_embeddings USING vec0(
  artwork_id integer primary key,
  embedding float[768]
);
```
