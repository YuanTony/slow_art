"""Shared module: look up pre-computed 768-d embeddings from a local parquet file
and insert directly into the sqlite-vec artwork_embeddings virtual table.

Source: metmuseum/met-asian-art-open-access-hackathon (HuggingFace)
Config: embeddings/agentic-vision-gemini
Model:  gemini-embedding-001, 768 dimensions

Download the parquet file(s) once and place them in data/embeddings/.
"""

import struct
import sys
from pathlib import Path

import pyarrow.parquet as pq
import sqlite_vec

_EMBEDDING_DIM = 768
ROOT = Path(__file__).resolve().parent.parent
DEFAULT_EMBEDDINGS_DIR = ROOT / "data" / "embeddings"

_EMBEDDING_SCHEMA = '''
CREATE VIRTUAL TABLE IF NOT EXISTS artwork_embeddings USING vec0(
  artwork_id integer primary key,
  embedding float[768]
);
'''

# Cache: object_id -> list[float]
_embedding_cache = None


def _load_embeddings(embeddings_dir: str = None) -> dict:
    """Load all parquet files from the embeddings directory. Returns {object_id: [float,...]}."""
    global _embedding_cache
    if _embedding_cache is not None:
        return _embedding_cache

    edir = Path(embeddings_dir) if embeddings_dir else DEFAULT_EMBEDDINGS_DIR
    if not edir.exists():
        print(f"[embeddings] directory not found: {edir} — skipping", file=sys.stderr)
        _embedding_cache = {}
        return _embedding_cache

    parquet_files = sorted(edir.glob("*.parquet"))
    if not parquet_files:
        print(f"[embeddings] no parquet files in {edir} — skipping", file=sys.stderr)
        _embedding_cache = {}
        return _embedding_cache

    cache = {}
    for pf in parquet_files:
        table = pq.read_table(pf, columns=["object_id", "vector"])
        obj_ids = table.column("object_id").to_pylist()
        embeddings = table.column("vector").to_pylist()
        for oid, emb in zip(obj_ids, embeddings):
            if emb is not None and len(emb) == _EMBEDDING_DIM:
                cache[oid] = [float(x) for x in emb]
        print(f"[embeddings] loaded {len(cache)} embeddings from {pf.name}", file=sys.stderr)

    print(f"[embeddings] total: {len(cache)} embeddings cached", file=sys.stderr)
    _embedding_cache = cache
    return _embedding_cache


def lookup_embedding(object_id: int, embeddings_dir: str = None) -> list | None:
    """Return the 768-d embedding for a Met object ID, or None if not found."""
    cache = _load_embeddings(embeddings_dir)
    return cache.get(object_id)


def enable_vec(conn):
    """Load the sqlite-vec extension and ensure the artwork_embeddings table exists."""
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.execute(_EMBEDDING_SCHEMA)


def insert_embedding(cur, artwork_id: int, embedding: list):
    """Insert a 768-d embedding into the artwork_embeddings vec0 table."""
    blob = struct.pack(f"{len(embedding)}f", *embedding)
    # vec0 virtual tables don't support UPSERT, so delete first if exists
    cur.execute("DELETE FROM artwork_embeddings WHERE artwork_id = ?", (artwork_id,))
    cur.execute(
        "INSERT INTO artwork_embeddings (artwork_id, embedding) VALUES (?, ?)",
        (artwork_id, blob),
    )
