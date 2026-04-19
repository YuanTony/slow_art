"""Shared module: look up artwork metadata (is_highlight, gallery_number) from
the local HuggingFace metadata parquet file.

Source: metmuseum/met-asian-art-open-access-hackathon (HuggingFace)
File:   metadata/train-00000.parquet

Download the parquet file once and place it at data/metadata.parquet.
"""

import sys
from pathlib import Path

import pyarrow.parquet as pq

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_METADATA_PATH = ROOT / "data" / "metadata.parquet"

# Cache: object_id -> (is_highlight: bool, gallery_number: str)
_metadata_cache = None


def _load_metadata(metadata_path: str = None) -> dict:
    """Load the metadata parquet. Returns {object_id: (is_highlight, gallery_number)}."""
    global _metadata_cache
    if _metadata_cache is not None:
        return _metadata_cache

    path = Path(metadata_path) if metadata_path else DEFAULT_METADATA_PATH
    if not path.exists():
        print(f"[metadata] file not found: {path} — skipping", file=sys.stderr)
        _metadata_cache = {}
        return _metadata_cache

    table = pq.read_table(path, columns=["object_id", "is_highlight", "gallery_number"])
    cache = {}
    obj_ids = table.column("object_id")
    highlights = table.column("is_highlight")
    galleries = table.column("gallery_number")
    for i in range(len(table)):
        oid = obj_ids[i].as_py()
        hl = bool(highlights[i].as_py())
        gn = galleries[i].as_py() or ""
        cache[oid] = (hl, str(gn).strip())

    print(f"[metadata] loaded {len(cache)} rows from {path.name}", file=sys.stderr)
    _metadata_cache = cache
    return _metadata_cache


def lookup_display_info(object_id: int, metadata_path: str = None) -> tuple[bool, str]:
    """Return (is_highlight, gallery_number) for a Met object ID.

    Returns (False, '') if not found.
    """
    cache = _load_metadata(metadata_path)
    return cache.get(object_id, (False, ""))
