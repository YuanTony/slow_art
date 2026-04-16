# Scripts

## Quick start

```bash
# Download the embedding parquet file (~99MB, requires HuggingFace access)
mkdir -p data/embeddings
curl -L -H "Authorization: Bearer $HF_TOKEN" \
  -o data/embeddings/train-00000.parquet \
  "https://huggingface.co/datasets/metmuseum/met-asian-art-open-access-hackathon/resolve/main/embeddings/agentic-vision-gemini/train-00000.parquet?download=true"

# Build the database from the curated list of 232 highlighted artworks
scripts/run_ids.sh data/highlighted_public_ids.txt --research-agent codex
```

## `run_ids.sh`

Batch runner: reads Met object IDs from a file (one per line) and runs `add_met_artwork_by_object_id.py` for each. All extra arguments are passed through.

```bash
scripts/run_ids.sh <id_file> [extra flags...]
```

**Examples:**

```bash
# Add all 232 highlighted public-domain artworks with Codex research
scripts/run_ids.sh data/highlighted_public_ids.txt --research-agent codex

# With Claude Code and a custom database
scripts/run_ids.sh data/highlighted_public_ids.txt --research-agent claude --db my_artworks.db

# Skip research, just populate metadata + embeddings
scripts/run_ids.sh data/highlighted_public_ids.txt --no-research
```

## `add_met_artwork_by_object_id.py`

Adds one artwork to the database from a Met Collection API object ID. Fetches structured metadata, invokes an agent for deep research, and looks up a pre-computed embedding.

```bash
python3 scripts/add_met_artwork_by_object_id.py --object-id 39328
```

**Parameters:**

| Flag | Required | Default | Description |
|------|----------|---------|-------------|
| `--object-id` | yes | — | Met Collection API object ID |
| `--db` | no | `artworks.db` | Path to SQLite database |
| `--research-agent` | no | `claude` | Agent CLI for deep research: `claude` or `codex` |
| `--no-research` | no | — | Skip agent research, store Met metadata only |
| `--embeddings-dir` | no | `data/embeddings/` | Directory containing embedding parquet files |
| `--no-embedding` | no | — | Skip embedding lookup |
| `--force` | no | — | Replace existing row if object ID already in DB |

**Behavior:**
- Skips non-Open-Access artworks (Met API returns 403)
- Populates `search_text` with structured Met API metadata (~500 chars)
- Populates `description` with metadata + deep research report (~20-30K chars)
- Inserts 768-d embedding from HuggingFace dataset into `artwork_embeddings`
- FTS index is kept in sync via SQLite triggers

## `add_met_audio_guide_artwork.py`

Adds one artwork from a Met audio guide stop ID. Extracts the Met object ID from the audio guide page, fetches metadata, runs deep research, and looks up embeddings.

```bash
python3 scripts/add_met_audio_guide_artwork.py --audio-id 102
```

**Parameters:**

| Flag | Required | Default | Description |
|------|----------|---------|-------------|
| `--audio-id` | yes | — | Met audio guide stop number |
| `--db` | no | `artworks.db` | Path to SQLite database |
| `--out-dir` | no | `output/` | Directory for JSON report output |
| `--research-agent` | no | `claude` | Agent CLI for deep research: `claude` or `codex` |
| `--no-research` | no | — | Skip agent research |
| `--sleep` | no | `1.5` | Delay in seconds before fetching (rate limiting) |
| `--force` | no | — | Replace existing row for this audio guide ID |
| `--embeddings-dir` | no | `data/embeddings/` | Directory containing embedding parquet files |
| `--no-embedding` | no | — | Skip embedding lookup |

**Behavior:**
- Fetches the audio guide HTML page and extracts title + `crdId` (Met object ID)
- Filters out non-artwork pages (intros, playlists, navigation) and pages with < 1000 chars
- Uses `crdId` as primary key when available; otherwise uses auto-increment
- Writes a JSON report to `output/audio_{id}.json`

## `research_artwork.py`

Shared module that invokes an agent CLI (`claude` or `codex`) to produce a deep research report. Used by both `add_met_*` scripts. Not run directly.

The agent works in three phases:
1. **Extract Met page content** — visits the Met collection page, extracts text and image URLs
2. **Deep research** — searches internet, Wikipedia, academic sources for additional context
3. **Write report** — structured plain-text output with Met content, verified image URLs, and research findings

## `hf_embeddings.py`

Shared module that looks up pre-computed 768-d embeddings from local parquet files (from the Met Asian Art HuggingFace dataset, `embeddings/agentic-vision-gemini` config). Used by both `add_met_*` scripts. Not run directly.

Requires: `pip install pyarrow sqlite-vec`

## `backfill_search_text.py`

One-time migration script to backfill `search_text` from the Met Collection API for existing rows. Only needed for databases created before the `search_text` column was added.

```bash
python3 scripts/backfill_search_text.py artworks.db
```

## `validate_artworks_db.py`

Validates the `artworks.db` schema and prints a summary of all rows.

```bash
python3 scripts/validate_artworks_db.py
```

## Metadata parquet and curated ID lists

The file `data/metadata.parquet` contains catalog metadata for ~31,500 Asian Art objects from the [Met Asian Art HuggingFace dataset](https://huggingface.co/datasets/metmuseum/met-asian-art-open-access-hackathon). Columns include `object_id`, `is_public_domain`, `is_highlight`, `title`, `culture`, `medium`, `object_date`, and more.

Download it:

```bash
curl -L -H "Authorization: Bearer $HF_TOKEN" \
  -o data/metadata.parquet \
  "https://huggingface.co/datasets/metmuseum/met-asian-art-open-access-hackathon/resolve/main/metadata/train-00000.parquet?download=true"
```

The file `data/highlighted_public_ids.txt` (checked into git) contains 232 Met object IDs that are both public domain and highlighted — the best-documented, most significant objects in the collection. It was generated from the metadata parquet:

```python
import pyarrow.parquet as pq

t = pq.read_table("data/metadata.parquet")
ids = t.column("object_id").to_pylist()
public = t.column("is_public_domain").to_pylist()
highlight = t.column("is_highlight").to_pylist()

results = sorted(oid for oid, pub, hi in zip(ids, public, highlight) if pub and hi)

with open("data/highlighted_public_ids.txt", "w") as f:
    for oid in results:
        f.write(f"{oid}\n")
```

You can adapt this to create other ID lists — for example, filtering by `culture`, `medium`, or `gallery_number`.
