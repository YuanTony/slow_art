#!/usr/bin/env python3
import argparse
import json
import re
import sqlite3
import sys
from pathlib import Path
from urllib.error import URLError, HTTPError
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


def collect_met_image_urls(obj: dict) -> list[str]:
    """Extract all image URLs from a Met API object."""
    urls = []
    for key in ["primaryImage", "primaryImageSmall"]:
        url = (obj.get(key) or "").strip()
        if url:
            urls.append(url)
    for url in obj.get("additionalImages") or []:
        url = (url or "").strip()
        if url:
            urls.append(url)
    # Deduplicate while preserving order
    seen = set()
    return [u for u in urls if not (u in seen or seen.add(u))]


_URL_PATTERN = re.compile(r'https?://[^\s,;"\'\]\)}\|]+', re.IGNORECASE)


def _check_url(url: str, timeout: int = 10) -> bool:
    """Return True if the URL is reachable.

    Only treats HTTP 404 (Not Found) and 410 (Gone) as truly dead.
    429 (rate limited), 403, etc. are treated as alive — likely transient or
    server-side rejection of HEAD requests, not a missing resource.
    Tries HEAD first, then falls back to GET with a browser User-Agent.
    """
    safe_url = url.encode("ascii", errors="ignore").decode("ascii")
    if not safe_url or len(safe_url) < 10:
        return False

    user_agent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"

    for method in ("HEAD", "GET"):
        try:
            req = Request(safe_url, method=method, headers={"User-Agent": user_agent})
            with urlopen(req, timeout=timeout) as resp:
                return resp.getcode() == 200
        except HTTPError as e:
            if e.code in (404, 410):
                return False
            return True
        except (URLError, TimeoutError, OSError, UnicodeError, ValueError):
            continue
    return True


def strip_dead_urls(description: str) -> tuple[str, int]:
    """Validate all URLs in the description and remove dead ones.

    Returns (cleaned_description, dead_count).
    """
    urls = _URL_PATTERN.findall(description)
    # Deduplicate
    seen = set()
    unique_urls = []
    for url in urls:
        url = url.rstrip(".,;:)]}\"'")
        if url not in seen:
            seen.add(url)
            unique_urls.append(url)

    dead_count = 0
    for url in unique_urls:
        if not _check_url(url):
            description = description.replace(url, "")
            dead_count += 1
            print(f"[validate] removed dead URL: {url}", file=sys.stderr)

    # Clean up double spaces
    description = re.sub(r"  +", " ", description)
    return description, dead_count


def build_description(object_id: int, title: str, met_metadata: str, research_report: str, level: int, met_image_urls: list[str] = None) -> str:
    parts = [
        f"Met object ID {object_id}.",
        f"Title: {title}.",
    ]
    if met_metadata:
        parts.append(met_metadata)
    if research_report:
        prefix = "Deep research report:" if level == 3 else "Research report:"
        parts.append(f"{prefix} {research_report}")
    if met_image_urls:
        parts.append("Met image URLs: " + " ".join(met_image_urls))
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

    met_image_urls = collect_met_image_urls(obj)

    description = build_description(
        args.object_id,
        official_name,
        met_metadata,
        research_report,
        args.level,
        met_image_urls,
    )

    # Validate all URLs and strip dead ones
    description, dead_count = strip_dead_urls(description)
    if dead_count:
        print(f"[validate] stripped {dead_count} dead URL(s) from description", file=sys.stderr)

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
