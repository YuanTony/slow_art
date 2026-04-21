#!/usr/bin/env python3
import argparse
import json
import sqlite3
import sys
from pathlib import Path
from urllib.parse import urlencode
from urllib.request import Request, urlopen

from research_artwork import run_agent_research, run_agent_shallow_research
from hf_embeddings import lookup_embedding, insert_embedding, enable_vec
from hf_metadata import lookup_display_info

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DB = ROOT / "artworks.db"

def fetch_text(url: str):
    req = Request(url, headers={"User-Agent": "ten-minute-art/1.0"})
    with urlopen(req, timeout=30) as resp:
        return resp.getcode(), resp.read().decode("utf-8", errors="replace")


def fetch_json(url: str, params=None):
    if params:
        url = f"{url}?{urlencode(params)}"
    status, text = fetch_text(url)
    return status, json.loads(text)



def fetch_met_object(object_id: int):
    """Fetch Met Collection API object. Returns dict or None if not accessible (e.g. 403)."""
    try:
        status, data = fetch_json(
            f"https://collectionapi.metmuseum.org/public/collection/v1/objects/{object_id}"
        )
        if status != 200:
            return None
        return data
    except Exception:
        return None



def build_met_metadata_text(obj: dict) -> str:
    bits = []
    for key, label in [
        ("artistDisplayName", "Artist"),
        ("artistDisplayBio", "Artist bio"),
        ("artistNationality", "Artist nationality"),
        ("artistRole", "Artist role"),
        ("culture", "Culture"),
        ("objectDate", "Date"),
        ("period", "Period"),
        ("dynasty", "Dynasty"),
        ("reign", "Reign"),
        ("medium", "Medium"),
        ("dimensions", "Dimensions"),
        ("classification", "Classification"),
        ("department", "Department"),
        ("creditLine", "Credit line"),
        ("repository", "Repository"),
        ("objectURL", "Met collection page"),
    ]:
        val = (obj.get(key) or "").strip()
        if val:
            bits.append(f"{label}: {val}.")

    geography = ", ".join(
        x for x in [
            (obj.get("country") or "").strip(),
            (obj.get("region") or "").strip(),
            (obj.get("subregion") or "").strip(),
            (obj.get("locale") or "").strip(),
        ] if x
    )
    if geography:
        bits.append(f"Geographic context: {geography}.")

    tags = ", ".join(
        t.get("term", "") for t in (obj.get("tags") or []) if t.get("term")
    )
    if tags:
        bits.append(f"Tags/themes: {tags}.")

    return " ".join(bits)


def build_description(object_id: int, title: str, met_metadata: str, research_report: str, level: int) -> str:
    parts = [
        f"Met object ID {object_id}.",
        f"Title: {title}.",
    ]
    if met_metadata:
        parts.append(met_metadata)
    if research_report:
        prefix = "Deep research report:" if level == 3 else "Research report:"
        parts.append(f"{prefix} {research_report}")
    return " ".join(parts).strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--object-id", type=int, required=True)
    parser.add_argument("--db", default=str(DEFAULT_DB))
    parser.add_argument("--research-agent", default="claude", choices=["claude", "codex"],
                        help="Agent CLI to use for deep research (default: claude)")
    parser.add_argument("--level", type=int, default=1, choices=[1, 2, 3],
                        help="Description level: 1=metadata only, 2=shallow research, 3=deep research (default: 1)")
    parser.add_argument("--no-research", action="store_true",
                        help="Skip agent research (equivalent to --level 1)")
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--embeddings-dir", default=None,
                        help="Directory containing embedding parquet files (default: data/embeddings/)")
    parser.add_argument("--no-embedding", action="store_true",
                        help="Skip embedding lookup")
    args = parser.parse_args()

    result = {"id": args.object_id, "audio_guide_id": 0}

    obj = fetch_met_object(args.object_id)
    if not obj:
        result.update({"status": "skipped", "reason": "Met API returned 403 or failed (not Open Access)"})
        print(json.dumps(result, indent=2))
        return

    official_name = (obj.get("title") or f"Met Object {args.object_id}").strip()
    object_url = (obj.get("objectURL") or "").strip()
    met_metadata = build_met_metadata_text(obj)
    hl, gn = lookup_display_info(args.object_id)
    is_highlight = 1 if hl else 0
    gallery_number = gn

    if args.no_research:
        args.level = 1

    research_report = ""
    if args.level == 2:
        research_report = run_agent_shallow_research(
            agent=args.research_agent,
            title=official_name,
            met_url=object_url,
            met_metadata=met_metadata,
        )
    elif args.level == 3:
        research_report = run_agent_research(
            agent=args.research_agent,
            title=official_name,
            met_url=object_url,
            met_metadata=met_metadata,
        )

    description = build_description(
        args.object_id,
        official_name,
        met_metadata,
        research_report,
        args.level,
    )

    conn = sqlite3.connect(args.db)
    cur = conn.cursor()
    if not args.force:
        cur.execute("SELECT id, audio_guide_id, official_name FROM artworks WHERE id = ?", (args.object_id,))
        existing = cur.fetchone()
        if existing:
            result.update({
                "status": "skipped",
                "reason": "met_object_id already exists",
                "existing_row_id": existing[0],
                "existing_audio_guide_id": existing[1],
                "existing_title": existing[2],
            })
            conn.close()
            print(json.dumps(result, indent=2))
            return

    cur.execute(
        """
        INSERT INTO artworks (id, audio_guide_id, official_name, search_text, description, description_level, is_highlight, gallery_number)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          audio_guide_id = excluded.audio_guide_id,
          official_name = excluded.official_name,
          search_text = excluded.search_text,
          description = excluded.description,
          description_level = excluded.description_level,
          is_highlight = excluded.is_highlight,
          gallery_number = excluded.gallery_number
        """,
        (args.object_id, 0, official_name, met_metadata, description, args.level, is_highlight, gallery_number),
    )

    # Look up and insert pre-computed embedding
    embedding_stored = False
    if not args.no_embedding:
        embedding = lookup_embedding(args.object_id, embeddings_dir=args.embeddings_dir)
        if embedding:
            enable_vec(conn)
            insert_embedding(cur, args.object_id, embedding)
            embedding_stored = True

    conn.commit()
    conn.close()

    result.update({
        "status": "inserted",
        "official_name": official_name,
        "met_metadata_chars": len(met_metadata),
        "research_chars": len(research_report),
        "description_chars": len(description),
        "description_level": args.level,
        "embedding_stored": embedding_stored,
    })
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
